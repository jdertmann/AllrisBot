use std::collections::BTreeSet;

use redis::{AsyncCommands, RedisResult};
use teloxide::types::ChatId;

const REGISTERED_CHATS_KEY: &str = "allrisbot:registered_users";
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

    pub async fn add_item(&self, item: &str) -> redis::RedisResult<bool> {
        let mut con = self.client.get_multiplexed_async_connection().await?;
        con.sadd(KNOWN_ITEMS_KEY, item).await
    }
}
