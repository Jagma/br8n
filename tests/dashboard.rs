mod common;
use std::io::{Read, Write};
use std::sync::{Mutex, OnceLock};

/// Guards the process-global `BR8N_DB` env var for the duration of any
/// request that depends on it resolving to a particular database.
///
/// `Config::db_path()` (and so `Store::open_existing`) reads `BR8N_DB`
/// fresh on every request, from whichever server thread happens to be
/// handling it — see the doc comment on `server_over_temp_corpus`. That is
/// fine as long as the value never changes. The store-unopenable test below
/// has to change it, if only for the one request it's testing, so every test
/// in this file that issues a request depending on `BR8N_DB` takes this
/// lock first — otherwise a request from one test could resolve against the
/// database another test just pointed the env var at.
static ENV_GUARD: Mutex<()> = Mutex::new(());

fn get(addr: std::net::SocketAddr, path: &str) -> (u16, String) {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut buf = String::new();
    s.read_to_string(&mut buf).unwrap();
    let status: u16 = buf.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = buf.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

/// A server over a fresh temp corpus, shared by every test in this file.
///
/// `BR8N_DB` (and, since Task 5, `BR8N_CONFIG`) are process-global env
/// vars, so two independently-initialized fixtures in one test binary would
/// race to set them out from under each other (tests run on separate threads
/// by default). Worse than a simple race: `/api/graph` and `/api/search`
/// resolve their store path via `Config::db_path()` at REQUEST time, not at
/// server-start time — so even a server built from its own `Config` would
/// still answer with whatever database `BR8N_DB` currently names, process-
/// wide. Two live servers pointed at two different databases cannot both be
/// correct while they share one env var. So there is exactly one server:
/// a single `OnceLock` builds one corpus, sets the env vars once, and every
/// test in this file — old and new — talks to the same instance.
///
/// The corpus is indexed through the real `OllamaEmbedder` against
/// `common::fake_ollama()` rather than `common::FakeEmbedder`, so that
/// `/api/search` (Task 5) can embed its query through the same stub the
/// documents were embedded with, no live model and no network. The `TempDir`
/// lives inside the `OnceLock` for the rest of the process; it is never
/// dropped, but that only leaks one temp directory per test binary run, and
/// the server thread itself already lives for the process.
fn server_over_temp_corpus() -> std::net::SocketAddr {
    static SERVER: OnceLock<(tempfile::TempDir, std::net::SocketAddr)> = OnceLock::new();
    let (_dir, addr) = SERVER.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db");
        let ollama = common::fake_ollama();
        let cfg_path = dir.path().join("config.toml");
        std::fs::write(&cfg_path, format!("[embed]\nollama_url = \"{ollama}\"\n")).unwrap();
        // SAFETY: this OnceLock guarantees these env vars are set exactly
        // once for the whole process, before any test reads them.
        unsafe { std::env::set_var("BR8N_CONFIG", &cfg_path) };
        unsafe { std::env::set_var("BR8N_DB", &db) };
        let cfg = br8n::config::Config::load();
        {
            let store = br8n::store::Store::open(&db, cfg.embed.dimensions).unwrap();
            let idx = br8n::index::Indexer::new(
                store,
                Box::new(br8n::embed::OllamaEmbedder::new(&cfg.embed)),
                cfg.clone(),
            );
            let doc = br8n::model::Document::new(
                br8n::model::SourceType::Markdown,
                "file:///n/a.md",
                "Alpha",
                "# Alpha\n\n## Pooling\n\nPgBouncer runs in transaction mode.",
            );
            idx.index_documents(std::slice::from_ref(&doc)).unwrap();
        }

        {
            let model_id =
                br8n::embed::Embedder::model_id(&br8n::embed::OllamaEmbedder::new(&cfg.embed));
            let store = br8n::store::Store::open_existing(&db, cfg.embed.dimensions).unwrap();
            let rows = store.all_rows_for_pack().unwrap();
            br8n::pack::Pack::build(
                &db,
                &model_id,
                cfg.embed.dimensions,
                rows,
                &Default::default(),
                &Default::default(),
            )
            .unwrap();
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let source = br8n::dashboard::ConfigSource::File(cfg_path);
        let agents = agent_env(dir.path());
        std::thread::spawn(move || {
            br8n::dashboard::serve_on_with_agents(source, listener, Some(agents))
        });
        (dir, addr)
    });
    *addr
}

#[test]
fn stats_reports_the_real_index_and_unknown_api_paths_404() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let (status, body) = get(addr, "/api/stats");
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["documents"], 1);
    assert!(v["chunks"].as_u64().unwrap() >= 1);
    assert_eq!(v["by_source"]["markdown"], 1);

    let (status, _) = get(addr, "/api/nope");
    assert_eq!(status, 404);

    // Unknown non-API paths serve the SPA shell (client-side routing).
    let (status, body) = get(addr, "/anything");
    assert_eq!(status, 200);
    assert!(body.contains("<"), "must serve HTML");
}

#[test]
fn progress_is_idle_when_no_index_runs() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let (status, body) = get(addr, "/api/progress");
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["idle"],
        true
    );
}

#[test]
fn graph_endpoint_serves_the_snapshot() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let (status, body) = get(addr, "/api/graph");
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(v["nodes"][0]["title"], "Alpha");
}

#[test]
fn search_endpoint_explains_the_pipeline() {
    // The server embeds through HTTP like the real binary, so the corpus and
    // the query go through the fake Ollama — no network, no live model.
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let (status, body) = get(addr, "/api/search?q=pgbouncer%20transaction&tier=1");
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let stages: Vec<&str> = v["stages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(stages.contains(&"vector"), "stages were {stages:?}");
    assert!(v["threshold"].as_f64().unwrap() > 0.0);
    assert!(v["fused"].as_array().is_some());
}

#[test]
fn every_search_hit_names_the_section_it_came_from_and_bm25_keeps_its_own_score() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let (status, body) = get(addr, "/api/search?q=pgbouncer%20transaction&tier=1");
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let stage = |name: &str| -> Vec<serde_json::Value> {
        v["stages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == name)
            .and_then(|s| s["hits"].as_array().cloned())
            .unwrap_or_default()
    };
    let every_hit: Vec<serde_json::Value> = v["stages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["hits"].as_array().cloned().unwrap_or_default())
        .chain(v["fused"].as_array().cloned().unwrap_or_default())
        .collect();
    assert!(
        !every_hit.is_empty(),
        "the query must find the one note: {v}"
    );
    assert!(
        every_hit.iter().all(|h| h["heading"] == "Pooling"),
        "every hit must carry its chunk's heading path, not only the document title: {v}"
    );
    let bm25 = stage("bm25");
    assert!(!bm25.is_empty(), "tier 1 runs BM25: {v}");
    assert!(
        bm25.iter()
            .all(|h| h["score"].as_f64().unwrap() > 0.0 && h["relevance"] == 0.0),
        "a BM25 hit carries its own positive score and an unmeasured relevance: {v}"
    );
}

#[test]
fn search_endpoint_returns_200_with_an_error_payload_when_ollama_is_unreachable() {
    // Ollama down is an expected runtime state, not a server failure: the
    // client must be able to show a banner on the search tab while stats,
    // graph and progress keep working elsewhere — which means 200, not 503.
    //
    // This reuses the shared corpus's database (`BR8N_DB` is fixed for the
    // whole binary — see `server_over_temp_corpus`) but serves it from a
    // second, independent listener whose `Config` points `ollama_url` at a
    // closed port. `search()` resolves the store via the env var (shared,
    // untouched here) but embeds through whichever `Config` its own server
    // thread was started with, so this does not race the shared server's
    // `BR8N_CONFIG`.
    server_over_temp_corpus(); // ensure BR8N_DB is set and the corpus exists
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let dead = {
        // Bind then drop: the port is free but nothing answers on it, so a
        // connection to it fails immediately instead of timing out.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let mut cfg = br8n::config::Config::load();
    cfg.embed.ollama_url = format!("http://{dead}");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || br8n::dashboard::serve_on(cfg, listener));

    let (status, body) = get(addr, "/api/search?q=pgbouncer&tier=0");
    assert_eq!(status, 200, "ollama-down must not surface as an HTTP error");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"], "embedding unavailable");
}

/// The boundary between the two tests around this one. A fresh `br8n index`
/// run over zero documents is a real state (a new install, or a corpus whose
/// every document was skipped) — it still publishes a pack, with zero rows —
/// so the search endpoint must answer with an empty pipeline, not an error:
/// neither a 503 nor an embedding banner, but a successful search that found
/// nothing.
#[test]
fn a_fresh_empty_index_is_an_empty_answer_not_an_outage() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let good_db = std::env::var("BR8N_DB").unwrap();
    let cfg = br8n::config::Config::load();
    let model_id = br8n::embed::Embedder::model_id(&br8n::embed::OllamaEmbedder::new(&cfg.embed));
    let empty_db = tempfile::tempdir().unwrap();
    br8n::pack::Pack::build(
        empty_db.path(),
        &model_id,
        cfg.embed.dimensions,
        Vec::new(),
        &Default::default(),
        &Default::default(),
    )
    .unwrap();

    // SAFETY: `_guard` holds ENV_GUARD for the whole window `BR8N_DB` names
    // this database, so no concurrent test resolves against it.
    unsafe { std::env::set_var("BR8N_DB", empty_db.path()) };
    let result = get(addr, "/api/search?q=pgbouncer%20transaction&tier=1");
    unsafe { std::env::set_var("BR8N_DB", &good_db) };

    let (status, body) = result;
    assert_eq!(
        status, 200,
        "an empty index is not a server failure: {body}"
    );
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        v["error"].is_null(),
        "no error payload — a fresh, empty pack must answer cleanly: {body}"
    );
    assert_eq!(v["fused"].as_array().unwrap().len(), 0);
    assert_eq!(v["injected"].as_array().unwrap().len(), 0);
}

#[test]
fn search_endpoint_returns_503_when_the_store_itself_is_unopenable() {
    // Proves the split Task 5's review fixed in `search()`: a store that
    // will not open (here: no index at the path `BR8N_DB` names — the same
    // failure `Store::open_existing` returns during the shadow-swap rename
    // window) must propagate to `json_result`'s 503 path, not get folded
    // into the 200 "embedding unavailable" banner that's reserved for
    // embedding/retrieval failure. Before the fix, `search()` chained
    // `retrieve_for` (which opens the store) and `search_explained` with a
    // single `.and_then`, so a store-open failure was misreported as
    // "embedding unavailable" too — a lie: Ollama was never asked anything.
    //
    // No separate server instance is needed here, unlike the
    // ollama-unreachable test above: `ollama_url` is bound into each
    // server's own `Config` at construction time, so testing it requires a
    // second server built with a different `Config`. The store path is not
    // — `Config::db_path()` re-reads the process-global `BR8N_DB` on every
    // request, regardless of which server's thread is handling it (see the
    // doc comment on `server_over_temp_corpus`) — so flipping `BR8N_DB` and
    // hitting the existing shared server's `addr` exercises the same code
    // path a second server would.
    let addr = server_over_temp_corpus(); // ensure BR8N_DB is set and the corpus exists
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let good_db = std::env::var("BR8N_DB").unwrap();
    let empty = tempfile::tempdir().unwrap();

    // SAFETY: `_guard` holds ENV_GUARD for the entire window `BR8N_DB`
    // names a path with no index, so no concurrently-running test's request
    // can be resolved against it instead of the real corpus.
    unsafe { std::env::set_var("BR8N_DB", empty.path()) };
    let result = get(addr, "/api/search?q=anything");
    unsafe { std::env::set_var("BR8N_DB", &good_db) };

    let (status, body) = result;
    assert_eq!(
        status, 503,
        "store-unopenable must not be reported as embedding-unavailable"
    );
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let error = v["error"].as_str().unwrap();
    assert!(
        error.contains("no index"),
        "expected the store's own error, got {error:?}"
    );
}

/// `/api/graph` reports the edge's real `kind`, not the literal "links_to".
///
/// `graph_snapshot` hardcoded that string, so every LINKS_TO edge looked like a
/// wikilink however it was written. This was the reader that had to be fixed, because the dashboard is the only way to verify
/// by inspection that typed edges exist at all.
#[test]
fn the_graph_api_reports_an_edges_real_kind_not_a_hardcoded_label() {
    let t = tempfile::tempdir().unwrap();
    let db = t.path().join("db");
    let store = br8n::store::Store::open(&db, 4).unwrap();

    for (id, title) in [("old", "Bazel"), ("new", "Nix")] {
        let mut d = br8n::model::Document::new(
            br8n::model::SourceType::Markdown,
            &format!("file:///{id}.md"),
            title,
            "body",
        );
        d.id = id.to_string();
        store.upsert_document(&d).unwrap();
    }
    store.link_documents("old", "new", "wikilink").unwrap();
    store.link_documents("old", "new", "superseded-by").unwrap();

    let snap = store.graph_snapshot().unwrap();
    let kinds: std::collections::HashSet<&str> =
        snap.edges.iter().map(|e| e.kind.as_str()).collect();

    assert!(
        kinds.contains("superseded-by"),
        "a typed edge must reach the dashboard under its own name; got {kinds:?}"
    );
    assert!(
        kinds.contains("wikilink"),
        "and a plain wikilink must say `wikilink`, not the table name; got {kinds:?}"
    );
    assert!(
        !kinds.contains("links_to"),
        "`links_to` is the TABLE, never a kind value — emitting it means the \
         hardcoded label came back; got {kinds:?}"
    );
}

/// The detail endpoint answers for a real document and reports a missing one
/// as a 200 payload, not as the 503 that means "retry".
#[test]
fn document_endpoint_serves_one_document_and_names_a_missing_one() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let (status, graph) = get(addr, "/api/graph");
    assert_eq!(status, 200);
    let g: serde_json::Value = serde_json::from_str(&graph).unwrap();
    let id = g["nodes"][0]["id"]
        .as_str()
        .expect("fixture has a document");

    let (status, body) = get(addr, &format!("/api/document?id={id}"));
    assert_eq!(status, 200);
    let d: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(d["id"].as_str(), Some(id));
    assert!(
        !d["uri"].as_str().unwrap().is_empty(),
        "uri is the field the panel exists to show"
    );
    // NOT `> 0`: a store-wide count is also > 0, so that version passes with
    // `document_detail`'s `WHERE d.id = $id` deleted. Pinned against the number
    // the graph payload already reports for this same document, which also
    // makes the two surfaces contradicting each other a test failure.
    assert_eq!(
        d["chunks"].as_u64(),
        g["nodes"][0]["chunks"].as_u64(),
        "the panel and the graph must report the same chunk count for one document"
    );

    // A document id that is not in the store is an ORDINARY client state (a
    // stale graph payload), so it must not take the 503 path — the SPA
    // retries a 503 and then tells the user the index is busy.
    let (status, body) = get(addr, "/api/document?id=no-such-document");
    assert_eq!(status, 200, "a missing document is not an outage");
    assert!(body.contains("unknown document"), "got: {body}");
}

/// `server_over_temp_corpus` indexes exactly one document, so a store-wide
/// chunk count and this-document's chunk count are numerically identical
/// there — the test above cannot, on its own, tell `document_detail`'s
/// chunks query apart from one with `WHERE d.id = $id` dropped (mutation-
/// tested: it does not fail when that clause is removed). This test builds
/// an isolated store with two documents carrying DIFFERENT chunk counts so
/// the two queries diverge and the WHERE clause has something to prove.
#[test]
fn document_detail_chunk_count_is_scoped_to_its_own_document() {
    let t = tempfile::tempdir().unwrap();
    let db = t.path().join("db");
    let store = br8n::store::Store::open(&db, 4).unwrap();

    for (id, title, n) in [("solo", "Solo", 1usize), ("busy", "Busy", 3usize)] {
        let mut d = br8n::model::Document::new(
            br8n::model::SourceType::Markdown,
            &format!("file:///{id}.md"),
            title,
            "body",
        );
        d.id = id.to_string();
        store.upsert_document(&d).unwrap();
        let chunks: Vec<_> = (0..n)
            .map(|ord| br8n::model::Chunk {
                id: br8n::model::Chunk::id(&d.id, ord as i64),
                doc_id: d.id.clone(),
                ord: ord as i64,
                text: format!("chunk {ord}"),
                embed_text: format!("chunk {ord}"),
                heading_path: String::new(),
                page_no: None,
            })
            .collect();
        let vecs = vec![Vec::new(); n];
        store.insert_chunks(&d.id, &chunks, &vecs).unwrap();
    }
    // One wikilink, `busy` -> `solo`, so the two documents disagree on
    // inbound count too — the same reasoning as the chunk fixture above
    // applies to `document_detail`'s inbound query: `solo`'s asserted 1 and
    // `busy`'s asserted 0 can only both hold if the query is actually scoped
    // by `WHERE d.id = $id`. Drop that clause and the single wikilink in the
    // store leaks into BOTH documents' counts identically (both would read
    // 1), which this test's two different expected values is what catches.
    store.link_documents("busy", "solo", "wikilink").unwrap();

    let solo = store.document_detail("solo").unwrap().expect("solo exists");
    assert_eq!(
        solo.chunks, 1,
        "must count only `solo`'s own chunk, not every chunk in the store"
    );
    assert_eq!(
        solo.inbound, 1,
        "must count only wikilinks that target `solo`, not every wikilink in the store"
    );
    let busy = store.document_detail("busy").unwrap().expect("busy exists");
    assert_eq!(busy.chunks, 3);
    assert_eq!(busy.inbound, 0, "`busy` has no inbound wikilink");
}

#[test]
fn stats_carry_memory_counts() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let (status, body) = get(addr, "/api/stats");
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v["memory"].is_object(), "{v}");
}

fn post(addr: std::net::SocketAddr, path: &str, origin: Option<&str>) -> (u16, String) {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    let origin = origin
        .map(|o| format!("Origin: {o}\r\n"))
        .unwrap_or_default();
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: x\r\n{origin}Content-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut buf = String::new();
    s.read_to_string(&mut buf).unwrap();
    let status: u16 = buf.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = buf.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

fn post_body(
    addr: std::net::SocketAddr,
    path: &str,
    origin: Option<&str>,
    body: &str,
) -> (u16, String) {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    let origin = origin
        .map(|o| format!("Origin: {o}\r\n"))
        .unwrap_or_default();
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: x\r\n{origin}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut buf = String::new();
    s.read_to_string(&mut buf).unwrap();
    let status: u16 = buf.split_whitespace().nth(1).unwrap().parse().unwrap();
    let out = buf.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, out)
}

fn root_of_shared_db() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("BR8N_DB").unwrap())
        .parent()
        .unwrap()
        .to_path_buf()
}

struct RemoveOnDrop(std::path::PathBuf);
impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn post_is_405_everywhere_but_the_update_route() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let (status, _) = post(addr, "/api/stats", None);
    assert_eq!(status, 405);
    let (status, _) = post(addr, "/api/search?q=x", None);
    assert_eq!(status, 405);
}

#[test]
fn update_is_refused_from_a_foreign_origin_under_an_index_lock_and_while_running() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let root = root_of_shared_db();
    let lock = std::path::PathBuf::from(std::env::var("BR8N_DB").unwrap()).with_extension("lock");
    std::fs::write(&lock, std::process::id().to_string()).unwrap();

    let (status, body) = post(addr, "/api/update", Some("http://evil.example"));
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("origin"), "{body}");

    let (status, body) = post(
        addr,
        "/api/update",
        Some(&format!("http://127.0.0.1:{}", addr.port())),
    );
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("index"), "{body}");
    std::fs::remove_file(&lock).unwrap();

    let live = serde_json::json!({ "pid": std::process::id(), "started_at": 0, "from": "1.0.0", "to": "2.0.0", "phase": "downloading", "message": "x", "done": false, "ok": null });
    std::fs::write(root.join("update.status"), live.to_string()).unwrap();
    let (status, body) = post(addr, "/api/update", None);
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("already running"), "{body}");

    let (status, body) = get(addr, "/api/version");
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["installed"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["update"]["phase"], "downloading");
    std::fs::remove_file(root.join("update.status")).unwrap();
}

#[test]
fn memories_endpoint_serves_the_pack_and_names_a_broken_one() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let (status, body) = get(addr, "/api/memories");
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v["memories"].is_array(), "{v}");
    assert!(
        v["unavailable"].is_null(),
        "an empty store is not an unavailable one: {v}"
    );

    let root = root_of_shared_db().join("memory");
    let pack = root.join("pack");
    std::fs::create_dir_all(&pack).unwrap();
    let _cleanup = RemoveOnDrop(pack.clone());
    std::fs::write(pack.join("pack.rec"), b"{}").unwrap();
    std::fs::write(pack.join("pack.recidx"), b"abc").unwrap();
    let (status, body) = get(addr, "/api/memories");
    assert_eq!(status, 200, "a broken pack must not fail the request");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        v["unavailable"]
            .as_str()
            .unwrap_or("")
            .contains("pack.recidx"),
        "a broken pack must be named, not read back as no memories: {v}"
    );
}

#[test]
fn version_reflects_the_cached_check() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let root = root_of_shared_db();
    br8n::update::UpdateCheck {
        installed: env!("CARGO_PKG_VERSION").into(),
        latest: Some("99.0.0".into()),
        url: Some("http://x/rel".into()),
        checked_at: 123,
        error: None,
    }
    .write(&root.join("update.json"))
    .unwrap();
    let (status, body) = get(addr, "/api/version");
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["latest"], "99.0.0");
    assert_eq!(v["update_available"], "99.0.0");
    assert_eq!(v["checked_at"], 123);
    assert!(v["update"].is_null());
    std::fs::remove_file(root.join("update.json")).unwrap();
}

#[test]
fn the_graph_carries_memory_nodes_and_their_real_edges() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let root = root_of_shared_db().join("memory");
    let _cleanup = RemoveOnDrop(root.clone());
    let cfg = br8n::config::Config::load();

    let mut scoped = br8n::memory::Remember {
        kind: br8n::memory::MemoryKind::Fact,
        text: "The notes vault lives under the home notes folder.".into(),
        title: None,
        project: Some(std::path::PathBuf::from("/Users/x/repo")),
        confidence: 100,
        origin: br8n::memory::Origin::User,
        session: None,
        source_hash: None,
        source_stamp: None,
        created: None,
    };
    br8n::memory::remember_at(
        &root,
        &cfg,
        Box::new(br8n::embed::OllamaEmbedder::new(&cfg.embed)),
        scoped.clone(),
    )
    .unwrap();
    scoped.kind = br8n::memory::MemoryKind::Episode;
    scoped.text = "Worked on the pack format and decided against a bump.".into();
    scoped.project = None;
    scoped.session = Some("claude-session:///tmp/sess-alpha.jsonl".into());
    br8n::memory::remember_at(
        &root,
        &cfg,
        Box::new(br8n::embed::OllamaEmbedder::new(&cfg.embed)),
        scoped,
    )
    .unwrap();

    let (status, body) = get(addr, "/api/graph");
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let nodes = v["nodes"].as_array().unwrap();
    let mems: Vec<&serde_json::Value> = nodes
        .iter()
        .filter(|n| n["source_type"] == "memory")
        .collect();
    assert_eq!(mems.len(), 2, "both memories must be nodes: {v}");
    assert!(mems.iter().any(|n| n["memory_kind"] == "fact"));
    assert!(mems.iter().any(|n| n["memory_kind"] == "episode"));
    let fact = mems
        .iter()
        .find(|n| n["memory_kind"] == "fact")
        .expect("the fact must be a node");
    let episode = mems
        .iter()
        .find(|n| n["memory_kind"] == "episode")
        .expect("the episode must be a node");
    assert!(
        fact["memory_id"]
            .as_str()
            .is_some_and(|s| s.len() == 12 && s.chars().all(|c| c.is_ascii_hexdigit())),
        "a fact's memory id is its content hash, not its doc id: {v}"
    );
    assert_eq!(
        episode["memory_id"], "sess-alpha",
        "an episode's memory id is its session stem, not its doc id: {v}"
    );
    assert!(
        mems.iter().all(|n| n["memory_id"] != n["id"]),
        "memory_id is the id the reach endpoint takes and must not be the doc id: {v}"
    );
    assert!(
        nodes
            .iter()
            .any(|n| n["source_type"] == "project" && n["title"] == "/Users/x/repo"),
        "a scoped memory gets a project cluster node: {v}"
    );

    let edges = v["edges"].as_array().unwrap();
    let want_session = br8n::model::Document::new_id("claude-session:///tmp/sess-alpha.jsonl");
    assert!(
        edges
            .iter()
            .any(|e| e["kind"] == "distilled-from" && e["to"] == want_session),
        "the episode must link to its session's node id: {v}"
    );
    assert!(
        edges
            .iter()
            .any(|e| e["kind"] == "scoped-to" && e["to"] == "project:/Users/x/repo"),
        "a scoped memory links to its project: {v}"
    );
    assert!(
        !edges.iter().any(|e| e["kind"] == "scoped-to"
            && e["from"] == mems.iter().find(|n| n["memory_kind"] == "episode").unwrap()["id"]),
        "a global memory has no project edge: {v}"
    );
}

#[test]
fn memory_reach_reports_what_one_memory_pulls_in() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let root = root_of_shared_db().join("memory");
    let _cleanup = RemoveOnDrop(root.clone());
    let cfg = br8n::config::Config::load();
    let out = br8n::memory::remember_at(
        &root,
        &cfg,
        Box::new(br8n::embed::OllamaEmbedder::new(&cfg.embed)),
        br8n::memory::Remember {
            kind: br8n::memory::MemoryKind::Fact,
            text: "PgBouncer runs in transaction mode in this deployment.".into(),
            title: None,
            project: None,
            confidence: 100,
            origin: br8n::memory::Origin::User,
            session: None,
            source_hash: None,
            source_stamp: None,
            created: None,
        },
    )
    .unwrap();
    let br8n::memory::Outcome::Saved { id } = out else {
        panic!("{out:?}")
    };

    let (status, body) = get(addr, &format!("/api/memory/reach?id={id}"));
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v["reach"].is_array(), "{v}");
    assert!(
        !v["reach"].as_array().unwrap().is_empty(),
        "the memory must actually reach the corpus, or every assertion below is vacuous: {v}"
    );
    assert!(
        v["reach"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["uri"] != "" && r["relevance"].is_number()),
        "each reached document names itself and its relevance: {v}"
    );
    assert!(
        v["reach"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| !r["uri"].as_str().unwrap_or("").starts_with("memory://")),
        "a memory's reach is what it pulls from the corpus, not itself: {v}"
    );

    let (status, body) = get(addr, "/api/memory/reach?id=000000000000");
    assert_eq!(
        status, 200,
        "an unknown id is an answer, not a server failure"
    );
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        v["error"].as_str().unwrap_or("").contains("no memory"),
        "{v}"
    );
}

#[test]
fn a_post_body_is_read_and_an_oversized_one_is_refused() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let (status, body) = post_body(addr, "/api/memory/delete", None, r#"{"id":"000000000000"}"#);
    assert_eq!(status, 404, "the body was read and the id resolved: {body}");
    assert!(body.contains("no memory"), "{body}");

    let huge = format!(r#"{{"id":"{}"}}"#, "x".repeat(70 * 1024));
    let (status, body) = post_body(addr, "/api/memory/delete", None, &huge);
    assert_eq!(
        status, 413,
        "an oversized body is refused, not buffered: {body}"
    );
}

#[test]
fn saving_a_memory_from_the_dashboard_creates_then_edits_it() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let root = root_of_shared_db().join("memory");
    let _cleanup = RemoveOnDrop(root.clone());

    let (status, body) = post_body(
        addr,
        "/api/memory/save",
        None,
        r#"{"kind":"lesson","text":"Never comment code unless asked.","scope":"global","confidence":100}"#,
    );
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = v["id"].as_str().expect("a save returns the id").to_string();

    let (status, body) = post_body(
        addr,
        "/api/memory/save",
        None,
        &format!(
            r#"{{"id":"{id}","kind":"lesson","text":"Never comment code unless the user asks.","scope":"global","confidence":90}}"#
        ),
    );
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let new_id = v["id"].as_str().unwrap().to_string();
    assert_ne!(
        new_id, id,
        "editing the text changes the content-addressed id"
    );

    let (_, body) = get(addr, "/api/memories");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let mems = v["memories"].as_array().unwrap();
    assert_eq!(mems.len(), 1, "an edit replaces rather than adding: {v}");
    assert!(mems[0]["text"].as_str().unwrap().contains("the user asks"));
    assert_eq!(mems[0]["facts"]["confidence"], 90);
    assert_eq!(
        mems[0]["facts"]["origin"], "user",
        "a memory written from the dashboard is the person's, not Claude's: {v}"
    );
    assert!(
        mems[0]["facts"]["project"].is_null(),
        "scope `global` means no project: {v}"
    );

    let (status, body) = post_body(
        addr,
        "/api/memory/save",
        None,
        r#"{"kind":"fact","text":"The staging database lives in eu-west-1.","scope":"/Users/x/repo","confidence":80}"#,
    );
    assert_eq!(status, 200, "{body}");
    let (_, body) = get(addr, "/api/memories");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let scoped = v["memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["facts"]["kind"] == "fact")
        .expect("the scoped fact must be saved");
    assert_eq!(
        scoped["facts"]["project"], "/Users/x/repo",
        "a scope that is not `global` becomes the project: {v}"
    );
    let scoped_id = scoped["id"].as_str().unwrap().to_string();
    let (status, body) = post_body(
        addr,
        "/api/memory/delete",
        None,
        &format!(r#"{{"id":"{scoped_id}"}}"#),
    );
    assert_eq!(status, 200, "{body}");

    let (status, body) = post_body(
        addr,
        "/api/memory/delete",
        None,
        &format!(r#"{{"id":"{new_id}"}}"#),
    );
    assert_eq!(status, 200, "{body}");
    let (_, body) = get(addr, "/api/memories");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        v["unavailable"].is_null(),
        "an unreadable pack reports an empty list too, so the emptiness below proves nothing without this: {v}"
    );
    assert!(v["memories"].as_array().unwrap().is_empty(), "{v}");
}

#[test]
fn memory_writes_refuse_a_foreign_origin_and_bad_input() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let root = root_of_shared_db().join("memory");
    let _cleanup = RemoveOnDrop(root.clone());

    let (status, body) = post_body(
        addr,
        "/api/memory/save",
        Some("http://evil.example"),
        r#"{"kind":"lesson","text":"Never comment code unless asked.","scope":"global","confidence":100}"#,
    );
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("origin"), "{body}");

    let (status, body) = post_body(
        addr,
        "/api/memory/save",
        None,
        r#"{"kind":"wish","text":"Never comment code unless asked.","scope":"global","confidence":100}"#,
    );
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("kind"), "{body}");

    let (status, body) = post_body(
        addr,
        "/api/memory/save",
        None,
        r#"{"kind":"lesson","text":"short","scope":"global","confidence":100}"#,
    );
    assert_eq!(
        status, 400,
        "a memory outside the length bounds is refused: {body}"
    );

    let (status, body) = post_body(addr, "/api/memory/save", None, "not json at all");
    assert_eq!(status, 400, "{body}");

    let (status, body) = post_body(
        addr,
        "/api/memory/save",
        None,
        r#"{"id":"000000000000","kind":"lesson","text":"Never comment code unless asked.","scope":"global","confidence":100}"#,
    );
    assert_eq!(
        status, 404,
        "a save naming an id edits that id and refuses when it does not exist, rather than creating: {body}"
    );
    assert!(body.contains("no memory"), "{body}");

    let (status, body) = post_body(addr, "/api/memory/delete", None, r#"{"id":"000000000000"}"#);
    assert_eq!(status, 404, "{body}");

    let (status, body) = post_body(addr, "/api/memory/delete", None, r#"{"id":""}"#);
    assert_eq!(
        status, 400,
        "an empty id prefix-matches every memory, so it must be refused rather than resolved: {body}"
    );

    let (status, body) = post_body(
        addr,
        "/api/memory/save",
        None,
        r#"{"kind":"lesson","text":"Never comment code unless asked.","scope":"global","confidence":101}"#,
    );
    assert_eq!(
        status, 400,
        "a confidence over 100 is refused, not clamped: {body}"
    );

    let (_, body) = get(addr, "/api/memories");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        v["unavailable"].is_null(),
        "an unreadable pack reports an empty list too, so the emptiness below proves nothing without this: {v}"
    );
    assert!(
        v["memories"].as_array().unwrap().is_empty(),
        "every refusal above must have written nothing: {v}"
    );
}

#[test]
fn a_write_is_refused_while_the_memory_store_is_locked() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let root = root_of_shared_db().join("memory");
    let _cleanup = RemoveOnDrop(root.clone());
    std::fs::create_dir_all(&root).unwrap();
    let lock = root.join("db.lock");
    std::fs::write(&lock, std::process::id().to_string()).unwrap();

    let (status, body) = post_body(
        addr,
        "/api/memory/save",
        None,
        r#"{"kind":"lesson","text":"Never comment code unless asked.","scope":"global","confidence":100}"#,
    );
    assert_eq!(
        status, 409,
        "a locked memory store refuses the write: {body}"
    );
    assert!(body.contains("busy"), "{body}");

    let (status, body) = post_body(addr, "/api/memory/delete", None, r#"{"id":"abc123"}"#);
    assert_eq!(status, 409, "{body}");

    std::fs::remove_file(&lock).unwrap();
    let (_, body) = get(addr, "/api/memories");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        v["unavailable"].is_null(),
        "an unreadable pack reports an empty list too: {v}"
    );
    assert!(
        v["memories"].as_array().unwrap().is_empty(),
        "a refused write wrote nothing: {v}"
    );
}

#[test]
fn a_body_that_arrives_after_the_headers_is_still_read() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let body = r#"{"id":"000000000000"}"#;
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    write!(
        s,
        "POST /api/memory/delete HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    s.flush().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let _ = write!(s, "{body}");
    let _ = s.flush();
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    let status: u16 = buf
        .split_whitespace()
        .nth(1)
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    let out = buf.split("\r\n\r\n").nth(1).unwrap_or("");
    assert_eq!(
        status, 404,
        "the server must wait for a body that does not arrive with the headers: {out}"
    );
    assert!(out.contains("no memory"), "{out}");
}

fn shared_config() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("BR8N_CONFIG").unwrap())
}

struct RestoreConfig {
    path: std::path::PathBuf,
    original: Vec<u8>,
}

impl RestoreConfig {
    fn take() -> RestoreConfig {
        let path = shared_config();
        let original = std::fs::read(&path).unwrap();
        RestoreConfig { path, original }
    }
}

impl Drop for RestoreConfig {
    fn drop(&mut self) {
        std::fs::write(&self.path, &self.original).unwrap();
        let _ = std::fs::remove_file(br8n::env_file::env_path(&self.path));
    }
}

fn own_origin(addr: std::net::SocketAddr) -> String {
    format!("http://127.0.0.1:{}", addr.port())
}

fn json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"))
}

fn current_etag(addr: std::net::SocketAddr) -> String {
    let (status, body) = get(addr, "/api/config");
    assert_eq!(status, 200, "{body}");
    json(&body)["etag"].as_str().unwrap().to_string()
}

fn write_private(path: &std::path::Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn the_config_view_shows_file_effective_and_defaults_and_never_a_secret() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let restore = RestoreConfig::take();
    let mut text = String::from_utf8(restore.original.clone()).unwrap();
    text.push_str("token = \"sk-config-sentinel\"\n\n[backup.s3]\nbucket = \"b\"\nregion = \"r\"\naws_secret_access_key = \"sk-aws-sentinel\"\n");
    std::fs::write(&restore.path, &text).unwrap();
    write_private(
        &br8n::env_file::env_path(&restore.path),
        "BR8N_EMBED_URL=http://127.0.0.1:1\nBR8N_EMBED_MODEL=wire-model\nBR8N_EMBED_TOKEN=sk-env-sentinel\n",
    );

    let (status, body) = get(addr, "/api/config");
    assert_eq!(status, 200, "{body}");
    assert!(
        !body.contains("sentinel"),
        "a secret leaked into the GET body: {body}"
    );
    let v = json(&body);
    assert_eq!(v["path"], restore.path.display().to_string());
    assert_eq!(v["exists"], true);
    assert_eq!(v["etag"], br8n::config::edit::etag_of(Some(&text)));
    assert_eq!(v["secrets"]["embed.token"], "set");
    assert_eq!(v["endpoint"]["backend"], "remote");
    assert_eq!(v["endpoint"]["url"], "http://127.0.0.1:1");
    assert_eq!(v["endpoint"]["model"], "wire-model");
    assert!(v["file"]["embed"]["ollama_url"].is_string(), "{v}");
    assert!(v["file"]["hook"].is_null(), "only what the file sets: {v}");
    assert_eq!(v["effective"]["hook"]["threshold"], 0.66);
    assert_eq!(v["defaults"]["mcp"]["threshold"], 0.55);
    assert_eq!(v["env_overrides"], serde_json::json!([]));
    let errors: Vec<&str> = v["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        errors,
        ["embed.token", "backup.s3.aws_secret_access_key"],
        "{v}"
    );
}

#[test]
fn saving_the_config_checks_the_etag_and_the_values_and_keeps_a_backup() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let restore = RestoreConfig::take();
    let origin = own_origin(addr);
    let etag = current_etag(addr);

    let save = serde_json::json!({ "etag": etag, "set": { "hook.threshold": 0.7 } }).to_string();
    let (status, body) = post_body(addr, "/api/config", Some(&origin), &save);
    assert_eq!(status, 200, "{body}");
    let written = std::fs::read_to_string(&restore.path).unwrap();
    assert!(written.contains("[hook]\nthreshold = 0.7\n"), "{written}");
    assert!(
        written.contains("ollama_url"),
        "the rest of the file survives: {written}"
    );
    assert_eq!(
        json(&body)["etag"],
        br8n::config::edit::etag_of(Some(&written))
    );
    assert_eq!(json(&body)["effective"]["hook"]["threshold"], 0.7);
    assert_eq!(
        std::fs::read(br8n::config::edit::backup_path(&restore.path)).unwrap(),
        restore.original
    );

    let (status, body) = post_body(addr, "/api/config", Some(&origin), &save);
    assert_eq!(status, 409, "a stale etag must not overwrite: {body}");
    assert_eq!(
        json(&body)["etag"],
        br8n::config::edit::etag_of(Some(&written))
    );
    assert_eq!(std::fs::read_to_string(&restore.path).unwrap(), written);

    let fresh = br8n::config::edit::etag_of(Some(&written));
    let bad = serde_json::json!({ "etag": fresh, "set": { "hook.quality": 9 } }).to_string();
    let (status, body) = post_body(addr, "/api/config", Some(&origin), &bad);
    assert_eq!(status, 422, "{body}");
    assert_eq!(json(&body)["errors"][0]["path"], "hook.quality");
    assert_eq!(std::fs::read_to_string(&restore.path).unwrap(), written);

    let reset = serde_json::json!({ "etag": fresh, "unset": ["hook.threshold"] }).to_string();
    let (status, body) = post_body(addr, "/api/config", Some(&origin), &reset);
    assert_eq!(status, 200, "{body}");
    assert_eq!(json(&body)["effective"]["hook"]["threshold"], 0.66);
}

#[test]
fn config_writes_refuse_a_foreign_origin() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let restore = RestoreConfig::take();
    let etag = br8n::config::edit::etag_of(Some(std::str::from_utf8(&restore.original).unwrap()));
    let save = serde_json::json!({ "etag": etag, "set": { "hook.threshold": 0.7 } }).to_string();
    for path in [
        "/api/config",
        "/api/config/check",
        "/api/config/embed",
        "/api/index",
        "/api/embed/test",
    ] {
        let (status, body) = post_body(addr, path, Some("http://evil.example"), &save);
        assert_eq!(status, 409, "{path}: {body}");
        assert!(body.contains("origin"), "{path}: {body}");
    }
    assert_eq!(std::fs::read(&restore.path).unwrap(), restore.original);
    assert!(!br8n::env_file::env_path(&restore.path).exists());
}

#[test]
fn checking_a_change_reports_its_errors_and_writes_nothing() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let restore = RestoreConfig::take();
    let before = std::fs::metadata(&restore.path)
        .unwrap()
        .modified()
        .unwrap();
    let body =
        serde_json::json!({ "set": { "hook.thresold": 0.5, "mcp.threshold": 2 } }).to_string();
    let (status, out) = post_body(addr, "/api/config/check", Some(&own_origin(addr)), &body);
    assert_eq!(status, 200, "{out}");
    let v = json(&out);
    let paths: Vec<&str> = v["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["hook.thresold", "mcp.threshold"], "{v}");
    assert!(v["errors"][0]["message"]
        .as_str()
        .unwrap()
        .contains("did you mean `threshold`"));
    assert_eq!(v["blocking"], v["errors"]);
    assert_eq!(std::fs::read(&restore.path).unwrap(), restore.original);
    assert_eq!(
        std::fs::metadata(&restore.path)
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
}

#[test]
fn testing_the_embedder_reports_an_unreachable_endpoint_without_failing() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let restore = RestoreConfig::take();
    let (status, body) = post_body(addr, "/api/embed/test", None, "{}");
    assert_eq!(status, 200, "{body}");
    let v = json(&body);
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["backend"], "ollama");
    assert_eq!(v["dimensions"], 512);

    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    std::fs::write(&restore.path, format!("[embed]\nollama_url = \"{dead}\"\n")).unwrap();
    let (status, body) = post_body(addr, "/api/embed/test", None, "");
    assert_eq!(
        status, 200,
        "an unreachable endpoint is an answer, not an outage: {body}"
    );
    let v = json(&body);
    assert_eq!(v["ok"], false, "{v}");
    assert_eq!(v["url"], dead);
    assert!(
        v["error"]
            .as_str()
            .is_some_and(|e| e.contains("unreachable")),
        "{v}"
    );
    assert!(v["dimensions"].is_null(), "{v}");
}

#[test]
fn the_embedding_endpoint_is_written_to_the_env_file_and_its_token_is_write_only() {
    use std::os::unix::fs::PermissionsExt;
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let restore = RestoreConfig::take();
    let env = br8n::env_file::env_path(&restore.path);
    let origin = own_origin(addr);

    let body = r#"{"url":"http://127.0.0.1:1/","model":"wire","token":"sk-write-only-sentinel"}"#;
    let (status, out) = post_body(addr, "/api/config/embed", Some(&origin), body);
    assert_eq!(status, 200, "{out}");
    assert!(!out.contains("sentinel"), "{out}");
    let v = json(&out);
    assert_eq!(v["secrets"]["embed.token"], "set");
    assert_eq!(v["endpoint"]["backend"], "remote");
    assert!(v["endpoint"]["error"].is_null(), "{v}");
    assert_eq!(
        std::fs::metadata(&env).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(std::fs::read_to_string(&env)
        .unwrap()
        .contains("BR8N_EMBED_TOKEN=sk-write-only-sentinel"));
    let (_, got) = get(addr, "/api/config");
    assert!(!got.contains("sentinel"), "{got}");

    let (status, out) = post_body(
        addr,
        "/api/config/embed",
        Some(&origin),
        r#"{"model":null}"#,
    );
    assert_eq!(
        status, 422,
        "a URL without a model would break every embed: {out}"
    );
    assert_eq!(json(&out)["errors"][0]["path"], "endpoint.model");
    let (status, out) = post_body(
        addr,
        "/api/config/embed",
        Some(&origin),
        r#"{"url":"ftp://x"}"#,
    );
    assert_eq!(status, 422, "{out}");

    let (status, out) = post_body(
        addr,
        "/api/config/embed",
        Some(&origin),
        r#"{"model":"other"}"#,
    );
    assert_eq!(status, 200, "{out}");
    assert!(
        std::fs::read_to_string(&env)
            .unwrap()
            .contains("BR8N_EMBED_TOKEN=sk-write-only-sentinel"),
        "an absent token is left alone"
    );

    let (status, out) = post_body(
        addr,
        "/api/config/embed",
        Some(&origin),
        r#"{"url":null,"model":null,"token":null}"#,
    );
    assert_eq!(status, 200, "{out}");
    let v = json(&out);
    assert_eq!(v["secrets"]["embed.token"], "unset");
    assert_eq!(v["endpoint"]["backend"], "ollama");
    assert_eq!(std::fs::read_to_string(&env).unwrap(), "");
}

#[test]
fn starting_an_index_is_refused_while_one_runs() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let lock = std::path::PathBuf::from(std::env::var("BR8N_DB").unwrap()).with_extension("lock");
    std::fs::write(&lock, std::process::id().to_string()).unwrap();
    let (status, body) = post(addr, "/api/index", None);
    std::fs::remove_file(&lock).unwrap();
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("already running"), "{body}");
}

#[test]
fn a_saved_config_change_reaches_the_read_routes_without_a_restart() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = RestoreConfig::take();
    let search = "/api/search?q=pgbouncer%20transaction";
    let (_, before) = get(addr, search);
    let threshold = |body: &str| json(body)["threshold"].as_f64().unwrap();
    assert!((threshold(&before) - 0.66).abs() < 1e-6, "{before}");

    let save = serde_json::json!({
        "etag": current_etag(addr),
        "set": { "hook.threshold": 0.25, "memory.enabled": false },
    })
    .to_string();
    let (status, body) = post_body(addr, "/api/config", Some(&own_origin(addr)), &save);
    assert_eq!(status, 200, "{body}");

    let (status, after) = get(addr, search);
    assert_eq!(status, 200, "{after}");
    assert!((threshold(&after) - 0.25).abs() < 1e-6, "{after}");
    let (_, memories) = get(addr, "/api/memories");
    assert!(
        json(&memories)["unavailable"].is_string(),
        "memory.enabled = false must reach /api/memories: {memories}"
    );
}

#[test]
fn the_index_run_status_is_empty_before_any_run() {
    let addr = server_over_temp_corpus();
    let (status, body) = get(addr, "/api/index");
    assert_eq!(status, 200, "{body}");
    assert!(json(&body)["run"].is_null(), "{body}");
}

fn agent_env(dir: &std::path::Path) -> br8n::setup::agents::AgentEnv {
    let home = dir.join("home");
    let paths = br8n::setup::Paths::at(&home.join("br8n"), vec![], home.join("cache"));
    br8n::setup::agents::AgentEnv::at(&home, paths, vec![])
}

#[test]
fn the_agents_api_lists_connects_and_disconnects_behind_the_origin_check() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let env = agent_env(&root_of_shared_db());
    let cursor = env.home.join(".cursor/mcp.json");

    let (status, body) = get(addr, "/api/agents");
    assert_eq!(status, 200, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["agents"].as_array().unwrap().len(), 5, "{v}");
    assert!(v["snippets"]["mcp_json"]
        .as_str()
        .unwrap()
        .contains("\"mcp\""));

    let body = r#"{"id":"cursor"}"#;
    let (status, out) = post_body(
        addr,
        "/api/agents/connect",
        Some("http://evil.example"),
        body,
    );
    assert_eq!(status, 409, "{out}");
    assert!(!cursor.exists(), "a refused origin writes nothing");

    let origin = format!("http://127.0.0.1:{}", addr.port());
    let (status, out) = post_body(addr, "/api/agents/connect", Some(&origin), body);
    assert_eq!(status, 200, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["agent"]["id"], "cursor");
    assert_eq!(v["change"]["files"][0], cursor.display().to_string());
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cursor).unwrap()).unwrap();
    assert_eq!(written["mcpServers"]["br8n"]["args"][0], "mcp");

    let (status, out) = post_body(addr, "/api/agents/disconnect", Some(&origin), body);
    assert_eq!(status, 200, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["agent"]["status"]["state"], "not_connected");

    let (status, _) = post_body(
        addr,
        "/api/agents/connect",
        Some(&origin),
        r#"{"id":"vim"}"#,
    );
    assert_eq!(status, 404);
    let (status, _) = post_body(addr, "/api/agents/connect", Some(&origin), "nope");
    assert_eq!(status, 400);
}

fn check<'a>(v: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no `{name}` check: {v}"))
}

#[test]
fn the_install_view_names_each_failed_check_with_its_fix_and_the_config_errors() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let restore = RestoreConfig::take();
    let env = agent_env(&root_of_shared_db());
    let exe = std::env::current_exe().unwrap();

    let (status, body) = get(addr, "/api/install");
    assert_eq!(status, 200, "{body}");
    let v = json(&body);
    assert_eq!(v["binary"], env.bin().display().to_string(), "{v}");
    let binary = check(&v, "binary");
    assert_eq!(binary["ok"], false, "{v}");
    assert!(
        binary["detail"]
            .as_str()
            .unwrap()
            .contains(&env.bin().display().to_string()),
        "{v}"
    );
    assert_eq!(binary["fix"]["kind"], "command", "{v}");
    assert_eq!(
        binary["fix"]["command"],
        format!("\"{}\" install", exe.display()),
        "{v}"
    );
    let plugin = check(&v, "plugin");
    assert_eq!(plugin["ok"], false, "{v}");
    assert_eq!(
        plugin["fix"]["kind"], "command",
        "with no `claude` to connect through, only an install writes the plugin: {v}"
    );
    let claude = check(&v, "claude");
    assert_eq!(claude["ok"], false, "{v}");
    assert_eq!(claude["fix"]["kind"], "manual", "{v}");
    assert!(check(&v, "path")["fix"].is_object(), "{v}");
    assert_eq!(v["config"]["path"], restore.path.display().to_string());
    assert_eq!(v["config"]["errors"], serde_json::json!([]), "{v}");
    assert_eq!(v["embed"]["backend"], "ollama", "{v}");
    assert!(
        v["embed"]["url"]
            .as_str()
            .is_some_and(|u| u.starts_with("http://127.0.0.1")),
        "{v}"
    );

    std::fs::create_dir_all(env.bin().parent().unwrap()).unwrap();
    let _placed = RemoveOnDrop(env.bin().parent().unwrap().to_path_buf());
    std::os::unix::fs::symlink(&exe, env.bin()).unwrap();
    std::fs::write(
        &restore.path,
        format!(
            "hook.thresold = 0.5\n{}",
            String::from_utf8_lossy(&restore.original)
        ),
    )
    .unwrap();
    let (status, body) = get(addr, "/api/install");
    assert_eq!(status, 200, "{body}");
    let v = json(&body);
    let binary = check(&v, "binary");
    assert_eq!(binary["ok"], true, "{v}");
    assert!(
        binary["fix"].is_null(),
        "a passing check offers no fix: {v}"
    );
    let errors = v["config"]["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "{v}");
    assert_eq!(errors[0]["path"], "hook.thresold", "{v}");
}

#[test]
fn a_machine_that_never_indexed_reads_as_empty_and_a_swap_window_still_as_busy() {
    let addr = server_over_temp_corpus();
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let good_db = std::env::var("BR8N_DB").unwrap();
    let fresh = tempfile::tempdir().unwrap();
    let db = fresh.path().join("db");
    unsafe { std::env::set_var("BR8N_DB", &db) };
    let stats = get(addr, "/api/stats");
    let graph = get(addr, "/api/graph");
    std::fs::create_dir_all(db.with_extension("old")).unwrap();
    let swapping = get(addr, "/api/stats");
    unsafe { std::env::set_var("BR8N_DB", &good_db) };

    assert_eq!(stats.0, 200, "{}", stats.1);
    let v = json(&stats.1);
    assert_eq!(v["documents"], 0, "{v}");
    assert_eq!(v["chunks"], 0, "{v}");
    assert_eq!(graph.0, 200, "{}", graph.1);
    assert_eq!(json(&graph.1)["edges"], serde_json::json!([]));
    assert_eq!(
        swapping.0, 503,
        "a live index renamed aside mid-swap is busy, not empty: {}",
        swapping.1
    );
}
