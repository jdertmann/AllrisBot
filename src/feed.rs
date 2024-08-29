use chrono::prelude::*;
use serde::{Deserialize, Deserializer};

#[derive(Deserialize, Debug)]
struct Rss {
    channel: Channel,
}

fn deserialize_rfc2822<'de, D>(deserializer: D) -> Result<DateTime<FixedOffset>, D::Error>
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
    #[serde(default)]
    pub item: Vec<Item>,
}

#[derive(Deserialize, Debug)]
pub struct Item {
    pub title: String,
    pub link: String,
    pub description: String,
    pub guid: String,
}

pub async fn fetch_feed(url: &str) -> Result<Channel, crate::Error> {
    let response = reqwest::get(url).await?.text().await?;
    let rss: Rss = serde_xml_rs::from_str(&response)?;
    Ok(rss.channel)
}
