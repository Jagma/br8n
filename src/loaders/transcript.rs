use crate::model::{Document, SourceType};
use anyhow::Result;
use once_cell::sync::Lazy;
use regex::Regex;
use std::path::{Path, PathBuf};

// URLs are stripped before scanning, otherwise "https://example.com/index.js"
// yields a phantom file `example.com/index.js`.
// Scheme URLs, www-prefixed hosts, and bare hosts on unambiguous TLDs.
// `.rs` and `.sh` are deliberately ABSENT from this TLD list because they
// collide with real source extensions — `main.rs` must still be recognised.
static URL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?:https?://\S+)|(?:\bwww\.\S+)|(?:\b[\w-]+(?:\.[\w-]+)*\.(?:com|org|net|io|dev|ai|app|co|edu|gov)\b(?:/\S*)?)",
    )
    .unwrap()
});

/// Developer hosts whose TLD is also a source extension, so no pattern can
/// separate them from a filename. `docs.rs` appears in almost every Rust
/// session and is indistinguishable from `main.rs` by shape alone.
static AMBIGUOUS_HOSTS: &[&str] = &["docs.rs", "lib.rs"];

// The final component's stem must contain a letter, so a bare "1.rs" in prose is
// not mistaken for a file. These become MENTIONS graph edges, and garbage edges
// drag unrelated content into retrieval — the same failure mode the wikilink
// regex had in Task 6.
//
// Verified: "src/db/pool.rs and 1.rs, version 1.2.3, e.g. this, main.go,
// config.toml, https://example.com/index.js, Cargo.lock, crates/foo/src/lib.rs,
// README.md"
//   -> ["Cargo.lock", "README.md", "config.toml", "crates/foo/src/lib.rs",
//       "main.go", "src/db/pool.rs"]
static FILE_REF: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"\b((?:[\w.-]+/)*[\w-]*[A-Za-z][\w-]*\.(?:rs|ts|tsx|js|jsx|py|go|java|rb|c|h|cpp|md|toml|json|ya?ml|sql|sh|lock))\b",
    )
    .unwrap()
});

pub struct TranscriptLoader {
    root: PathBuf,
}

impl TranscriptLoader {
    /// How long a transcript must sit untouched before it is worth reading.
    ///
    /// A session transcript is APPENDED TO for as long as its session is
    /// alive, so reading one mid-session re-reads, re-chunks and re-hashes a
    /// file that will change again within seconds — and on this machine ~722
    /// of 760 indexed documents are transcripts, so that is most of what every
    /// run does. Every `SessionStart` therefore found work, which is why the
    /// indexer looked like it restarted the moment it finished.
    ///
    /// Ten minutes, and the number trades in one direction only. The highest-
    /// value transcript query is "what did we just decide", so a long window
    /// throws away exactly the content this tool exists to find: an hour would
    /// make the last hour of your own work unsearchable. Much shorter than a
    /// minute buys nothing, because a live session appends every few seconds
    /// and the file would be re-read on essentially every run regardless.
    ///
    /// What ten minutes costs is the transcript of the session you are sitting
    /// in — it stays out of the index until you have been quiet for ten
    /// minutes. That is the cheapest thing to give up, because that
    /// conversation is still on your screen; the transcripts worth retrieving
    /// are the ones you have already left.
    pub const SETTLE: std::time::Duration = std::time::Duration::from_secs(600);

    /// Whether a transcript last modified `age` ago is still settling.
    ///
    /// Split out and pure so the boundary can be tested without a clock.
    pub fn is_settling(age: std::time::Duration) -> bool {
        age < Self::SETTLE
    }

    /// `Some(age)` when `path` was modified within `SETTLE`, i.e. some live
    /// session is probably still appending to it.
    ///
    /// Every failure answers `None`, meaning "read it now". An unreadable
    /// mtime, or one in the FUTURE (a clock change, a restored backup, a copy
    /// that preserved a bad timestamp), would otherwise defer the file on
    /// every run forever — and a transcript deferred permanently is a document
    /// that leaves the corpus with nothing printed anywhere, which is the one
    /// outcome this must not be able to produce.
    pub fn settling_for(path: &Path) -> Option<std::time::Duration> {
        let age = std::fs::metadata(path)
            .ok()?
            .modified()
            .ok()?
            .elapsed()
            .ok()?;
        Self::is_settling(age).then_some(age)
    }

    pub fn new<P: AsRef<Path>>(root: P) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    /// Defaults to `~/.claude/projects` when no explicit root is configured.
    pub fn default_root() -> PathBuf {
        directories::BaseDirs::new()
            .map(|b| b.home_dir().join(".claude/projects"))
            .unwrap_or_else(|| PathBuf::from(".claude/projects"))
    }

    pub fn load_all(&self) -> Result<Vec<Document>> {
        let mut out = Vec::new();
        for entry in ignore::WalkBuilder::new(&self.root).build().flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Ok(doc) = Self::load_session(path) {
                out.push(doc);
            }
        }
        out.sort_by(|a, b| a.uri.cmp(&b.uri));
        Ok(out)
    }

    pub fn load_session(path: &Path) -> Result<Document> {
        let raw = std::fs::read_to_string(path)?;
        let mut md = String::new();
        let mut project = String::new();
        let mut started = String::new();
        let mut turn = 0usize;

        for line in raw.lines().filter(|l| !l.trim().is_empty()) {
            let v: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if project.is_empty() {
                if let Some(c) = v["cwd"].as_str() {
                    project = c.to_string();
                }
            }
            if started.is_empty() {
                if let Some(t) = v["timestamp"].as_str() {
                    started = t.to_string();
                }
            }
            let role = match v["message"]["role"].as_str() {
                Some(r) => r,
                None => continue,
            };
            let text = flatten_content(&v["message"]["content"]);
            if text.trim().is_empty() {
                continue;
            }
            // A bare "user"/"assistant" heading is contentless, and the heading
            // path is prefixed onto every chunk's embed text — so it added the
            // same two words to every vector in the corpus. Number the turns so
            // the heading at least locates a chunk within the session.
            turn += 1;
            md.push_str(&format!("## {role} (turn {turn})\n\n{}\n\n", text.trim()));
        }

        anyhow::ensure!(!md.trim().is_empty(), "empty transcript");

        let files = mentioned_files(&md);

        let session = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("session");
        // Lead with the project, not the UUID. The title is prefixed onto every
        // chunk's embed text, so a 36-character random identifier was pure noise
        // added to every vector in the largest source in the corpus. The short
        // session id stays for disambiguation between same-project sessions.
        let title = if project.is_empty() {
            format!("Session {session}")
        } else {
            let name = std::path::Path::new(&project)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(project.as_str());
            let short: String = session.chars().take(8).collect();
            format!("{name} session ({short})")
        };

        let uri = SessionAgent::ClaudeCode.uri_for(&path.canonicalize()?);
        let mut doc = Document::new(SourceType::Transcript, &uri, &title, &md);
        doc.meta = serde_json::json!({
            "project": project,
            "started": started,
            "files": files,
        });
        Ok(doc)
    }
}

pub(crate) fn mentioned_files(md: &str) -> Vec<String> {
    let cleaned = URL.replace_all(md, " ");
    let mut v: Vec<String> = FILE_REF
        .captures_iter(&cleaned)
        .map(|c| c[1].to_string())
        .filter(|f| !AMBIGUOUS_HOSTS.contains(&f.as_str()))
        .collect();
    v.sort();
    v.dedup();
    v
}

pub(crate) const TOOL_TARGET_KEYS: [&str; 6] =
    ["file_path", "path", "command", "pattern", "query", "url"];

const TOOL_RESULT_BUDGET: usize = 600;

pub(crate) fn tool_line(name: &str, target: &str) -> String {
    if target.is_empty() {
        format!("[tool: {name}]")
    } else {
        format!("[tool: {name} {target}]")
    }
}

pub(crate) fn result_line(body: &str) -> Option<String> {
    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    let mut clipped: String = body.chars().take(TOOL_RESULT_BUDGET).collect();
    if body.chars().count() > TOOL_RESULT_BUDGET {
        clipped.push('…');
    }
    Some(format!("[result] {clipped}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAgent {
    ClaudeCode,
    Codex,
}

impl SessionAgent {
    pub const ALL: [SessionAgent; 2] = [SessionAgent::ClaudeCode, SessionAgent::Codex];

    pub fn id(self) -> &'static str {
        match self {
            SessionAgent::ClaudeCode => "claude-code",
            SessionAgent::Codex => "codex",
        }
    }

    pub fn scheme(self) -> &'static str {
        match self {
            SessionAgent::ClaudeCode => "claude-session://",
            SessionAgent::Codex => "codex-session://",
        }
    }

    pub fn from_uri(uri: &str) -> Option<SessionAgent> {
        Self::ALL.into_iter().find(|a| uri.starts_with(a.scheme()))
    }

    pub fn path_of(uri: &str) -> Option<&Path> {
        let agent = Self::from_uri(uri)?;
        uri.strip_prefix(agent.scheme()).map(Path::new)
    }

    pub fn sniff(path: &Path) -> SessionAgent {
        use std::io::BufRead;
        let first = std::fs::File::open(path).ok().and_then(|f| {
            std::io::BufReader::new(f)
                .lines()
                .map_while(|l| l.ok())
                .find(|l| !l.trim().is_empty())
        });
        let is_codex = first
            .and_then(|l| serde_json::from_str::<serde_json::Value>(&l).ok())
            .is_some_and(|v| v["type"].as_str() == Some("session_meta"));
        if is_codex {
            SessionAgent::Codex
        } else {
            SessionAgent::ClaudeCode
        }
    }

    pub fn uri_for(self, canonical: &Path) -> String {
        format!("{}{}", self.scheme(), canonical.display())
    }

    pub fn load_session(self, path: &Path) -> Result<Document> {
        match self {
            SessionAgent::ClaudeCode => TranscriptLoader::load_session(path),
            SessionAgent::Codex => super::codex::CodexSessionLoader::load_session(path),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionRoots {
    pub claude_code: PathBuf,
    pub codex: PathBuf,
}

impl SessionRoots {
    pub fn from_env() -> SessionRoots {
        SessionRoots {
            claude_code: TranscriptLoader::default_root(),
            codex: super::codex::CodexSessionLoader::default_root(),
        }
    }

    pub fn enabled(&self, cfg: &crate::config::Config) -> Vec<(SessionAgent, &Path)> {
        if !cfg.index_transcripts {
            return Vec::new();
        }
        let mut out = vec![(SessionAgent::ClaudeCode, self.claude_code.as_path())];
        if cfg.index_codex_sessions {
            out.push((SessionAgent::Codex, self.codex.as_path()));
        }
        out
    }
}

/// Assistant content is an array of typed blocks; user content is usually a string.
///
/// Only `text` blocks used to survive, which discarded most of what makes a
/// session worth searching: which tool ran, against which file, and what came
/// back. A session where the work was "edited src/store/mod.rs and ran the
/// tests" indexed as whatever prose happened to surround it, so searching for
/// the filename found nothing.
///
/// Tool results are truncated rather than dropped — a 5000-line test log adds
/// noise, but its first lines carry the command and the outcome.
fn flatten_content(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|b| match b["type"].as_str() {
                Some("text") | None => b["text"].as_str().map(str::to_string),
                Some("tool_use") => {
                    let name = b["name"].as_str().unwrap_or("tool");
                    // The interesting part of the input is whichever field names
                    // a target: a path, a command, a pattern, a URL.
                    let target = TOOL_TARGET_KEYS
                        .iter()
                        .find_map(|k| b["input"][k].as_str())
                        .unwrap_or_default();
                    Some(tool_line(name, target))
                }
                Some("tool_result") => {
                    let body = match &b["content"] {
                        serde_json::Value::String(s) => s.clone(),
                        other => flatten_content(other),
                    };
                    result_line(&body)
                }
                // Thinking is the model reasoning to itself, not a record of the
                // work. Indexing it buys noise the user never wrote or read.
                Some("thinking") => None,
                _ => b["text"].as_str().map(str::to_string),
            })
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

impl super::Loader for TranscriptLoader {
    fn load(&self, uri: &str) -> Result<Vec<Document>> {
        let path = uri
            .strip_prefix(SessionAgent::ClaudeCode.scheme())
            .unwrap_or(uri);
        Ok(vec![Self::load_session(Path::new(path))?])
    }
}
