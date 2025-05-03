//! Telegram bot that notifies users about newly published documents in the Allris 4 council information system.
//!
//! This application consists of three main components:
//!
//! 1. **Allris Scraper ([`allris`] module)**: Regularly fetches the latest updates from the council information system's [OParl API](https://oparl.org/).
//! 2. **Broadcasting ([`broadcasting`] module)**: Sends scheduled notification messages to subscribed users.
//! 3. **Bot ([`bot`] module)**: Handles user interactions with the Telegram bot, including responding to messages and managing configurations.
//!
//! The application uses a Redis database ([`database`] module) to store its state and scheduled notifications.

mod allris;
mod bot;
mod broadcasting;
mod database;
mod lru_cache;
mod types;

use std::error::Error;
use std::process::ExitCode;
use std::time::Duration;

use bot_utils::broadcasting::Broadcaster;
use broadcasting::RedisBackend;
use clap::Parser;
use database::DatabaseConnection;
use redis::{ConnectionInfo, IntoConnectionInfo};
use tokio::sync::oneshot;
use url::Url;

use crate::allris::AllrisUrl;

type Bot = frankenstein::client_reqwest::Bot;

/// Telegram bot that notifies about newly published documents in the Allris 4 council information system.
#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    /// Telegram bot token
    #[arg(
        short = 't',
        long = "token",
        value_name = "TOKEN",
        env = "BOT_TOKEN",
        hide_env_values = true
    )]
    bot_token: String,

    /// URL of the Redis instance
    #[arg(
        short,
        long,
        value_name = "URL",
        env = "REDIS_URL",
        value_parser = parse_redis_url,
        default_value = "redis://127.0.0.1"
    )]
    redis_url: ConnectionInfo,

    /// URL of the Allris 4 instance
    #[arg(
        short,
        long,
        value_name = "URL",
        value_parser = AllrisUrl::parse,
        default_value = "https://www.bonn.sitzung-online.de/"
    )]
    allris_url: AllrisUrl,

    /// interval to check for new messages
    #[arg(short, long, value_name = "SECONDS", default_value_t = 900)]
    update_interval: u64,

    /// ignore incoming messages
    #[arg(long)]
    ignore_messages: bool,

    /// Telegram username of the bot's owner
    #[arg(short, long, value_parser = parse_owner_username)]
    owner: Option<String>,

    /// increase verbosity
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// disable logging
    #[arg(short, long, conflicts_with = "verbose")]
    quiet: bool,
}

fn parse_redis_url(input: &str) -> Result<ConnectionInfo, String> {
    let url = Url::parse(input).map_err(|e| e.to_string())?;
    let info = url.into_connection_info().map_err(
        // the `redis` crate implements no other way to get to a human-friendly error description
        #[allow(deprecated)]
        |e| e.description().to_string(),
    )?;
    Ok(info)
}

fn parse_owner_username(mut input: &str) -> Result<String, String> {
    if let Some(name) = input.strip_prefix('@') {
        input = name;
    }

    if input.chars().all(|x| x.is_ascii_alphanumeric() || x == '_') {
        Ok(input.into())
    } else {
        Err("Not a valid Telegram username".into())
    }
}

fn init_logging(args: &Args) {
    let log_level = match (args.quiet, args.verbose) {
        (true, _) => log::LevelFilter::Off,
        (_, 0) => log::LevelFilter::Error,
        (_, 1) => log::LevelFilter::Warn,
        (_, 2) => log::LevelFilter::Info,
        (_, 3) => log::LevelFilter::Debug,
        _ => log::LevelFilter::Trace,
    };

    env_logger::Builder::from_default_env()
        .filter_level(log_level)
        .filter_module("scraper", log::LevelFilter::Off)
        .filter_module("selectors", log::LevelFilter::Off)
        .filter_module("html5ever", log::LevelFilter::Off)
        .init();
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();

    init_logging(&args);

    // this will actually not establish a database connection, and will also not fail
    // since `args.redis_url` is already of type `ConnectionInfo`
    let db_client = redis::Client::open(args.redis_url).unwrap();
    let bot = frankenstein::client_reqwest::Bot::new(&args.bot_token);

    // star bot, the unless `--ignore-messages` flag is set
    let bot_shutdown = if args.ignore_messages {
        None
    } else {
        let (tx, rx) = oneshot::channel();

        let handle = tokio::spawn(bot::run(
            bot.clone(),
            DatabaseConnection::new(db_client.clone(), Some(Duration::from_secs(6))).shared(),
            args.owner,
            rx,
        ));

        Some((handle, tx))
    };

    // start Allris scraper task
    let scraper_task = allris::scraper(
        args.allris_url,
        Duration::from_secs(args.update_interval),
        db_client.clone(),
    );
    let scraper_handle = tokio::spawn(scraper_task);

    // start the broadcasting task
    let mut broadcaster = Broadcaster::new(RedisBackend::new(bot, db_client));

    // listen for CTRL+C
    tokio::signal::ctrl_c()
        .await
        .expect("Unable to listen for shutdown signal");

    log::info!("Shutting down ...");

    // enqueueing messages is transactional, so we can safely abort the task
    scraper_handle.abort();

    // wait until message queue is empty, unless CTRL+C is pressed a second time
    // or 20 seconds have passed
    let success = tokio::select! {
        _ = broadcaster.soft_shutdown() => true,
        _ = tokio::signal::ctrl_c() => false,
        _ = tokio::time::sleep(Duration::from_secs(20)) => false
    };

    if !success {
        log::warn!("Not all pending messages have been sent, shutting down anyway ...");
        broadcaster.hard_shutdown().await;
    }

    // We want users to be able to stop broadcasts even if we're in the process of shutting down,
    // so this comes last
    if let Some((handle, tx)) = bot_shutdown {
        log::info!("Shutting down bot");
        let _ = tx.send(());
        let _ = handle.await;
    }

    ExitCode::SUCCESS
}
