use std::collections::HashSet;
use std::time::Duration;

use chrono::NaiveDate;
use lazy_static::lazy_static;
use reqwest::Client;
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use teloxide::prelude::*;
use teloxide::types::{ChatId, InlineKeyboardButton, InlineKeyboardMarkup, ParseMode};
use teloxide::utils::html;
use tokio::time::{interval, MissedTickBehavior};
use url::Url;

use crate::database::RedisClient;
use crate::{feed, Error, FEED_URL};

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

async fn scrape_website(client: &Client, url: &str) -> Result<WebsiteData, Error> {
    log::info!("Scraping website at {url}");

    let url = Url::parse(url)?;

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
    } = scrape_website(client, &item.link)
        .await
        .inspect_err(|e| log::warn!("Couldn't scrape website: {e}"))
        .unwrap_or_default();

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
        msg += &html::escape(&gremien.join(" | "));
    }

    if let Some(dsnr) = dsnr {
        msg += "\n📎 Ds.-Nr. ";
        msg += &html::escape(dsnr);
    }

    let button1 = url.map(|url| InlineKeyboardButton::url("🌐 Allris", url));
    let button2 = sammeldokument.map(|url| InlineKeyboardButton::url("📄 PDF", url));
    let buttons = [button1, button2].into_iter().flatten().collect();

    Some((msg, buttons))
}

#[derive(Serialize, Deserialize)]
pub struct SavedState {
    pub date: NaiveDate,
    pub known_guids: HashSet<String>,
}

enum UpdateChatId {
    Keep,
    Remove,
    Migrate(ChatId),
}

async fn send_message(
    bot: &Bot,
    mut chat: ChatId,
    msg: &str,
    buttons: &[InlineKeyboardButton],
) -> UpdateChatId {
    const MAX_TRIES: usize = 3;
    let mut update = UpdateChatId::Keep;
    for _ in 0..MAX_TRIES {
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
                    | CantTalkWithBots => return UpdateChatId::Remove,
                    Unknown(e) if &e == "Forbidden: bot was kicked from the channel chat" => {
                        return UpdateChatId::Remove
                    }
                    _ => {
                        // Invalid message
                        return UpdateChatId::Keep;
                    }
                },
                MigrateToChatId(c) => {
                    chat = c;
                    update = UpdateChatId::Migrate(c);
                }
                RetryAfter(secs) => {
                    tokio::time::sleep(secs.duration()).await;
                }
                _ => return UpdateChatId::Keep,
            }
        } else {
            break;
        }
    }

    update
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

            for user in redis.get_users().await? {
                match send_message(bot, user, &msg, &buttons).await {
                    UpdateChatId::Keep => (),
                    UpdateChatId::Remove => {
                        let _ = redis.unregister_user(user).await;
                    }
                    UpdateChatId::Migrate(c) => {
                        let _ = redis.unregister_user(user).await;
                        let _ = redis.register_user(c).await;
                    }
                }
            }
        }
    }

    let new_state = SavedState {
        date: current_date,
        known_guids: channel.item.into_iter().map(|x| x.guid).collect(),
    };
    redis.save_state(new_state).await?;

    Ok(())
}

pub async fn feed_updater(bot: Bot, redis: RedisClient) {
    let mut interval = interval(Duration::from_secs(300));
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay); // not that it will probably happen

    loop {
        interval.tick().await;
        log::info!("Updating ...");
        match do_update(&bot, &redis).await {
            Ok(()) => log::info!("Update finished!"),
            Err(e) => log::error!("Update failed: {e}"),
        }
    }
}
