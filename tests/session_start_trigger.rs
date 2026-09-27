//! `SessionStart` decides whether to index; it no longer always indexes.
//!
//! The defect: `run_session_start` spawned a detached `br8n index` on every
//! SessionStart, unconditionally, and never read stdin — so it could not tell
//! a fresh start from a resume, a `/clear` or a compaction. With seven live
//! sessions, `db.log` held 247 completed runs and 258 lock refusals in a day:
//! `IndexLock` serialised them correctly, but the moment one finished the next
//! trigger started another.
//!
//! Two things are pinned here. The pure decision (`session_start_decision`),
//! which is now the rate limit and nothing else — no trigger is refused for
//! being the wrong KIND of trigger, and the tests below say so for all four
//! sources at once rather than treating `compact` as a special case. And the
//! real binary, because the hazard that mattered most is not a logic error —
//! it is `read_to_string` on a stdin that never reaches EOF, which would hang
//! the hook until Claude Code's 60s timeout and break session startup.
//!
//! `compact` was denied outright in the first version of this file. It is not
//! any more: see `session_start_decision`'s own comment for the repro that
//! killed that rule, and `a_compaction_is_rate_limited_like_every_other_source`
//! for what replaced it.

use br8n::hook::{session_start_decision, SessionStart};
use std::io::Write;
use std::time::Duration;

/// The shipped interval. Written out rather than imported, so a change to the
/// constant has to come here and be argued for — an imported constant makes
/// every boundary test below tautological.
const INTERVAL: Duration = Duration::from_secs(15 * 60);

fn indexes(source: Option<&str>, age: Option<Duration>) -> bool {
    matches!(session_start_decision(source, age), SessionStart::Index)
}

fn reason(source: Option<&str>, age: Option<Duration>) -> String {
    match session_start_decision(source, age) {
        SessionStart::Skip(why) => why,
        SessionStart::Index => panic!("expected a skip for source={source:?} age={age:?}"),
    }
}

/// Every source Claude Code sends. Written out rather than derived, because
/// the property under test is that this list has no exceptions in it.
const SOURCES: [&str; 4] = ["startup", "resume", "clear", "compact"];

#[test]
fn a_compaction_is_rate_limited_like_every_other_source() {
    // The behaviour that changed after review, so this test is the inverse of
    // the one it replaces and the reason has to be here rather than in a
    // commit message.
    //
    // Compaction was denied outright, on the argument that it continues a
    // session that "already had a startup or a resume". True about the
    // session, false about the clock: in an all-day single process, startup
    // fired once in the morning and compaction is the ONLY trigger for the
    // next eight hours. Reproduced — a note written mid-session, `db.stamps`
    // backdated so the interval was out of the picture, `source=compact`: the
    // note was never indexed and no further trigger fired that session. Denying
    // it bought a quiet `db.log` and paid with an index that silently stopped
    // growing, which is the trade this file refuses everywhere else.
    //
    // `None` for the age is the load-bearing case: no index has ever
    // succeeded, so nothing else could be making this decision.
    assert!(
        indexes(Some("compact"), None),
        "a compaction must index when no index has ever succeeded"
    );
    assert!(
        indexes(Some("compact"), Some(Duration::from_secs(3600))),
        "and an hour after the last one"
    );
    // Bounded, though — the interval is what makes it cheap, not a refusal.
    assert!(
        !indexes(Some("compact"), Some(Duration::from_secs(60))),
        "a compaction a minute after a good index must still skip"
    );
    let why = reason(Some("compact"), Some(Duration::from_secs(60)));
    assert!(
        why.contains("compact") && why.contains("interval"),
        "and it must be refused for its TIMING, naming the source: {why}"
    );
    assert!(
        !why.contains("continues the same session"),
        "the old refusal claimed nothing on disk had changed, which was false: {why}"
    );
}

#[test]
fn every_source_indexes_when_nothing_ran_recently() {
    // No source is denied. A user who always resumes, who works all day in one
    // process and only ever `/clear`s, or who lets that one process compact
    // itself, would otherwise never index at all — trading a storm of runs for
    // an index that silently stops growing, which is the worse failure.
    for source in SOURCES {
        assert!(
            indexes(Some(source), None),
            "{source} must index when no index has ever succeeded"
        );
        assert!(
            indexes(Some(source), Some(Duration::from_secs(86_400))),
            "{source} must index a day after the last one"
        );
    }
}

#[test]
fn the_rate_limit_is_what_stops_the_allowed_sources_repeating() {
    // Both sides of the boundary, for every source. One second under the
    // interval must skip and the interval itself must run: a test that only
    // checked "an hour ago indexes, a second ago skips" passes against any
    // threshold between the two.
    for source in SOURCES {
        assert!(
            !indexes(Some(source), Some(INTERVAL - Duration::from_secs(1))),
            "{source} one second inside the interval must skip"
        );
        assert!(
            indexes(Some(source), Some(INTERVAL)),
            "{source} exactly at the interval must index"
        );
        assert!(
            indexes(Some(source), Some(INTERVAL + Duration::from_secs(1))),
            "{source} one second past the interval must index"
        );
    }
}

#[test]
fn an_unknown_or_absent_source_still_indexes() {
    // Absent, empty, and a value this build has never heard of. A trigger we
    // cannot classify must not silently disable indexing — the failure that
    // hides is the one where nothing ever runs again.
    for source in [None, Some(""), Some("startup-but-newer"), Some("COMPACT")] {
        assert!(
            indexes(source, None),
            "{source:?} must index rather than be silently dropped"
        );
    }
    // …and is still rate-limited, so an unknown source cannot spin either.
    assert!(!indexes(None, Some(Duration::from_secs(60))));
}

#[test]
fn a_rate_limited_skip_says_which_source_and_how_long_ago() {
    // The house failure mode is a skip nobody can see. The reason string is
    // what lands in `db.log`, so it has to carry enough to diagnose a run that
    // did not happen: which trigger asked, how recently the last good index
    // was, and what the interval is.
    let why = reason(Some("resume"), Some(Duration::from_secs(42)));
    assert!(why.contains("resume"), "must name the source: {why}");
    assert!(why.contains("42"), "must name the age in seconds: {why}");
    assert!(
        why.contains(&INTERVAL.as_secs().to_string()),
        "must name the interval it was measured against: {why}"
    );
}

// ---------------------------------------------------------------------------
// The binary. Everything above is a pure function; none of it can catch a hook
// that hangs, and a hang is the failure that would be worse than the bug.
// ---------------------------------------------------------------------------

/// A scratch environment whose spawned indexer cannot do any work.
///
/// TWO INDEPENDENT GUARDS, and the history is why both are here rather than
/// one. An earlier version of this file had only the first; an adversarial
/// review of it found three orphaned `br8n index` processes on the
/// developer's REAL corpus after one `cargo test`, one with an ETA of 27,000
/// seconds, three-way contending for the single Ollama slot that every
/// latency and recall number in this project depends on.
///
/// GUARD 1 — the writer lock, held BY THE TEST PROCESS for as long as this
/// lives, so a `br8n index` the hook spawns dies on `IndexLock` within
/// milliseconds and writes its refusal into `db.log`. That refusal line is
/// also the DETECTOR that a process was spawned at all: positive evidence,
/// where the absence of a database directory is only the absence of evidence.
/// `Drop` below waits for it before letting the lock go.
///
/// GUARD 2 — `HOME`, pointed at this fixture's own directory. Guard 1 is a
/// RACE, and it is one this test can lose: the child is detached and starts
/// after the hook has already exited, so if it has not reached its lock
/// attempt by the time the wait gives up, the fixture drops, the lock is
/// released, and the `TempDir` holding `BR8N_CONFIG` is deleted underneath
/// it. `Config::load` never fails — it falls back to `Config::default()`,
/// whose `index_transcripts` is TRUE — and `TranscriptLoader::default_root()`
/// resolves `~/.claude/projects` through `HOME`. With `HOME` faked that
/// fallback finds no transcript root and no sources, so the worst case is a
/// child that indexes NOTHING instead of the developer's 762-document corpus.
/// A guard that can lose a race needs a second one that cannot.
struct Scratch {
    // Declared first so it drops first: the guard removes its own lock file,
    // which has to happen while the directory still exists.
    _lock: br8n::index::IndexLock,
    _dir: tempfile::TempDir,
    db: std::path::PathBuf,
    cfg: std::path::PathBuf,
    /// Passed to every child as `HOME`. Per-fixture rather than process-wide:
    /// `std::env::set_var` would race the other tests in this binary, which
    /// run in parallel, and only the SUBPROCESS's `HOME` matters here.
    home: std::path::PathBuf,
}

fn scratch() -> Scratch {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(
        &cfg,
        "sources = []\nindex_transcripts = false\n\n[embed]\nollama_url = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();
    let db = dir.path().join("db");
    // Created, so it is a real directory while the test runs; it disappears
    // with the `TempDir`, which is exactly the state guard 2 has to survive.
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let lock = br8n::index::IndexLock::acquire(&db).expect("a fresh scratch lock must be free");
    Scratch {
        _lock: lock,
        _dir: dir,
        db,
        cfg,
        home,
    }
}

impl Drop for Scratch {
    /// Wait for the indexer this hook spawned to die on the lock, BEFORE
    /// releasing that lock — and FAIL LOUDLY if it never showed up.
    ///
    /// The wait used to be `let _waited = indexer_was_spawned(…)`, discarding
    /// a `#[must_use]`. Under a loaded machine the debug binary's start
    /// exceeds the wait, and that spelling then let the fixture drop with the
    /// child still unaccounted for — silently. A guard that fails open without
    /// saying so is the house failure mode wearing test clothing, which is
    /// precisely how the orphans in the doc-comment above went unnoticed.
    ///
    /// The `panicking()` check is not a way back to failing open. Dropping
    /// during an unwind means an assertion in the test body ALREADY failed, so
    /// the run is red and nothing is hidden; panicking a second time there
    /// would abort the whole test binary and take every other test's result
    /// with it. On the normal path — the one that hides things — this panics.
    fn drop(&mut self) {
        if !log_of(&self.db).contains("SessionStart indexing") {
            return;
        }
        if indexer_was_spawned(&self.db, Duration::from_secs(60)) {
            return;
        }
        let msg = format!(
            "the hook logged `SessionStart indexing` but no child ever reached `IndexLock` \
             in 60s: it is now running unaccounted for, against whatever config it can \
             find. db={} log:\n{}",
            self.db.display(),
            log_of(&self.db)
        );
        if std::thread::panicking() {
            eprintln!("session_start_trigger: {msg}");
            return;
        }
        panic!("{msg}");
    }
}

fn log_of(db: &std::path::Path) -> String {
    std::fs::read_to_string(db.with_extension("log")).unwrap_or_default()
}

/// Did the hook actually spawn a `br8n index`?
///
/// The indexer is detached, so its output lands in `db.log` after the hook has
/// already exited — hence the poll. With the writer lock held it can only ever
/// print one thing, and `main` returns that error, so `Error: another
/// `br8n index` is already running` in the log is the child saying it existed.
#[must_use]
fn indexer_was_spawned(db: &std::path::Path, within: Duration) -> bool {
    let deadline = std::time::Instant::now() + within;
    loop {
        if log_of(db).contains("already running") {
            return true;
        }
        if std::time::Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Backdate `db.stamps` so `index::last_index_age` reads `age`.
///
/// The rate limit's only input. Writing the file with a real timestamp is what
/// lets the binary tests below exercise the interval without waiting.
fn last_index_was(db: &std::path::Path, age: Duration) {
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let stamps = db.with_extension("stamps");
    std::fs::write(&stamps, "{}").unwrap();
    let when = std::time::SystemTime::now() - age;
    let f = std::fs::File::options().write(true).open(&stamps).unwrap();
    f.set_times(
        std::fs::FileTimes::new()
            .set_accessed(when)
            .set_modified(when),
    )
    .unwrap();
}

/// Run `br8n hook session-start` with `stdin` piped in, and return `db.log`.
///
/// Takes the whole fixture rather than two paths so `HOME` cannot be forgotten
/// at a call site — see `Scratch`'s guard 2. It also asserts the stdout
/// contract on EVERY invocation, which is the cheapest place to put it: see
/// `the_hook_writes_nothing_at_all_to_stdout`.
fn hook_with_stdin(s: &Scratch, stdin: &str) -> String {
    let out = assert_cmd::Command::cargo_bin("br8n")
        .unwrap()
        .env("BR8N_DB", &s.db)
        .env("BR8N_CONFIG", &s.cfg)
        .env("HOME", &s.home)
        .args(["hook", "session-start"])
        .write_stdin(stdin.to_string())
        .timeout(Duration::from_secs(30))
        .output()
        .expect("the hook must not hang");
    // The status ALONE is not enough to act on. When this fired on CI it
    // reported nothing but its own text, so the failure could not be diagnosed from the log. The exit code and
    // stderr are what say whether the hook refused, crashed, or was killed.
    assert!(
        out.status.success(),
        "SessionStart must always exit 0, got {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "SessionStart stdout becomes model context; it must stay empty, got {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    log_of(&s.db)
}

#[test]
fn every_source_reaches_the_binary_and_is_recorded() {
    // The payload is read for real here: parsed off stdin, routed through the
    // decision, and the outcome written to `db.log`. With no `db.stamps` the
    // rate limit is out of the picture, so what this isolates is that the
    // source LABEL survives the trip — the source no longer changes the
    // decision, but it is the whole content of the log line that explains a
    // run to a human afterwards.
    for source in SOURCES {
        let s = scratch();
        let log = hook_with_stdin(
            &s,
            &format!(r#"{{"session_id":"abc","source":"{source}"}}"#),
        );
        assert!(
            log.contains(source),
            "the log must name the source it saw ({source}): {log}"
        );
        assert!(
            log.contains("SessionStart indexing"),
            "with no previous index every source must run, including {source}: {log}"
        );
        assert!(
            !log.contains("SessionStart did not index"),
            "and must not also record a skip for {source}: {log}"
        );
    }
}

#[test]
fn db_log_rotates_once_it_passes_ten_mebibytes() {
    let s = scratch();
    let log = s.db.with_extension("log");
    let rotated = log.with_extension("log.1");
    std::fs::write(&rotated, "STALE-PREVIOUS-ROTATION\n").unwrap();
    let filler = "x".repeat(1024 * 1024);
    let mut big = std::fs::File::create(&log).unwrap();
    for _ in 0..11 {
        big.write_all(filler.as_bytes()).unwrap();
    }
    big.write_all(b"PRE-ROTATION-MARKER\n").unwrap();
    drop(big);
    assert!(std::fs::metadata(&log).unwrap().len() > 10 * 1024 * 1024);

    hook_with_stdin(&s, r#"{"session_id":"abc","source":"startup"}"#);

    let rotated_content = std::fs::read_to_string(&rotated)
        .expect("db.log.1 must exist once db.log has passed the limit");
    assert!(rotated_content.contains("PRE-ROTATION-MARKER"));
    assert!(
        !rotated_content.contains("STALE-PREVIOUS-ROTATION"),
        "a fresh rotation must replace whatever db.log.1 already held: {rotated_content}"
    );

    let fresh = log_of(&s.db);
    assert!(
        !fresh.contains("PRE-ROTATION-MARKER"),
        "the old content must not remain in the fresh db.log: {fresh}"
    );
    assert!(
        fresh.contains("SessionStart indexing"),
        "the fresh db.log must still receive this run's own lines: {fresh}"
    );
    assert!(std::fs::metadata(&log).unwrap().len() < 10 * 1024 * 1024);
}

#[test]
fn a_rate_limited_trigger_spawns_no_indexer_at_all() {
    // Stronger than reading the hook's own log line, which only says what it
    // decided: this asserts on whether a `br8n index` PROCESS came into
    // existence. The positive control is the point — the same wait, on an
    // otherwise identical fixture, finds one when the interval has passed, so
    // the negative arm is a real difference and not a wait that is simply too
    // short for anything to show up in. Only `db.stamps`' mtime differs.
    let spawning = scratch();
    last_index_was(&spawning.db, Duration::from_secs(16 * 60));
    hook_with_stdin(&spawning, r#"{"source":"startup"}"#);
    assert!(
        indexer_was_spawned(&spawning.db, Duration::from_secs(20)),
        "control: past the interval, startup must spawn an indexer, or this test \
         proves nothing. log: {}",
        log_of(&spawning.db)
    );

    let limited = scratch();
    last_index_was(&limited.db, Duration::from_secs(60));
    hook_with_stdin(&limited, r#"{"source":"startup"}"#);
    assert!(
        !indexer_was_spawned(&limited.db, Duration::from_secs(5)),
        "a minute after a good index, nothing may be spawned. log: {}",
        log_of(&limited.db)
    );
}

#[test]
fn the_hook_writes_nothing_at_all_to_stdout() {
    // `SessionStart` STDOUT IS FED TO THE MODEL AS CONTEXT. A diagnostic
    // printed there does not merely look untidy — it lands in the user's
    // conversation, on every session, forever.
    //
    // Nothing asserted this. `log_line` writing to stdout IN ADDITION to
    // `db.log` was applied as a mutation and all 35 test binaries stayed
    // green, because every existing assertion reads `db.log` and none of them
    // could tell the difference. Byte counts, not `contains`, for the same
    // reason: the failure is any output at all, whatever it says.
    //
    // Both branches, because they print different lines through the same
    // `log_line`, and a skip is the branch that runs most often.
    for (label, stamps_age) in [
        ("indexing", Some(Duration::from_secs(16 * 60))),
        ("skipping", Some(Duration::from_secs(60))),
    ] {
        let s = scratch();
        if let Some(age) = stamps_age {
            last_index_was(&s.db, age);
        }
        let out = assert_cmd::Command::cargo_bin("br8n")
            .unwrap()
            .env("BR8N_DB", &s.db)
            .env("BR8N_CONFIG", &s.cfg)
            .env("HOME", &s.home)
            .args(["hook", "session-start"])
            .write_stdin(r#"{"session_id":"abc","source":"startup"}"#.to_string())
            .timeout(Duration::from_secs(30))
            .output()
            .expect("the hook must not hang");
        assert!(
            out.status.success(),
            "SessionStart must always exit 0, got {:?}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.stdout.len(),
            0,
            "the {label} branch wrote {} bytes to stdout, which become model \
             context: {:?}",
            out.stdout.len(),
            String::from_utf8_lossy(&out.stdout)
        );
        // The control: it did decide, and it did say so — somewhere that is
        // not stdout. Without this the assertion above passes on a hook that
        // does nothing at all.
        let log = log_of(&s.db);
        assert!(
            log.contains("SessionStart"),
            "the {label} branch must still have recorded its decision in db.log: {log}"
        );
    }
}

#[test]
fn the_binary_honours_the_rate_limit_on_both_sides() {
    // One minute since the last successful index: skip. Sixteen: run. Same
    // source, same everything else — only `db.stamps`' mtime differs.
    let recent = scratch();
    last_index_was(&recent.db, Duration::from_secs(60));
    let log = hook_with_stdin(&recent, r#"{"source":"startup"}"#);
    assert!(
        log.contains("SessionStart did not index"),
        "a minute after a good index, startup must skip: {log}"
    );
    assert!(
        log.contains("inside the"),
        "the skip must say it was the interval, not the source: {log}"
    );

    let old = scratch();
    last_index_was(&old.db, Duration::from_secs(16 * 60));
    let log2 = hook_with_stdin(&old, r#"{"source":"startup"}"#);
    assert!(
        log2.contains("SessionStart indexing"),
        "sixteen minutes after the last index, startup must run: {log2}"
    );
}

#[test]
fn absent_empty_and_garbage_stdin_all_index_without_hanging() {
    // `/dev/null` (no payload at all), an empty string, JSON with no `source`,
    // and something that is not JSON. None may hang, none may crash, and all
    // must fall through to "unknown source" — which indexes, because a payload
    // this build cannot read must not disable indexing.
    for stdin in ["", "not json at all {{{", "{}", r#"{"session_id":"x"}"#] {
        let s = scratch();
        let log = hook_with_stdin(&s, stdin);
        assert!(
            log.contains("SessionStart indexing") && log.contains("unknown"),
            "stdin {stdin:?} must index as an unknown source: {log}"
        );
    }
}

#[test]
fn a_stdin_that_never_closes_does_not_hang_the_hook() {
    // THE hazard. `run_session_start` did not read stdin at all before this
    // change; `read_to_string` blocks until EOF, and a pipe whose writer never
    // writes and never closes never delivers one. Claude Code would sit on the
    // hook until its own 60s timeout and session startup would stall — far
    // worse than the storm of index runs being fixed.
    //
    // The stdin handle is deliberately held open for the whole wait, so the
    // child cannot see EOF. `write_stdin`-based helpers cannot express this:
    // they close the pipe, which is exactly the case that was never in danger.
    let s = scratch();
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("br8n"))
        .env("BR8N_DB", &s.db)
        .env("BR8N_CONFIG", &s.cfg)
        .env("HOME", &s.home)
        .args(["hook", "session-start"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let _held_open = child.stdin.take().expect("keep the write end alive");

    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait().unwrap() {
            Some(s) => break s,
            None if started.elapsed() > Duration::from_secs(20) => {
                let _ = child.kill();
                panic!(
                    "the hook was still blocked on stdin after 20s; Claude Code's own \
                     timeout is 60s and session startup would have stalled"
                );
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    assert!(status.success(), "and it must still exit 0");
    // Not merely "it exited" — it exited having DECIDED, so the bounded read
    // fell through to the unknown-source path rather than aborting early.
    let log = log_of(&s.db);
    assert!(
        log.contains("SessionStart indexing") && log.contains("unknown"),
        "a hook that gave up on stdin must still decide and record it: {log}"
    );
}

fn hook_stdout(s: &Scratch, stdin: &str) -> String {
    let out = assert_cmd::Command::cargo_bin("br8n")
        .unwrap()
        .env("BR8N_DB", &s.db)
        .env("BR8N_CONFIG", &s.cfg)
        .env("HOME", &s.home)
        .args(["hook", "session-start"])
        .write_stdin(stdin.to_string())
        .timeout(Duration::from_secs(30))
        .output()
        .expect("the hook must not hang");
    assert!(out.status.success(), "SessionStart must always exit 0");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn seed_lesson(s: &Scratch, id: &str, text: &str, project: Option<&str>) {
    use br8n::pack::records::Record;
    let root = br8n::memory::root(&s.db);
    let dir = br8n::memory::pack_dir(&root);
    std::fs::create_dir_all(&dir).unwrap();
    let existing: Vec<(Record, Vec<f32>)> = if dir.join("pack.rec").exists() {
        let r = br8n::pack::records::Reader::open(&dir).unwrap();
        (0..r.len())
            .map(|i| (r.get(i).unwrap(), vec![0.5; 4]))
            .collect()
    } else {
        Vec::new()
    };
    let mut rows = existing;
    rows.push((
        Record {
            chunk_id: format!("{id}:0"),
            doc_id: id.into(),
            text: text.into(),
            heading_path: String::new(),
            uri: format!("memory://lesson/{id}"),
            title: text.into(),
            page_no: None,
            source_type: "memory".into(),
            inbound: 0,
            lifecycle: Default::default(),
            last_used: None,
            memory: Some(br8n::memory::MemoryFacts {
                kind: br8n::memory::MemoryKind::Lesson,
                created: 1_756_684_800,
                project: project.map(String::from),
                origin: br8n::memory::Origin::User,
                confidence: 100,
                session: None,
                source_hash: None,
                source_stamp: None,
            }),
        },
        vec![0.5; 4],
    ));
    rows.sort_by(|a, b| a.0.chunk_id.cmp(&b.0.chunk_id));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    br8n::pack::Pack::build(
        &dir,
        "fake@4",
        4,
        rows,
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
}

#[test]
fn session_start_prints_applicable_lessons_as_plain_text() {
    let s = scratch();
    seed_lesson(
        &s,
        "aaaaaaaaaaaa",
        "Always squash before opening a PR.",
        None,
    );
    seed_lesson(
        &s,
        "bbbbbbbbbbbb",
        "In this repo run cargo fmt first.",
        Some("/Users/x/repo"),
    );
    seed_lesson(
        &s,
        "cccccccccccc",
        "Use pnpm in the web repo.",
        Some("/Users/x/web"),
    );
    let out = hook_stdout(&s, r#"{"source":"startup","cwd":"/Users/x/repo/src"}"#);
    assert!(out.starts_with("<br8n-lessons>"), "got {out:?}");
    assert!(out.contains("squash") && out.contains("cargo fmt"));
    assert!(!out.contains("pnpm"));
    assert!(!out.trim_start().starts_with('{'));
}

#[test]
fn session_start_stays_silent_when_no_lesson_applies() {
    let s = scratch();
    seed_lesson(
        &s,
        "cccccccccccc",
        "Use pnpm in the web repo.",
        Some("/Users/x/web"),
    );
    let out = hook_stdout(&s, r#"{"source":"startup","cwd":"/Users/x/repo"}"#);
    assert!(out.is_empty(), "got {out:?}");
}

#[test]
fn a_corrupt_lessons_pack_is_reported_in_the_log_not_swallowed() {
    let s = scratch();
    let root = br8n::memory::root(&s.db);
    let dir = br8n::memory::pack_dir(&root);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("pack.rec"), b"{}").unwrap();
    std::fs::write(dir.join("pack.recidx"), [0u8; 3]).unwrap();
    let log = hook_with_stdin(&s, r#"{"source":"startup"}"#);
    assert!(
        log.contains("lessons pack unreadable") && log.contains("not injected"),
        "a corrupt lessons pack must be reported in db.log rather than read back as \
         no lessons: {log}"
    );
}

#[test]
fn an_enormous_stdin_is_truncated_rather_than_buffered_whole() {
    // `read_to_string` has no ceiling. `br8n hook session-start < /dev/zero`
    // exits in 0.8s — the timeout works — having peaked at 992 MB RSS, because
    // the timeout bounds how LONG the read waits and nothing bounded how MUCH
    // it read. `STDIN_CAP` is the other half.
    //
    // MEASURING THE CAP, NOT THE MEMORY. An RSS assertion would be a
    // platform-specific flake, so this uses the cap's one behavioural
    // consequence instead: past it the JSON no longer parses, so the source
    // label is lost and the decision falls through to `unknown` — which
    // indexes, which is why losing the label is safe. Uncapped, this exact
    // payload parses and the log says `startup`.
    //
    // The control is the same payload under the cap. Without it this passes on
    // a hook that has stopped reading stdin at all.
    let small = scratch();
    let log = hook_with_stdin(
        &small,
        &format!(
            r#"{{"padding":"{}","source":"startup"}}"#,
            "p".repeat(64 * 1024)
        ),
    );
    assert!(
        log.contains("source=startup"),
        "control: 64 KiB is under the cap and must still be parsed: {log}"
    );

    let huge = scratch();
    let log = hook_with_stdin(
        &huge,
        &format!(
            r#"{{"padding":"{}","source":"startup"}}"#,
            "p".repeat(2 * 1024 * 1024)
        ),
    );
    assert!(
        log.contains("SessionStart indexing"),
        "an oversized payload must still index — failing open is the point: {log}"
    );
    assert!(
        log.contains("unknown"),
        "and it must read as an unknown source, which is what proves the read \
         was cut short rather than buffered whole: {log}"
    );
}

fn answering_embed_stub() -> (String, std::sync::mpsc::Receiver<String>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut buf = [0u8; 65536];
            let n = stream.read(&mut buf).unwrap_or(0);
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
            let body = r#"{"data":[{"index":0,"embedding":[1.0,0.0]}]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    (url, rx)
}

#[test]
fn a_remote_endpoint_is_loaded_in_the_background_not_warmed_inline() {
    let s = scratch();
    std::fs::write(
        &s.cfg,
        "sources = []\nindex_transcripts = false\n\n[embed]\ndimensions = 2\nollama_url = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();
    let (url, rx) = answering_embed_stub();
    let out = assert_cmd::Command::cargo_bin("br8n")
        .unwrap()
        .env("BR8N_DB", &s.db)
        .env("BR8N_CONFIG", &s.cfg)
        .env("HOME", &s.home)
        .env("BR8N_EMBED_URL", &url)
        .env("BR8N_EMBED_MODEL", "wire-name")
        .env("BR8N_EMBED_TOKEN", "tok")
        .args(["hook", "session-start"])
        .write_stdin(r#"{"source":"startup"}"#)
        .timeout(Duration::from_secs(30))
        .output()
        .expect("the hook must not hang");
    assert!(out.status.success(), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");

    let req = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the background load must reach the remote endpoint");
    assert!(req.contains("POST /v1/embeddings"), "{req}");

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let log = loop {
        let log = log_of(&s.db);
        if log.contains("remote embedding model loaded in") || std::time::Instant::now() > deadline
        {
            break log;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        log.contains("SessionStart loading the remote embedding model in the background"),
        "{log}"
    );
    assert!(log.contains("remote embedding model loaded in"), "{log}");
}

#[test]
fn a_second_session_start_does_not_stack_another_remote_load() {
    let s = scratch();
    std::fs::write(
        &s.cfg,
        "sources = []\nindex_transcripts = false\n\n[embed]\ndimensions = 2\nollama_url = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();
    let (url, _rx) = answering_embed_stub();
    for _ in 0..2 {
        let out = assert_cmd::Command::cargo_bin("br8n")
            .unwrap()
            .env("BR8N_DB", &s.db)
            .env("BR8N_CONFIG", &s.cfg)
            .env("HOME", &s.home)
            .env("BR8N_EMBED_URL", &url)
            .env("BR8N_EMBED_MODEL", "wire-name")
            .env("BR8N_EMBED_TOKEN", "tok")
            .args(["hook", "session-start"])
            .write_stdin(r#"{"source":"startup"}"#)
            .timeout(Duration::from_secs(30))
            .output()
            .expect("the hook must not hang");
        assert!(out.status.success(), "{out:?}");
    }
    let log = log_of(&s.db);
    assert_eq!(
        log.matches("SessionStart loading the remote embedding model in the background")
            .count(),
        1,
        "{log}"
    );
    assert!(
        log.contains("SessionStart skipped the remote model load"),
        "{log}"
    );
}

#[test]
fn a_broken_env_file_is_logged_at_session_start() {
    let s = scratch();
    let out = assert_cmd::Command::cargo_bin("br8n")
        .unwrap()
        .env("BR8N_DB", &s.db)
        .env("BR8N_CONFIG", &s.cfg)
        .env("HOME", &s.home)
        .env("BR8N_EMBED_URL", "http://127.0.0.1:1")
        .env_remove("BR8N_EMBED_MODEL")
        .env("BR8N_EMBED_TOKEN", "tok")
        .args(["hook", "session-start"])
        .write_stdin(r#"{"source":"startup"}"#)
        .timeout(Duration::from_secs(30))
        .output()
        .expect("the hook must not hang");
    assert!(out.status.success(), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    let log = log_of(&s.db);
    assert!(
        log.contains("SessionStart cannot embed") && log.contains("BR8N_EMBED_MODEL"),
        "{log}"
    );
}
