use std::fmt::{self, Debug};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures_util::lock::Mutex;
use redis::aio::MultiplexedConnection;
use redis::{AsyncCommands, Client, Cmd, FromRedisValue, RedisError, RedisWrite, RetryMethod};
use tokio::time::{Instant, sleep_until};

use crate::types::{Filter, Message};

// define keys

const REGISTERED_CHATS_KEY: &str = "allrisbot:registered_chats";
const KNOWN_ITEMS_KEY: &str = "allrisbot:known_items";
const SCHEDULED_MESSAGES_KEY: &str = "allrisbot:scheduled_messages";
const LAST_UPDATE_KEY: &str = "allrisbot:last_update";

fn registered_chat_key(chat_id: i64) -> String {
    format!("allrisbot:registered_chats:{chat_id}")
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Redis(#[from] redis::RedisError),
    #[error("database timeout")]
    Timeout,
}

pub type Result<T> = std::result::Result<T, Error>;

macro_rules! script {
    ($file:literal) => {{
        use std::sync::LazyLock;
        static SCRIPT: LazyLock<redis::Script> =
            LazyLock::new(|| redis::Script::new(include_str!(concat!("redis_scripts/", $file))));
        &*SCRIPT
    }};
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct StreamId(u64, u64);

impl StreamId {
    const ZERO: Self = StreamId(0, 0);
}

impl fmt::Display for StreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!("{}-{}", self.0, self.1))
    }
}

macro_rules! invalid_type_error {
    ($v:expr,$det:expr) => {
        return Err(redis::RedisError::from((
            redis::ErrorKind::TypeError,
            "Response was of incompatible type",
            format!("{} (response was {:?})", $det, $v),
        ))
        .into())
    };
}

impl redis::FromRedisValue for StreamId {
    fn from_redis_value(v: &redis::Value) -> redis::RedisResult<Self> {
        macro_rules! try_assign {
            ($(let $assign:pat = $val:expr , else $det:expr ;)+) => {
                $(let $assign = $val else { invalid_type_error!(v, $det) };)+
            };
        }

        try_assign! {
            let redis::Value::BulkString(bytes) = v, else "Stream ID is not a bulk string";
            let Ok(string) = std::str::from_utf8(bytes), else "Could not convert from string.";
            let Some((a, b)) = string.split_once('-'), else "Stream ID has invalid format.";
            let Ok(a) = a.parse(), else "Stream ID has invalid format.";
            let Ok(b) = b.parse(), else "Stream ID has invalid format.";
        }

        Ok(Self(a, b))
    }
}

impl redis::ToRedisArgs for StreamId {
    fn write_redis_args<W>(&self, out: &mut W)
    where
        W: ?Sized + RedisWrite,
    {
        out.write_arg_fmt(self);
    }
}

impl FromRedisValue for Message {
    fn from_redis_value(v: &redis::Value) -> redis::RedisResult<Self> {
        let mut iter = match v.as_map_iter() {
            Some(iter) => iter,
            None => invalid_type_error!(v, "stream entry should be map"),
        };

        let content = iter
            .find(|(k, _)| matches!(String::from_redis_value(k).as_deref(), Ok("message")))
            .map(|(_, v)| v);

        let Some(content) = content else {
            invalid_type_error!(v, "stream entry missing key")
        };

        let content = String::from_redis_value(content)?;

        Ok(serde_json::from_str(&content)?)
    }
}

#[derive(Debug)]
pub struct DatabaseConnection {
    client: Client,
    connection: Option<MultiplexedConnection>,
    timeout: Option<Duration>,
    retry_counter: u32,
}

impl DatabaseConnection {
    pub fn new(client: Client, timeout: Option<Duration>) -> Self {
        Self {
            client,
            connection: None,
            timeout,
            retry_counter: 0,
        }
    }

    pub async fn connect(client: Client, timeout: Option<Duration>) -> Result<Self> {
        let mut this = DatabaseConnection::new(client, timeout);
        let deadline = timeout.map(|x| Instant::now() + x);
        timeout_at(deadline, this.get_connection()).await?;
        Ok(this)
    }

    async fn get_connection(&mut self) -> Result<&mut MultiplexedConnection> {
        if self.connection.is_some() {
            Ok(self.connection.as_mut().unwrap())
        } else {
            let connection = self.client.get_multiplexed_async_connection().await?;
            let connection_ref = self.connection.insert(connection);
            Ok(connection_ref)
        }
    }

    async fn handle_error(&mut self, err: Error, deadline: Option<Instant>) -> Result<()> {
        let err = match err {
            Error::Redis(err) => err,
            Error::Timeout => return Err(Error::Timeout),
        };
        log::warn!("Database error: {err}");

        self.retry_counter += 1;

        match err.retry_method() {
            RetryMethod::RetryImmediately if self.retry_counter == 1 => return Ok(()),
            RetryMethod::WaitAndRetry | RetryMethod::RetryImmediately => {
                // reconnect once in a while if it doesn't work
                if self.retry_counter % 3 == 0 {
                    self.connection = None;
                }
            }
            RetryMethod::Reconnect => {
                self.connection = None;
            }
            _ => return Err(err.into()),
        }

        let duration_ms = (10 * 5_u64.pow(self.retry_counter.min(5))).min(15_000);
        let retry_at = Instant::now()
            + Duration::from_millis(duration_ms).mul_f64(0.75 + rand::random::<f64>() / 2.);

        if deadline.is_some_and(|t| t < retry_at) {
            return Err(err.into());
        }

        sleep_until(retry_at).await;

        if self.connection.is_none() {
            log::info!("Reconnecting ...");
        } else {
            log::info!("Retrying ...")
        }

        Ok(())
    }
}

#[derive(Debug)]
pub struct SharedDatabaseConnection {
    connection: Mutex<DatabaseConnection>,
    timeout: Option<Duration>,
}

impl SharedDatabaseConnection {
    pub fn new(connection: DatabaseConnection) -> Self {
        let timeout = connection.timeout;
        Self {
            connection: Mutex::new(connection),
            timeout,
        }
    }
}

async fn timeout_at<T>(
    deadline: Option<Instant>,
    future: impl Future<Output = Result<T>>,
) -> Result<T> {
    if let Some(deadline) = deadline {
        tokio::time::timeout_at(deadline, future)
            .await
            .map_err(|_| Error::Timeout)?
    } else {
        future.await
    }
}

macro_rules! implement_with_retry {
    (
        $conn_struct:ident,
        $conn_struct_shared: ident;
        $(
            $(#[$attr:ident])?
            $vis:vis async fn $fn_name:ident
            (
                $conn:ident $(, $param_name:ident : $param_type:ty)* $(,)?
            ) $(-> $return_type:ty)?
            $body:block
        )+
    ) => {

        mod common {
            use super::*;

            $(
                pub(super) async fn $fn_name(conn: &mut $conn_struct, deadline: Option<Instant>, $($param_name: $param_type),*) -> Result<Option<implement_with_retry!(@ret $($return_type)?)>> {
                    let request = async {
                        let $conn = conn.get_connection().await?;
                        Ok($body)
                    };

                    match timeout_at(deadline, request).await {
                        Ok(r) => {
                            conn.retry_counter = 0;
                            return Ok(Some(r));
                        }
                        Err(e) => {
                            implement_with_retry!(@reset $($attr)?, conn);
                            conn.handle_error(e, deadline).await?
                        }
                    }

                    Ok(None)
                }
            )+
        }

        impl $conn_struct {
        $(
            #[allow(dead_code)]
            $vis async fn $fn_name (
                &mut self,
                $($param_name: $param_type),*
            ) -> Result<implement_with_retry!(@ret $($return_type)?)> {
                let deadline = self.timeout.map(|timeout| Instant::now() + timeout);

                loop {
                    if let Some(r) = common::$fn_name(self, deadline, $($param_name),*).await? {
                        return Ok(r)
                    }
                }
            }
        )+
        }
        impl $conn_struct_shared {
            $(
                #[allow(dead_code)]
                $vis async fn $fn_name (
                    &self,
                    $($param_name: $param_type),*
                ) -> Result<implement_with_retry!(@ret $($return_type)?)>  {
                    let deadline = self.timeout.map(|timeout| Instant::now() + timeout);

                    loop {
                        let mut conn = timeout_at(deadline, async { Ok(self.connection.lock().await) }).await?;

                        for _ in 0..4 {
                            if let Some(r) = common::$fn_name(&mut conn, deadline, $($param_name),*).await? {
                                return Ok(r)
                            }
                        }

                        // reacquire mutex after 4 failed attempts, in case the failure is the request's fault
                    }
                }
            )+
        }
    };
    (@reset reset_connection_on_error, $conn:expr) => { $conn.connection = None; };
    (@reset , $conn:expr ) => { };
    (@ret $t:ty) =>  { $t };
    (@ret) => { () };
}

pub enum ChatState {
    Active { last_sent: StreamId },
    Migrated { to: i64 },
    Stopped,
}

implement_with_retry! {
    DatabaseConnection, SharedDatabaseConnection;

    pub async fn is_known_volfdnr(connection, volfdnr: &str) -> bool {
        connection.sismember(KNOWN_ITEMS_KEY, volfdnr).await?
    }

    pub async fn add_known_volfdnr(connection, volfdnr: &str) {
        connection.sadd(KNOWN_ITEMS_KEY, volfdnr).await?
    }

    pub async fn schedule_broadcast(
        connection,
        volfdnr: &str,
        message: &Message
    ) -> Option<String> {
        let serialized = serde_json::to_string(message).map_err(RedisError::from)?;

        script!("schedule_broadcast.lua")
            .key(SCHEDULED_MESSAGES_KEY)
            .key(KNOWN_ITEMS_KEY)
            .arg(volfdnr)
            .arg(&serialized)
            .invoke_async(connection)
            .await?
    }

    pub async fn add_subscription(
        connection,
        chat_id: i64,
        filter: &str
    ) -> bool {
        script!("add_subscription.lua")
            .key(SCHEDULED_MESSAGES_KEY)
            .key(REGISTERED_CHATS_KEY)
            .key(registered_chat_key(chat_id))
            .arg(chat_id)
            .arg(filter)
            .invoke_async(connection)
            .await?
    }

    pub async fn acknowledge_message (
        connection,
        chat_id: i64,
        next_id: StreamId
    ) -> bool {
        script!("acknowledge_message.lua")
            .key(registered_chat_key(chat_id))
            .key(SCHEDULED_MESSAGES_KEY)
            .arg(next_id)
            .invoke_async(connection)
            .await?
    }

    pub async fn migrate_chat (
        connection,
        old_chat_id: i64,
        new_chat_id: i64
    ) -> bool {
        script!("migrate_chat.lua")
            .key(REGISTERED_CHATS_KEY)
            .key(registered_chat_key(old_chat_id))
            .key(registered_chat_key(new_chat_id))
            .arg(old_chat_id)
            .arg(new_chat_id)
            .invoke_async(connection)
            .await?
    }

    pub async fn unacknowledge_message (
        connection,
        chat_id: i64,
        message_id: StreamId
    ) -> Option<StreamId> {
        script!("unacknowledge_message.lua")
            .key(registered_chat_key(chat_id))
            .key(SCHEDULED_MESSAGES_KEY)
            .arg(message_id)
            .invoke_async(connection)
            .await?
    }

    pub async fn remove_subscription(connection, chat_id: i64) -> bool {
        let [result] = redis::pipe()
            .atomic()
            .add_command(Cmd::srem(REGISTERED_CHATS_KEY, chat_id))
            .add_command(Cmd::del(registered_chat_key(chat_id)))
            .ignore()
            .query_async(connection)
            .await?;

        result
    }

    pub async fn get_active_chats(connection) -> Vec<i64> {
        connection.smembers(REGISTERED_CHATS_KEY).await?
    }

    pub async fn get_filters(connection, chat_id: i64) -> Vec<Filter> {
        let content : Option<String> = connection.hget(registered_chat_key(chat_id), "filter").await?;

        match content {
            Some(filter) => serde_json::from_str(&filter).map_err(RedisError::from)?,
            None => vec![]
        }
    }

    pub async fn next_message_ready(
        connection,
        stream_id: Option<StreamId>,
    ) -> StreamId {
        loop {
            let r = if let Some(stream_id) = stream_id {
                let response: Vec<((), Vec<(StreamId, Message)>)> = redis::cmd("XREAD")
                    .arg("BLOCK").arg(10000)
                    .arg("COUNT").arg(1)
                    .arg("STREAMS").arg(SCHEDULED_MESSAGES_KEY).arg(stream_id)
                    .query_async(connection)
                    .await?;

                response.into_iter().next().map(|(_, v)| v)
            } else {
                let response: Vec<(StreamId, Message)> = redis::cmd("XREVRANGE")
                    .arg(SCHEDULED_MESSAGES_KEY)
                    .arg("+").arg("-")
                    .arg("COUNT").arg(1)
                    .query_async(connection)
                    .await?;

                Some(response)
            };

            let id = r
                .and_then(|v| v.into_iter().next())
                .map(|(id, _)| id);

            if let Some(id) = id {
                break id
            } else if stream_id.is_none() {
                break StreamId::ZERO;
            }
        }
    }

    pub async fn get_next_message(
        connection,
        last_processed: StreamId,
    ) -> Option<(StreamId, Message)> {
        let response: Vec<((), Vec<(StreamId, Message)>)> =
            redis::cmd("XREAD")
                .arg("COUNT")
                .arg(1)
                .arg("STREAMS")
                .arg(SCHEDULED_MESSAGES_KEY)
                .arg(last_processed)
                .query_async(connection)
                .await?;

        response
            .into_iter()
            .next()
            .and_then(|(_, v)| v.into_iter().next())
    }

    pub async fn get_last_update(connection) -> Option<DateTime<Utc>> {
        if let Some(timestamp) = connection.get(LAST_UPDATE_KEY).await? {
            match DateTime::from_timestamp_millis(timestamp) {
                Some(d) => Some(d),
                None => invalid_type_error!(timestamp, "timestamp out of range")
            }

        } else {
            None
        }
    }

    pub async fn set_last_update(connection, timestamp: DateTime<Utc>) {
        connection.set(LAST_UPDATE_KEY, timestamp.timestamp_millis()).await?
    }

    #[reset_connection_on_error]
    pub async fn update_filter(connection, chat_id: i64, update: &impl Fn(&mut Vec<Filter>)) {
        let key = registered_chat_key(chat_id);
        let script_content : &'static str = include_str!("redis_scripts/add_subscription.lua");

        loop {
            let ((), current_filters): ((), Option<String>) = redis::pipe()
                .add_command(redis::cmd("WATCH").arg(&key).to_owned())
                .add_command(Cmd::hget(&key, "filter"))
                .query_async(connection)
                .await?;

            let mut filters = match &current_filters {
                Some(filter) => serde_json::from_str(filter).map_err(RedisError::from)?,
                None => vec![]
            };

            update(&mut filters);

            let value: redis::Value = if filters.is_empty() {
                if current_filters.is_some() {
                    redis::pipe()
                        .atomic()
                        .add_command(Cmd::srem(REGISTERED_CHATS_KEY, chat_id))
                        .add_command(Cmd::del(registered_chat_key(chat_id)))
                        .query_async(connection)
                        .await?
                } else {
                    // nothing has changed
                    break
                }
            } else {
                let filter_str = serde_json::to_string(&filters).map_err(RedisError::from)?;

                let mut script = redis::cmd("EVAL");
                script.arg(script_content).arg(3).arg(&[SCHEDULED_MESSAGES_KEY,REGISTERED_CHATS_KEY, &key]).arg(chat_id).arg(&filter_str);

                redis::pipe()
                    .atomic()
                    .add_command(script)
                    .query_async(connection)
                    .await?
            };

            if !matches!(value, redis::Value::Nil) {
                break
            }
        }
    }

    pub async fn get_chat_state(
        connection,
        chat_id: i64,
    ) -> ChatState {
        let (last_sent, migrated) = connection.hget(registered_chat_key(chat_id), &["last_sent", "migrated"]).await?;

        if let Some(last_sent) = last_sent {
            ChatState::Active {  last_sent }
        } else if let Some(to) = migrated {
            ChatState::Migrated { to }
        } else {
            ChatState::Stopped
        }
    }

}
