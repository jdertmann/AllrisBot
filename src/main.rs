mod database;
mod feed;
mod updater;

#[cfg(feature = "handle_updates")]
mod dispatcher;
use teloxide::Bot;
use thiserror::Error;

const FEED_URL: &str = "https://www.bonn.sitzung-online.de/rss/voreleased";

#[derive(Debug, Error)]
pub enum Error {
    #[error("web request error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("invalid feed format: {0}")]
    ParseError(#[from] serde_xml_rs::Error),
    #[error("redis error: {0}")]
    RedisError(#[from] redis::RedisError),
    #[error("parsing url failed: {0}")]
    UrlParseError(#[from] url::ParseError),
}

#[tokio::main]
async fn main() {
    env_logger::init();

    let bot = Bot::from_env();
    let redis_client = database::RedisClient::new("redis://127.0.0.1/");

    #[cfg(feature = "handle_updates")]
    {
        tokio::spawn(updater::feed_updater(bot.clone(), redis_client.clone()));

        dispatcher::dispatch(bot, redis_client).await;
    }

    #[cfg(not(feature = "handle_updates"))]
    updater::feed_updater(bot, redis_client).await
}

// As soon as this fails, the error handling in `send_message` must be adapted
#[test]
#[cfg(feature = "handle_updates")]
fn test_api_error_not_yet_added() {
    use teloxide_core::ApiError;

    const ERROR_MSG: &str = "Forbidden: bot was kicked from the channel chat";
    let api_error: ApiError = serde_json::from_str(&format!("\"{ERROR_MSG}\"")).unwrap();
    assert_eq!(api_error, ApiError::Unknown(ERROR_MSG.to_string()));
}
