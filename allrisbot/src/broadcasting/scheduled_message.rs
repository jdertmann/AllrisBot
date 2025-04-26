use std::ops::ControlFlow;
use std::time::Duration;

use frankenstein::response::{ErrorResponse, ResponseParameters};
use frankenstein::types::LinkPreviewOptions;
use frankenstein::{AsyncTelegramApi, Error as RequestError, ParseMode};
use regex::Regex;
use tokio::time::sleep;
use tokio_retry::strategy::{ExponentialBackoff, jitter};

use super::{ProcessNextResult, SharedDependencies};
use crate::database::{self, StreamId};
use crate::lru_cache::CacheItem;
use crate::types::{ChatId, Condition, Filter, Message};

/// error messages that imply we're not allowed to send messages
/// to this chat in the future.
const TELEGRAM_ERRORS: [&str; 14] = [
    "Bad Request: CHAT_WRITE_FORBIDDEN",
    "Bad Request: TOPIC_CLOSED",
    "Bad Request: chat not found",
    "Bad Request: have no rights to send a message",
    "Bad Request: not enough rights to send text messages to the chat",
    "Bad Request: need administrator rights in the channel chat",
    "Forbidden: bot is not a member of the channel chat",
    "Forbidden: bot is not a member of the supergroup chat",
    "Forbidden: bot was blocked by the user",
    "Forbidden: bot was kicked from the channel chat",
    "Forbidden: bot was kicked from the group chat",
    "Forbidden: bot was kicked from the supergroup chat",
    "Forbidden: the group chat was deleted",
    "Forbidden: user is deactivated",
];

fn backoff_strategy() -> impl Iterator<Item = Duration> {
    ExponentialBackoff::from_millis(10)
        .factor(10)
        .max_delay(Duration::from_secs(30))
        .map(jitter)
        .take(5)
}

impl Condition {
    fn matches(&self, message: &Message) -> Result<bool, regex::Error> {
        let regex = Regex::new(&self.pattern)?;
        let result = message
            .tags
            .iter()
            .filter(|x| x.0 == self.tag)
            .any(|x| regex.is_match(&x.1));

        Ok(result ^ self.negate)
    }
}

impl Filter {
    fn matches(&self, message: &Message) -> Result<bool, regex::Error> {
        for condition in &self.conditions {
            if !condition.matches(message)? {
                return Ok(false);
            }
        }

        Ok(true)
    }
}

/// A message that is scheduled to be sent to a certain chat
pub struct ScheduledMessage {
    chat_id: ChatId,
    entry: CacheItem<(StreamId, Message)>,
}

impl ScheduledMessage {
    pub fn new(chat_id: ChatId, entry: CacheItem<(StreamId, Message)>) -> Self {
        Self { chat_id, entry }
    }

    pub fn message_id(&self) -> StreamId {
        self.entry.0
    }

    pub fn message(&self) -> &Message {
        &self.entry.1
    }

    /// checks whether this message should be sent
    pub async fn check_filters(&self, shared: &SharedDependencies) -> database::Result<bool> {
        let filters = shared.db.get_filters(self.chat_id).await?;
        for filter in filters {
            if filter.matches(self.message())? {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// mark this message as sent in the database
    pub async fn acknowledge_message(&self, shared: &SharedDependencies) -> database::Result<bool> {
        shared
            .db
            .acknowledge_message(self.chat_id, self.message_id())
            .await
    }

    async fn unacknowledge_message(&self, shared: &SharedDependencies) -> database::Result<bool> {
        shared
            .db
            .unacknowledge_message(self.chat_id, self.message_id())
            .await
    }

    async fn handle_response(
        &self,
        shared: &SharedDependencies,
        response: Result<(), RequestError>,
        backoff: Option<Duration>,
    ) -> database::Result<ControlFlow<ProcessNextResult, Duration>> {
        let response = response.inspect_err(|e| log::warn!("Failed to send message: {e}"));

        macro_rules! retry {
            ($dur:expr) => {
                if self.unacknowledge_message(shared).await? {
                    return Ok(ControlFlow::Continue($dur));
                } else {
                    ProcessNextResult::OutOfSync
                }
            };
        }

        let result = match response {
            Ok(()) => ProcessNextResult::Processed(self.message_id()),
            Err(RequestError::Api(e)) => match e {
                ErrorResponse {
                    error_code: 401 | 404,
                    ..
                } => {
                    log::error!("Invalid token! Was it revoked?");
                    shared.hard_shutdown.send_replace(true);
                    ProcessNextResult::ShuttingDown
                }
                ErrorResponse {
                    parameters:
                        Some(ResponseParameters {
                            migrate_to_chat_id: Some(new_chat_id),
                            ..
                        }),
                    ..
                } => {
                    self.unacknowledge_message(shared).await?;
                    shared.db.migrate_chat(self.chat_id, new_chat_id).await?;
                    ProcessNextResult::MigratedTo(new_chat_id)
                }
                ErrorResponse {
                    parameters:
                        Some(ResponseParameters {
                            retry_after: Some(secs),
                            ..
                        }),
                    ..
                } => {
                    retry!(Duration::from_secs(secs as u64))
                }
                ErrorResponse { description, .. }
                    if TELEGRAM_ERRORS.contains(&description.as_str()) =>
                {
                    shared.db.remove_subscription(self.chat_id).await?;
                    ProcessNextResult::ChatStopped
                }
                _ => {
                    if let Some(backoff) = backoff {
                        retry!(backoff)
                    } else {
                        log::error!("Sending failed definitely, not retrying!");
                        ProcessNextResult::Processed(self.message_id())
                    }
                }
            },
            _ => {
                if let Some(backoff) = backoff {
                    retry!(backoff)
                } else {
                    log::error!("Sending failed definitely, not retrying!");
                    ProcessNextResult::Processed(self.message_id())
                }
            }
        };

        Ok(ControlFlow::Break(result))
    }

    /// Sends a message. Will retry a number of times if it fails
    pub async fn send_message(
        &self,
        shared: &SharedDependencies,
        message_sent: &mut bool,
    ) -> database::Result<ProcessNextResult> {
        let mut backoff = backoff_strategy();

        loop {
            *message_sent = false;

            if !self.acknowledge_message(shared).await? {
                return Ok(ProcessNextResult::OutOfSync);
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

    async fn try_send_message(&self, shared: &SharedDependencies) -> Result<(), RequestError> {
        let message = self.message();

        let mut params = message.request.clone();
        params.parse_mode = Some(ParseMode::Html);
        params.chat_id = self.chat_id.into();
        params.link_preview_options = Some(LinkPreviewOptions::builder().is_disabled(true).build());

        shared.bot.send_message(&params).await?;

        Ok(())
    }
}
