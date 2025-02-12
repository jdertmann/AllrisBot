mod dispatcher;

use std::time::Duration;

use teloxide::prelude::*;
use teloxide::types::{ChatId, InlineKeyboardMarkup};

use self::dispatcher::QueueEntry;
use crate::database::{DatabaseClient, Message, ScheduledMessageKey};
use crate::Bot;

const RETRY_LIMIT: usize = 5;
const BASE_RETRY_DELAY: Duration = Duration::from_millis(500);
const RETRY_MULTIPLIER: u32 = 3;
const ADDITIONAL_ERRORS: &[&str] = &[
    "Forbidden: bot was kicked from the channel chat",
    "Forbidden: bot was kicked from the group chat",
    "Bad Request: not enough rights to send text messages to the chat",
];

pub type MessageDispatcher = dispatcher::MessageDispatcher<ScheduledMessageKey>;

impl QueueEntry for ScheduledMessageKey {
    type Params = (crate::Bot, DatabaseClient);
    type Chat = ChatId;

    async fn process(self, p: &Self::Params) {
        match p.1.pop_message(&self).await {
            Ok(msg) => {
                send_message(&p.0, self.chat_id(), &msg).await;
            }
            Err(e) => log::warn!(
                "Failed to retrieve scheduled message {} from database: {e}",
                self.key()
            ),
        }
    }

    fn get_chat(&self) -> Self::Chat {
        self.chat_id()
    }

    fn is_reply(&self) -> bool {
        self.priority() == 1
    }
}

enum ChatStatus {
    Active,
    Invalid,
    Migrated(ChatId),
}

/// Attempts to send a message with retries on failure.
async fn send_message(bot: &Bot, mut chat_id: ChatId, msg: &Message) -> ChatStatus {
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

// As soon as this fails, the error handling in `send_message` must be adapted
#[test]
fn test_api_error_not_yet_added() {
    use teloxide::ApiError;

    for msg in ADDITIONAL_ERRORS {
        let api_error: ApiError = serde_json::from_str(&format!("\"{msg}\"")).unwrap();
        assert_eq!(api_error, ApiError::Unknown(msg.to_string()));
    }
}
