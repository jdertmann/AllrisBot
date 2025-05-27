//! Schedules and broadcasts messages to chats in a rate-limited and fault-tolerant way.

// The `broadcast_task` function forms the main loop of this module and is responsible
// for triggering per-chat processing until each chat has caught up with the most recent
// entry of the `receive_updates` stream.
//
// Processing for each chat consists of:
// 1. Retrieving and preprocessing of the next message from the backend.
// 2. Sending it to the sender task.
// 3. Waiting for the sender task's confirmation that the message was sent.
// 4. Sleeping for a short duration to comply with per-chat rate limits.
//
// The sender task receives filtered messages and handles the actual delivery while enforcing
// a global broadcast rate limit.

// TODO: if filter was checked a long time ago, check it again before sending
// TODO: allow sending multiple messages per update

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::error::Error;
use std::fmt::Debug;
use std::hash::Hash;
use std::ops::ControlFlow;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{FusedStream, FuturesUnordered, Stream, StreamExt as _};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval, sleep, sleep_until};
use tracing::instrument;

use super::ChatId;
use crate::response::RequestError;

const BROADCASTS_PER_SECOND: f32 = 30.;
const MESSAGE_INTERVAL_CHAT: Duration = Duration::from_secs(1);
const MESSAGE_INTERVAL_GROUP: Duration = Duration::from_secs(3);

#[derive(Debug)]
enum ChatStatus<U> {
    Processed(U),
    OutOfSync,
    Stopped,
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

type OneshotResponse<B> = (
    Result<ChatStatus<<B as Backend>::UpdateId>, <B as Backend>::Error>,
    bool,
);
type SendMessage<B> = (ScheduledMessage<B>, oneshot::Sender<OneshotResponse<B>>);

pub enum NextUpdate<B: Backend> {
    Ready { id: B::UpdateId, msg: B::Message },
    Skipped { id: B::UpdateId },
    OutOfSync,
    Pending { previous: B::UpdateId },
    Migrated { to: ChatId },
    Stopped,
}

macro_rules! ret_ty {
    ($x:ty, $e:ty ) => {
        impl Future<Output = Result<$x, $e>> + Send
    };
    ($x:ty) => {
        ret_ty!($x, Self::Error)
    };
}

pub trait Backend: Send + Sync + Sized + 'static {
    type UpdateId: Debug + Hash + Ord + Send + Sync + Copy + 'static;

    type Message: Debug + Send + Sync + 'static;

    type Error: Error + Send + 'static;

    /// Returns a stream that first yields the id of the latest update as soon as possible, and then
    /// yields whenever there are new updates with a later UpdateId. When it returns None, a soft shutdown
    /// is initiated.
    fn receive_updates(&self)
    -> impl Stream<Item = (Self::UpdateId, Vec<ChatId>)> + Send + 'static;

    fn next_update(&self, chat: ChatId) -> ret_ty![NextUpdate<Self>];

    fn send(&self, chat: ChatId, message: &Self::Message) -> ret_ty![(), frankenstein::Error];

    fn acknowledge(&self, chat: ChatId, update: Self::UpdateId) -> ret_ty![bool];

    fn unacknowledge(&self, chat: ChatId, update: Self::UpdateId) -> ret_ty![bool];

    fn migrate_chat(&self, old: ChatId, new: ChatId) -> ret_ty![bool];

    fn remove_chat(&self, id: ChatId) -> ret_ty![bool];
}

struct SharedDependencies<B: Backend> {
    backend: B,
    hard_shutdown: watch::Sender<bool>,
    sender_tx: mpsc::Sender<SendMessage<B>>,
}

fn backoff_strategy() -> impl Iterator<Item = Duration> {
    (1..=6).map(|i| {
        let millis = 10 * 6_u64.pow(i);
        let millis = millis.min(120_000);
        Duration::from_millis(millis)
    })
}

/// A message that is scheduled to be sent to a certain chat
struct ScheduledMessage<B: Backend> {
    pub chat_id: ChatId,
    pub update: B::UpdateId,
    pub message: B::Message,
}

impl<B: Backend> ScheduledMessage<B> {
    async fn unacknowledge(&self, shared: &SharedDependencies<B>) -> Result<bool, B::Error> {
        let r = shared
            .backend
            .unacknowledge(self.chat_id, self.update)
            .await?;
        if !r {
            tracing::warn!("Failed to unacknowledge message!");
        }
        Ok(r)
    }

    async fn handle_response(
        &self,
        shared: &SharedDependencies<B>,
        response: Result<(), frankenstein::Error>,
        backoff: Option<Duration>,
    ) -> Result<ControlFlow<ChatStatus<B::UpdateId>, Duration>, B::Error> {
        if let Err(e) = response.as_ref() {
            tracing::error!(error=%e, "Sending message failed");
        } else {
            tracing::info!("Message has been sent!");
        }

        macro_rules! retry_with_backoff {
            ($dur:expr) => {
                if self.unacknowledge(shared).await? {
                    return Ok(ControlFlow::Continue($dur));
                } else {
                    ChatStatus::OutOfSync
                }
            };
        }

        let response = response.as_ref().map_err(crate::response::map_error);

        let result = match response {
            Ok(_) => ChatStatus::Processed(self.update),
            Err(RequestError::InvalidToken) => {
                tracing::error!("Invalid token! Was it revoked?");
                shared.hard_shutdown.send_replace(true);
                _ = self.unacknowledge(shared).await?;
                ChatStatus::ShuttingDown
            }
            Err(RequestError::BotBlocked) => {
                shared.backend.remove_chat(self.chat_id).await?;
                tracing::info!("Bot is unable to send to this chat, subscription was removed!");
                ChatStatus::Stopped
            }
            Err(RequestError::ChatMigrated(new_chat_id)) => {
                _ = self.unacknowledge(shared).await?;
                shared
                    .backend
                    .migrate_chat(self.chat_id, new_chat_id)
                    .await?;
                tracing::info!("Chat has been migrated to {new_chat_id}!");
                ChatStatus::MigratedTo(new_chat_id)
            }
            Err(RequestError::RetryAfter(dur)) => retry_with_backoff!(dur),
            Err(RequestError::ClientError) => {
                tracing::error!("Client error, won't retry!");
                ChatStatus::Processed(self.update)
            }
            Err(RequestError::Other) => {
                if let Some(backoff) = backoff {
                    retry_with_backoff!(backoff)
                } else {
                    tracing::error!("Max number of retries reached, won't retry!");
                    ChatStatus::Processed(self.update)
                }
            }
        };

        Ok(ControlFlow::Break(result))
    }

    /// Sends a message. Will retry a number of times if it fails
    #[tracing::instrument(skip_all, fields(chat_id=self.chat_id, update_id=?self.update))]
    async fn send_message(
        &self,
        shared: &SharedDependencies<B>,
        message_sent: &mut bool,
    ) -> Result<ChatStatus<B::UpdateId>, B::Error> {
        let mut backoff = backoff_strategy();

        loop {
            tracing::debug!("Starting attempt to send message!");
            *message_sent = false;
            let ack = shared
                .backend
                .acknowledge(self.chat_id, self.update)
                .await?;
            if !ack {
                tracing::warn!("Failed to acknowledged message!");
                return Ok(ChatStatus::OutOfSync);
            }
            tracing::trace!("Message was acknowledged, trying to send it!");
            let response = shared.backend.send(self.chat_id, &self.message).await;
            *message_sent = true;

            match self
                .handle_response(shared, response, backoff.next())
                .await?
            {
                ControlFlow::Break(result) => {
                    tracing::debug!("Message was sent or failed definitely");
                    return Ok(result);
                }
                ControlFlow::Continue(retry_after) => {
                    tracing::info!("Retrying in {retry_after:?} ...");
                    sleep(retry_after).await;
                }
            }
        }
    }
}

/// Process the next entry of the message stream for a certain chat
#[instrument(skip(shared), ret(level = "debug"))]
async fn process_next_update<B: Backend>(
    shared: &SharedDependencies<B>,
    chat_id: ChatId,
) -> Result<ChatStatus<B::UpdateId>, B::Error> {
    tracing::debug!("Processing next update");
    let started = Instant::now();

    let (update, message) = match shared.backend.next_update(chat_id).await? {
        NextUpdate::Ready { id, msg: next } => (id, next),
        NextUpdate::Skipped { id } => return Ok(ChatStatus::Processed(id)),
        NextUpdate::OutOfSync => return Ok(ChatStatus::OutOfSync),
        NextUpdate::Pending { previous: last } => return Ok(ChatStatus::Processed(last)),
        NextUpdate::Migrated { to } => return Ok(ChatStatus::MigratedTo(to)),
        NextUpdate::Stopped => return Ok(ChatStatus::Stopped),
    };

    // pass the message to the sender task
    let scheduled = ScheduledMessage {
        chat_id,
        update,
        message,
    };
    let (oneshot_tx, oneshot_rx) = oneshot::channel();
    _ = shared.sender_tx.send((scheduled, oneshot_tx)).await;

    match oneshot_rx.await {
        Ok((r, true)) => {
            // message has been sent, apply a delay for rate limiting
            tracing::debug!("Applying delay for rate limiting");
            sleep_until(started + delay(chat_id)).await;
            r
        }
        Ok((r, false)) => {
            // message has not been sent
            r
        }
        Err(_) => {
            // sender task apparently not running anymore
            Ok(ChatStatus::ShuttingDown)
        }
    }
}

async fn sender_task<B: Backend>(
    shared: Arc<SharedDependencies<B>>,
    mut sender_rx: mpsc::Receiver<SendMessage<B>>,
) {
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
    triggered_while_running: bool,
}

enum ShutdownSignal {
    Soft,
    Hard,
}

struct BroadcastManager<'a, B: Backend, Fut, F: Fn(&'a SharedDependencies<B>, ChatId) -> Fut> {
    shared: &'a SharedDependencies<B>,
    latest_entry_id: Option<B::UpdateId>,
    states: HashMap<ChatId, ProcessingState>,
    process_next_message: F,
    processing: FuturesUnordered<Fut>,
}

impl<'a, B: Backend, Fut, F: Fn(&'a SharedDependencies<B>, ChatId) -> Fut>
    BroadcastManager<'a, B, Fut, F>
{
    /// should be called if there's possibly a new message for this chat
    fn trigger_chat(&mut self, chat_id: ChatId) {
        match self.states.entry(chat_id) {
            Entry::Occupied(mut entry) => {
                tracing::debug!("Triggered chat {chat_id} already running");
                entry.get_mut().triggered_while_running = true;
            }
            Entry::Vacant(entry) => {
                self.processing
                    .push((self.process_next_message)(self.shared, chat_id));
                entry.insert(ProcessingState::default());
            }
        }
    }

    /// triggers all active chats after a new message has arrived
    fn on_message_scheduled(&mut self, id: B::UpdateId, active_chats: Vec<ChatId>) {
        tracing::info!(
            active_chats = active_chats.len(),
            "Latest scheduled message: {id:?}"
        );
        self.latest_entry_id = Some(id);

        for chat_id in active_chats {
            self.trigger_chat(chat_id);
        }
    }

    fn on_processing_finished(
        &mut self,
        chat_id: ChatId,
        result: Result<ChatStatus<B::UpdateId>, B::Error>,
    ) {
        let restart = self
            .states
            .remove(&chat_id)
            .map(|s| s.triggered_while_running)
            .unwrap_or_else(|| {
                tracing::warn!(chat_id, "ProcessingState is missing unexpectedly");
                true // restart task to be on the safe site
            });
        match result {
            Ok(ChatStatus::Processed(stream_id)) => {
                if Some(stream_id) < self.latest_entry_id {
                    self.trigger_chat(chat_id);
                }
            }
            Ok(ChatStatus::OutOfSync) => self.trigger_chat(chat_id),
            Ok(ChatStatus::Stopped) => {
                if restart {
                    // It's possible that a user unsubscribes and then quickly re-subscribes.
                    // In such cases, the previous task might report `ChatStopped`, even though the
                    // chat is active again. We avoid prematurely stopping processing by checking if the chat
                    // was re-triggered during that time. Ignoring these `ChatStopped` results is
                    // harmless and helps avoid missing messages.

                    self.trigger_chat(chat_id);
                }
            }
            Ok(ChatStatus::MigratedTo(chat_id)) => self.trigger_chat(chat_id),
            Ok(ChatStatus::ShuttingDown) => (),
            Err(e) => tracing::error!(error=%e, "Processing chat failed"),
        }
    }
}

async fn broadcast_task(backend: impl Backend, mut shutdown_rx: mpsc::Receiver<ShutdownSignal>) {
    let (sender_tx, sender_rx) = mpsc::channel(3);
    let shared = Arc::new(SharedDependencies {
        sender_tx,
        backend,
        hard_shutdown: watch::Sender::new(false),
    });

    let mut sender_handle = tokio::spawn(sender_task(shared.clone(), sender_rx));
    let mut soft_shutdown = false;
    let mut updates = pin!(shared.backend.receive_updates().fuse());
    let mut manager = BroadcastManager {
        shared: &shared,
        latest_entry_id: None,
        states: HashMap::new(),
        process_next_message: |shared, chat_id| async move {
            let result = process_next_update(shared, chat_id).await;
            (chat_id, result)
        },
        processing: FuturesUnordered::new(),
    };

    while !(soft_shutdown && manager.processing.is_empty()) {
        tokio::select! {
            biased;
            _ = &mut sender_handle => return,
            signal = shutdown_rx.recv() => match signal {
                Some(ShutdownSignal::Soft) => {
                    tracing::info!("Received soft shutdown signal");
                    soft_shutdown = true;
                }
                Some(ShutdownSignal::Hard) => {
                    tracing::info!("Received hard shutdown signal");
                    break;
                }
                None => {
                    tracing::warn!("Shutdown channel closed unexpectedly");
                    break;
                }
            },
            item = updates.next(), if !updates.is_terminated() => {
                if let Some((id,active_chats)) = item {
                    manager.on_message_scheduled(id, active_chats)
                } else {
                    tracing::info!("Scheduled messages stream is terminated, doing soft shutdown");
                    soft_shutdown = true;
                }
            },
            Some((chat_id, result)) = manager.processing.next(), if !manager.processing.is_empty() => {
                manager.on_processing_finished(chat_id, result);
            }
        }
    }

    // notify the sender task to stop after the next message
    shared.hard_shutdown.send_replace(true);
    let _ = sender_handle.await;
}

pub struct Broadcaster {
    shutdown_tx: mpsc::Sender<ShutdownSignal>,
    handle: JoinHandle<()>,
}

impl Broadcaster {
    pub fn new(backend: impl Backend) -> Self {
        let (shutdown_tx, shutdown_rx) = mpsc::channel(2);
        let handle = tokio::spawn(broadcast_task(backend, shutdown_rx));
        Self {
            shutdown_tx,
            handle,
        }
    }

    pub async fn soft_shutdown(&mut self) {
        _ = self.shutdown_tx.send(ShutdownSignal::Soft).await;

        if !self.handle.is_finished() {
            _ = (&mut self.handle).await;
        }
    }

    pub async fn hard_shutdown(self) {
        _ = self.shutdown_tx.send(ShutdownSignal::Hard).await;

        if !self.handle.is_finished() {
            _ = self.handle.await;
        }
    }
}
