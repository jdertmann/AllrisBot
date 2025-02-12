use std::collections::BTreeMap;
use std::fmt::Debug;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use bb8_redis::{bb8, RedisConnectionManager};
use lazy_static::lazy_static;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use teloxide::types::ChatId;
use thiserror::Error;

const REGISTERED_CHATS_KEY: &str = "allrisbot:registered_chats";
const KNOWN_ITEMS_KEY: &str = "allrisbot:known_items";
const SCHEDULED_MESSAGES_KEY: &str = "allrisbot:scheduled_messages_hash";

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

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Clone)]
pub struct ScheduledMessageKey(String);

impl ScheduledMessageKey {
    fn new(priority: u8, timestamp: u64, message_id: &str, chat_id: ChatId) -> Self {
        let chat_id = chat_id.0;
        assert!(!message_id.contains(":"));
        Self(format!(
            "{priority:03}:{timestamp:013}:{message_id}:{chat_id}"
        ))
    }

    pub fn key(&self) -> &str {
        &self.0
    }

    fn nth<T: FromStr>(&self, n: usize) -> T
    where
        T::Err: Debug,
    {
        self.0
            .split(":")
            .nth(n)
            .expect("key invariant should be fulfilled")
            .parse()
            .expect("should be number")
    }

    pub fn priority(&self) -> u8 {
        self.nth(0)
    }

    pub fn timestamp(&self) -> u64 {
        self.nth(1)
    }

    pub fn message_id(&self) -> &str {
        self.0
            .split(":")
            .nth(2)
            .expect("key invariant should be fulfilled")
    }

    pub fn chat_id(&self) -> ChatId {
        ChatId(self.nth(3))
    }
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
        volfdnr: &str,
        msg: &Message,
        chats: impl Iterator<Item = ChatId>,
    ) -> Result<Vec<ScheduledMessageKey>, DatabaseError> {
        let msg = serde_json::to_string(&msg).unwrap();

        let mut script = QUEUE_MESSAGES_SCRIPT.prepare_invoke();

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis() as u64;

        script
            .key(KNOWN_ITEMS_KEY)
            .key(SCHEDULED_MESSAGES_KEY)
            .arg(volfdnr)
            .arg(msg);

        let keys: Vec<_> = chats
            .into_iter()
            .map(|chat_id| ScheduledMessageKey::new(2, timestamp, volfdnr, chat_id))
            .collect();

        for key in &keys {
            script.arg(&key.0);
        }

        let result = script.invoke_async(&mut *self.client().await?).await?;

        if result {
            Ok(keys)
        } else {
            Ok(Vec::new())
        }
    }

    pub async fn pop_message(&self, key: &ScheduledMessageKey) -> Result<Message, DatabaseError> {
        let (msg, _): (String, ()) = redis::pipe()
            .atomic()
            .hget(SCHEDULED_MESSAGES_KEY, key.key())
            .hdel(SCHEDULED_MESSAGES_KEY, key.key())
            .query_async(&mut *self.client().await?)
            .await?;

        let msg = serde_json::from_str(&msg)?;
        Ok(msg)
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
}

impl From<bb8::RunError<redis::RedisError>> for DatabaseError {
    fn from(value: bb8::RunError<redis::RedisError>) -> Self {
        match value {
            bb8::RunError::TimedOut => Self::PoolTimeout,
            bb8::RunError::User(e) => Self::RedisError(e),
        }
    }
}
