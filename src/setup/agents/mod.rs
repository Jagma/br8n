pub mod api;
pub mod claude_code;
pub mod claude_desktop;
pub mod codex;
pub mod cursor;
pub mod files;
pub mod gemini;
pub mod json_config;

use super::claude::ClaudeCli;
use super::Paths;
use anyhow::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const SERVER_NAME: &str = "br8n";
pub const MCP_ARGS: [&str; 1] = ["mcp"];

#[derive(Debug, Clone)]
pub struct AgentEnv {
    pub home: PathBuf,
    pub app_config: PathBuf,
    pub codex_home: PathBuf,
    pub search_path: Vec<PathBuf>,
    pub paths: Paths,
    pub claude: ClaudeCli,
    pub version: String,
}

impl AgentEnv {
    pub fn from_env() -> AgentEnv {
        let paths = Paths::from_env();
        let home = directories::BaseDirs::new()
            .map(|b| b.home_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let app_config = if cfg!(target_os = "macos") {
            home.join("Library/Application Support")
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"))
        };
        let codex_home = codex_home_from(std::env::var_os("CODEX_HOME"), &home);
        let search_path = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        AgentEnv {
            home,
            app_config,
            codex_home,
            search_path,
            paths,
            claude: ClaudeCli::from_path(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    pub fn at(home: &Path, paths: Paths, search_path: Vec<PathBuf>) -> AgentEnv {
        let mut env = AgentEnv {
            home: home.to_path_buf(),
            app_config: home.join(".config"),
            codex_home: home.join(".codex"),
            search_path,
            paths,
            claude: ClaudeCli::at(Path::new("/nonexistent/claude")),
            version: env!("CARGO_PKG_VERSION").to_string(),
        };
        if let Some(claude) = env.find_program("claude") {
            env.claude = ClaudeCli::at(&claude);
        }
        env
    }

    pub fn bin(&self) -> &Path {
        &self.paths.bin
    }

    pub fn bin_str(&self) -> String {
        self.paths.bin.to_string_lossy().into_owned()
    }

    pub fn find_program(&self, name: &str) -> Option<PathBuf> {
        self.search_path
            .iter()
            .map(|d| d.join(name))
            .find(|c| c.is_file())
    }
}

pub fn codex_home() -> PathBuf {
    let home = directories::BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    codex_home_from(std::env::var_os("CODEX_HOME"), &home)
}

pub fn codex_home_from(codex_home_var: Option<std::ffi::OsString>, home: &Path) -> PathBuf {
    codex_home_var
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Detected {
    pub installed: bool,
    pub version: Option<String>,
    pub config_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Connected,
    NotConnected,
    Stale(String),
    Broken(String),
}

impl Status {
    pub fn state(&self) -> &'static str {
        match self {
            Status::Connected => "connected",
            Status::NotConnected => "not_connected",
            Status::Stale(_) => "stale",
            Status::Broken(_) => "broken",
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Status::Stale(r) | Status::Broken(r) => Some(r),
            _ => None,
        }
    }

    pub fn label(&self) -> String {
        match self.reason() {
            Some(r) => format!("{}: {r}", self.state().replace('_', " ")),
            None => self.state().replace('_', " "),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    pub mcp: bool,
    pub prompt_hook: bool,
    pub session_hook: bool,
    pub transcripts: bool,
    pub instructions: bool,
}

impl Capabilities {
    pub fn names(&self) -> Vec<&'static str> {
        [
            (self.mcp, "mcp"),
            (self.prompt_hook, "prompt hook"),
            (self.session_hook, "session hook"),
            (self.transcripts, "transcripts"),
            (self.instructions, "instructions"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ConnectOptions {
    pub instructions: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Change {
    pub files: Vec<PathBuf>,
    pub backups: Vec<PathBuf>,
    pub notes: Vec<String>,
}

impl Change {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.notes.is_empty()
    }

    pub fn wrote(&mut self, file: &Path, written: files::Written) {
        if !self.files.iter().any(|f| f == file) {
            self.files.push(file.to_path_buf());
        }
        if let Some(b) = written.backup {
            self.backups.push(b);
        }
    }

    pub fn absorb(&mut self, other: Change) {
        for f in other.files {
            if !self.files.contains(&f) {
                self.files.push(f);
            }
        }
        self.backups.extend(other.backups);
        self.notes.extend(other.notes);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("unknown agent `{0}`; known agents: {known}", known = ids().join(", "))]
    Unknown(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    BadOption(String),
}

pub trait Agent: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn detect(&self, env: &AgentEnv) -> Detected;
    fn status(&self, env: &AgentEnv) -> Status;
    fn connect(&self, env: &AgentEnv, opts: &ConnectOptions) -> Result<Change>;
    fn disconnect(&self, env: &AgentEnv) -> Result<Change>;
    fn capabilities(&self) -> Capabilities;
    fn snippet(&self, env: &AgentEnv) -> Option<String>;
    fn instructions(&self, _env: &AgentEnv) -> Option<bool> {
        None
    }
}

pub fn all() -> Vec<Box<dyn Agent>> {
    vec![
        Box::new(claude_code::ClaudeCode),
        Box::new(codex::Codex),
        Box::new(claude_desktop::ClaudeDesktop),
        Box::new(cursor::Cursor),
        Box::new(gemini::Gemini),
    ]
}

pub fn ids() -> Vec<&'static str> {
    all().iter().map(|a| a.id()).collect()
}

pub fn find(id: &str) -> Result<Box<dyn Agent>> {
    all()
        .into_iter()
        .find(|a| a.id() == id)
        .ok_or_else(|| AgentError::Unknown(id.to_string()).into())
}

pub fn connect(agent: &dyn Agent, env: &AgentEnv, opts: &ConnectOptions) -> Result<Change> {
    if opts.instructions && !agent.capabilities().instructions {
        return Err(AgentError::BadOption(format!(
            "{} takes no instructions file; only codex does",
            agent.id()
        ))
        .into());
    }
    agent.connect(env, opts)
}

pub fn with_binary_check(status: Status, env: &AgentEnv) -> Status {
    match status {
        Status::Connected if !env.bin().is_file() => Status::Broken(format!(
            "the entry points at {}, which does not exist; run `br8n install`",
            env.bin().display()
        )),
        other => other,
    }
}

pub fn detect_by(env: &AgentEnv, programs: &[&str], dirs: &[PathBuf], config: PathBuf) -> Detected {
    let program = programs.iter().find_map(|p| env.find_program(p));
    let version = program.as_deref().and_then(program_version);
    Detected {
        installed: program.is_some() || dirs.iter().any(|d| d.is_dir()),
        version,
        config_path: Some(config),
    }
}

const VERSION_TIMEOUT: Duration = Duration::from_secs(3);

pub fn program_version(program: &Path) -> Option<String> {
    use std::io::Read;
    let mut child = std::process::Command::new(program)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < VERSION_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    if !status.success() {
        return None;
    }
    out.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

pub fn mcp_json_snippet(env: &AgentEnv) -> String {
    let mut doc = files::Json::object();
    json_config::upsert_server(&mut doc, &env.bin_str(), &[]).expect("a fresh object");
    doc.render("  ")
}

pub fn codex_toml_snippet(env: &AgentEnv) -> String {
    let mut doc = toml_edit::DocumentMut::new();
    codex::upsert_server(&mut doc, &env.bin_str()).expect("a fresh document");
    doc.to_string()
}

pub fn quoted_command(bin: &str, rest: &str) -> String {
    format!("\"{bin}\" {rest}")
}

#[derive(Debug, Default)]
pub struct AgentsReport {
    pub lines: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn describe(agent: &dyn Agent, change: &Change) -> String {
    if change.is_empty() {
        return format!("{}: already connected, nothing changed", agent.id());
    }
    let mut parts = Vec::new();
    if !change.files.is_empty() {
        parts.push(format!(
            "wrote {}",
            change
                .files
                .iter()
                .map(|f| f.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !change.backups.is_empty() {
        parts.push(format!(
            "original kept at {}",
            change
                .backups
                .iter()
                .map(|f| f.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    parts.extend(change.notes.iter().cloned());
    format!("{}: {}", agent.id(), parts.join("; "))
}

pub fn offer_after_install(
    env: &AgentEnv,
    yes: bool,
    quiet: bool,
    confirm: fn(&str) -> bool,
) -> AgentsReport {
    let mut r = AgentsReport::default();
    for agent in all().into_iter().filter(|a| a.id() != claude_code::ID) {
        let status = agent.status(env);
        let wanted = match &status {
            Status::Connected => {
                r.lines.push(format!("{}: connected", agent.id()));
                false
            }
            Status::Stale(_) => true,
            Status::Broken(reason) => {
                r.warnings.push(format!("{}: {reason}", agent.id()));
                false
            }
            Status::NotConnected => {
                !quiet
                    && agent.detect(env).installed
                    && (yes
                        || confirm(&format!(
                            "connect br8n to {} as well?",
                            agent.display_name()
                        )))
            }
        };
        if !wanted {
            if status == Status::NotConnected && !quiet && agent.detect(env).installed {
                r.lines.push(format!(
                    "{}: detected, not connected (run `br8n connect {}`)",
                    agent.id(),
                    agent.id()
                ));
            }
            continue;
        }
        match agent.connect(env, &ConnectOptions::default()) {
            Ok(change) => r.lines.push(describe(agent.as_ref(), &change)),
            Err(e) => r.warnings.push(format!("{}: {e:#}", agent.id())),
        }
    }
    r
}

pub fn disconnect_all_but_claude_code(env: &AgentEnv) -> AgentsReport {
    let mut r = AgentsReport::default();
    for agent in all().into_iter().filter(|a| a.id() != claude_code::ID) {
        if agent.status(env) == Status::NotConnected {
            continue;
        }
        match agent.disconnect(env) {
            Ok(change) if change.is_empty() => {}
            Ok(change) => r.lines.push(format!(
                "disconnected {} ({})",
                agent.id(),
                change
                    .files
                    .iter()
                    .map(|f| f.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Err(e) => r
                .warnings
                .push(format!("could not disconnect {}: {e:#}", agent.id())),
        }
    }
    r
}
