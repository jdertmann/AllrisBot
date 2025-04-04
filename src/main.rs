mod admin;
mod allris;
mod bot_commands;
mod broadcasting;
mod database;
mod types;

use std::error::Error;
use std::process::ExitCode;
use std::time::Duration;

use admin::AdminToken;
use broadcasting::broadcast_task;
use clap::Parser;
use redis::{ConnectionInfo, IntoConnectionInfo};
use teloxide::adaptors::CacheMe;
use teloxide::prelude::RequesterExt;
use tokio::sync::mpsc;
use url::Url;

use crate::allris::AllrisUrl;

type Bot = CacheMe<teloxide::Bot>;

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

    /// generate an admin token, which will be valid for 10 minutes from startup
    #[arg(long, conflicts_with = "ignore_messages")]
    generate_admin_token: bool,

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
        #[allow(deprecated)]
        |e| e.description().to_string(),
    )?;
    Ok(info)
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

    let db_client = redis::Client::open(args.redis_url).unwrap();
    let bot = teloxide::Bot::new(&args.bot_token).cache_me();

    let admin_token = args.generate_admin_token.then(|| {
        let token = AdminToken::new();
        println!("Admin token (valid for 10 minutes): {token}");
        token
    });

    let dispatcher = if args.ignore_messages {
        bot_commands::DispatcherTask::do_nothing()
    } else {
        bot_commands::DispatcherTask::new(
            bot.clone(),
            db_client.clone(),
            args.allris_url.clone(),
            admin_token,
        )
    };

    let scraper = allris::scraper(
        args.allris_url,
        Duration::from_secs(args.update_interval),
        db_client.clone(),
    );

    tokio::spawn(scraper);

    let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
    let mut broadcaster_handle = tokio::spawn(broadcast_task(bot, db_client, ctrl_rx));

    tokio::signal::ctrl_c()
        .await
        .expect("Unable to listen for shutdown signal");

    log::info!("Shutting down ...");
    let _ = dispatcher.shutdown().await;

    let _ = ctrl_tx.send(broadcasting::ShutdownSignal::Soft);

    tokio::select! {
        _ = &mut broadcaster_handle => {
            return ExitCode::SUCCESS;
        },
        _ = tokio::signal::ctrl_c() => (),
        _ = tokio::time::sleep(Duration::from_secs(20)) => ()
    };

    log::warn!("Not all pending messages have been sent, shutting down anyway ...");
    let _ = ctrl_tx.send(broadcasting::ShutdownSignal::Hard);
    let _ = broadcaster_handle.await;

    ExitCode::SUCCESS
}
