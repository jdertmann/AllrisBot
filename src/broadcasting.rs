mod lru_cache;
mod message_sender;

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{FuturesUnordered, StreamExt as _};
use lru_cache::Lru;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, MissedTickBehavior, interval, sleep, sleep_until};

use self::lru_cache::Cache;
use self::message_sender::MessageSender;
use crate::database::{self, ChatState, DatabaseConnection, SharedDatabaseConnection, StreamId};
use crate::types::{ChatId, Condition, Filter, Message};

const BROADCASTS_PER_SECOND: f32 = 30.;
const MESSAGE_INTERVAL_CHAT: Duration = Duration::from_secs(1);
const MESSAGE_INTERVAL_GROUP: Duration = Duration::from_secs(3);

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

enum WorkerResult {
    Processed(StreamId),
    OutOfSync,
    ChatStopped,
    ShuttingDown,
    MigratedTo(ChatId),
}

fn delay(chat_id: ChatId) -> Duration {
    if chat_id < 0 {
        MESSAGE_INTERVAL_GROUP
    } else {
        MESSAGE_INTERVAL_CHAT
    }
}

type OneshotResponse = (database::Result<WorkerResult>, bool);
type SendMessage = (MessageSender, oneshot::Sender<OneshotResponse>);

struct BroadcastResources {
    bot: crate::Bot,
    db: SharedDatabaseConnection,
    hard_shutdown: watch::Sender<bool>,
    next_message_cache: Cache<StreamId, (StreamId, Message), Lru<StreamId>>,
    sender_tx: mpsc::Sender<SendMessage>,
}

impl BroadcastResources {
    async fn try_process_next(&self, chat_id: ChatId) -> database::Result<WorkerResult> {
        let started = Instant::now();

        let last_sent = match self.db.get_chat_state(chat_id).await? {
            ChatState::Active { last_sent } => last_sent,
            ChatState::Migrated { to } => return Ok(WorkerResult::MigratedTo(to)),
            ChatState::Stopped => return Ok(WorkerResult::ChatStopped),
        };

        let next_message = self
            .next_message_cache
            .get_some(last_sent, || self.db.get_next_message(last_sent))
            .await?;

        let sender = match next_message {
            Some(next) => MessageSender::new(chat_id, next),
            None => return Ok(WorkerResult::Processed(last_sent)),
        };

        if !sender.check_filters(self).await? {
            if sender.acknowledge_message(self).await? {
                return Ok(WorkerResult::Processed(sender.message_id()));
            } else {
                return Ok(WorkerResult::OutOfSync);
            }
        }

        let (oneshot_tx, oneshot_rx) = oneshot::channel();

        if self.sender_tx.send((sender, oneshot_tx)).await.is_err() {
            return Ok(WorkerResult::ShuttingDown);
        }

        match oneshot_rx.await {
            Ok((r, true)) => {
                sleep_until(started + delay(chat_id)).await;
                r
            }
            Ok((r, false)) => r,
            Err(_) => Ok(WorkerResult::ShuttingDown),
        }
    }

    async fn sender_task(self: Arc<Self>, mut sender_rx: mpsc::Receiver<SendMessage>) {
        let mut shutdown = self.hard_shutdown.subscribe();
        let mut interval = interval(Duration::from_secs_f32(1. / BROADCASTS_PER_SECOND));
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            let recv = async {
                interval.tick().await;
                sender_rx.recv().await
            };

            let (sender, result_tx) = tokio::select! {
                biased;
                _ = shutdown.wait_for(|x| *x) => break,
                next = recv => match next {
                    Some(next) => next,
                    None => break
                }
            };

            let mut message_sent = false;
            let result = sender.send_message(&self, &mut message_sent).await;
            let _ = result_tx.send((result, message_sent));
        }
    }
}

#[derive(Default)]
struct ProcessingState {
    should_restart: bool,
}

pub enum ShutdownSignal {
    Soft,
    Hard,
}

struct BroadcastManager<'a, Fut, F: Fn(&'a BroadcastResources, ChatId) -> Fut> {
    resources: &'a BroadcastResources,
    latest_entry_id: Option<StreamId>,
    states: HashMap<i64, ProcessingState>,
    process_next_message: F,
    processing: FuturesUnordered<Fut>,
}

impl<'a, Fut, F: Fn(&'a BroadcastResources, ChatId) -> Fut> BroadcastManager<'a, Fut, F> {
    fn trigger_chat(&mut self, chat_id: ChatId) {
        match self.states.entry(chat_id) {
            Entry::Occupied(mut entry) => {
                entry.get_mut().should_restart = true;
            }
            Entry::Vacant(entry) => {
                self.processing
                    .push((self.process_next_message)(self.resources, chat_id));
                entry.insert(ProcessingState::default());
            }
        }
    }

    fn on_message_scheduled(&mut self, id: StreamId, active_chats: Vec<ChatId>) {
        self.latest_entry_id = Some(id);

        for chat_id in active_chats {
            self.trigger_chat(chat_id);
        }
    }

    fn on_processing_finished(&mut self, chat_id: ChatId, result: database::Result<WorkerResult>) {
        let restart = self.states.remove(&chat_id).unwrap().should_restart;
        match result {
            Ok(WorkerResult::Processed(stream_id)) => {
                if Some(stream_id) < self.latest_entry_id {
                    self.trigger_chat(chat_id);
                }
            }
            Ok(WorkerResult::OutOfSync) => self.trigger_chat(chat_id),
            Ok(WorkerResult::ChatStopped) => {
                if restart {
                    self.trigger_chat(chat_id);
                }
            }
            Ok(WorkerResult::MigratedTo(chat_id)) => self.trigger_chat(chat_id),
            Ok(WorkerResult::ShuttingDown) => (),
            Err(e) => log::error!("Database error: {e}"),
        }
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
    let (sender_tx, sender_rx) = mpsc::channel(3);

    let resources = Arc::new(BroadcastResources {
        bot,
        sender_tx,
        db: DatabaseConnection::new(db.clone(), None).shared(),
        hard_shutdown: watch::Sender::new(false),
        next_message_cache: Cache::new(Lru::new(15)),
    });

    let mut sender_handle = tokio::spawn(resources.clone().sender_task(sender_rx));

    let mut soft_shutdown = false;

    let mut manager = BroadcastManager {
        resources: &resources,
        latest_entry_id: None,
        states: HashMap::new(),
        process_next_message: |shared, chat_id| async move {
            let result = shared.try_process_next(chat_id).await;
            (chat_id, result)
        },
        processing: FuturesUnordered::new(),
    };

    let conn = DatabaseConnection::new(db, None);
    let mut next_message_ready = pin!(manager.next_message_ready(conn, false));

    while !(soft_shutdown && manager.processing.is_empty()) {
        tokio::select! {
            biased;
            _ = &mut sender_handle => return,
            signal = shutdown_rx.recv() => match signal {
                Some(ShutdownSignal::Soft) => soft_shutdown = true,
                Some(ShutdownSignal::Hard) | None => break
            },
            (result, conn) = &mut next_message_ready => {
                let was_error = result.is_err();

                match result {
                    Ok((id, active_chats)) => manager.on_message_scheduled(id, active_chats),
                    Err(e) => log::error!("Database error: {e}")
                };

                next_message_ready.set(manager.next_message_ready(conn, was_error));
            },
            Some(result) = manager.processing.next(), if !manager.processing.is_empty() => {
                manager.on_processing_finished(result.0, result.1);
            }
        }
    }

    resources.hard_shutdown.send_replace(true);
    let _ = sender_handle.await;
}
