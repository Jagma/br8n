use crate::model::{Document, SourceType};
use anyhow::{bail, Result};
use std::io::Read as _;

/// Hard ceiling on how many bytes of a fetched page body we will read into
/// memory. 8 MiB is generous for long-form article HTML (which is what this
/// loader targets) while still bounding worst-case memory use from a very
/// large or slow-drip response within the 20s request timeout — the timeout
/// alone bounds wall-clock time but not bytes.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

pub struct WebLoader;

impl WebLoader {
    /// Pure: no network, so the extraction logic is unit-testable.
    pub fn from_html(url: &str, html: &str) -> Result<Document> {
        let mut readability = dom_smoothie::Readability::new(html, Some(url), None)?;
        let article = readability.parse()?;

        let markdown = htmd::convert(&article.content)?;
        let title = if article.title.trim().is_empty() {
            url.to_string()
        } else {
            article.title.to_string()
        };

        let domain = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_string()))
            .unwrap_or_default();

        let mut doc = Document::new(SourceType::Web, url, &title, &markdown);
        doc.meta = serde_json::json!({ "domain": domain });
        Ok(doc)
    }

    pub fn fetch(url: &str) -> Result<Document> {
        let mut response = reqwest::blocking::Client::builder()
            .user_agent(concat!(
                "br8n/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com/Jagma/br8n)"
            ))
            .timeout(std::time::Duration::from_secs(20))
            .build()?
            .get(url)
            .send()?
            .error_for_status()?;

        // A server can declare a Content-Length that's already over the cap;
        // reject early rather than starting the read. It can also lie about
        // or omit this header, so the read below enforces the cap regardless.
        if let Some(len) = response.content_length() {
            if len > MAX_BODY_BYTES as u64 {
                bail!(
                    "response body for {url} is {len} bytes, exceeding the {MAX_BODY_BYTES} byte limit"
                );
            }
        }

        let mut body = Vec::new();
        response
            .by_ref()
            .take(MAX_BODY_BYTES as u64 + 1)
            .read_to_end(&mut body)?;
        if body.len() > MAX_BODY_BYTES {
            bail!("response body for {url} exceeds the {MAX_BODY_BYTES} byte limit");
        }

        // Decode lossily: a mis-declared charset should degrade gracefully
        // rather than fail the whole fetch.
        let html = String::from_utf8_lossy(&body).into_owned();
        Self::from_html(url, &html)
    }
}

impl super::Loader for WebLoader {
    fn load(&self, uri: &str) -> Result<Vec<Document>> {
        Ok(vec![Self::fetch(uri)?])
    }
}
