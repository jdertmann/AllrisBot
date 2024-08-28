mod database;
mod feed;

use std::collections::HashSet;
use std::future;

use chrono::NaiveDate;
use database::RedisClient;
use lazy_static::lazy_static;
use reqwest::Client;
use scraper::{ElementRef, Html, Selector};
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

lazy_static! {
    static ref TITLE_REGEX: regex::Regex = regex::RegexBuilder::new("</h3>.*<h3>([^<]*)</h3>")
        .dot_matches_new_line(true)
        .build()
        .unwrap();
    static ref GREMIEN_SELECTOR: Selector =
        Selector::parse("#bfTable > table > tbody > tr > td:not(.date) + td:nth-of-type(3)")
            .unwrap();
    static ref VOART_SELECTOR: Selector = Selector::parse("#voart").unwrap();
    static ref VOFAMT_SELECTOR: Selector = Selector::parse("#vofamt").unwrap();
    static ref VOVERFASSER_SELECTOR: Selector = Selector::parse("#voverfasser1").unwrap();
}

async fn scrape_website(client: &Client, url: &str) -> [Option<String>; 4] {
    async fn get_html(client: &Client, url: &str) -> reqwest::Result<String> {
        client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await
    }

    fn get_text(elem: ElementRef) -> String {
        let mut text = String::new();
        for s in elem.text() {
            text += s;
        }
        return text.trim().to_string();
    }

    let html = match get_html(client, url).await {
        Ok(html) => html,
        Err(e) => {
            log::warn!("Unable to get site: {e}");
            return Default::default();
        }
    };

    let document = Html::parse_document(&html);

    let gremien: Vec<_> = document
        .select(&GREMIEN_SELECTOR)
        .map(get_text)
        .filter(|s| !s.is_empty())
        .collect();

    let gremien = if gremien.is_empty() {
        None
    } else {
        Some(gremien.join(", "))
    };

    [
        document.select(&VOART_SELECTOR).next().map(get_text),
        document.select(&VOVERFASSER_SELECTOR).next().map(get_text),
        document.select(&VOFAMT_SELECTOR).next().map(get_text),
        gremien,
    ]
}

async fn generate_notification(client: &Client, item: &feed::Item) -> Option<String> {
    let title = TITLE_REGEX.captures(&item.description)?.get(1)?.as_str();
    let dsnr = item.title.strip_prefix("Vorlage ");
    let [art, verfasser, amt, gremien] = scrape_website(client, &item.link).await;

    let verfasser = match (art.as_deref(), &verfasser, &amt) {
        (Some("Anregungen und Beschwerden"), _, _) => None,
        (Some("Stellungnahme der Verwaltung"), _, Some(amt)) => Some(amt),
        (_, Some(verfasser), _) => Some(verfasser),
        (_, None, Some(amt)) => Some(amt),
        _ => None,
    };

    let mut msg = html::bold(title) + "\n";

    if let Some(art) = art {
        msg += "\n📌 ";
        msg += &html::escape(&art);
    }

    if let Some(verfasser) = verfasser {
        msg += "\n👤 ";
        msg += &html::escape(&verfasser);
    }

    if let Some(gremien) = gremien {
        msg += "\n🏛️ ";
        msg += &html::escape(&gremien);
    }

    if let Some(dsnr) = dsnr {
        msg += "\n📎 Ds.-Nr. ";
        msg += &html::escape(&dsnr);
    }

    msg += &"\n👉 ";
    msg += &html::link(&item.link, &"Zur Vorlage");

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

        let client = reqwest::Client::new();

        for item in &channel.item {
            if known_guids.contains(&item.guid) {
                continue; // item already known
            }

            let Some(msg) = generate_notification(&client, &item).await else {
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

    let answer = move |bot: Bot, msg: Message, cmd: Command, redis_client: RedisClient| {
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
            Ok(()) as ResponseResult<()>
        }
    };

    let channel_perm_changed = move |upd: ChatMemberUpdated, redis_client: RedisClient| {
        let redis_client = redis_client.clone();
        async move {
            if upd.new_chat_member.can_post_messages() {
                match redis_client.register_user(upd.chat.id).await {
                    Ok(_) => log::info!("Added channel \"{}\"", upd.chat.title().unwrap_or("")),
                    Err(e) => log::error!(
                        "Adding channel \"{}\" failed: {e}",
                        upd.chat.title().unwrap_or("")
                    ),
                }
            } else {
                match redis_client.unregister_user(upd.chat.id).await {
                    Ok(_) => log::info!("Removed channel \"{}\"", upd.chat.title().unwrap_or("")),
                    Err(e) => log::error!(
                        "Removing channel \"{}\" failed: {e}",
                        upd.chat.title().unwrap_or("")
                    ),
                }
            }

            Ok(())
        }
    };

    let handler = dptree::entry()
        .inspect(|u: Update| {
            log::debug!("{u:#?}"); // Print the update to the console with inspect
        })
        .branch(
            Update::filter_message()
                .filter_command::<Command>()
                .endpoint(answer),
        )
        .branch(
            Update::filter_my_chat_member()
                .filter(|x: ChatMemberUpdated| {
                    x.chat.is_channel()
                        && x.old_chat_member.can_post_messages()
                            != x.new_chat_member.can_post_messages()
                })
                .endpoint(channel_perm_changed),
        );

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![redis_client])
        .error_handler(LoggingErrorHandler::with_custom_text(
            "An error has occurred in the dispatcher",
        ))
        .default_handler(|_| future::ready(()))
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}
