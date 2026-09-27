use super::check::check;
use super::edit::{etag_of, read_document};
use super::Config;
use crate::env_file::{self, MODEL_VAR, TOKEN_VAR, URL_VAR};
use anyhow::Result;
use serde_json::{json, Value};
use std::path::Path;

const SECRET_NAMES: [&str; 8] = [
    "token",
    "password",
    "secret",
    "api_key",
    "apikey",
    "access_key",
    "secret_key",
    "credentials",
];
const SECRET_SUFFIXES: [&str; 4] = ["_token", "_password", "_secret", "_key"];

pub fn looks_secret(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    SECRET_NAMES.contains(&key.as_str()) || SECRET_SUFFIXES.iter().any(|s| key.ends_with(s))
}

pub fn strip_secrets(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|key, _| !looks_secret(key));
            map.values_mut().for_each(strip_secrets);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_secrets),
        _ => {}
    }
}

pub fn config_json(cfg: &Config) -> Result<Value> {
    let mut value = serde_json::to_value(cfg)?;
    strip_secrets(&mut value);
    shorten_single_precision_floats(&mut value);
    Ok(value)
}

fn shorten_single_precision_floats(value: &mut Value) {
    match value {
        Value::Number(n) if n.is_f64() => {
            let shortest = n
                .as_f64()
                .and_then(|f| (f as f32).to_string().parse::<f64>().ok())
                .and_then(serde_json::Number::from_f64);
            if let Some(shortest) = shortest {
                *n = shortest;
            }
        }
        Value::Object(map) => map.values_mut().for_each(shorten_single_precision_floats),
        Value::Array(items) => items.iter_mut().for_each(shorten_single_precision_floats),
        _ => {}
    }
}

pub fn without_nulls(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), without_nulls(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(without_nulls).collect()),
        other => other.clone(),
    }
}

fn from_process_env(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|v| !v.is_empty())
}

fn set_or_unset(present: bool) -> &'static str {
    if present {
        "set"
    } else {
        "unset"
    }
}

pub fn view(config_path: &Path) -> Result<Value> {
    let text = read_document(config_path)?;
    let cfg = Config::load_from(config_path);
    let effective = config_json(&cfg)?;
    let mut file = match text.as_deref() {
        None => json!({}),
        Some(t) => toml::from_str::<toml::Value>(t)
            .ok()
            .and_then(|v| serde_json::to_value(v).ok())
            .unwrap_or(Value::Null),
    };
    strip_secrets(&mut file);
    let defaults = config_json(&Config::default())?;

    let env_file_values = env_file::read_lenient(&env_file::env_path(config_path));
    let pick = |var: &str| {
        from_process_env(var)
            .or_else(|| env_file_values.get(var).cloned().filter(|v| !v.is_empty()))
    };
    let overrides: Vec<Value> = [
        (URL_VAR, "endpoint.url"),
        (MODEL_VAR, "endpoint.model"),
        (TOKEN_VAR, "embed.token"),
    ]
    .into_iter()
    .filter(|(var, _)| from_process_env(var).is_some())
    .map(|(variable, setting)| json!({ "variable": variable, "setting": setting }))
    .collect();

    Ok(json!({
        "path": config_path.display().to_string(),
        "exists": text.is_some(),
        "etag": etag_of(text.as_deref()),
        "effective": effective,
        "file": file,
        "defaults": defaults,
        "errors": text.as_deref().map(check).unwrap_or_default(),
        "secrets": {
            "embed.token": set_or_unset(pick(TOKEN_VAR).is_some()),
            "backup.key": set_or_unset(cfg.backup_key_path().exists()),
            "backup.drive_token": set_or_unset(cfg.drive_token_path().exists()),
        },
        "env_overrides": overrides,
        "endpoint": {
            "backend": if cfg.embed.remote.is_some() { "remote" } else { "ollama" },
            "url": pick(URL_VAR),
            "model": pick(MODEL_VAR),
            "error": cfg.embed.remote_error,
        },
    }))
}
