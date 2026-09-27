use crate::config::{EmbedConfig, KeepAlive};
use crate::model::{Chunk, Document};
use anyhow::Result;

pub fn build_prompt(title: &str, doc_excerpt: &str, chunk: &str) -> String {
    format!(
        "Document: {title}\n\n<document>\n{doc_excerpt}\n</document>\n\n\
         Here is a chunk from that document:\n<chunk>\n{chunk}\n</chunk>\n\n\
         Write one or two short sentences that situate this chunk within the \
         document, so it can be found by search. Answer with the sentences only.",
    )
}

pub struct Enricher {
    url: String,
    model: String,
    keep_alive: KeepAlive,
    // `None` when `cfg.contextual` is false, and absence of the client IS the
    // disabled state (no separate `enabled` flag to drift out of sync).
    // Building a `reqwest::Client` a disabled feature will never use is
    // wasted work, and this also removes the `.expect()` panic path from a
    // constructor that a disabled feature should never be able to trip.
    client: Option<reqwest::blocking::Client>,
}

pub struct GenerateOpts {
    pub num_predict: u32,
    pub num_ctx: Option<u32>,
    pub keep_alive: KeepAlive,
    pub temperature: f32,
}

pub fn request_body(model: &str, prompt: &str, opts: &GenerateOpts) -> serde_json::Value {
    let mut options = serde_json::json!({
        "num_predict": opts.num_predict,
        "temperature": opts.temperature,
    });
    if let Some(ctx) = opts.num_ctx {
        options["num_ctx"] = serde_json::json!(ctx);
    }
    serde_json::json!({
        "model": model,
        "prompt": prompt,
        "stream": false,
        "keep_alive": opts.keep_alive,
        // Without this the thinking block consumes the whole 80-token budget and
        // the response comes back empty, so `enrich` takes its "blurb was blank"
        // path and contextual enrichment silently does nothing. Measured: empty
        // with thinking on, real text with it off.
        "think": false,
        "options": options,
    })
}

pub fn generate(
    client: &reqwest::blocking::Client,
    url: &str,
    model: &str,
    prompt: &str,
    opts: &GenerateOpts,
) -> Result<String> {
    let resp: serde_json::Value = client
        .post(format!("{}/api/generate", url.trim_end_matches('/')))
        .json(&request_body(model, prompt, opts))
        .send()?
        .error_for_status()?
        .json()?;
    Ok(resp["response"].as_str().unwrap_or_default().to_string())
}

impl Enricher {
    pub fn new(cfg: &EmbedConfig) -> Self {
        Self {
            url: cfg.ollama_url.trim_end_matches('/').to_string(),
            model: cfg.enrich_model.clone(),
            keep_alive: cfg.keep_alive.clone(),
            client: cfg.contextual.then(|| {
                reqwest::blocking::Client::builder()
                    .timeout(std::time::Duration::from_secs(60))
                    .build()
                    .expect("build http client")
            }),
        }
    }

    pub fn enrich(&self, doc: &Document, chunks: &mut [Chunk]) -> Result<()> {
        let Some(client) = self.client.as_ref() else {
            return Ok(());
        };
        // Cap the document excerpt so the prompt stays small on long PDFs.
        let excerpt: String = doc.text.chars().take(4000).collect();
        let opts = GenerateOpts {
            num_predict: 80,
            num_ctx: None,
            // Enrichment scores every chunk in a document, so without this the
            // model can unload between chunks and reload repeatedly.
            keep_alive: self.keep_alive.clone(),
            temperature: 0.0,
        };
        for c in chunks.iter_mut() {
            let prompt = build_prompt(&doc.title, &excerpt, &c.text);
            match generate(client, &self.url, &self.model, &prompt, &opts) {
                Ok(b) if !b.trim().is_empty() => {
                    c.embed_text = format!("{}\n\n{}", b.trim(), c.embed_text);
                }
                // A failed blurb costs quality, never correctness — keep indexing.
                _ => continue,
            }
        }
        Ok(())
    }
}
