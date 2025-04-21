use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use frankenstein::AsyncTelegramApi;
use frankenstein::methods::GetUpdatesParams;
use frankenstein::types::{AllowedUpdate, Message};
use frankenstein::updates::UpdateContent;
use futures_util::FutureExt;
use tokio::select;
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinSet;
use tokio::time::sleep;

const CLEANUP_PERIOD: Duration = Duration::from_secs(300);

pub trait UpdateHandler: Clone + Send + 'static {
    fn handle_message(self, message: Message) -> impl Future<Output = ()> + Send;
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
pub async fn handle_updates(
    bot: crate::Bot,
    handler: impl UpdateHandler,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut mutexes: HashMap<i64, Weak<Mutex<()>>> = HashMap::new();
    let mut last_cleanup = Instant::now();

    let mut join_set = JoinSet::new();
    let mut marked_seen = true;

    let mut params = GetUpdatesParams::builder()
        .timeout(30)
        .allowed_updates(vec![AllowedUpdate::Message])
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

                    let msg = match update.content {
                        UpdateContent::Message(msg) => msg,
                        _ => {
                            log::warn!("Received unexpected update: {:?}", update.content);
                            continue;
                        }
                    };

                    let mutex = mutexes
                        .get(&msg.chat.id)
                        .and_then(|weak| weak.upgrade())
                        .unwrap_or_else(|| {
                            let mutex = Default::default();
                            mutexes.insert(msg.chat.id, Arc::downgrade(&mutex));
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

                        handler.handle_message(msg).await;
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
