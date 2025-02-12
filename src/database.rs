use std::collections::BTreeMap;

use bb8_redis::{bb8, RedisConnectionManager};
use lazy_static::lazy_static;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use teloxide::types::ChatId;
use thiserror::Error;

const REGISTERED_CHATS_KEY: &str = "allrisbot:registered_chats";
const KNOWN_ITEMS_KEY: &str = "allrisbot:known_items";
const SCHEDULED_MESSAGES_KEY: &str = "allrisbot:scheduled_messages";

lazy_static! {
    static ref MIGRATE_SCRIPT: redis::Script =
        redis::Script::new(include_str!("redis_scripts/migrate_chat.lua"));
    static ref QUEUE_MESSAGES_SCRIPT: redis::Script =
        redis::Script::new(include_str!("redis_scripts/queue_messages.lua"));
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub text: String,
    pub parse_mode: teloxide::types::ParseMode,
    pub buttons: Vec<teloxide::types::InlineKeyboardButton>,
}

#[derive(Clone)]
pub struct DatabaseClient {
    pool: bb8::Pool<RedisConnectionManager>,
}

impl DatabaseClient {
    async fn client(
        &self,
    ) -> Result<bb8::PooledConnection<'_, RedisConnectionManager>, DatabaseError> {
        let r = self.pool.get().await?;
        Ok(r)
    }

    pub async fn new(redis_url: &str) -> Result<Self, DatabaseError> {
        let manager = RedisConnectionManager::new(redis_url)?;
        let pool = bb8::Pool::builder().build(manager).await?;
        Ok(DatabaseClient { pool })
    }

    pub async fn register_chat(
        &self,
        chat_id: ChatId,
        gremium: &str,
    ) -> Result<bool, DatabaseError> {
        let added = self
            .client()
            .await?
            .hset_nx(REGISTERED_CHATS_KEY, chat_id.0, gremium)
            .await?;
        Ok(added)
    }

    pub async fn unregister_chat(&self, chat_id: ChatId) -> Result<bool, DatabaseError> {
        let removed = self
            .client()
            .await?
            .hdel(REGISTERED_CHATS_KEY, chat_id.0)
            .await?;
        Ok(removed)
    }

    pub async fn migrate_chat(
        &self,
        old_chat_id: ChatId,
        new_chat_id: ChatId,
    ) -> Result<(), DatabaseError> {
        MIGRATE_SCRIPT
            .key(REGISTERED_CHATS_KEY)
            .arg(old_chat_id.0)
            .arg(new_chat_id.0)
            .invoke_async(&mut *self.client().await?)
            .await?;

        Ok(())
    }

    pub async fn get_chats(&self) -> Result<BTreeMap<ChatId, String>, DatabaseError> {
        let user_ids: BTreeMap<i64, String> =
            self.client().await?.hgetall(REGISTERED_CHATS_KEY).await?;
        Ok(user_ids.into_iter().map(|(k, v)| (ChatId(k), v)).collect())
    }

    pub async fn has_item(&self, item: &str) -> Result<bool, DatabaseError> {
        let result = self
            .client()
            .await?
            .sismember(KNOWN_ITEMS_KEY, item)
            .await?;

        Ok(result)
    }

    pub async fn queue_messages(
        &self,
        item: &str,
        msg: &Message,
        chats: impl Iterator<Item = ChatId>,
    ) -> Result<bool, DatabaseError> {
        let msg = serde_json::to_string(&msg).unwrap();

        let mut script = QUEUE_MESSAGES_SCRIPT.prepare_invoke();

        script
            .key(KNOWN_ITEMS_KEY)
            .key(SCHEDULED_MESSAGES_KEY)
            .arg(item)
            .arg(msg);

        for ChatId(id) in chats {
            script.arg(id);
        }

        let result = script.invoke_async(&mut *self.client().await?).await?;

        Ok(result)
    }

    pub async fn pop_message(
        &self,
        timeout: f64,
    ) -> Result<Option<(ChatId, Message)>, DatabaseError> {
        let response: Option<((), String)> = self
            .client()
            .await?
            .brpop(SCHEDULED_MESSAGES_KEY, timeout)
            .await?;

        let msg = match response {
            Some(((), msg)) => msg,
            None => {
                return Ok(None);
            }
        };

        let (chat_id, msg) = msg
            .split_once(':')
            .ok_or(DatabaseError::InvalidEntryError)?;
        let msg = serde_json::from_str(msg)?;
        let chat_id = chat_id
            .parse()
            .map_err(|_| DatabaseError::InvalidEntryError)?;

        Ok(Some((ChatId(chat_id), msg)))
    }
}

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("{0}")]
    RedisError(#[from] redis::RedisError),
    #[error("connection pool timed out")]
    PoolTimeout,
    #[error("deserialization failed: {0}")]
    DeserializationError(#[from] serde_json::Error),
    #[error("invalid entry")]
    InvalidEntryError,
}

impl From<bb8::RunError<redis::RedisError>> for DatabaseError {
    fn from(value: bb8::RunError<redis::RedisError>) -> Self {
        match value {
            bb8::RunError::TimedOut => Self::PoolTimeout,
            bb8::RunError::User(e) => Self::RedisError(e),
        }
    }
}
