use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use teloxide::prelude::*;
use teloxide::types::InlineKeyboardMarkup;
use tokio::task::JoinHandle;

use crate::database::{DatabaseClient, Message};
use crate::Bot;

const RETRY_LIMIT: usize = 5;
const BASE_RETRY_DELAY: Duration = Duration::from_millis(500);
const RETRY_MULTIPLIER: u32 = 3;
const ADDITIONAL_ERRORS: &[&str] = &[
    "Forbidden: bot was kicked from the channel chat",
    "Forbidden: bot was kicked from the group chat",
    "Bad Request: not enough rights to send text messages to the chat",
];

/// Defines possible outcomes when sending a message.
enum ChatStatus {
    Active,
    Invalid,
    Migrated(ChatId),
}

/// Rate limiter for handling API request limits.
struct RateLimiter {
    period: Duration,
    max_requests: usize,
    history: VecDeque<Instant>,
}

impl RateLimiter {
    pub fn new(period: Duration, max_requests: usize) -> Self {
        assert!(max_requests > 0, "max_requests must be at least 1");
        Self {
            period,
            max_requests,
            history: VecDeque::with_capacity(max_requests),
        }
    }

    fn prune(&mut self) {
        while self
            .history
            .front()
            .map_or(false, |&t| t.elapsed() > self.period)
        {
            self.history.pop_front().unwrap();
        }
    }

    pub fn can_send(&mut self) -> bool {
        self.prune();
        self.history.len() < self.max_requests
    }

    pub async fn wait_until_available(&mut self) {
        while !self.can_send() {
            let index = self.history.len() - self.max_requests; // usually equals 0
            let next = self.history[index];
            tokio::time::sleep_until((next + self.period).into()).await;
        }
    }

    pub fn register_send(&mut self) {
        self.prune();
        self.history.push_back(Instant::now());
    }
}

/// Attempts to send a message with retries on failure.
async fn attempt_send_message(
    bot: &Bot,
    mut chat_id: ChatId,
    msg: &Message,
    mut on_send: impl FnMut(),
) -> ChatStatus {
    let mut delay = BASE_RETRY_DELAY;
    let mut chat_status = ChatStatus::Active;

    let mut error = true;

    for _ in 0..RETRY_LIMIT {
        let mut request = bot
            .send_message(chat_id, &msg.text)
            .parse_mode(msg.parse_mode);

        if !msg.buttons.is_empty() {
            request = request.reply_markup(InlineKeyboardMarkup::new(vec![msg.buttons.clone()]));
        }

        let response = request.await;
        on_send();

        if let Err(e) = response {
            log::warn!("Failed to send message: {e}");
            use teloxide::ApiError::*;
            use teloxide::RequestError::*;

            match e {
                Api(
                    BotBlocked
                    | ChatNotFound
                    | GroupDeactivated
                    | BotKicked
                    | BotKickedFromSupergroup
                    | UserDeactivated
                    | CantInitiateConversation
                    | CantTalkWithBots,
                ) => return ChatStatus::Invalid,
                Api(Unknown(err)) if ADDITIONAL_ERRORS.contains(&err.as_str()) => {
                    return ChatStatus::Invalid
                }
                MigrateToChatId(new_id) => {
                    chat_id = new_id;
                    chat_status = ChatStatus::Migrated(new_id);
                }
                RetryAfter(secs) => tokio::time::sleep(secs.duration()).await,
                _ => {
                    tokio::time::sleep(delay).await;
                    delay *= RETRY_MULTIPLIER;
                }
            }
        } else {
            error = false;
            break;
        }
    }

    if error {
        log::error!("Sending message failed repeatedly, skipping ...");
    }

    chat_status
}

/// Main task loop handling message dispatching.
async fn sender_task(bot: Bot, mut db: DatabaseClient, shutdown: Weak<()>) {
    let mut chat_rate_limiters: HashMap<ChatId, [RateLimiter; 2]> = HashMap::new();

    // at most 30 messages per second, i.e. one every 33.3 milliseconds
    let mut global_rate_limiter = tokio::time::interval(Duration::from_millis(34));
    global_rate_limiter.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        global_rate_limiter.tick().await;
        let msg = db
            .pop_message(0.5)
            .await
            .inspect_err(|e| log::error!("{e}"));

        let (chat_id, msg) = match msg {
            Ok(Some(msg)) => msg,
            _ if shutdown.strong_count() == 0 => return,
            _ => continue,
        };

        let rate_limiters = chat_rate_limiters.entry(chat_id).or_insert_with(|| {
            [
                RateLimiter::new(Duration::from_secs(60), 20),
                RateLimiter::new(Duration::from_secs(1), 1),
            ]
        });

        for limiter in rate_limiters.iter_mut() {
            limiter.wait_until_available().await;
        }

        let chat_status = attempt_send_message(&bot, chat_id, &msg, || {
            rate_limiters
                .iter_mut()
                .for_each(|limiter| limiter.register_send())
        })
        .await;

        match chat_status {
            ChatStatus::Active => (),
            ChatStatus::Migrated(new_id) => {
                let _ = db.migrate_chat(chat_id, new_id).await;
            }
            ChatStatus::Invalid => {
                let _ = db.unregister_chat(chat_id).await;
            }
        }
    }
}

pub struct MessageSender {
    // when dropping this, the task (which holds a weak reference) will notice and finish
    shutdown: Arc<()>,
    handle: JoinHandle<()>,
}

impl MessageSender {
    pub fn new(bot: Bot, db: DatabaseClient) -> Self {
        let shutdown = Arc::new(());
        let shutdown_weak = Arc::downgrade(&shutdown);
        let handle = tokio::spawn(sender_task(bot, db, shutdown_weak));
        Self { shutdown, handle }
    }

    pub async fn shutdown(self) {
        drop(self.shutdown);
        let _ = self.handle.await;
    }
}

// As soon as this fails, the error handling in `send_message` must be adapted
#[test]
fn test_api_error_not_yet_added() {
    use teloxide::ApiError;

    for msg in ADDITIONAL_ERRORS {
        let api_error: ApiError = serde_json::from_str(&format!("\"{msg}\"")).unwrap();
        assert_eq!(api_error, ApiError::Unknown(msg.to_string()));
    }
}
