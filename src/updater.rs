use std::time::Duration;

use lazy_static::lazy_static;
use reqwest::Client;
use scraper::{ElementRef, Html, Selector};
use serde::Deserialize;
use teloxide::prelude::*;
use teloxide::types::{ChatId, InlineKeyboardButton, InlineKeyboardMarkup, ParseMode};
use teloxide::utils::html;
use thiserror::Error;
use tokio::sync::oneshot;
use tokio::time::{interval, MissedTickBehavior};
use url::Url;

use crate::database::RedisClient;
use crate::Bot;

const FEED_URL: &str = "https://www.bonn.sitzung-online.de/rss/voreleased";
const ADDITIONAL_ERRORS: &[&str] = &[
    "Forbidden: bot was kicked from the channel chat",
    "Forbidden: bot was kicked from the group chat",
    "Bad Request: not enough rights to send text messages to the chat",
];

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

#[derive(Debug, Error)]
enum Error {
    #[error("web request error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("invalid feed format: {0}")]
    ParseXML(#[from] serde_xml_rs::Error),
    #[error("redis error: {0}")]
    Redis(#[from] redis::RedisError),
    #[error("parsing url failed: {0}")]
    ParseUrl(#[from] url::ParseError),
}

#[derive(Deserialize, Debug)]
struct Rss {
    channel: Channel,
}

#[derive(Deserialize, Debug)]
struct Channel {
    #[serde(default)]
    item: Vec<Item>,
}

#[derive(Deserialize, Debug)]
struct Item {
    title: String,
    link: String,
    description: String,
}

async fn fetch_feed(url: &str) -> Result<Channel, Error> {
    let response = reqwest::get(url).await?.text().await?;
    let rss: Rss = serde_xml_rs::from_str(&response)?;
    Ok(rss.channel)
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
    art: Option<String>,
    verfasser: Option<String>,
    amt: Option<String>,
    gremien: Vec<String>,
    sammeldokument: Option<Url>,
}

async fn scrape_website_inner(client: &Client, url: &Url) -> Result<WebsiteData, Error> {
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

async fn scrape_website(client: &Client, url: &str) -> (Option<Url>, Result<WebsiteData, Error>) {
    log::info!("Scraping website at {url}");
    let url = match Url::parse(url) {
        Ok(url) => url,
        Err(e) => {
            return (None, Err(e.into()));
        }
    };
    let data = scrape_website_inner(client, &url).await;
    (Some(url), data)
}

async fn generate_notification(
    client: &Client,
    item: &Item,
) -> Option<(String, Vec<InlineKeyboardButton>)> {
    let title = TITLE_REGEX.captures(&item.description)?.get(1)?.as_str();
    let dsnr = item.title.strip_prefix("Vorlage ");

    let (url, data) = scrape_website(client, &item.link).await;

    let WebsiteData {
        art,
        verfasser,
        amt,
        gremien,
        sammeldokument,
    } = data
        .inspect_err(|e| log::warn!("Couldn't scrape website: {e}"))
        .unwrap_or_default();

    let verfasser = match (art.as_deref(), &verfasser, &amt) {
        (Some("Anregungen und Beschwerden" | "Informationsbrief"), _, _) => None,
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
    const MAX_TRIES: usize = 5;
    const BASE_DELAY: Duration = Duration::from_millis(500);
    const MULTIPLIER: u32 = 3;

    let mut delay = BASE_DELAY;

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
                    Unknown(e) if ADDITIONAL_ERRORS.contains(&e.as_str()) => {
                        return UpdateChatId::Remove
                    }
                    _ => {
                        // Invalid message probably
                        return update;
                    }
                },
                MigrateToChatId(c) => {
                    chat = c;
                    update = UpdateChatId::Migrate(c);
                }
                RetryAfter(secs) => {
                    tokio::time::sleep(secs.duration()).await;
                }
                _ => {
                    tokio::time::sleep(delay).await;
                    delay *= MULTIPLIER;
                }
            }
        } else {
            break;
        }

        log::info!("Retrying ...")
    }

    update
}

async fn do_update(bot: &Bot, redis: &mut RedisClient) -> Result<(), Error> {
    let channel = fetch_feed(FEED_URL).await?;
    let client = reqwest::Client::new();

    for item in &channel.item {
        let Some(volfdnr) = item
            .link
            .strip_prefix("https://www.bonn.sitzung-online.de/vo020?VOLFDNR=")
        else {
            log::warn!(
                "Link deviates from the usual pattern, skipping: {}",
                item.link
            );
            continue;
        };

        // if this fails, just abort the whole operation. If redis is down, we will just try again on a later invocation.
        if !redis.add_item(volfdnr).await? {
            continue; // item already known (new version)
        }

        let Some((msg, buttons)) = generate_notification(&client, item).await else {
            continue;
        };

        for user in redis.get_chats().await? {
            match send_message(bot, user, &msg, &buttons).await {
                UpdateChatId::Keep => (),
                UpdateChatId::Remove => {
                    let _ = redis.unregister_chat(user).await;
                }
                UpdateChatId::Migrate(c) => {
                    let _ = redis.unregister_chat(user).await;
                    let _ = redis.register_chat(c).await;
                }
            }
        }
    }

    Ok(())
}

pub async fn feed_updater(bot: Bot, mut redis: RedisClient, mut shutdown: oneshot::Receiver<()>) {
    let mut interval = interval(Duration::from_secs(300));
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay); // not that it will probably happen

    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = interval.tick() => ()
        };

        log::info!("Updating ...");
        match do_update(&bot, &mut redis).await {
            Ok(()) => log::info!("Update finished!"),
            Err(e) => log::error!("Update failed: {e}"),
        }
    }
}

// As soon as this fails, the error handling in `send_message` must be adapted
#[test]
fn test_api_error_not_yet_added() {
    use teloxide::ApiError;

    for msg in ADDITIONAL_ERRORS {
        let api_error: ApiError = serde_json::from_str(&format!("\"{msg}\"")).unwrap();
        assert_eq!(api_error, ApiError::Unknown(msg.to_string()));
    }
}
