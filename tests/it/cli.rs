use crate::common;

use assert_cmd::Command;
use predicates::str::contains;

fn br8n(tmp: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("br8n").unwrap();
    c.env("BR8N_DB", tmp.join("db"));
    c.env("BR8N_CONFIG", tmp.join("config.toml"));
    c.env("PATH", "/usr/bin:/bin");
    c
}

#[test]
fn status_on_a_fresh_machine_reports_empty_and_exits_zero() {
    let t = tempfile::tempdir().unwrap();
    br8n(t.path())
        .arg("status")
        .assert()
        .success()
        .stdout(contains("documents"));
}

#[test]
fn search_with_no_index_exits_zero_with_no_results() {
    let t = tempfile::tempdir().unwrap();
    br8n(t.path())
        .args(["search", "anything"])
        .assert()
        .success();
}

#[test]
fn json_output_is_machine_readable() {
    let t = tempfile::tempdir().unwrap();
    let out = br8n(t.path())
        .args(["search", "anything", "--json"])
        .output()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(parsed["results"].is_array());
}

#[test]
fn quality_flag_is_accepted_and_clamped() {
    let t = tempfile::tempdir().unwrap();
    br8n(t.path())
        .args(["search", "x", "--quality", "99"])
        .assert()
        .success();
}

#[test]
fn unknown_subcommand_fails_with_usage() {
    let t = tempfile::tempdir().unwrap();
    br8n(t.path())
        .arg("frobnicate")
        .assert()
        .failure()
        .stderr(contains("Usage"));
}

/// A pack the binary must REFUSE, published where `open_pack_beside` looks for
/// it: directly beside the database directory `BR8N_DB` names.
///
/// Built for real (`Pack::build`) at the configured model and dimensions, then
/// its manifest's `analyzer` is rewritten — the one corruption that makes a
/// structurally perfect pack unreadable, because postings built by one analyzer
/// and queried by another come back wrong with no error. Every other file is
/// genuine, so nothing but the refusal itself can explain a silent result.
///
/// Needs no Ollama: `build_retriever` opens the pack before it ever reaches an
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

/// `br8n search` must never answer a refused pack the way it answers an honest
/// no-match. The pack below validates on nothing but its analyzer, so the index
/// cannot be read at all — and the user typing this command is usually asking
/// why the hook did not inject something.
///
/// Three properties, all of which the old `.unwrap_or_default()` on this path
/// broke together while looking perfectly healthy:
///   (a) the cause reaches STDERR,
///   (b) STDOUT stays clean, so `--json` still parses,
///   (c) the exit code stays 0, so the hook never blocks a prompt.
///
/// Only (a) distinguishes the fix from the defect: a swallowed error produces
/// the same empty `results` array and the same zero exit status, which is
/// exactly why this was invisible for so long.
#[test]
fn search_reports_a_refused_pack_on_stderr_instead_of_answering_empty() {
    let t = tempfile::tempdir().unwrap();
    publish_a_refused_pack(&t.path().join("db"));

    // This machine may well have a live Ollama, and a test that passes only
    // because one is listening proves nothing about the offline suite. Point
    // the binary's embedder at a closed port: `model_id` deliberately excludes
    // the host, so the pack fixture above still matches the model the binary
    // computes, and any embedding attempt is now a connection refusal rather
    // than a silent round trip.
    std::fs::write(
        t.path().join("config.toml"),
        "[embed]\nollama_url = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();

    let json = br8n(t.path())
        .args(["search", "pooling", "--json"])
        .output()
        .unwrap();

    // (c) exit 0.
    assert_eq!(json.status.code(), Some(0), "search must exit 0");

    // (a) the refusal, and its cause, reach stderr.
    let err = String::from_utf8_lossy(&json.stderr);
    assert!(
        err.contains("search unavailable"),
        "a refused pack must be reported on stderr, got stderr: {err:?}"
    );
    assert!(
        err.contains("analyzer"),
        "the reported reason must name the actual cause, got stderr: {err:?}"
    );

    // (b) stdout is still nothing but parseable JSON — no diagnostics leaked
    // into the result channel.
    let out = String::from_utf8_lossy(&json.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("stdout not JSON ({e}): {out}"));
    assert_eq!(parsed["results"].as_array().map(|a| a.len()), Some(0));

    // The human path has the same obligation: `no results` on stdout is the
    // honest-no-match phrasing, so the difference has to be on stderr.
    let human = br8n(t.path()).args(["search", "pooling"]).output().unwrap();
    assert_eq!(human.status.code(), Some(0), "search must exit 0");
    let human_err = String::from_utf8_lossy(&human.stderr);
    assert!(
        human_err.contains("search unavailable") && human_err.contains("analyzer"),
        "got stderr: {human_err:?}"
    );
}

/// A structurally valid pack whose `pack.status` side-file covers the WRONG
/// number of rows — as if a status file from a stale generation survived
/// beside a fresh manifest, records, vectors and postings.
///
/// Built for real (`Pack::build`), exactly like `publish_a_refused_pack`, then
/// `pack.status` is overwritten with one row fewer than the pack actually
/// has. Every other file is genuine, so `Pack::open` must take the
/// mismatched-length branch and nothing else.
fn publish_a_pack_with_a_mismatched_status_file(db: &std::path::Path) -> (String, usize) {
    use br8n::pack::records::Record;

    std::fs::create_dir_all(db).unwrap();
    let cfg = br8n::config::Config::default();
    let dims = cfg.embed.dimensions;
    let model_id = br8n::embed::Embedder::model_id(&br8n::embed::OllamaEmbedder::new(&cfg.embed));

    let n = 3;
    let rows: Vec<(Record, Vec<f32>)> = (0..n)
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

    // Overwrite the side-file `Pack::build` just wrote with one covering
    // `n - 1` rows instead of `n` — a real generation mismatch, not a
    // truncated/corrupt file (that is `status::Reader::open`'s own error
    // branch, a different `eprintln!` a few lines below this one).
    br8n::pack::status::write(db, &vec![br8n::pack::status::Lifecycle::Superseded; n - 1]).unwrap();

    (model_id, dims)
}

/// A structurally valid pack whose `pack.status` side-file is not a
/// row-count mismatch but outright unreadable — a bad magic number, as if
/// the file were truncated mid-write or clobbered by something else
/// entirely.
///
/// Built for real (`Pack::build`), exactly like the mismatched-length sibling
/// above, then `pack.status` is overwritten with 16 header-sized bytes that
/// do not start with `status::Reader`'s magic (`BRNSTAT1`). This is the
/// SIBLING branch of `Pack::open`'s status handling: the mismatched-length
/// test above never reaches `status::Reader::open`'s own `Err` arm, and this
/// one never reaches the `Ok(r) if r.rows() != m.rows` arm.
fn publish_a_pack_with_a_corrupt_status_file(db: &std::path::Path) -> (String, usize) {
    use br8n::pack::records::Record;

    std::fs::create_dir_all(db).unwrap();
    let cfg = br8n::config::Config::default();
    let dims = cfg.embed.dimensions;
    let model_id = br8n::embed::Embedder::model_id(&br8n::embed::OllamaEmbedder::new(&cfg.embed));

    let n = 3;
    let rows: Vec<(Record, Vec<f32>)> = (0..n)
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

    // Overwrite the side-file with a 16-byte header (long enough to pass the
    // truncation check) whose first 8 bytes are not the magic `BRNSTAT1` —
    // `status::Reader::open`'s bad-magic `ensure!`, not the row-count check.
    let mut bad = Vec::with_capacity(16);
    bad.extend_from_slice(b"NOTVALID");
    bad.extend_from_slice(&(n as u32).to_le_bytes());
    bad.extend_from_slice(&1u32.to_le_bytes());
    std::fs::write(db.join(br8n::pack::status::STATUS_FILE), &bad).unwrap();

    (model_id, dims)
}

/// `Pack::open` must not silently swallow a `pack.status` it cannot join —
/// deleting its two `eprintln!` calls and collapsing both match arms to
/// `None` leaves every existing pack test green, because none of them checks
/// stderr. This is the project's named house failure mode (silent
/// degradation), applied to the one side-file whose whole design point was
/// to degrade LOUDLY rather than demote every document with nobody noticing.
///
/// Unlike the refused-pack test above, this pack still OPENS — a status
/// mismatch demotes nothing rather than refusing the pack outright — so the
/// binary reaches the embedder and needs one that actually answers;
/// `common::fake_ollama` serves that without any real network or model.
///
/// Same three properties as the refused-pack and bm25-unavailable tests:
/// (a) the cause reaches stderr, (b) stdout stays clean JSON, (c) exit 0.
#[test]
fn search_reports_a_mismatched_status_file_on_stderr_instead_of_ranking_everything_current() {
    let t = tempfile::tempdir().unwrap();
    publish_a_pack_with_a_mismatched_status_file(&t.path().join("db"));

    let ollama = common::fake_ollama();
    std::fs::write(
        t.path().join("config.toml"),
        format!("[embed]\nollama_url = \"{ollama}\"\n"),
    )
    .unwrap();

    let out = br8n(t.path())
        .args(["search", "pooling", "--quality", "1", "--json"])
        .output()
        .unwrap();

    // (c) exit 0.
    assert_eq!(out.status.code(), Some(0), "search must exit 0");

    // (a) the mismatch, and its remedy, reach stderr.
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("pack.status covers") && err.contains("ignoring it"),
        "a mismatched pack.status must be reported on stderr, got: {err:?}"
    );
    assert!(
        err.contains("--compact"),
        "the message must name the cheap remedy, got: {err:?}"
    );

    // (b) stdout is still nothing but parseable JSON, and the pack still
    // ANSWERS — a status mismatch demotes nothing, it does not refuse.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not JSON ({e}): {stdout}"));
    let results = parsed["results"]
        .as_array()
        .unwrap_or_else(|| panic!("no results array: {stdout}"));
    assert!(
        !results.is_empty(),
        "a mismatched status file must still let the pack answer, got: {stdout}"
    );
}

/// The sibling of the test above, for `Pack::open`'s OTHER `pack.status`
/// error branch. The row-count-mismatch test drives
/// `Ok(r) if r.rows() != m.rows`; this one drives the plain `Err(e)` arm —
/// `status::Reader::open` refusing a file with a bad magic number — which has
/// its own `eprintln!` and was unpinned before this test.
///
/// Same three properties as the row-count-mismatch test: (a) the cause
/// reaches stderr, (b) stdout stays clean JSON, (c) exit 0.
#[test]
fn search_reports_an_unreadable_status_file_on_stderr_instead_of_ranking_everything_current() {
    let t = tempfile::tempdir().unwrap();
    publish_a_pack_with_a_corrupt_status_file(&t.path().join("db"));

    let ollama = common::fake_ollama();
    std::fs::write(
        t.path().join("config.toml"),
        format!("[embed]\nollama_url = \"{ollama}\"\n"),
    )
    .unwrap();

    let out = br8n(t.path())
        .args(["search", "pooling", "--quality", "1", "--json"])
        .output()
        .unwrap();

    // (c) exit 0.
    assert_eq!(out.status.code(), Some(0), "search must exit 0");

    // (a) the cause reaches stderr — the OTHER branch's message, not the
    // row-count one.
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("pack.status unreadable"),
        "an unreadable pack.status must be reported on stderr, got: {err:?}"
    );

    // (b) stdout is still nothing but parseable JSON, and the pack still
    // ANSWERS — a corrupt status file demotes nothing, it does not refuse.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not JSON ({e}): {stdout}"));
    let results = parsed["results"]
        .as_array()
        .unwrap_or_else(|| panic!("no results array: {stdout}"));
    assert!(
        !results.is_empty(),
        "an unreadable status file must still let the pack answer, got: {stdout}"
    );
}

/// A tiny real corpus plus the config the `br8n()` helper above points at.
///
/// `index_transcripts = false` is not optional. `Config::load` never fails and
/// defaults that flag to `true`, so a config that only sets `[embed]` sends
/// `br8n index` into the developer's real `~/.claude/projects` — see the same
/// note on `reindex_safety.rs`'s `corpus`.
///
/// `common::fake_ollama` is a local TCP stub owned by this test process, so
/// indexing needs no network and no live model.
fn corpus(dir: &std::path::Path) {
    let ollama = common::fake_ollama();
    let notes = dir.join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    for i in 0..3 {
        std::fs::write(
            notes.join(format!("n{i}.md")),
            format!(
                "# Pooling {i}\n\nPgBouncer runs in transaction mode and drops \
                 session state, note {i}."
            ),
        )
        .unwrap();
    }
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "index_transcripts = false\nsources = [\"{}\"]\n\n[embed]\nollama_url = \"{ollama}\"\n",
            notes.display()
        ),
    )
    .unwrap();
}

/// Delete every artifact the pack published beside the store (see
/// `src/pack/{manifest,records,vectors,postings}.rs` for the `pack.*`
/// filenames), leaving a database that is still perfectly valid.
///
/// This is the store-fallback condition: `open_pack_beside` finds no manifest,
/// reports "no pack" rather than an error, and `build_retriever` opens the
/// store instead. The count is asserted because a rename that changed the
/// `pack.` prefix would silently turn every caller into a no-op.
fn remove_the_pack(db: &std::path::Path) {
    let mut removed = 0;
    for entry in std::fs::read_dir(db).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name().to_string_lossy().starts_with("pack.") {
            std::fs::remove_file(entry.path()).unwrap();
            removed += 1;
        }
    }
    assert!(
        removed > 0,
        "the index must have published a pack to remove"
    );
}

#[test]
fn search_reports_a_packless_index_on_stderr_instead_of_answering_empty() {
    let t = tempfile::tempdir().unwrap();
    corpus(t.path());
    let db = t.path().join("db");

    br8n(t.path())
        .args(["index", "--no-embed"])
        .assert()
        .success();
    remove_the_pack(&db);

    let out = br8n(t.path())
        .args(["search", "pooling", "--quality", "1", "--json"])
        .output()
        .unwrap();

    // (c) exit 0.
    assert_eq!(out.status.code(), Some(0), "search must exit 0");

    // (a) the refusal reaches stderr and names the repair.
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("search unavailable"),
        "a packless index must be reported on stderr, got: {err:?}"
    );
    assert!(
        err.contains("br8n index"),
        "the reported reason must name the repair, got: {err:?}"
    );

    // (b) stdout stays parseable JSON. Empty is correct here — a refused
    // index answers nothing — which is precisely why (a) is the only thing
    // that tells a broken index from an honest no-match.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not JSON ({e}): {stdout}"));
    assert_eq!(parsed["results"].as_array().map(|a| a.len()), Some(0));
}

/// The DEADLINE degradation reaches stderr from `br8n search`, not only from
/// the hook.
///
/// `hook_contract.rs::a_blown_budget_prints_the_degraded_line_on_stderr` has
/// pinned the hook's copy of this `eprintln!` since it was written. The copy in
/// `Cmd::Search` did not exist at all, and the asymmetry was backwards: the
/// hook's user sees a prompt that quietly lacked a document, while THIS command
/// exists to answer "why didn't the hook inject X". A silent stage skip here
/// made the diagnostic tool hide the diagnosis.
///
/// Found by being misled by it. On the live index, one query over 14 runs at
/// tier 1: the single run that took 384ms — past the 220ms budget — returned a
/// result set missing a document the other 13 all found, and printed nothing.
/// It reads as run-to-run nondeterminism in retrieval, which sends you looking
/// at the wrong code entirely.
///
/// Same trigger as the hook's test, and for the same reason: `retrieve::run`'s
/// clock starts before the query embed, so a stub holding every response for
/// 500ms blows the 220ms budget deterministically — no timing luck, no live
/// Ollama.
#[test]
fn search_reports_a_blown_budget_on_stderr_instead_of_silently_skipping_stages() {
    let t = tempfile::tempdir().unwrap();
    let notes = t.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(
        notes.join("n.md"),
        "# Pooling\n\nPgBouncer runs in transaction mode and drops session state.",
    )
    .unwrap();

    let ollama = common::fake_ollama_delayed(std::time::Duration::from_millis(500));
    // `index_transcripts = false` is not optional: `Config::load` leaves it
    // true, and a config that only sets `[embed]` scans the developer's real
    // `~/.claude/projects`.
    std::fs::write(
        t.path().join("config.toml"),
        format!(
            "index_transcripts = false\nsources = [\"{}\"]\n\n[embed]\nollama_url = \"{ollama}\"\n",
            notes.display()
        ),
    )
    .unwrap();

    br8n(t.path()).arg("index").assert().success();

    // Tier 1 explicitly — the hook's own tier, and the one whose 220ms budget
    // the 500ms stub is chosen to blow.
    let out = br8n(t.path())
        .args(["search", "pooling", "--quality", "1", "--json"])
        .output()
        .unwrap();

    // Exit 0: a degradation is not a failure.
    assert_eq!(out.status.code(), Some(0), "search must still exit 0");

    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("budget") && err.contains("exceeded"),
        "a blown budget must name itself on stderr, not skip stages in silence; got: {err:?}"
    );
    // The message must say WHAT ran, or it cannot be acted on — "degraded" alone
    // does not tell you which retriever went missing.
    assert!(
        err.contains("ran only ["),
        "the line must name the stages that did run; got: {err:?}"
    );

    // stdout stays parseable JSON — the degradation is reported beside the
    // answer, never instead of it.
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not JSON ({e}): {stdout}"));
}
#[test]
fn the_pack_rows_lifecycle_byte_demotes_a_superseded_document() {
    let t = tempfile::tempdir().unwrap();
    let ollama = common::fake_ollama();
    let notes = t.path().join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(
        notes.join("old.md"),
        "---\nstatus: superseded\nsuperseded-by: ADR-9\n---\n\n\
         # Bazel\n\nBazel builds our code and defines the machines.",
    )
    .unwrap();

    let base = format!(
        "index_transcripts = false\nsources = [\"{}\"]\n\n[embed]\nollama_url = \"{ollama}\"\n",
        notes.display()
    );
    let cfg_on = t.path().join("config.toml"); // shipped default: 0.88
    std::fs::write(&cfg_on, &base).unwrap();
    let cfg_off = t.path().join("off.toml");
    std::fs::write(&cfg_off, format!("{base}\n[weights]\nsuperseded = 1.0\n")).unwrap();

    br8n(t.path()).arg("index").assert().success();

    let score = |cfg: &std::path::Path| -> f64 {
        let out = Command::cargo_bin("br8n")
            .unwrap()
            .env("BR8N_DB", t.path().join("db"))
            .env("BR8N_CONFIG", cfg)
            .args([
                "search",
                "bazel builds our code",
                "--quality",
                "1",
                "--json",
            ])
            .output()
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        v["results"]
            .as_array()
            .and_then(|rs| {
                rs.iter()
                    .find(|r| r["uri"].as_str().is_some_and(|u| u.ends_with("old.md")))
            })
            .and_then(|r| r["relevance"].as_f64())
            .unwrap_or_else(|| panic!("the superseded document was not retrieved at all: {v}"))
    };

    let off = score(&cfg_off);
    let on = score(&cfg_on);

    assert!(
        on < off,
        "the pack row's lifecycle byte must demote this hit; \
         got {on} with the feature on against {off} with it off"
    );
    // By the shipped amount, not merely "less": a token difference would pass
    // the line above while leaving the record above the gate.
    let expected = off * 0.88;
    assert!(
        (on - expected).abs() < 1e-4,
        "expected {expected} (undemoted x 0.88), got {on}"
    );
}

#[test]
fn br8n_memory_add_list_forget_round_trip() {
    let t = tempfile::tempdir().unwrap();
    let url = common::fake_ollama();
    std::fs::write(
        t.path().join("config.toml"),
        format!("sources = []\nindex_transcripts = false\n\n[embed]\nollama_url = \"{url}\"\n"),
    )
    .unwrap();
    let run = |args: &[&str]| {
        assert_cmd::Command::cargo_bin("br8n")
            .unwrap()
            .env("BR8N_DB", t.path().join("db"))
            .env("BR8N_CONFIG", t.path().join("config.toml"))
            .args(args)
            .output()
            .unwrap()
    };
    let added = run(&[
        "memory", "add", "--kind", "lesson", "Never", "comment", "code", "unless", "asked.",
    ]);
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let stdout = String::from_utf8_lossy(&added.stdout);
    assert!(stdout.starts_with("Saved lesson "), "{stdout}");
    let id = stdout
        .trim()
        .trim_end_matches('.')
        .rsplit(' ')
        .next()
        .unwrap()
        .to_string();

    let listed = run(&["memory", "list", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["id"], id);
    assert_eq!(v[0]["facts"]["kind"], "lesson");
    assert_eq!(v[0]["facts"]["origin"], "user");
    assert_eq!(v[0]["facts"]["confidence"], 100);

    let status = run(&["status"]);
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("memory:     1 lesson"),
        "{}",
        String::from_utf8_lossy(&status.stdout)
    );

    let forgot = run(&["memory", "forget", &id]);
    assert!(
        forgot.status.success(),
        "{}",
        String::from_utf8_lossy(&forgot.stderr)
    );
    let listed = run(&["memory", "list", "--json"]);
    assert_eq!(String::from_utf8_lossy(&listed.stdout).trim(), "[]");
}

#[test]
fn br8n_memory_add_rejects_short_text_with_nonzero_exit() {
    let t = tempfile::tempdir().unwrap();
    let url = common::fake_ollama();
    std::fs::write(
        t.path().join("config.toml"),
        format!("sources = []\nindex_transcripts = false\n\n[embed]\nollama_url = \"{url}\"\n"),
    )
    .unwrap();
    let added = assert_cmd::Command::cargo_bin("br8n")
        .unwrap()
        .env("BR8N_DB", t.path().join("db"))
        .env("BR8N_CONFIG", t.path().join("config.toml"))
        .args(["memory", "add", "--kind", "lesson", "too", "short"])
        .output()
        .unwrap();
    assert!(
        !added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stdout)
    );
    let stdout = String::from_utf8_lossy(&added.stdout);
    assert!(stdout.contains("Not saved"), "{stdout}");
}

fn installed_hook_bin_path(t: &std::path::Path) -> std::path::PathBuf {
    t.join("bin").join("br8n")
}

#[test]
fn status_reports_the_hook_binary_and_compares_it_by_content() {
    let t = tempfile::tempdir().unwrap();
    let out = br8n(t.path()).arg("status").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);

    assert!(
        text.contains("build:"),
        "status must print this build's own content hash; got:\n{text}"
    );
    assert!(
        text.contains("hook binary:"),
        "status must name the hook's binary even when it is missing; got:\n{text}"
    );
    assert!(
        text.contains("not installed yet"),
        "a missing hook binary is a real, plain state and must say so; got:\n{text}"
    );
}

#[test]
fn status_says_same_as_this_binary_when_the_hook_binary_is_a_byte_identical_copy() {
    let t = tempfile::tempdir().unwrap();
    let hook_path = installed_hook_bin_path(t.path());
    std::fs::create_dir_all(hook_path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_br8n"), &hook_path).unwrap();

    let out = br8n(t.path()).arg("status").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);

    assert!(
        text.contains(&format!("hook binary:{}", hook_path.display())),
        "status must name the exact path it compared; got:\n{text}"
    );
    assert!(
        text.contains("same as this binary"),
        "a hook binary that is a byte-identical copy of this test binary must \
         compare equal; got:\n{text}"
    );
    assert!(
        !text.contains("MISMATCH"),
        "must not report MISMATCH when the hook binary is a copy of this one; got:\n{text}"
    );
}

#[test]
fn status_reports_mismatch_with_a_fix_line_when_the_hook_binary_differs() {
    let t = tempfile::tempdir().unwrap();
    let hook_path = installed_hook_bin_path(t.path());
    std::fs::create_dir_all(hook_path.parent().unwrap()).unwrap();
    std::fs::write(&hook_path, b"not actually a br8n binary").unwrap();

    let out = br8n(t.path()).arg("status").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);

    assert!(
        text.contains("MISMATCH — the hook runs a different build"),
        "a hook binary with different content must be reported as a mismatch; got:\n{text}"
    );
    assert!(
        text.contains(&format!("fix: rm -f {} && cp", hook_path.display())),
        "a mismatch must print the rm-then-cp fix line naming the hook path; got:\n{text}"
    );
}

/// `br8n audit-injections` over an empty root must say it found nothing, not
/// print a table of zeros — an empty ledger and a broken parser must not look
/// the same.
#[test]
fn audit_injections_says_so_when_the_ledger_is_empty() {
    let t = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let out = br8n(t.path())
        .args(["audit-injections", "--root"])
        .arg(root.path())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("no injections found"), "got:\n{text}");
    assert!(out.status.success(), "an empty ledger is not an error");
}

#[test]
#[cfg(unix)]
fn index_lowers_its_own_priority_to_nice_ten() {
    let t = tempfile::tempdir().unwrap();
    corpus(t.path());

    let out = br8n(t.path())
        .env("BR8N_REPORT_NICENESS", "1")
        .args(["index", "--no-embed"])
        .output()
        .unwrap();

    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("br8n: niceness 10"),
        "index must report niceness 10 after lowering its own scheduling \
         priority; got: {err:?}"
    );
}
