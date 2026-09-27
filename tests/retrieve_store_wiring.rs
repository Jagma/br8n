//! Pins the wiring `retrieve_for` itself performs, not just
//! `Retriever::packed`'s behaviour in isolation (that's `retrieve_primitives.rs`).
//!
//! Task 6 made `Retriever.store` an `Option`, opened only when the profile
//! needs it. Hard-coding that condition to `true` keeps every other suite green
//! while silently deleting the whole point of the retrieval pack — this test is
//! the one thing that catches it.
//!
//! The condition changed when the inbound link counts authority weighting
//! needs moved into `pack.links`:
//!
//!     let needs_store = profile.graph.is_some()
//!         || (cfg.weights_for(surface).authority > 0.0 && pack.is_none());
//!
//! Authority used to force a store open at EVERY tier — measured at 0.168s
//! against 0.078s per query — because `inbound_link_counts` lived only in the
//! database. It now reads the pack. The `&& pack.is_none()` half is not
//! redundant: with no pack the counts really are store-only, and dropping that
//! clause would silently discard the authority lift instead of paying for it.
//! All three branches are pinned below.
//!
//! `BR8N_DB` is process-global (`Config::db_path()` reads it fresh on every
//! call), so every test in this file sets it for its own duration only and
//! restores whatever was there before, the same discipline `tests/dashboard.rs`
//! documents for its `ENV_GUARD`. Unlike that file, the two tests here never
//! run concurrently with anything else that touches `BR8N_DB` in THIS binary
//! except each other, so `ENV_GUARD` below only has to serialize the two of
//! them against one another.

mod common;

use std::path::PathBuf;

static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn retrieve_for_opens_a_store_only_when_the_profile_needs_one() {
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    std::fs::create_dir_all(&db).unwrap();

    // `model_id` is computed from config alone — no network call, so this is
    // safe to run with no Ollama anywhere nearby.
    let mut cfg = br8n::config::Config::default();
    cfg.embed.ollama_url = "http://127.0.0.1:1".to_string();
    let embedder = br8n::embed::OllamaEmbedder::new(&cfg.embed);
    let model_id = br8n::embed::Embedder::model_id(&embedder);

    // An empty pack is enough: this test only exercises the store-vs-pack
    // DECISION `retrieve_for` makes before any query runs, never a search.
    // The directory holds pack files and deliberately no database at all —
    // if the wiring under test regressed to always opening a store, the
    // `authority = 0.0` case below would fail to open anything, not just
    // return the wrong `Option`.
    br8n::pack::Pack::build(
        &db,
        &model_id,
        cfg.embed.dimensions,
        Vec::new(),
        &Default::default(),
        &Default::default(),
    )
    .unwrap();

    let prev = std::env::var("BR8N_DB").ok();
    // SAFETY: this test is the only one in this binary that touches
    // `BR8N_DB`, and the previous value is restored before returning,
    // including on panic via the guard below.
    unsafe { std::env::set_var("BR8N_DB", &db) };
    struct RestoreEnv(Option<String>);
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            match &self.0 {
                Some(v) => unsafe { std::env::set_var("BR8N_DB", v) },
                None => unsafe { std::env::remove_var("BR8N_DB") },
            }
        }
    }
    let _restore = RestoreEnv(prev);

    // authority = 0.0 (the shipped default): tier 1 at the hook surface needs
    // neither graph expansion nor authority weighting, so `retrieve_for` must
    // take the store-free path — no database exists at `db` for it to open.
    let storeless = br8n::retrieve_for(&cfg, br8n::config::Surface::Hook)
        .expect("a valid pack with no database must still succeed at tier 1");
    assert!(
        storeless.store().is_none(),
        "authority = 0.0 must not open a store when a valid pack is present"
    );

    // authority = 0.3 WITH a pack: the counts now live in `pack.links`, so this
    // must ALSO take the store-free path. This assertion is the inverse of what
    // it was before 2026-08-31, when the same case was required to FAIL for want
    // of a database. There is still no database at `db`, so a regression that
    // sent authority back to the store would fail to open one and error here.
    cfg.weights.authority = 0.3;
    let with_authority = br8n::retrieve_for(&cfg, br8n::config::Surface::Hook)
        .expect("authority reads the pack now, so a valid pack with no database must succeed");
    assert!(
        with_authority.store().is_none(),
        "authority weighting must read pack.links, not open a database"
    );

    // NOT PINNED, deliberately, and stated so rather than faked: the
    // `&& pack.is_none()` half of the condition makes NO observable difference.
    // With no pack, `needs_store == false` falls into the `None` arm, which
    // calls `Store::open_existing` itself (`src/lib.rs`), and `needs_store ==
    // true` calls the same thing in the else arm — both open a store, so no
    // assertion here can tell the two apart.
    //
    // This file's first draft asserted that case anyway. Deleting the clause
    // left the assertion green, which is the definition of a vacuous test and
    // exactly the failure this codebase keeps shipping. The clause is worth
    // keeping for what it says to a reader — authority with no pack really is
    // store-only — but it is documentation, not behaviour, and pretending
    // otherwise would be worse than leaving it uncovered.
}

#[test]
fn an_index_with_no_pack_refuses_loudly_instead_of_searching_the_store() {
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");

    let mut cfg = br8n::config::Config::default();
    cfg.embed.ollama_url = common::fake_ollama();
    let model_id = br8n::embed::Embedder::model_id(&br8n::embed::OllamaEmbedder::new(&cfg.embed));

    {
        let store = br8n::store::Store::open(&db, cfg.embed.dimensions).unwrap();
        let idx = br8n::index::Indexer::new(
            store,
            Box::new(br8n::embed::OllamaEmbedder::new(&cfg.embed)),
            cfg.clone(),
        );
        idx.index_documents(&[br8n::model::Document::new(
            br8n::model::SourceType::Markdown,
            "file:///review.md",
            "Review",
            "# Review\n\nHow do we review code before merging.",
        )])
        .unwrap();
    }
    {
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
    std::fs::remove_file(db.join(br8n::pack::manifest::MANIFEST_FILE)).unwrap();

    let prev = std::env::var("BR8N_DB").ok();
    unsafe { std::env::set_var("BR8N_DB", &db) };
    struct RestoreEnv(Option<String>);
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            match &self.0 {
                Some(v) => unsafe { std::env::set_var("BR8N_DB", v) },
                None => unsafe { std::env::remove_var("BR8N_DB") },
            }
        }
    }
    let _restore = RestoreEnv(prev);

    let retriever = br8n::retrieve_for(&cfg, br8n::config::Surface::Hook)
        .expect("a packless index still constructs a retriever; the refusal is at query time");
    let err = retriever
        .search(
            "how do we review code",
            &cfg.profile_for(br8n::config::Surface::Hook),
        )
        .expect_err("a packless index must refuse, not fall back to a store vector search");
    assert!(
        err.downcast_ref::<br8n::retrieve::PackRefused>().is_some(),
        "the refusal must be tagged PackRefused, not a plain error, or the dashboard \
         routes it down the transient 503 path instead of showing the repair; got: {err}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("--compact"),
        "the refusal must tell the user how to repair it; got: {msg}"
    );
}

struct McpWorld {
    _dir: tempfile::TempDir,
    notes: PathBuf,
    db: PathBuf,
    cfg: PathBuf,
    _db_env: common::EnvVarGuard,
}

impl McpWorld {
    fn new() -> McpWorld {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        let ollama = common::fake_ollama();
        let cfg = dir.path().join("config.toml");
        std::fs::write(
            &cfg,
            format!(
                "index_transcripts = false\nsources = [\"{}\"]\n\n[embed]\nollama_url = \"{ollama}\"\n",
                notes.display()
            ),
        )
        .unwrap();
        let db = dir.path().join("db");
        McpWorld {
            _db_env: common::EnvVarGuard::set("BR8N_DB", &db),
            _dir: dir,
            notes,
            db,
            cfg,
        }
    }

    fn note(&self, name: &str, body: &str) {
        std::fs::write(self.notes.join(name), body).unwrap();
    }

    fn index(&self) {
        assert_cmd::Command::cargo_bin("br8n")
            .unwrap()
            .env("BR8N_DB", &self.db)
            .env("BR8N_CONFIG", &self.cfg)
            .arg("index")
            .timeout(std::time::Duration::from_secs(60))
            .assert()
            .success();
    }

    fn server(&self) -> br8n::mcp::Br8nTools {
        br8n::mcp::Br8nTools::new(br8n::config::Config::load_from(&self.cfg))
    }
}

#[test]
fn two_searches_with_no_publish_open_the_pack_once() {
    let _env = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let world = McpWorld::new();
    world.note("alpha.md", "# Alpha\n\nThe zebracorn migrates at dawn.\n");
    world.index();
    let server = world.server();

    let first = server.search("zebracorn", None);
    let second = server.search("zebracorn", None);

    assert!(first.contains("alpha.md"), "{first}");
    assert!(second.contains("alpha.md"), "{second}");
    assert_eq!(
        server.pack_opens(),
        1,
        "a second search against an unchanged pack must reuse the one already open"
    );
}

#[test]
fn a_search_after_a_reindex_sees_the_newly_published_pack() {
    let _env = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let world = McpWorld::new();
    world.note("alpha.md", "# Alpha\n\nThe zebracorn migrates at dawn.\n");
    world.index();
    let server = world.server();

    server.search("quokkaline", None);

    world.note(
        "beta.md",
        "# Beta\n\nThe quokkaline sleeps under the bridge.\n",
    );
    world.index();

    let after = server.search("quokkaline", None);
    assert!(
        after.contains("beta.md"),
        "the search after a re-index must read the pack that re-index published; got: {after}"
    );
    assert_eq!(server.pack_opens(), 2);
}
