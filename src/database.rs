use std::collections::BTreeSet;

use redis::{AsyncCommands, RedisResult};
use teloxide::types::ChatId;

use crate::updater::SavedState;

const REGISTERED_CHATS_KEY: &str = "allrisbot:registered_users";
const SAVED_KEY: &str = "allrisbot:saved_state";
const KNOWN_ITEMS_KEY: &str = "allrisbot:known_items";

#[derive(Clone)]
pub struct RedisClient {
    client: redis::Client,
}

impl RedisClient {
    pub fn new(redis_url: &str) -> RedisResult<Self> {
        let client = redis::Client::open(redis_url)?;
        Ok(RedisClient { client })
    }

    pub async fn register_chat(&self, chat_id: ChatId) -> redis::RedisResult<bool> {
        let mut con = self.client.get_multiplexed_async_connection().await?;
        let added = con.sadd(REGISTERED_CHATS_KEY, chat_id.0).await?;
        Ok(added)
    }

    pub async fn unregister_chat(&self, chat_id: ChatId) -> redis::RedisResult<bool> {
        let mut con = self.client.get_multiplexed_async_connection().await?;
        let removed = con.srem(REGISTERED_CHATS_KEY, chat_id.0).await?;
        Ok(removed)
    }

    pub async fn get_chats(&self) -> redis::RedisResult<BTreeSet<ChatId>> {
        let mut con = self.client.get_multiplexed_async_connection().await?;
        let user_ids: BTreeSet<i64> = con.smembers(REGISTERED_CHATS_KEY).await?;
        Ok(user_ids.into_iter().map(ChatId).collect())
    }

    pub async fn save_state(&self, state: SavedState) -> redis::RedisResult<()> {
        let mut con = self.client.get_multiplexed_async_connection().await?;
        let serialized_state = serde_json::to_string(&state)?;
        con.set(SAVED_KEY, serialized_state).await?;
        Ok(())
    }

    pub async fn get_saved_state(&self) -> redis::RedisResult<Option<SavedState>> {
        let mut con = self.client.get_multiplexed_async_connection().await?;
        let serialized_state: Option<String> = con.get(SAVED_KEY).await?;
        if let Some(serialized_state) = serialized_state {
            let state: SavedState = serde_json::from_str(&serialized_state)?;
            Ok(Some(state))
        } else {
            Ok(None)
        }
    }

    pub async fn add_item(&self, item: &str) -> redis::RedisResult<bool> {
        let mut con = self.client.get_multiplexed_async_connection().await?;
        con.sadd(KNOWN_ITEMS_KEY, item).await
    }
}
