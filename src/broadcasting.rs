use std::future;
use std::time::Duration;

use bot_utils::ChatId;
use bot_utils::broadcasting::{Backend, NextUpdate};
use frankenstein::AsyncTelegramApi as _;
use frankenstein::types::LinkPreviewOptions;
use futures_util::{Stream, StreamExt, stream};
use regex::Regex;
use tokio::time::sleep;

use crate::database::{self, ChatState, DatabaseConnection, SharedDatabaseConnection, StreamId};
use crate::lru_cache::{CacheItem, Lru, LruCache};
use crate::types::{Condition, Filter, Message};

impl Condition {
    fn matches(&self, message: &Message) -> bool {
        let Ok(regex) = Regex::new(&self.pattern) else {
            log::warn!("Invalid regex pattern!");
            return false;
        };

        let result = message
            .tags
            .iter()
            .filter(|x| x.0 == self.tag)
            .any(|x| regex.is_match(&x.1));

        result ^ self.negate
    }
}

impl Filter {
    fn matches(&self, message: &Message) -> bool {
        for condition in &self.conditions {
            if !condition.matches(message) {
                return false;
            }
        }

        true
    }
}

pub struct RedisBackend {
    pub bot: crate::Bot,
    pub db: SharedDatabaseConnection,
    pub cache: LruCache<StreamId, (StreamId, Message)>,
}

impl RedisBackend {
    pub fn new(bot: crate::Bot, db: redis::Client) -> Self {
        let db = DatabaseConnection::new(db, None).shared();
        let cache = LruCache::new(Lru::new(30));

        Self { bot, db, cache }
    }

    async fn get_next_entry(
        &self,
        last_sent: StreamId,
    ) -> database::Result<Option<CacheItem<(StreamId, Message)>>> {
        self.cache
            .get_some(last_sent, || self.db.get_next_message(last_sent))
            .await
    }

    async fn matches_filter(&self, chat: i64, msg: &Message) -> database::Result<bool> {
        let filters = self.db.get_filters(chat).await?;
        let matches = filters.iter().any(|filter| filter.matches(msg));
        Ok(matches)
    }
}

impl Backend for RedisBackend {
    type UpdateId = StreamId;

    type Message = CacheItem<(StreamId, Message)>;

    type Error = database::Error;

    async fn acknowledge(
        &self,
        chat_id: i64,
        message_id: Self::UpdateId,
    ) -> Result<bool, Self::Error> {
        self.db.acknowledge_message(chat_id, message_id).await
    }

    async fn unacknowledge(
        &self,
        chat_id: i64,
        message_id: Self::UpdateId,
    ) -> Result<bool, Self::Error> {
        self.db.unacknowledge_message(chat_id, message_id).await
    }

    async fn migrate_chat(
        &self,
        old_chat_id: ChatId,
        new_chat_id: ChatId,
    ) -> Result<bool, Self::Error> {
        self.db.migrate_chat(old_chat_id, new_chat_id).await
    }

    async fn remove_chat(&self, chat_id: ChatId) -> Result<bool, Self::Error> {
        self.db.remove_subscription(chat_id).await
    }

    async fn next_update(&self, chat: ChatId) -> Result<NextUpdate<Self>, Self::Error> {
        let last_sent = match self.db.get_chat_state(chat).await? {
            ChatState::Active { last_sent } => last_sent,
            ChatState::Migrated { to } => return Ok(NextUpdate::Migrated { to }),
            ChatState::Stopped => return Ok(NextUpdate::Stopped),
        };

        let update = match self.get_next_entry(last_sent).await? {
            Some(msg) if self.matches_filter(chat, &msg.1).await? => {
                NextUpdate::Ready { id: msg.0, msg }
            }
            Some(msg) => {
                self.acknowledge(chat, msg.0).await?;
                NextUpdate::Skipped { id: msg.0 }
            }
            None => NextUpdate::Pending {
                previous: last_sent,
            },
        };

        Ok(update)
    }

    fn receive_updates(&self) -> impl Stream<Item = (StreamId, Vec<ChatId>)> + 'static {
        let db = self.db.get_dedicated();

        stream::unfold(
            (None, false, db),
            |(last_stream_id, was_error, mut db)| async move {
                if was_error {
                    sleep(Duration::from_secs(20)).await;
                }

                let result: Result<_, Self::Error> = async {
                    let next_id = if let Some(id) = last_stream_id {
                        db.next_message_id_blocking(id).await?
                    } else {
                        db.current_message_id().await?
                    };
                    let active_chats = db.get_active_chats().await?;
                    Ok((next_id, active_chats))
                }
                .await;

                let was_error = result.is_err();
                let item = result.ok();
                let stream_id = item.as_ref().map(|item| item.0).or(last_stream_id);

                Some((item, (stream_id, was_error, db)))
            },
        )
        .filter_map(future::ready)
    }

    async fn send(&self, chat_id: i64, message: &Self::Message) -> Result<(), frankenstein::Error> {
        let message = &message.1;
        let mut params = message.request.clone();
        params.chat_id = chat_id.into();
        params.link_preview_options = Some(LinkPreviewOptions::builder().is_disabled(true).build());

        self.bot.send_message(&params).await?;

        Ok(())
    }
}
