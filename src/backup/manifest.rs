use crate::backup::crypto::Key;
use crate::config::Config;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

pub const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub hash: String,
    pub size: u64,
    pub mode: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    pub hash: String,
    pub size: u64,
    pub embed_model: String,
    pub dimensions: usize,
    pub chunk_tokens: usize,
    pub documents: usize,
    pub chunks: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Encryption {
    pub algo: String,
    pub key_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub created_at: String,
    pub host: String,
    pub br8n_version: String,
    #[serde(default)]
    pub encryption: Option<Encryption>,
    #[serde(default)]
    pub files: Vec<FileEntry>,
    #[serde(default)]
    pub index: Option<IndexEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatestPointer {
    pub generation: String,
}

pub fn blob_key(key_id: Option<&str>, hash: &str) -> String {
    match key_id {
        Some(id) => format!("blobs/{id}/{hash}"),
        None => format!("blobs/{hash}"),
    }
}

pub fn index_key(key_id: Option<&str>, hash: &str) -> String {
    match key_id {
        Some(id) => format!("index/{id}/{hash}.tar.gz"),
        None => format!("index/{hash}.tar.gz"),
    }
}

impl Manifest {
    pub fn new(key: Option<&Key>) -> Manifest {
        Manifest {
            version: MANIFEST_VERSION,
            created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            host: hostname(),
            br8n_version: env!("CARGO_PKG_VERSION").to_string(),
            encryption: key.map(|k| Encryption {
                algo: "xchacha20poly1305".into(),
                key_id: k.id(),
            }),
            files: Vec::new(),
            index: None,
        }
    }

    pub fn generation_key(&self) -> String {
        format!("manifest/{}.json", self.created_at.replace(':', "-"))
    }

    pub fn latest_key() -> &'static str {
        "manifest/latest.json"
    }

    pub fn check_key(&self, key: Option<&Key>) -> Result<()> {
        if self.version > MANIFEST_VERSION {
            return Err(anyhow!(
                "this backup was written by a newer `br8n` (manifest version {}, this build understands {MANIFEST_VERSION}). Upgrade `br8n` and try again.",
                self.version
            ));
        }
        match (&self.encryption, key) {
            (None, _) => Ok(()),
            (Some(_), None) => Err(anyhow!(
                "this backup is encrypted, but no key is configured. Point `key_file` at the key printed by `br8n backup init`."
            )),
            (Some(e), Some(k)) if e.key_id != k.id() => Err(anyhow!(
                "this backup was made with a different key (backup key id {}, configured key id {}). Restoring would produce nothing but decryption failures.",
                e.key_id,
                k.id()
            )),
            (Some(_), Some(_)) => Ok(()),
        }
    }

    pub fn check_index_compat(&self, cfg: &Config) -> Result<()> {
        let Some(ix) = &self.index else {
            return Ok(());
        };
        let mismatch = |what: &str, backup: String, current: String| {
            anyhow!(
                "this backup's index was built with {what} {backup}, but the current config uses {current}. Embeddings from different settings are not comparable. Restore without `--index` and rebuild with `br8n index --reindex`."
            )
        };
        if ix.embed_model != cfg.embed.model {
            return Err(mismatch(
                "embedding model",
                ix.embed_model.clone(),
                cfg.embed.model.clone(),
            ));
        }
        if ix.dimensions != cfg.embed.dimensions {
            return Err(mismatch(
                "dimensions",
                ix.dimensions.to_string(),
                cfg.embed.dimensions.to_string(),
            ));
        }
        if ix.chunk_tokens != cfg.embed.chunk_tokens {
            return Err(mismatch(
                "chunk size",
                ix.chunk_tokens.to_string(),
                cfg.embed.chunk_tokens.to_string(),
            ));
        }
        Ok(())
    }
}

fn hostname() -> String {
    if let Ok(h) = std::env::var("HOSTNAME") {
        if !h.is_empty() {
            return h;
        }
    }
    std::process::Command::new("uname")
        .arg("-n")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}
