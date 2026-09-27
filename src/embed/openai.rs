use super::ollama::PrefixScheme;
use super::{decode_vectors, Embedder};
use crate::config::EmbedConfig;
use crate::env_file::RemoteEmbed;
use anyhow::{Context, Result};

pub struct OpenAiEmbedder {
    url: String,
    wire_model: String,
    stamped_model: String,
    token: String,
    scheme: PrefixScheme,
    dims: usize,
    batch: usize,
    client: reqwest::blocking::Client,
    query_client: reqwest::blocking::Client,
}

impl OpenAiEmbedder {
    pub fn new(cfg: &EmbedConfig, remote: &RemoteEmbed) -> Self {
        Self {
            url: remote.url.trim_end_matches('/').to_string(),
            wire_model: remote.model.clone(),
            stamped_model: cfg.model.clone(),
            token: remote.token.clone(),
            scheme: cfg
                .prefix_scheme
                .as_deref()
                .and_then(PrefixScheme::parse)
                .unwrap_or_else(|| PrefixScheme::for_model(&cfg.model)),
            dims: cfg.dimensions,
            batch: cfg.batch.max(1),
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("build http client"),
            query_client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_millis(4000))
                .build()
                .expect("build http client"),
        }
    }

    fn call_with(
        &self,
        client: &reqwest::blocking::Client,
        inputs: Vec<String>,
    ) -> Result<Vec<Vec<f32>>> {
        let expected = inputs.len();
        let body = serde_json::json!({ "model": self.wire_model, "input": inputs });
        let resp = client
            .post(format!("{}/v1/embeddings", self.url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .map_err(|e| {
                let why = if e.is_timeout() {
                    format!(
                        "{} did not answer in time — it may be busy or still loading its model",
                        self.url
                    )
                } else if e.is_builder() {
                    "BR8N_EMBED_TOKEN is not a valid header value — check it for a stray \
                     newline or control character"
                        .to_string()
                } else {
                    format!("{} is unreachable", self.url)
                };
                anyhow::Error::new(e).context(why)
            })?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            anyhow::bail!(
                "{} rejected the token from BR8N_EMBED_TOKEN ({status})",
                self.url
            );
        }
        if status == reqwest::StatusCode::BAD_REQUEST || status == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!(
                "{} rejected model `{}` ({status}) — check BR8N_EMBED_MODEL against the \
                 server's model list",
                self.url,
                self.wire_model
            );
        }
        anyhow::ensure!(
            status.is_success(),
            "{} answered {status} to /v1/embeddings",
            self.url
        );
        let resp: serde_json::Value = resp.json().with_context(|| {
            format!(
                "{} answered /v1/embeddings with something that is not JSON",
                self.url
            )
        })?;

        let items = resp["data"].as_array().context("response missing `data`")?;
        let mut rows: Vec<(usize, Vec<f32>)> = items
            .iter()
            .enumerate()
            .map(|(pos, item)| {
                let idx = item["index"].as_u64().map(|i| i as usize).unwrap_or(pos);
                let v = item["embedding"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_f64())
                            .map(|x| x as f32)
                            .collect()
                    })
                    .unwrap_or_default();
                (idx, v)
            })
            .collect();
        rows.sort_by_key(|(i, _)| *i);
        let indices: Vec<usize> = rows.iter().map(|(i, _)| *i).collect();
        anyhow::ensure!(
            indices.iter().enumerate().all(|(want, got)| want == *got),
            "the endpoint returned indices {indices:?} for {expected} inputs; \
             refusing to pair vectors with chunks by guesswork"
        );
        decode_vectors(
            rows.into_iter().map(|(_, v)| v).collect(),
            expected,
            self.dims,
        )
    }
}

impl Embedder for OpenAiEmbedder {
    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(self.batch) {
            out.extend(self.call_with(
                &self.client,
                batch.iter().map(|t| self.scheme.doc(t)).collect(),
            )?);
        }
        Ok(out)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.call_with(&self.query_client, vec![self.scheme.query(text)])?
            .pop()
            .context("empty embedding response")
    }

    fn warm(&self) -> Result<()> {
        let _ = self.call_with(&self.query_client, vec!["warm".to_string()]);
        Ok(())
    }

    fn model_id(&self) -> String {
        // The wire name is deliberately absent. This endpoint and local Ollama
        // serve the same weights: measured 2026-09-17, cosine 0.999647 /
        // 0.999634 / 0.999779 over three texts at 512 dims, against 0.15-0.83
        // for unrelated texts. Stamping the wire name would make check_model
        // demand a rebuild of an index whose vectors are already correct.
        format!("{}@{}+{}", self.stamped_model, self.dims, self.scheme.tag())
    }

    fn dimensions(&self) -> usize {
        self.dims
    }
}
