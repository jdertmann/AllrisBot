mod lru_cache;

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::pin::pin;
use std::time::Duration;

use futures_util::stream::{FuturesUnordered, StreamExt as _};
use lru_cache::Lru;
use teloxide::RequestError;
use teloxide::payloads::SendMessageSetters as _;
use teloxide::prelude::Requester as _;
use teloxide::types::InlineKeyboardMarkup;
use tokio::sync::{RwLock, RwLockReadGuard, Semaphore, mpsc};
use tokio::time::{Instant, Interval, MissedTickBehavior, interval, sleep, sleep_until};
use tokio_retry::strategy::{ExponentialBackoff, jitter};

use self::lru_cache::{Cache, CacheItem};
use crate::database::{self, ChatState, DatabaseConnection, SharedDatabaseConnection, StreamId};
use crate::types::{ChatId, Condition, Filter, Message};

const ADDITIONAL_ERRORS: &[&str] = &[
    "Forbidden: bot was kicked from the group chat",
    "Bad Request: not enough rights to send text messages to the chat",
];

const BROADCASTS_PER_SECOND: f32 = 30.;
const GROUP_MESSAGES_PER_MINUTE: f32 = 20.;
const CHAT_MESSAGES_PER_SECOND: f32 = 1.;

const PARALLEL_SENDS: usize = 3;
const DELAY_AFTER_SEND: f32 = PARALLEL_SENDS as f32 / BROADCASTS_PER_SECOND;

fn chat_rate_limit(is_group: bool) -> Interval {
    let interval = if is_group {
        60. / GROUP_MESSAGES_PER_MINUTE
    } else {
        1. / CHAT_MESSAGES_PER_SECOND
    };

    let mut interval = tokio::time::interval(Duration::from_secs_f32(interval));
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    interval
}

struct SharedResources {
    bot: crate::Bot,
    db: SharedDatabaseConnection,
    semaphore: Semaphore,
    hard_shutdown: tokio::sync::RwLock<bool>,
    next_message_cache: Cache<StreamId, (StreamId, Message), Lru<StreamId>>,
}

impl Condition {
    fn matches(&self, message: &Message) -> bool {
        message
            .tags
            .iter()
            .filter(|x| x.0 == self.tag)
            .any(|x| self.pattern.is_match(&x.1))
            ^ self.negate
    }
}

impl Filter {
    fn matches(&self, message: &Message) -> bool {
        self.conditions
            .iter()
            .all(|condition| condition.matches(message))
    }
}
#[test]
fn serialize() {
    use crate::types::Tag;

    let filter = serde_json::to_string_pretty(&vec![
        Filter {
            conditions: vec![
                Condition {
                    negate: false,
                    tag: Tag::Gremium,
                    pattern: "^(Rat|Bezirksvertretung Bad Godesberg)$".parse().unwrap(),
                },
                Condition {
                    negate: true,
                    tag: Tag::Verfasser,
                    pattern: "BBB".parse().unwrap(),
                },
            ],
        },
        Filter {
            conditions: vec![
                Condition {
                    negate: false,
                    tag: Tag::Beteiligt,
                    pattern: "^61".parse().unwrap(),
                },
                Condition {
                    negate: true,
                    tag: Tag::Federführend,
                    pattern: "^61-3".parse().unwrap(),
                },
            ],
        },
    ])
    .unwrap();

    println!("{filter}");
}

struct ChatWorker<'a> {
    chat_id: ChatId,
    shared: &'a SharedResources,
    last_processed: Option<StreamId>,
    rate_limit: Interval,
    filters: Vec<Filter>,
}

enum WorkerResult {
    Ok,
    Stopped,
    TokenInvalid,
    MigratedTo(ChatId),
}

struct MessageAcknowledged<'a> {
    id: StreamId,
    _lock: RwLockReadGuard<'a, bool>,
}

impl<'a> MessageAcknowledged<'a> {
    async fn acknowledged(
        id: StreamId,
        worker: &mut ChatWorker<'a>,
    ) -> database::Result<Option<Self>> {
        let lock = match worker.shared.hard_shutdown.try_read() {
            Ok(lock) if !*lock => lock,
            _ => return Ok(None),
        };

        if worker.acknowledge_message(id).await? {
            Ok(Some(Self { id, _lock: lock }))
        } else {
            Ok(None)
        }
    }

    async fn unacknowledge(self, worker: &mut ChatWorker<'_>) -> database::Result<()> {
        worker.unacknowledge_message(self.id).await?;

        Ok(())
    }
}

impl<'a> ChatWorker<'a> {
    fn new(chat_id: ChatId, shared: &'a SharedResources) -> Self {
        ChatWorker {
            chat_id,
            shared,
            rate_limit: chat_rate_limit(chat_id < 0),
            last_processed: None,
            filters: vec![],
        }
    }

    async fn run(mut self) -> (Self, database::Result<WorkerResult>) {
        let future = async {
            let last_processed = if let Some(id) = self.last_processed {
                id
            } else {
                match self.shared.db.get_chat_state(self.chat_id).await? {
                    ChatState::Active { last_sent } => last_sent,
                    ChatState::Migrated { to } => {
                        return Ok(WorkerResult::MigratedTo(to));
                    }
                    ChatState::Stopped => {
                        return Ok(WorkerResult::Stopped);
                    }
                }
            };

            self.last_processed = Some(last_processed);

            match self.get_next_message(last_processed).await?.as_deref() {
                Some((id, message)) => self.process_message(*id, message).await,
                None => Ok(WorkerResult::Ok),
            }
        };

        let result = future.await;

        (self, result)
    }

    async fn get_next_message(
        &self,
        last_processed: StreamId,
    ) -> database::Result<Option<CacheItem<(StreamId, Message)>>> {
        let get_next = || async {
            match self.shared.db.get_next_message(last_processed).await {
                Ok(Some(r)) => Ok(r),
                Ok(None) => Err(None),
                Err(e) => Err(Some(e)),
            }
        };

        let result = self
            .shared
            .next_message_cache
            .get(last_processed, get_next)
            .await;

        match result {
            Ok(next) => Ok(Some(next)),
            Err(None) => Ok(None),
            Err(Some(e)) => Err(e),
        }
    }

    async fn process_message(
        &mut self,
        id: StreamId,
        message: &Message,
    ) -> database::Result<WorkerResult> {
        if !self.should_send(message) {
            self.update_filters().await?;
            if !self.should_send(message) {
                self.acknowledge_message(id).await?;
                return Ok(WorkerResult::Ok);
            }
        }

        self.rate_limit.tick().await;

        let Ok(permit) = self.shared.semaphore.acquire().await else {
            return Ok(WorkerResult::Ok);
        };
        let permit_acquired = Instant::now();

        let (was_sent, result) = self.send_message(id, message).await?;

        if was_sent {
            sleep_until(permit_acquired + Duration::from_secs_f32(DELAY_AFTER_SEND)).await;
        } else {
            self.rate_limit.reset_immediately();
        }

        drop(permit);

        Ok(result)
    }

    async fn handle_response(
        &mut self,
        response: Result<impl Sized, RequestError>,
        backoff: Option<Duration>,
        acknowledged: MessageAcknowledged<'a>,
    ) -> database::Result<ControlFlow<WorkerResult, Duration>> {
        let response = response.inspect_err(|e| log::warn!("Failed to send message: {e}"));

        let result = match response {
            Ok(_) => ControlFlow::Break(WorkerResult::Ok),
            Err(RequestError::Api(e)) if is_chat_invalid(&e) => {
                self.shared.db.remove_subscription(self.chat_id).await?;
                self.last_processed = None;
                ControlFlow::Break(WorkerResult::Stopped)
            }
            Err(RequestError::Api(teloxide::ApiError::InvalidToken)) => {
                // token revoked?
                ControlFlow::Break(WorkerResult::TokenInvalid)
            }
            Err(RequestError::MigrateToChatId(new_chat_id)) => {
                acknowledged.unacknowledge(self).await?;
                self.shared
                    .db
                    .migrate_chat(self.chat_id, new_chat_id.0)
                    .await?;
                self.last_processed = None;
                ControlFlow::Break(WorkerResult::MigratedTo(new_chat_id.0))
            }
            Err(RequestError::RetryAfter(secs)) => {
                acknowledged.unacknowledge(self).await?;
                ControlFlow::Continue(secs.duration())
            }
            _ => {
                if let Some(backoff) = backoff {
                    acknowledged.unacknowledge(self).await?;
                    ControlFlow::Continue(backoff)
                } else {
                    log::warn!("Failed definitely, not retrying!");
                    ControlFlow::Break(WorkerResult::Ok)
                }
            }
        };

        Ok(result)
    }

    async fn send_message(
        &mut self,
        id: StreamId,
        message: &Message,
    ) -> database::Result<(bool, WorkerResult)> {
        let mut backoff = ExponentialBackoff::from_millis(10)
            .factor(10)
            .max_delay(Duration::from_secs(30))
            .map(jitter)
            .take(5);

        loop {
            self.update_filters().await?;

            let acknowledged = MessageAcknowledged::acknowledged(id, self).await?;

            let (Some(acknowledged), true) = (acknowledged, self.should_send(message)) else {
                return Ok((false, WorkerResult::Ok));
            };

            let mut request = self
                .shared
                .bot
                .send_message(teloxide::types::ChatId(self.chat_id), &message.text)
                .parse_mode(message.parse_mode);

            if !message.buttons.is_empty() {
                request =
                    request.reply_markup(InlineKeyboardMarkup::new(vec![message.buttons.clone()]));
            }

            log::debug!(
                "Sending {id}, {}...",
                &message.text.chars().take(20).collect::<String>()
            );

            let response = request.await;
            log::debug!("Sent {id}, response: {response:?}",);

            match self
                .handle_response(response, backoff.next(), acknowledged)
                .await?
            {
                ControlFlow::Break(result) => {
                    return Ok((true, result));
                }
                ControlFlow::Continue(retry_after) => {
                    sleep(retry_after).await;
                }
            }
        }
    }

    async fn acknowledge_message(&mut self, id: StreamId) -> database::Result<bool> {
        let result = self.shared.db.acknowledge_message(self.chat_id, id).await;

        log::debug!("Ack {id}: {result:?}");

        self.last_processed = match result {
            Ok(true) => Some(id),
            _ => None,
        };

        result
    }

    async fn unacknowledge_message(&mut self, id: StreamId) -> database::Result<bool> {
        let result = self.shared.db.unacknowledge_message(self.chat_id, id).await;

        log::debug!("Unack {id}: {result:?}");

        if matches!(result, Ok(None)) {
            log::warn!("Queue got out of sync!");
        }

        self.last_processed = result.as_ref().ok().copied().flatten();

        result.map(|x| x.is_some())
    }

    async fn update_filters(&mut self) -> database::Result<()> {
        self.filters = self.shared.db.get_filters(self.chat_id).await?;
        Ok(())
    }

    fn should_send(&mut self, message: &Message) -> bool {
        self.filters.iter().any(|filter| filter.matches(message))
    }
}

enum WorkerState<'a> {
    Idle {
        worker: ChatWorker<'a>,
        since: Instant,
    },
    Running {
        restart: bool,
    },
}

pub enum ShutdownSignal {
    Soft,
    Hard,
}

struct BroadcastTaskContext<'a, Fut, F: Fn(ChatWorker<'a>) -> Fut> {
    shared: &'a SharedResources,
    latest_entry_id: Option<StreamId>,
    worker_states: HashMap<i64, WorkerState<'a>>,
    run_worker: F,
    running_workers: FuturesUnordered<Fut>,
}

impl<'a, Fut, F: Fn(ChatWorker<'a>) -> Fut> BroadcastTaskContext<'a, Fut, F> {
    fn send_next_if_available(&mut self, worker: ChatWorker<'a>) {
        if worker.last_processed < self.latest_entry_id {
            self.worker_states
                .insert(worker.chat_id, WorkerState::Running { restart: false });

            self.running_workers.push((self.run_worker)(worker));
        } else {
            self.set_idle(worker);
        }
    }

    fn set_idle(&mut self, worker: ChatWorker<'a>) {
        self.worker_states.insert(
            worker.chat_id,
            WorkerState::Idle {
                worker,
                since: Instant::now(),
            },
        );
    }

    fn trigger_chat(&mut self, chat_id: ChatId) {
        match self.worker_states.remove(&chat_id) {
            Some(WorkerState::Idle { worker, .. }) => {
                self.send_next_if_available(worker);
            }
            Some(WorkerState::Running { .. }) => {
                let new_state = WorkerState::Running { restart: true };
                self.worker_states.insert(chat_id, new_state);
            }
            None => {
                self.send_next_if_available(ChatWorker::new(chat_id, self.shared));
            }
        }
    }

    fn cleanup(&mut self) {
        let now = Instant::now();
        self.worker_states.retain(|_, state| match state {
            WorkerState::Idle { since, .. } => {
                now.duration_since(*since) < Duration::from_secs(3600)
            }
            WorkerState::Running { .. } => true,
        });
    }

    fn message_scheduled(&mut self, id: StreamId, active_chats: Vec<ChatId>) {
        self.latest_entry_id = Some(id);

        for chat_id in active_chats {
            self.trigger_chat(chat_id);
        }
    }

    fn handle_worker_finished(
        &mut self,
        (mut worker, result): (ChatWorker<'a>, database::Result<WorkerResult>),
    ) -> Result<(), ()> {
        let send_next = match result {
            Ok(WorkerResult::Ok) => true,
            Ok(WorkerResult::Stopped) => match self.worker_states.get(&worker.chat_id) {
                Some(&WorkerState::Running { restart }) => restart,
                _ => false,
            },
            Ok(WorkerResult::MigratedTo(new_id)) => {
                self.trigger_chat(new_id);
                false
            }
            Ok(WorkerResult::TokenInvalid) => return Err(()),
            Err(e) => {
                log::error!("Database error: {e}");
                worker.last_processed = None;
                false
            }
        };

        if send_next {
            self.send_next_if_available(worker);
        } else {
            self.set_idle(worker);
        }

        Ok(())
    }

    fn next_message_ready(
        &self,
        mut conn: DatabaseConnection,
        was_error: bool,
    ) -> impl Future<
        Output = (
            database::Result<(StreamId, Vec<ChatId>)>,
            DatabaseConnection,
        ),
    > + 'static {
        let id = self.latest_entry_id;

        async move {
            if was_error {
                sleep(Duration::from_secs(60)).await;
            }

            let result = async {
                let id = conn.next_message_ready(id).await?;
                let active_chats = conn.get_active_chats().await?;
                Ok((id, active_chats))
            }
            .await;

            (result, conn)
        }
    }
}

pub async fn broadcast_task(
    bot: crate::Bot,
    db: redis::Client,
    mut shutdown_rx: mpsc::UnboundedReceiver<ShutdownSignal>,
) {
    let conn = DatabaseConnection::new(db.clone(), None);

    let shared = SharedResources {
        bot,
        db: SharedDatabaseConnection::new(conn),
        semaphore: Semaphore::new(PARALLEL_SENDS),
        hard_shutdown: RwLock::new(false),
        next_message_cache: Cache::new(Lru::new(15)),
    };

    let mut soft_shutdown = false;

    let mut cx = BroadcastTaskContext {
        shared: &shared,
        latest_entry_id: None,
        worker_states: HashMap::new(),
        run_worker: |worker| worker.run(),
        running_workers: FuturesUnordered::new(),
    };

    let mut cleanup_timer = interval(Duration::from_secs(3600));
    cleanup_timer.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let conn = DatabaseConnection::new(db, None);
    let mut next_message_ready = pin!(cx.next_message_ready(conn, false));

    while !(soft_shutdown && cx.running_workers.is_empty()) {
        tokio::select! {
            biased;
            signal = shutdown_rx.recv() => match signal {
                Some(ShutdownSignal::Soft) => soft_shutdown = true,
                Some(ShutdownSignal::Hard) | None => break
            },
            (result, conn) = &mut next_message_ready => {
                let was_error = result.is_err();

                match result {
                    Ok((id, active_chats)) => cx.message_scheduled(id, active_chats),
                    Err(e) => log::error!("Database error: {e}")
                };

                next_message_ready.set(cx.next_message_ready(conn, was_error));
            },
            Some(result) = cx.running_workers.next(), if !cx.running_workers.is_empty() => {
                if cx.handle_worker_finished(result).is_err() {
                    log::error!("Bot token invalid, aborting!");
                    return
                }
            }
            _ = cleanup_timer.tick() => cx.cleanup(),
        }
    }

    shared.semaphore.close();

    tokio::select! {
        _ = cx.running_workers.for_each(|_| async {}) => (),
        mut lock = shared.hard_shutdown.write() => *lock = true
    };
}

fn is_chat_invalid(e: &teloxide::ApiError) -> bool {
    use teloxide::ApiError::*;

    match e {
        BotBlocked
        | ChatNotFound
        | GroupDeactivated
        | BotKicked
        | BotKickedFromSupergroup
        | UserDeactivated
        | CantInitiateConversation
        | NotEnoughRightsToPostMessages
        | CantTalkWithBots => true,
        Unknown(err) => ADDITIONAL_ERRORS.contains(&err.as_str()),
        _ => false,
    }
}

// As soon as this fails, `ADDITIONAL_ERRORS` must be adapted
#[test]
fn test_api_error_not_yet_added() {
    use teloxide::ApiError;

    for msg in ADDITIONAL_ERRORS {
        let api_error: ApiError = serde_json::from_str(&format!("\"{msg}\"")).unwrap();
        assert_eq!(api_error, ApiError::Unknown(msg.to_string()));
        assert!(is_chat_invalid(&api_error));
    }
}
