use super::Embedder;
use crate::config::EmbedConfig;
use anyhow::{Context, Result};

/// How a model wants an asymmetric document/query pair marked.
///
/// This is per-model and NOT cosmetic. `search_document:` / `search_query:` is
/// Nomic's format; qwen3-embedding was never trained on it, so prefixing it
/// there is off-distribution text prepended to every vector in the index — and
/// the 0.70 threshold was calibrated on top of that. Qwen3 uses a bare document
/// and an instruction-wrapped query; E5 uses `passage:` / `query:`.
///
/// Changing a model's scheme invalidates an existing index, exactly like
/// changing the model does. That is safe because `model_id` is stamped into the
/// index and checked on open, and the scheme is folded into it below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixScheme {
    /// Bare passage; query wrapped in a one-line instruction. Qwen3-embedding.
    Qwen3Instruct,
    /// `search_document:` / `search_query:`. Nomic-embed-text.
    Nomic,
    /// `passage:` / `query:`. E5 and its derivatives.
    E5,
    /// No prefixes at all — symmetric models, and the safe default for a model
    /// we do not recognise. Guessing wrong is worse than not guessing.
    None,
}

impl PrefixScheme {
    /// Pick by model name. Matches on the family, so a tag or a registry
    /// namespace (`hf.co/...`, `:q4_K_M`) does not defeat it.
    /// Parse an explicit override; `None` if unrecognised, so a typo falls
    /// back to model detection rather than silently selecting `plain`.
    pub fn parse(name: &str) -> Option<PrefixScheme> {
        match name.to_ascii_lowercase().as_str() {
            "qwen3" | "qwen3instruct" => Some(PrefixScheme::Qwen3Instruct),
            "nomic" => Some(PrefixScheme::Nomic),
            "e5" => Some(PrefixScheme::E5),
            "plain" | "none" => Some(PrefixScheme::None),
            _ => None,
        }
    }

    pub fn for_model(model: &str) -> PrefixScheme {
        let m = model.to_ascii_lowercase();
        if m.contains("qwen3-embedding") || m.contains("qwen3_embedding") {
            PrefixScheme::Qwen3Instruct
        } else if m.contains("nomic-embed") {
            PrefixScheme::Nomic
        } else if m.contains("e5-") || m.contains("multilingual-e5") {
            PrefixScheme::E5
        } else {
            PrefixScheme::None
        }
    }

    /// Short, stable tag folded into `model_id` so switching schemes forces a
    /// rebuild rather than silently mixing two embedding spaces.
    pub fn tag(&self) -> &'static str {
        match self {
            PrefixScheme::Qwen3Instruct => "qwen3",
            PrefixScheme::Nomic => "nomic",
            PrefixScheme::E5 => "e5",
            PrefixScheme::None => "plain",
        }
    }

    pub fn doc(&self, s: &str) -> String {
        match self {
            // Qwen3-embedding takes the passage bare; only the query is wrapped.
            PrefixScheme::Qwen3Instruct | PrefixScheme::None => s.to_string(),
            PrefixScheme::Nomic => format!("search_document: {s}"),
            PrefixScheme::E5 => format!("passage: {s}"),
        }
    }

    pub fn query(&self, s: &str) -> String {
        match self {
            PrefixScheme::Qwen3Instruct => format!(
                "Instruct: Given a search query, retrieve relevant passages that answer it\nQuery: {s}"
            ),
            PrefixScheme::Nomic => format!("search_query: {s}"),
            PrefixScheme::E5 => format!("query: {s}"),
            PrefixScheme::None => s.to_string(),
        }
    }
}

fn embed_request_body(
    model: &str,
    inputs: &[String],
    keep_alive: &crate::config::KeepAlive,
) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "input": inputs,
        "keep_alive": keep_alive,
    })
}

pub struct OllamaEmbedder {
    url: String,
    model: String,
    scheme: PrefixScheme,
    dims: usize,
    batch: usize,
    keep_alive: crate::config::KeepAlive,
    client: reqwest::blocking::Client,
    query_client: reqwest::blocking::Client,
}

impl OllamaEmbedder {
    pub fn new(cfg: &EmbedConfig) -> Self {
        Self {
            url: cfg.ollama_url.trim_end_matches('/').to_string(),
            scheme: cfg
                .prefix_scheme
                .as_deref()
                .and_then(PrefixScheme::parse)
                .unwrap_or_else(|| PrefixScheme::for_model(&cfg.model)),
            model: cfg.model.clone(),
            dims: cfg.dimensions,
            batch: cfg.batch.max(1),
            keep_alive: cfg.keep_alive.clone(),
            // Indexing may legitimately take minutes.
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("build http client"),
            // Query path: a stalled embed must never block the user's prompt.
            query_client: reqwest::blocking::Client::builder()
                // 4000, not 1500. With the query-priority marker the wait is
                // bounded by ONE in-flight bulk batch (a few seconds), not the
                // whole queue — 1500ms lost the race to even a single batch.
                // The hook's own process-level timeout still caps the prompt
                // stall at 5s.
                .timeout(std::time::Duration::from_millis(4000))
                .build()
                .expect("build http client"),
        }
    }

    fn call(&self, inputs: Vec<String>) -> Result<Vec<Vec<f32>>> {
        self.call_with(&self.client, inputs)
    }

    fn call_with(
        &self,
        client: &reqwest::blocking::Client,
        inputs: Vec<String>,
    ) -> Result<Vec<Vec<f32>>> {
        let expected = inputs.len();
        // keep_alive pins the model in memory: 2000ms cold vs 24ms warm (Spike 2).
        let body = embed_request_body(&self.model, &inputs, &self.keep_alive);
        let resp: serde_json::Value = client
            .post(format!("{}/api/embed", self.url))
            .json(&body)
            .send()
            .map_err(|e| {
                let why = if e.is_timeout() {
                    "ollama did not answer in time — it serves one request at a time, so it may \
                     be busy with indexing or still loading the model"
                } else {
                    "ollama unreachable — is `ollama serve` running?"
                };
                anyhow::Error::new(e).context(why)
            })?
            .error_for_status()
            .with_context(|| {
                format!(
                    "ollama rejected model `{}` — try `ollama pull {}`",
                    self.model, self.model
                )
            })?
            .json()?;

        let raw = resp["embeddings"]
            .as_array()
            .context("ollama response missing `embeddings`")?
            .iter()
            .map(|v| {
                v.as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.as_f64())
                            .map(|x| x as f32)
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .collect::<Vec<Vec<f32>>>();
        super::decode_vectors(raw, expected, self.dims)
    }
}

impl Embedder for OllamaEmbedder {
    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        // Batch to keep request bodies and Ollama memory bounded on large corpora.
        for batch in texts.chunks(self.batch) {
            out.extend(self.call(batch.iter().map(|t| self.scheme.doc(t)).collect())?);
        }
        Ok(out)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.call_with(&self.query_client, vec![self.scheme.query(text)])?
            .pop()
            .context("empty embedding response")
    }

    fn warm(&self) -> Result<()> {
        // `query_client` (4s), NOT `client` (120s), and the distinction is a
        // crash fix rather than a preference. Warming runs on the SessionStart
        // path, which is latency-bound like the prompt path and nothing like
        // indexing — a warm that hangs for two minutes against an
        // unreachable-but-not-refusing Ollama would stall session startup, the
        // failure `run_session_start` says is worse than the one it solves.
        // The 120s budget belongs to bulk embedding, which legitimately takes
        // minutes; a single warm token never does.
        let _ = self.call_with(&self.query_client, vec!["warm".to_string()]);
        Ok(())
    }

    fn model_id(&self) -> String {
        // Deliberately excludes the host: the URL does not change the vectors
        // a given model produces, and including it would force a full re-index
        // on a purely cosmetic config edit (e.g., localhost → 127.0.0.1). The
        // residual risk — two hosts serving different weights under one tag
        // name would go undetected — is accepted because Ollama tags are
        // content-addressed by digest, making this scenario hard to create.
        //
        // The prefix scheme IS included. It changes the text that gets embedded,
        // so vectors written under one scheme are not comparable with vectors
        // written under another; folding it in makes `check_model` catch the
        // change and demand a rebuild, exactly as it does for the model itself.
        format!("{}@{}+{}", self.model, self.dims, self.scheme.tag())
    }

    fn dimensions(&self) -> usize {
        self.dims
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_keep_alive_is_thirty_minutes() {
        let cfg: EmbedConfig = toml::from_str("model = \"m\"\ndimensions = 4").unwrap();
        let e = OllamaEmbedder::new(&cfg);
        let body = embed_request_body(&e.model, &["x".to_string()], &e.keep_alive);
        assert_eq!(body["keep_alive"], "30m");
    }

    #[test]
    fn a_configured_keep_alive_reaches_the_request_body() {
        let cfg: EmbedConfig =
            toml::from_str("model = \"m\"\ndimensions = 4\nkeep_alive = \"5m\"").unwrap();
        let e = OllamaEmbedder::new(&cfg);
        let body = embed_request_body(&e.model, &["x".to_string()], &e.keep_alive);
        assert_eq!(body["keep_alive"], "5m");
    }

    #[test]
    fn an_integer_keep_alive_reaches_the_request_body_as_a_number() {
        let cfg: EmbedConfig =
            toml::from_str("model = \"m\"\ndimensions = 4\nkeep_alive = 300").unwrap();
        let e = OllamaEmbedder::new(&cfg);
        let body = embed_request_body(&e.model, &["x".to_string()], &e.keep_alive);
        assert_eq!(body["keep_alive"], serde_json::json!(300));

        let forever: EmbedConfig =
            toml::from_str("model = \"m\"\ndimensions = 4\nkeep_alive = -1").unwrap();
        let body = embed_request_body(&e.model, &["x".to_string()], &forever.keep_alive);
        assert_eq!(body["keep_alive"], serde_json::json!(-1));
    }
}
