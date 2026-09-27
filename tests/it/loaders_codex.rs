use crate::common;

use br8n::config::Config;
use br8n::index::{discover_stat_first_at, Deferral, Discovered, OcrPass};
use br8n::loaders::codex::{is_injected_context, CodexSessionLoader};
use br8n::loaders::transcript::{SessionAgent, SessionRoots};
use br8n::memory::distill::{candidates, distill_input, distill_pending_at};
use br8n::memory::{list_at, Filter, MemoryKind};
use br8n::model::SourceType;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const FIXTURE: &str = "tests/fixtures/codex/sessions/2026/09/20/rollout-2026-09-20T10-15-00-0199aaaa-bbbb-7ccc-8ddd-eeeeffff0001.jsonl";
const SESSION: &str = "0199aaaa-bbbb-7ccc-8ddd-eeeeffff0001";

fn aged(path: &Path, age: Duration) {
    let when = SystemTime::now() - age;
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_accessed(when)
                .set_modified(when),
        )
        .unwrap();
}

struct World {
    dir: tempfile::TempDir,
    roots: SessionRoots,
}

impl World {
    fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::write(notes.join("note.md"), "# Tides\n\nHarbour gauges.\n").unwrap();
        let roots = SessionRoots {
            claude_code: dir.path().join("claude/projects"),
            codex: dir.path().join("codex/sessions"),
        };
        std::fs::create_dir_all(roots.claude_code.join("proj")).unwrap();
        std::fs::create_dir_all(&roots.codex).unwrap();
        World { dir, roots }
    }

    fn config(&self) -> Config {
        Config {
            sources: vec![self.dir.path().join("notes")],
            index_transcripts: true,
            ..Config::default()
        }
    }

    fn codex_session(&self, day: &str, name: &str, age: Duration) -> PathBuf {
        let dir = self.roots.codex.join(day);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::copy(FIXTURE, &p).unwrap();
        aged(&p, age);
        p
    }

    fn claude_session(&self, name: &str, age: Duration) -> PathBuf {
        let p = self.roots.claude_code.join("proj").join(name);
        std::fs::write(
            &p,
            "{\"cwd\":\"/w/proj\",\"timestamp\":\"2026-09-20T09:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"pooling question\"}}\n",
        )
        .unwrap();
        aged(&p, age);
        p
    }

    fn discover(&self, cfg: &Config) -> Discovered {
        discover_stat_first_at(
            cfg,
            &HashMap::new(),
            Deferral::Allowed,
            OcrPass::Skip,
            &self.roots,
        )
        .unwrap()
        .0
    }
}

fn codex_uri(path: &Path) -> String {
    format!("codex-session://{}", path.canonicalize().unwrap().display())
}

fn read_uris(found: &Discovered) -> Vec<String> {
    found.docs.iter().map(|d| d.uri.clone()).collect()
}

#[test]
fn a_rollout_becomes_one_document_holding_only_the_conversation() {
    let doc = CodexSessionLoader::load_session(Path::new(FIXTURE)).unwrap();
    assert_eq!(doc.source_type, SourceType::Transcript);
    assert_eq!(doc.uri, codex_uri(Path::new(FIXTURE)));
    assert_eq!(doc.title, "tidepool codex session (0199aaaa)");
    assert_eq!(doc.meta["project"], "/home/dev/projects/tidepool");
    assert_eq!(doc.meta["agent"], "codex");
    assert_eq!(doc.meta["session_id"], SESSION);
    assert_eq!(doc.meta["started"], "2026-09-20T10:15:00.100Z");

    let t = &doc.text;
    assert_eq!(
        t.matches("Why does the harbour scheduler drop tide readings after midnight?")
            .count(),
        1,
        "the event_msg copy of a prompt must not index it twice: {t}"
    );
    assert_eq!(
        t.matches("switched it to the harbour's local offset")
            .count(),
        1
    );
    assert!(t.contains("Add a regression test for the midnight boundary"));
    assert!(t.contains("feeds a reading at 23:59 local time"));
    assert!(t.starts_with("## user (turn 1)\n\nWhy does the harbour"));
    assert!(t.contains("## assistant (turn 2)"));
    for absent in [
        "SECRETREASONING",
        "gAAAA",
        "AGENTSFILETEXT",
        "environment_context",
        "PERMISSIONSTEXT",
        "You are Codex",
        "UNKNOWNLINETYPE",
        "MALFORMEDLINE",
        "token_count",
        "1200",
    ] {
        assert!(!t.contains(absent), "`{absent}` leaked into the index: {t}");
    }
}

#[test]
fn tool_calls_are_one_line_markers_and_outputs_are_clipped_like_claude_code_sessions() {
    let doc = CodexSessionLoader::load_session(Path::new(FIXTURE)).unwrap();
    let t = &doc.text;
    assert!(
        t.contains("[tool: shell rg -n midnight src/scheduler.rs]"),
        "{t}"
    );
    assert!(t.contains("[result] src/scheduler.rs:42:"), "{t}");
    assert!(t.contains("[tool: apply_patch src/scheduler.rs]"), "{t}");
    assert!(!t.contains("*** Begin Patch"), "{t}");
    let files: Vec<&str> = doc.meta["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(files.contains(&"src/scheduler.rs"), "{files:?}");

    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("rollout-long.jsonl");
    let long = "x".repeat(5000);
    std::fs::write(
        &p,
        format!(
            "{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"run it\"}}]}}}}\n\
             {{\"type\":\"response_item\",\"payload\":{{\"type\":\"function_call_output\",\"call_id\":\"c\",\"output\":\"{long}\"}}}}\n"
        ),
    )
    .unwrap();
    let doc = CodexSessionLoader::load_session(&p).unwrap();
    assert!(doc.text.contains("[result] xxx"));
    assert!(doc.text.contains('…'));
    assert!(doc.text.len() < 1000, "a tool output must be clipped");
}

#[test]
fn distillation_input_from_a_rollout_keeps_both_sides_and_drops_the_tools() {
    let doc = CodexSessionLoader::load_session(Path::new(FIXTURE)).unwrap();
    let input = distill_input(&doc.text);
    assert!(input.contains("User: Why does the harbour scheduler"));
    assert!(input.contains("Assistant: The cutoff used UTC midnight"));
    assert!(!input.contains("[tool:"));
    assert!(!input.contains("[result]"));
}

#[test]
fn a_rollout_with_no_conversation_is_refused_and_legacy_lines_still_load() {
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("rollout-empty.jsonl");
    std::fs::write(
        &empty,
        "{\"timestamp\":\"t\",\"type\":\"session_meta\",\"payload\":{\"id\":\"abc\",\"cwd\":\"/x\"}}\n\
         {\"timestamp\":\"t\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"<environment_context>\\n<cwd>/x</cwd>\\n</environment_context>\"}]}}\n\
         not json at all\n",
    )
    .unwrap();
    assert!(CodexSessionLoader::load_session(&empty).is_err());

    let legacy = dir
        .path()
        .join("rollout-2025-05-07T17-24-21-5973b6c0-94b8-487b-a530-2aeb6098ae0e.jsonl");
    std::fs::write(
        &legacy,
        "{\"id\":\"5973b6c0-94b8-487b-a530-2aeb6098ae0e\",\"timestamp\":\"2025-05-07T17:24:21.123Z\",\"instructions\":null}\n\
         {\"record_type\":\"state\"}\n\
         {\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"rename the lighthouse module\"}]}\n\
         {\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Renamed it to beacon.\"}]}\n",
    )
    .unwrap();
    let doc = CodexSessionLoader::load_session(&legacy).unwrap();
    assert!(doc.text.contains("rename the lighthouse module"));
    assert!(doc.text.contains("Renamed it to beacon."));
    assert_eq!(
        doc.meta["session_id"],
        "5973b6c0-94b8-487b-a530-2aeb6098ae0e"
    );
    assert_eq!(doc.title, "Codex session 5973b6c0");
}

#[test]
fn injected_context_is_recognised_by_its_wrapper_not_its_words() {
    assert!(is_injected_context(
        "<environment_context>\n<cwd>/x</cwd>\n</environment_context>"
    ));
    assert!(is_injected_context(
        "  <user_instructions>be terse</user_instructions>  "
    ));
    assert!(is_injected_context(
        "# AGENTS.md instructions for /x\n\n<INSTRUCTIONS>\nbody\n</INSTRUCTIONS>"
    ));
    assert!(is_injected_context("<external_br8n>ctx</external_br8n>"));
    assert!(!is_injected_context("why does <b>bold</b> render twice?"));
    assert!(!is_injected_context("<div> is the wrong element here"));
    assert!(!is_injected_context("what does environment_context mean?"));
}

#[test]
fn codex_home_follows_codex_home_and_falls_back_to_dot_codex() {
    use br8n::setup::agents::codex_home_from;
    let home = Path::new("/home/someone");
    assert_eq!(
        codex_home_from(Some("/srv/codex".into()), home),
        PathBuf::from("/srv/codex")
    );
    assert_eq!(codex_home_from(Some("".into()), home), home.join(".codex"));
    assert_eq!(codex_home_from(None, home), home.join(".codex"));
}

#[test]
fn session_uris_name_their_agent() {
    assert_eq!(
        SessionAgent::from_uri("codex-session:///a/rollout-x.jsonl"),
        Some(SessionAgent::Codex)
    );
    assert_eq!(
        SessionAgent::from_uri("claude-session:///a/b.jsonl"),
        Some(SessionAgent::ClaudeCode)
    );
    assert_eq!(SessionAgent::from_uri("file:///a/b.md"), None);
    assert_eq!(
        SessionAgent::path_of("codex-session:///a/rollout-x.jsonl"),
        Some(Path::new("/a/rollout-x.jsonl"))
    );
    assert_eq!(SessionAgent::sniff(Path::new(FIXTURE)), SessionAgent::Codex);
    assert_eq!(
        SessionAgent::sniff(Path::new("tests/fixtures/distill/finished.jsonl")),
        SessionAgent::ClaudeCode
    );
}

#[test]
fn a_live_rollout_is_deferred_and_kept_live_and_a_settled_one_is_read() {
    let w = World::new();
    let settled = w.codex_session(
        "2026/09/19",
        "rollout-2026-09-19T08-00-00-0199aaaa-bbbb-7ccc-8ddd-000000000001.jsonl",
        Duration::from_secs(3600),
    );
    let live = w.codex_session(
        "2026/09/20",
        "rollout-2026-09-20T10-15-00-0199aaaa-bbbb-7ccc-8ddd-000000000002.jsonl",
        Duration::from_secs(30),
    );
    let found = w.discover(&w.config());

    assert!(read_uris(&found).contains(&codex_uri(&settled)));
    assert!(!read_uris(&found).contains(&codex_uri(&live)));
    assert_eq!(found.deferred, vec![codex_uri(&live)]);
    assert!(
        found
            .skipped
            .iter()
            .any(|s| s.contains(&live.display().to_string()) && s.contains("still being written")),
        "{:?}",
        found.skipped
    );
    let live_uris = found.live_uris();
    assert!(live_uris.contains(&codex_uri(&live)));
    assert!(live_uris.contains(&codex_uri(&settled)));

    let forced = discover_stat_first_at(
        &w.config(),
        &HashMap::new(),
        Deferral::Forbidden,
        OcrPass::Skip,
        &w.roots,
    )
    .unwrap()
    .0;
    assert!(read_uris(&forced).contains(&codex_uri(&live)));
}

#[test]
fn a_rollout_older_than_the_transcript_age_limit_is_not_indexed() {
    let w = World::new();
    let old = w.codex_session(
        "2026/09/01",
        "rollout-2026-09-01T08-00-00-0199aaaa-bbbb-7ccc-8ddd-000000000003.jsonl",
        Duration::from_secs(3 * 86_400),
    );
    let recent = w.codex_session(
        "2026/09/19",
        "rollout-2026-09-19T08-00-00-0199aaaa-bbbb-7ccc-8ddd-000000000004.jsonl",
        Duration::from_secs(3600),
    );
    let cfg = Config {
        index_transcripts_max_age_days: Some(1),
        ..w.config()
    };
    let found = w.discover(&cfg);
    assert!(read_uris(&found).contains(&codex_uri(&recent)));
    assert!(!found.live_uris().contains(&codex_uri(&old)));
}

#[test]
fn codex_sessions_can_be_declined_alone_and_index_transcripts_declines_both() {
    let w = World::new();
    let codex = w.codex_session(
        "2026/09/19",
        "rollout-2026-09-19T08-00-00-0199aaaa-bbbb-7ccc-8ddd-000000000005.jsonl",
        Duration::from_secs(3600),
    );
    let claude = w.claude_session("s.jsonl", Duration::from_secs(3600));
    let claude_uri = format!(
        "claude-session://{}",
        claude.canonicalize().unwrap().display()
    );

    let both = w.discover(&w.config());
    assert!(read_uris(&both).contains(&codex_uri(&codex)));
    assert!(read_uris(&both).contains(&claude_uri));

    let no_codex = w.discover(&Config {
        index_codex_sessions: false,
        ..w.config()
    });
    assert!(!no_codex
        .live_uris()
        .iter()
        .any(|u| u.starts_with("codex-session://")));
    assert!(read_uris(&no_codex).contains(&claude_uri));

    let none = w.discover(&Config {
        index_transcripts: false,
        ..w.config()
    });
    assert!(!none.live_uris().iter().any(|u| u.contains("-session://")));
}

#[test]
fn a_finished_rollout_is_distilled_into_an_episode_keyed_by_its_session() {
    let w = World::new();
    let path = w.codex_session(
        "2026/09/20",
        "rollout-2026-09-20T10-15-00-0199aaaa-bbbb-7ccc-8ddd-eeeeffff0001.jsonl",
        Duration::from_secs(5 * 3600),
    );
    let found = candidates(
        &[(SessionAgent::Codex, w.roots.codex.as_path())],
        3.0,
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].1.uri, codex_uri(&path));

    let url = common::fake_ollama_generate("- Moved the tide cutoff to local midnight.");
    let mut cfg = w.config();
    cfg.embed.ollama_url = url;
    cfg.embed.dimensions = 512;
    let memory = w.dir.path().join("memory");
    let report = distill_pending_at(&memory, &cfg, &w.roots, 5, false).unwrap();
    assert_eq!(report.distilled, 1);
    let eps = list_at(
        &memory,
        &Filter {
            kind: Some(MemoryKind::Episode),
            project: None,
        },
    )
    .unwrap();
    assert_eq!(eps.len(), 1);
    assert_eq!(eps[0].id, SESSION);
    assert_eq!(
        eps[0].facts.session.as_deref(),
        Some(codex_uri(&path).as_str())
    );
    assert_eq!(
        eps[0].facts.project.as_deref(),
        Some("/home/dev/projects/tidepool")
    );

    let declined = Config {
        index_codex_sessions: false,
        ..cfg
    };
    let again =
        distill_pending_at(&w.dir.path().join("memory2"), &declined, &w.roots, 5, false).unwrap();
    assert_eq!(again.candidates, 0);
}

#[test]
fn the_graph_names_the_agent_a_session_came_from() {
    let dir = tempfile::tempdir().unwrap();
    let store = br8n::store::Store::open(dir.path(), 4).unwrap();
    let idx = br8n::index::Indexer::new(
        store,
        Box::new(common::FakeEmbedder {
            calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            queries: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }),
        Config::default(),
    );
    let doc = |uri: &str, st: SourceType| br8n::model::Document::new(st, uri, uri, "text body");
    idx.index_documents(&[
        doc("codex-session:///s/rollout-a.jsonl", SourceType::Transcript),
        doc("claude-session:///p/b.jsonl", SourceType::Transcript),
        doc("file:///n/c.md", SourceType::Markdown),
    ])
    .unwrap();
    let snap = idx.store().graph_snapshot().unwrap();
    let agent_of = |title: &str| snap.nodes.iter().find(|n| n.title == title).unwrap().agent;
    assert_eq!(
        agent_of("codex-session:///s/rollout-a.jsonl"),
        Some("codex")
    );
    assert_eq!(agent_of("claude-session:///p/b.jsonl"), Some("claude-code"));
    assert_eq!(agent_of("file:///n/c.md"), None);
    let json = serde_json::to_value(&snap).unwrap();
    assert!(json["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|n| n["source_type"] != "markdown" || n.get("agent").is_none()));

    let counts = idx.store().counts_sessions_by_agent().unwrap();
    assert_eq!(counts.get("codex"), Some(&1));
    assert_eq!(counts.get("claude-code"), Some(&1));

    let pruned = idx
        .prune_missing(&["claude-session:///p/b.jsonl".to_string()])
        .unwrap();
    assert_eq!(
        pruned, 1,
        "a codex session discovery no longer lists is pruned"
    );
    assert_eq!(
        idx.store().counts_sessions_by_agent().unwrap().get("codex"),
        Some(&0)
    );
}
