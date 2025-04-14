use std::ops::ControlFlow;
use std::time::Duration;

use teloxide::payloads::SendMessageSetters as _;
use teloxide::prelude::Requester as _;
use teloxide::sugar::request::RequestLinkPreviewExt;
use teloxide::types::InlineKeyboardMarkup;
use teloxide::{ApiError, RequestError};
use tokio::time::sleep;
use tokio_retry::strategy::{ExponentialBackoff, jitter};

use super::{BroadcastResources, WorkerResult};
use crate::database::{self, StreamId};
use crate::lru_cache::CacheItem;
use crate::types::{ChatId, Message};

const ADDITIONAL_ERRORS: &[&str] = &[
    "Forbidden: bot was kicked from the channel chat",
    "Forbidden: bot was kicked from the group chat",
    "Bad Request: not enough rights to send text messages to the chat",
];

fn is_chat_invalid(e: &teloxide::ApiError) -> bool {
    use ApiError::*;

    match e {
        BotBlocked
        | ChatNotFound
        | GroupDeactivated
        | BotKicked
        | BotKickedFromSupergroup
        | UserDeactivated
        | CantInitiateConversation
        | NotEnoughRightsToPostMessages
        | CantTalkWithBots => true,
        Unknown(err) if ADDITIONAL_ERRORS.contains(&err.as_str()) => true,
        _ => false,
    }
}

fn backoff_strategy() -> impl Iterator<Item = Duration> {
    ExponentialBackoff::from_millis(10)
        .factor(10)
        .max_delay(Duration::from_secs(30))
        .map(jitter)
        .take(5)
}

pub struct MessageSender {
    chat_id: ChatId,
    entry: CacheItem<(StreamId, Message)>,
}

impl MessageSender {
    pub fn new(chat_id: ChatId, entry: CacheItem<(StreamId, Message)>) -> Self {
        Self { chat_id, entry }
    }

    pub fn message_id(&self) -> StreamId {
        self.entry.0
    }

    pub fn message(&self) -> &Message {
        &self.entry.1
    }

    pub async fn check_filters(&self, shared: &BroadcastResources) -> database::Result<bool> {
        let filters = shared.db.get_filters(self.chat_id).await?;
        let matches = filters.iter().any(|filter| filter.matches(self.message()));

        Ok(matches)
    }

    pub async fn acknowledge_message(&self, shared: &BroadcastResources) -> database::Result<bool> {
        shared
            .db
            .acknowledge_message(self.chat_id, self.message_id())
            .await
    }

    async fn unacknowledge_message(&self, shared: &BroadcastResources) -> database::Result<bool> {
        shared
            .db
            .unacknowledge_message(self.chat_id, self.message_id())
            .await
    }

    async fn handle_response(
        &self,
        shared: &BroadcastResources,
        response: Result<(), RequestError>,
        backoff: Option<Duration>,
    ) -> database::Result<ControlFlow<WorkerResult, Duration>> {
        let response = response.inspect_err(|e| log::warn!("Failed to send message: {e}"));

        macro_rules! retry {
            ($dur:expr) => {
                if self.unacknowledge_message(shared).await? {
                    return Ok(ControlFlow::Continue($dur));
                } else {
                    WorkerResult::OutOfSync
                }
            };
        }

        let result = match response {
            Ok(()) => WorkerResult::Processed(self.message_id()),
            Err(RequestError::Api(e)) if is_chat_invalid(&e) => {
                shared.db.remove_subscription(self.chat_id).await?;
                WorkerResult::ChatStopped
            }
            Err(RequestError::Api(ApiError::InvalidToken)) => {
                log::error!("Invalid token! Was it revoked?");
                shared.hard_shutdown.send_replace(true);
                WorkerResult::ShuttingDown
            }
            Err(RequestError::MigrateToChatId(new_chat_id)) => {
                self.unacknowledge_message(shared).await?;
                shared.db.migrate_chat(self.chat_id, new_chat_id.0).await?;
                WorkerResult::MigratedTo(new_chat_id.0)
            }
            Err(RequestError::RetryAfter(secs)) => retry!(secs.duration()),
            _ => {
                if let Some(backoff) = backoff {
                    retry!(backoff)
                } else {
                    log::error!("Sending failed definitely, not retrying!");
                    WorkerResult::Processed(self.message_id())
                }
            }
        };

        Ok(ControlFlow::Break(result))
    }

    pub async fn send_message(
        &self,
        shared: &BroadcastResources,
        message_sent: &mut bool,
    ) -> database::Result<WorkerResult> {
        let mut backoff = backoff_strategy();

        loop {
            *message_sent = false;

            if !self.acknowledge_message(shared).await? {
                return Ok(WorkerResult::OutOfSync);
            }

            let response = self.try_send_message(shared).await;
            *message_sent = true;

            match self
                .handle_response(shared, response, backoff.next())
                .await?
            {
                ControlFlow::Break(result) => return Ok(result),
                ControlFlow::Continue(retry_after) => sleep(retry_after).await,
            }
        }
    }

    async fn try_send_message(&self, shared: &BroadcastResources) -> Result<(), RequestError> {
        let message = self.message();

        let mut request = shared
            .bot
            .send_message(teloxide::types::ChatId(self.chat_id), &message.text)
            .disable_link_preview(true)
            .parse_mode(self.message().parse_mode);

        if !message.buttons.is_empty() {
            let keyboard = InlineKeyboardMarkup::new(vec![message.buttons.clone()]);
            request = request.reply_markup(keyboard);
        }

        request.await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As soon as this fails, `ADDITIONAL_ERRORS` must be adapted
    #[test]
    fn test_api_error_not_yet_added() {
        for msg in ADDITIONAL_ERRORS {
            let api_error: ApiError = serde_json::from_str(&format!("\"{msg}\"")).unwrap();
            assert_eq!(api_error, ApiError::Unknown(msg.to_string()));
            assert!(is_chat_invalid(&api_error));
        }
    }
}
