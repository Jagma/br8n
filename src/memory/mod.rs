pub mod distill;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const MIN_TEXT_CHARS: usize = 10;
pub const MAX_TEXT_CHARS: usize = 2000;
pub const TITLE_CHARS: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryKind {
    Lesson,
    Fact,
    Episode,
}

impl MemoryKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryKind::Lesson => "lesson",
            MemoryKind::Fact => "fact",
            MemoryKind::Episode => "episode",
        }
    }

    pub fn parse(s: &str) -> Option<MemoryKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "lesson" => Some(MemoryKind::Lesson),
            "fact" => Some(MemoryKind::Fact),
            "episode" => Some(MemoryKind::Episode),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    User,
    Claude,
    Distill,
}

impl Origin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Origin::User => "user",
            Origin::Claude => "claude",
            Origin::Distill => "distill",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryFacts {
    pub kind: MemoryKind,
    pub created: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub origin: Origin,
    pub confidence: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_stamp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryMeta {
    #[serde(flatten)]
    pub facts: MemoryFacts,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Memory {
    pub id: String,
    pub uri: String,
    pub doc_id: String,
    pub title: String,
    pub text: String,
    pub facts: MemoryFacts,
}

pub fn root(db: &Path) -> PathBuf {
    db.with_file_name("memory")
}

pub fn store_dir(root: &Path) -> PathBuf {
    root.join("db")
}

pub fn pack_dir(root: &Path) -> PathBuf {
    root.join("pack")
}

pub fn memory_id(kind: MemoryKind, text: &str) -> String {
    let mut h = Sha256::new();
    h.update(kind.as_str().as_bytes());
    h.update(b"|");
    h.update(text.trim().as_bytes());
    hex::encode(&h.finalize()[..6])
}

pub fn uri_for(kind: MemoryKind, id: &str) -> String {
    format!("memory://{}/{id}", kind.as_str())
}

pub fn id_from_uri(uri: &str) -> Option<&str> {
    uri.strip_prefix("memory://")?.split('/').nth(1)
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn ymd(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

use crate::config::Config;
use crate::embed::Embedder;
use crate::index::{IndexLock, Indexer};
use crate::model::{Document, SourceType};
use crate::pack::{records, Pack};
use crate::store::Store;
use anyhow::Result;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub kind: Option<MemoryKind>,
    pub project: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Remember {
    pub kind: MemoryKind,
    pub text: String,
    pub title: Option<String>,
    pub project: Option<PathBuf>,
    pub confidence: u8,
    pub origin: Origin,
    pub session: Option<String>,
    pub source_hash: Option<String>,
    pub source_stamp: Option<String>,
    pub created: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Saved {
        id: String,
    },
    Replaced {
        id: String,
        previous: String,
        previous_title: String,
    },
    Duplicate {
        of: String,
    },
    Rejected(String),
}

impl Outcome {
    pub fn describe(&self, kind: MemoryKind) -> String {
        match self {
            Outcome::Saved { id } => format!("Saved {} {id}.", kind.as_str()),
            Outcome::Replaced {
                id,
                previous,
                previous_title,
            } => {
                format!(
                    "Saved {} {id}, replacing {previous} (\"{previous_title}\") which said \
                     nearly the same thing.",
                    kind.as_str()
                )
            }
            Outcome::Duplicate { of } => format!("Not saved: duplicate of {} {of}.", kind.as_str()),
            Outcome::Rejected(why) => format!("Not saved: {why}"),
        }
    }
}

const LOCK_RETRIES: u32 = 30;
const LOCK_RETRY: std::time::Duration = std::time::Duration::from_millis(100);
const DEDUPE_K: usize = 5;
const DEDUPE_EFS: usize = 50;

pub fn default_root() -> PathBuf {
    root(&Config::db_path())
}

pub fn remember(cfg: &Config, r: Remember) -> Result<Outcome> {
    let embedder = crate::embed::for_config(&cfg.embed)?;
    remember_at(&default_root(), cfg, embedder, r)
}

pub const DISABLED: &str = "memory is disabled in config ([memory] enabled = false)";

fn validate_remember(cfg: &Config, r: &Remember) -> std::result::Result<String, String> {
    if !cfg.memory.enabled {
        return Err(DISABLED.into());
    }
    let text = r.text.trim().to_string();
    let chars = text.chars().count();
    if !(MIN_TEXT_CHARS..=MAX_TEXT_CHARS).contains(&chars) {
        return Err(format!(
            "a memory must be {MIN_TEXT_CHARS} to {MAX_TEXT_CHARS} characters, this one is {chars}"
        ));
    }
    if r.origin == Origin::Claude && r.confidence < cfg.memory.min_confidence {
        return Err(format!(
            "confidence {} is below the {} floor for memories Claude writes on its own",
            r.confidence, cfg.memory.min_confidence
        ));
    }
    Ok(text)
}

pub fn remember_at(
    root: &Path,
    cfg: &Config,
    embedder: Box<dyn Embedder>,
    r: Remember,
) -> Result<Outcome> {
    let text = match validate_remember(cfg, &r) {
        Ok(text) => text,
        Err(reason) => return Ok(Outcome::Rejected(reason)),
    };

    let id = match r.kind {
        MemoryKind::Episode => r
            .session
            .as_deref()
            .and_then(session_id)
            .unwrap_or_else(|| memory_id(r.kind, &text)),
        _ => memory_id(r.kind, &text),
    };
    let uri = uri_for(r.kind, &id);
    let doc_id = Document::new_id(&uri);
    let model_id = embedder.model_id();
    let dims = embedder.dimensions();

    let existing = list_at(root, &Filter::default())?;
    if existing
        .iter()
        .any(|m| m.doc_id == doc_id && m.text == text)
    {
        return Ok(Outcome::Duplicate { of: id });
    }
    let near = nearest_of_kind(
        root,
        &model_id,
        dims,
        embedder.as_ref(),
        &text,
        r.kind,
        cfg.memory.duplicate_similarity,
    )?
    .filter(|m| m.doc_id != doc_id);
    let replaced = match (&near, r.kind) {
        (Some(m), MemoryKind::Episode) => return Ok(Outcome::Duplicate { of: m.id.clone() }),
        (Some(m), _) => Some(m.clone()),
        (None, _) => None,
    };

    let title = r.title.clone().unwrap_or_else(|| summary_title(&text));
    let facts = MemoryFacts {
        kind: r.kind,
        created: r.created.unwrap_or_else(now_secs),
        project: r.project.as_ref().map(|p| p.display().to_string()),
        origin: r.origin,
        confidence: r.confidence,
        session: r.session.clone(),
        source_hash: r.source_hash.clone(),
        source_stamp: r.source_stamp.clone(),
    };
    let mut doc = Document::new(SourceType::Memory, &uri, &title, &text);
    doc.meta = serde_json::json!({ "memory": MemoryMeta { facts, text: text.clone() } });

    let mut quiet_cfg = cfg.clone();
    quiet_cfg.embed.contextual = false;
    quiet_cfg.embed.chunk_tokens = Config::default().embed.chunk_tokens;
    quiet_cfg.embed.dimensions = dims;

    let _lock = acquire_lock(root)?;
    let store = Store::open(&store_dir(root), dims)?;
    let idx = Indexer::new(store, embedder, quiet_cfg)
        .silent()
        .without_yielding_to_queries();
    idx.index_documents(std::slice::from_ref(&doc))?;
    if let Some(prev) = &replaced {
        idx.store().delete_document(&prev.doc_id)?;
    }
    let net_growth = if replaced.is_some() { 0 } else { 1 };
    prune_over_cap(idx.store(), root, cfg.memory.max_memories, net_growth)?;
    publish_pack(idx.store(), root, dims)?;
    Ok(match replaced {
        Some(prev) => Outcome::Replaced {
            id,
            previous: prev.id,
            previous_title: prev.title,
        },
        None => Outcome::Saved { id },
    })
}

fn session_id(session_uri: &str) -> Option<String> {
    use crate::loaders::transcript::SessionAgent;
    let stem = SessionAgent::path_of(session_uri)
        .unwrap_or(Path::new(session_uri))
        .file_stem()?
        .to_str()?;
    Some(match SessionAgent::from_uri(session_uri) {
        Some(SessionAgent::Codex) => crate::loaders::codex::id_from_file_stem(stem),
        _ => stem.to_string(),
    })
}

fn acquire_lock(root: &Path) -> Result<IndexLock> {
    std::fs::create_dir_all(root)?;
    let db = store_dir(root);
    for _ in 0..LOCK_RETRIES {
        if let Some(lock) = IndexLock::acquire(&db) {
            return Ok(lock);
        }
        std::thread::sleep(LOCK_RETRY);
    }
    Err(MemoryBusy.into())
}

fn nearest_of_kind(
    root: &Path,
    model_id: &str,
    dims: usize,
    embedder: &dyn Embedder,
    text: &str,
    kind: MemoryKind,
    min_similarity: f32,
) -> Result<Option<Memory>> {
    let Some(pack) = crate::pack::open_pack_beside(&pack_dir(root), model_id, dims)? else {
        return Ok(None);
    };
    if !pack.has_vectors() {
        return Ok(None);
    }
    let qv = embedder.embed_query(text)?;
    for (rec, sim) in pack.search(&qv, DEDUPE_K, DEDUPE_EFS)? {
        let Some(facts) = &rec.memory else { continue };
        if facts.kind != kind || sim < min_similarity {
            continue;
        }
        return Ok(Some(memory_from_record(&rec)));
    }
    Ok(None)
}

fn prune_over_cap(store: &Store, root: &Path, max: usize, net_growth: usize) -> Result<()> {
    let all = list_at(root, &Filter::default())?;
    let projected = all.len() + net_growth;
    if projected <= max {
        return Ok(());
    }
    let excess = projected - max;
    let mut episodes: Vec<Memory> = all
        .into_iter()
        .filter(|m| m.facts.kind == MemoryKind::Episode)
        .collect();
    episodes.sort_by_key(|m| m.facts.created);
    for m in episodes.into_iter().take(excess) {
        store.delete_document(&m.doc_id)?;
    }
    Ok(())
}

#[cfg(test)]
static PACK_PUBLISHES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn publish_pack(store: &Store, root: &Path, dims: usize) -> Result<()> {
    #[cfg(test)]
    PACK_PUBLISHES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let model_id = store.get_meta("embed_model")?.unwrap_or_default();
    let rows = store.all_rows_for_pack()?;
    let live = pack_dir(root);
    if rows.is_empty() {
        let _ = std::fs::remove_dir_all(&live);
        return Ok(());
    }
    let shadow = root.join("pack.new");
    let _ = std::fs::remove_dir_all(&shadow);
    std::fs::create_dir_all(&shadow)?;
    Pack::build(
        &shadow,
        &model_id,
        dims,
        rows,
        &HashMap::new(),
        &HashMap::new(),
    )?;
    crate::index::publish_shadow(&live, &shadow)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryNotFound(pub String);

impl std::fmt::Display for MemoryNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no memory with id `{}`; run `br8n memory list`", self.0)
    }
}

impl std::error::Error for MemoryNotFound {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryAmbiguous {
    pub id: String,
    pub matches: usize,
}

impl std::fmt::Display for MemoryAmbiguous {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`{}` matches {} memories; give more of the id",
            self.id, self.matches
        )
    }
}

impl std::error::Error for MemoryAmbiguous {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryBusy;

impl std::fmt::Display for MemoryBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the memory store is busy (another br8n process is writing memories); try again"
        )
    }
}

impl std::error::Error for MemoryBusy {}

fn resolve_one(root: &Path, id: &str) -> Result<Memory> {
    resolve_among(&list_at(root, &Filter::default())?, id)
}

fn resolve_among(all: &[Memory], id: &str) -> Result<Memory> {
    let id = id.trim();
    if id.is_empty() {
        return Err(MemoryNotFound(String::new()).into());
    }
    let matches: Vec<&Memory> = all.iter().filter(|m| m.id.starts_with(id)).collect();
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(MemoryNotFound(id.to_string()).into()),
        many => Err(MemoryAmbiguous {
            id: id.to_string(),
            matches: many.len(),
        }
        .into()),
    }
}

pub fn forget(cfg: &Config, id: &str) -> Result<Memory> {
    forget_at(&default_root(), cfg, id)
}

pub fn forget_at(root: &Path, cfg: &Config, id: &str) -> Result<Memory> {
    if !cfg.memory.enabled {
        anyhow::bail!(DISABLED);
    }
    let target = resolve_one(root, id)?;
    let dims = cfg.embed.dimensions;
    let _lock = acquire_lock(root)?;
    let store = Store::open(&store_dir(root), dims)?;
    store.delete_document(&target.doc_id)?;
    publish_pack(&store, root, dims)?;
    Ok(target)
}

pub fn edit(cfg: &Config, old_id: &str, r: Remember) -> Result<Outcome> {
    let embedder = crate::embed::for_config(&cfg.embed)?;
    edit_at(&default_root(), cfg, embedder, old_id, r)
}

pub fn edit_at(
    root: &Path,
    cfg: &Config,
    embedder: Box<dyn Embedder>,
    old_id: &str,
    r: Remember,
) -> Result<Outcome> {
    let all = list_at(root, &Filter::default())?;
    let target = resolve_among(&all, old_id)?;
    let text = match validate_remember(cfg, &r) {
        Ok(text) => text,
        Err(reason) => return Ok(Outcome::Rejected(reason)),
    };

    let id = match r.kind {
        MemoryKind::Episode => r
            .session
            .as_deref()
            .or(target.facts.session.as_deref())
            .and_then(session_id)
            .unwrap_or_else(|| memory_id(r.kind, &text)),
        _ => memory_id(r.kind, &text),
    };
    let uri = uri_for(r.kind, &id);
    let doc_id = Document::new_id(&uri);
    if doc_id != target.doc_id {
        if let Some(collision) = all.iter().find(|m| m.doc_id == doc_id) {
            return Ok(Outcome::Duplicate {
                of: collision.id.clone(),
            });
        }
    }
    let dims = embedder.dimensions();

    let facts = MemoryFacts {
        kind: r.kind,
        created: r.created.unwrap_or(target.facts.created),
        project: r.project.as_ref().map(|p| p.display().to_string()),
        origin: r.origin,
        confidence: r.confidence,
        session: r.session.clone().or_else(|| target.facts.session.clone()),
        source_hash: r
            .source_hash
            .clone()
            .or_else(|| target.facts.source_hash.clone()),
        source_stamp: r
            .source_stamp
            .clone()
            .or_else(|| target.facts.source_stamp.clone()),
    };
    let title = r
        .title
        .clone()
        .unwrap_or_else(|| carried_title(&target, &text));
    let mut doc = Document::new(SourceType::Memory, &uri, &title, &text);
    doc.meta = serde_json::json!({ "memory": MemoryMeta { facts, text: text.clone() } });

    let mut quiet_cfg = cfg.clone();
    quiet_cfg.embed.contextual = false;
    quiet_cfg.embed.chunk_tokens = Config::default().embed.chunk_tokens;
    quiet_cfg.embed.dimensions = dims;

    let _lock = acquire_lock(root)?;
    let store = Store::open(&store_dir(root), dims)?;
    if target.doc_id == doc_id {
        rewrite_under_the_same_id(
            &store,
            embedder.as_ref(),
            quiet_cfg.embed.chunk_tokens,
            &doc,
        )?;
        publish_pack(&store, root, dims)?;
    } else {
        let idx = Indexer::new(store, embedder, quiet_cfg)
            .silent()
            .without_yielding_to_queries();
        idx.index_documents(std::slice::from_ref(&doc))?;
        idx.store().delete_document(&target.doc_id)?;
        publish_pack(idx.store(), root, dims)?;
    }
    Ok(Outcome::Replaced {
        id,
        previous: target.id,
        previous_title: target.title,
    })
}

fn carried_title(target: &Memory, new_text: &str) -> String {
    if target.title == summary_title(&target.text) {
        summary_title(new_text)
    } else {
        target.title.clone()
    }
}

fn rewrite_under_the_same_id(
    store: &Store,
    embedder: &dyn Embedder,
    chunk_tokens: usize,
    doc: &Document,
) -> Result<()> {
    let target = chunk_tokens.max(64);
    let chunker = crate::chunk::Chunker::new(target, target / 2);
    let chunks = chunker.chunk(doc);
    let reusable = store.embeddings_by_hash(&doc.id)?;
    let mut vecs: Vec<Vec<f32>> = Vec::with_capacity(chunks.len());
    let mut need: Vec<(usize, String)> = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        let hash = Document::content_hash(&c.embed_text);
        match reusable.get(&hash) {
            Some(v) => vecs.push(v.clone()),
            None => {
                vecs.push(Vec::new());
                need.push((i, c.embed_text.clone()));
            }
        }
    }
    if !need.is_empty() {
        let texts: Vec<String> = need.iter().map(|(_, t)| t.clone()).collect();
        let fresh = embedder.embed_documents(&texts)?;
        anyhow::ensure!(
            fresh.len() == need.len(),
            "embedding count mismatch: {} requested, {} returned",
            need.len(),
            fresh.len()
        );
        for ((i, _), v) in need.into_iter().zip(fresh) {
            vecs[i] = v;
        }
    }
    store.upsert_document(doc)?;
    store.replace_chunks(&doc.id, &chunks, &vecs)?;
    Ok(())
}

pub fn list(cfg: &Config, f: &Filter) -> Result<Vec<Memory>> {
    if !cfg.memory.enabled {
        return Ok(Vec::new());
    }
    list_at(&default_root(), f)
}

pub fn list_at(root: &Path, f: &Filter) -> Result<Vec<Memory>> {
    let dir = pack_dir(root);
    if !dir.join(records::REC_FILE).exists() {
        return Ok(Vec::new());
    }
    let reader = records::Reader::open(&dir)?;
    let mut out: Vec<Memory> = Vec::new();
    let mut seen: std::collections::HashSet<String> = Default::default();
    for row in 0..reader.len() {
        let rec = reader.get(row)?;
        if rec.memory.is_none() || !seen.insert(rec.doc_id.clone()) {
            continue;
        }
        let m = memory_from_record(&rec);
        if let Some(k) = f.kind {
            if m.facts.kind != k {
                continue;
            }
        }
        if let Some(p) = &f.project {
            if m.facts.project.as_deref() != Some(&p.display().to_string()) {
                continue;
            }
        }
        out.push(m);
    }
    out.sort_by(|a, b| {
        b.facts
            .created
            .cmp(&a.facts.created)
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(out)
}

fn memory_from_record(rec: &records::Record) -> Memory {
    Memory {
        id: id_from_uri(&rec.uri).unwrap_or_default().to_string(),
        uri: rec.uri.clone(),
        doc_id: rec.doc_id.clone(),
        title: rec.title.clone(),
        text: rec.text.clone(),
        facts: rec
            .memory
            .clone()
            .expect("memory_from_record is only called on memory rows"),
    }
}

pub fn counts_at(root: &Path) -> Result<HashMap<MemoryKind, usize>> {
    let mut out = HashMap::new();
    for m in list_at(root, &Filter::default())? {
        *out.entry(m.facts.kind).or_insert(0) += 1;
    }
    Ok(out)
}

pub fn open_pack(cfg: &Config, model_id: &str) -> Result<Option<Pack>> {
    if !cfg.memory.enabled {
        return Ok(None);
    }
    crate::pack::open_pack_beside(&pack_dir(&default_root()), model_id, cfg.embed.dimensions)
}

const LESSONS_HEADER: &str = "<br8n-lessons>\nCorrections this user has taught Claude. Follow them; they override defaults.\nIf the user says one no longer applies, call br8n_forget with its id.\n";
const LESSONS_FOOTER: &str = "</br8n-lessons>\n";

pub fn lessons_block(cfg: &Config, cwd: Option<&Path>) -> Result<Option<String>> {
    if !cfg.memory.enabled {
        return Ok(None);
    }
    lessons_block_at(&default_root(), &cfg.memory, cwd)
}

pub fn lessons_block_at(
    root: &Path,
    cfg: &crate::config::MemoryConfig,
    cwd: Option<&Path>,
) -> Result<Option<String>> {
    let all = list_at(
        root,
        &Filter {
            kind: Some(MemoryKind::Lesson),
            project: None,
        },
    )?;
    let applicable: Vec<Memory> = all
        .into_iter()
        .filter(|m| match (&m.facts.project, cwd) {
            (None, _) => true,
            (Some(p), Some(c)) => c.starts_with(Path::new(p)),
            (Some(_), None) => false,
        })
        .collect();
    Ok(render_lessons(&applicable, cfg.lessons_max_tokens))
}

pub fn render_lessons(lessons: &[Memory], max_tokens: usize) -> Option<String> {
    if lessons.is_empty() {
        return None;
    }
    let budget = max_tokens * 4;
    let mut ordered: Vec<&Memory> = lessons.iter().collect();
    ordered.sort_by(|a, b| {
        b.facts
            .created
            .cmp(&a.facts.created)
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut lines: Vec<String> = Vec::new();
    let mut used = LESSONS_HEADER.len() + LESSONS_FOOTER.len();
    for m in &ordered {
        let scope = m.facts.project.as_deref().unwrap_or("global");
        let line = format!(
            "- [{} · {} · {scope}] {}\n",
            m.id,
            ymd(m.facts.created),
            m.text.trim()
        );
        if used + line.len() > budget {
            break;
        }
        used += line.len();
        lines.push(line);
    }
    let omitted = ordered.len() - lines.len();
    let mut out = String::from(LESSONS_HEADER);
    for l in &lines {
        out.push_str(l);
    }
    if omitted > 0 {
        out.push_str(&format!(
            "({omitted} older lessons omitted; run `br8n memory list --kind lesson`)\n"
        ));
    }
    out.push_str(LESSONS_FOOTER);
    Some(out)
}

pub fn summary_title(text: &str) -> String {
    let first = text
        .trim()
        .split(['.', '\n', '!', '?'])
        .next()
        .unwrap_or("")
        .trim();
    let mut out: String = first.chars().take(TITLE_CHARS).collect();
    if first.chars().count() > TITLE_CHARS {
        out.push('…');
    }
    if out.is_empty() {
        "memory".to_string()
    } else {
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportRow {
    pub kind: MemoryKind,
    pub text: String,
    pub title: String,
    #[serde(default)]
    pub project: Option<String>,
    pub confidence: u8,
    pub origin: Origin,
    pub created: i64,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub source_hash: Option<String>,
    #[serde(default)]
    pub source_stamp: Option<String>,
}

pub fn export(cfg: &Config) -> Result<Vec<ExportRow>> {
    export_at(&default_root(), cfg.embed.dimensions)
}

pub fn export_at(root: &Path, dims: usize) -> Result<Vec<ExportRow>> {
    let dir = store_dir(root);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let store = Store::open_existing(&dir, dims)?;
    let mut out = Vec::new();
    for (_, title, meta) in store.all_documents_meta()? {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&meta) else {
            continue;
        };
        let Ok(m) = serde_json::from_value::<MemoryMeta>(v["memory"].clone()) else {
            continue;
        };
        out.push(ExportRow {
            kind: m.facts.kind,
            text: m.text,
            title,
            project: m.facts.project,
            confidence: m.facts.confidence,
            origin: m.facts.origin,
            created: m.facts.created,
            session: m.facts.session,
            source_hash: m.facts.source_hash,
            source_stamp: m.facts.source_stamp,
        });
    }
    out.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.text.cmp(&b.text)));
    Ok(out)
}

pub fn import(cfg: &Config, rows: &[ExportRow]) -> Result<(usize, usize)> {
    let embed = cfg.embed.clone();
    import_at(
        &default_root(),
        cfg,
        &move || crate::embed::for_config(&embed),
        rows,
    )
}

pub fn import_at(
    root: &Path,
    cfg: &Config,
    embedders: &dyn Fn() -> Result<Box<dyn Embedder>>,
    rows: &[ExportRow],
) -> Result<(usize, usize)> {
    let remembers: Vec<Remember> = rows
        .iter()
        .map(|r| Remember {
            kind: r.kind,
            text: r.text.clone(),
            title: Some(r.title.clone()),
            project: r.project.clone().map(PathBuf::from),
            confidence: r.confidence,
            origin: r.origin,
            session: r.session.clone(),
            source_hash: r.source_hash.clone(),
            source_stamp: r.source_stamp.clone(),
            created: Some(r.created),
        })
        .collect();
    remember_many_at(root, cfg, embedders, &remembers)
}

fn remember_many_at(
    root: &Path,
    cfg: &Config,
    embedders: &dyn Fn() -> Result<Box<dyn Embedder>>,
    rows: &[Remember],
) -> Result<(usize, usize)> {
    if rows.is_empty() {
        return Ok((0, 0));
    }
    let embedder = embedders()?;
    let dims = embedder.dimensions();

    let mut quiet_cfg = cfg.clone();
    quiet_cfg.embed.contextual = false;
    quiet_cfg.embed.chunk_tokens = Config::default().embed.chunk_tokens;
    quiet_cfg.embed.dimensions = dims;

    let existing = list_at(root, &Filter::default())?;
    let mut seen: std::collections::HashSet<(String, String)> =
        existing.into_iter().map(|m| (m.doc_id, m.text)).collect();

    let mut docs: Vec<Document> = Vec::new();
    let mut saved = 0usize;
    let mut skipped = 0usize;

    for r in rows {
        let text = match validate_remember(cfg, r) {
            Ok(text) => text,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let id = match r.kind {
            MemoryKind::Episode => r
                .session
                .as_deref()
                .and_then(session_id)
                .unwrap_or_else(|| memory_id(r.kind, &text)),
            _ => memory_id(r.kind, &text),
        };
        let uri = uri_for(r.kind, &id);
        let doc_id = Document::new_id(&uri);
        if !seen.insert((doc_id, text.clone())) {
            skipped += 1;
            continue;
        }

        let title = r.title.clone().unwrap_or_else(|| summary_title(&text));
        let facts = MemoryFacts {
            kind: r.kind,
            created: r.created.unwrap_or_else(now_secs),
            project: r.project.as_ref().map(|p| p.display().to_string()),
            origin: r.origin,
            confidence: r.confidence,
            session: r.session.clone(),
            source_hash: r.source_hash.clone(),
            source_stamp: r.source_stamp.clone(),
        };
        let mut doc = Document::new(SourceType::Memory, &uri, &title, &text);
        doc.meta = serde_json::json!({ "memory": MemoryMeta { facts, text: text.clone() } });
        docs.push(doc);
        saved += 1;
    }

    if docs.is_empty() {
        return Ok((0, skipped));
    }

    let _lock = acquire_lock(root)?;
    let store = Store::open(&store_dir(root), dims)?;
    let idx = Indexer::new(store, embedder, quiet_cfg).silent();
    idx.index_documents(&docs)?;
    prune_over_cap(idx.store(), root, cfg.memory.max_memories, saved)?;
    publish_pack(idx.store(), root, dims)?;
    Ok((saved, skipped))
}

pub fn rebuild(cfg: &Config) -> Result<usize> {
    let embed = cfg.embed.clone();
    rebuild_at(&default_root(), cfg, &move || {
        crate::embed::for_config(&embed)
    })
}

pub fn rebuild_at(
    root: &Path,
    cfg: &Config,
    embedders: &dyn Fn() -> Result<Box<dyn Embedder>>,
) -> Result<usize> {
    let _lock = acquire_lock(root)?;
    let rows = export_at(root, cfg.embed.dimensions)?;
    let fresh = root.join("fresh");
    let _ = std::fs::remove_dir_all(&fresh);
    let (saved, skipped) = import_at(&fresh, cfg, embedders, &rows)?;
    if !store_dir(&fresh).exists() {
        let _ = std::fs::remove_dir_all(&fresh);
        return Ok(saved);
    }
    anyhow::ensure!(
        skipped == 0,
        "rebuild refused: {skipped} of {} memories would be dropped (below [memory] \
         min_confidence, or otherwise invalid); the live memory store is untouched. Lower \
         min_confidence or fix the rejected memories, then run `br8n memory rebuild` again.",
        rows.len()
    );
    let fresh_count =
        Store::open_existing(&store_dir(&fresh), cfg.embed.dimensions)?.count_documents()?;
    anyhow::ensure!(
        fresh_count as usize == rows.len(),
        "rebuild refused: the fresh memory store holds {fresh_count} memories, not the {} that \
         were exported; the live memory store is untouched.",
        rows.len()
    );
    crate::index::publish_shadow(&store_dir(root), &store_dir(&fresh))?;
    if pack_dir(&fresh).exists() {
        crate::index::publish_shadow(&pack_dir(root), &pack_dir(&fresh))?;
    } else {
        let _ = std::fs::remove_dir_all(pack_dir(root));
    }
    let _ = std::fs::remove_dir_all(&fresh);
    Ok(saved)
}

pub fn count_at(root: &Path, dims: usize) -> Result<i64> {
    let dir = store_dir(root);
    if !dir.exists() {
        return Ok(0);
    }
    Store::open_existing(&dir, dims)?.count_documents()
}

#[cfg(test)]
mod tests {
    use super::*;

    static PUBLISH_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct CountingEmbedder;
    impl Embedder for CountingEmbedder {
        fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|t| fake_vec(t)).collect())
        }
        fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
            Ok(fake_vec(text))
        }
        fn warm(&self) -> Result<()> {
            Ok(())
        }
        fn model_id(&self) -> String {
            "fake@4".into()
        }
        fn dimensions(&self) -> usize {
            4
        }
    }

    fn fake_vec(s: &str) -> Vec<f32> {
        let b = s.as_bytes();
        let v: Vec<f32> = (0..4)
            .map(|i| b.iter().skip(i).step_by(4).map(|x| *x as f32).sum())
            .collect();
        crate::embed::normalize(v)
    }

    fn cfg4() -> Config {
        let mut c = Config::default();
        c.embed.dimensions = 4;
        c
    }

    fn row(i: i64) -> ExportRow {
        ExportRow {
            kind: MemoryKind::Fact,
            text: format!("Fact number {i} is distinct enough to embed on its own."),
            title: format!("Fact {i}"),
            project: None,
            confidence: 100,
            origin: Origin::User,
            created: 1_700_000_000 + i,
            session: None,
            source_hash: None,
            source_stamp: Some(format!("stamp-{i}")),
        }
    }

    #[test]
    fn importing_many_rows_publishes_the_pack_exactly_once() {
        let _guard = PUBLISH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("memory");
        let rows: Vec<ExportRow> = (0..12).map(row).collect();

        let before = PACK_PUBLISHES.load(std::sync::atomic::Ordering::SeqCst);
        let (saved, skipped) =
            import_at(&root, &cfg4(), &|| Ok(Box::new(CountingEmbedder)), &rows).unwrap();
        let after = PACK_PUBLISHES.load(std::sync::atomic::Ordering::SeqCst);

        assert_eq!((saved, skipped), (12, 0));
        assert_eq!(
            after - before,
            1,
            "importing 12 rows must publish the pack exactly once"
        );

        let listed = list_at(&root, &Filter::default()).unwrap();
        assert_eq!(listed.len(), 12);
        for i in 0..12 {
            let text = format!("Fact number {i} is distinct enough to embed on its own.");
            let m = listed.iter().find(|m| m.text == text).unwrap();
            assert_eq!(m.facts.kind, MemoryKind::Fact);
            assert_eq!(m.facts.created, 1_700_000_000 + i);
            assert_eq!(
                m.facts.source_stamp.as_deref(),
                Some(format!("stamp-{i}").as_str())
            );
        }
    }

    #[test]
    fn an_exact_duplicate_within_one_batch_is_written_once() {
        let _guard = PUBLISH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("memory");
        let repeated = Remember {
            kind: MemoryKind::Fact,
            text: "The vault lives at ~/notes/vault and nowhere else.".into(),
            title: None,
            project: None,
            confidence: 100,
            origin: Origin::User,
            session: None,
            source_hash: None,
            source_stamp: None,
            created: Some(1_700_000_000),
        };
        let rows = vec![repeated.clone(), repeated];

        let (saved, skipped) =
            remember_many_at(&root, &cfg4(), &|| Ok(Box::new(CountingEmbedder)), &rows).unwrap();
        assert_eq!((saved, skipped), (1, 1));

        let listed = list_at(&root, &Filter::default()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].text,
            "The vault lives at ~/notes/vault and nowhere else."
        );
    }
}
