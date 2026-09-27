// Each integration test file is compiled as its own crate with `mod common;`
// pulled in fresh, and no single test file calls every helper here — e.g.
// `retrieve_profile.rs` never calls `setup()`, and the other files never call
// `retriever_with_corpus`/`empty_retriever`. That makes a subset of these
// `pub fn`s legitimately unused from the point of view of any one binary;
// `dead_code` would otherwise flag them per-binary depending on which test
// file happens to link this module in.
#![allow(dead_code)]

use br8n::config::Config;
use br8n::embed::Embedder;
use br8n::index::Indexer;
use br8n::model::{Document, SourceType};
use br8n::retrieve::Retriever;
use br8n::store::Store;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

pub static ENV_VAR_LOCK: Mutex<()> = Mutex::new(());

pub fn lock_env_vars() -> MutexGuard<'static, ()> {
    match ENV_VAR_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

pub struct EnvVarGuard {
    key: &'static str,
    prior: Option<OsString>,
}

impl EnvVarGuard {
    pub fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let prior = std::env::var_os(key);
        std::env::set_var(key, value);
        EnvVarGuard { key, prior }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.prior.take() {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

/// A minimal Ollama /api/embed stand-in.
///
/// These tests spawn the real binary, and the real binary embeds through
/// HTTP. They used to point at localhost:11434 — which exists on a developer
/// machine and not on CI, so every one of them died there with "tcp connect
/// error" while the ci.yml header claimed no test reaches the network. What
/// they actually verify is filesystem orchestration (the writer lock, the
/// shadow swap, the stat-skip); the embeddings are scaffolding, so a stub
/// that returns a fixed unit-ish vector per input serves them fully.
///
/// One request per connection, `Connection: close`, so reqwest cannot pipeline
/// a second request into a stream this loop has finished with.
pub fn fake_ollama() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind fake ollama");
    let addr = listener.local_addr().expect("fake ollama addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut received: Vec<u8> = Vec::new();
            let mut buf = [0u8; 8192];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        received.extend_from_slice(&buf[..n]);
                        if let Some(end) = received.windows(4).position(|w| w == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&received[..end]);
                            let want: usize = headers
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|v| v.trim().to_string())
                                })
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(0);
                            if received.len() - (end + 4) >= want {
                                break;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            let inputs = received
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|end| &received[end + 4..])
                .and_then(|body| serde_json::from_slice::<serde_json::Value>(body).ok())
                .and_then(|v| v["input"].as_array().map(|a| a.len()))
                .unwrap_or(1);
            // 512 dims to match the default config; non-zero so normalize works.
            let one: String = format!("[{}]", vec!["0.044"; 512].join(","));
            let body = format!(
                r#"{{"embeddings":[{}]}}"#,
                vec![one.as_str(); inputs.max(1)].join(",")
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    format!("http://{addr}")
}

/// `fake_ollama`, but every response is held for `delay` before it is written.
///
/// Exists to force a `Profile::budget_ms` deadline deterministically: the
/// pipeline's clock (`retrieve::run`'s `t0`) starts before the query embed
/// call, so a delay here longer than the tier's budget guarantees
/// `StageReport::degraded` without racing real hardware timing. `delay` must
/// stay under the embed HTTP clients' own timeouts (`query_client` 4000ms,
/// `client` 120s in `embed/ollama.rs`) or the call fails outright instead of
/// merely running late.
pub fn fake_ollama_delayed(delay: std::time::Duration) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind fake ollama");
    let addr = listener.local_addr().expect("fake ollama addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut received: Vec<u8> = Vec::new();
            let mut buf = [0u8; 8192];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        received.extend_from_slice(&buf[..n]);
                        if let Some(end) = received.windows(4).position(|w| w == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&received[..end]);
                            let want: usize = headers
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|v| v.trim().to_string())
                                })
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(0);
                            if received.len() - (end + 4) >= want {
                                break;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            let inputs = received
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|end| &received[end + 4..])
                .and_then(|body| serde_json::from_slice::<serde_json::Value>(body).ok())
                .and_then(|v| v["input"].as_array().map(|a| a.len()))
                .unwrap_or(1);
            let one: String = format!("[{}]", vec!["0.044"; 512].join(","));
            let body = format!(
                r#"{{"embeddings":[{}]}}"#,
                vec![one.as_str(); inputs.max(1)].join(",")
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            std::thread::sleep(delay);
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    format!("http://{addr}")
}

pub fn fake_ollama_generate(response: &'static str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind fake ollama");
    let addr = listener.local_addr().expect("fake ollama addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut received: Vec<u8> = Vec::new();
            let mut buf = [0u8; 8192];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        received.extend_from_slice(&buf[..n]);
                        if let Some(end) = received.windows(4).position(|w| w == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&received[..end]);
                            let want: usize = headers
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|v| v.trim().to_string())
                                })
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(0);
                            if received.len() - (end + 4) >= want {
                                break;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            let head = String::from_utf8_lossy(&received);
            let body = if head.starts_with("POST /api/generate") {
                serde_json::json!({ "response": response }).to_string()
            } else {
                let inputs = received
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|end| &received[end + 4..])
                    .and_then(|body| serde_json::from_slice::<serde_json::Value>(body).ok())
                    .and_then(|v| v["input"].as_array().map(|a| a.len()))
                    .unwrap_or(1);
                let one: String = format!("[{}]", vec!["0.044"; 512].join(","));
                format!(
                    r#"{{"embeddings":[{}]}}"#,
                    vec![one.as_str(); inputs.max(1)].join(",")
                )
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    format!("http://{addr}")
}

/// Deterministic stand-in so indexing tests never need Ollama. Counts calls to
/// `embed_documents` so tests can prove the skip path never re-embeds.
#[derive(Default)]
pub struct FakeEmbedder {
    pub calls: Arc<AtomicUsize>,
    pub queries: Arc<AtomicUsize>,
}
impl Embedder for FakeEmbedder {
    fn embed_documents(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(texts.iter().map(|t| hash_vec(t)).collect())
    }
    fn embed_query(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        Ok(hash_vec(text))
    }
    fn warm(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn model_id(&self) -> String {
        "fake@4".into()
    }
    fn dimensions(&self) -> usize {
        4
    }
}

pub fn hash_vec(s: &str) -> Vec<f32> {
    let b = s.as_bytes();
    let v: Vec<f32> = (0..4)
        .map(|i| b.iter().skip(i).step_by(4).map(|x| *x as f32).sum())
        .collect();
    br8n::embed::normalize(v)
}

pub fn setup() -> (tempfile::TempDir, Indexer, Arc<AtomicUsize>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), 4).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let idx = Indexer::new(
        store,
        Box::new(FakeEmbedder {
            calls: calls.clone(),
            queries: Arc::new(AtomicUsize::new(0)),
        }),
        Config::default(),
    );
    (dir, idx, calls)
}

/// `setup`, but with configured source roots.
///
/// Any test touching `prune_missing` needs these: pruning only ever deletes
/// documents discovery could have enumerated, which means files under a
/// configured root. A `Config::default()` fixture has no roots at all, so it
/// cannot distinguish "tracked file deleted" from "web clipping added by hand"
/// — the exact distinction the prune logic turns on.
pub fn setup_with_sources(
    sources: Vec<std::path::PathBuf>,
) -> (tempfile::TempDir, Indexer, Arc<AtomicUsize>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), 4).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let cfg = Config {
        sources,
        ..Config::default()
    };
    let idx = Indexer::new(
        store,
        Box::new(FakeEmbedder {
            calls: calls.clone(),
            queries: Arc::new(AtomicUsize::new(0)),
        }),
        cfg,
    );
    (dir, idx, calls)
}

/// A canonical source root plus the `file://` URI of a file inside it.
/// macOS tempdirs live under a symlink (`/var` -> `/private/var`), so the root
/// must be canonicalized before it can prefix-match a document URI.
pub fn rooted(dir: &tempfile::TempDir, name: &str) -> (std::path::PathBuf, String) {
    let root = dir.path().canonicalize().unwrap();
    let uri = format!("file://{}/{}", root.display(), name);
    (root, uri)
}

/// The two-document corpus from Task 15 (`retrieve_primitives.rs`'s `seeded()`),
/// indexed into a fresh store and handed back as a `Retriever`. `Indexer` owns
/// its `Store` outright (no way to hand it back), so indexing happens through a
/// throwaway `Indexer` against the on-disk store, then a second `Store::open`
/// on the same directory hands a fresh handle to the `Retriever` once the first
/// connection has been dropped.
///
/// Carries a pack built from the same rows, attached with `with_pack`. BM25
/// moved to the pack in schema version 3 — `Store::fts_search` always errors
/// now (see its doc comment) — so a store-only retriever could never run the
/// `bm25` stage this fixture's callers gate on. A real index always publishes
/// a pack alongside the store (`reindex_swap` builds one every run), so
/// attaching one here matches production; the store-fallback (no-pack) path
/// this loses is exercised deliberately elsewhere by fixtures built without
/// one, e.g. `Store::fts_search`'s own tests in `retrieve_primitives.rs`.
pub fn retriever_with_corpus() -> (tempfile::TempDir, Retriever) {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = Store::open(dir.path(), 4).unwrap();
        let idx = Indexer::new(
            store,
            Box::new(FakeEmbedder {
                calls: Arc::new(AtomicUsize::new(0)),
                queries: Arc::new(AtomicUsize::new(0)),
            }),
            Config::default(),
        );
        let docs = vec![
            Document::new(
                SourceType::Markdown,
                "file:///a.md",
                "Pooling",
                "# Pooling\n\nPgBouncer runs in transaction mode and drops session state.",
            ),
            Document::new(
                SourceType::Markdown,
                "file:///b.md",
                "Baking",
                "# Baking\n\nSourdough needs a long cold ferment for flavour.",
            ),
        ];
        idx.index_documents(&docs).unwrap();
    }
    let store = Store::open(dir.path(), 4).unwrap();
    let rows = store.all_rows_for_pack().unwrap();
    let pack_dir = dir.path().join("pack");
    std::fs::create_dir_all(&pack_dir).unwrap();
    br8n::pack::Pack::build(
        &pack_dir,
        "fake@4",
        4,
        rows,
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    let pack = br8n::pack::Pack::open(&pack_dir, "fake@4", 4).unwrap();
    let embedder = Box::new(FakeEmbedder {
        calls: Arc::new(AtomicUsize::new(0)),
        queries: Arc::new(AtomicUsize::new(0)),
    });
    let retriever =
        Retriever::new(store, embedder, "http://localhost:11434".into()).with_pack(Some(pack));
    (dir, retriever)
}

/// A `Retriever` over a store that has never been indexed into. Exercises the
/// "nothing indexed yet" path every retrieval primitive degrades to.
pub fn empty_retriever() -> (tempfile::TempDir, Retriever) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), 4).unwrap();
    let pack_dir = dir.path().join("pack");
    std::fs::create_dir_all(&pack_dir).unwrap();
    br8n::pack::Pack::build(
        &pack_dir,
        "fake@4",
        4,
        Vec::new(),
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    let pack = br8n::pack::Pack::open(&pack_dir, "fake@4", 4).unwrap();
    let embedder = Box::new(FakeEmbedder {
        calls: Arc::new(AtomicUsize::new(0)),
        queries: Arc::new(AtomicUsize::new(0)),
    });
    let retriever =
        Retriever::new(store, embedder, "http://localhost:11434".into()).with_pack(Some(pack));
    (dir, retriever)
}
