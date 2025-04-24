mod allris;
mod bot;
mod broadcasting;
mod database;
mod lru_cache;
mod types;

use std::borrow::Cow;
use std::error::Error;
use std::pin::{Pin, pin};
use std::process::ExitCode;
use std::time::Duration;

use broadcasting::broadcast_task;
use clap::Parser;
use database::DatabaseConnection;
use redis::{ConnectionInfo, IntoConnectionInfo};
use tokio::sync::{mpsc, oneshot};
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
        // the redis crate implements no other way to get to a human-friendly error description
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

fn escape_html<'a, T: Into<Cow<'a, str>>>(input: T) -> Cow<'a, str> {
    const SPECIAL_CHARS: [char; 5] = ['&', '<', '>', '"', '\''];
    const REPLACE: [(char, &str); 5] = [
        ('&', "&amp;"),
        ('<', "&lt;"),
        ('>', "&gt;"),
        ('"', "&quot;"),
        ('\'', "&#39;"),
    ];

    let input = input.into();

    match input.find(SPECIAL_CHARS) {
        Some(index) => {
            let mut escaped = String::with_capacity(input.len() + 1);

            escaped.push_str(&input[0..index]);

            for c in input[index..].chars() {
                if let Some(replace) = REPLACE.iter().find(|&(x, _)| *x == c).map(|(_, y)| y) {
                    escaped.push_str(replace);
                } else {
                    escaped.push(c);
                }
            }

            Cow::Owned(escaped)
        }
        None => input,
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();

    init_logging(&args);

    let db_client = redis::Client::open(args.redis_url).unwrap();
    let bot = frankenstein::client_reqwest::Bot::new(&args.bot_token);

    macro_rules! pin_dyn {
        ($x:expr) => {
            pin!($x) as Pin<&mut dyn Future<Output = _>>
        };
    }

    let (bot_handle, bot_shutdown) = if args.ignore_messages {
        let (tx, _) = oneshot::channel();

        (pin_dyn!(async { Ok(()) }), tx)
    } else {
        let (tx, rx) = oneshot::channel();

        let handle = tokio::spawn(bot::run(
            bot.clone(),
            DatabaseConnection::new(db_client.clone(), Some(Duration::from_secs(6))).shared(),
            args.owner,
            rx,
        ));

        (pin_dyn!(handle), tx)
    };

    let scraper = allris::scraper(
        args.allris_url,
        Duration::from_secs(args.update_interval),
        db_client.clone(),
    );

    let scraper_handle = tokio::spawn(scraper);

    let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
    let mut broadcaster_handle = tokio::spawn(broadcast_task(bot, db_client, ctrl_rx));

    tokio::signal::ctrl_c()
        .await
        .expect("Unable to listen for shutdown signal");

    log::info!("Shutting down broadcasting");

    scraper_handle.abort();
    let _ = ctrl_tx.send(broadcasting::ShutdownSignal::Soft);

    let success = tokio::select! {
        _ = &mut broadcaster_handle => true,
        _ = tokio::signal::ctrl_c() => false,
        _ = tokio::time::sleep(Duration::from_secs(20)) => false
    };

    if !success {
        log::warn!("Not all pending messages have been sent, shutting down anyway ...");
        let _ = ctrl_tx.send(broadcasting::ShutdownSignal::Hard);
        let _ = broadcaster_handle.await;
    }

    // We want users to be able to stop broadcasts even if we're in the process of shutting down,
    // so this comes last
    log::info!("Shutting down bot");
    let _ = bot_shutdown.send(());
    let _ = bot_handle.await;

    ExitCode::SUCCESS
}
