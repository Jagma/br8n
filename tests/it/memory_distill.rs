use crate::common;

use br8n::config::Config;
use br8n::loaders::transcript::{SessionAgent, SessionRoots};
use br8n::memory::distill::{
    build_prompt, candidates, distill_input, distill_pending_at, distill_session_at, Known,
};
use br8n::memory::{list_at, Filter, MemoryKind, Outcome};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn fixture_root() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("projects").join("-Users-x-repo");
    std::fs::create_dir_all(&projects).unwrap();
    let dst = projects.join("11111111-2222-3333-4444-555555555555.jsonl");
    std::fs::copy("tests/fixtures/distill/finished.jsonl", &dst).unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(5 * 3600);
    std::fs::File::options()
        .write(true)
        .open(&dst)
        .unwrap()
        .set_modified(old)
        .unwrap();
    (dir, dst)
}

fn roots(dir: &Path) -> SessionRoots {
    SessionRoots {
        claude_code: dir.join("projects"),
        codex: dir.join("codex").join("sessions"),
    }
}

fn cfg(url: &str) -> Config {
    let mut c = Config::default();
    c.embed.ollama_url = url.into();
    c.embed.dimensions = 512;
    c
}

#[test]
fn distill_input_keeps_typed_turns_and_drops_tool_noise() {
    let md = "## user (turn 1)\n\nPlease fix the pack.\n\n## assistant (turn 2)\n\nI'll add a guard.\n[tool: Read src/pack/mod.rs]\n\n## user (turn 3)\n\n<command-name>compact</command-name>\nActual text after the command.\n[result] pub fn build…\n\n## assistant (turn 4)\n\n<br8n-context>\nstuff\n</br8n-context>\nDone: the guard is in.\n\n## user (turn 5)\n\n<command-args>\nThis text after an unmatched open tag must not survive.\n";
    let input = distill_input(md);
    assert!(input.contains("Please fix the pack."));
    assert!(input.contains("Done: the guard is in."));
    assert!(input.contains("Actual text after the command."));
    assert!(!input.contains("[tool:"));
    assert!(!input.contains("[result]"));
    assert!(!input.contains("br8n-context"));
    assert!(
        !input.contains("compact"),
        "text between an open and close command span must not survive: {input}"
    );
    assert!(
        !input.contains("command-name"),
        "neither the open nor the close tag may leak: {input}"
    );
    assert!(
        !input.contains("must not survive"),
        "an unmatched open tag must drop the rest of its turn, exactly like <br8n-context>: {input}"
    );
    let huge = format!("## user (turn 1)\n\n{}\n\n", "a".repeat(50_000));
    assert!(distill_input(&huge).len() <= 12_200);
    assert!(build_prompt("x").contains("NOTHING"));
}

#[test]
fn one_episode_per_finished_session_and_a_rerun_writes_nothing() {
    let (dir, path) = fixture_root();
    let url = common::fake_ollama_generate(
        "- Made Pack::build refuse a mixed set of vectors.\n- Decided against sparse usearch support.",
    );
    let root = dir.path().join("memory");
    let c = cfg(&url);
    let report = distill_pending_at(&root, &c, &roots(dir.path()), 5, false).unwrap();
    assert_eq!(report.candidates, 1);
    assert_eq!(report.distilled, 1);
    let eps = list_at(
        &root,
        &Filter {
            kind: Some(MemoryKind::Episode),
            project: None,
        },
    )
    .unwrap();
    assert_eq!(eps.len(), 1);
    assert!(eps[0].text.contains("mixed set of vectors"));
    assert_eq!(eps[0].facts.project.as_deref(), Some("/Users/x/repo"));
    assert!(eps[0]
        .facts
        .session
        .as_deref()
        .unwrap()
        .ends_with("555555555555.jsonl"));
    assert_eq!(eps[0].id, "11111111-2222-3333-4444-555555555555");
    let again = distill_pending_at(&root, &c, &roots(dir.path()), 5, false).unwrap();
    assert_eq!(again.candidates, 0);
    assert_eq!(again.distilled, 0);
    let _ = path;
}

#[test]
fn a_grown_transcript_replaces_its_episode_in_place() {
    let (dir, path) = fixture_root();
    let url = common::fake_ollama_generate("- First summary.");
    let root = dir.path().join("memory");
    distill_pending_at(&root, &cfg(&url), &roots(dir.path()), 5, false).unwrap();
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    use std::io::Write;
    writeln!(
        f,
        r#"{{"cwd":"/Users/x/repo","timestamp":"2026-09-01T11:00:00Z","message":{{"role":"user","content":"And also add a test please."}}}}"#
    )
    .unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(5 * 3600);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let url2 = common::fake_ollama_generate("- Second summary with the test.");
    let report = distill_pending_at(&root, &cfg(&url2), &roots(dir.path()), 5, false).unwrap();
    assert_eq!(report.distilled, 1);
    let eps = list_at(
        &root,
        &Filter {
            kind: Some(MemoryKind::Episode),
            project: None,
        },
    )
    .unwrap();
    assert_eq!(eps.len(), 1);
    assert!(eps[0].text.contains("Second summary"));
}

#[test]
fn young_transcripts_nothing_answers_and_the_batch_cap_are_respected() {
    let (dir, path) = fixture_root();
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(std::time::SystemTime::now())
        .unwrap();
    let url = common::fake_ollama_generate("- Should not be reached.");
    let root = dir.path().join("memory");
    let report = distill_pending_at(&root, &cfg(&url), &roots(dir.path()), 5, false).unwrap();
    assert_eq!(report.candidates, 0);

    let (dir, _) = fixture_root();
    let url = common::fake_ollama_generate("NOTHING");
    let root = dir.path().join("memory");
    let report = distill_pending_at(&root, &cfg(&url), &roots(dir.path()), 5, false).unwrap();
    assert_eq!(report.candidates, 1);
    assert_eq!(report.distilled, 0);
    assert!(list_at(&root, &Filter::default()).unwrap().is_empty());

    let (dir, path) = fixture_root();
    let second = path.with_file_name("22222222-2222-3333-4444-555555555555.jsonl");
    std::fs::copy(&path, &second).unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(5 * 3600);
    std::fs::File::options()
        .write(true)
        .open(&second)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let url = common::fake_ollama_generate("- A summary.");
    let root = dir.path().join("memory");
    let mut batch_cfg = cfg(&url);
    batch_cfg.memory.duplicate_similarity = 1.5;
    let report = distill_pending_at(&root, &batch_cfg, &roots(dir.path()), 1, false).unwrap();
    assert_eq!(report.candidates, 2);
    assert_eq!(report.distilled, 1);
}

#[test]
fn a_fresh_query_marker_skips_the_automatic_run_and_a_dead_ollama_latches() {
    let (dir, _) = fixture_root();
    let root = dir.path().join("memory");
    let db = dir.path().join("db");
    std::fs::create_dir_all(&db).unwrap();
    let marker = br8n::index::QueryPriority::announce(&db);
    let mut c = cfg("http://127.0.0.1:1");
    c.memory.distill_idle_secs = 60;
    let report =
        br8n::memory::distill::distill_pending_with(&root, &c, &roots(dir.path()), &db, 5, true)
            .unwrap();
    assert!(report.skipped_idle);
    assert_eq!(report.distilled, 0);
    drop(marker);
    let report =
        br8n::memory::distill::distill_pending_with(&root, &c, &roots(dir.path()), &db, 5, true)
            .unwrap();
    assert!(!report.skipped_idle);
    assert!(report.latched.is_some(), "a connection error must latch");
    assert_eq!(report.distilled, 0);
}

#[test]
fn a_matching_source_stamp_skips_reparsing_even_when_the_stored_hash_is_wrong() {
    let (dir, path) = fixture_root();
    let meta = std::fs::metadata(&path).unwrap();
    let secs = meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let stamp = format!("{secs}:{}", meta.len());
    let uri = format!(
        "claude-session://{}",
        path.canonicalize().unwrap().display()
    );
    let mut known = HashMap::new();
    known.insert(
        uri,
        Known {
            source_hash: "not-the-real-hash".into(),
            created: 0,
            source_stamp: Some(stamp),
        },
    );
    let found = candidates(
        &[(
            SessionAgent::ClaudeCode,
            dir.path().join("projects").as_path(),
        )],
        3.0,
        &known,
    )
    .unwrap();
    assert!(
        found.is_empty(),
        "a matching source_stamp must skip load_session before the (wrong) stored hash is ever consulted"
    );
}

#[test]
fn distill_session_targets_one_transcript() {
    let (dir, path) = fixture_root();
    let url = common::fake_ollama_generate("- Just this one.");
    let root = dir.path().join("memory");
    let out = distill_session_at(&root, &cfg(&url), &path).unwrap();
    assert!(matches!(out, Outcome::Saved { .. }), "{out:?}");
    let _ = Path::new("");
}
