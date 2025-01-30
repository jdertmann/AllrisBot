use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use teloxide::prelude::*;
use teloxide::types::InlineKeyboardMarkup;

use crate::database::{Message, RedisClient};
use crate::Bot;

const ADDITIONAL_ERRORS: &[&str] = &[
    "Forbidden: bot was kicked from the channel chat",
    "Forbidden: bot was kicked from the group chat",
    "Bad Request: not enough rights to send text messages to the chat",
];

enum UpdateChatId {
    Keep,
    Remove,
    Migrate(ChatId),
}

enum ChatState {
    Migrated(),
}

struct RateLimiter {
    period: Duration,
    messages_per_period: usize,
    history: VecDeque<Instant>,
}

impl RateLimiter {
    pub fn new(period: Duration, messages_per_period: usize) -> Self {
        if messages_per_period < 1 {
            panic!("messages_per_period must not be 0")
        }
        Self {
            period,
            messages_per_period,
            history: VecDeque::with_capacity(messages_per_period),
        }
    }

    fn prune(&mut self) {
        while self
            .history
            .front()
            .map(|x| x.elapsed() > self.period)
            .unwrap_or(false)
        {
            self.history.pop_front().unwrap();
        }
    }

    pub fn free_to_send(&mut self) -> bool {
        self.prune();
        self.history.len() < self.messages_per_period
    }

    pub async fn wait_for_it(&mut self) {
        loop {
            self.prune();
            if self.history.len() < self.messages_per_period {
                return;
            }
            let wait_for = *self.history.front().unwrap();
            tokio::time::sleep_until((wait_for + self.period).into()).await;
        }
    }

    pub fn sent(&mut self) {
        self.prune();
        self.history.push_back(Instant::now());
    }
}

async fn send_message(
    bot: &Bot,
    mut chat_id: ChatId,
    msg: &Message,
    mut sent: impl FnMut(),
) -> UpdateChatId {
    const MAX_TRIES: usize = 5;
    const BASE_DELAY: Duration = Duration::from_millis(500);
    const MULTIPLIER: u32 = 3;

    let mut delay = BASE_DELAY;

    let mut update = UpdateChatId::Keep;
    for _ in 0..MAX_TRIES {
        let mut request = bot
            .send_message(chat_id, &msg.text)
            .parse_mode(msg.parse_mode);

        if !msg.buttons.is_empty() {
            request = request.reply_markup(InlineKeyboardMarkup::new(vec![msg.buttons.clone()]));
        }

        let response = request.await;
        sent();

        if let Err(e) = response {
            log::warn!("Sending notification failed: {e}");
            use teloxide::ApiError::*;
            use teloxide::RequestError::*;
            match e {
                Api(e) => match e {
                    BotBlocked
                    | ChatNotFound
                    | GroupDeactivated
                    | BotKicked
                    | BotKickedFromSupergroup
                    | UserDeactivated
                    | CantInitiateConversation
                    | CantTalkWithBots => return UpdateChatId::Remove,
                    Unknown(e) if ADDITIONAL_ERRORS.contains(&e.as_str()) => {
                        return UpdateChatId::Remove
                    }
                    _ => {
                        // Invalid message probably
                        return update;
                    }
                },
                MigrateToChatId(c) => {
                    chat_id = c;
                    update = UpdateChatId::Migrate(c);
                }
                RetryAfter(secs) => {
                    tokio::time::sleep(secs.duration()).await;
                }
                _ => {
                    tokio::time::sleep(delay).await;
                    delay *= MULTIPLIER;
                }
            }
        } else {
            break;
        }

        log::info!("Retrying ...")
    }

    update
}

pub async fn task(bot: Bot, mut redis_client: RedisClient) {
    let mut global_rate_limiter = RateLimiter::new(Duration::from_secs(1), 30);
    let mut rate_limiters: HashMap<ChatId, (RateLimiter, RateLimiter)> = HashMap::new();
    loop {
        let (chat_id, msg) = redis_client.pop_message().await.unwrap();
        let (rl1, rl2) = rate_limiters.entry(chat_id).or_insert_with(|| {
            (
                RateLimiter::new(Duration::from_secs(60), 20),
                RateLimiter::new(Duration::from_secs(1), 1),
            )
        });

        let mut rls = [&mut global_rate_limiter, rl1, rl2];
        for rl in &mut rls {
            rl.wait_for_it().await;
        }

        send_message(&bot, chat_id, &msg, || {
            rls.iter_mut().for_each(|x| x.sent())
        })
        .await;
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
