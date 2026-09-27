use anyhow::{bail, Context, Result};

#[derive(Debug)]
pub enum OllamaState {
    Unreachable(String),
    Reachable { missing: Vec<String> },
}

pub fn probe(url: &str, models: &[String]) -> OllamaState {
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(e) => return OllamaState::Unreachable(e.to_string()),
    };
    let tags = format!("{}/api/tags", url.trim_end_matches('/'));
    let v: serde_json::Value = match client.get(&tags).send().and_then(|r| r.error_for_status()) {
        Ok(r) => match r.json() {
            Ok(v) => v,
            Err(e) => return OllamaState::Unreachable(format!("{tags}: {e}")),
        },
        Err(e) => return OllamaState::Unreachable(e.to_string()),
    };
    let present: Vec<String> = v["models"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| m["name"].as_str().or_else(|| m["model"].as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let missing = models
        .iter()
        .filter(|m| {
            !present
                .iter()
                .any(|p| p == *m || p.trim_end_matches(":latest") == m.trim_end_matches(":latest"))
        })
        .cloned()
        .collect();
    OllamaState::Reachable { missing }
}

pub fn pull(model: &str) -> Result<()> {
    let status = std::process::Command::new("ollama")
        .arg("pull")
        .arg(model)
        .status()
        .context("cannot run `ollama pull`; is the ollama CLI installed?")?;
    if !status.success() {
        bail!("`ollama pull {model}` exited {status}");
    }
    Ok(())
}

pub fn preflight(
    url: &str,
    models: &[String],
    yes: bool,
    confirm: fn(&str) -> bool,
    pull: fn(&str) -> Result<()>,
) -> (Vec<String>, Vec<String>) {
    let mut lines = Vec::new();
    let mut warnings = Vec::new();
    match probe(url, models) {
        OllamaState::Unreachable(why) => warnings.push(format!(
            "Ollama is not reachable at {url} ({why}). Install it from https://ollama.com/download, \
             start it with `ollama serve`, then run `br8n install` again to pull the model."
        )),
        OllamaState::Reachable { missing } if missing.is_empty() => {
            lines.push(format!(
                "ollama:   reachable at {url}, model(s) present: {}",
                models.join(", ")
            ));
        }
        OllamaState::Reachable { missing } => {
            let hint = missing
                .iter()
                .map(|m| format!("ollama pull {m}"))
                .collect::<Vec<_>>()
                .join(" && ");
            let go = yes || confirm(&format!("pull {} (about 1 GB)?", missing.join(", ")));
            if !go {
                warnings.push(format!(
                    "missing model(s): {} — run: {hint}",
                    missing.join(", ")
                ));
                return (lines, warnings);
            }
            for m in &missing {
                match pull(m) {
                    Ok(()) => lines.push(format!("ollama:   pulled {m}")),
                    Err(e) => warnings.push(format!("{e:#}; run: ollama pull {m}")),
                }
            }
        }
    }
    (lines, warnings)
}
