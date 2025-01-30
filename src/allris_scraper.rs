use std::time::Duration;

use lazy_static::lazy_static;
use reqwest::Client;
use scraper::{ElementRef, Html, Selector};
use serde::Deserialize;
use teloxide::types::InlineKeyboardButton;
use teloxide::utils::html;
use thiserror::Error;
use tokio::sync::oneshot;
use tokio::time::{interval, MissedTickBehavior};
use url::Url;

use crate::database::{self, Message, RedisClient};

const FEED_URL: &str = "https://www.bonn.sitzung-online.de/rss/voreleased";

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
    static ref TO_LINK_SELECTOR: Selector = Selector::parse("#bfTable .toLink a").unwrap();
}

#[derive(Debug, Error)]
enum Error {
    #[error("web request error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("invalid feed format: {0}")]
    ParseXML(#[from] serde_xml_rs::Error),
    #[error("db error: {0}")]
    DbError(#[from] database::DatabaseError),
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
    has_to_link: bool,
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

    let has_to_link = document.select(&TO_LINK_SELECTOR).next().is_some();

    Ok(WebsiteData {
        art: document.select(&VOART_SELECTOR).next().map(extract_text),
        verfasser: document
            .select(&VOVERFASSER_SELECTOR)
            .next()
            .map(extract_text),
        amt: document.select(&VOFAMT_SELECTOR).next().map(extract_text),
        gremien,
        sammeldokument,
        has_to_link,
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

async fn generate_notification(client: &Client, item: &Item) -> Option<(Message, Vec<String>)> {
    let title = TITLE_REGEX.captures(&item.description)?.get(1)?.as_str();
    let dsnr = item.title.strip_prefix("Vorlage ");

    let (url, data) = scrape_website(client, &item.link).await;

    let WebsiteData {
        art,
        verfasser,
        amt,
        gremien,
        sammeldokument,
        has_to_link,
    } = data
        .inspect_err(|e| log::warn!("Couldn't scrape website: {e}"))
        .unwrap_or_default();

    if has_to_link {
        // was already discussed, probably old Vorlage, skipping
        log::info!("Skipping {dsnr:?} ({title}): was already discussed");
        return None;
    }

    let verfasser = match (art.as_deref(), &verfasser, &amt) {
        (Some("Anregungen und Beschwerden" | "Einwohnerfrage" | "Informationsbrief"), _, _) => {
            // author is meaningless here, it's always the same Amt.
            None
        }
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

    Some((
        Message {
            text: msg,
            parse_mode: teloxide::types::ParseMode::Html,
            buttons,
        },
        gremien,
    ))
}

async fn do_update(db: &mut RedisClient) -> Result<(), Error> {
    let feed_content = fetch_feed(FEED_URL).await?;
    let http_client = reqwest::Client::new();

    let chats = db.get_chats().await?;

    for item in &feed_content.item {
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

        // if this fails, it is ok to abort the whole operation. If redis is down, we will just try again on a later invocation.
        if db.has_item(volfdnr).await? {
            continue; // item already known
        }

        let Some((msg, gremien)) = generate_notification(&http_client, item).await else {
            continue;
        };

        let chats = chats
            .iter()
            .filter(|(_, gremium)| gremium.is_empty() || gremien.contains(&gremium))
            .map(|(chat_id, _)| *chat_id);

        db.queue_messages(volfdnr, &msg, chats).await?;
    }

    Ok(())
}

pub async fn run_task(
    update_interval: Duration,
    mut db: RedisClient,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut interval = interval(update_interval);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay); // not that it will probably happen

    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = interval.tick() => ()
        };

        log::info!("Updating ...");

        match do_update(&mut db).await {
            Ok(()) => log::info!("Update finished!"),
            Err(e) => log::error!("Update failed: {e}"),
        }
    }
}
