use crate::common;

use br8n::config::{Config, MemoryConfig, Weights};
use br8n::memory::{
    edit_at, export_at, forget_at, import_at, lessons_block_at, list_at, memory_id, rebuild_at,
    remember_at, render_lessons, summary_title, uri_for, Filter, Memory, MemoryAmbiguous,
    MemoryFacts, MemoryKind, MemoryNotFound, Origin, Outcome, Remember, MAX_TEXT_CHARS,
};
use br8n::model::SourceType;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

#[test]
fn memory_is_a_source_type_with_its_own_weight() {
    assert_eq!(SourceType::Memory.as_str(), "memory");
    let w = Weights {
        memory: 0.5,
        ..Config::default().weights
    };
    assert_eq!(
        w.for_source("memory"),
        0.5,
        "the memory arm must not fall through to markdown"
    );
    assert_eq!(w.for_source("markdown"), 1.0);
}

#[test]
fn a_partial_memory_table_keeps_every_other_default() {
    let c: Config = toml::from_str("[memory]\nlessons_max_tokens = 100\n").unwrap();
    let d = MemoryConfig::default();
    assert_eq!(c.memory.lessons_max_tokens, 100);
    assert_eq!(c.memory.min_confidence, d.min_confidence);
    assert_eq!(c.memory.duplicate_similarity, d.duplicate_similarity);
    assert_eq!(c.memory.episode_half_life_days, d.episode_half_life_days);
    assert_eq!(c.memory.episode_decay_floor, d.episode_decay_floor);
    assert_eq!(c.memory.distill_model, d.distill_model);
    assert_eq!(c.memory.max_memories, d.max_memories);
    assert!(c.memory.enabled);
    assert!(c.memory.distill_episodes);
}

#[test]
fn a_hook_memory_override_leaves_its_siblings_alone() {
    let c: Config =
        toml::from_str("[weights]\nweb = 0.8\n\n[hook.weights]\nmemory = 0.5\n").unwrap();
    let w = c.weights_for(br8n::config::Surface::Hook);
    assert_eq!(w.memory, 0.5);
    assert_eq!(w.web, 0.8);
    assert_eq!(c.weights_for(br8n::config::Surface::Mcp).memory, 1.0);
}

#[test]
fn episode_decay_is_one_at_zero_age_and_halves_toward_the_floor() {
    let m = MemoryConfig {
        episode_half_life_days: 30.0,
        episode_decay_floor: 0.8,
        ..MemoryConfig::default()
    };
    assert!((m.episode_decay(0.0) - 1.0).abs() < 1e-6);
    assert!((m.episode_decay(30.0) - 0.9).abs() < 1e-6);
    assert!((m.episode_decay(3000.0) - 0.8).abs() < 1e-4);
    let flat = MemoryConfig {
        episode_decay_floor: 1.0,
        ..MemoryConfig::default()
    };
    assert_eq!(flat.episode_decay(400.0), 1.0);
}

#[test]
fn ids_are_content_addressed_and_uris_name_the_kind() {
    let a = memory_id(MemoryKind::Lesson, "never comment code");
    let b = memory_id(MemoryKind::Lesson, "never comment code");
    let c = memory_id(MemoryKind::Fact, "never comment code");
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_eq!(a.len(), 12);
    assert_eq!(uri_for(MemoryKind::Fact, &c), format!("memory://fact/{c}"));
}

#[test]
fn the_shipped_memory_defaults_are_the_documented_numbers() {
    let m = MemoryConfig::default();
    assert!(m.enabled);
    assert_eq!(m.lessons_max_tokens, 600);
    assert_eq!(m.min_confidence, 80);
    assert_eq!(m.duplicate_similarity, 0.94);
    assert_eq!(m.episode_half_life_days, 30.0);
    assert_eq!(m.episode_decay_floor, 0.85);
    assert!(m.distill_episodes);
    assert_eq!(m.distill_after_hours, 3.0);
    assert_eq!(m.distill_idle_secs, 60);
    assert_eq!(m.distill_model, "qwen3:4b");
    assert_eq!(m.max_memories, 10_000);
    assert_eq!(Config::default().weights.memory, 1.0);
}

#[test]
fn facts_round_trip_through_json_with_lowercase_kinds() {
    let f = MemoryFacts {
        kind: MemoryKind::Episode,
        created: 1_757_000_000,
        project: Some("/Users/x/repo".into()),
        origin: Origin::Distill,
        confidence: 70,
        session: Some("claude-session:///x.jsonl".into()),
        source_hash: Some("abc".into()),
        source_stamp: None,
    };
    let j = serde_json::to_string(&f).unwrap();
    assert!(j.contains("\"kind\":\"episode\""));
    assert!(j.contains("\"origin\":\"distill\""));
    let back: MemoryFacts = serde_json::from_str(&j).unwrap();
    assert_eq!(back, f);
    assert_eq!(MemoryKind::parse("lesson"), Some(MemoryKind::Lesson));
    assert_eq!(MemoryKind::parse("nope"), None);
}

#[test]
fn all_rows_for_pack_carries_memory_facts_only_for_memory_documents() {
    use br8n::index::Indexer;
    use br8n::model::Document;
    use br8n::store::Store;
    let dir = tempfile::tempdir().unwrap();
    {
        let store = Store::open(dir.path(), 4).unwrap();
        let idx = Indexer::new(
            store,
            Box::new(common::FakeEmbedder::default()),
            Config::default(),
        );
        let mut m = Document::new(
            SourceType::Memory,
            "memory://fact/abc",
            "The vault",
            "The vault lives at ~/notes.",
        );
        m.meta = serde_json::json!({ "memory": {
            "kind": "fact", "created": 1_757_000_000, "origin": "user", "confidence": 100,
            "text": "The vault lives at ~/notes."
        }});
        let n = Document::new(
            SourceType::Markdown,
            "file:///n.md",
            "Note",
            "A plain note.",
        );
        idx.index_documents(&[m, n]).unwrap();
    }
    let store = Store::open(dir.path(), 4).unwrap();
    let rows = store.all_rows_for_pack().unwrap();
    let mem = rows
        .iter()
        .find(|(r, _)| r.source_type == "memory")
        .unwrap();
    let note = rows
        .iter()
        .find(|(r, _)| r.source_type == "markdown")
        .unwrap();
    assert_eq!(
        mem.0.memory.as_ref().map(|f| f.kind),
        Some(MemoryKind::Fact)
    );
    assert_eq!(mem.0.memory.as_ref().map(|f| f.confidence), Some(100));
    assert!(note.0.memory.is_none());
    let metas = store.all_documents_meta().unwrap();
    assert!(metas
        .iter()
        .any(|(uri, _, meta)| uri == "memory://fact/abc" && meta.contains("\"text\"")));
}

fn fake() -> Box<dyn br8n::embed::Embedder> {
    Box::new(common::FakeEmbedder::default())
}

fn fake_embedders() -> anyhow::Result<Box<dyn br8n::embed::Embedder>> {
    Ok(fake())
}

fn lesson(text: &str) -> Remember {
    Remember {
        kind: MemoryKind::Lesson,
        text: text.into(),
        title: None,
        project: None,
        confidence: 100,
        origin: Origin::User,
        session: None,
        source_hash: None,
        source_stamp: None,
        created: None,
    }
}

fn cfg4() -> Config {
    let mut c = Config::default();
    c.embed.dimensions = 4;
    c
}

fn no_dedup_cfg() -> Config {
    let mut c = cfg4();
    c.memory.duplicate_similarity = 1.1;
    c
}

#[test]
fn a_saved_memory_is_in_the_pack_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let out = remember_at(
        &root,
        &cfg4(),
        fake(),
        lesson("Never comment code unless asked."),
    )
    .unwrap();
    let Outcome::Saved { id } = out else {
        panic!("{out:?}")
    };
    assert_eq!(id.len(), 12);
    let all = list_at(&root, &Filter::default()).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, id);
    assert_eq!(all[0].facts.kind, MemoryKind::Lesson);
    assert_eq!(all[0].text, "Never comment code unless asked.");
    assert!(br8n::memory::pack_dir(&root).join("pack.manifest").exists());
}

#[test]
fn saving_the_same_text_twice_is_a_duplicate_not_a_second_row() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    remember_at(
        &root,
        &cfg4(),
        fake(),
        lesson("Never comment code unless asked."),
    )
    .unwrap();
    let again = remember_at(
        &root,
        &cfg4(),
        fake(),
        lesson("Never comment code unless asked."),
    )
    .unwrap();
    assert!(matches!(again, Outcome::Duplicate { .. }), "{again:?}");
    assert_eq!(list_at(&root, &Filter::default()).unwrap().len(), 1);
}

#[test]
fn a_near_duplicate_lesson_replaces_its_predecessor() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let mut c = cfg4();
    c.memory.duplicate_similarity = 0.0;
    remember_at(
        &root,
        &c,
        fake(),
        lesson("Use tabs for indentation in this repo."),
    )
    .unwrap();
    let out = remember_at(
        &root,
        &c,
        fake(),
        lesson("Use spaces for indentation in this repo."),
    )
    .unwrap();
    let Outcome::Replaced { previous_title, .. } = &out else {
        panic!("expected Replaced, got {out:?}")
    };
    assert!(
        previous_title.contains("tabs"),
        "the outcome must carry the superseded memory's title, got {previous_title:?}"
    );
    let all = list_at(&root, &Filter::default()).unwrap();
    assert_eq!(all.len(), 1);
    assert!(all[0].text.contains("spaces"));

    struct FailsToIndex;
    impl br8n::embed::Embedder for FailsToIndex {
        fn embed_documents(&self, _: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
            anyhow::bail!("ollama down")
        }
        fn embed_query(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(common::hash_vec(text))
        }
        fn warm(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn model_id(&self) -> String {
            "fake@4".into()
        }
        fn dimensions(&self) -> usize {
            4
        }
    }
    let err = remember_at(
        &root,
        &c,
        Box::new(FailsToIndex),
        lesson("Use spaces consistently for indentation everywhere."),
    )
    .unwrap_err();
    assert!(err.to_string().contains("ollama down"), "{err:#}");
    let store = br8n::store::Store::open(&br8n::memory::store_dir(&root), 4).unwrap();
    assert_eq!(store.count_documents().unwrap(), 1);
    let all = list_at(&root, &Filter::default()).unwrap();
    assert_eq!(all.len(), 1);
    assert!(all[0].text.contains("spaces"));
}

#[test]
fn a_near_duplicate_episode_is_rejected_unless_it_is_the_same_session() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let mut c = cfg4();
    c.memory.duplicate_similarity = 0.0;
    let ep = |text: &str, session: &str| Remember {
        kind: MemoryKind::Episode,
        text: text.into(),
        title: Some("br8n session".into()),
        project: None,
        confidence: 70,
        origin: Origin::Distill,
        session: Some(session.into()),
        source_hash: Some("h1".into()),
        source_stamp: None,
        created: None,
    };
    remember_at(
        &root,
        &c,
        fake(),
        ep("Worked on the pack format.", "claude-session:///a.jsonl"),
    )
    .unwrap();
    let other = remember_at(
        &root,
        &c,
        fake(),
        ep("Worked on the pack layout.", "claude-session:///b.jsonl"),
    )
    .unwrap();
    assert!(matches!(other, Outcome::Duplicate { .. }), "{other:?}");
    let same = remember_at(
        &root,
        &c,
        fake(),
        ep(
            "Worked on the pack format and tests.",
            "claude-session:///a.jsonl",
        ),
    )
    .unwrap();
    assert!(matches!(same, Outcome::Saved { .. }), "{same:?}");
    assert_eq!(list_at(&root, &Filter::default()).unwrap().len(), 1);
}

#[test]
fn bounds_and_the_confidence_gate_reject_before_touching_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let short = remember_at(&root, &cfg4(), fake(), lesson("tiny")).unwrap();
    assert!(matches!(short, Outcome::Rejected(_)));
    let long = remember_at(&root, &cfg4(), fake(), lesson(&"x".repeat(2001))).unwrap();
    assert!(matches!(long, Outcome::Rejected(_)));
    let mut low = lesson("Claude thinks the user prefers rebase.");
    low.origin = Origin::Claude;
    low.confidence = 50;
    assert!(matches!(
        remember_at(&root, &cfg4(), fake(), low).unwrap(),
        Outcome::Rejected(_)
    ));
    assert!(
        !br8n::memory::store_dir(&root).exists(),
        "a rejected write must not create the store"
    );
}

#[test]
fn an_embedder_failure_leaves_store_and_pack_untouched() {
    struct Broken;
    impl br8n::embed::Embedder for Broken {
        fn embed_documents(&self, _: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
            anyhow::bail!("ollama down")
        }
        fn embed_query(&self, _: &str) -> anyhow::Result<Vec<f32>> {
            anyhow::bail!("ollama down")
        }
        fn warm(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn model_id(&self) -> String {
            "fake@4".into()
        }
        fn dimensions(&self) -> usize {
            4
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    remember_at(
        &root,
        &cfg4(),
        fake(),
        lesson("Never comment code unless asked."),
    )
    .unwrap();
    let manifest_before =
        std::fs::read(br8n::memory::pack_dir(&root).join("pack.manifest")).unwrap();
    let err = remember_at(
        &root,
        &cfg4(),
        Box::new(Broken),
        lesson("Always squash before a PR."),
    )
    .unwrap_err();
    assert!(err.to_string().contains("ollama down"), "{err:#}");
    assert_eq!(list_at(&root, &Filter::default()).unwrap().len(), 1);
    assert_eq!(
        std::fs::read(br8n::memory::pack_dir(&root).join("pack.manifest")).unwrap(),
        manifest_before
    );
}

#[test]
fn forget_removes_the_memory_and_republishes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let c = no_dedup_cfg();
    let Outcome::Saved { id } = remember_at(
        &root,
        &c,
        fake(),
        lesson("Never comment code unless asked."),
    )
    .unwrap() else {
        panic!()
    };
    remember_at(
        &root,
        &c,
        fake(),
        lesson("Always squash before opening a PR."),
    )
    .unwrap();
    let gone = forget_at(&root, &c, &id).unwrap();
    assert_eq!(gone.id, id);
    let left = list_at(&root, &Filter::default()).unwrap();
    assert_eq!(left.len(), 1);
    assert!(left[0].text.contains("squash"));
    assert!(forget_at(&root, &c, "000000000000").is_err());
}

#[test]
fn forgetting_the_last_memory_removes_the_pack_entirely() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let Outcome::Saved { id } = remember_at(
        &root,
        &cfg4(),
        fake(),
        lesson("Never comment code unless asked."),
    )
    .unwrap() else {
        panic!()
    };
    forget_at(&root, &cfg4(), &id).unwrap();
    assert!(!br8n::memory::pack_dir(&root).exists());
    assert!(list_at(&root, &Filter::default()).unwrap().is_empty());
}

#[test]
fn list_filters_by_kind_and_project() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let c = no_dedup_cfg();
    let mut scoped = lesson("In this repo, run cargo fmt before every commit.");
    scoped.project = Some("/Users/x/repo".into());
    remember_at(&root, &c, fake(), scoped).unwrap();
    remember_at(
        &root,
        &c,
        fake(),
        lesson("Always squash before opening a PR."),
    )
    .unwrap();
    let mut fact = lesson("The notes vault lives at ~/notes/vault.");
    fact.kind = MemoryKind::Fact;
    remember_at(&root, &c, fake(), fact).unwrap();
    assert_eq!(
        list_at(
            &root,
            &Filter {
                kind: Some(MemoryKind::Lesson),
                project: None
            }
        )
        .unwrap()
        .len(),
        2
    );
    assert_eq!(
        list_at(
            &root,
            &Filter {
                kind: None,
                project: Some("/Users/x/repo".into())
            }
        )
        .unwrap()
        .len(),
        1
    );
    assert_eq!(
        list_at(
            &root,
            &Filter {
                kind: Some(MemoryKind::Fact),
                project: None
            }
        )
        .unwrap()[0]
            .facts
            .kind,
        MemoryKind::Fact
    );
}

#[test]
fn a_replace_at_the_cap_evicts_no_extra_episode() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let mut cap_cfg = no_dedup_cfg();
    cap_cfg.memory.max_memories = 3;

    let ep = |text: &str, session: &str, created: i64| Remember {
        kind: MemoryKind::Episode,
        text: text.into(),
        title: Some("br8n session".into()),
        project: None,
        confidence: 70,
        origin: Origin::Distill,
        session: Some(session.into()),
        source_hash: Some("h1".into()),
        source_stamp: None,
        created: Some(created),
    };

    remember_at(
        &root,
        &cap_cfg,
        fake(),
        ep("First session note.", "claude-session:///e1.jsonl", 100),
    )
    .unwrap();
    remember_at(
        &root,
        &cap_cfg,
        fake(),
        ep("Second session note.", "claude-session:///e2.jsonl", 200),
    )
    .unwrap();
    remember_at(
        &root,
        &cap_cfg,
        fake(),
        ep("Third session note.", "claude-session:///e3.jsonl", 300),
    )
    .unwrap();
    assert_eq!(list_at(&root, &Filter::default()).unwrap().len(), 3);

    let fresh = remember_at(
        &root,
        &cap_cfg,
        fake(),
        lesson("Use tabs for indentation in this repo."),
    )
    .unwrap();
    assert!(matches!(fresh, Outcome::Saved { .. }), "{fresh:?}");
    let after_fresh = list_at(&root, &Filter::default()).unwrap();
    assert_eq!(
        after_fresh.len(),
        3,
        "a fresh save at the cap must evict exactly one episode"
    );
    assert!(!after_fresh.iter().any(|m| m.id == "e1"));
    assert!(after_fresh.iter().any(|m| m.id == "e2"));

    let mut replace_cfg = cap_cfg.clone();
    replace_cfg.memory.duplicate_similarity = 0.0;
    let replaced = remember_at(
        &root,
        &replace_cfg,
        fake(),
        lesson("Use spaces for indentation in this repo."),
    )
    .unwrap();
    assert!(matches!(replaced, Outcome::Replaced { .. }), "{replaced:?}");

    let after_replace = list_at(&root, &Filter::default()).unwrap();
    assert_eq!(
        after_replace.len(),
        3,
        "a replace at the cap is net zero and must not evict an extra episode"
    );
    assert!(
        after_replace.iter().any(|m| m.id == "e2"),
        "the oldest surviving episode must not be evicted by a net-zero replace"
    );
}

fn mem(id: &str, text: &str, project: Option<&str>, created: i64) -> Memory {
    Memory {
        id: id.into(),
        uri: format!("memory://lesson/{id}"),
        doc_id: format!("d{id}"),
        title: text.into(),
        text: text.into(),
        facts: MemoryFacts {
            kind: MemoryKind::Lesson,
            created,
            project: project.map(String::from),
            origin: Origin::User,
            confidence: 100,
            session: None,
            source_hash: None,
            source_stamp: None,
        },
    }
}

#[test]
fn the_lessons_block_is_plain_text_with_ids_dates_and_scope() {
    let block = render_lessons(
        &[mem(
            "abc123abc123",
            "Never comment code unless asked.",
            Some("/Users/x/repo"),
            1_756_684_800,
        )],
        600,
    )
    .unwrap();
    assert!(block.starts_with("<br8n-lessons>"));
    assert!(block.trim_end().ends_with("</br8n-lessons>"));
    assert!(block
        .contains("[abc123abc123 · 2025-09-01 · /Users/x/repo] Never comment code unless asked."));
    assert!(block.contains("br8n_forget"));
    assert!(
        !block.trim_start().starts_with('{'),
        "must not be JSON: other session-start output may precede it on stdout"
    );
    assert!(render_lessons(&[], 600).is_none());
}

#[test]
fn the_token_cap_drops_the_oldest_lessons_and_says_so() {
    let many: Vec<Memory> = (0..40)
        .map(|i| {
            mem(
                &format!("{i:012}"),
                &format!("Lesson number {i} is {}", "long ".repeat(20)),
                None,
                1_000_000 + i,
            )
        })
        .collect();
    let block = render_lessons(&many, 100).unwrap();
    assert!(
        block.len() <= 100 * 4 + 200,
        "block is {} chars",
        block.len()
    );
    assert!(
        block.contains("Lesson number 39"),
        "the newest lesson survives"
    );
    assert!(
        !block.contains("Lesson number 0 "),
        "the oldest lesson is dropped"
    );
    assert!(block.contains("older lessons omitted"));
}

#[test]
fn lessons_are_filtered_by_cwd_and_ordered_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let c = no_dedup_cfg();
    let mut scoped = lesson("In this repo, run cargo fmt before every commit.");
    scoped.project = Some("/Users/x/repo".into());
    remember_at(&root, &c, fake(), scoped).unwrap();
    let mut elsewhere = lesson("In the other repo, use pnpm.");
    elsewhere.project = Some("/Users/x/other".into());
    remember_at(&root, &c, fake(), elsewhere).unwrap();
    remember_at(
        &root,
        &c,
        fake(),
        lesson("Always squash before opening a PR."),
    )
    .unwrap();
    let block = lessons_block_at(
        &root,
        &MemoryConfig::default(),
        Some(std::path::Path::new("/Users/x/repo/src")),
    )
    .unwrap()
    .unwrap();
    assert!(block.contains("cargo fmt"));
    assert!(block.contains("squash"));
    assert!(!block.contains("pnpm"));
    let none = lessons_block_at(
        &root,
        &MemoryConfig::default(),
        Some(std::path::Path::new("/tmp/elsewhere")),
    )
    .unwrap()
    .unwrap();
    assert!(none.contains("squash") && !none.contains("cargo fmt"));
    assert!(
        lessons_block_at(&dir.path().join("nothing"), &MemoryConfig::default(), None)
            .unwrap()
            .is_none()
    );
}

#[test]
fn export_import_round_trip_keeps_kind_scope_and_text() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let mut scoped = lesson("In this repo, run cargo fmt before every commit.");
    scoped.project = Some("/Users/x/repo".into());
    remember_at(&root, &cfg4(), fake(), scoped).unwrap();
    let mut fact = lesson("The notes vault lives at ~/notes/vault.");
    fact.kind = MemoryKind::Fact;
    fact.source_stamp = Some("1700000000:4096".into());
    remember_at(&root, &cfg4(), fake(), fact).unwrap();
    let rows = export_at(&root, 4).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .any(|r| r.source_stamp.as_deref() == Some("1700000000:4096")));
    let other = dir.path().join("other");
    let (saved, skipped) = import_at(&other, &cfg4(), &fake_embedders, &rows).unwrap();
    assert_eq!((saved, skipped), (2, 0));
    let again = import_at(&other, &cfg4(), &fake_embedders, &rows).unwrap();
    assert_eq!(again, (0, 2));
    let listed = list_at(&other, &Filter::default()).unwrap();
    assert!(listed
        .iter()
        .any(|m| m.facts.kind == MemoryKind::Fact && m.text.contains("vault")));
    assert!(listed
        .iter()
        .any(|m| m.facts.project.as_deref() == Some("/Users/x/repo")));
    assert!(listed
        .iter()
        .any(|m| m.facts.source_stamp.as_deref() == Some("1700000000:4096")));
}

#[test]
fn rebuild_re_embeds_every_memory_and_republishes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let setup_cfg = no_dedup_cfg();
    remember_at(
        &root,
        &setup_cfg,
        fake(),
        lesson("Never comment code unless asked."),
    )
    .unwrap();
    remember_at(
        &root,
        &setup_cfg,
        fake(),
        lesson("Always squash before opening a PR."),
    )
    .unwrap();
    let manifest_path = br8n::memory::pack_dir(&root).join("pack.manifest");
    let before = std::fs::read(&manifest_path).unwrap();
    let before_mtime = std::fs::metadata(&manifest_path)
        .unwrap()
        .modified()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let counter = Arc::new(AtomicUsize::new(0));
    let counting = || -> anyhow::Result<Box<dyn br8n::embed::Embedder>> {
        Ok(Box::new(common::FakeEmbedder {
            calls: counter.clone(),
            ..Default::default()
        }))
    };
    let n = rebuild_at(&root, &cfg4(), &counting).unwrap();
    assert_eq!(n, 2);
    assert!(
        counter.load(std::sync::atomic::Ordering::SeqCst) >= 2,
        "rebuild must embed again"
    );
    assert_eq!(list_at(&root, &Filter::default()).unwrap().len(), 2);
    assert!(!root.join("fresh").exists());
    let after = std::fs::read(&manifest_path).unwrap();
    assert_eq!(
        before, after,
        "same model, same rows: the republished manifest matches"
    );
    let after_mtime = std::fs::metadata(&manifest_path)
        .unwrap()
        .modified()
        .unwrap();
    assert!(
        after_mtime > before_mtime,
        "the pack must actually be republished, not left in place"
    );
}

#[test]
fn rebuild_on_an_empty_memory_store_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let n = rebuild_at(&root, &cfg4(), &fake_embedders).unwrap();
    assert_eq!(n, 0);
    assert!(!root.join("fresh").exists());
    assert!(!root.join("db").exists());
    assert!(!root.join("pack").exists());
}

#[test]
fn a_rebuild_that_would_drop_memories_refuses_and_leaves_the_store_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let low_bar = no_dedup_cfg();

    let mut user_fact = lesson("The vault lives at ~/notes/vault and nowhere else.");
    user_fact.kind = MemoryKind::Fact;
    user_fact.confidence = 100;
    user_fact.origin = Origin::User;
    remember_at(&root, &low_bar, fake(), user_fact).unwrap();

    let mut claude_lesson = lesson("Squash the branch before opening a pull request, always.");
    claude_lesson.confidence = 80;
    claude_lesson.origin = Origin::Claude;
    remember_at(&root, &low_bar, fake(), claude_lesson).unwrap();

    let before = list_at(&root, &Filter::default()).unwrap();
    assert_eq!(before.len(), 2);

    let mut strict = no_dedup_cfg();
    strict.memory.min_confidence = 90;

    let err = rebuild_at(&root, &strict, &fake_embedders).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("untouched"),
        "the error must say the live store is untouched; got: {msg}"
    );

    let after = list_at(&root, &Filter::default()).unwrap();
    assert_eq!(
        after, before,
        "a refused rebuild must not change the live store"
    );
}

#[test]
fn a_long_memory_stays_one_chunk_even_with_a_small_configured_chunk_size() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let mut c = no_dedup_cfg();
    c.embed.chunk_tokens = 256;

    let sentence = "The vault lives at ~/notes/vault and nowhere else, and this memory \
                     repeats itself on purpose so a small chunk_tokens would split it. ";
    let mut raw = String::new();
    while raw.chars().count() < MAX_TEXT_CHARS {
        raw.push_str(sentence);
    }
    let raw: String = raw.chars().take(MAX_TEXT_CHARS).collect();
    let expected = raw.trim().to_string();

    let out = remember_at(&root, &c, fake(), lesson(&raw)).unwrap();
    let Outcome::Saved { id } = out else {
        panic!("expected Saved, got {out:?}")
    };

    let all = list_at(&root, &Filter::default()).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, id);
    assert_eq!(
        all[0].text, expected,
        "a memory must stay one chunk regardless of [embed] chunk_tokens"
    );
}

#[test]
fn editing_text_gives_a_new_id_and_keeps_the_original_date() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let mut first = lesson("Never comment code unless asked.");
    first.created = Some(1_700_000_000);
    let Outcome::Saved { id } = remember_at(&root, &cfg4(), fake(), first).unwrap() else {
        panic!("the fixture must save")
    };
    let mut edited = lesson("Never comment code unless the user asks for it.");
    edited.created = None;
    let out = edit_at(&root, &cfg4(), fake(), &id, edited).unwrap();
    let Outcome::Replaced {
        id: new_id,
        previous,
        ..
    } = out
    else {
        panic!("an edit reports a replacement, got {out:?}")
    };
    assert_eq!(previous, id);
    assert_ne!(
        new_id, id,
        "editing the text must change the content-addressed id"
    );
    let all = list_at(&root, &Filter::default()).unwrap();
    assert_eq!(
        all.len(),
        1,
        "the original must be gone, not left beside the edit"
    );
    assert_eq!(all[0].id, new_id);
    assert!(all[0].text.contains("the user asks"));
    assert_eq!(
        all[0].facts.created, 1_700_000_000,
        "the created date carries forward so ordering and decay do not reset"
    );
}

#[test]
fn an_edit_deletes_only_the_memory_it_names() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let c = no_dedup_cfg();
    let Outcome::Saved { id } = remember_at(
        &root,
        &c,
        fake(),
        lesson("Always squash before opening a PR."),
    )
    .unwrap() else {
        panic!()
    };
    remember_at(
        &root,
        &c,
        fake(),
        lesson("Never comment code unless asked."),
    )
    .unwrap();
    remember_at(
        &root,
        &c,
        fake(),
        lesson("Run cargo fmt before every commit."),
    )
    .unwrap();
    edit_at(
        &root,
        &c,
        fake(),
        &id,
        lesson("Always squash a branch before opening a PR."),
    )
    .unwrap();
    let all = list_at(&root, &Filter::default()).unwrap();
    assert_eq!(all.len(), 3, "an edit is not a delete of anything else");
    assert!(all.iter().any(|m| m.text.contains("squash a branch")));
    assert!(all.iter().any(|m| m.text.contains("Never comment")));
    assert!(all.iter().any(|m| m.text.contains("cargo fmt")));
}

#[test]
fn editing_into_another_memorys_text_refuses_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let c = no_dedup_cfg();

    let Outcome::Saved { id: id_a } = remember_at(
        &root,
        &c,
        fake(),
        lesson("Always squash before opening a PR."),
    )
    .unwrap() else {
        panic!()
    };
    let mut b = lesson("Never comment code unless asked.");
    b.confidence = 90;
    let Outcome::Saved { id: id_b } = remember_at(&root, &c, fake(), b).unwrap() else {
        panic!()
    };

    let mut edit = lesson("Never comment code unless asked.");
    edit.confidence = 55;
    let out = edit_at(&root, &c, fake(), &id_a, edit).unwrap();
    let Outcome::Duplicate { of } = out else {
        panic!(
            "an edit that collides with another memory must refuse instead of replacing, got \
             {out:?}"
        )
    };
    assert_eq!(
        of, id_b,
        "an edit that collides with another memory must name the memory that already holds \
         this text"
    );

    let all = list_at(&root, &Filter::default()).unwrap();
    assert_eq!(
        all.len(),
        2,
        "an edit that collides with another memory must change nothing: both memories must \
         survive"
    );
    let a = all.iter().find(|m| m.id == id_a).expect(
        "an edit that collides with another memory must change nothing: A must still exist",
    );
    assert_eq!(
        a.text, "Always squash before opening a PR.",
        "an edit that collides with another memory must change nothing: A's text must be \
         unchanged"
    );
    let b = all.iter().find(|m| m.id == id_b).expect(
        "an edit that collides with another memory must change nothing: B must still exist",
    );
    assert_eq!(
        b.text, "Never comment code unless asked.",
        "an edit that collides with another memory must change nothing: B's text must be \
         unchanged"
    );
    assert_eq!(
        b.facts.confidence, 90,
        "an edit that collides with another memory must change nothing: B's facts must be \
         unchanged"
    );
}

#[test]
fn editing_only_the_confidence_keeps_the_same_id() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let Outcome::Saved { id } = remember_at(
        &root,
        &cfg4(),
        fake(),
        lesson("Never comment code unless asked."),
    )
    .unwrap() else {
        panic!()
    };
    let mut same = lesson("Never comment code unless asked.");
    same.confidence = 60;
    edit_at(&root, &cfg4(), fake(), &id, same).unwrap();
    let all = list_at(&root, &Filter::default()).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(
        all[0].id, id,
        "the id hashes kind and text, so unchanged text keeps it"
    );
    assert_eq!(all[0].facts.confidence, 60);
}

fn counting() -> (Box<dyn br8n::embed::Embedder>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let embedder = common::FakeEmbedder {
        calls: calls.clone(),
        queries: Arc::new(AtomicUsize::new(0)),
    };
    (Box::new(embedder), calls)
}

fn titled(text: &str, title: &str) -> Remember {
    Remember {
        title: Some(title.into()),
        ..lesson(text)
    }
}

fn only(root: &std::path::Path) -> Memory {
    let mut all = list_at(root, &Filter::default()).unwrap();
    assert_eq!(all.len(), 1, "{all:?}");
    all.remove(0)
}

#[test]
fn an_edit_that_sends_no_title_keeps_a_title_written_by_hand() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let Outcome::Saved { id } = remember_at(
        &root,
        &cfg4(),
        fake(),
        titled("Never comment code unless asked.", "Comment policy"),
    )
    .unwrap() else {
        panic!("the fixture must save")
    };

    let (embedder, calls) = counting();
    let mut confidence_only = lesson("Never comment code unless asked.");
    confidence_only.confidence = 60;
    edit_at(&root, &cfg4(), embedder, &id, confidence_only).unwrap();
    let kept = only(&root);
    assert_eq!(kept.title, "Comment policy");
    assert_eq!(kept.facts.confidence, 60);
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the title is part of the embedded text, so keeping it must reuse the stored vector"
    );

    edit_at(
        &root,
        &cfg4(),
        fake(),
        &kept.id,
        lesson("Never comment code unless the user asks for it."),
    )
    .unwrap();
    let rewritten = only(&root);
    assert!(rewritten.text.contains("the user asks"), "{rewritten:?}");
    assert_eq!(
        rewritten.title, "Comment policy",
        "a hand-written title outlives an edit of the text it heads"
    );
}

#[test]
fn an_edit_that_sends_no_title_rederives_a_title_that_was_derived() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let Outcome::Saved { id } = remember_at(
        &root,
        &cfg4(),
        fake(),
        lesson("Never comment code unless asked."),
    )
    .unwrap() else {
        panic!("the fixture must save")
    };
    assert_eq!(
        only(&root).title,
        summary_title("Never comment code unless asked.")
    );
    edit_at(
        &root,
        &cfg4(),
        fake(),
        &id,
        lesson("Squash every branch to one commit before the PR."),
    )
    .unwrap();
    assert_eq!(
        only(&root).title,
        summary_title("Squash every branch to one commit before the PR.")
    );
}

#[test]
fn an_edit_that_changes_only_the_title_keeps_the_id_and_re_embeds() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let Outcome::Saved { id } = remember_at(
        &root,
        &cfg4(),
        fake(),
        lesson("Never comment code unless asked."),
    )
    .unwrap() else {
        panic!("the fixture must save")
    };
    let (embedder, calls) = counting();
    edit_at(
        &root,
        &cfg4(),
        embedder,
        &id,
        titled("Never comment code unless asked.", "Comment policy"),
    )
    .unwrap();
    let edited = only(&root);
    assert_eq!(edited.id, id, "the id hashes kind and text, not the title");
    assert_eq!(edited.title, "Comment policy");
    assert!(
        calls.load(std::sync::atomic::Ordering::SeqCst) > 0,
        "the title is embedded with the text, so a new title needs a new vector"
    );
}

#[test]
fn an_embedder_failure_during_a_title_only_edit_leaves_the_original_intact() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let Outcome::Saved { id } = remember_at(
        &root,
        &cfg4(),
        fake(),
        lesson("Never comment code unless asked."),
    )
    .unwrap() else {
        panic!("the fixture must save")
    };
    let err = edit_at(
        &root,
        &cfg4(),
        Box::new(BrokenEmbedder),
        &id,
        titled("Never comment code unless asked.", "Comment policy"),
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("ollama down"), "{err:#}");
    let original = only(&root);
    assert_eq!(original.id, id);
    assert_eq!(
        original.title,
        summary_title("Never comment code unless asked.")
    );
}

struct BrokenEmbedder;
impl br8n::embed::Embedder for BrokenEmbedder {
    fn embed_documents(&self, _: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!("ollama down")
    }
    fn embed_query(&self, _: &str) -> anyhow::Result<Vec<f32>> {
        Ok(vec![0.5; 4])
    }
    fn warm(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn model_id(&self) -> String {
        "fake@4".into()
    }
    fn dimensions(&self) -> usize {
        4
    }
}

#[test]
fn editing_an_unknown_or_ambiguous_id_refuses_and_says_which() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let e = edit_at(
        &root,
        &cfg4(),
        fake(),
        "000000000000",
        lesson("Anything at all here."),
    )
    .unwrap_err();
    assert!(e.downcast_ref::<MemoryNotFound>().is_some(), "got {e:#}");
    let c = no_dedup_cfg();
    let mut by_first: std::collections::HashMap<char, String> = std::collections::HashMap::new();
    let mut pair: Option<(String, String, char)> = None;
    for n in 0..64 {
        let text = format!("Lesson number {n} that this project has learned.");
        let id = memory_id(MemoryKind::Lesson, &text);
        let first = id.chars().next().unwrap();
        if let Some(other) = by_first.get(&first) {
            pair = Some((other.clone(), text, first));
            break;
        }
        by_first.insert(first, text);
    }
    let (first_text, second_text, shared) =
        pair.expect("16 hex values over 64 candidates must collide on the first character");
    remember_at(&root, &c, fake(), lesson(&first_text)).unwrap();

    let e = edit_at(&root, &c, fake(), "", lesson("Anything at all here.")).unwrap_err();
    assert!(
        e.downcast_ref::<MemoryNotFound>().is_some(),
        "an empty id prefix-matches the one memory present, so it must name nothing rather than silently edit it, got {e:#}"
    );
    assert_eq!(
        list_at(&root, &Filter::default()).unwrap().len(),
        1,
        "the refused edit left the memory alone"
    );

    remember_at(&root, &c, fake(), lesson(&second_text)).unwrap();
    let all = list_at(&root, &Filter::default()).unwrap();
    assert_eq!(all.len(), 2, "both lessons must be stored");

    let e = edit_at(
        &root,
        &c,
        fake(),
        &shared.to_string(),
        lesson("Anything at all here."),
    )
    .unwrap_err();
    assert!(
        e.downcast_ref::<MemoryAmbiguous>().is_some(),
        "an ambiguous prefix must say so, got {e:#}"
    );
    assert!(
        format!("{e}").contains("matches 2 memories"),
        "the refusal names how many it matched, got {e}"
    );
}

#[test]
fn an_embedder_failure_during_an_edit_leaves_the_original_intact() {
    struct Broken;
    impl br8n::embed::Embedder for Broken {
        fn embed_documents(&self, _: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
            anyhow::bail!("ollama down")
        }
        fn embed_query(&self, _: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![0.5; 4])
        }
        fn warm(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn model_id(&self) -> String {
            "fake@4".into()
        }
        fn dimensions(&self) -> usize {
            4
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("memory");
    let Outcome::Saved { id } = remember_at(
        &root,
        &cfg4(),
        fake(),
        lesson("Never comment code unless asked."),
    )
    .unwrap() else {
        panic!()
    };
    let err = edit_at(
        &root,
        &cfg4(),
        Box::new(Broken),
        &id,
        lesson("Never comment code unless the user asks."),
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("ollama down"), "{err:#}");
    let store = br8n::store::Store::open(&br8n::memory::store_dir(&root), 4).unwrap();
    assert_eq!(
        store.count_documents().unwrap(),
        1,
        "the store itself must still hold the original, not just its published pack"
    );
    let all = list_at(&root, &Filter::default()).unwrap();
    assert_eq!(all.len(), 1, "the original must survive a failed edit");
    assert_eq!(all[0].id, id);
    assert!(all[0].text.contains("unless asked"));
}
