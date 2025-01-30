mod allris_scraper;
mod bot_commands;
mod database;
mod message_queue;

use std::process::ExitCode;

use clap::Parser;
use database::RedisClient;

use crate::allris_scraper::AllrisUrl;

type Bot = teloxide::Bot;

/// Telegram bot that notifies about newly published documents in the Allris 4 council information system.
#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    /// URL of the Redis instance
    #[arg(short, long, env = "REDIS_URL")]
    redis_url: String,

    /// Telegram bot token
    #[arg(short = 't', long, env = "BOT_TOKEN")]
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
        .init();
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();

    init_logging(&args);

    let redis_client = match RedisClient::new(&args.redis_url).await {
        Ok(client) => client,
        Err(e) => {
            log::error!("Redis connection failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let bot = teloxide::Bot::new(&args.bot_token);

    let dispatcher = if args.ignore_messages {
        bot_commands::DispatcherTask::do_nothing()
    } else {
        bot_commands::DispatcherTask::new(bot.clone(), redis_client.clone())
    };

    let scraper =
        allris_scraper::Scraper::new(args.allris_url, args.update_interval, redis_client.clone());

    let _ = tokio::spawn(message_queue::task(bot.clone(), redis_client));

    match tokio::signal::ctrl_c().await {
        Ok(_) => (),
        Err(e) => log::error!("Unable to listen for shutdown signal: {e}"),
    }

    log::info!("Shutting down ...");
    let _ = tokio::join!(dispatcher.shutdown(), scraper.shutdown());

    ExitCode::SUCCESS
}
