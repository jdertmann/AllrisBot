mod database;
mod feed;

use std::collections::HashSet;

use chrono::NaiveDate;
use database::RedisClient;
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use teloxide::prelude::*;
use teloxide::types::ParseMode;
use teloxide::utils::command::BotCommands;
use teloxide::utils::html;
use thiserror::Error;
use tokio::time::{interval, Duration, MissedTickBehavior};

const FEED_URL: &str = "https://www.bonn.sitzung-online.de/rss/voreleased";

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to retrieve feed: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("invalid feed format: {0}")]
    ParseError(#[from] serde_xml_rs::Error),
    #[error("redis error: {0}")]
    RedisError(#[from] redis::RedisError),
}

fn generate_notification(item: &feed::Item) -> Option<String> {
    lazy_static! {
        static ref TITLE_REGEX: regex::Regex = regex::RegexBuilder::new("</h3>.*<h3>([^<]*)</h3>")
            .dot_matches_new_line(true)
            .build()
            .unwrap();
        static ref HYPERLINK_TEXT: String = html::escape("Zur Vorlage");
        static ref BEFORE_HYPERLINK: String = html::escape("\n\n👉 ");
    }

    let title = TITLE_REGEX.captures(&item.description)?.get(1)?.as_str();
    let msg = html::bold(title) + &BEFORE_HYPERLINK + &html::link(&item.link, &HYPERLINK_TEXT);
    Some(msg)
}

#[derive(Serialize, Deserialize)]
pub struct SavedState {
    pub date: NaiveDate,
    pub known_guids: HashSet<String>,
}

async fn do_update(bot: &Bot, redis: &RedisClient) -> Result<(), Error> {
    let channel = feed::fetch_feed(FEED_URL).await?;
    let current_date = channel.pub_date.date_naive();

    if let Some(old_state) = redis.get_saved_state().await? {
        let known_guids = if old_state.date == current_date {
            old_state.known_guids
        } else {
            // Neuer Tag, neues Glück
            HashSet::new()
        };

        for item in &channel.item {
            if known_guids.contains(&item.guid) {
                continue; // item already known
            }

            let Some(msg) = generate_notification(&item) else {
                continue;
            };

            for user in redis.get_users().await? {
                let request = bot.send_message(user, &msg).parse_mode(ParseMode::Html);

                if let Err(e) = request.await {
                    log::warn!("Sending notification failed: {e}");
                    // TODO: Maybe retry or remove user from list
                }
            }
        }
    }

    let known_guids: HashSet<_> = channel.item.into_iter().map(|x| x.guid).collect();
    redis
        .save_state(SavedState {
            date: current_date,
            known_guids,
        })
        .await?;
    Ok(())
}

async fn feed_updater(bot: Bot, redis: RedisClient) {
    let mut interval = interval(Duration::from_secs(300));
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay); // not that it will probably happen

    loop {
        interval.tick().await;
        log::info!("Updating ...");
        match do_update(&bot, &redis).await {
            Ok(()) => log::info!("Update successful!"),
            Err(e) => log::error!("Update failed: {e}"),
        }
    }
}

#[derive(BotCommands, Clone, Debug)]
#[command(
    rename_rule = "lowercase",
    description = "Diese Befehle werden unterstützt:"
)]
enum Command {
    #[command(description = "für Benachrichtigungen registrieren.")]
    Start,
    #[command(description = "Benachrichtigungen abbestellen.")]
    Stop,
    #[command(description = "zeige diesen Text.")]
    Help,
}

#[tokio::main]
async fn main() {
    env_logger::init();

    let bot = Bot::from_env();
    let redis_client = database::RedisClient::new("redis://127.0.0.1/");

    tokio::spawn(feed_updater(bot.clone(), redis_client.clone()));

    let answer = move |bot: Bot, msg: Message, cmd: Command| {
        let redis_client = redis_client.clone();
        async move {
            match cmd {
                Command::Start => {
                    let reply = match redis_client.register_user(msg.chat.id).await {
                        Ok(true) => "Du hast dich erfolgreich für Benachrichtigungen registriert.",
                        Ok(false) => "Du bist bereits für Benachrichtigungen registriert.",
                        Err(e) => {
                            log::warn!("Database error: {e}");
                            "Ein interner Fehler ist aufgetreten :(("
                        }
                    };

                    bot.send_message(msg.chat.id, reply).await?;
                }
                Command::Stop => {
                    let reply = match redis_client.unregister_user(msg.chat.id).await {
                        Ok(true) => "Du hast die Benachrichtigungen abbestellt.",
                        Ok(false) => "Du warst nicht für Benachrichtigungen registriert.",
                        Err(e) => {
                            log::warn!("Database error: {e}");
                            "Ein interner Fehler ist aufgetreten :(("
                        }
                    };

                    bot.send_message(msg.chat.id, reply).await?;
                }
                Command::Help => {
                    bot.send_message(msg.chat.id, Command::descriptions().to_string())
                        .await?;
                }
            };
            Ok(())
        }
    };

    Command::repl(bot, answer).await;
}
