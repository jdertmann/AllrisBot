mod database;
mod updater;

#[cfg(feature = "handle_updates")]
mod dispatcher;

use std::process::ExitCode;

use database::RedisClient;
use teloxide::adaptors::Throttle;
use tokio::sync::oneshot;

type Bot = Throttle<teloxide::Bot>;

#[tokio::main]
async fn main() -> ExitCode {
    env_logger::init();

    let Ok(redis_url) = std::env::var("REDIS_URL") else {
        log::error!("Environment variable REDIS_URL not set!");
        return ExitCode::FAILURE;
    };

    let Ok(mut redis_client) = RedisClient::new(&redis_url)
        .await
        .inspect_err(|e| log::error!("Redis connection failed: {e}"))
    else {
        return ExitCode::FAILURE;
    };

    // remove again once deployed
    let _ = redis_client.migrate_db().await;

    let (bot, worker) = Throttle::new(teloxide::Bot::from_env(), Default::default());
    let throttle_handle = tokio::spawn(worker);

    let shutdown_dispatcher;

    #[cfg(feature = "handle_updates")]
    {
        let mut dispatcher = dispatcher::create(bot.clone(), redis_client.clone());
        let shutdown_token = dispatcher.shutdown_token();
        tokio::spawn(async move { dispatcher.dispatch().await });
        shutdown_dispatcher = || async move {
            if let Ok(f) = shutdown_token.shutdown() {
                f.await
            }
        };
    }

    #[cfg(not(feature = "handle_updates"))]
    {
        shutdown_dispatcher = || std::future::ready(());
    }

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let updater_handle = tokio::spawn(updater::feed_updater(bot, redis_client, shutdown_rx));

    match tokio::signal::ctrl_c().await {
        Ok(_) => (),
        Err(e) => log::error!("Unable to listen for shutdown signal: {e}"),
    }

    log::info!("Shutting down ...");
    let _ = shutdown_tx.send(());
    let _ = tokio::join!(shutdown_dispatcher(), updater_handle, throttle_handle);

    ExitCode::SUCCESS
}
