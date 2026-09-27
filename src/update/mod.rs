pub mod download;
pub mod release;

use crate::setup::Paths;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const CHECK_EVERY: u64 = 24 * 3600;
pub const RETRY_AFTER_FAILURE: u64 = 3600;

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCheck {
    pub installed: String,
    pub latest: Option<String>,
    pub url: Option<String>,
    pub checked_at: u64,
    pub error: Option<String>,
}

impl UpdateCheck {
    pub fn read(p: &Path) -> Option<UpdateCheck> {
        serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
    }

    pub fn write(&self, p: &Path) -> Result<()> {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(p, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("write {}", p.display()))
    }

    pub fn available(&self) -> Option<String> {
        self.available_against(&self.installed)
    }

    pub fn available_against(&self, installed: &str) -> Option<String> {
        let latest = semver::Version::parse(self.latest.as_deref()?).ok()?;
        let installed = semver::Version::parse(installed).ok()?;
        (latest > installed).then(|| latest.to_string())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CheckDecision {
    Check,
    Skip(String),
}

pub fn check_decision(previous: Option<&UpdateCheck>, now: u64, enabled: bool) -> CheckDecision {
    if !enabled {
        return CheckDecision::Skip("update.check = false".to_string());
    }
    let Some(prev) = previous else {
        return CheckDecision::Check;
    };
    let age = now.saturating_sub(prev.checked_at);
    let limit = if prev.error.is_some() {
        RETRY_AFTER_FAILURE
    } else {
        CHECK_EVERY
    };
    if age < limit {
        CheckDecision::Skip(format!(
            "last check {age}s ago, inside the {limit}s interval"
        ))
    } else {
        CheckDecision::Check
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub pid: u32,
    pub started_at: u64,
    pub from: String,
    pub to: Option<String>,
    pub phase: String,
    pub message: String,
    pub done: bool,
    pub ok: Option<bool>,
}

impl Status {
    pub fn read(p: &Path) -> Option<Status> {
        serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
    }

    pub fn running(p: &Path) -> Option<Status> {
        let s = Status::read(p)?;
        (!s.done && crate::index::pid_is_alive(s.pid)).then_some(s)
    }

    fn write(&self, p: &Path) {
        let _ = std::fs::write(p, serde_json::to_string(self).unwrap_or_default());
    }
}

pub struct UpdateOpts {
    pub paths: Paths,
    pub installed: String,
    pub api: String,
    pub token: Option<String>,
    pub target: String,
    pub check_only: bool,
}

#[derive(Debug)]
pub enum Outcome {
    CheckOnly(UpdateCheck),
    UpToDate(String),
    Updated { from: String, to: String },
}

pub fn check(opts: &UpdateOpts) -> Result<UpdateCheck> {
    let agent = format!("br8n/{}", opts.installed);
    let previous = UpdateCheck::read(&opts.paths.update_json);
    let record = match release::fetch_latest(&opts.api, opts.token.as_deref(), &agent) {
        Ok(r) => UpdateCheck {
            installed: opts.installed.clone(),
            latest: Some(r.version.to_string()),
            url: Some(r.html_url),
            checked_at: now(),
            error: None,
        },
        Err(e) => UpdateCheck {
            installed: opts.installed.clone(),
            latest: previous.as_ref().and_then(|p| p.latest.clone()),
            url: previous.as_ref().and_then(|p| p.url.clone()),
            checked_at: now(),
            error: Some(format!("{e:#}")),
        },
    };
    record.write(&opts.paths.update_json)?;
    match &record.error {
        Some(e) => Err(anyhow!("update check failed: {e}")),
        None => Ok(record),
    }
}

struct Staging(std::path::PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Phase<'a> {
    path: Option<&'a Path>,
    status: Status,
}
impl Phase<'_> {
    fn set(&mut self, phase: &str, message: impl Into<String>) {
        self.status.phase = phase.to_string();
        self.status.message = message.into();
        self.publish();
    }
    fn finish(&mut self, ok: bool, message: impl Into<String>) {
        self.status.done = true;
        self.status.ok = Some(ok);
        self.status.phase = if ok { "done" } else { "failed" }.to_string();
        self.status.message = message.into();
        self.publish();
    }
    fn publish(&self) {
        if let Some(path) = self.path {
            self.status.write(path);
        }
    }
}

pub fn run(opts: &UpdateOpts) -> Result<Outcome> {
    let paths = &opts.paths;
    if !opts.check_only && crate::index::IndexLock::is_held(&paths.db) {
        bail!("an index is running; wait for it to finish (`br8n status` shows progress), then run `br8n update` again");
    }
    if let Some(s) = Status::running(&paths.update_status) {
        bail!("an update is already running (pid {}, {})", s.pid, s.phase);
    }
    std::fs::create_dir_all(&paths.root)?;
    if !opts.check_only {
        let _ = std::fs::remove_file(&paths.update_status);
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&paths.update_status)
            .with_context(|| {
                format!(
                    "another update just started ({})",
                    paths.update_status.display()
                )
            })?;
    }
    let mut phase = Phase {
        path: (!opts.check_only).then_some(paths.update_status.as_path()),
        status: Status {
            pid: std::process::id(),
            started_at: now(),
            from: opts.installed.clone(),
            to: None,
            phase: "checking".to_string(),
            message: opts.api.clone(),
            done: false,
            ok: None,
        },
    };
    phase.set("checking", opts.api.clone());

    match run_phases(opts, &mut phase) {
        Ok(o) => Ok(o),
        Err(e) => {
            phase.finish(false, format!("{e:#}"));
            Err(e)
        }
    }
}

fn run_phases(opts: &UpdateOpts, phase: &mut Phase<'_>) -> Result<Outcome> {
    let paths = &opts.paths;
    let agent = format!("br8n/{}", opts.installed);
    let record = check(opts)?;
    let Some(latest) = record.available() else {
        phase.finish(true, format!("up to date at {}", opts.installed));
        return Ok(if opts.check_only {
            Outcome::CheckOnly(record)
        } else {
            Outcome::UpToDate(opts.installed.clone())
        });
    };
    phase.status.to = Some(latest.clone());
    if opts.check_only {
        phase.finish(true, format!("{latest} is available"));
        return Ok(Outcome::CheckOnly(record));
    }

    let release = release::fetch_latest(&opts.api, opts.token.as_deref(), &agent)?;
    let tgz_name = format!("br8n-{}.tar.gz", opts.target);
    let sha_name = format!("br8n-{}.sha256", opts.target);
    let tgz = release::asset(&release, &tgz_name)?;
    let sha = release::asset(&release, &sha_name)?;

    std::fs::create_dir_all(&paths.tmp)?;
    let staging = Staging(paths.tmp.join(format!("update-{}", std::process::id())));
    std::fs::create_dir_all(&staging.0)?;
    let tgz_path = staging.0.join(&tgz_name);
    let sha_path = staging.0.join(&sha_name);

    phase.set("downloading", tgz_name.clone());
    download::download(&tgz.url, opts.token.as_deref(), &agent, &tgz_path)?;
    download::download(&sha.url, opts.token.as_deref(), &agent, &sha_path)?;

    phase.set("verifying", "checksum");
    download::verify_sha256(&tgz_path, &sha_path)?;

    phase.set("extracting", tgz_name.clone());
    let new_bin = download::extract_single(&tgz_path, &staging.0.join("unpacked"))?;

    phase.set("verifying", format!("{} --version", new_bin.display()));
    let out = std::process::Command::new(&new_bin)
        .arg("--version")
        .output()
        .with_context(|| {
            format!(
                "the downloaded binary will not execute ({})",
                new_bin.display()
            )
        })?;
    let got = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let want = format!("br8n {latest}");
    if !out.status.success() || got != want {
        bail!("the downloaded binary reports `{got}`, expected `{want}`; refusing to install it");
    }

    phase.set("installing", format!("{} install", new_bin.display()));
    let status = std::process::Command::new(&new_bin)
        .args(["install", "--yes", "--quiet"])
        .status()
        .with_context(|| format!("run {} install", new_bin.display()))?;
    if !status.success() {
        bail!("`{} install` exited {status}", new_bin.display());
    }

    phase.finish(true, format!("updated {} -> {latest}", opts.installed));
    Ok(Outcome::Updated {
        from: opts.installed.clone(),
        to: latest,
    })
}

fn ago(seconds: u64) -> String {
    match seconds {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

pub fn version_line(
    installed: &str,
    check: Option<&UpdateCheck>,
    running: Option<&Status>,
    now: u64,
) -> String {
    if let Some(r) = running {
        return format!(
            "{installed}  (update to {} running: {})",
            r.to.as_deref().unwrap_or("?"),
            r.phase
        );
    }
    let Some(c) = check else {
        return format!("{installed}  (never checked for updates)");
    };
    let when = ago(now.saturating_sub(c.checked_at));
    if let Some(e) = &c.error {
        return format!("{installed}  (update check failed {when} ago: {e})");
    }
    match c.available_against(installed) {
        Some(v) => format!("{installed}  ({v} available — run br8n update)"),
        None => format!("{installed}  (up to date, checked {when} ago)"),
    }
}
