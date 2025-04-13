use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use frankenstein::AsyncTelegramApi;
use frankenstein::methods::GetUpdatesParams;
use frankenstein::types::{AllowedUpdate, Message};
use frankenstein::updates::UpdateContent;
use tokio::sync::Mutex;
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

pub async fn handle_updates(bot: crate::Bot, handler: impl UpdateHandler) {
    let mut mutexes: HashMap<i64, Weak<Mutex<()>>> = HashMap::new();
    let mut last_cleanup = Instant::now();

    let mut params = GetUpdatesParams::builder()
        .timeout(30)
        .allowed_updates(vec![AllowedUpdate::Message])
        .build();

    loop {
        match bot.get_updates(&params).await {
            Ok(updates) => {
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
                    let acquiring = mutex.lock_owned();

                    let fut = async move {
                        let guard = acquiring.await;
                        handler.handle_message(msg).await;
                        drop(guard)
                    };
                    tokio::spawn(fut);
                }
            }
            Err(e) => {
                log::error!("Error retrieving updates: {:?}", e);
                sleep(Duration::from_secs(5)).await;
            }
        }

        cleanup(&mut last_cleanup, &mut mutexes);
    }
}
