use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use url::Url;

use super::{AllrisUrl, Error};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Consultation {
    pub role: String,
    pub authoritative: bool,
    pub organization: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Paper {
    pub id: Url,
    pub name: Option<String>,
    pub reference: Option<String>,
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

pub async fn get_update(
    client: &reqwest::Client,
    url: &AllrisUrl,
    since: DateTime<Utc>,
) -> Result<Vec<Paper>, Error> {
    let timestamp = since.to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut url = url.url.join("oparl/papers")?;
    url.query_pairs_mut()
        .append_pair("modified_since", &timestamp)
        .append_pair("omit_internal", "true");

    let mut next_url = Some(url);
    let mut papers = vec![];

    while let Some(url) = next_url {
        log::info!("Retrieving {url} ...");
        let content: Papers = client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        papers.extend(content.data.into_iter().filter(|paper| !paper.deleted));
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
