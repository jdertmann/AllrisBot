use std::collections::HashMap;
use std::fmt::Display;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use frankenstein::AsyncTelegramApi;
use frankenstein::methods::GetUpdatesParams;
use frankenstein::types::{
    AllowedUpdate, CallbackQuery, ChatMemberUpdated, MaybeInaccessibleMessage, Message,
};
use frankenstein::updates::UpdateContent;
use futures_util::FutureExt;
use tokio::select;
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinSet;
use tokio::time::sleep;

const CLEANUP_PERIOD: Duration = Duration::from_secs(300);

#[allow(unused_variables)]
pub trait UpdateHandler: Clone + Send + 'static {
    fn handle_message(self, message: Message) -> impl Future<Output = ()> + Send {
        async {}
    }

    fn handle_my_chat_member(self, update: ChatMemberUpdated) -> impl Future<Output = ()> + Send {
        async {}
    }

    fn handle_callback_query(self, update: CallbackQuery) -> impl Future<Output = ()> + Send {
        async {}
    }
}

fn cleanup(last_cleanup: &mut Instant, mutexes: &mut HashMap<i64, Weak<Mutex<()>>>) {
    let now = Instant::now();

    if now - *last_cleanup < CLEANUP_PERIOD {
        return;
    }

    mutexes.retain(|_, weak| weak.strong_count() > 0);

    *last_cleanup = now;
}

/// Gets new incoming messages and calls `handler` on them, while ensuring that no messages
/// from the same chat are processed in parallel.
pub async fn handle_updates<B: AsyncTelegramApi<Error: Display>>(
    bot: B,
    handler: impl UpdateHandler,
    allowed_updates: Vec<AllowedUpdate>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut mutexes: HashMap<i64, Weak<Mutex<()>>> = HashMap::new();
    let mut last_cleanup = Instant::now();

    let mut join_set = JoinSet::new();
    let mut marked_seen = true;

    let mut params = GetUpdatesParams::builder()
        .timeout(30)
        .allowed_updates(allowed_updates)
        .build();

    loop {
        let updates = select! {
            updates = bot.get_updates(&params) => updates,
            _ = &mut shutdown => break
        };

        match updates {
            Ok(updates) => {
                marked_seen = updates.result.is_empty();
                for update in updates.result {
                    params.offset = Some(update.update_id as i64 + 1);

                    let chat = match &update.content {
                        UpdateContent::Message(msg) => &*msg.chat,
                        UpdateContent::MyChatMember(member) => &member.chat,
                        UpdateContent::CallbackQuery(query) => match &query.message {
                            Some(MaybeInaccessibleMessage::InaccessibleMessage(m)) => &m.chat,
                            Some(MaybeInaccessibleMessage::Message(m)) => &m.chat,
                            None => {
                                log::warn!("Unsupported!");
                                continue;
                            }
                        },
                        _ => {
                            log::warn!("Received unsupported update: {:?}", update.content);
                            continue;
                        }
                    };

                    let mutex = mutexes
                        .get(&chat.id)
                        .and_then(|weak| weak.upgrade())
                        .unwrap_or_else(|| {
                            let mutex = Default::default();
                            mutexes.insert(chat.id, Arc::downgrade(&mutex));
                            mutex
                        });

                    let handler = handler.clone();
                    let mut acquiring = Box::pin(mutex.lock_owned());

                    // to ensure correct order, it's necessary to poll the future
                    // once now (to enqueue it in the mutex' fifo queue)
                    let guard = acquiring.as_mut().now_or_never();

                    let fut = async move {
                        let guard = if let Some(guard) = guard {
                            guard
                        } else {
                            acquiring.await
                        };

                        match update.content {
                            UpdateContent::Message(msg) => handler.handle_message(msg).await,
                            UpdateContent::MyChatMember(msg) => {
                                handler.handle_my_chat_member(msg).await
                            }
                            UpdateContent::CallbackQuery(q) => {
                                handler.handle_callback_query(q).await;
                            }
                            _ => log::warn!("Unreachable code reached!"),
                        }
                        drop(guard)
                    };
                    join_set.spawn(fut);
                }
            }
            Err(e) => {
                log::error!("Error retrieving updates: {}", e);
                sleep(Duration::from_secs(5)).await;
            }
        }

        cleanup(&mut last_cleanup, &mut mutexes);
    }

    // just mark as seen, but don't handle the response
    if !marked_seen {
        params.timeout = Some(0);
        params.limit = Some(1);

        if let Err(e) = bot.get_updates(&params).await {
            log::error!("Error marking messages as seen: {}", e);
        }
    }

    join_set.join_all().await;
}
