use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, PartialEq, Eq)]
pub struct RemoteEmbed {
    pub url: String,
    pub model: String,
    pub token: String,
}

impl std::fmt::Debug for RemoteEmbed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteEmbed")
            .field("url", &self.url)
            .field("model", &self.model)
            .field("token", &"<redacted>")
            .finish()
    }
}

pub const URL_VAR: &str = "BR8N_EMBED_URL";
pub const MODEL_VAR: &str = "BR8N_EMBED_MODEL";
pub const TOKEN_VAR: &str = "BR8N_EMBED_TOKEN";

pub fn read_lenient(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .map(|text| parse(&text))
        .unwrap_or_default()
}

fn entry_key(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.starts_with('#') {
        return None;
    }
    line.split_once('=').map(|(k, _)| k.trim())
}

pub fn write_entries(path: &Path, changes: &[(&str, Option<String>)]) -> Result<()> {
    if let Some((key, _)) = changes
        .iter()
        .find(|(_, v)| v.as_deref().is_some_and(|v| v.contains(['\n', '\r'])))
    {
        bail!("{key} must be a single line");
    }
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut written = std::collections::BTreeSet::new();
    let mut out = String::new();
    for line in text.lines() {
        let change = entry_key(line).and_then(|k| changes.iter().find(|(c, _)| *c == k));
        match change {
            None => {
                out.push_str(line);
                out.push('\n');
            }
            Some((key, Some(value))) if written.insert(*key) => {
                out.push_str(&format!("{key}={value}\n"));
            }
            Some(_) => {}
        }
    }
    for (key, value) in changes {
        if let Some(value) = value {
            if written.insert(*key) {
                out.push_str(&format!("{key}={value}\n"));
            }
        }
    }
    crate::config::edit::replace_file(path, out.as_bytes(), Some(0o600))
}

pub fn env_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name("env")
}

pub fn parse(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        out.insert(k.trim().to_string(), v.trim().to_string());
    }
    out
}

pub fn read_map(path: &Path) -> Result<BTreeMap<String, String>> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        bail!(
            "{} is mode {:04o}; it holds a token, so br8n refuses to read it. \
             Run: chmod 0600 {}",
            path.display(),
            mode,
            path.display()
        );
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(parse(&text))
}

pub fn resolve(config_path: &Path) -> Result<Option<RemoteEmbed>> {
    let path = env_path(config_path);
    let file = if path.exists() {
        read_map(&path)?
    } else {
        BTreeMap::new()
    };
    let pick = |key: &str| -> Option<String> {
        std::env::var(key)
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| file.get(key).cloned().filter(|v| !v.is_empty()))
    };

    let Some(url) = pick("BR8N_EMBED_URL") else {
        return Ok(None);
    };
    let model = pick("BR8N_EMBED_MODEL").with_context(|| {
        format!(
            "BR8N_EMBED_URL is set to {url} but BR8N_EMBED_MODEL is not; \
             set it in {}",
            path.display()
        )
    })?;
    let token = pick("BR8N_EMBED_TOKEN").with_context(|| {
        format!(
            "BR8N_EMBED_URL is set to {url} but BR8N_EMBED_TOKEN is not; \
             set it in {}",
            path.display()
        )
    })?;
    Ok(Some(RemoteEmbed {
        url: url.trim_end_matches('/').to_string(),
        model,
        token,
    }))
}
