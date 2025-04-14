use std::future::ready;

use chrono::{DateTime, Days, NaiveDate, SecondsFormat, TimeZone, Utc};
use chrono_tz::Europe;
use futures_util::{Stream, TryStreamExt};
use reqwest::Response;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use url::Url;

use super::{AllrisUrl, Error};
use crate::allris::http_request;

const LOCAL_TZ: chrono_tz::Tz = Europe::Berlin;

/*
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Consultation {
    pub role: String,
    pub authoritative: bool,
    pub organization: Vec<String>,
}
*/

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct File {
    pub access_url: Url,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Paper {
    pub id: Url,
    pub name: Option<String>,
    pub reference: Option<String>,
    pub main_file: Option<File>,
    pub date: Option<NaiveDate>,
    pub paper_type: Option<String>,
    pub web: Option<Url>,
    // #[serde(default)]
    // pub consultation: Vec<Consultation>,
    pub deleted: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct Papers {
    data: Vec<Paper>,
    #[serde(default)]
    links: Links,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct Links {
    next: Option<Url>,
}

fn to_rfc3339(t: DateTime<impl TimeZone>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, false)
}

fn endpoint_url<T: TimeZone>(
    url: &AllrisUrl,
    since: DateTime<T>,
    until: Option<DateTime<T>>,
) -> Url {
    let mut url = url.url.join("oparl/papers").unwrap();

    {
        let mut query_pairs = url.query_pairs_mut();
        query_pairs.append_pair("omit_internal", "true");
        query_pairs.append_pair("modified_since", &to_rfc3339(since));
        if let Some(until) = until {
            query_pairs.append_pair("modified_until", &to_rfc3339(until));
        }
    }

    url
}

fn get_papers(
    client: reqwest::Client,
    url: Url,
) -> impl Stream<Item = Result<Paper, Error>> + Send + Sync + Unpin + 'static {
    let (tx, rx) = mpsc::channel::<Result<Vec<Paper>, Error>>(3);

    tokio::spawn(async move {
        let mut next_url = Some(url);

        while let Some(url) = next_url {
            log::info!("Retrieving {url} ...");
            match http_request::<Papers, _>(&client, &url, Response::json).await {
                Ok(content) => {
                    if tx.send(Ok(content.data)).await.is_err() {
                        return;
                    }
                    next_url = content.links.next;
                }
                Err(e) => {
                    let _ = tx.send(Err(e.into())).await;
                    return;
                }
            }
        }
    });

    ReceiverStream::new(rx)
        .map_ok(|vec| futures_util::stream::iter(vec.into_iter().map(Ok)))
        .try_flatten()
}

pub fn get_day(
    client: &reqwest::Client,
    url: &AllrisUrl,
    day: NaiveDate,
) -> impl Stream<Item = Result<Paper, Error>> + Send + Sync + Unpin + 'static {
    let start = LOCAL_TZ
        .from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .expect("no DST transition at midnight");

    let end = LOCAL_TZ
        .from_local_datetime(&day.succ_opt().unwrap().and_hms_opt(0, 0, 0).unwrap())
        .single()
        .expect("no DST transition at midnight");

    let url = endpoint_url(url, start, Some(end));

    get_papers(client.clone(), url)
        .try_filter(move |paper| ready(!paper.deleted && paper.date == Some(day)))
}

pub fn get_update(
    client: &reqwest::Client,
    url: &AllrisUrl,
    since: DateTime<Utc>,
) -> impl Stream<Item = Result<Paper, Error>> + Send + Sync + Unpin + 'static {
    // there are sometimes very old papers included. we don't want them
    let oldest_date = (since - Days::new(2)).date_naive();

    // include older changes to address possible inaccuracies
    let since = since - chrono::Duration::hours(2);
    let url = endpoint_url(url, since, None);
    get_papers(client.clone(), url)
        .try_filter(move |paper| ready(!paper.deleted && paper.date >= Some(oldest_date)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_update() {
        use chrono::Days;
        use futures_util::StreamExt;

        let url = AllrisUrl::parse("https://www.bonn.sitzung-online.de/").unwrap();
        let mut update = get_update(&reqwest::Client::new(), &url, Utc::now() - Days::new(2));

        while let Some(x) = update.next().await {
            x.unwrap();
        }
    }
}
