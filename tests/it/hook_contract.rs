use crate::common;

use assert_cmd::Command;
use br8n::hook::{build_context, should_retrieve};
use br8n::store::Hit;

fn hook(tmp: &std::path::Path, stdin: &str) -> assert_cmd::assert::Assert {
    Command::cargo_bin("br8n")
        .unwrap()
        .env("BR8N_DB", tmp.join("db"))
        .env("BR8N_CONFIG", tmp.join("config.toml"))
        .args(["hook", "prompt"])
        .write_stdin(stdin.to_string())
        .assert()
}

#[test]
fn exits_zero_when_the_index_does_not_exist() {
    let t = tempfile::tempdir().unwrap();
    hook(
        t.path(),
        r#"{"prompt":"how did I configure the connection pooler"}"#,
    )
    .success();
}

#[test]
fn exits_zero_on_malformed_json() {
    let t = tempfile::tempdir().unwrap();
    hook(t.path(), "not json at all {{{").success();
}

#[test]
fn exits_zero_on_empty_stdin() {
    let t = tempfile::tempdir().unwrap();
    hook(t.path(), "").success();
}

#[test]
fn exits_zero_when_ollama_is_unreachable() {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(
        t.path().join("config.toml"),
        "[embed]\nollama_url = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();
    hook(t.path(), r#"{"prompt":"anything at all here"}"#).success();
}

#[test]
fn produces_no_output_when_there_is_nothing_to_inject() {
    let t = tempfile::tempdir().unwrap();
    let out = Command::cargo_bin("br8n")
        .unwrap()
        .env("BR8N_DB", t.path().join("db"))
        .env("BR8N_CONFIG", t.path().join("config.toml"))
        .args(["hook", "prompt"])
        .write_stdin(r#"{"prompt":"how did I configure the pooler"}"#)
        .output()
        .unwrap();
    assert!(out.stdout.is_empty(), "silence costs zero tokens");
}

#[test]
fn short_prompts_are_not_worth_retrieving_on() {
    assert!(!should_retrieve("yes"));
    assert!(!should_retrieve("continue"));
    assert!(!should_retrieve("now fix it"));
    assert!(!should_retrieve(""));
    assert!(should_retrieve("how did I configure the connection pooler"));
}

#[test]
fn context_block_is_tagged_and_carries_sources() {
    let h = Hit {
        chunk_id: "c".into(),
        doc_id: "d".into(),
        text: "PgBouncer transaction mode.".into(),
        heading_path: "Pooling".into(),
        uri: "file:///n/pool.md".into(),
        title: "Pooling".into(),
        page_no: None,
        score: 0.9,
        relevance: 0.9,
        source_type: "markdown".into(),
        inbound: 0,
        lifecycle: Default::default(),
        last_used: None,
        memory: None,
    };
    let ctx = build_context(&[h], 1500).unwrap();
    assert!(ctx.starts_with("<br8n-context>"));
    assert!(ctx.ends_with("</br8n-context>"));
    assert!(ctx.contains("file:///n/pool.md"));
    assert!(ctx.contains("PgBouncer"));
}

#[test]
fn context_respects_the_token_budget() {
    let big = Hit {
        chunk_id: "c".into(),
        doc_id: "d".into(),
        text: "word ".repeat(5000),
        heading_path: String::new(),
        uri: "file:///n/a.md".into(),
        title: "A".into(),
        page_no: None,
        score: 0.9,
        relevance: 0.9,
        source_type: "markdown".into(),
        inbound: 0,
        lifecycle: Default::default(),
        last_used: None,
        memory: None,
    };
    let ctx = build_context(&[big], 100).unwrap();
    assert!(
        ctx.len() < 100 * 4 + 400,
        "budget must actually bound the output"
    );
}

#[test]
fn no_hits_yields_no_context_block() {
    assert!(build_context(&[], 1500).is_none());
}

/// Companion to `retrieve_profile.rs`'s
/// `a_zero_budget_at_tier_1_skips_bm25_when_the_deadline_trips`, which asserts
/// only on the in-process `StageReport` and never touches `hook.rs` or stderr
/// at all — it was previously (mis)named
/// `a_degraded_retrieval_is_reported_on_stderr`. This test drives the real
/// `br8n` binary end to end and checks the actual line `run_prompt` prints
/// to stderr in `src/hook.rs` when `report.degraded` is true, so deleting
/// that `eprintln!` fails a test again.
#[test]
fn a_blown_budget_prints_the_degraded_line_on_stderr() {
    let t = tempfile::tempdir().unwrap();
    let notes = t.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(
        notes.join("n.md"),
        "# Pooling\n\nPgBouncer runs in transaction mode and drops session state.",
    )
    .unwrap();

    // The hook defaults to tier 1 (`budget_ms: 220`). `retrieve::run`'s clock
    // starts before the query embed call, so a stub that holds every
    // response for 500ms guarantees the deadline trips deterministically —
    // no timing luck, and no live Ollama (`fake_ollama_delayed` is a local
    // TCP stub, same family as the one `reindex_safety.rs` already uses).
    let ollama = common::fake_ollama_delayed(std::time::Duration::from_millis(500));
    let cfg = t.path().join("config.toml");
    // `index_transcripts` must be turned off explicitly: `Config::load` leaves
    // it `true` by default, and a config that only sets `[embed]` would scan
    // the developer's real `~/.claude/projects` (see `reindex_safety.rs`).
    std::fs::write(
        &cfg,
        format!(
            "index_transcripts = false\nsources = [\"{}\"]\n\n[embed]\nollama_url = \"{ollama}\"\n",
            notes.display()
        ),
    )
    .unwrap();
    let db = t.path().join("db");

    Command::cargo_bin("br8n")
        .unwrap()
        .env("BR8N_DB", &db)
        .env("BR8N_CONFIG", &cfg)
        .arg("index")
        .assert()
        .success();

    let out = Command::cargo_bin("br8n")
        .unwrap()
        .env("BR8N_DB", &db)
        .env("BR8N_CONFIG", &cfg)
        .args(["hook", "prompt"])
        .write_stdin(r#"{"prompt":"why did the connection pooler drop sessions"}"#)
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "the hook must still exit 0 even when retrieval degrades"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("br8n: retrieval degraded"),
        "expected the degraded line on stderr, got: {stderr:?}"
    );
}

/// Companion to `retrieve_profile.rs`'s
/// `tier_1_without_a_pack_refuses_rather_than_falling_back_to_the_store`,
/// which proves the in-process `Result` in isolation but cannot prove
/// anything reads it or prints it — that assertion would stay green even if
/// `run_prompt`'s `eprintln!` for the `Err` arm were deleted. This test
/// drives the real `br8n` binary end to end: index normally (which always
/// publishes a pack — see `reindex_swap`), delete every `pack.*` file it
/// wrote so `open_pack_beside` sees no manifest, then confirm the hook
/// refuses on stderr rather than silently answering with nothing, and still
/// exits 0.
#[test]
fn a_no_pack_index_refuses_and_reports_it_on_stderr() {
    let t = tempfile::tempdir().unwrap();
    let notes = t.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(
        notes.join("n.md"),
        "# Pooling\n\nPgBouncer runs in transaction mode and drops session state.",
    )
    .unwrap();

    let ollama = common::fake_ollama();
    let cfg = t.path().join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            "index_transcripts = false\nsources = [\"{}\"]\n\n[embed]\nollama_url = \"{ollama}\"\n",
            notes.display()
        ),
    )
    .unwrap();
    let db = t.path().join("db");

    Command::cargo_bin("br8n")
        .unwrap()
        .env("BR8N_DB", &db)
        .env("BR8N_CONFIG", &cfg)
        .arg("index")
        .assert()
        .success();

    // Remove every artifact the pack published beside the store (see
    // `src/pack/{manifest,records,vectors,postings}.rs` for the `pack.*`
    // filenames) so `open_pack_beside` reports "no pack" — the only way to
    // reach a packless index with a current binary, which always publishes
    // one.
    for entry in std::fs::read_dir(&db).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name().to_string_lossy().starts_with("pack.") {
            std::fs::remove_file(entry.path()).unwrap();
        }
    }

    let out = Command::cargo_bin("br8n")
        .unwrap()
        .env("BR8N_DB", &db)
        .env("BR8N_CONFIG", &cfg)
        .args(["hook", "prompt"])
        .write_stdin(r#"{"prompt":"why did the connection pooler drop sessions"}"#)
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "the hook must still exit 0 on a packless index"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("retrieval unavailable"),
        "a packless index must be reported on stderr, got: {stderr:?}"
    );
    assert!(
        stderr.contains("br8n index"),
        "the reported reason must name the repair, got: {stderr:?}"
    );
    assert!(
        out.stdout.is_empty(),
        "stdout carries the hook's JSON contract; diagnostics belong on stderr, got: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// A pack the binary must REFUSE, published where `open_pack_beside` looks for
/// it: directly beside the database directory `BR8N_DB` names.
///
/// Duplicated from `tests/it/cli.rs` on purpose — `tests/common/mod.rs` is
/// shared by files outside this binary too, and must not grow a dependency
/// on `br8n::pack` internals.
///
/// Built for real (`Pack::build`) at the configured model and dimensions, then
/// its manifest's `analyzer` is rewritten — the one corruption that makes a
/// structurally perfect pack unreadable, because postings built by one analyzer
/// and queried by another come back wrong with no error. Every other file is
/// genuine, so nothing but the refusal itself can explain a silent result.
///
/// Needs no Ollama: `retrieve_for` opens the pack before it ever reaches an
/// embedder, so the process fails at validation with no round trip.
fn publish_a_refused_pack(db: &std::path::Path) {
    use br8n::pack::records::Record;

    std::fs::create_dir_all(db).unwrap();
    let cfg = br8n::config::Config::default();
    let dims = cfg.embed.dimensions;
    let model_id = br8n::embed::Embedder::model_id(&br8n::embed::OllamaEmbedder::new(&cfg.embed));

    // Sorted by `chunk_id`: `Pack::build` refuses anything else.
    let rows: Vec<(Record, Vec<f32>)> = (0..3)
        .map(|i| {
            let mut v = vec![0.0f32; dims];
            v[i] = 1.0;
            (
                Record {
                    chunk_id: format!("c{i}"),
                    doc_id: format!("d{i}"),
                    text: format!("pgbouncer transaction pooling note {i}"),
                    heading_path: String::new(),
                    uri: format!("file:///{i}.md"),
                    title: format!("Note {i}"),
                    page_no: None,
                    source_type: "markdown".into(),
                    inbound: 0,
                    lifecycle: Default::default(),
                    last_used: None,
                    memory: None,
                },
                v,
            )
        })
        .collect();
    br8n::pack::Pack::build(
        db,
        &model_id,
        dims,
        rows,
        &Default::default(),
        &Default::default(),
    )
    .unwrap();

    let mut m = br8n::pack::manifest::Manifest::read(db).unwrap();
    m.analyzer = "something-else/9".into();
    m.write(db).unwrap();

    // The fixture is only worth anything if this really is a refusal, and one
    // whose cause is the analyzer. `Ok(None)` would be a degrade to the store
    // and `Ok(Some(_))` an accepted pack; either would make the assertions
    // below vacuous.
    let err = br8n::pack::open_pack_beside(db, &model_id, dims).unwrap_err();
    assert!(format!("{err:#}").contains("analyzer"), "got: {err:#}");
}

/// The THIRD stderr line in `run_prompt`, and the one that fires when
/// retrieval does not run at all.
///
/// `a_blown_budget_prints_the_degraded_line_on_stderr` exercises the
/// `Ok((hits, report))` arm — a pipeline that RAN and lost a stage. This test
/// exercises the `Err(e)` arm through a different trigger (a pack this binary
/// refuses to read at all, on manifest validation) than
/// `a_no_pack_index_refuses_and_reports_it_on_stderr` above (no pack beside
/// the index) — both land on the same `eprintln!("br8n: retrieval
/// unavailable — …")`, and this one is what first proved it had a test at
/// all.
///
/// That is the line the user actually feels: on this path the hook returns
/// before `build_context`, so the prompt gets no context whatsoever, and the
/// hook still exits 0 by design. Without the line, a broken index and a
/// knowledge base with nothing to say are the same observable event.
///
/// Three properties, the same three `search_reports_a_refused_pack_on_stderr_
/// instead_of_answering_empty` pins for `br8n search`:
///   (a) the cause reaches STDERR,
///   (b) STDOUT stays clean — it carries the hook's JSON contract, and a
///       diagnostic printed there would be parsed by Claude Code as one,
///   (c) the exit code stays 0, so the hook NEVER blocks a prompt.
#[test]
fn the_hook_reports_an_unavailable_retriever_on_stderr_and_still_exits_zero() {
    let t = tempfile::tempdir().unwrap();
    publish_a_refused_pack(&t.path().join("db"));

    // This machine may well have a live Ollama, and a test that passes only
    // because one is listening proves nothing about the offline suite. Point
    // the binary's embedder at a closed port: `model_id` deliberately excludes
    // the host, so the pack fixture above still matches the model the binary
    // computes, and any embedding attempt is now a connection refusal rather
    // than a silent round trip. (`retrieve_for` refuses the pack before the
    // embedder is ever used, so nothing here depends on which error wins.)
    std::fs::write(
        t.path().join("config.toml"),
        "[embed]\nollama_url = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();

    let out = Command::cargo_bin("br8n")
        .unwrap()
        .env("BR8N_DB", t.path().join("db"))
        .env("BR8N_CONFIG", t.path().join("config.toml"))
        .args(["hook", "prompt"])
        // Four words or more, or `should_retrieve` returns before any of this
        // is reached and the test would pass for the wrong reason.
        .write_stdin(r#"{"prompt":"why did the connection pooler drop sessions"}"#)
        .output()
        .unwrap();

    // (c) exit 0.
    assert_eq!(
        out.status.code(),
        Some(0),
        "the hook must never block a prompt, whatever retrieval did"
    );

    // (a) the failure, and its cause, reach stderr.
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("retrieval unavailable"),
        "a retriever that cannot be built must be reported on stderr, got: {err:?}"
    );
    assert!(
        err.contains("analyzer"),
        "the reported reason must name the actual cause, got: {err:?}"
    );

    // (b) stdout stays the hook's contract channel and nothing else. Empty is
    // the whole contract here: `run_prompt` returns before `build_context`, so
    // there is no `hookSpecificOutput` to print.
    assert!(
        out.stdout.is_empty(),
        "stdout carries the hook's JSON contract; diagnostics belong on stderr, got: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn a_prompt_that_times_out_on_a_remote_starts_a_background_load() {
    let t = tempfile::tempdir().unwrap();
    let notes = t.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(
        notes.join("n.md"),
        "# Pooling\n\nPgBouncer runs in transaction mode and drops session state.",
    )
    .unwrap();
    let ollama = common::fake_ollama();
    let cfg = t.path().join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            "index_transcripts = false\nsources = [\"{}\"]\n\n[embed]\nollama_url = \"{ollama}\"\n",
            notes.display()
        ),
    )
    .unwrap();
    let db = t.path().join("db");
    Command::cargo_bin("br8n")
        .unwrap()
        .env("BR8N_DB", &db)
        .env("BR8N_CONFIG", &cfg)
        .arg("index")
        .assert()
        .success();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let remote = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            held.push(stream);
            let _ = tx.send(());
        }
    });

    let out = Command::cargo_bin("br8n")
        .unwrap()
        .env("BR8N_DB", &db)
        .env("BR8N_CONFIG", &cfg)
        .env("BR8N_EMBED_URL", &remote)
        .env("BR8N_EMBED_MODEL", "wire-name")
        .env("BR8N_EMBED_TOKEN", "tok")
        .args(["hook", "prompt"])
        .write_stdin(r#"{"prompt":"why did the connection pooler drop sessions"}"#)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("loading it in the background"),
        "expected the background-load line, got: {stderr:?}"
    );

    let wait = std::time::Duration::from_secs(30);
    rx.recv_timeout(wait).expect("the prompt's own query");
    rx.recv_timeout(wait)
        .expect("the background load must connect to the remote after the prompt timed out");
}
