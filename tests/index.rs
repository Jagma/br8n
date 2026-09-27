mod common;

use br8n::config::Config;
use br8n::model::{Document, SourceType};
use common::{rooted, setup, setup_with_sources};
use std::sync::atomic::Ordering;

#[test]
fn first_index_adds_documents_and_chunks() {
    let (_d, idx, _calls) = setup();
    let doc = Document::new(
        SourceType::Markdown,
        "file:///a.md",
        "A",
        "# A\n\nHello world.",
    );
    let stats = idx.index_documents(&[doc]).unwrap();
    assert_eq!(stats.added, 1);
    assert!(stats.chunks >= 1);
}

#[test]
fn reindexing_unchanged_content_skips_without_re_embedding() {
    let (_d, idx, calls) = setup();
    let doc = Document::new(
        SourceType::Markdown,
        "file:///a.md",
        "A",
        "# A\n\nHello world.",
    );
    idx.index_documents(std::slice::from_ref(&doc)).unwrap();
    let calls_after_first = calls.load(Ordering::SeqCst);
    assert!(calls_after_first >= 1, "first index must actually embed");

    let second = idx.index_documents(&[doc]).unwrap();
    assert_eq!(second.skipped, 1);
    assert_eq!(second.added, 0);
    assert_eq!(
        second.chunks, 0,
        "no chunks re-embedded for unchanged content"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        calls_after_first,
        "embed_documents must not be called again on an unchanged re-index"
    );
}

#[test]
fn changed_content_replaces_old_chunks_rather_than_accumulating() {
    let (_d, idx, _calls) = setup();
    let v1 = Document::new(SourceType::Markdown, "file:///a.md", "A", "# A\n\nOne.");
    idx.index_documents(&[v1]).unwrap();
    let before = idx.store().count_chunks().unwrap();

    let v2 = Document::new(
        SourceType::Markdown,
        "file:///a.md",
        "A",
        "# A\n\nTwo. Different.",
    );
    let stats = idx.index_documents(&[v2]).unwrap();

    assert_eq!(stats.updated, 1);
    assert_eq!(idx.store().count_documents().unwrap(), 1);
    assert!(idx.store().count_chunks().unwrap() >= 1);
    assert_eq!(before, 1);
}

#[test]
fn prune_removes_documents_whose_source_disappeared() {
    let src = tempfile::tempdir().unwrap();
    let (root, a_uri) = rooted(&src, "a.md");
    let (_, b_uri) = rooted(&src, "b.md");
    let (_d, idx, _calls) = setup_with_sources(vec![root]);
    let a = Document::new(SourceType::Markdown, &a_uri, "A", "# A\n\nOne.");
    let b = Document::new(SourceType::Markdown, &b_uri, "B", "# B\n\nTwo.");
    idx.index_documents(&[a.clone(), b]).unwrap();

    let removed = idx.prune_missing(std::slice::from_ref(&a.uri)).unwrap();
    assert_eq!(removed, 1);
    assert_eq!(idx.store().count_documents().unwrap(), 1);
}

#[test]
fn model_id_is_stamped_on_first_index() {
    let (_d, idx, _calls) = setup();
    let doc = Document::new(SourceType::Markdown, "file:///a.md", "A", "# A\n\nHi.");
    idx.index_documents(&[doc]).unwrap();
    assert_eq!(
        idx.store().get_meta("embed_model").unwrap().as_deref(),
        Some("fake@4")
    );
}

#[test]
fn indexing_with_a_different_model_is_a_hard_error() {
    let (_d, idx, _calls) = setup();
    idx.store().set_meta("embed_model", "other@768").unwrap();
    let doc = Document::new(SourceType::Markdown, "file:///a.md", "A", "# A\n\nHi.");
    let err = idx.index_documents(&[doc]).unwrap_err().to_string();
    assert!(
        err.contains("other@768"),
        "error must name the stored model: {err}"
    );
    assert!(err.to_lowercase().contains("reindex") || err.to_lowercase().contains("re-index"));
}

/// An index built before schema versioning existed has `embed_model` stamped
/// (every index gets that on its first run) but no `schema_version` key at
/// all — pre-Task-2 binaries never wrote one. That must be told apart from a
/// genuinely brand-new store (neither key set) and refused with the same
/// remedy as a model mismatch, not silently adopted: `embed_text` -> `embed_hash`
/// drops the text a migration would need, so reading an old row under the new
/// schema would be wrong, not merely stale.
#[test]
fn an_index_predating_schema_versioning_is_refused_not_silently_upgraded() {
    let (_d, idx, _calls) = setup();
    // Reproduces the pre-Task-2 shape directly, without needing an old binary:
    // `embed_model` set (as every index has always done on first run),
    // `schema_version` absent (the key this task introduces).
    idx.store().set_meta("embed_model", "fake@4").unwrap();
    let doc = Document::new(SourceType::Markdown, "file:///a.md", "A", "# A\n\nHi.");
    let err = idx.index_documents(&[doc]).unwrap_err().to_string();
    assert!(
        err.to_lowercase().contains("reindex") || err.to_lowercase().contains("re-index"),
        "error must point at the remedy: {err}"
    );
    assert_eq!(
        idx.store().get_meta("schema_version").unwrap(),
        None,
        "a refused index must not be silently stamped with the new schema version"
    );
}

/// Self-review: a document that chunks to zero chunks (empty/whitespace-only
/// text) must still be tracked as a Document node — with zero chunks — so a
/// later unchanged re-index sees it as already indexed rather than re-adding
/// it forever, and so it never gets embedded.
#[test]
fn empty_document_is_added_with_no_chunks_and_then_skipped_on_rerun() {
    let (_d, idx, calls) = setup();
    let doc = Document::new(
        SourceType::Markdown,
        "file:///empty.md",
        "Empty",
        "   \n\n  ",
    );

    let first = idx.index_documents(std::slice::from_ref(&doc)).unwrap();
    assert_eq!(first.added, 1);
    assert_eq!(first.chunks, 0);
    assert_eq!(idx.store().count_documents().unwrap(), 1);
    assert_eq!(idx.store().count_chunks().unwrap(), 0);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "an empty document must never be embedded"
    );

    let second = idx.index_documents(&[doc]).unwrap();
    assert_eq!(
        second.skipped, 1,
        "an already-tracked empty document must be skipped on rerun"
    );
    assert_eq!(second.added, 0);
}

#[test]
fn prune_refuses_to_empty_a_populated_index() {
    // `discover` returns Ok(vec![]) when a source path is wrong or unreachable —
    // it swallows loader errors — so this signal must never mean "delete everything".
    let src = tempfile::tempdir().unwrap();
    let (root, a_uri) = rooted(&src, "a.md");
    let (_, b_uri) = rooted(&src, "b.md");
    let (_d, idx, _calls) = setup_with_sources(vec![root]);
    let a = Document::new(SourceType::Markdown, &a_uri, "A", "# A\n\nOne.");
    let b = Document::new(SourceType::Markdown, &b_uri, "B", "# B\n\nTwo.");
    idx.index_documents(&[a, b]).unwrap();

    let err = idx.prune_missing(&[]).unwrap_err().to_string();
    assert!(err.contains("refusing to prune"), "got: {err}");
    assert_eq!(
        idx.store().count_documents().unwrap(),
        2,
        "nothing may be deleted"
    );
}

#[test]
fn discover_refuses_when_a_source_root_is_missing() {
    // The partial-loss case: several roots configured, one unreachable. Discovery
    // would return the other roots' documents, prune would see a NON-empty live
    // list, the empty-list guard would stay silent, and everything under the
    // missing root would be deleted.
    let present = tempfile::tempdir().unwrap();
    std::fs::write(present.path().join("a.md"), "# A\n\nbody").unwrap();

    let cfg = Config {
        sources: vec![
            present.path().to_path_buf(),
            std::path::PathBuf::from("/definitely/not/mounted"),
        ],
        ..Config::default()
    };

    let err = br8n::index::discover(&cfg).unwrap_err().to_string();
    assert!(err.contains("does not exist"), "got: {err}");
    assert!(
        err.contains("/definitely/not/mounted"),
        "error must name the bad root: {err}"
    );
}

#[test]
fn discover_accepts_a_root_that_exists_but_is_empty() {
    // Emptiness is legitimate — the user deleted their notes. Pruning is correct here.
    //
    // `discover` also unconditionally scans `TranscriptLoader::default_root()`
    // (`~/.claude/projects`), which is real and populated on any machine that has
    // actually used Claude Code — including the one that built this test suite.
    // Point HOME at an empty temp directory for the duration of this test so the
    // result reflects only the configured `sources`, not this machine's own
    // session history.
    let empty = tempfile::tempdir().unwrap();
    let fake_home = tempfile::tempdir().unwrap();
    let orig_home = std::env::var_os("HOME");
    std::env::set_var("HOME", fake_home.path());

    let cfg = Config {
        sources: vec![empty.path().to_path_buf()],
        ..Config::default()
    };
    let result = br8n::index::discover(&cfg);

    match orig_home {
        Some(h) => std::env::set_var("HOME", h),
        None => std::env::remove_var("HOME"),
    }

    assert!(result.unwrap().is_empty());
}

#[test]
fn prune_on_a_genuinely_empty_index_is_a_no_op() {
    let (_d, idx, _calls) = setup();
    assert_eq!(idx.prune_missing(&[]).unwrap(), 0);
}

#[test]
fn prune_never_deletes_what_discovery_cannot_enumerate() {
    // `br8n add` indexes two kinds of document `discover` never returns: web
    // clippings (an https URI that lives nowhere on disk) and files from
    // outside every configured root. Both used to be deleted by the very next
    // `br8n index` — which SessionStart spawns with stderr closed on every
    // session — so anything you added was destroyed within minutes, invisibly.
    let src = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let (root, tracked_uri) = rooted(&src, "a.md");
    let (_, stray_uri) = rooted(&elsewhere, "z.md");
    let (_d, idx, _calls) = setup_with_sources(vec![root]);

    let tracked = Document::new(SourceType::Markdown, &tracked_uri, "A", "# A\n\nOne.");
    let clipping = Document::new(
        SourceType::Web,
        "https://example.com/p",
        "P",
        "# P\n\nClipped.",
    );
    let stray = Document::new(SourceType::Markdown, &stray_uri, "Z", "# Z\n\nBy hand.");
    idx.index_documents(&[tracked.clone(), clipping, stray])
        .unwrap();

    // A complete discovery pass: it finds the tracked note and nothing else,
    // because those are the only files under a configured root.
    let removed = idx
        .prune_missing(std::slice::from_ref(&tracked.uri))
        .unwrap();

    assert_eq!(removed, 0, "out-of-band documents must survive a prune");
    assert_eq!(
        idx.store().count_documents().unwrap(),
        3,
        "clipping and stray file must both still be indexed"
    );
}

#[test]
fn prune_still_removes_tracked_files_when_clippings_are_present() {
    // The guard above must not become a blanket "never prune anything": a
    // tracked file that really was deleted still has to go, clippings or not.
    let src = tempfile::tempdir().unwrap();
    let (root, keep_uri) = rooted(&src, "keep.md");
    let (_, gone_uri) = rooted(&src, "gone.md");
    let (_d, idx, _calls) = setup_with_sources(vec![root]);

    let keep = Document::new(SourceType::Markdown, &keep_uri, "K", "# K\n\nStays.");
    let gone = Document::new(SourceType::Markdown, &gone_uri, "G", "# G\n\nDeleted.");
    let clipping = Document::new(
        SourceType::Web,
        "https://example.com/p",
        "P",
        "# P\n\nClipped.",
    );
    idx.index_documents(&[keep.clone(), gone, clipping])
        .unwrap();

    let removed = idx.prune_missing(std::slice::from_ref(&keep.uri)).unwrap();

    assert_eq!(removed, 1, "the deleted tracked file must still be pruned");
    assert_eq!(idx.store().count_documents().unwrap(), 2);
}

#[test]
fn prune_guard_counts_only_reachable_documents() {
    // An index holding nothing but clippings must not read as "populated" and
    // block a legitimate empty-discovery prune — there is nothing to protect.
    let src = tempfile::tempdir().unwrap();
    let (root, _) = rooted(&src, "unused.md");
    let (_d, idx, _calls) = setup_with_sources(vec![root]);
    let clipping = Document::new(
        SourceType::Web,
        "https://example.com/p",
        "P",
        "# P\n\nClipped.",
    );
    idx.index_documents(&[clipping]).unwrap();

    assert_eq!(idx.prune_missing(&[]).unwrap(), 0);
    assert_eq!(idx.store().count_documents().unwrap(), 1);
}

#[test]
fn the_mcp_index_path_goes_through_the_locked_swap() {
    // `index_now` — what the MCP `br8n_index` tool calls — used to open the
    // live store directly, skipping BOTH protections the CLI path has: the
    // writer lock and the shadow swap. An MCP index racing the indexer that
    // SessionStart spawns could interleave filesystem mutations, and for the
    // duration the hook (a separate process) got nothing at all, because
    // LadybugDB holds an exclusive OS file lock.
    //
    // Asserting the routing, not just that the lock primitive excludes itself:
    // with the lock already held, `index_now` must REFUSE rather than write.
    let db = tempfile::tempdir().unwrap();
    let db_path = db.path().join("graph");
    std::fs::create_dir_all(&db_path).unwrap();

    // SAFETY: single-threaded test; restored before returning.
    let prev = std::env::var("BR8N_DB").ok();
    unsafe { std::env::set_var("BR8N_DB", &db_path) };

    // `index_transcripts` defaults to TRUE, and this calls the real indexer.
    // If the lock were ever free when this ran, the default config would send it
    // through the user's actual ~/.claude/projects — 277 files / 72 MB on one
    // machine — and out to a live Ollama. Turn it off so the test is inert even
    // if the thing it is asserting stops working.
    let cfg = Config {
        index_transcripts: false,
        sources: vec![],
        ..Config::default()
    };

    let held = br8n::index::IndexLock::acquire(&db_path).expect("lock must be free");
    let refused = br8n::index_now(&cfg);
    drop(held);

    match prev {
        Some(v) => unsafe { std::env::set_var("BR8N_DB", v) },
        None => unsafe { std::env::remove_var("BR8N_DB") },
    }

    let err = refused.expect_err("index_now must refuse while another writer holds the lock");
    assert!(
        err.to_string().contains("already running"),
        "must fail on the lock, not on something incidental; got: {err}"
    );
}

/// The walker that indexes the user's corpus is `discover_stat_first`, reached
/// through `discover`. `MarkdownLoader::load_all` is a different walker used
/// only by tests, so this asserts against the one that ships.
#[test]
fn discover_honours_the_ignore_list() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path();
    std::fs::create_dir_all(root.join("_templates")).unwrap();
    std::fs::write(
        root.join("real.md"),
        "# Real

Body.
",
    )
    .unwrap();
    std::fs::write(
        root.join("_templates/tpl.md"),
        "# Template

Body.
",
    )
    .unwrap();

    let mut cfg = br8n::config::Config {
        sources: vec![root.to_path_buf()],
        index_transcripts: false,
        ..Config::default()
    };

    let docs = br8n::index::discover(&cfg).unwrap();
    assert_eq!(docs.len(), 1, "the template must not be discovered");
    assert!(docs[0].uri.ends_with("real.md"));

    // Opt-out must work, or a user with real notes under that name is stuck.
    cfg.ignore = Vec::new();
    let all = br8n::index::discover(&cfg).unwrap();
    assert_eq!(all.len(), 2, "an empty ignore list must exclude nothing");
}

/// `discover_stat_first` prints the skip count on stderr, but nothing ever
/// asserted the number — deleting the whole `if ignored > 0 { eprintln!(...) }`
/// block, while leaving the `continue` that does the real exclusion, failed no
/// test. `discover()` throws the count away (it returns only `Vec<Document>`),
/// so this calls `discover_stat_first` directly and reads `Discovered::ignored`.
#[test]
fn discover_stat_first_counts_the_files_it_ignored() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path();
    std::fs::create_dir_all(root.join("_templates")).unwrap();
    std::fs::write(root.join("real.md"), "# Real\n\nBody.\n").unwrap();
    std::fs::write(root.join("_templates/tpl-a.md"), "# A\n\nBody.\n").unwrap();
    std::fs::write(root.join("_templates/tpl-b.md"), "# B\n\nBody.\n").unwrap();

    let cfg = br8n::config::Config {
        sources: vec![root.to_path_buf()],
        index_transcripts: false,
        ..Config::default()
    };

    let (found, _fresh) = br8n::index::discover_stat_first(
        &cfg,
        &Default::default(),
        br8n::index::Deferral::Forbidden,
    )
    .unwrap();
    assert_eq!(found.docs.len(), 1, "the templates must not be discovered");
    assert_eq!(
        found.ignored, 2,
        "both ignored files must be counted, not just excluded"
    );
}

/// `reindex_swap_with` reads `Store::all_lifecycles` and passes it to
/// `Pack::build` at two call sites (the normal path, around :1570, and
/// `compact_swap`, around :1822). Both take the map by reference, and both
/// sites read identically as `&lifecycles` and `&Default::default()` to the
/// type checker — nothing distinguishes "the real map" from "an empty one" at
/// the call site itself. Swap either one for `&Default::default()` and the
/// entire suite stays green except for rustc's `unused variable` warning,
/// which disappears the moment the argument is inlined. That is "feature is
/// inert on the live index while every test passes."
///
/// This drives the REAL CLI end to end — `br8n index` against a scratch
/// vault with `index_transcripts = false` and a local `fake_ollama` stub, no
/// live network or model — so it exercises the actual production wiring
/// rather than calling `Pack::build` directly with a hand-built map (that
/// would only prove `Pack::build` honours its argument, which
/// `tests/it/pack.rs` already does).
///
/// Two documents, one with `status: superseded` in its frontmatter and one
/// with none at all, distinguished at read time by a marker word unique to
/// each so `Pack::bm25` finds the right row deterministically even though
/// `fake_ollama` returns the same vector for every input. The status-less
/// document doubles as a check that this test composes with the `all_lifecycles`
/// fix (`tests/it/store.rs`'s `a_document_with_no_status_key_lands_on_proposed_not_current`):
/// it must publish as `Proposed`, not `Current`.
#[test]
fn a_documents_frontmatter_status_reaches_the_published_pack_as_its_lifecycle() {
    let t = tempfile::tempdir().unwrap();
    let notes = t.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(
        notes.join("old.md"),
        "---\nstatus: superseded\n---\n\n# Old Pooling Notes\n\n\
         Walrus PgBouncer transaction pooling, the superseded write-up.",
    )
    .unwrap();
    std::fs::write(
        notes.join("new.md"),
        "# New Pooling Notes\n\n\
         Narwhal PgBouncer transaction pooling, the current write-up.",
    )
    .unwrap();

    let ollama = common::fake_ollama();
    let cfg_path = t.path().join("config.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "index_transcripts = false\nsources = [\"{}\"]\n\n[embed]\nollama_url = \"{ollama}\"\n",
            notes.display()
        ),
    )
    .unwrap();
    let db = t.path().join("db");

    assert_cmd::Command::cargo_bin("br8n")
        .unwrap()
        .env("BR8N_DB", &db)
        .env("BR8N_CONFIG", &cfg_path)
        .arg("index")
        .assert()
        .success();

    // `model_id` deliberately excludes the ollama host (see `tests/it/cli.rs`'s
    // `publish_a_refused_pack`), so a default config's embed settings are
    // enough to open the pack the binary above just published.
    let embed_cfg = br8n::config::Config::default();
    let model_id =
        br8n::embed::Embedder::model_id(&br8n::embed::OllamaEmbedder::new(&embed_cfg.embed));
    let pack = br8n::pack::open_pack_beside(&db, &model_id, embed_cfg.embed.dimensions)
        .unwrap()
        .expect("`br8n index` must publish a pack");

    let superseded = pack
        .bm25("walrus pooling", 5)
        .unwrap()
        .into_iter()
        .find(|(r, _)| r.uri.ends_with("old.md"))
        .expect("the superseded note must be found by its own marker word");
    assert_eq!(
        superseded.0.lifecycle,
        br8n::pack::status::Lifecycle::Superseded,
        "a document with `status: superseded` in its frontmatter must hydrate \
         as Superseded — MUTATION-CONFIRM: fails if either `Pack::build` call \
         site in src/index.rs is passed `&Default::default()` instead of \
         `&lifecycles`"
    );

    let current = pack
        .bm25("narwhal pooling", 5)
        .unwrap()
        .into_iter()
        .find(|(r, _)| r.uri.ends_with("new.md"))
        .expect("the status-less note must be found by its own marker word");
    assert_eq!(
        current.0.lifecycle,
        br8n::pack::status::Lifecycle::Proposed,
        "a document with no status key at all must hydrate as Proposed, per \
         the ladder in src/pack/status.rs — not Current, which is the value a \
         dropped `all_lifecycles` entry would produce"
    );
}

/// The sibling of the test above, for the OTHER `Pack::build` call site.
///
/// `reindex_swap_with` (around :1570) and `compact_swap` (around :1822) are
/// two separate calls to `Store::all_lifecycles`/`Pack::build`, reached by two
/// separate commands (`br8n index` and `br8n index --compact`), and nothing
/// ties their correctness together — a regression in one call site could ship
/// with the other still green. This test drives `--compact` specifically, so
/// it is a MUTATION-CONFIRM sibling to
/// `a_documents_frontmatter_status_reaches_the_published_pack_as_its_lifecycle`
/// which only ever exercises the plain `br8n index` path.
#[test]
fn compacting_an_index_republishes_the_lifecycle_it_already_carried() {
    let t = tempfile::tempdir().unwrap();
    let notes = t.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(
        notes.join("old.md"),
        "---\nstatus: superseded\n---\n\n# Old Pooling Notes\n\n\
         Walrus PgBouncer transaction pooling, the superseded write-up.",
    )
    .unwrap();
    std::fs::write(
        notes.join("new.md"),
        "# New Pooling Notes\n\n\
         Narwhal PgBouncer transaction pooling, the current write-up.",
    )
    .unwrap();

    let ollama = common::fake_ollama();
    let cfg_path = t.path().join("config.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "index_transcripts = false\nsources = [\"{}\"]\n\n[embed]\nollama_url = \"{ollama}\"\n",
            notes.display()
        ),
    )
    .unwrap();
    let db = t.path().join("db");

    assert_cmd::Command::cargo_bin("br8n")
        .unwrap()
        .env("BR8N_DB", &db)
        .env("BR8N_CONFIG", &cfg_path)
        .arg("index")
        .assert()
        .success();

    // Compact: rebuilds the shadow database FROM the live store's rows, then
    // calls `Store::all_lifecycles`/`Pack::build` at its own call site
    // (`compact_swap`), separate from the one the plain `index` run above
    // already exercised.
    assert_cmd::Command::cargo_bin("br8n")
        .unwrap()
        .env("BR8N_DB", &db)
        .env("BR8N_CONFIG", &cfg_path)
        .args(["index", "--compact"])
        .assert()
        .success();

    let embed_cfg = br8n::config::Config::default();
    let model_id =
        br8n::embed::Embedder::model_id(&br8n::embed::OllamaEmbedder::new(&embed_cfg.embed));
    let pack = br8n::pack::open_pack_beside(&db, &model_id, embed_cfg.embed.dimensions)
        .unwrap()
        .expect("`br8n index --compact` must publish a pack");

    let superseded = pack
        .bm25("walrus pooling", 5)
        .unwrap()
        .into_iter()
        .find(|(r, _)| r.uri.ends_with("old.md"))
        .expect("the superseded note must be found by its own marker word");
    assert_eq!(
        superseded.0.lifecycle,
        br8n::pack::status::Lifecycle::Superseded,
        "a document with `status: superseded` in its frontmatter must still \
         hydrate as Superseded after `br8n index --compact` — \
         MUTATION-CONFIRM: fails if `compact_swap`'s `Pack::build` call in \
         src/index.rs is passed `&Default::default()` instead of \
         `&lifecycles`"
    );

    let current = pack
        .bm25("narwhal pooling", 5)
        .unwrap()
        .into_iter()
        .find(|(r, _)| r.uri.ends_with("new.md"))
        .expect("the status-less note must be found by its own marker word");
    assert_eq!(
        current.0.lifecycle,
        br8n::pack::status::Lifecycle::Proposed,
        "a document with no status key at all must still hydrate as Proposed \
         after `br8n index --compact`, not Current"
    );
}

/// `skipped` is persisted and rendered by `br8n status`, so its order is
/// user-visible. It was the only one of the four collections left unsorted,
/// which parallel loading would have turned into output that reorders on an
/// unchanged corpus.
#[test]
fn the_skip_record_is_sorted() {
    let notes = tempfile::tempdir().unwrap();
    // Two unreadable PDFs. Names chosen so walk order and sorted order differ:
    // `zz` is created first, so an unsorted record hands it back first.
    for name in ["zz-broken.pdf", "aa-broken.pdf"] {
        std::fs::write(notes.path().join(name), b"not a pdf at all").unwrap();
    }
    let cfg = br8n::config::Config {
        sources: vec![notes.path().to_path_buf()],
        index_transcripts: false,
        ..Config::default()
    };

    let (found, _) = br8n::index::discover_stat_first(
        &cfg,
        &std::collections::HashMap::new(),
        br8n::index::Deferral::Forbidden,
    )
    .unwrap();

    assert_eq!(found.skipped.len(), 2, "both bad PDFs must be reported");
    let mut expected = found.skipped.clone();
    expected.sort();
    assert_eq!(found.skipped, expected, "the skip record must be sorted");
}

/// Where the canonicalize failure is NOT reachable from, recorded so the next
/// reader does not spend the afternoon I did looking for it.
///
/// The obvious fixture for `consider_into`'s canonicalize arm is a dangling
/// symlink, and it does not work: `ignore::WalkBuilder` does not follow links,
/// so the entry carries the LINK's own metadata, `is_file()` is false, and the
/// walk `continue`s before `consider_into` is ever called. The arm is pinned
/// one level down instead, by `a_canonicalize_failure_is_recorded_in_the_skip_list`
/// in `src/index.rs`, which calls `consider_into` directly.
///
/// What this test is worth on its own: a dangling link must not become a
/// Document, and must not cost the run its readable notes. What it ALSO does is
/// fail loudly the day `ignore` starts yielding such links as files — at which
/// point the filter below stops being vacuous and should be promoted into an
/// assertion that the entry appears in `skipped`.
#[test]
fn a_dangling_symlink_is_filtered_by_the_walk_before_the_loader_sees_it() {
    let notes = tempfile::tempdir().unwrap();
    std::fs::write(notes.path().join("real.md"), "# Real\n\nbody text\n").unwrap();
    std::os::unix::fs::symlink(
        "/nonexistent/target/for/this/test",
        notes.path().join("dangling.md"),
    )
    .unwrap();

    let cfg = br8n::config::Config {
        sources: vec![notes.path().to_path_buf()],
        index_transcripts: false,
        ..Config::default()
    };

    let (found, _) = br8n::index::discover_stat_first(
        &cfg,
        &std::collections::HashMap::new(),
        br8n::index::Deferral::Forbidden,
    )
    .unwrap();

    // The readable note is unaffected: one bad path must not cost the run.
    assert_eq!(found.docs.len(), 1, "the readable note must still index");

    // MEASURED, not assumed: the walk filters the link, so nothing about it
    // reaches the skip record. This assertion is what turns a future change in
    // `ignore` into a failing test rather than a silently vacuous one.
    let dangling: Vec<&String> = found
        .skipped
        .iter()
        .filter(|s| s.contains("dangling.md"))
        .collect();
    assert!(
        dangling.is_empty(),
        "`ignore::WalkBuilder` now yields a dangling symlink as a file — this \
         test is no longer vacuous, so promote the filter below into a real \
         assertion that it appears in `skipped`: {dangling:?}"
    );
    assert!(
        !found.docs.iter().any(|d| d.uri.ends_with("dangling.md")),
        "a dangling symlink must never become a Document"
    );
}

/// Nested source roots walked the overlapping subtree twice, producing two
/// `Document`s with the same URI and therefore the same id. The sort that is
/// supposed to make document order deterministic is STABLE and keyed on `uri`
/// alone, so a duplicate leaves the tie broken by walk order.
#[test]
fn nested_source_roots_yield_each_document_once() {
    let notes = tempfile::tempdir().unwrap();
    let work = notes.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("note.md"), "# Note\n\nbody text here\n").unwrap();

    let cfg = br8n::config::Config {
        // An overlapping pair: `work` is inside `notes`, and the single
        // fixture file sits in the overlap, so it is reachable from BOTH
        // roots. Walking each root independently visits it twice.
        //
        // The listed ORDER is not part of what this proves, and an earlier
        // comment here claimed it was. `distinct_roots` canonicalizes, sorts
        // and dedups internally, so the caller's order cannot reach the logic
        // under test at all — a "keeps the first entry it sees" fix would see
        // the sorted order, not this one.
        sources: vec![work.clone(), notes.path().to_path_buf()],
        index_transcripts: false,
        ..Config::default()
    };

    let (found, _) = br8n::index::discover_stat_first(
        &cfg,
        &std::collections::HashMap::new(),
        br8n::index::Deferral::Forbidden,
    )
    .unwrap();

    let uris: Vec<&str> = found.docs.iter().map(|d| d.uri.as_str()).collect();
    assert_eq!(
        found.docs.len(),
        1,
        "one file must produce one document, got {uris:?}"
    );

    let ids: std::collections::HashSet<&str> = found.docs.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(
        ids.len(),
        found.docs.len(),
        "two documents must never share an id"
    );
}

/// Phase 1 publishes without waiting for recognition, so it must not OCR — and
/// a PDF that still owes pages must keep its PREVIOUS stamp, or the next pass
/// sees it as unchanged and the pages are never read.
///
/// WHAT THIS TEST CANNOT PIN, and it took a review to notice: on a machine
/// without PDFium and ONNX Runtime, `ocr = auto` falls back to the text layer
/// on its own, so a Skip pass and a Run pass return identical results and every
/// assertion below passes whether or not Skip forced recognition off. It is
/// load-bearing only where the dylibs resolve.
///
/// The forcing itself is therefore pinned machine-independently in two places
/// that need no dylib: `the_skip_pass_forces_ocr_off_and_the_run_pass_does_not`
/// (unit test in `src/index.rs`) pins that Skip builds an `ocr = off` config
/// from the user's `auto`, and the `ocr_attempted` assertion at the foot of
/// this test pins that loading with that config genuinely never calls the
/// engine. `Loaded::ocr_attempted` is not reachable through
/// `discover_stat_first_with`, which returns `Discovered`, so that half runs
/// against `load_file_reporting` directly.
#[test]
fn phase_one_defers_a_scanned_pdf_without_recording_its_stamp() {
    let notes = tempfile::tempdir().unwrap();
    std::fs::copy(
        "tests/fixtures/corpus/mixed.pdf",
        notes.path().join("mixed.pdf"),
    )
    .unwrap();

    // Struct-update syntax, not `default()` then field assignment:
    // `clippy::field_reassign_with_default` fires on the latter and the plan
    // requires zero clippy warnings.
    let mut cfg = br8n::config::Config {
        sources: vec![notes.path().to_path_buf()],
        index_transcripts: false,
        ..Config::default()
    };
    // Auto is the shipped default. The point of the test is that Skip beats it.
    cfg.pdf.ocr = br8n::config::OcrMode::Auto;

    let (found, fresh) = br8n::index::discover_stat_first_with(
        &cfg,
        &std::collections::HashMap::new(),
        br8n::index::Deferral::Forbidden,
        br8n::index::OcrPass::Skip,
    )
    .unwrap();

    assert_eq!(found.ocr_pending.len(), 1, "the mixed PDF owes recognition");
    let uri = &found.ocr_pending[0];
    assert!(uri.ends_with("mixed.pdf"), "wrong file queued: {uri}");
    assert!(
        !fresh.contains_key(uri),
        "a pending PDF must NOT record a current stamp, or the next pass \
         skips it as unchanged and its pages are never read"
    );
    assert_eq!(
        found.docs.len(),
        1,
        "its good pages are still published now"
    );

    // The half that holds on ANY machine. `off` is the config the Skip pass
    // builds (pinned by `the_skip_pass_forces_ocr_off_and_the_run_pass_does_not`
    // in `src/index.rs`), and loading with it must leave `ocr_attempted` false
    // — meaning the engine was never called, rather than called and failed.
    // Where the dylibs are absent this is the only assertion in the test that
    // can tell those two apart, and it is exactly the guarantee Skip exists to
    // make: phase 1 pays no recognition cost.
    let off = br8n::config::PdfConfig {
        ocr: br8n::config::OcrMode::Off,
        ..br8n::config::PdfConfig::default()
    };
    let loaded =
        br8n::loaders::pdf::PdfLoader::load_file_reporting(&notes.path().join("mixed.pdf"), &off)
            .expect("the mixed PDF's text pages still load");
    assert!(
        !loaded.ocr_attempted,
        "phase 1 must never call the recognition engine"
    );
    assert!(
        !loaded.pending_ocr.is_empty(),
        "and must still report what it could not read, or nothing is queued"
    );
}

/// A fully scanned PDF produces no `Document` at all — `load_file_reporting`
/// returns `Err(PdfError::Scanned)`, so the `Err` arm in `consider` is what
/// records it, never the `Ok` arm that populates `docs`. That URI is
/// therefore absent from `found.docs`, and if `live_uris()` forgot
/// `ocr_pending` the way it once did, `prune_missing` would delete the
/// previously-indexed copy of a document that this two-pass feature exists to
/// serve. A mixed PDF (see the test above) is safe because its readable pages
/// still land in `docs`; a fully scanned one has no such fallback.
#[test]
fn a_pdf_awaiting_ocr_is_not_pruned_from_the_index() {
    let notes = tempfile::tempdir().unwrap();
    std::fs::copy(
        "tests/fixtures/corpus/scanned.pdf",
        notes.path().join("scanned.pdf"),
    )
    .unwrap();

    let cfg = br8n::config::Config {
        sources: vec![notes.path().to_path_buf()],
        index_transcripts: false,
        ..Config::default()
    };

    let (found, _fresh) = br8n::index::discover_stat_first_with(
        &cfg,
        &std::collections::HashMap::new(),
        br8n::index::Deferral::Forbidden,
        br8n::index::OcrPass::Skip,
    )
    .unwrap();

    assert_eq!(
        found.ocr_pending.len(),
        1,
        "the fully scanned PDF must be queued for a later OCR pass"
    );
    assert!(
        found.docs.is_empty(),
        "a fully scanned PDF has no usable pages and must produce no document"
    );
    let uri = &found.ocr_pending[0];
    assert!(uri.ends_with("scanned.pdf"), "wrong file queued: {uri}");

    let live = found.live_uris();
    assert!(
        live.iter().any(|u| u == uri),
        "a PDF awaiting OCR must stay in live_uris, or prune_missing deletes \
         the previously-indexed copy of the one document this feature exists \
         to serve: {live:?}"
    );
}

/// Phase 2 must run only when phase 1 left something, and must never fail the
/// run — the index is already published by the time it starts.
///
/// This asserts the GUARD, not recognition itself: OCR needs PDFium and ONNX
/// Runtime, which the suite must not require. A corpus with no scanned pages
/// must leave `ocr_pending` empty, which is what keeps the second pass — a
/// second seed copy, a second stat walk and a second full pack build — off
/// every ordinary run.
#[test]
fn a_corpus_with_no_scanned_pages_queues_no_second_pass() {
    let notes = tempfile::tempdir().unwrap();
    std::fs::write(notes.path().join("note.md"), "# Note\n\nbody\n").unwrap();
    std::fs::copy(
        "tests/fixtures/corpus/paper.pdf",
        notes.path().join("paper.pdf"),
    )
    .unwrap();

    // Struct-update syntax, not `default()` then field assignment:
    // `clippy::field_reassign_with_default` fires on the latter and the plan
    // requires zero clippy warnings.
    let cfg = br8n::config::Config {
        sources: vec![notes.path().to_path_buf()],
        index_transcripts: false,
        ..Config::default()
    };

    let (found, _) = br8n::index::discover_stat_first_with(
        &cfg,
        &std::collections::HashMap::new(),
        br8n::index::Deferral::Forbidden,
        br8n::index::OcrPass::Skip,
    )
    .unwrap();

    assert!(
        found.ocr_pending.is_empty(),
        "a text PDF and a note owe no recognition, got {:?}",
        found.ocr_pending
    );
    assert_eq!(found.docs.len(), 2, "both files still index in phase 1");
}

/// CORRECTION B. The anti-retry rule in a `Run` pass exists for exactly one
/// failure: a PDF that was genuinely attempted with recognition ON and still
/// produced no usable pages (`PdfError::Scanned`, matched via `downcast_ref`
/// just above it in `consider`). Any OTHER loader failure in a `Run` pass — a
/// corrupt file, a transient I/O error, anything that is not "scanned" — must
/// record NO stamp at all, exactly as an ordinary incremental pass would, so
/// the file is retried on the very next run instead of silently disappearing
/// from the index until its mtime or size next changes.
///
/// A `.pdf` file whose bytes are not a PDF at all is the fixture that proves
/// the distinction: `load_file_reporting` fails with a genuine parse error,
/// not `PdfError::Scanned` — that variant only fires when the file opens fine
/// and yields zero readable pages (see `scanned.pdf` above), not when it
/// cannot be opened as a PDF at all.
#[test]
fn a_non_scanned_pdf_failure_in_the_ocr_pass_records_no_stamp() {
    let notes = tempfile::tempdir().unwrap();
    std::fs::write(
        notes.path().join("garbage.pdf"),
        b"this is not a pdf at all, just bytes",
    )
    .unwrap();

    let cfg = br8n::config::Config {
        sources: vec![notes.path().to_path_buf()],
        index_transcripts: false,
        ..Config::default()
    };

    let (found, fresh) = br8n::index::discover_stat_first_with(
        &cfg,
        &std::collections::HashMap::new(),
        br8n::index::Deferral::Forbidden,
        br8n::index::OcrPass::Run,
    )
    .unwrap();

    assert!(
        found.docs.is_empty(),
        "a file that fails to parse produces no document"
    );
    assert!(
        found.ocr_pending.is_empty(),
        "OcrPass::Run must never populate ocr_pending"
    );

    let canon = notes.path().join("garbage.pdf").canonicalize().unwrap();
    let expected_uri = format!("file://{}", canon.display());
    assert!(
        !fresh.contains_key(&expected_uri),
        "a non-scanned failure in a Run pass must record no stamp, or the \
         file silently disappears from the index until its mtime or size \
         next changes: {fresh:?}"
    );
}

/// THE FINDING: `consider_into` recorded `ocr_pending` and withheld the
/// current stamp for any PDF that still owed pages, REGARDLESS of `[pdf] ocr`
/// — while `reindex_swap_inner` launches phase 2 only when `ocr != "off"`. The
/// two decisions disagreed, and with recognition turned off there is no pass
/// that can ever clear what phase 1 queues.
///
/// For a MIXED PDF the cost is this test: it never receives a current stamp,
/// so every run re-reads and re-chunks it. `OcrMode::Off` documents itself as
/// restoring the pre-OCR behaviour exactly, and CLAUDE.md's "a no-op run costs
/// 0s" is the invariant it breaks.
///
/// Two consecutive passes is the whole point — the second is what proves the
/// first actually recorded a stamp, which is a claim no single pass can make.
#[test]
fn ocr_off_stamps_a_mixed_pdf_instead_of_deferring_it_forever() {
    let notes = tempfile::tempdir().unwrap();
    std::fs::copy(
        "tests/fixtures/corpus/mixed.pdf",
        notes.path().join("mixed.pdf"),
    )
    .unwrap();

    let mut cfg = br8n::config::Config {
        sources: vec![notes.path().to_path_buf()],
        index_transcripts: false,
        ..Config::default()
    };
    // The whole subject of the test. `Auto` is the shipped default.
    cfg.pdf.ocr = br8n::config::OcrMode::Off;

    let canon = notes.path().join("mixed.pdf").canonicalize().unwrap();
    let uri = format!("file://{}", canon.display());

    let (first, fresh) = br8n::index::discover_stat_first(
        &cfg,
        &std::collections::HashMap::new(),
        br8n::index::Deferral::Allowed,
    )
    .unwrap();

    assert!(
        first.ocr_pending.is_empty(),
        "with OCR off there is no second pass to hand work to, so nothing may \
         be queued: {:?}",
        first.ocr_pending
    );
    assert_eq!(first.docs.len(), 1, "its readable pages still index");
    assert!(
        fresh.contains_key(&uri),
        "a mixed PDF read with OCR off is as complete as it will ever be and \
         must be stamped: {fresh:?}"
    );

    // Feed the first run's stamps back in, exactly as `reindex_swap_inner`
    // does via `load_stamps`.
    let (second, _) =
        br8n::index::discover_stat_first(&cfg, &fresh, br8n::index::Deferral::Allowed).unwrap();

    assert!(
        second.docs.is_empty(),
        "nothing changed on disk, so the second pass must open no file — \
         re-reading it here is the permanent re-chunking this pins"
    );
    assert!(
        second.unchanged.iter().any(|u| u == &uri),
        "the PDF must come back as unchanged: {:?}",
        second.unchanged
    );
    assert!(
        second.ocr_pending.is_empty(),
        "still nothing queued: {:?}",
        second.ocr_pending
    );
}

/// The other half of the same finding, and the more expensive half. A FULLY
/// scanned PDF produces no document at all, so with OCR off it used to land in
/// `ocr_pending` from the `Err` arm — and a nonzero `ocr_backlog` blocks
/// `reindex_swap_inner`'s unchanged-corpus fast path on EVERY run (a seed copy,
/// a rebuild and a full pack build on every `SessionStart`) while the only pass
/// that could drain it is switched off.
#[test]
fn ocr_off_queues_no_backlog_for_a_fully_scanned_pdf() {
    let notes = tempfile::tempdir().unwrap();
    std::fs::copy(
        "tests/fixtures/corpus/scanned.pdf",
        notes.path().join("scanned.pdf"),
    )
    .unwrap();

    let mut cfg = br8n::config::Config {
        sources: vec![notes.path().to_path_buf()],
        index_transcripts: false,
        ..Config::default()
    };
    cfg.pdf.ocr = br8n::config::OcrMode::Off;

    let (found, _) = br8n::index::discover_stat_first(
        &cfg,
        &std::collections::HashMap::new(),
        br8n::index::Deferral::Allowed,
    )
    .unwrap();

    assert!(
        found.ocr_pending.is_empty(),
        "a backlog nothing can drain keeps the fast path shut forever: {:?}",
        found.ocr_pending
    );
    assert_eq!(
        found.skipped.len(),
        1,
        "it is still reported as skipped, exactly as it was before OCR existed"
    );
}

/// THE FINDING, and the one with the data-loss consequence. `OCR_DISABLED` is
/// a PROCESS-WIDE latch: the first PDF that finds PDFium or ONNX Runtime
/// unusable turns recognition off for the rest of the run, so every scanned
/// PDF after it fails with the same `Err(PdfError::Scanned)` having never been
/// looked at.
///
/// The `Run` arm did not distinguish the two. It recorded the CURRENT stamp
/// (the anti-retry rule) and did NOT add the URI to `ocr_pending`, so the URI
/// was absent from `live_uris()` and `prune_missing` DELETED the indexed copy
/// — of a document nothing had even attempted to read. Before this branch the
/// next run restored it; the anti-retry stamp makes the deletion PERMANENT,
/// recoverable only by `--reindex`.
///
/// FORCING "never attempted" DETERMINISTICALLY, on any machine: load a file
/// whose bytes are not a PDF with recognition ON. `process_pdf_with_ocr`
/// fails on it — on a machine WITH the libraries because the bytes are
/// garbage, on a machine without them because the dylib will not load — and
/// either way `disable_ocr` latches. No environment variable is touched, so
/// this races nothing else in the binary, and the latch only ever moves one
/// way. The `!mixed.ocr_attempted` assertion below is the precondition check:
/// it fails loudly rather than letting the real assertions pass vacuously.
#[test]
fn a_scanned_pdf_ocr_never_reached_is_neither_stamped_nor_pruned() {
    let staging = tempfile::tempdir().unwrap();
    let garbage = staging.path().join("garbage.pdf");
    std::fs::write(&garbage, b"this is not a pdf at all, just bytes").unwrap();

    let ocr_on = br8n::config::PdfConfig {
        ocr: br8n::config::OcrMode::Auto,
        ..br8n::config::PdfConfig::default()
    };

    // Trips the latch. Its own result is irrelevant — the file cannot load by
    // any route — so it is deliberately not asserted on.
    let _ = br8n::loaders::pdf::PdfLoader::load_file_with(&garbage, &ocr_on);

    // Precondition, and the only observable the latch has from out here:
    // `Loaded::ocr_attempted` on a file that DOES load.
    let mixed = br8n::loaders::pdf::PdfLoader::load_file_reporting(
        std::path::Path::new("tests/fixtures/corpus/mixed.pdf"),
        &ocr_on,
    )
    .expect("mixed.pdf still loads from its text layer");
    assert!(
        !mixed.ocr_attempted,
        "the latch did not trip, so this test would be asserting nothing"
    );

    let notes = tempfile::tempdir().unwrap();
    std::fs::copy(
        "tests/fixtures/corpus/scanned.pdf",
        notes.path().join("scanned.pdf"),
    )
    .unwrap();

    let mut cfg = br8n::config::Config {
        sources: vec![notes.path().to_path_buf()],
        index_transcripts: false,
        ..Config::default()
    };
    cfg.pdf.ocr = br8n::config::OcrMode::Auto;

    let canon = notes.path().join("scanned.pdf").canonicalize().unwrap();
    let uri = format!("file://{}", canon.display());

    // Phase 2: the pass that recognizes. It never got as far as recognizing
    // this one.
    let (found, fresh) = br8n::index::discover_stat_first_with(
        &cfg,
        &std::collections::HashMap::new(),
        br8n::index::Deferral::Allowed,
        br8n::index::OcrPass::Run,
    )
    .unwrap();

    // THE LOAD-BEARING PAIR. Either one alone loses the document: without the
    // first, `prune_missing` deletes the indexed copy; without the second, the
    // stamp makes that deletion permanent.
    assert!(
        found.live_uris().iter().any(|u| u == &uri),
        "a PDF recognition never reached must stay in live_uris, or \
         prune_missing deletes the indexed copy of a document nothing looked \
         at: {:?}",
        found.live_uris()
    );
    assert!(
        !fresh.contains_key(&uri),
        "the anti-retry stamp is only honest for a file OCR actually tried; \
         recording it here makes the deletion permanent, recoverable only by \
         --reindex: {fresh:?}"
    );
}

/// THE FINDING: `reindex_swap_inner`'s unchanged-corpus fast path (around
/// :1680) checked only whether the stamp key sets matched, never whether
/// phase 1 left an OCR backlog. A fully scanned PDF produces no `Document` —
/// `load_file_reporting` returns `Err(PdfError::Scanned)`, and the `Err` arm
/// only pushes the URI to `ocr_pending` — so it never reaches `probe.docs`.
/// Being brand new, it has no previous stamp to withhold either, so
/// `probe_stamps` ends up with exactly the same key set as `stamps`.
/// `unchanged_corpus` therefore reads true on the very run that discovers the
/// PDF, the fast path returns before the phase-2 block, and phase 2 — the
/// only pass that can ever recognise it — never runs. Not just that one run:
/// every later run reaches the identical fast path for the identical reason,
/// so the PDF sits unrecognised until some unrelated file's stamp changes and
/// drags the corpus onto the slow path by coincidence.
///
/// This drives the real CLI end to end (`assert_cmd`, a scratch `BR8N_DB`/
/// `BR8N_CONFIG` per invocation, `fake_ollama` for embedding — the same shape
/// `a_documents_frontmatter_status_reaches_the_published_pack_as_its_lifecycle`
/// above uses) rather than asserting on the fast-path condition in isolation,
/// because `reindex_swap_inner` is private to `src/index.rs` and cannot be
/// called from here, and because the bug is precisely about which CODE PATH a
/// second `br8n index` run takes — a property of the whole function, not
/// just of one boolean.
///
/// The observable: on the run that reaches phase 2, `src/index.rs` prints
/// `"br8n: {n} PDF(s) still need recognition; running the OCR pass"` to
/// stderr UNCONDITIONALLY, before it even attempts OCR — so this needs
/// neither PDFium nor ONNX Runtime to be installed, and does not care whether
/// recognition itself succeeds. Its absence is exactly the fast-path bug: the
/// line can only be missing if phase 2 never started.
#[test]
fn a_pending_ocr_backlog_forces_the_second_pass_instead_of_the_no_op_fast_path() {
    let t = tempfile::tempdir().unwrap();
    let notes = t.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(
        notes.join("note.md"),
        "# Ordinary Note\n\nNothing scanned about this one.",
    )
    .unwrap();

    let ollama = common::fake_ollama();
    let cfg_path = t.path().join("config.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "index_transcripts = false\nsources = [\"{}\"]\n\n[embed]\nollama_url = \"{ollama}\"\n",
            notes.display()
        ),
    )
    .unwrap();
    let db = t.path().join("db");

    let br8n = || {
        let mut c = assert_cmd::Command::cargo_bin("br8n").unwrap();
        c.env("BR8N_DB", &db).env("BR8N_CONFIG", &cfg_path);
        c.arg("index");
        c
    };

    // Settle: first run indexes the ordinary note and writes the initial
    // `db.stamps`. Not yet touching the fast path at all — this is a genuine
    // "corpus changed" run.
    br8n().assert().success();

    // A real no-op: nothing changed, no PDF anywhere, so the fast path must
    // fire and phase 2 must not even be considered. Sanity check on the
    // harness itself — if this ever fails, the test below proves nothing.
    let noop = br8n().assert().success();
    let noop_stderr = String::from_utf8_lossy(&noop.get_output().stderr).into_owned();
    assert!(
        !noop_stderr.contains("still need recognition"),
        "a corpus with no PDFs at all must never queue a second pass: {noop_stderr}"
    );

    // Drop a fully-scanned PDF into the settled corpus — the exact case this
    // whole two-pass feature exists to serve.
    std::fs::copy(
        "tests/fixtures/corpus/scanned.pdf",
        notes.join("scanned.pdf"),
    )
    .unwrap();

    let with_backlog = br8n().assert().success();
    let stderr = String::from_utf8_lossy(&with_backlog.get_output().stderr).into_owned();
    assert!(
        stderr.contains("still need recognition"),
        "a run that discovers a fully-scanned PDF must not take the \
         unchanged-corpus fast path — phase 2 must start so the PDF is ever \
         recognised, but no phase-2 announcement appeared on stderr: {stderr}"
    );
}

/// Parallel loading must not change WHAT is discovered, only how fast. This
/// pins the invariants that a threaded walk can break: every document present,
/// exactly once, in a deterministic order, with a stamp for each.
#[test]
fn parallel_loading_finds_the_same_corpus_in_the_same_order() {
    let notes = tempfile::tempdir().unwrap();
    // Enough files to actually occupy several workers, named so that walk
    // order and sorted order differ.
    for i in 0..40 {
        std::fs::write(
            notes.path().join(format!("note-{:02}.md", 39 - i)),
            format!("# Note {i}\n\nbody text for note {i}\n"),
        )
        .unwrap();
    }
    // Struct-update syntax, not `default()` then field assignment:
    // `clippy::field_reassign_with_default` fires on the latter and the plan
    // requires zero clippy warnings.
    let cfg = br8n::config::Config {
        sources: vec![notes.path().to_path_buf()],
        index_transcripts: false,
        ..Config::default()
    };

    let run = || {
        br8n::index::discover_stat_first(
            &cfg,
            &std::collections::HashMap::new(),
            br8n::index::Deferral::Forbidden,
        )
        .unwrap()
    };

    let (first, first_stamps) = run();
    assert_eq!(first.docs.len(), 40, "every file must be found");
    assert_eq!(first_stamps.len(), 40, "every file must record a stamp");

    let uris: Vec<&str> = first.docs.iter().map(|d| d.uri.as_str()).collect();
    let mut sorted = uris.clone();
    sorted.sort_unstable();
    assert_eq!(uris, sorted, "document order must be deterministic");

    let ids: std::collections::HashSet<&str> = first.docs.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids.len(), 40, "no document may be discovered twice");

    // Repeatability is the half that a threaded walk breaks.
    let (second, _) = run();
    let second_uris: Vec<&str> = second.docs.iter().map(|d| d.uri.as_str()).collect();
    assert_eq!(uris, second_uris, "two runs must agree on order");
}

/// "Prompts preempt indexing" is a stated invariant, and parallel loading is
/// exactly what could quietly break it: the gate used to sit on the PDF arm
/// alone, so markdown and transcripts never yielded at all.
///
/// Driven as a SUBPROCESS with `.env`, not by setting `BR8N_DB` in this
/// process. Rust runs this file's tests on parallel threads, and an in-process
/// `set_var` is precisely the unsoundness Task 4 removes from the library —
/// reintroducing it one file away would be absurd. `hook_contract.rs` and
/// `transcript_deferral_cli.rs` already use this shape.
///
/// PAIRED, because a single timing assertion cannot tell a gate that parked
/// from a machine that was simply slow. Two identical corpora, one with a
/// fresh query marker beside its database and one without; the difference is
/// the gate. `MARKER_FRESH` is 3s and the marker is written once, so the
/// parked run stalls about 3s and then proceeds as the marker goes stale.
///
/// `--no-embed` because the suite must not need a live Ollama. Discovery — and
/// therefore the gate — runs either way.
#[test]
fn a_waiting_query_parks_the_loading_pool() {
    fn corpus() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        let notes = t.path().join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        for i in 0..4 {
            std::fs::write(
                notes.join(format!("note-{i}.md")),
                format!("# Note {i}\n\nbody text for note {i}\n"),
            )
            .unwrap();
        }
        std::fs::write(
            t.path().join("config.toml"),
            format!(
                "sources = [\"{}\"]\nindex_transcripts = false\n",
                notes.display()
            ),
        )
        .unwrap();
        t
    }

    fn run(t: &tempfile::TempDir) -> std::time::Duration {
        let db = t.path().join("db");
        let started = std::time::Instant::now();
        assert_cmd::Command::cargo_bin("br8n")
            .unwrap()
            .env("BR8N_DB", &db)
            .env("BR8N_CONFIG", t.path().join("config.toml"))
            .args(["index", "--no-embed"])
            .assert()
            .success();
        started.elapsed()
    }

    // Throwaway warm-up: the FIRST subprocess this test harness spawns pays a
    // one-off tax (the harness binary is ~244MB; the first fork/exec pages it
    // in and does dyld work the second exec does not) that has nothing to do
    // with the query gate. Whichever timed arm ran first was absorbing that
    // entire tax alone, which is not a theory — it was observed directly:
    // "unblocked 5.479208042s against blocked 3.310557083s", the UNBLOCKED
    // (baseline, run first) arm slower than the blocked one, while invoking
    // the same binary directly outside the harness gave the expected ~3s gap
    // (0.4s unblocked, 3.3s blocked). Running one untimed invocation here,
    // over its own scratch corpus, moves that fixed cost before both timed
    // runs so baseline and parked pay it equally instead of one arm eating it
    // whole.
    let warmup = corpus();
    run(&warmup);

    let free = corpus();
    let baseline = run(&free);

    let blocked = corpus();
    // Another reader's marker. `a_query_is_waiting` scans for any fresh
    // `db.qwait.*` beside the database and does NOT exclude any pid, so this
    // process's own number names a marker the child will honour.
    let db = blocked.path().join("db");
    std::fs::create_dir_all(&db).unwrap();
    let theirs = db.with_extension(format!("qwait.{}", std::process::id()));
    std::fs::write(&theirs, b"").unwrap();
    let parked = run(&blocked);

    assert!(
        parked >= baseline + std::time::Duration::from_millis(1500),
        "a fresh query marker must park the loading pool: unblocked {baseline:?} \
         against blocked {parked:?}, so the gate is not being consulted"
    );
    // The paired assertion above is RELATIVE, and on a loaded machine a
    // pathologically slow baseline satisfies it for the wrong reason: if the
    // gate were dead and both arms took 4s and 5.6s, the difference alone
    // still reads as a park. Absolute floor as well, so a run that never
    // waited cannot pass however slow it was — this corpus is four tiny notes
    // and no honest unblocked run comes near it.
    assert!(
        parked >= std::time::Duration::from_millis(1500),
        "the blocked run finished in {parked:?}, which is less than the wait \
         itself — the gate cannot have been honoured at all"
    );
}

#[test]
fn a_query_marker_is_seen_within_its_window_and_not_after() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    std::fs::create_dir_all(&db).unwrap();
    assert!(!br8n::index::a_query_was_seen_within(
        &db,
        std::time::Duration::from_secs(60)
    ));
    let _p = br8n::index::QueryPriority::announce(&db);
    assert!(br8n::index::a_query_was_seen_within(
        &db,
        std::time::Duration::from_secs(60)
    ));
    assert!(!br8n::index::a_query_was_seen_within(
        &db,
        std::time::Duration::from_secs(0)
    ));
}

#[test]
fn threads_racing_for_the_index_lock_never_both_hold_it() {
    use br8n::index::IndexLock;
    use std::sync::{Arc, Barrier};

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    let racers = 8;
    for round in 0..200 {
        let start = Arc::new(Barrier::new(racers));
        let holders: Vec<_> = (0..racers)
            .map(|_| {
                let (db, start) = (db.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    IndexLock::acquire(&db)
                })
            })
            .collect();
        let held: Vec<IndexLock> = holders
            .into_iter()
            .filter_map(|h| h.join().unwrap())
            .collect();
        assert_eq!(
            held.len(),
            1,
            "round {round}: {} threads held the lock at once",
            held.len()
        );
    }
}

#[test]
fn sweeping_query_markers_removes_only_those_of_dead_processes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    let dead = db.with_extension("qwait.999999");
    let live = db.with_extension(format!("qwait.{}", std::process::id()));
    let unrelated = db.with_extension("qwait.notapid");
    for marker in [&dead, &live, &unrelated] {
        std::fs::write(marker, b"q").unwrap();
    }

    br8n::index::sweep_dead_query_markers(&db);

    assert!(!dead.exists(), "a dead process's marker must be swept");
    assert!(live.exists(), "a live process's marker must be kept");
    assert!(
        unrelated.exists(),
        "a file that names no pid is not ours to remove"
    );
}

#[test]
fn an_index_run_sweeps_the_markers_dead_processes_left_behind() {
    let t = tempfile::tempdir().unwrap();
    let notes = t.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(notes.join("a.md"), "# A\n\nbody\n").unwrap();
    let config = t.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "sources = [\"{}\"]\nindex_transcripts = false\n",
            notes.display()
        ),
    )
    .unwrap();
    let db = t.path().join("db");
    let dead = db.with_extension("qwait.999999");
    std::fs::write(&dead, b"q").unwrap();

    assert_cmd::Command::cargo_bin("br8n")
        .unwrap()
        .env("BR8N_DB", &db)
        .env("BR8N_CONFIG", &config)
        .args(["index", "--no-embed"])
        .assert()
        .success();

    assert!(
        !dead.exists(),
        "an index run must sweep a dead process's marker"
    );
}
