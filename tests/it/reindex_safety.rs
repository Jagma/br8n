use crate::common;

use assert_cmd::Command;
use std::path::{Path, PathBuf};

/// Every test must write a config. `Config::load()` never fails — a missing file
/// yields defaults, and `index_transcripts` defaults to `true`, so a test that
/// only points `BR8N_CONFIG` at a nonexistent path scans the developer's real
/// `~/.claude/projects`. That happened here: 283 files, 75 MB, two concurrent
/// runs still going after four minutes.
fn corpus(dir: &Path, n: usize) -> PathBuf {
    let ollama = common::fake_ollama();
    let notes = dir.join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    for i in 0..n {
        std::fs::write(
            notes.join(format!("n{i}.md")),
            format!("# Note {i}\n\nConnection pooling and PgBouncer transaction mode, entry {i}."),
        )
        .unwrap();
    }
    let cfg = dir.join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            "index_transcripts = false\nsources = [\"{}\"]\n\n[embed]\nollama_url = \"{ollama}\"\n",
            notes.display()
        ),
    )
    .unwrap();
    cfg
}

/// A `fake_ollama` whose embedding actually depends on the input text.
///
/// `common::fake_ollama` returns the identical vector for every input, which
/// is fine for tests that only check filesystem orchestration but useless
/// here: with every document embedded to the same point, a compaction bug
/// that zeroed or otherwise corrupted every vector would be invisible to a
/// search-based check — a uniformly wrong embedding still ranks candidates
/// exactly like a uniformly identical one. `Store::insert_chunks`/
/// `fit_dimensions` normalize whatever comes back, so the raw values here
/// only need to differ per input, not already be unit vectors.
fn content_aware_fake_ollama() -> String {
    use std::io::{Read, Write};
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
            let inputs: Vec<String> = received
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|end| &received[end + 4..])
                .and_then(|body| serde_json::from_slice::<serde_json::Value>(body).ok())
                .and_then(|v| {
                    v["input"].as_array().map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                })
                .unwrap_or_else(|| vec!["x".to_string()]);
            let one = |s: &str| -> String {
                // The hashing trick: bag-of-tokens into a 512-dim vector, each
                // token hashed to a bucket with a hashed sign. Unlike hashing
                // the whole string (which gives two DIFFERENT strings
                // essentially uncorrelated vectors — no good for a
                // similarity-based test), documents that share vocabulary
                // land close together and a query finds the note whose words
                // it actually shares, the way a real embedding would.
                let mut vec = [0.0f64; 512];
                for tok in s
                    .to_lowercase()
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|t| !t.is_empty())
                {
                    let mut h: u64 = 0xcbf29ce484222325;
                    for b in tok.bytes() {
                        h ^= b as u64;
                        h = h.wrapping_mul(0x100000001b3);
                    }
                    let bucket = (h % 512) as usize;
                    let sign = if (h >> 63) & 1 == 1 { 1.0 } else { -1.0 };
                    vec[bucket] += sign;
                }
                let vals: Vec<String> = vec.iter().map(|v| format!("{v:.3}")).collect();
                format!("[{}]", vals.join(","))
            };
            let body = format!(
                r#"{{"embeddings":[{}]}}"#,
                inputs.iter().map(|s| one(s)).collect::<Vec<_>>().join(",")
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

/// `corpus`, but backed by `content_aware_fake_ollama` so distinct notes get
/// distinct embeddings — needed by any test that checks WHICH document a
/// query matches, not just how many documents exist.
fn content_aware_corpus(dir: &Path, n: usize) -> PathBuf {
    let ollama = content_aware_fake_ollama();
    let notes = dir.join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    for i in 0..n {
        std::fs::write(
            notes.join(format!("n{i}.md")),
            format!("# Note {i}\n\nConnection pooling and PgBouncer transaction mode, entry {i}."),
        )
        .unwrap();
    }
    let cfg = dir.join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            "index_transcripts = false\nsources = [\"{}\"]\n\n[embed]\nollama_url = \"{ollama}\"\n",
            notes.display()
        ),
    )
    .unwrap();
    cfg
}

/// The `uri` of `br8n search --json`'s top hit for `query`.
fn top_hit_uri(db: &Path, cfg: &Path, query: &str) -> String {
    let out = br8n(db, cfg)
        .args(["search", query, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON from --json");
    parsed["results"][0]["uri"]
        .as_str()
        .expect("at least one result with a uri")
        .to_string()
}

fn touch_all(dir: &Path, n: usize, marker: &str) {
    for i in 0..n {
        let p = dir.join("notes").join(format!("n{i}.md"));
        let mut body = std::fs::read_to_string(&p).unwrap();
        body.push_str(&format!("\n{marker}\n"));
        std::fs::write(&p, body).unwrap();
    }
}

fn br8n(db: &Path, cfg: &Path) -> Command {
    let mut c = Command::cargo_bin("br8n").unwrap();
    c.env("BR8N_DB", db).env("BR8N_CONFIG", cfg);
    c
}

/// Document count from `br8n status`'s stdout, the same field every other
/// test in this file already parses inline — pulled out once so the
/// compaction test below doesn't repeat it a fifth time.
fn docs_in(db: &Path, cfg: &Path) -> usize {
    let out = br8n(db, cfg)
        .arg("status")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8_lossy(&out)
        .lines()
        .find_map(|l| l.strip_prefix("documents:")?.trim().parse().ok())
        .unwrap_or(0)
}

/// The pending-vectors figure from `br8n status`'s stdout — `docs_in`'s
/// sibling for phase 2's backlog. `Cmd::Status` renders it as
/// `chunks:     N (M vectors pending — run \`br8n index --backfill\`)` when
/// there is a backlog, and as a bare `chunks:     N` when there is not — the
/// latter is what makes `unwrap_or(0)` correct rather than merely convenient.
fn pending_count(db: &Path, cfg: &Path) -> usize {
    let out = br8n(db, cfg)
        .arg("status")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8_lossy(&out)
        .lines()
        .find_map(|l| {
            let rest = l.strip_prefix("chunks:")?;
            let after_paren = rest.split('(').nth(1)?;
            after_paren.split_whitespace().next()?.parse().ok()
        })
        .unwrap_or(0)
}

#[test]
fn the_hook_can_still_read_during_a_reindex() {
    // The point of the shadow swap. With in-place indexing the hook returned
    // zero bytes for the whole run — measured, three probes, all empty.
    let t = tempfile::tempdir().unwrap();
    let db = t.path().join("db");
    let cfg = corpus(t.path(), 40);

    br8n(&db, &cfg).arg("index").assert().success();
    touch_all(t.path(), 40, "changed");

    let mut bg = std::process::Command::new(assert_cmd::cargo::cargo_bin("br8n"))
        .env("BR8N_DB", &db)
        .env("BR8N_CONFIG", &cfg)
        .arg("index")
        .spawn()
        .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(400));

    // What the shadow swap actually guarantees is that the DATABASE stays
    // open to other processes. Assert that, because it is deterministic:
    // `status` reads the store and touches no model.
    let status = br8n(&db, &cfg)
        .arg("status")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status = String::from_utf8_lossy(&status);
    let docs: usize = status
        .lines()
        .find_map(|l| l.strip_prefix("documents:")?.trim().parse().ok())
        .unwrap_or(0);

    // The end-to-end hook check needs a RESPONSIVE Ollama, which is a separate
    // condition from the one under test. Its query client allows 1500ms, and a
    // concurrent index saturates the GPU — measured 7.8s, 15.3s and 7.9s for a
    // single query embed while a large index ran. Asserting on it made this
    // test fail for a reason the shadow swap cannot influence, so the hook is
    // reported rather than asserted.
    let out = br8n(&db, &cfg)
        .args(["hook", "prompt"])
        .write_stdin(r#"{"prompt":"why did the connection pooler drop sessions"}"#)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let _ = bg.wait();

    assert!(
        docs > 0,
        "the database must stay readable during a reindex; `status` reported \
         {docs} documents"
    );
    if out.is_empty() {
        eprintln!(
            "note: the hook returned nothing — the store was readable, so this is \
             Ollama contention, not the swap"
        );
    }
}

#[test]
fn a_second_indexer_declines_instead_of_destroying_the_index() {
    // A peer reproduced the damage: B failed with a bare `No such file or
    // directory` and afterwards only `db.old` survived, so `br8n status`
    // reported 0 documents. B's remove_dir_all(&shadow) had deleted A's
    // in-progress shadow after A renamed live to old, leaving A's final rename
    // nothing to move. The lock must therefore be taken before the FIRST
    // filesystem mutation, not merely before the database is opened.
    let t = tempfile::tempdir().unwrap();
    let db = t.path().join("db");
    let cfg = corpus(t.path(), 20);

    br8n(&db, &cfg).arg("index").assert().success();
    touch_all(t.path(), 20, "changed");

    let spawn = || {
        std::process::Command::new(assert_cmd::cargo::cargo_bin("br8n"))
            .env("BR8N_DB", &db)
            .env("BR8N_CONFIG", &cfg)
            .arg("index")
            .spawn()
            .unwrap()
    };
    let mut a = spawn();
    std::thread::sleep(std::time::Duration::from_millis(100));
    let mut b = spawn();
    let (ra, rb) = (a.wait().unwrap(), b.wait().unwrap());

    assert!(
        ra.success() || rb.success(),
        "at least one indexer must complete"
    );
    assert!(
        db.exists(),
        "the live index must survive two concurrent indexers"
    );
    assert!(
        !db.with_extension("new").exists(),
        "db.new must not survive"
    );
    assert!(
        !db.with_extension("old").exists(),
        "db.old must not survive"
    );

    let out = br8n(&db, &cfg)
        .arg("status")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    let docs: usize = text
        .lines()
        .find_map(|l| l.strip_prefix("documents:")?.trim().parse().ok())
        .unwrap_or(0);
    assert_eq!(
        docs, 20,
        "index must still hold every document; status said:\n{text}"
    );
}

#[test]
fn a_stale_lock_file_does_not_wedge_indexing_forever() {
    // A crashed indexer leaves its lock behind. Without pid liveness checking,
    // indexing is disabled permanently and the user is never told why.
    let t = tempfile::tempdir().unwrap();
    let db = t.path().join("db");
    let cfg = corpus(t.path(), 3);
    std::fs::create_dir_all(&db).unwrap();
    std::fs::write(db.with_extension("lock"), "999999").unwrap();

    br8n(&db, &cfg).arg("index").assert().success();
    assert!(
        !db.with_extension("lock").exists(),
        "a completed run must release its lock"
    );
}

#[test]
fn an_unchanged_corpus_is_skipped_rather_than_re_embedded() {
    // The shadow is seeded from the live index so stored content hashes survive
    // the swap. Without seeding every document looks new every run, and
    // SessionStart indexes every session.
    let t = tempfile::tempdir().unwrap();
    let db = t.path().join("db");
    let cfg = corpus(t.path(), 8);

    br8n(&db, &cfg).arg("index").assert().success();
    let out = br8n(&db, &cfg)
        .arg("index")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("8 skipped"),
        "second run must skip everything; got: {text}"
    );
    assert!(
        text.contains("0 added"),
        "second run must add nothing; got: {text}"
    );
}

/// The pack must be published by the SAME rename that publishes the database.
/// Built anywhere else, there is a window in which the two disagree — and the
/// row ordinal is the only thing tying vectors to records, so a reader in that
/// window gets the right score on the wrong document.
#[test]
fn a_successful_index_leaves_a_pack_beside_the_database() {
    let t = tempfile::tempdir().unwrap();
    let db = t.path().join("db");
    let cfg = corpus(t.path(), 8);

    br8n(&db, &cfg).arg("index").assert().success();

    for f in ["pack.manifest", "pack.vec", "pack.rec", "pack.recidx"] {
        assert!(
            db.join(f).exists(),
            "{f} must be published with the index, in {}",
            db.display()
        );
    }

    let m = br8n::pack::manifest::Manifest::read(&db).unwrap();
    assert!(
        m.rows > 0,
        "a pack with no rows is a broken build, not an empty index"
    );
}

/// `--reindex` must discard and REBUILD, not discard and leave empty.
///
/// Reproduced against the release binary before this fix: index 8 notes
/// (`documents: 8`), then `br8n index --reindex` reported
/// `0 added, 0 updated, 8 skipped, 0 chunks` and `status` afterwards said
/// `documents: 0`. The cause was that `discover_stat_first` was handed the
/// REAL stamp map regardless of `from_scratch`, so every document looked
/// unchanged and `probe.docs` came back empty — while the shadow legitimately
/// started empty (seeding is skipped for `from_scratch`). Nothing was ever
/// indexed into it, and that empty shadow was published over the real index.
/// Every pack-refusal message tells the user to run `--reindex` to recover,
/// so this must not be the documented remedy that deletes their data.
#[test]
fn reindex_rebuilds_instead_of_publishing_an_empty_shadow() {
    let t = tempfile::tempdir().unwrap();
    let db = t.path().join("db");
    let cfg = corpus(t.path(), 8);

    br8n(&db, &cfg).arg("index").assert().success();
    let before = br8n(&db, &cfg)
        .arg("status")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let before_docs: usize = String::from_utf8_lossy(&before)
        .lines()
        .find_map(|l| l.strip_prefix("documents:")?.trim().parse().ok())
        .unwrap_or(0);
    assert_eq!(before_docs, 8, "sanity check: initial index must hold 8");

    br8n(&db, &cfg)
        .args(["index", "--reindex"])
        .assert()
        .success();

    let after = br8n(&db, &cfg)
        .arg("status")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let after_docs: usize = String::from_utf8_lossy(&after)
        .lines()
        .find_map(|l| l.strip_prefix("documents:")?.trim().parse().ok())
        .unwrap_or(0);

    assert_eq!(
        after_docs,
        8,
        "`br8n index --reindex` must rebuild the corpus, not publish an \
         empty index over it; status after reindex said:\n{}",
        String::from_utf8_lossy(&after)
    );
}

/// Compaction must shrink the file and keep every document, without
/// re-embedding. lbug cannot vacuum — `CHECKPOINT` frees 0 bytes and `VACUUM`
/// does not exist — so the only way to reclaim is to rebuild and swap.
#[test]
fn compaction_shrinks_the_database_and_keeps_every_document() {
    let t = tempfile::tempdir().unwrap();
    let db = t.path().join("db");
    let cfg = corpus(t.path(), 30);

    br8n(&db, &cfg).arg("index").assert().success();
    let before_docs = docs_in(&db, &cfg);

    // Churn: rewrite every note several times so dead rows accumulate.
    for i in 0..4 {
        touch_all(t.path(), 30, &format!("revision {i}"));
        br8n(&db, &cfg).arg("index").assert().success();
    }
    let grown = std::fs::metadata(db.join("graph.kz")).unwrap().len();

    br8n(&db, &cfg)
        .args(["index", "--compact"])
        .assert()
        .success();
    let compacted = std::fs::metadata(db.join("graph.kz")).unwrap().len();

    assert_eq!(
        docs_in(&db, &cfg),
        before_docs,
        "compaction must keep every document"
    );
    assert!(
        compacted < grown,
        "compaction must reclaim space: {grown} -> {compacted}"
    );
}

/// Closes a real gap in the test above: `compaction_shrinks_the_database_
/// and_keeps_every_document` only checks document count and file size, so it
/// would not notice compaction silently zeroing or otherwise corrupting
/// every embedding on the way through (verified by hand — mutating
/// `compact_swap` to write a corrupted vector still leaves that test green).
/// This test can tell the difference because `content_aware_corpus` gives
/// every note a distinct embedding: if compaction discarded or recomputed
/// them instead of carrying the stored ones across, the notes would collapse
/// onto whatever placeholder replaced their vectors and a query's top match
/// would no longer reliably be the note actually being searched for.
#[test]
fn compaction_does_not_change_which_document_a_query_matches() {
    let t = tempfile::tempdir().unwrap();
    let db = t.path().join("db");
    let cfg = content_aware_corpus(t.path(), 12);

    br8n(&db, &cfg).arg("index").assert().success();
    let query = "PgBouncer transaction mode, entry 7";
    let before = top_hit_uri(&db, &cfg, query);
    assert!(
        before.ends_with("n7.md"),
        "sanity check: content-aware embeddings must let an exact-content \
         query find its own note; got {before}"
    );

    for i in 0..3 {
        touch_all(t.path(), 12, &format!("revision {i}"));
        br8n(&db, &cfg).arg("index").assert().success();
    }
    br8n(&db, &cfg)
        .args(["index", "--compact"])
        .assert()
        .success();

    let after = top_hit_uri(&db, &cfg, query);
    assert_eq!(
        before, after,
        "compaction must not change which document a query matches — a \
         corrupted or discarded embedding would silently re-rank results"
    );
}

/// Phase 1 must publish a searchable index without embedding anything.
///
/// A full re-index of the live corpus took 86 minutes, and for all of it the
/// corpus was unsearchable. Phase 1 writes rows and BM25 postings — no model
/// involved — so keyword search works the moment it swaps.
#[test]
fn phase_one_publishes_a_keyword_searchable_index_with_no_vectors() {
    let t = tempfile::tempdir().unwrap();
    let db = t.path().join("db");
    let cfg = corpus(t.path(), 10);

    br8n(&db, &cfg)
        .args(["index", "--no-embed"])
        .assert()
        .success();

    let m = br8n::pack::manifest::Manifest::read(&db).unwrap();
    assert!(m.rows > 0, "rows must be published");
    assert_eq!(m.rows_with_vectors, 0, "phase 1 embeds nothing");
    assert!(db.join("pack.fts").exists(), "postings must be published");

    // Keyword search works immediately.
    let out = br8n(&db, &cfg)
        .args(["search", "--quality", "1", "PgBouncer"])
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(
        !String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "BM25 must answer from a vectorless index"
    );
}

/// The backlog is derived from the store, so a killed backfill resumes.
#[test]
fn the_embed_backlog_is_derived_from_the_store_not_from_memory() {
    let t = tempfile::tempdir().unwrap();
    let db = t.path().join("db");
    let cfg = corpus(t.path(), 10);
    br8n(&db, &cfg)
        .args(["index", "--no-embed"])
        .assert()
        .success();

    let before = pending_count(&db, &cfg);
    assert!(before > 0, "there must be a backlog to drain");

    br8n(&db, &cfg)
        .args(["index", "--backfill"])
        .assert()
        .success();
    assert_eq!(pending_count(&db, &cfg), 0, "the backfill must drain it");
}

/// The trailing `Store::open_existing` assertion below is NOT load-bearing
/// for the property this task exists to guard: POSIX record locks
/// (`fcntl(F_SETLK, ...)`, what lbug uses) are keyed on `(process, inode)`,
/// so a process can never conflict with a lock it already holds — this
/// assertion would read exactly the same whether or not `compact_swap` still
/// held the live store. `a_reader_can_still_read_status_while_a_compaction_
/// runs`, below, is the test that actually covers the lock-release property,
/// because it checks from a genuinely different OS process.
#[test]
fn compaction_reads_the_live_store_in_one_pass_then_releases_it() {
    let t = tempfile::tempdir().unwrap();
    let live = t.path().join("db");
    let cfg_path = corpus(t.path(), 4);
    br8n(&live, &cfg_path).arg("index").assert().success();

    let cfg = br8n::config::Config::default();

    let snap = br8n::index::read_source_for_test(&cfg, &live).unwrap();
    assert_eq!(snap.docs.len(), 4, "every document is read in the one pass");
    assert!(!snap.chunks.is_empty(), "chunks come across too");
    assert!(
        !snap.model_id.is_empty(),
        "the model id is part of the snapshot"
    );

    let reader = br8n::store::Store::open_existing(&live, cfg.embed.dimensions);
    assert!(
        reader.is_ok(),
        "the live store must be free once the read phase has returned: {:?}",
        reader.err()
    );
}

#[test]
fn a_reader_can_still_read_status_while_a_compaction_runs() {
    let mut n = 400usize;
    for _ in 0..8 {
        let t = tempfile::tempdir().unwrap();
        let db = t.path().join("db");
        let cfg = corpus(t.path(), n);

        br8n(&db, &cfg).arg("index").assert().success();
        let before = docs_in(&db, &cfg);
        assert!(before > 0, "sanity check: the corpus must be indexed");

        let mut bg = std::process::Command::new(assert_cmd::cargo::cargo_bin("br8n"))
            .env("BR8N_DB", &db)
            .env("BR8N_CONFIG", &cfg)
            .args(["index", "--compact"])
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(250));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let mut polls_while_running = 0usize;
        let mut successes_while_running = 0usize;
        let compact_status = loop {
            if let Some(status) = bg.try_wait().unwrap() {
                break status;
            }
            polls_while_running += 1;
            if docs_in(&db, &cfg) == before {
                successes_while_running += 1;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "`--compact` over {n} documents did not finish within 120s"
            );
            std::thread::sleep(std::time::Duration::from_millis(15));
        };

        if !compact_status.success() {
            let mut stderr = String::new();
            std::io::Read::read_to_string(bg.stderr.as_mut().unwrap(), &mut stderr).unwrap();
            assert!(
                stderr.contains("Could not set lock on file"),
                "`--compact` over {n} documents failed for a reason unrelated \
                 to lock contention with this test's own polling:\n{stderr}"
            );
            continue;
        }

        if polls_while_running < 5 {
            n *= 4;
            continue;
        }

        assert!(
            successes_while_running * 2 >= polls_while_running,
            "a concurrent `br8n status` succeeded only {successes_while_running} \
             of {polls_while_running} polls while `--compact` was running over \
             {n} documents — a healthy build phase keeps the live store free \
             the whole time, so a lone success at the tail (the moment the \
             store is released right before the process exits) is not enough; \
             this looks like the live store was held for most of the build \
             phase"
        );
        assert_eq!(
            docs_in(&db, &cfg),
            before,
            "compaction must keep every document"
        );
        return;
    }

    panic!(
        "gave up after several attempts at growing the corpus (last size \
         {n}) without ever getting a clean run with enough polls landing \
         while `--compact` was still running — either compaction is too \
         fast on this machine for this test's polling loop to observe, or \
         it keeps losing its own brief initial read of the live store to \
         this test's polling"
    );
}
