use std::collections::BTreeSet;

use redis::aio::ConnectionManager;
use redis::{AsyncCommands, RedisResult};
use teloxide::types::ChatId;

const REGISTERED_CHATS_KEY: &str = "allrisbot:registered_users";
const KNOWN_ITEMS_KEY: &str = "allrisbot:known_items";

#[derive(Clone)]
pub struct RedisClient {
    client: ConnectionManager,
}

impl RedisClient {
    pub async fn new(redis_url: &str) -> RedisResult<Self> {
        let client = redis::Client::open(redis_url)?;
        Ok(RedisClient {
            client: ConnectionManager::new(client).await?,
        })
    }

    pub async fn register_chat(&mut self, chat_id: ChatId) -> redis::RedisResult<bool> {
        let added = self.client.sadd(REGISTERED_CHATS_KEY, chat_id.0).await?;
        Ok(added)
    }

    pub async fn unregister_chat(&mut self, chat_id: ChatId) -> redis::RedisResult<bool> {
        let removed = self.client.srem(REGISTERED_CHATS_KEY, chat_id.0).await?;
        Ok(removed)
    }

    pub async fn get_chats(&mut self) -> redis::RedisResult<BTreeSet<ChatId>> {
        let user_ids: BTreeSet<i64> = self.client.smembers(REGISTERED_CHATS_KEY).await?;
        Ok(user_ids.into_iter().map(ChatId).collect())
    }

    pub async fn add_item(&mut self, item: &str) -> redis::RedisResult<bool> {
        self.client.sadd(KNOWN_ITEMS_KEY, item).await
    }
}
