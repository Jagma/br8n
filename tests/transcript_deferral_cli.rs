//! The transcript deferral, through the real binary.
//!
//! `tests/transcript_settle.rs` pins `discover_stat_first`, which is where the
//! decision lives. Nothing there can catch the two failures that actually
//! shipped, because both are in what `reindex_swap_with` does with that
//! decision afterwards:
//!
//!   * `br8n index --reindex` REMOVED every live transcript from the index.
//!     `FromScratch` passes an empty stamp map, so every transcript looks
//!     changed; deferring one then leaves it out of a shadow that was built
//!     from nothing, and `live_uris` cannot save it because there is no
//!     previous copy to preserve. Reproduced end to end at 3 documents before
//!     and 2 after, with the third gone for as long as its session stayed
//!     alive.
//!   * The `N skipped` count and the `br8n: deferring …` stderr line are the
//!     ONLY two channels that report a deferral on a run whose corpus is
//!     otherwise unchanged — `br8n status` cannot, because persisting the
//!     skip record needs the store's exclusive lock that the fast path exists
//!     to avoid taking. Both were unpinned; deleting either left the whole
//!     suite green.
//!
//! `HOME` is set per SUBPROCESS here, never with `std::env::set_var`. These
//! tests spawn processes, and mutating the parent's environment while another
//! thread is spawning is precisely the race that makes `set_var` unsafe.

mod common;

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// A whole world: a notes root, a fake `HOME` with a transcript directory, and
/// a config wired to `fake_ollama` so nothing reaches the network.
struct World {
    dir: tempfile::TempDir,
    cfg: PathBuf,
    db: PathBuf,
    home: PathBuf,
    notes: PathBuf,
    projects: PathBuf,
}

fn world() -> World {
    let ollama = common::fake_ollama();
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("notes");
    let home = dir.path().join("home");
    let projects = home.join(".claude/projects/proj");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::create_dir_all(&projects).unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            "index_transcripts = true\nsources = [\"{}\"]\n\n[embed]\nollama_url = \"{ollama}\"\n",
            notes.display()
        ),
    )
    .unwrap();
    let db = dir.path().join("db");
    World {
        dir,
        cfg,
        db,
        home,
        notes,
        projects,
    }
}

fn world_with_max_age_days(days: u32) -> World {
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("notes");
    let home = dir.path().join("home");
    let projects = home.join(".claude/projects/proj");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::create_dir_all(&projects).unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            "index_transcripts = true\nindex_transcripts_max_age_days = {days}\n\
             sources = [\"{}\"]\n\n[embed]\nollama_url = \"http://127.0.0.1:1\"\n",
            notes.display()
        ),
    )
    .unwrap();
    let db = dir.path().join("db");
    World {
        dir,
        cfg,
        db,
        home,
        notes,
        projects,
    }
}

impl World {
    fn br8n(&self) -> Command {
        let mut c = Command::cargo_bin("br8n").unwrap();
        // `HOME` is what `TranscriptLoader::default_root()` resolves
        // `~/.claude/projects` against, so this is what keeps the run inside
        // the fixture instead of on the developer's real corpus.
        c.env("BR8N_DB", &self.db)
            .env("BR8N_CONFIG", &self.cfg)
            .env("HOME", &self.home)
            .env("CODEX_HOME", self.home.join(".codex"));
        c
    }

    fn note(&self, name: &str, body: &str) {
        std::fs::write(self.notes.join(name), body).unwrap();
    }

    /// A transcript the real loader will accept. An unparseable one lands in
    /// `skipped` as a LOAD failure, which reads exactly like a deferral in the
    /// counts below and would make these tests pass for the wrong reason.
    fn transcript(&self, name: &str, turns: usize) -> PathBuf {
        let p = self.projects.join(format!("{name}.jsonl"));
        let mut body = String::new();
        for i in 0..turns {
            body.push_str(&format!(
                "{{\"cwd\":\"/w/proj\",\"timestamp\":\"2026-08-3{}T09:00:00Z\",\
                 \"message\":{{\"role\":\"user\",\"content\":\"turn {i} about connection \
                 pooling and retrieval latency in the store\"}}}}\n",
                i % 10
            ));
        }
        std::fs::write(&p, body).unwrap();
        p
    }

    /// Every `claude-session://` URI the published index actually holds.
    ///
    /// Read off the store rather than parsed out of `br8n status`, because
    /// what these tests need is WHICH documents survived, not how many.
    fn indexed_transcripts(&self) -> Vec<String> {
        let store = br8n::store::Store::open_existing(&self.db, 512).unwrap();
        let mut v: Vec<String> = store
            .all_doc_uris()
            .unwrap()
            .into_iter()
            .map(|(_, uri)| uri)
            .filter(|u| u.starts_with("claude-session://"))
            .collect();
        v.sort();
        v
    }

    /// The stored content hash of the document at `uri`.
    ///
    /// The discriminator between "the document is still there" and "the
    /// document was actually re-read": a deferral leaves the previous hash
    /// untouched, a rebuild that read the file changes it.
    fn hash_of(&self, uri: &str) -> String {
        let store = br8n::store::Store::open_existing(&self.db, 512).unwrap();
        let (id, _) = store
            .all_doc_uris()
            .unwrap()
            .into_iter()
            .find(|(_, u)| u == uri)
            .unwrap_or_else(|| panic!("{uri} is not in the index"));
        store
            .doc_hash(&id)
            .unwrap()
            .unwrap_or_else(|| panic!("{uri} has no stored hash"))
    }
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

/// The `N skipped` field of `indexed: … N skipped …`.
fn skipped_in(stdout: &str) -> usize {
    stdout
        .lines()
        .find(|l| l.starts_with("indexed:"))
        .and_then(|l| {
            l.split(", ")
                .find_map(|f| f.strip_suffix(" skipped"))
                .and_then(|n| n.trim().parse().ok())
        })
        .unwrap_or_else(|| panic!("no `indexed: … skipped …` line in:\n{stdout}"))
}

#[test]
fn reindex_keeps_a_live_transcript_instead_of_deleting_it() {
    // THE BLOCKER. A rebuild from nothing has no earlier version of anything,
    // so a file it declines to read is a file it publishes without.
    let w = world();
    w.note("note.md", "# Pooling\n\nPgBouncer transaction mode.\n");
    let live = w.transcript("live", 4);
    let quiet = w.transcript("quiet", 4);
    aged(&live, Duration::from_secs(3600));
    aged(&quiet, Duration::from_secs(3600));

    w.br8n().arg("index").assert().success();
    assert_eq!(
        w.indexed_transcripts(),
        vec![uri_of(&live), uri_of(&quiet)],
        "both transcripts must be indexed before this test means anything"
    );
    let before = w.hash_of(&uri_of(&live));

    // The session appends, so the file is now both CHANGED and FRESH — the
    // only state in which deferral does anything at all.
    append_turn(&live, "and then we changed the fusion weights");
    aged(&live, Duration::from_secs(20));

    // Control: a plain incremental run really does defer it, and really does
    // keep it. Without this the assertion below passes if deferral has simply
    // stopped working.
    let out = w.br8n().arg("index").output().unwrap();
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("br8n: deferring"),
        "control: an incremental run must defer the fresh transcript. stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        w.indexed_transcripts(),
        vec![uri_of(&live), uri_of(&quiet)],
        "control: deferring must not remove it either"
    );
    assert_eq!(
        w.hash_of(&uri_of(&live)),
        before,
        "control: and the incremental run must genuinely have left it alone — \
         an equal hash is what makes the inequality below mean something"
    );

    // And now the case that lost data.
    w.br8n().args(["index", "--reindex"]).assert().success();
    assert_eq!(
        w.indexed_transcripts(),
        vec![uri_of(&live), uri_of(&quiet)],
        "`--reindex` must REBUILD the live transcript, not drop it"
    );
    // Rebuilt from the file on disk, not carried across from the old index:
    // the content hash has to have moved, because the file gained a turn.
    assert_ne!(
        w.hash_of(&uri_of(&live)),
        before,
        "`--reindex` must have READ the live transcript, not merely kept a copy"
    );
    drop(w.dir);
}

#[test]
fn the_deferral_line_reaches_stderr_on_a_real_run() {
    // On a run whose only change is a live transcript this is the ONLY channel
    // that says so — `br8n status` shows the previous run's skip list, because
    // the fast path returns before it can take the store's lock to persist a
    // new one. `SessionStart` redirects the indexer's stderr into `db.log`, so
    // this line is what a human can still go and read.
    //
    // The line was unpinned: deleting the `eprintln!` left all 35 test
    // binaries green.
    let w = world();
    w.note("note.md", "# Pooling\n\nPgBouncer transaction mode.\n");
    let live = w.transcript("live", 4);
    aged(&live, Duration::from_secs(3600));
    w.br8n().arg("index").assert().success();

    append_turn(&live, "and then we changed the fusion weights");
    aged(&live, Duration::from_secs(20));

    let out = w.br8n().arg("index").output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("br8n: deferring"),
        "the deferral must be announced: {err}"
    );
    assert!(
        err.contains("live.jsonl"),
        "and it must name the file, or it cannot be acted on: {err}"
    );
    assert!(
        err.contains("still being written"),
        "and say why it was left, not only that it was: {err}"
    );
    // Not stdout. `br8n index`'s stdout is its summary; a diagnostic there
    // would also land in `SessionStart`'s model context by the same redirect.
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("deferring"),
        "the deferral belongs on stderr"
    );
    drop(w.dir);
}

#[test]
fn a_deferred_transcript_is_counted_as_skipped_on_both_paths() {
    // Two code paths add `deferred.len()` to the skip count — the
    // unchanged-corpus fast path in `reindex_swap_with`, and the real build
    // below it — and deleting BOTH left the suite green. The number is
    // user-facing: `indexed: 0 added, 0 updated, N skipped` is how a run says
    // it looked at something and chose not to read it.
    //
    // The corpus is sized so the two counts differ from each other and from
    // the un-deferred answer: 1 note + 2 transcripts, one of which is live.
    let w = world();
    w.note("note.md", "# Pooling\n\nPgBouncer transaction mode.\n");
    let live = w.transcript("live", 4);
    let quiet = w.transcript("quiet", 4);
    aged(&live, Duration::from_secs(3600));
    aged(&quiet, Duration::from_secs(3600));
    w.br8n().arg("index").assert().success();

    append_turn(&live, "and then we changed the fusion weights");
    aged(&live, Duration::from_secs(20));

    // FAST PATH. Nothing changed except the live transcript, which carries its
    // previous fingerprint forward, so the key sets match and the run returns
    // before building anything. unchanged = note + quiet = 2, deferred = 1.
    let fast = w.br8n().arg("index").output().unwrap();
    let fast_out = String::from_utf8_lossy(&fast.stdout);
    assert!(
        fast_out.contains("0 added, 0 updated"),
        "this run must reach the fast path: {fast_out}"
    );
    assert_eq!(
        skipped_in(&fast_out),
        3,
        "the fast path must count the deferred transcript, not only the two \
         unchanged files: {fast_out}"
    );

    // WORKING PATH. Touch the note so there is real work; the transcript is
    // still fresh. docs = note, unchanged = quiet, deferred = live.
    w.note(
        "note.md",
        "# Pooling\n\nPgBouncer transaction mode, revised.\n",
    );
    aged(&live, Duration::from_secs(20));
    let slow = w.br8n().arg("index").output().unwrap();
    let slow_out = String::from_utf8_lossy(&slow.stdout);
    assert!(
        slow_out.contains("1 added, 0 updated") || slow_out.contains("0 added, 1 updated"),
        "this run must do real work: {slow_out}"
    );
    assert_eq!(
        skipped_in(&slow_out),
        2,
        "the working path must count the deferred transcript alongside the one \
         unchanged file: {slow_out}"
    );
    drop(w.dir);
}

#[test]
fn a_transcript_that_ages_past_the_limit_is_pruned_by_a_real_index_run() {
    let w = world_with_max_age_days(30);
    w.note("note.md", "# Pooling\n\nPgBouncer transaction mode.\n");
    let live = w.transcript("live", 4);
    aged(&live, Duration::from_secs(5 * 86_400));

    w.br8n().args(["index", "--no-embed"]).assert().success();
    assert_eq!(
        w.indexed_transcripts(),
        vec![uri_of(&live)],
        "the transcript must be indexed before this test means anything"
    );

    aged(&live, Duration::from_secs(40 * 86_400));

    w.br8n().args(["index", "--no-embed"]).assert().success();
    assert!(
        w.indexed_transcripts().is_empty(),
        "a transcript that aged past the configured limit, with nothing else \
         changed, must be pruned by the very next run rather than surviving \
         under a wrongly-taken no-op fast path"
    );
}
