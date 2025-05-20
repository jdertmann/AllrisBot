use std::time::Duration;

use frankenstein::Error;
use frankenstein::response::{ErrorResponse, ResponseParameters};

#[derive(Debug)]
pub enum RequestError {
    InvalidToken,
    ChatMigrated(i64),
    BotBlocked,
    RetryAfter(Duration),
    ClientError(ErrorResponse),
    Other(Error),
}

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

pub fn map_error(e: frankenstein::Error) -> RequestError {
    let Error::Api(api_error) = e else {
        return RequestError::Other(e);
    };

    match api_error {
        ErrorResponse {
            error_code: 401 | 404,
            ..
        } => RequestError::InvalidToken,

        ErrorResponse {
            parameters:
                Some(ResponseParameters {
                    migrate_to_chat_id: Some(new_chat_id),
                    ..
                }),
            ..
        } => RequestError::ChatMigrated(new_chat_id),

        ErrorResponse { description, .. } if TELEGRAM_ERRORS.contains(&description.as_str()) => {
            RequestError::BotBlocked
        }

        ErrorResponse {
            parameters:
                Some(ResponseParameters {
                    retry_after: Some(secs),
                    ..
                }),
            ..
        } => RequestError::RetryAfter(Duration::from_secs(secs as u64)),

        e @ ErrorResponse {
            error_code: 400..=499,
            ..
        } => RequestError::ClientError(e),

        e => RequestError::Other(Error::Api(e)),
    }
}

#[derive(Debug)]
pub enum RequestErrorRef {
    InvalidToken,
    ChatMigrated(i64),
    BotBlocked,
    RetryAfter(Duration),
    ClientError,
    Other,
}

pub fn map_error_ref(e: &frankenstein::Error) -> RequestErrorRef {
    let Error::Api(api_error) = e else {
        return RequestErrorRef::Other;
    };

    match api_error {
        ErrorResponse {
            error_code: 401 | 404,
            ..
        } => RequestErrorRef::InvalidToken,

        ErrorResponse {
            parameters:
                Some(ResponseParameters {
                    migrate_to_chat_id: Some(new_chat_id),
                    ..
                }),
            ..
        } => RequestErrorRef::ChatMigrated(*new_chat_id),

        ErrorResponse { description, .. } if TELEGRAM_ERRORS.contains(&description.as_str()) => {
            RequestErrorRef::BotBlocked
        }

        ErrorResponse {
            parameters:
                Some(ResponseParameters {
                    retry_after: Some(secs),
                    ..
                }),
            ..
        } => RequestErrorRef::RetryAfter(Duration::from_secs(*secs as u64)),

        ErrorResponse {
            error_code: 400..=499,
            ..
        } => RequestErrorRef::ClientError,

        _ => RequestErrorRef::Other,
    }
}
