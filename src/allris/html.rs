use std::sync::LazyLock;

use reqwest::{Client, Response};
use scraper::{ElementRef, Html, Selector};
use url::Url;

use super::Error;
use crate::allris::http_request;

macro_rules! select {
    ($document:expr, $selector:literal) => {{
        static SELECTOR: LazyLock<Selector> = LazyLock::new(|| Selector::parse($selector).unwrap());
        $document.select(&SELECTOR)
    }};
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
pub struct WebsiteData {
    pub verfasser: Option<String>,
    pub unterstuetzer: Vec<String>,
    pub amt: Option<String>,
    pub beteiligt: Vec<String>,
    pub gremien: Vec<(String, Option<Url>, bool)>,
    pub already_discussed: bool,
}

/// extracts relevant information from a document's web page.
pub async fn scrape_website(client: &Client, url: &Url) -> Result<WebsiteData, Error> {
    let html = http_request(client, url, Response::text).await?;
    let document = Html::parse_document(&html);

    let gremien: Vec<_> = select!(
        document,
        "#bfTable > table > tbody > tr > td:not(.date) + td:nth-of-type(3)"
    )
    .map(extract_text)
    .filter(|s| !s.is_empty())
    .map(|x| (x, None, false))
    .collect();

    let beteiligt = select!(document, "#vobamt")
        .next()
        .map(|el| {
            extract_text(el)
                .split(";")
                .map(str::trim)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();

    let already_discussed = select!(document, "#bfTable .toLink a").next().is_some();

    Ok(WebsiteData {
        verfasser: select!(document, "#voverfasser1").map(extract_text).next(),
        unterstuetzer: select!(document, "#anunterstuetzer")
            .next()
            .map(|el| el.text().map(str::trim).map(str::to_owned).collect())
            .unwrap_or_default(),
        amt: select!(document, "#vofamt").map(extract_text).next(),
        beteiligt,
        gremien,
        already_discussed,
    })
}
