use std::collections::BTreeMap;

use redis::aio::ConnectionManager;
use redis::{AsyncCommands, RedisResult};
use teloxide::types::ChatId;

const REGISTERED_CHATS_KEY: &str = "allrisbot:registered_chats";
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

    pub async fn migrate_db(&mut self) -> RedisResult<()> {
        if self.client.exists("allrisbot:registered_users").await? {
            log::info!("Migrating db ...");
            let chats: Vec<i64> = self.client.smembers("allrisbot:registered_users").await?;
            for m in chats {
                self.client.hset_nx(&REGISTERED_CHATS_KEY, m, "").await?;
            }
            self.client.del("allrisbot:registered_users").await?;
            log::info!("Migration successful!");
        } else {
            log::info!("Database up to date!");
        }

        Ok(())
    }

    pub async fn register_chat(
        &mut self,
        chat_id: ChatId,
        gremium: &str,
    ) -> redis::RedisResult<bool> {
        let added = self
            .client
            .hset_nx(REGISTERED_CHATS_KEY, chat_id.0, gremium)
            .await?;
        Ok(added)
    }

    pub async fn unregister_chat(&mut self, chat_id: ChatId) -> redis::RedisResult<bool> {
        let removed = self.client.hdel(REGISTERED_CHATS_KEY, chat_id.0).await?;
        Ok(removed)
    }

    pub async fn migrate_chat(
        &mut self,
        old_chat_id: ChatId,
        new_chat_id: ChatId,
    ) -> redis::RedisResult<()> {
        const SCRIPT: &str = "local old_value = redis.call('HGET', KEYS[1], ARGV[1]);
            if old_value then
            redis.call('HSET', KEYS[1], ARGV[2], old_value);
            redis.call('HDEL', KEYS[1], ARGV[1]); return 1;
            else return 0; end";

        redis::cmd("EVAL")
            .arg(SCRIPT)
            .arg(1)
            .arg(&REGISTERED_CHATS_KEY)
            .arg(old_chat_id.0)
            .arg(new_chat_id.0)
            .exec_async(&mut self.client)
            .await
    }

    pub async fn get_chats(&mut self) -> redis::RedisResult<BTreeMap<ChatId, String>> {
        let user_ids: BTreeMap<i64, String> = self.client.hgetall(REGISTERED_CHATS_KEY).await?;
        Ok(user_ids.into_iter().map(|(k, v)| (ChatId(k), v)).collect())
    }

    pub async fn add_item(&mut self, item: &str) -> redis::RedisResult<bool> {
        self.client.sadd(KNOWN_ITEMS_KEY, item).await
    }
}
