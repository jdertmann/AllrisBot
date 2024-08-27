use chrono::prelude::*;
use reqwest::Client;
use serde::{Deserialize, Deserializer};
use thiserror::Error;

#[derive(Deserialize, Debug)]
pub(crate) struct Rss {
    pub(crate) channel: Channel,
}

pub(crate) fn deserialize_rfc2822<'de, D>(
    deserializer: D,
) -> Result<DateTime<FixedOffset>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    DateTime::parse_from_rfc2822(&s).map_err(serde::de::Error::custom)
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    #[serde(deserialize_with = "deserialize_rfc2822")]
    pub pub_date: DateTime<FixedOffset>,
    pub item: Vec<Item>,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to retrieve feed: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("invalid feed format: {0}")]
    ParseError(#[from] serde_xml_rs::Error),
}

#[derive(Deserialize, Debug)]
pub struct Item {
    pub title: String,
    pub link: String,
    pub description: String,
    pub guid: String,
}

pub async fn fetch_feed(client: &Client, url: &str) -> Result<Channel, Error> {
    let response = client.get(url).send().await?.text().await?;
    let rss: Rss = serde_xml_rs::from_str(&response)?;
    Ok(rss.channel)
}
