use chrono::{DateTime, Days, Local, NaiveDate, SecondsFormat, TimeZone, Utc};
use reqwest::Response;
use serde::{Deserialize, Serialize};
use url::Url;

use super::{AllrisUrl, Error};
use crate::allris::http_request;

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

pub async fn get_day(
    client: &reqwest::Client,
    url: &AllrisUrl,
    day: NaiveDate,
) -> Result<Vec<Paper>, Error> {
    let start = Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
        .unwrap();
    let end = Local
        .from_local_datetime(&(day + Days::new(1)).and_hms_opt(0, 0, 0).unwrap())
        .unwrap();

    let mut url = url.url.join("oparl/papers")?;
    url.query_pairs_mut()
        .append_pair(
            "modified_since",
            &start.to_rfc3339_opts(SecondsFormat::Secs, true),
        )
        .append_pair(
            "modified_until",
            &end.to_rfc3339_opts(SecondsFormat::Secs, true),
        )
        .append_pair("omit_internal", "true");

    let mut next_url = Some(url);
    let mut papers = vec![];

    while let Some(url) = next_url {
        log::info!("Retrieving {url} ...");
        let content: Papers = http_request(client, &url, Response::json).await?;
        papers.extend(
            content
                .data
                .into_iter()
                .filter(|paper| !paper.deleted && paper.date == Some(day)),
        );
        next_url = content.links.next;
    }

    Ok(papers)
}

pub async fn get_update(
    client: &reqwest::Client,
    url: &AllrisUrl,
    since: DateTime<Utc>,
) -> Result<Vec<Paper>, Error> {
    let timestamp = since.to_rfc3339_opts(SecondsFormat::Secs, true);

    // there are sometimes old papers included. we don't want them
    let oldest_date = (since - Days::new(2)).date_naive();
    let mut url = url.url.join("oparl/papers")?;
    url.query_pairs_mut()
        .append_pair("modified_since", &timestamp)
        .append_pair("omit_internal", "true");

    let mut next_url = Some(url);
    let mut papers = vec![];

    while let Some(url) = next_url {
        log::info!("Retrieving {url} ...");
        let content: Papers = http_request(client, &url, Response::json).await?;
        papers.extend(
            content
                .data
                .into_iter()
                .filter(|paper| !paper.deleted && paper.date >= Some(oldest_date)),
        );
        next_url = content.links.next;
    }

    Ok(papers)
}

#[tokio::test]
async fn test_get_update() {
    use chrono::Days;

    let url = AllrisUrl::parse("https://www.bonn.sitzung-online.de/").unwrap();
    get_update(&reqwest::Client::new(), &url, Utc::now() - Days::new(2))
        .await
        .unwrap();
}
