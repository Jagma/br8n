use crate::common;

use assert_cmd::Command;
use std::path::{Path, PathBuf};

fn db(t: &Path) -> PathBuf {
    t.join("db")
}

fn pending_logs(t: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(t)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix("db.usage."))
                .is_some_and(|pid| pid.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect()
}

#[test]
fn concurrent_writers_do_not_lose_or_interleave_records() {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(db(t.path())).unwrap();

    let handles: Vec<_> = (0..4)
        .map(|w| {
            let d = db(t.path());
            std::thread::spawn(move || {
                for i in 0..50 {
                    br8n::usage::record(&d, &[format!("doc-{w}-{i}")]);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let folded = br8n::usage::fold(&db(t.path())).unwrap();
    assert_eq!(folded, 200, "every record written must survive the fold");

    let map = br8n::usage::load(&db(t.path())).unwrap();
    assert_eq!(map.len(), 200, "and each document must appear exactly once");
    assert!(
        map.values().all(|u| u.last_used.is_some()),
        "a folded record means the document was retrieved, so last_used is set"
    );
    assert!(
        pending_logs(t.path()).is_empty(),
        "a folded log must be removed"
    );
}

#[test]
fn folding_twice_changes_nothing_the_second_time() {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(db(t.path())).unwrap();
    br8n::usage::record(&db(t.path()), &["doc-a".to_string()]);
    br8n::usage::fold(&db(t.path())).unwrap();
    let first = br8n::usage::load(&db(t.path())).unwrap();

    let folded = br8n::usage::fold(&db(t.path())).unwrap();
    assert_eq!(folded, 0, "a second fold has nothing to fold");
    let second = br8n::usage::load(&db(t.path())).unwrap();
    assert_eq!(first.get("doc-a"), second.get("doc-a"));
}

#[test]
fn a_later_fold_keeps_what_an_earlier_fold_recorded() {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(db(t.path())).unwrap();
    br8n::usage::record(&db(t.path()), &["doc-a".to_string()]);
    br8n::usage::fold(&db(t.path())).unwrap();
    br8n::usage::record(&db(t.path()), &["doc-b".to_string()]);
    br8n::usage::fold(&db(t.path())).unwrap();

    let map = br8n::usage::load(&db(t.path())).unwrap();
    assert!(map.contains_key("doc-a"), "the first fold's entry was lost");
    assert!(map.contains_key("doc-b"));
}

#[test]
fn recording_into_an_unwritable_location_is_silent_and_harmless() {
    let missing = Path::new("/nonexistent-br8n-usage-test/db");
    br8n::usage::record(missing, &["doc-a".to_string()]);
}

struct Fixture {
    t: tempfile::TempDir,
    db: PathBuf,
    cfg: PathBuf,
}

impl Fixture {
    fn indexed() -> Fixture {
        let t = tempfile::tempdir().unwrap();
        let notes = t.path().join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        let body = "PgBouncer runs in transaction mode and drops session state. ".repeat(20);
        for name in ["a", "b", "c"] {
            std::fs::write(
                notes.join(format!("{name}.md")),
                format!("# Pooling {name}\n\n{body}"),
            )
            .unwrap();
        }
        let ollama = common::fake_ollama();
        let cfg = t.path().join("config.toml");
        std::fs::write(
            &cfg,
            format!(
                "index_transcripts = false\nsources = [\"{}\"]\n\n[embed]\nollama_url = \"{ollama}\"\n\n[hook]\nmax_tokens = 150\n",
                notes.display()
            ),
        )
        .unwrap();
        let f = Fixture {
            db: db(t.path()),
            cfg,
            t,
        };
        f.br8n().arg("index").assert().success();
        f
    }

    fn br8n(&self) -> Command {
        let mut c = Command::cargo_bin("br8n").unwrap();
        c.env("BR8N_DB", &self.db).env("BR8N_CONFIG", &self.cfg);
        c
    }

    fn prompt(&self) -> String {
        let out = self
            .br8n()
            .args(["hook", "prompt"])
            .write_stdin(r#"{"prompt":"why did the connection pooler drop session state"}"#)
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn pack(&self) -> br8n::pack::Pack {
        let cfg = br8n::config::Config::default();
        let model_id =
            br8n::embed::Embedder::model_id(&br8n::embed::OllamaEmbedder::new(&cfg.embed));
        br8n::pack::open_pack_beside(&self.db, &model_id, cfg.embed.dimensions)
            .unwrap()
            .expect("the index publishes a pack")
    }
}

#[test]
fn the_hook_records_only_what_it_injected_and_the_next_index_folds_it() {
    let f = Fixture::indexed();
    let stdout = f.prompt();
    assert_eq!(
        stdout.matches("file://").count(),
        1,
        "the fixture must retrieve several notes but inject exactly one, got: {stdout}"
    );

    let logs = pending_logs(f.t.path());
    assert_eq!(logs.len(), 1, "one hook process writes one log");
    let recorded = std::fs::read_to_string(&logs[0]).unwrap();
    assert_eq!(
        recorded.lines().count(),
        1,
        "only the injected document is used, not every gated candidate: {recorded:?}"
    );

    let index = f.br8n().arg("index").output().unwrap();
    assert!(index.status.success());
    assert!(
        String::from_utf8_lossy(&index.stderr).contains("folded 1 usage records"),
        "the next index run must fold the log: {}",
        String::from_utf8_lossy(&index.stderr)
    );
    assert!(pending_logs(f.t.path()).is_empty());
    let map = br8n::usage::load(&f.db).unwrap();
    assert_eq!(map.len(), 1);
    assert!(map.values().all(|u| u.last_used.is_some()));
}

#[test]
fn the_pack_published_after_a_use_carries_it_on_both_publish_paths() {
    let f = Fixture::indexed();
    f.prompt();
    let logs = pending_logs(f.t.path());
    let recorded = std::fs::read_to_string(&logs[0]).unwrap();
    let doc_id = recorded.lines().next().unwrap().split_once(' ').unwrap().1;
    let chunk = format!("{doc_id}:0");
    assert_eq!(f.pack().last_used_of(&chunk), None);

    std::fs::write(
        f.t.path().join("notes/d.md"),
        "# Sourdough\n\nFeed the starter twice a day.",
    )
    .unwrap();
    f.br8n().arg("index").assert().success();
    let after_index = f.pack().last_used_of(&chunk);
    assert!(
        after_index.is_some(),
        "an ordinary index run must publish the folded use into pack.used"
    );

    f.br8n().args(["index", "--compact"]).assert().success();
    assert_eq!(
        f.pack().last_used_of(&chunk),
        after_index,
        "compaction republishes the pack and must carry the use across"
    );
}

fn used_doc_id(f: &Fixture) -> String {
    f.prompt();
    let logs = pending_logs(f.t.path());
    let recorded = std::fs::read_to_string(&logs[0]).unwrap();
    for log in logs {
        std::fs::remove_file(log).unwrap();
    }
    recorded
        .lines()
        .next()
        .unwrap()
        .split_once(' ')
        .unwrap()
        .1
        .to_string()
}

#[test]
fn a_document_unused_for_decades_keeps_its_usage_while_it_is_still_indexed() {
    let f = Fixture::indexed();
    let doc_id = used_doc_id(&f);
    let log = PathBuf::from(format!("{}.usage.999999", f.db.display()));
    std::fs::write(&log, format!("1 {doc_id}\n1 never-indexed\n")).unwrap();
    std::fs::write(
        f.t.path().join("notes/d.md"),
        "# Sourdough\n\nFeed the starter twice a day.",
    )
    .unwrap();

    let index = f.br8n().arg("index").output().unwrap();
    assert!(index.status.success());
    let stderr = String::from_utf8_lossy(&index.stderr);
    assert!(
        stderr.contains("dropped usage records for 1 documents no longer indexed"),
        "{stderr}"
    );

    let map = br8n::usage::load(&f.db).unwrap();
    assert_eq!(
        map.get(&doc_id).and_then(|u| u.last_used),
        Some(1),
        "an indexed document must keep its last use however old it is, or decay reads it as never used"
    );
    assert!(!map.contains_key("never-indexed"));
}

#[test]
fn usage_leaves_with_the_document_that_left_the_index() {
    let f = Fixture::indexed();
    let doc_id = used_doc_id(&f);
    let log = PathBuf::from(format!("{}.usage.999999", f.db.display()));
    std::fs::write(&log, format!("1 {doc_id}\n")).unwrap();
    f.br8n().arg("index").assert().success();
    assert!(br8n::usage::load(&f.db).unwrap().contains_key(&doc_id));

    for name in ["a", "b", "c"] {
        std::fs::remove_file(f.t.path().join(format!("notes/{name}.md"))).unwrap();
    }
    std::fs::write(
        f.t.path().join("notes/d.md"),
        "# Sourdough\n\nFeed the starter twice a day.",
    )
    .unwrap();
    f.br8n().arg("index").assert().success();

    assert!(
        !br8n::usage::load(&f.db).unwrap().contains_key(&doc_id),
        "a document the index no longer holds must not keep a usage entry"
    );
}

#[test]
fn compaction_drops_usage_for_documents_it_does_not_publish() {
    let f = Fixture::indexed();
    let doc_id = used_doc_id(&f);
    let log = PathBuf::from(format!("{}.usage.999999", f.db.display()));
    std::fs::write(&log, format!("1 {doc_id}\n1 never-indexed\n")).unwrap();

    f.br8n().args(["index", "--compact"]).assert().success();

    let map = br8n::usage::load(&f.db).unwrap();
    assert!(map.contains_key(&doc_id));
    assert!(!map.contains_key("never-indexed"));
}
