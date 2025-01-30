mod allris_scraper;
mod bot_commands;
mod database;
mod message_queue;

use std::process::ExitCode;
use std::time::Duration;

use database::RedisClient;
use tokio::sync::oneshot;

type Bot = teloxide::Bot;

#[tokio::main]
async fn main() -> ExitCode {
    env_logger::init();

    let Ok(redis_url) = std::env::var("REDIS_URL") else {
        log::error!("Environment variable REDIS_URL not set!");
        return ExitCode::FAILURE;
    };

    let Ok(redis_client) = RedisClient::new(&redis_url)
        .await
        .inspect_err(|e| log::error!("Redis connection failed: {e}"))
    else {
        return ExitCode::FAILURE;
    };

    let bot = teloxide::Bot::from_env();

    let shutdown_token = if false {
        None
    } else {
        let mut dispatcher = bot_commands::create(bot.clone(), redis_client.clone());
        let token = dispatcher.shutdown_token();
        tokio::spawn(async move { dispatcher.dispatch().await });
        Some(token)
    };

    let shutdown_dispatcher = || async move {
        if let Some(t) = shutdown_token {
            if let Ok(f) = t.shutdown() {
                f.await
            }
        }
    };

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let scraper_task_handle = tokio::spawn(allris_scraper::run_task(
        Duration::from_secs(900),
        redis_client.clone(),
        shutdown_rx,
    ));

    let _ = tokio::spawn(message_queue::task(bot.clone(), redis_client));

    match tokio::signal::ctrl_c().await {
        Ok(_) => (),
        Err(e) => log::error!("Unable to listen for shutdown signal: {e}"),
    }

    log::info!("Shutting down ...");
    let _ = shutdown_tx.send(());
    let _ = tokio::join!(shutdown_dispatcher(), scraper_task_handle);

    ExitCode::SUCCESS
}
