use std::sync::LazyLock;

use reqwest::Client;
use scraper::{ElementRef, Html, Selector};
use tokio_retry::RetryIf;
use tokio_retry::strategy::ExponentialBackoff;
use url::Url;

use super::Error;

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
    pub art: Option<String>,       //paperType
    pub verfasser: Option<String>, //
    pub unterstuetzer: Vec<String>,
    pub amt: Option<String>,
    pub beteiligt: Vec<String>,
    pub gremien: Vec<String>,
    pub sammeldokument: Option<Url>,
    pub already_discussed: bool,
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

pub async fn scrape_website(client: &Client, url: &Url) -> Result<WebsiteData, Error> {
    log::info!("Scraping website at {url}");

    let html = get_html(client, &url).await?;
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
        art: select!(document, "#voart").map(extract_text).next(),
        verfasser: select!(document, "#voverfasser1").map(extract_text).next(),
        unterstuetzer: select!(document, "#anunterstuetzer")
            .next()
            .map(|el| el.text().map(|x| x.trim().to_string()).collect())
            .unwrap_or_default(),
        amt: select!(document, "#vofamt").map(extract_text).next(),
        beteiligt,
        gremien,
        sammeldokument,
        already_discussed,
    })
}
