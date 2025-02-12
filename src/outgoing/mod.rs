mod dispatcher;

use std::time::Duration;

use teloxide::prelude::*;
use teloxide::types::{ChatId, InlineKeyboardMarkup};

use self::dispatcher::{QueueEntry, QueueEntryError};
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

    async fn process(self, p: &Self::Params) -> Result<(), QueueEntryError<ScheduledMessageKey>> {
        let msg = match p.1.pop_message(&self).await {
            Ok(msg) => msg,
            Err(e) => {
                log::warn!(
                    "Failed to retrieve scheduled message {} from database: {e}",
                    self.key()
                );
                return Ok(());
            }
        };

        let result = match send_message(&p.0, self.chat_id(), &msg).await {
            SendResult::ChatInvalid => Err(QueueEntryError::ChatInvalid),
            SendResult::Failed => {
                // TODO: exponential backoff
                Err(QueueEntryError::Retry {
                    entry: self,
                    retry_after: Duration::from_secs(5),
                })
            }
            SendResult::RetryAfter(d) => Err(QueueEntryError::Retry {
                entry: self,
                retry_after: d,
            }),
            SendResult::Sent => Ok(()),
        };

        if let Err(QueueEntryError::Retry { entry, .. }) = &result {
            if let Err(e) = p.1.add_message(entry, &msg).await {
                log::error!("Unable to re-schedule message: {e}");
                return Ok(());
            }
        }

        result
    }

    fn get_chat(&self) -> Self::Chat {
        self.chat_id()
    }

    fn is_reply(&self) -> bool {
        self.priority() == 1
    }

    async fn delete(self, p: &Self::Params) {
        let _ = p.1.delete_message(&self).await;
    }
}

enum SendResult {
    Sent,
    ChatInvalid,
    RetryAfter(Duration),
    Failed,
}

/// Attempts to send a message with retries on failure.
async fn send_message(bot: &Bot, chat_id: ChatId, msg: &Message) -> SendResult {
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
            ) => SendResult::ChatInvalid,
            Api(Unknown(err)) if ADDITIONAL_ERRORS.contains(&err.as_str()) => {
                SendResult::ChatInvalid
            }
            MigrateToChatId(_) => {
                // todo: handle migrate
                SendResult::ChatInvalid
            }
            RetryAfter(secs) => return SendResult::RetryAfter(secs.duration()),
            _ => SendResult::Failed,
        }
    } else {
        SendResult::Sent
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
