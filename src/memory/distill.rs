use super::{list_at, remember_at, Filter, MemoryKind, Origin, Outcome, Remember};
use crate::config::Config;
use crate::enrich::{generate, GenerateOpts};
use crate::loaders::transcript::{SessionAgent, SessionRoots};
use crate::model::Document;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const MIN_TRANSCRIPT_BYTES: u64 = 4096;
const HEAD_CHARS: usize = 4000;
const TAIL_CHARS: usize = 8000;
const NUM_CTX: u32 = 8192;
const NUM_PREDICT: u32 = 400;
const GENERATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);
const EPISODE_CONFIDENCE: u8 = 70;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DistillReport {
    pub candidates: usize,
    pub distilled: usize,
    pub skipped_idle: bool,
    pub latched: Option<String>,
}

pub fn distill_pending(cfg: &Config, limit: usize, require_idle: bool) -> Result<DistillReport> {
    distill_pending_with(
        &super::default_root(),
        cfg,
        &SessionRoots::from_env(),
        &Config::db_path(),
        limit,
        require_idle,
    )
}

pub fn distill_pending_at(
    root: &Path,
    cfg: &Config,
    transcripts: &SessionRoots,
    limit: usize,
    require_idle: bool,
) -> Result<DistillReport> {
    distill_pending_with(
        root,
        cfg,
        transcripts,
        &root.join("unused-db"),
        limit,
        require_idle,
    )
}

pub fn distill_pending_with(
    root: &Path,
    cfg: &Config,
    transcripts: &SessionRoots,
    db: &Path,
    limit: usize,
    require_idle: bool,
) -> Result<DistillReport> {
    let mut report = DistillReport::default();
    if require_idle
        && crate::index::a_query_was_seen_within(
            db,
            std::time::Duration::from_secs(cfg.memory.distill_idle_secs),
        )
    {
        report.skipped_idle = true;
        return Ok(report);
    }
    let known = known_sessions(root)?;
    let pending = candidates(
        &distillable(cfg, transcripts),
        cfg.memory.distill_after_hours,
        &known,
    )?;
    report.candidates = pending.len();
    for (path, doc) in pending.into_iter().take(limit) {
        let agent = SessionAgent::from_uri(&doc.uri).unwrap_or(SessionAgent::ClaudeCode);
        match distill_session_as(root, cfg, agent, &path) {
            Ok(Outcome::Saved { .. }) | Ok(Outcome::Replaced { .. }) => report.distilled += 1,
            Ok(_) => {}
            Err(e) if is_connection_failure(&e) => {
                report.latched = Some(format!("{e:#}"));
                eprintln!("br8n: distillation stopped for this run — {e:#}");
                break;
            }
            Err(e) => eprintln!("br8n: could not distill {}: {e:#}", path.display()),
        }
    }
    Ok(report)
}

fn distillable<'a>(cfg: &Config, roots: &'a SessionRoots) -> Vec<(SessionAgent, &'a Path)> {
    let mut out = vec![(SessionAgent::ClaudeCode, roots.claude_code.as_path())];
    if cfg.index_codex_sessions {
        out.push((SessionAgent::Codex, roots.codex.as_path()));
    }
    out
}

fn is_connection_failure(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<reqwest::Error>().is_some_and(|r| {
            r.is_connect() || r.is_timeout() || r.status().is_some_and(|s| s.as_u16() == 404)
        })
    })
}

pub struct Known {
    pub source_hash: String,
    pub created: i64,
    pub source_stamp: Option<String>,
}

fn known_sessions(root: &Path) -> Result<HashMap<String, Known>> {
    let mut out = HashMap::new();
    for m in list_at(
        root,
        &Filter {
            kind: Some(MemoryKind::Episode),
            project: None,
        },
    )? {
        let source_stamp = m.facts.source_stamp.clone();
        if let (Some(s), Some(h)) = (m.facts.session, m.facts.source_hash) {
            out.insert(
                s,
                Known {
                    source_hash: h,
                    created: m.facts.created,
                    source_stamp,
                },
            );
        }
    }
    Ok(out)
}

pub fn candidates(
    sessions: &[(SessionAgent, &Path)],
    after_hours: f32,
    known: &HashMap<String, Known>,
) -> Result<Vec<(PathBuf, Document)>> {
    let mut out = Vec::new();
    for (agent, transcripts) in sessions {
        candidates_in(*agent, transcripts, after_hours, known, &mut out);
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn candidates_in(
    agent: SessionAgent,
    transcripts: &Path,
    after_hours: f32,
    known: &HashMap<String, Known>,
    out: &mut Vec<(PathBuf, Document)>,
) {
    if !transcripts.exists() {
        return;
    }
    let min_age = std::time::Duration::from_secs_f32(after_hours.max(0.0) * 3600.0);
    for entry in ignore::WalkBuilder::new(transcripts).build().flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = p.metadata() else { continue };
        if meta.len() < MIN_TRANSCRIPT_BYTES {
            continue;
        }
        let quiet = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .unwrap_or_default();
        if quiet < min_age {
            continue;
        }
        if let (Some(stamp), Ok(canon)) = (crate::index::stamp(p), p.canonicalize()) {
            let uri = agent.uri_for(&canon);
            if known
                .get(&uri)
                .and_then(|k| k.source_stamp.as_deref())
                .is_some_and(|known_stamp| known_stamp == stamp)
            {
                continue;
            }
        }
        let Ok(doc) = agent.load_session(p) else {
            continue;
        };
        if known
            .get(&doc.uri)
            .is_some_and(|k| k.source_hash == doc.content_hash)
        {
            continue;
        }
        out.push((p.to_path_buf(), doc));
    }
}

pub fn distill_session(cfg: &Config, path: &Path) -> Result<Outcome> {
    distill_session_at(&super::default_root(), cfg, path)
}

pub fn distill_session_at(root: &Path, cfg: &Config, path: &Path) -> Result<Outcome> {
    distill_session_as(root, cfg, SessionAgent::sniff(path), path)
}

pub fn distill_session_as(
    root: &Path,
    cfg: &Config,
    agent: SessionAgent,
    path: &Path,
) -> Result<Outcome> {
    let doc = agent.load_session(path)?;
    let source_stamp = crate::index::stamp(path);
    let input = distill_input(&doc.text);
    let client = reqwest::blocking::Client::builder()
        .timeout(GENERATE_TIMEOUT)
        .build()
        .context("build http client")?;
    let opts = GenerateOpts {
        num_predict: NUM_PREDICT,
        num_ctx: Some(NUM_CTX),
        keep_alive: "0".into(),
        temperature: 0.0,
    };
    let summary = generate(
        &client,
        &cfg.embed.ollama_url,
        &cfg.memory.distill_model,
        &build_prompt(&input),
        &opts,
    )?;
    let summary = clip_summary(&summary);
    if summary.is_empty() || summary.eq_ignore_ascii_case("nothing") {
        return Ok(Outcome::Rejected(
            "the model judged the session trivial".into(),
        ));
    }
    let project = doc.meta["project"]
        .as_str()
        .filter(|p| !p.is_empty())
        .map(PathBuf::from);
    let started = doc.meta["started"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(10)
        .collect::<String>();
    let name = project
        .as_ref()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("session");
    let title = if started.is_empty() {
        format!("{name} session")
    } else {
        format!("{name} session, {started}")
    };
    let embedder = crate::embed::for_config(&cfg.embed)?;
    remember_at(
        root,
        cfg,
        embedder,
        Remember {
            kind: MemoryKind::Episode,
            text: summary,
            title: Some(title),
            project,
            confidence: EPISODE_CONFIDENCE,
            origin: Origin::Distill,
            session: Some(doc.uri.clone()),
            source_hash: Some(doc.content_hash.clone()),
            source_stamp,
            created: None,
        },
    )
}

fn clip_summary(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.chars().count() <= super::MAX_TEXT_CHARS {
        return trimmed.to_string();
    }
    let mut out = String::new();
    for line in trimmed.lines() {
        if out.chars().count() + line.chars().count() + 1 > super::MAX_TEXT_CHARS {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}

pub fn distill_input(session_markdown: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    for section in session_markdown.split("\n## ") {
        let Some((heading, body)) = section.split_once('\n') else {
            continue;
        };
        let heading = heading.trim_start_matches("## ");
        let role = if heading.starts_with("user") {
            "User"
        } else if heading.starts_with("assistant") {
            "Assistant"
        } else {
            continue;
        };
        let body = strip_spans(body, "<br8n-context");
        let body = strip_spans(&body, "<command-");
        let lines: Vec<&str> = body
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with("[tool:") && !l.starts_with("[result]"))
            .collect();
        if lines.is_empty() {
            continue;
        }
        kept.push(format!("{role}: {}", lines.join(" ")));
    }
    let joined = kept.join("\n");
    let n = joined.chars().count();
    if n <= HEAD_CHARS + TAIL_CHARS {
        return joined;
    }
    let head: String = joined.chars().take(HEAD_CHARS).collect();
    let tail: String = joined.chars().skip(n - TAIL_CHARS).collect();
    format!("{head}\n[…]\n{tail}")
}

fn strip_spans(text: &str, open_prefix: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(open_prefix) {
        out.push_str(&rest[..start]);
        let after_lt = &rest[start + 1..];
        let Some(gt) = after_lt.find('>') else {
            return out;
        };
        let name = &after_lt[..gt];
        if name.ends_with('/') {
            rest = &after_lt[gt + 1..];
            continue;
        }
        let close = format!("</{name}>");
        let after_open = &after_lt[gt + 1..];
        match after_open.find(close.as_str()) {
            Some(end) => rest = &after_open[end + close.len()..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

pub fn build_prompt(input: &str) -> String {
    format!(
        "Below is a coding session between a user and an AI assistant, with tool output removed.\n\n\
         <session>\n{input}\n</session>\n\n\
         Write 3 to 6 bullet points in the past tense describing what was worked on, what was \
         decided and why, what was tried and failed, and what was left unfinished. Name files, \
         commands and tools where they matter. Do not invent details that are not in the session. \
         If the session was trivial or contains no real work, answer with exactly: NOTHING\n\n\
         Bullets:"
    )
}
