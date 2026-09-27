//! A transcript that is still being written is left for a later run.
//!
//! A session transcript is appended to for as long as its session is alive, so
//! indexing one mid-session re-reads and re-chunks a file that will change
//! again within seconds. On this machine ~722 of 760 indexed documents are
//! transcripts, so every `SessionStart` run found work and the indexer looked
//! like it restarted on completion.
//!
//! The dangerous half of the fix is not the skip, it is the recovery: a file
//! skipped for freshness must be picked up later, and "skipped forever" is a
//! corpus that shrinks with nothing printed anywhere. That is what
//! `a_deferred_transcript_is_indexed_once_it_settles` exists for, and it is
//! the reason this file is willing to spend five seconds of wall clock.

mod common;

use br8n::config::Config;
use br8n::index::{discover_stat_first, Deferral};
use br8n::loaders::transcript::TranscriptLoader;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// `HOME` is process-global and `TranscriptLoader::default_root()` reads it on
/// every call, so every test in this file that points it at a fixture has to
/// take this first. They are all in one file precisely so one mutex covers
/// them; a second test binary touching `HOME` would not be serialised by it.
static HOME_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct FakeHome {
    _guard: std::sync::MutexGuard<'static, ()>,
    dir: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
    previous_codex: Option<std::ffi::OsString>,
}

impl FakeHome {
    fn new() -> FakeHome {
        let guard = HOME_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("HOME");
        let previous_codex = std::env::var_os("CODEX_HOME");
        std::env::set_var("HOME", dir.path());
        std::env::set_var("CODEX_HOME", dir.path().join("codex-home"));
        std::fs::create_dir_all(dir.path().join(".claude/projects/proj")).unwrap();
        FakeHome {
            _guard: guard,
            dir,
            previous,
            previous_codex,
        }
    }
    fn projects(&self) -> PathBuf {
        self.dir.path().join(".claude/projects/proj")
    }
}

impl Drop for FakeHome {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        match self.previous_codex.take() {
            Some(h) => std::env::set_var("CODEX_HOME", h),
            None => std::env::remove_var("CODEX_HOME"),
        }
    }
}

/// Write a transcript whose lines the real loader will actually accept — an
/// unparseable one lands in `skipped` as a load failure, which would look like
/// a pass for the wrong reason.
fn write_transcript(path: &Path, turns: usize) {
    let mut body = String::new();
    for i in 0..turns {
        body.push_str(&format!(
            "{{\"cwd\":\"/w/proj\",\"timestamp\":\"2026-08-3{}T09:00:00Z\",\
             \"message\":{{\"role\":\"user\",\"content\":\"turn {i} about connection \
             pooling and retrieval latency in the store\"}}}}\n",
            i % 10
        ));
    }
    std::fs::write(path, body).unwrap();
}

fn append_turn(path: &Path, text: &str) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(
        f,
        "{{\"cwd\":\"/w/proj\",\"timestamp\":\"2026-08-31T10:00:00Z\",\
         \"message\":{{\"role\":\"assistant\",\"content\":\"{text}\"}}}}"
    )
    .unwrap();
}

/// Set a file's modification time to `age` ago. The whole test file turns on
/// this: freshness is `mtime.elapsed()`, so this is the only dial.
fn aged(path: &Path, age: Duration) {
    let when = SystemTime::now() - age;
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_times(
        std::fs::FileTimes::new()
            .set_accessed(when)
            .set_modified(when),
    )
    .unwrap();
}

fn uri_of(path: &Path) -> String {
    format!(
        "claude-session://{}",
        path.canonicalize().unwrap().display()
    )
}

/// A config with one real notes root, so `prune_missing` has something to
/// distinguish and `discover_stat_first`'s root check passes.
fn config(notes: &Path) -> Config {
    Config {
        sources: vec![notes.to_path_buf()],
        index_transcripts: true,
        ..Config::default()
    }
}

#[test]
fn the_settle_window_has_a_boundary_and_it_is_ten_minutes() {
    // Pure, so the boundary is testable without a clock. Both sides, and the
    // window itself written out rather than imported — importing the constant
    // would make every line below true by construction.
    let ten_minutes = Duration::from_secs(600);
    assert_eq!(TranscriptLoader::SETTLE, ten_minutes);
    assert!(TranscriptLoader::is_settling(Duration::from_secs(0)));
    assert!(TranscriptLoader::is_settling(
        ten_minutes - Duration::from_secs(1)
    ));
    assert!(!TranscriptLoader::is_settling(ten_minutes));
    assert!(!TranscriptLoader::is_settling(
        ten_minutes + Duration::from_secs(1)
    ));
}

#[test]
fn freshness_is_read_off_the_file_and_a_bad_clock_never_defers_forever() {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("s.jsonl");
    write_transcript(&p, 3);

    aged(&p, Duration::from_secs(5));
    assert!(
        TranscriptLoader::settling_for(&p).is_some(),
        "a file touched five seconds ago is still being written"
    );
    aged(&p, Duration::from_secs(900));
    assert!(
        TranscriptLoader::settling_for(&p).is_none(),
        "fifteen minutes of quiet is settled"
    );

    // A modification time in the FUTURE — a clock change, a restored backup, a
    // copy that preserved a bad timestamp. `elapsed()` fails on it, and the
    // answer has to be "read it": treating it as fresh would defer the file on
    // every run for as long as the clock stayed wrong, and a transcript
    // deferred permanently leaves the corpus silently.
    let f = std::fs::File::options().write(true).open(&p).unwrap();
    let future = SystemTime::now() + Duration::from_secs(86_400);
    f.set_times(
        std::fs::FileTimes::new()
            .set_accessed(future)
            .set_modified(future),
    )
    .unwrap();
    assert!(
        TranscriptLoader::settling_for(&p).is_none(),
        "a future mtime must read as settled, not as forever-fresh"
    );

    assert!(
        TranscriptLoader::settling_for(&t.path().join("gone.jsonl")).is_none(),
        "an unreadable file must not be deferred either"
    );
}

#[test]
fn a_live_transcript_is_deferred_reported_and_kept_live() {
    let home = FakeHome::new();
    let notes = tempfile::tempdir().unwrap();
    std::fs::write(notes.path().join("note.md"), "# Pooling\n\nPgBouncer.\n").unwrap();

    // Three transcripts, not one: two settled and one fresh, so the assertion
    // is that the fresh one is separated from its peers rather than that
    // transcripts are handled at all.
    let old_a = home.projects().join("aaa.jsonl");
    let old_b = home.projects().join("bbb.jsonl");
    let live = home.projects().join("ccc.jsonl");
    for p in [&old_a, &old_b, &live] {
        write_transcript(p, 4);
    }
    aged(&old_a, Duration::from_secs(3600));
    aged(&old_b, Duration::from_secs(3600));
    aged(&live, Duration::from_secs(30));

    let (found, _) =
        discover_stat_first(&config(notes.path()), &HashMap::new(), Deferral::Allowed).unwrap();

    let read: Vec<&str> = found.docs.iter().map(|d| d.uri.as_str()).collect();
    assert!(read.contains(&uri_of(&old_a).as_str()));
    assert!(read.contains(&uri_of(&old_b).as_str()));
    assert!(
        !read.contains(&uri_of(&live).as_str()),
        "a transcript written 30s ago must not be read: {read:?}"
    );
    assert_eq!(found.deferred, vec![uri_of(&live)]);

    // Reported, not silent. `br8n status` renders exactly these strings.
    let said: Vec<&String> = found
        .skipped
        .iter()
        .filter(|s| s.contains("ccc.jsonl"))
        .collect();
    assert_eq!(said.len(), 1, "exactly one skip line: {:?}", found.skipped);
    assert!(
        said[0].contains("still being written"),
        "the reason must say why, not just that: {}",
        said[0]
    );

    // And still live, so pruning does not delete it.
    assert!(found.live_uris().contains(&uri_of(&live)));
}

#[test]
fn a_deferred_transcript_is_not_pruned_out_of_the_index() {
    // `uri_is_discoverable` returns TRUE for every `claude-session://` URI, so
    // a transcript missing from `live_uris` is not merely un-refreshed — it is
    // DELETED. Reachability, not existence: the document is indexed first,
    // then pruned against a live set produced by a run that deferred it.
    let home = FakeHome::new();
    let notes = tempfile::tempdir().unwrap();
    std::fs::write(notes.path().join("note.md"), "# Pooling\n\nPgBouncer.\n").unwrap();
    let live = home.projects().join("live.jsonl");
    write_transcript(&live, 4);

    let (_dir, idx, _calls) = common::setup_with_sources(vec![notes.path().to_path_buf()]);
    aged(&live, Duration::from_secs(3600));
    let (settled, _) =
        discover_stat_first(&config(notes.path()), &HashMap::new(), Deferral::Allowed).unwrap();
    idx.index_documents(&settled.docs).unwrap();
    assert!(
        idx.store()
            .all_doc_uris()
            .unwrap()
            .iter()
            .any(|(_, u)| *u == uri_of(&live)),
        "the transcript must be in the index before this test means anything"
    );

    // Now it is being written again, and the next run defers it.
    append_turn(&live, "and then we changed the fusion weights");
    aged(&live, Duration::from_secs(10));
    let (deferring, _) =
        discover_stat_first(&config(notes.path()), &HashMap::new(), Deferral::Allowed).unwrap();
    assert_eq!(deferring.deferred, vec![uri_of(&live)]);

    let removed = idx.prune_missing(&deferring.live_uris()).unwrap();
    assert_eq!(removed, 0, "deferring a transcript must not delete it");
    assert!(
        idx.store()
            .all_doc_uris()
            .unwrap()
            .iter()
            .any(|(_, u)| *u == uri_of(&live)),
        "the deferred transcript must still be reachable in the index"
    );
}

#[test]
fn a_deferred_transcript_is_indexed_once_it_settles() {
    // The failure this pins is permanent loss, and it is only reachable
    // through the stamp map: a deferred file records the fingerprint of the
    // version ALREADY INDEXED, never the version on disk. Record the current
    // one instead and the file is marked up to date without ever having been
    // read — invisible, and unrecoverable until some later write happens to
    // move the mtime again.
    //
    // Catching that needs a file that is fresh on one run and settled on the
    // next WITH ITS STAMP UNCHANGED, which is why this test spends five real
    // seconds: the mtime is parked four seconds inside the window and the test
    // waits for it to cross. Backdating instead would move the stamp, and both
    // the correct and the broken version would then re-read the file — a test
    // that passes either way.
    let home = FakeHome::new();
    let notes = tempfile::tempdir().unwrap();
    std::fs::write(notes.path().join("note.md"), "# Pooling\n\nPgBouncer.\n").unwrap();
    let cfg = config(notes.path());

    let live = home.projects().join("live.jsonl");
    write_transcript(&live, 4);
    aged(&live, Duration::from_secs(3600));

    // Run 1: settled, so it is read and fingerprinted.
    let (first, stamps1) = discover_stat_first(&cfg, &HashMap::new(), Deferral::Allowed).unwrap();
    assert!(first.docs.iter().any(|d| d.uri == uri_of(&live)));
    let indexed_stamp = stamps1
        .get(&uri_of(&live))
        .expect("run 1 must fingerprint it");

    // The session appends. Park the mtime just inside the settle window.
    append_turn(&live, "we then repaired the fts index and recall moved");
    aged(&live, TranscriptLoader::SETTLE - Duration::from_secs(4));

    // Run 2: changed, but still being written.
    let (second, stamps2) = discover_stat_first(&cfg, &stamps1, Deferral::Allowed).unwrap();
    assert_eq!(second.deferred, vec![uri_of(&live)]);
    assert!(!second.docs.iter().any(|d| d.uri == uri_of(&live)));
    assert_eq!(
        stamps2.get(&uri_of(&live)),
        Some(indexed_stamp),
        "a deferred file must carry the INDEXED fingerprint forward — not the \
         one on disk (which loses the file) and not nothing (which breaks the \
         unchanged-corpus fast path every live session would then defeat)"
    );

    // Four seconds inside the window, so five is past it. Nothing touches the
    // file: same mtime, same size, same stamp — only the clock moves.
    std::thread::sleep(Duration::from_secs(5));
    assert!(TranscriptLoader::settling_for(&live).is_none());

    // Run 3: settled at last, and read — with the appended turn in it.
    let (third, _) = discover_stat_first(&cfg, &stamps2, Deferral::Allowed).unwrap();
    assert!(
        third.deferred.is_empty(),
        "nothing should still be deferred: {:?}",
        third.deferred
    );
    let doc = third
        .docs
        .iter()
        .find(|d| d.uri == uri_of(&live))
        .expect("the deferred transcript must be picked up by a later run");
    assert!(
        doc.text.contains("repaired the fts index"),
        "and it must be the CURRENT content, not the version run 1 read"
    );
}

#[test]
fn notes_are_never_deferred_however_recently_they_were_saved() {
    // Only transcripts are appended to by a running process. A markdown note
    // arrives in one save from an editor, and making the user wait ten minutes
    // for the note they just wrote would be a worse tool.
    let home = FakeHome::new();
    let notes = tempfile::tempdir().unwrap();
    let just_saved = notes.path().join("fresh.md");
    std::fs::write(&just_saved, "# Fusion\n\nRRF over vector and bm25.\n").unwrap();
    aged(&just_saved, Duration::from_secs(1));

    // A transcript of the same age, as the control: same second, opposite
    // treatment, so this cannot pass by nothing being deferred at all.
    let live = home.projects().join("live.jsonl");
    write_transcript(&live, 4);
    aged(&live, Duration::from_secs(1));

    let (found, _) =
        discover_stat_first(&config(notes.path()), &HashMap::new(), Deferral::Allowed).unwrap();
    let note_uri = format!("file://{}", just_saved.canonicalize().unwrap().display());
    assert!(
        found.docs.iter().any(|d| d.uri == note_uri),
        "a note saved one second ago must be indexed now: {:?}",
        found.docs.iter().map(|d| &d.uri).collect::<Vec<_>>()
    );
    assert_eq!(found.deferred, vec![uri_of(&live)]);
}

#[test]
fn a_from_scratch_run_reads_a_live_transcript_rather_than_losing_it() {
    // `br8n index --reindex` DELETED every live transcript, and this is the
    // unit-level half of the pin (`tests/transcript_deferral_cli.rs` runs the
    // real binary end to end).
    //
    // The two arms share one fixture and one stamp map, and differ in exactly
    // the argument under test — so this cannot pass by the transcript being
    // settled, by the fixture being wrong, or by deferral being broken
    // generally. `Allowed` must defer it; `Forbidden` must read it.
    //
    // A from-scratch run has no previous version anywhere: it skips the seed
    // copy, so a file it does not read is simply absent from the index it
    // publishes. `live_uris` keeping it away from `prune_missing` does nothing
    // — there is nothing to prune.
    let home = FakeHome::new();
    let notes = tempfile::tempdir().unwrap();
    std::fs::write(notes.path().join("note.md"), "# Pooling\n\nPgBouncer.\n").unwrap();
    let cfg = config(notes.path());

    let live = home.projects().join("live.jsonl");
    write_transcript(&live, 4);
    aged(&live, Duration::from_secs(20));

    // `--reindex` passes an EMPTY stamp map, which is what makes every
    // transcript look changed and therefore deferrable. Both arms get it.
    let (deferring, _) = discover_stat_first(&cfg, &HashMap::new(), Deferral::Allowed).unwrap();
    assert_eq!(
        deferring.deferred,
        vec![uri_of(&live)],
        "control: with deferral allowed this fixture must defer, or the other \
         arm proves nothing"
    );

    let (rebuilding, stamps) =
        discover_stat_first(&cfg, &HashMap::new(), Deferral::Forbidden).unwrap();
    assert!(
        rebuilding.deferred.is_empty(),
        "a rebuild from nothing must defer nothing: {:?}",
        rebuilding.deferred
    );
    let doc = rebuilding
        .docs
        .iter()
        .find(|d| d.uri == uri_of(&live))
        .expect("the live transcript must be READ, not left for a run that has no earlier index");
    assert!(
        doc.text.contains("connection"),
        "and it must be the real parsed transcript, not an empty placeholder"
    );
    assert!(
        stamps.contains_key(&uri_of(&live)),
        "a file that was read must be fingerprinted, or the next run re-reads it"
    );
    assert!(
        !rebuilding.skipped.iter().any(|s| s.contains("live.jsonl")),
        "and nothing may be reported as skipped: {:?}",
        rebuilding.skipped
    );
}

#[test]
fn discover_never_defers_because_it_has_no_index_to_protect() {
    // `br8n add` calls `discover` to resolve wikilinks against the WHOLE
    // corpus. Deferring there drops every transcript touched in the last ten
    // minutes — including the session the user is typing the `br8n add` into
    // — and the caller gets a short corpus with nothing to tell it so.
    //
    // The control is the same file through `discover_stat_first`, which does
    // defer it. Without that line this passes if deferral stops working
    // altogether.
    let home = FakeHome::new();
    let notes = tempfile::tempdir().unwrap();
    std::fs::write(notes.path().join("note.md"), "# Pooling\n\nPgBouncer.\n").unwrap();
    let cfg = config(notes.path());

    let live = home.projects().join("live.jsonl");
    write_transcript(&live, 4);
    aged(&live, Duration::from_secs(20));

    let (control, _) = discover_stat_first(&cfg, &HashMap::new(), Deferral::Allowed).unwrap();
    assert_eq!(
        control.deferred,
        vec![uri_of(&live)],
        "control: this transcript is fresh enough to be deferrable"
    );

    let corpus = br8n::index::discover(&cfg).unwrap();
    assert!(
        corpus.iter().any(|d| d.uri == uri_of(&live)),
        "`discover` must enumerate the live transcript: {:?}",
        corpus.iter().map(|d| &d.uri).collect::<Vec<_>>()
    );
}

#[test]
fn deferred_uris_come_back_in_a_stable_order() {
    // `found.deferred.sort()`. Not a correctness property on its own —
    // `prune_missing` does not care about order — but `live_uris` is built by
    // concatenating three lists, and the other two are already sorted. An
    // unsorted third makes the output depend on the filesystem's readdir
    // order, which differs between machines and between runs of the same
    // machine after a directory has been rewritten.
    //
    // THE FIXTURE IS THE WHOLE DIFFICULTY, exactly as with the `chunk_id`
    // tie-breaks: a list that readdir happens to hand back in order passes
    // with the sort deleted. Eight names are used, chosen so that lexical
    // order is not insertion order either, and the mutation was run: deleting
    // `found.deferred.sort()` fails this test.
    let home = FakeHome::new();
    let notes = tempfile::tempdir().unwrap();
    std::fs::write(notes.path().join("note.md"), "# Pooling\n\nPgBouncer.\n").unwrap();

    let mut want: Vec<String> = Vec::new();
    for name in [
        "zebra", "mango", "apple", "quartz", "banana", "yak", "cedar", "nimbus",
    ] {
        let p = home.projects().join(format!("{name}.jsonl"));
        write_transcript(&p, 2);
        aged(&p, Duration::from_secs(15));
        want.push(uri_of(&p));
    }
    want.sort();

    let (found, _) =
        discover_stat_first(&config(notes.path()), &HashMap::new(), Deferral::Allowed).unwrap();
    assert_eq!(found.deferred, want, "deferred URIs must come back sorted");
}

fn config_with_max_age(notes: &Path, max_age_days: Option<u32>) -> Config {
    Config {
        index_transcripts_max_age_days: max_age_days,
        ..config(notes)
    }
}

#[test]
fn an_old_transcript_is_indexed_by_default_with_no_age_limit() {
    let home = FakeHome::new();
    let notes = tempfile::tempdir().unwrap();
    std::fs::write(notes.path().join("note.md"), "# Pooling\n\nPgBouncer.\n").unwrap();

    let old = home.projects().join("old.jsonl");
    write_transcript(&old, 4);
    aged(&old, Duration::from_secs(400 * 86_400));

    let (found, _) = discover_stat_first(
        &config_with_max_age(notes.path(), None),
        &HashMap::new(),
        Deferral::Allowed,
    )
    .unwrap();
    assert!(
        found.docs.iter().any(|d| d.uri == uri_of(&old)),
        "with no age limit configured, an old transcript must still be read: {:?}",
        found.docs.iter().map(|d| &d.uri).collect::<Vec<_>>()
    );
}

#[test]
fn an_old_transcript_is_excluded_once_an_age_limit_is_set() {
    let home = FakeHome::new();
    let notes = tempfile::tempdir().unwrap();
    std::fs::write(notes.path().join("note.md"), "# Pooling\n\nPgBouncer.\n").unwrap();

    let old = home.projects().join("old.jsonl");
    write_transcript(&old, 4);
    aged(&old, Duration::from_secs(400 * 86_400));

    let (found, _) = discover_stat_first(
        &config_with_max_age(notes.path(), Some(30)),
        &HashMap::new(),
        Deferral::Allowed,
    )
    .unwrap();
    assert!(
        !found.docs.iter().any(|d| d.uri == uri_of(&old)),
        "a transcript older than the configured limit must not be read: {:?}",
        found.docs.iter().map(|d| &d.uri).collect::<Vec<_>>()
    );
    assert!(
        !found.live_uris().contains(&uri_of(&old)),
        "an excluded transcript must not keep pruning off it either"
    );
}

#[test]
fn a_fresh_transcript_is_kept_once_an_age_limit_is_set() {
    let home = FakeHome::new();
    let notes = tempfile::tempdir().unwrap();
    std::fs::write(notes.path().join("note.md"), "# Pooling\n\nPgBouncer.\n").unwrap();

    let fresh = home.projects().join("fresh.jsonl");
    write_transcript(&fresh, 4);
    aged(&fresh, Duration::from_secs(2 * 86_400));

    let (found, _) = discover_stat_first(
        &config_with_max_age(notes.path(), Some(30)),
        &HashMap::new(),
        Deferral::Allowed,
    )
    .unwrap();
    assert!(
        found.docs.iter().any(|d| d.uri == uri_of(&fresh)),
        "a transcript younger than the configured limit must still be read: {:?}",
        found.docs.iter().map(|d| &d.uri).collect::<Vec<_>>()
    );
}

#[test]
fn a_transcript_that_ages_past_the_limit_is_pruned_on_the_next_run() {
    let home = FakeHome::new();
    let notes = tempfile::tempdir().unwrap();
    std::fs::write(notes.path().join("note.md"), "# Pooling\n\nPgBouncer.\n").unwrap();
    let cfg = config_with_max_age(notes.path(), Some(30));

    let live = home.projects().join("live.jsonl");
    write_transcript(&live, 4);
    aged(&live, Duration::from_secs(2 * 86_400));

    let (_dir, idx, _calls) = common::setup_with_sources(vec![notes.path().to_path_buf()]);
    let (first, stamps1) = discover_stat_first(&cfg, &HashMap::new(), Deferral::Allowed).unwrap();
    idx.index_documents(&first.docs).unwrap();
    assert!(
        idx.store()
            .all_doc_uris()
            .unwrap()
            .iter()
            .any(|(_, u)| *u == uri_of(&live)),
        "the transcript must be in the index before this test means anything"
    );

    aged(&live, Duration::from_secs(60 * 86_400));
    let (second, _) = discover_stat_first(&cfg, &stamps1, Deferral::Allowed).unwrap();
    assert!(
        !second.docs.iter().any(|d| d.uri == uri_of(&live)),
        "a transcript that has aged past the limit must not be re-read"
    );
    assert!(
        !second.live_uris().contains(&uri_of(&live)),
        "and must not appear live, or prune_missing will spare it"
    );

    let removed = idx.prune_missing(&second.live_uris()).unwrap();
    assert_eq!(
        removed, 1,
        "a transcript that aged past the limit must be pruned on the next run"
    );
    assert!(
        !idx.store()
            .all_doc_uris()
            .unwrap()
            .iter()
            .any(|(_, u)| *u == uri_of(&live)),
        "and it must actually be gone from the index"
    );
}

#[test]
fn crossing_the_age_limit_shrinks_the_stamp_key_set() {
    let home = FakeHome::new();
    let notes = tempfile::tempdir().unwrap();
    std::fs::write(notes.path().join("note.md"), "# Pooling\n\nPgBouncer.\n").unwrap();

    let live = home.projects().join("live.jsonl");
    write_transcript(&live, 4);
    aged(&live, Duration::from_secs(2 * 86_400));

    let cfg_no_limit = config_with_max_age(notes.path(), None);
    let (_, stamps1) =
        discover_stat_first(&cfg_no_limit, &HashMap::new(), Deferral::Allowed).unwrap();
    assert!(stamps1.contains_key(&uri_of(&live)));

    aged(&live, Duration::from_secs(60 * 86_400));
    let cfg_with_limit = config_with_max_age(notes.path(), Some(30));
    let (_, stamps2) = discover_stat_first(&cfg_with_limit, &stamps1, Deferral::Allowed).unwrap();

    let looks_unchanged =
        stamps1.len() == stamps2.len() && stamps2.keys().all(|k| stamps1.contains_key(k));
    assert!(
        !looks_unchanged,
        "a transcript crossing the age limit must shrink the discovered key set, \
         or the no-op fast path would wrongly treat this run as unchanged"
    );
    assert!(!stamps2.contains_key(&uri_of(&live)));
}

#[test]
fn codex_sessions_are_found_under_codex_home_and_settle_the_same_way() {
    let home = FakeHome::new();
    let notes = tempfile::tempdir().unwrap();
    std::fs::write(notes.path().join("note.md"), "# Pooling\n\nPgBouncer.\n").unwrap();
    let day = home.dir.path().join("codex-home/sessions/2026/09/20");
    std::fs::create_dir_all(&day).unwrap();
    let fixture = "tests/fixtures/codex/sessions/2026/09/20/rollout-2026-09-20T10-15-00-0199aaaa-bbbb-7ccc-8ddd-eeeeffff0001.jsonl";
    let settled =
        day.join("rollout-2026-09-20T08-00-00-0199aaaa-bbbb-7ccc-8ddd-000000000011.jsonl");
    let live = day.join("rollout-2026-09-20T09-00-00-0199aaaa-bbbb-7ccc-8ddd-000000000012.jsonl");
    std::fs::copy(fixture, &settled).unwrap();
    std::fs::copy(fixture, &live).unwrap();
    aged(&settled, Duration::from_secs(3600));
    aged(&live, Duration::from_secs(30));
    let codex_uri = |p: &Path| format!("codex-session://{}", p.canonicalize().unwrap().display());

    let (found, _) =
        discover_stat_first(&config(notes.path()), &HashMap::new(), Deferral::Allowed).unwrap();

    let read: Vec<&str> = found.docs.iter().map(|d| d.uri.as_str()).collect();
    assert!(read.contains(&codex_uri(&settled).as_str()), "{read:?}");
    assert_eq!(found.deferred, vec![codex_uri(&live)]);
    assert!(found.live_uris().contains(&codex_uri(&live)));
}
