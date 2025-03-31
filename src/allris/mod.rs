mod html;
mod oparl;

use std::collections::BTreeMap;
use std::pin::pin;
use std::time::Duration;

use chrono::{NaiveDate, Utc};
use futures_util::{Stream, TryStreamExt};
use oparl::Paper;
use reqwest::{Client, Response};
use teloxide::types::InlineKeyboardButton;
use teloxide::utils::html::{bold, escape};
use thiserror::Error;
use tokio::time::{MissedTickBehavior, interval};
use tokio_retry::RetryIf;
use tokio_retry::strategy::ExponentialBackoff;
use url::Url;

use self::html::{WebsiteData, scrape_website};
use crate::database::{self, DatabaseConnection};
use crate::types::{Message, Tag};

#[derive(Debug, Error)]
pub enum Error {
    #[error("web request error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("db error: {0}")]
    Database(#[from] database::Error),
    #[error("parsing url failed: {0}")]
    ParseUrl(#[from] url::ParseError),
}

async fn http_request<T, Fut: Future<Output = reqwest::Result<T>>>(
    client: &Client,
    url: &Url,
    f: impl Fn(Response) -> Fut,
) -> reqwest::Result<T> {
    let action = || async { f(client.get(url.clone()).send().await?.error_for_status()?).await };
    let retry_strategy = ExponentialBackoff::from_millis(20).take(3);
    let retry_condition =
        |e: &reqwest::Error| !matches!(e.status(), Some(status) if !status.is_server_error());

    RetryIf::spawn(retry_strategy, action, retry_condition).await
}

fn generate_tags(dsnr: Option<&str>, paper: &Paper, data: &WebsiteData) -> Vec<(Tag, String)> {
    use Tag::*;

    let mut tags = vec![];

    if let Some(dsnr) = dsnr {
        tags.push((Dsnr, dsnr.to_string()));
    }

    let WebsiteData {
        unterstuetzer,
        amt,
        gremien,
        beteiligt,
        ..
    } = data;

    if let Some(paper_type) = &paper.paper_type {
        tags.push((Art, paper_type.clone()));
    }

    for verfasser in unterstuetzer {
        tags.push((Verfasser, verfasser.clone()));
    }

    if let Some(amt) = amt {
        tags.push((Federführend, amt.clone()));
        tags.push((Beteiligt, amt.clone()));
    }

    for amt in beteiligt {
        tags.push((Beteiligt, amt.clone()))
    }

    for gremium in gremien {
        tags.push((Gremium, gremium.clone()));
    }

    tags
}

async fn generate_notification(client: &Client, paper: &Paper) -> Option<Message> {
    let title = paper.name.as_deref()?;
    let dsnr = paper.reference.as_deref();
    let url = paper.web.as_ref()?;

    let data = scrape_website(client, url).await;

    let data = match data {
        Ok(data) => data,
        Err(e) => {
            log::warn!("Couldn't scrape website: {e}");
            WebsiteData::default()
        }
    };

    let tags = generate_tags(dsnr, paper, &data);

    let WebsiteData {
        verfasser,
        amt,
        gremien,
        already_discussed,
        ..
    } = data;

    if already_discussed {
        // was already discussed, probably old document, skipping
        log::info!("Skipping {dsnr:?} ({title}): was already discussed");
        return None;
    }

    let verfasser = match (paper.paper_type.as_deref(), &verfasser, &amt) {
        (Some("Anregungen und Beschwerden" | "Einwohnerfrage" | "Informationsbrief"), _, _) => {
            // author is meaningless here, it's always the same Amt.
            None
        }
        (Some("Stellungnahme der Verwaltung"), _, Some(amt)) => Some(amt),
        (_, Some(verfasser), _) => Some(verfasser),
        (_, None, Some(amt)) => Some(amt),
        _ => None,
    };

    let mut msg = bold(title) + "\n";

    if let Some(paper_type) = paper.paper_type.as_deref() {
        msg += "\n📌 ";
        msg += &escape(paper_type);
    }

    if let Some(verfasser) = verfasser {
        msg += "\n👤 ";
        msg += &escape(verfasser);
    }

    if !gremien.is_empty() {
        msg += "\n🏛️ ";
        msg += &escape(&gremien.join(" | "));
    }

    if let Some(dsnr) = dsnr {
        msg += "\n📎 Ds.-Nr. ";
        msg += &escape(dsnr);
    }

    let mut buttons = vec![InlineKeyboardButton::url("🌐 Allris", url.clone())];
    buttons.extend(
        paper
            .main_file
            .as_ref()
            .map(|file| InlineKeyboardButton::url("📄 PDF", file.access_url.clone())),
    );

    Some(Message {
        text: msg,
        parse_mode: teloxide::types::ParseMode::Html,
        buttons,
        tags,
    })
}

async fn send_notifications(
    db: &mut DatabaseConnection,
    http_client: Client,
    papers: impl Stream<Item = Result<Paper, Error>>,
) -> Result<(), Error> {
    // if operations fail, it is ok to abort the whole function (`?` operator).
    // If redis or network connection is down, we'll just have to try again on a later invocation.

    // collect items to BTreeMap to ensure ascending order
    let mut papers_map: BTreeMap<String, Paper> = BTreeMap::new();
    let mut papers = pin!(papers);
    while let Some(paper) = papers.try_next().await? {
        match paper.id.query_pairs().find(|(q, _)| q == "id") {
            Some((_, volfdnr)) => {
                if !db.is_known_volfdnr(&volfdnr).await? {
                    papers_map.insert(volfdnr.to_string(), paper);
                }
            }
            None => {
                log::warn!("Link deviates from usual pattern, skipping: {}", paper.id);
            }
        }
    }

    for (volfdnr, paper) in papers_map {
        if let Some(message) = generate_notification(&http_client, &paper).await {
            db.schedule_broadcast(&volfdnr, &message).await?;
        } else {
            db.add_known_volfdnr(&volfdnr).await?;
        }
    }

    Ok(())
}

pub async fn scan_day(
    allris_url: &AllrisUrl,
    db: &mut DatabaseConnection,
    day: NaiveDate,
) -> Result<(), Error> {
    let http_client = reqwest::Client::new();
    let papers = oparl::get_day(&http_client, allris_url, day);
    send_notifications(db, http_client, papers).await
}

pub async fn do_update(
    allris_url: &AllrisUrl,
    db_conn: &mut DatabaseConnection,
) -> Result<(), Error> {
    let Some(last_updated) = db_conn.get_last_update().await? else {
        // the very first invocation :) save the timestamp but do nothing yet
        db_conn.set_last_update(Utc::now()).await?;
        return Ok(());
    };

    let update_started = Utc::now();
    let http_client = reqwest::Client::new();
    let papers = oparl::get_update(&http_client, allris_url, last_updated);
    send_notifications(db_conn, http_client, papers).await?;
    db_conn.set_last_update(update_started).await?;

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
}

pub async fn scraper(allris_url: AllrisUrl, update_interval: Duration, db: redis::Client) {
    let db_timeout = Some(Duration::from_secs(10));

    let mut interval = interval(update_interval);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay); // not that it will probably happen

    loop {
        interval.tick().await;

        log::info!("Updating ...");
        let mut db_conn = DatabaseConnection::new(db.clone(), db_timeout);
        match do_update(&allris_url, &mut db_conn).await {
            Ok(()) => log::info!("Update finished!"),
            Err(e) => log::error!("Update failed: {e}"),
        }
    }
}
