use std::sync::LazyLock;
use std::time::Duration;

use reqwest::Client;
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use teloxide::types::InlineKeyboardButton;
use teloxide::utils::html;
use thiserror::Error;
use tokio::time::{MissedTickBehavior, interval};
use tokio_retry::RetryIf;
use tokio_retry::strategy::ExponentialBackoff;
use url::Url;

use crate::database::{self, DatabaseConnection};

static TITLE_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::RegexBuilder::new("</h3>.*<h3>([^<]*)</h3>")
        .dot_matches_new_line(true)
        .build()
        .unwrap()
});

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub text: String,
    pub parse_mode: teloxide::types::ParseMode,
    pub buttons: Vec<teloxide::types::InlineKeyboardButton>,
    pub gremien: Vec<String>,
}

macro_rules! select {
    ($document:expr, $selector:literal) => {{
        static SELECTOR: LazyLock<Selector> = LazyLock::new(|| Selector::parse($selector).unwrap());
        $document.select(&SELECTOR)
    }};
}

#[derive(Debug, Error)]
enum Error {
    #[error("web request error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("invalid feed format: {0}")]
    ParseXML(#[from] serde_xml_rs::Error),
    #[error("db error: {0}")]
    Database(#[from] database::Error),
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

#[cfg(not(feature = "feed_from_file"))]
async fn fetch_feed(url: Url) -> Result<Channel, Error> {
    let response = reqwest::get(url).await?.text().await?;
    let rss: Rss = serde_xml_rs::from_str(&response)?;
    Ok(rss.channel)
}

#[cfg(feature = "feed_from_file")]
async fn fetch_feed(_: Url) -> Result<Channel, Error> {
    let rss: Rss = serde_xml_rs::from_str(include_str!("../voreleased"))?;
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
    already_discussed: bool,
}

async fn get_html(client: &Client, url: &Url) -> Result<String, reqwest::Error> {
    let action = || async {
        client
            .get(url.clone())
            .send()
            .await?
            .error_for_status()?
            .text()
            .await
    };
    let retry_strategy = ExponentialBackoff::from_millis(20).take(3);
    let retry_condition =
        |e: &reqwest::Error| !matches!(e.status(), Some(status) if !status.is_server_error());

    RetryIf::spawn(retry_strategy, action, retry_condition).await
}

async fn scrape_website(client: &Client, url: &str) -> (Option<Url>, Result<WebsiteData, Error>) {
    log::info!("Scraping website at {url}");

    let url = match Url::parse(url) {
        Ok(url) => url,
        Err(e) => return (None, Err(e.into())),
    };

    let html = match get_html(client, &url).await {
        Ok(html) => html,
        Err(e) => return (Some(url), Err(e.into())),
    };

    let document = Html::parse_document(&html);

    let gremien: Vec<_> = select!(
        document,
        "#bfTable > table > tbody > tr > td:not(.date) + td:nth-of-type(3)"
    )
    .map(extract_text)
    .filter(|s| !s.is_empty())
    .collect();

    let sammeldokument: Option<Url> = select!(document, "#dokumenteHeaderPanel a")
        .find(|el| el.text().next() == Some("Sammeldokument"))
        .and_then(|el| el.attr("href"))
        .and_then(|s| url.join(s).ok());

    let already_discussed = select!(document, "#bfTable .toLink a").next().is_some();

    let data = WebsiteData {
        art: select!(document, "#voart").map(extract_text).next(),
        verfasser: select!(document, "#voverfasser1").map(extract_text).next(),
        amt: select!(document, "#vofamt").map(extract_text).next(),
        gremien,
        sammeldokument,
        already_discussed,
    };

    (Some(url), Ok(data))
}

async fn generate_notification(client: &Client, item: &Item) -> Option<Message> {
    let title = TITLE_REGEX.captures(&item.description)?.get(1)?.as_str();
    let dsnr = item.title.strip_prefix("Vorlage ");

    let (url, data) = scrape_website(client, &item.link).await;

    let WebsiteData {
        art,
        verfasser,
        amt,
        gremien,
        sammeldokument,
        already_discussed,
    } = match data {
        Ok(data) => data,
        Err(e) => {
            log::warn!("Couldn't scrape website: {e}");
            WebsiteData::default()
        }
    };

    #[cfg(not(feature = "feed_from_file"))]
    if already_discussed {
        // was already discussed, probably old document, skipping
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

    Some(Message {
        text: msg,
        parse_mode: teloxide::types::ParseMode::Html,
        buttons,
        gremien,
    })
}
async fn do_update(feed_url: Url, db: &redis::Client) -> Result<(), Error> {
    let feed_content = fetch_feed(feed_url).await?;

    let http_client = reqwest::Client::new();
    let mut db_conn =
        DatabaseConnection::connect(db.clone(), Some(Duration::from_secs(10))).await?;

    for item in &feed_content.item {
        let link = Url::parse(&item.link).ok();
        let Some(volfdnr) = link.as_ref().and_then(|link| {
            link.query_pairs()
                .find(|(q, _)| q == "VOLFDNR")
                .map(|(_, v)| v)
        }) else {
            log::warn!(
                "Link deviates from the usual pattern, skipping: {}",
                item.link
            );
            continue;
        };

        // if db operations fail, it is ok to abort the whole operation (`?` operator).
        // If redis is down, we'll just have to try again on a later invocation.

        if db_conn.is_known_volfdnr(&volfdnr).await? {
            continue; // item already known
        }

        if let Some(message) = generate_notification(&http_client, item).await {
            db_conn.schedule_broadcast(&volfdnr, &message).await?;
        } else {
            db_conn.add_known_volfdnr(&volfdnr).await?;
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
pub struct AllrisUrl {
    url: Url,
}

impl AllrisUrl {
    pub fn parse(input: &str) -> Result<Self, url::ParseError> {
        let mut url = Url::parse(input)?;

        let path = url.path();
        if !path.ends_with("/") {
            url.set_path(&format!("{path}/"));
        }

        Ok(Self { url })
    }

    fn feed_url(&self) -> Url {
        self.url.join("rss/voreleased").unwrap()
    }
}

pub async fn scraper(allris_url: AllrisUrl, update_interval: Duration, db: redis::Client) {
    let feed_url = allris_url.feed_url();

    let mut interval = interval(update_interval);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay); // not that it will probably happen

    loop {
        interval.tick().await;

        log::info!("Updating ...");

        match do_update(feed_url.clone(), &db).await {
            Ok(()) => log::info!("Update finished!"),
            Err(e) => log::error!("Update failed: {e}"),
        }
    }
}
