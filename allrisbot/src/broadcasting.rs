//! Schedules and broadcasts messages to chats in a rate-limited and fault-tolerant way.
//!
//! A broadcast is scheduled as a Redis stream entry. The `broadcast_task` function forms the
//! main loop of this module and is responsible for triggering per-chat processing until each
//! chat has caught up with the most recent stream entry.
//!
//! Processing for each chat consists of:
//! 1. Retrieving the next message from the database or cache.
//! 2. Checking if the message matches the user's subscription filters.
//! 3. If it does, sending it to the sender task.
//! 4. Sleeping for a short duration to comply with per-chat rate limits.
//!
//! The sender task receives filtered messages and handles the actual delivery while enforcing
//! a global broadcast rate limit.

mod scheduled_message;

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{FuturesUnordered, StreamExt as _};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, MissedTickBehavior, interval, sleep, sleep_until};

use self::scheduled_message::ScheduledMessage;
use crate::database::{self, ChatState, DatabaseConnection, SharedDatabaseConnection, StreamId};
use crate::lru_cache::{Cache, Lru};
use crate::types::{ChatId, Message};

const BROADCASTS_PER_SECOND: f32 = 30.;
const MESSAGE_INTERVAL_CHAT: Duration = Duration::from_secs(1);
const MESSAGE_INTERVAL_GROUP: Duration = Duration::from_secs(3);

enum ProcessNextResult {
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

type OneshotResponse = (database::Result<ProcessNextResult>, bool);
type SendMessage = (ScheduledMessage, oneshot::Sender<OneshotResponse>);

struct SharedDependencies {
    bot: crate::Bot,
    db: SharedDatabaseConnection,
    hard_shutdown: watch::Sender<bool>,
    next_message_cache: Cache<StreamId, (StreamId, Message), Lru<StreamId>>,
    sender_tx: mpsc::Sender<SendMessage>,
}

/// Process the next entry of the message stream for a certain chat
async fn try_process_next(
    shared: &SharedDependencies,
    chat_id: ChatId,
) -> database::Result<ProcessNextResult> {
    let started = Instant::now();

    let last_sent = match shared.db.get_chat_state(chat_id).await? {
        ChatState::Active { last_sent } => last_sent,
        ChatState::Migrated { to } => return Ok(ProcessNextResult::MigratedTo(to)),
        ChatState::Stopped => return Ok(ProcessNextResult::ChatStopped),
    };

    let next_message = shared
        .next_message_cache
        .get_some(last_sent, || shared.db.get_next_message(last_sent))
        .await?;

    let scheduled = match next_message {
        Some(next) => ScheduledMessage::new(chat_id, next),
        None => return Ok(ProcessNextResult::Processed(last_sent)),
    };

    if !scheduled.check_filters(shared).await? {
        // message should not be sent, early return
        if scheduled.acknowledge_message(shared).await? {
            return Ok(ProcessNextResult::Processed(scheduled.message_id()));
        } else {
            return Ok(ProcessNextResult::OutOfSync);
        }
    }

    // pass the message to the sender task
    let (oneshot_tx, oneshot_rx) = oneshot::channel();

    if shared
        .sender_tx
        .send((scheduled, oneshot_tx))
        .await
        .is_err()
    {
        return Ok(ProcessNextResult::ShuttingDown);
    }

    match oneshot_rx.await {
        Ok((r, true)) => {
            // message has been sent, apply a delay for rate limiting
            sleep_until(started + delay(chat_id)).await;
            r
        }
        Ok((r, false)) => {
            // message has not been sent
            r
        }
        Err(_) => {
            // sender task apparently not running anymore
            Ok(ProcessNextResult::ShuttingDown)
        }
    }
}

async fn sender_task(shared: Arc<SharedDependencies>, mut sender_rx: mpsc::Receiver<SendMessage>) {
    let mut shutdown = shared.hard_shutdown.subscribe();
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
        let result = sender.send_message(&shared, &mut message_sent).await;
        let _ = result_tx.send((result, message_sent));
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

struct BroadcastManager<'a, Fut, F: Fn(&'a SharedDependencies, ChatId) -> Fut> {
    shared: &'a SharedDependencies,
    latest_entry_id: Option<StreamId>,
    states: HashMap<i64, ProcessingState>,
    process_next_message: F,
    processing: FuturesUnordered<Fut>,
}

impl<'a, Fut, F: Fn(&'a SharedDependencies, ChatId) -> Fut> BroadcastManager<'a, Fut, F> {
    /// should be called if there's possibly a new message for this chat
    fn trigger_chat(&mut self, chat_id: ChatId) {
        match self.states.entry(chat_id) {
            Entry::Occupied(mut entry) => {
                entry.get_mut().should_restart = true;
            }
            Entry::Vacant(entry) => {
                self.processing
                    .push((self.process_next_message)(self.shared, chat_id));
                entry.insert(ProcessingState::default());
            }
        }
    }

    /// triggers all active chats after a new message has arrived
    fn on_message_scheduled(&mut self, id: StreamId, active_chats: Vec<ChatId>) {
        self.latest_entry_id = Some(id);

        for chat_id in active_chats {
            self.trigger_chat(chat_id);
        }
    }

    fn on_processing_finished(
        &mut self,
        chat_id: ChatId,
        result: database::Result<ProcessNextResult>,
    ) {
        let restart = self.states.remove(&chat_id).unwrap().should_restart;
        match result {
            Ok(ProcessNextResult::Processed(stream_id)) => {
                if Some(stream_id) < self.latest_entry_id {
                    self.trigger_chat(chat_id);
                }
            }
            Ok(ProcessNextResult::OutOfSync) => self.trigger_chat(chat_id),
            Ok(ProcessNextResult::ChatStopped) => {
                if restart {
                    // It's possible that a user unsubscribes and then quickly re-subscribes.
                    // In such cases, the previous task might report `ChatStopped`, even though the
                    // chat is active again. We avoid prematurely stopping processing by checking if the chat
                    // was re-triggered during that time. Ignoring these outdated `ChatStopped` results is
                    // harmless and helps avoid missing messages.

                    self.trigger_chat(chat_id);
                }
            }
            Ok(ProcessNextResult::MigratedTo(chat_id)) => self.trigger_chat(chat_id),
            Ok(ProcessNextResult::ShuttingDown) => (),
            Err(e) => log::error!("Database error: {e}"),
        }
    }

    /// Returns the next stream id and a list of active chats
    /// as soon as a new entry is added to the message stream
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
                sleep(Duration::from_secs(20)).await;
            }

            let result = async {
                let next_id = if let Some(id) = id {
                    conn.next_message_id_blocking(id).await?
                } else {
                    conn.current_message_id().await?
                };
                let active_chats = conn.get_active_chats().await?;
                Ok((next_id, active_chats))
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

    let shared = Arc::new(SharedDependencies {
        bot,
        sender_tx,
        db: DatabaseConnection::new(db.clone(), None).shared(),
        hard_shutdown: watch::Sender::new(false),
        next_message_cache: Cache::new(Lru::new(15)),
    });

    let mut sender_handle = tokio::spawn(sender_task(shared.clone(), sender_rx));

    let mut soft_shutdown = false;

    let mut manager = BroadcastManager {
        shared: &shared,
        latest_entry_id: None,
        states: HashMap::new(),
        process_next_message: |shared, chat_id| async move {
            let result = try_process_next(shared, chat_id).await;
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

    // notifiy the sender task to stop after the next message
    shared.hard_shutdown.send_replace(true);
    let _ = sender_handle.await;
}
