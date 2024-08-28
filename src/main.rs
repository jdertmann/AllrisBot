mod database;
mod feed;

use std::collections::HashSet;
use std::future;

use chrono::NaiveDate;
use database::RedisClient;
use lazy_static::lazy_static;
use reqwest::{Client, Url};
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use teloxide::prelude::*;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup, ParseMode};
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
    static ref DOKUMENTE_SELECTOR: Selector = Selector::parse("#dokumenteHeaderPanel a").unwrap();
}

fn extract_text(element: ElementRef) -> String {
    element
        .text()
        .collect::<Vec<_>>()
        .concat()
        .trim()
        .to_string()
}

#[derive(Default)]
struct WebsiteData {
    url: Option<Url>,
    art: Option<String>,
    verfasser: Option<String>,
    amt: Option<String>,
    gremien: Vec<String>,
    sammeldokument: Option<Url>,
}

async fn scrape_website_inner(client: &Client, url: Url) -> reqwest::Result<WebsiteData> {
    let html = client
        .get(url.clone())
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    let document = Html::parse_document(&html);

    let gremien: Vec<_> = document
        .select(&GREMIEN_SELECTOR)
        .map(extract_text)
        .filter(|s| !s.is_empty())
        .collect();

    let sammeldokument: Option<Url> = document
        .select(&DOKUMENTE_SELECTOR)
        .find(|el| el.text().next() == Some("Sammeldokument"))
        .and_then(|el| el.attr("href"))
        .and_then(|s| url.join(s).ok());

    Ok(WebsiteData {
        url: Some(url),
        art: document.select(&VOART_SELECTOR).next().map(extract_text),
        verfasser: document
            .select(&VOVERFASSER_SELECTOR)
            .next()
            .map(extract_text),
        amt: document.select(&VOFAMT_SELECTOR).next().map(extract_text),
        gremien,
        sammeldokument,
    })
}

async fn scrape_website(client: &Client, url: &str) -> Option<WebsiteData> {
    let url = Url::parse(url)
        .inspect_err(|e| log::warn!("Invalid url: {e}"))
        .ok()?;
    scrape_website_inner(client, url)
        .await
        .inspect_err(|e| log::warn!("Unable to fetch website: {e}"))
        .ok()
}

async fn generate_notification(
    client: &Client,
    item: &feed::Item,
) -> Option<(String, Vec<InlineKeyboardButton>)> {
    let title = TITLE_REGEX.captures(&item.description)?.get(1)?.as_str();
    let dsnr = item.title.strip_prefix("Vorlage ");

    let WebsiteData {
        url,
        art,
        verfasser,
        amt,
        gremien,
        sammeldokument,
    } = scrape_website(client, &item.link).await.unwrap_or_default();

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
        msg += &html::escape(verfasser);
    }

    if !gremien.is_empty() {
        msg += "\n🏛️ ";
        msg += &html::escape(&gremien.join(" — "));
    }

    if let Some(dsnr) = dsnr {
        msg += "\n📎 Ds.-Nr. ";
        msg += &html::escape(dsnr);
    }

    let button1 = url.map(|url| InlineKeyboardButton::url("🌐 Allris", url));
    let button2 = sammeldokument.map(|url| InlineKeyboardButton::url("📄 Sammeldokument", url));
    let buttons = [button1, button2].into_iter().flatten().collect();

    Some((msg, buttons))
}

#[derive(Serialize, Deserialize)]
pub struct SavedState {
    pub date: NaiveDate,
    pub known_guids: HashSet<String>,
}

async fn send_message(
    bot: &Bot,
    chat: ChatId,
    msg: &str,
    buttons: &[InlineKeyboardButton],
) -> Result<(), Option<ChatId>> {
    let mut request = bot.send_message(chat, msg).parse_mode(ParseMode::Html);

    if !buttons.is_empty() {
        request = request.reply_markup(InlineKeyboardMarkup::new(vec![buttons.to_owned()]));
    }

    if let Err(e) = request.await {
        log::warn!("Sending notification failed: {e}");
        use teloxide::ApiError::*;
        use teloxide::RequestError::*;
        match e {
            Api(e) => match e {
                BotBlocked
                | ChatNotFound
                | GroupDeactivated
                | BotKicked
                | BotKickedFromSupergroup
                | UserDeactivated
                | CantInitiateConversation
                | CantTalkWithBots => return Err(None),
                Unknown(e) if &e == "Forbidden: bot was kicked from the channel chat" => {
                    return Err(None)
                }
                InvalidToken | MessageTextIsEmpty | MessageIsTooLong | ButtonUrlInvalid
                | CantParseEntities(_) | WrongHttpUrl | _ => (),
            },
            MigrateToChatId(c) => {
                return Err(Some(c));
            }
            RetryAfter(secs) => {
                tokio::time::sleep(secs.duration()).await;
            }
            _ => (),
        }
    }
    Ok(())
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

            let Some((msg, buttons)) = generate_notification(&client, item).await else {
                continue;
            };

            let users = redis.get_users().await?;

            for user in users {
                if let Err(new_chat) = send_message(bot, user, &msg, &buttons).await {
                    let _ = redis.unregister_user(user).await;
                    if let Some(new_chat) = new_chat {
                        let _ = redis.register_user(new_chat).await;
                        let _ = send_message(bot, new_chat, &msg, &buttons).await;
                    }
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

async fn handle_message(
    bot: Bot,
    msg: Message,
    cmd: Command,
    redis_client: RedisClient,
) -> ResponseResult<()> {
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
    }
    Ok(())
}

async fn handle_update_perm(
    update: ChatMemberUpdated,
    redis_client: RedisClient,
) -> ResponseResult<()> {
    if update.new_chat_member.can_post_messages() {
        match redis_client.register_user(update.chat.id).await {
            Ok(_) => log::info!("Added channel \"{}\"", update.chat.title().unwrap_or("")),
            Err(e) => log::error!(
                "Adding channel \"{}\" failed: {e}",
                update.chat.title().unwrap_or("<unknown>")
            ),
        }
    } else {
        match redis_client.unregister_user(update.chat.id).await {
            Ok(_) => log::info!("Removed channel \"{}\"", update.chat.title().unwrap_or("")),
            Err(e) => log::error!(
                "Removing channel \"{}\" failed: {e}",
                update.chat.title().unwrap_or("<unknown>")
            ),
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    env_logger::init();

    let bot = Bot::from_env();
    let redis_client = database::RedisClient::new("redis://127.0.0.1/");

    tokio::spawn(feed_updater(bot.clone(), redis_client.clone()));

    let handler = dptree::entry()
        .inspect(|u: Update| {
            log::debug!("{u:#?}"); // Print the update to the console with inspect
        })
        .branch(
            Update::filter_message()
                .filter_command::<Command>()
                .endpoint(handle_message),
        )
        .branch(
            Update::filter_my_chat_member()
                .filter(|x: ChatMemberUpdated| {
                    x.chat.is_channel()
                        && x.old_chat_member.can_post_messages()
                            != x.new_chat_member.can_post_messages()
                })
                .endpoint(handle_update_perm),
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
