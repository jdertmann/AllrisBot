mod allris;
mod bot_commands;
mod broadcasting;
mod database;
mod types;

use std::process::ExitCode;
use std::time::Duration;

use broadcasting::broadcast_task;
use clap::Parser;
use redis::{ConnectionInfo, IntoConnectionInfo};
use tokio::sync::mpsc;

use crate::allris::AllrisUrl;

type Bot = teloxide::Bot;

/// Telegram bot that notifies about newly published documents in the Allris 4 council information system.
#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    /// URL of the Redis instance
    #[arg(short, long, env = "REDIS_URL", value_parser = |s: &str| s.into_connection_info(), default_value = "redis://127.0.0.1")]
    redis_url: ConnectionInfo,

    /// Telegram bot token
    #[arg(short = 't', long = "token", env = "BOT_TOKEN")]
    bot_token: String,

    /// Ignore incoming messages
    #[arg(short, long)]
    ignore_messages: bool,

    /// URL of the Allris 4 instance
    #[arg(short, long, value_parser = AllrisUrl::parse, default_value = "https://www.bonn.sitzung-online.de/")]
    allris_url: AllrisUrl,

    /// Update interval in seconds
    #[arg(short, long, default_value_t = 900)]
    update_interval: u64,

    /// Increase verbosity
    #[arg(short, action = clap::ArgAction::Count)]
    verbose: u8,
}

fn init_logging(args: &Args) {
    let log_level = match args.verbose {
        0 => log::LevelFilter::Error,
        1 => log::LevelFilter::Warn,
        2 => log::LevelFilter::Info,
        3 => log::LevelFilter::Debug,
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
    let bot = teloxide::Bot::new(&args.bot_token);

    let dispatcher = if args.ignore_messages {
        bot_commands::DispatcherTask::do_nothing()
    } else {
        bot_commands::DispatcherTask::new(bot.clone(), db_client.clone())
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
