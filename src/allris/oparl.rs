use std::future::ready;
use std::sync::LazyLock;

use chrono::{DateTime, Days, Duration, NaiveDate, SecondsFormat, TimeZone, Utc};
use futures_util::{Stream, TryStreamExt};
use reqwest::Response;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use url::Url;

use super::{AllrisUrl, Error};
use crate::allris::http_request;
use crate::lru_cache::{Cache, Lru};

type LruCache<K, V> = Cache<K, V, Lru<K>>;

static ORGANIZATIONS: LazyLock<LruCache<Url, (DateTime<Utc>, Organization)>> =
    LazyLock::new(|| Cache::new(Lru::new(50)));

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Organization {
    pub id: Url,
    pub name: Option<String>,
    pub web: Option<Url>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Consultation {
    pub role: Option<String>,
    pub authoritative: Option<bool>,
    #[serde(default)]
    pub organization: Vec<Url>,
    pub agenda_item: Option<Url>,
}

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
    #[serde(default)]
    pub consultation: Vec<Consultation>,
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

pub async fn get_organization(client: &reqwest::Client, id: &Url) -> Result<Organization, Error> {
    ORGANIZATIONS
        .get_if_valid(
            id.clone(),
            |(t, _)| Utc::now() - t < Duration::days(3),
            async || {
                let r: Organization = http_request(client, id, Response::json).await?;
                Ok((Utc::now(), r))
            },
        )
        .await
        .map(|x| x.1.clone())
}

fn get_papers(
    client: reqwest::Client,
    url: Url,
) -> impl Stream<Item = Result<Paper, Error>> + Send + Sync + Unpin + 'static {
    let (tx, rx) = mpsc::channel::<Result<Vec<Paper>, Error>>(3);

    tokio::spawn(async move {
        let mut next_url = Some(url);

        while let Some(url) = next_url {
            match http_request::<Papers>(&client, &url, Response::json).await {
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
