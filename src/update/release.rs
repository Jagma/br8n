use anyhow::{anyhow, Context, Result};

pub const DEFAULT_API: &str = "https://api.github.com/repos/Jagma/br8n";

#[derive(Debug, Clone)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct Release {
    pub version: semver::Version,
    pub html_url: String,
    pub assets: Vec<Asset>,
}

pub fn api_base() -> String {
    std::env::var("BR8N_RELEASE_API")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_API.to_string())
        .trim_end_matches('/')
        .to_string()
}

pub fn token() -> Option<String> {
    std::env::var("BR8N_GITHUB_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

pub fn parse_release(v: &serde_json::Value) -> Result<Release> {
    let tag = v["tag_name"].as_str().unwrap_or_default();
    let version = semver::Version::parse(tag.trim_start_matches('v'))
        .with_context(|| format!("release tag `{tag}` is not v<major>.<minor>.<patch>"))?;
    let assets = v["assets"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some(Asset {
                        name: x["name"].as_str()?.to_string(),
                        url: x["url"].as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Release {
        version,
        html_url: v["html_url"].as_str().unwrap_or_default().to_string(),
        assets,
    })
}

pub fn client(agent: &str, timeout: std::time::Duration) -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .user_agent(agent.to_string())
        .build()
        .context("build http client")
}

pub fn fetch_latest(api: &str, token: Option<&str>, agent: &str) -> Result<Release> {
    let url = format!("{}/releases/latest", api.trim_end_matches('/'));
    let mut req = client(agent, std::time::Duration::from_secs(5))?
        .get(&url)
        .header("Accept", "application/vnd.github+json");
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow!("GET {url}: HTTP {status}"));
    }
    let v: serde_json::Value = resp
        .json()
        .with_context(|| format!("GET {url}: not JSON"))?;
    parse_release(&v)
}

pub fn asset<'a>(r: &'a Release, name: &str) -> Result<&'a Asset> {
    r.assets.iter().find(|a| a.name == name).ok_or_else(|| {
        anyhow!(
            "release v{} has no asset named {name} (it has: {})",
            r.version,
            r.assets
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}
