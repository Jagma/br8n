use crate::common;

use br8n::config::{MemoryConfig, Profile};
use br8n::index::Indexer;
use br8n::memory::{MemoryFacts, MemoryKind, Origin};
use br8n::model::{Document, SourceType};
use br8n::pack::records::Record;
use br8n::pack::Pack;
use br8n::retrieve::Retriever;
use br8n::store::Store;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn facts(kind: MemoryKind, created: i64) -> MemoryFacts {
    MemoryFacts {
        kind,
        created,
        project: None,
        origin: Origin::User,
        confidence: 100,
        session: None,
        source_hash: None,
        source_stamp: None,
    }
}

fn memory_record(n: usize, text: &str, kind: MemoryKind, created: i64) -> Record {
    Record {
        chunk_id: format!("mem{n}:0"),
        doc_id: format!("mem{n}"),
        text: text.into(),
        heading_path: String::new(),
        uri: format!("memory://{}/{n:012}", kind.as_str()),
        title: text.into(),
        page_no: None,
        source_type: "memory".into(),
        inbound: 0,
        lifecycle: Default::default(),
        last_used: None,
        memory: Some(facts(kind, created)),
    }
}

fn build_pack(dir: &std::path::Path, rows: Vec<(Record, Vec<f32>)>) -> Pack {
    std::fs::create_dir_all(dir).unwrap();
    Pack::build(dir, "fake@4", 4, rows, &HashMap::new(), &HashMap::new()).unwrap();
    Pack::open(dir, "fake@4", 4).unwrap()
}

fn main_store_and_pack(dir: &std::path::Path, docs: &[Document]) -> (Store, Pack) {
    {
        let store = Store::open(dir, 4).unwrap();
        let idx = Indexer::new(
            store,
            Box::new(common::FakeEmbedder::default()),
            br8n::config::Config::default(),
        );
        idx.index_documents(docs).unwrap();
    }
    let store = Store::open(dir, 4).unwrap();
    let rows = store.all_rows_for_pack().unwrap();
    let pack = build_pack(&dir.join("pack"), rows);
    (store, pack)
}

fn two_docs() -> Vec<Document> {
    vec![
        Document::new(
            SourceType::Markdown,
            "file:///a.md",
            "Pooling",
            "# Pooling\n\nPgBouncer runs in transaction mode and drops session state.",
        ),
        Document::new(
            SourceType::Markdown,
            "file:///b.md",
            "Baking",
            "# Baking\n\nSourdough needs a long cold ferment for flavour.",
        ),
    ]
}

#[test]
fn a_memory_found_only_by_keyword_gets_its_cosine_from_the_memory_pack() {
    let dir = tempfile::tempdir().unwrap();
    let (store, main) = main_store_and_pack(dir.path(), &two_docs());
    let query = "zorblax";
    let text = "The zorblax flag disables telemetry in the build.";
    let decoy = memory_record(
        1,
        "An unrelated decoy that vector search prefers.",
        MemoryKind::Fact,
        br8n::memory::now_secs(),
    );
    let target = memory_record(2, text, MemoryKind::Fact, br8n::memory::now_secs());
    let rows = vec![
        (decoy, common::hash_vec(query)),
        (
            target,
            common::hash_vec(&br8n::model::Chunk::plain_embed_text(text, "", text)),
        ),
    ];
    let mem = build_pack(&dir.path().join("memory-pack"), rows);
    let r = Retriever::new(
        store,
        Box::new(common::FakeEmbedder::default()),
        "http://localhost:11434".into(),
    )
    .with_pack(Some(main))
    .with_memory(Ok(Some(mem)), MemoryConfig::default());
    let mut one_candidate = Profile::tier(1);
    one_candidate.candidates_k = 1;
    let (hits, report) = r.search_with_report(query, &one_candidate).unwrap();
    let m = hits
        .iter()
        .find(|h| h.doc_id == "mem2")
        .expect("the memory must be retrievable by its rare keyword");
    assert!(
        m.relevance > 0.0,
        "relevance must be measured from the memory pack, got {}",
        m.relevance
    );
    assert_eq!(m.memory.as_ref().map(|f| f.kind), Some(MemoryKind::Fact));
    assert!(report.stages_run.contains(&"memory"));
    assert!(hits
        .iter()
        .filter(|h| h.source_type != "memory")
        .all(|h| h.memory.is_none()));
}

#[test]
fn with_no_memory_pack_results_are_identical_to_before() {
    let dir = tempfile::tempdir().unwrap();
    let before: Vec<(String, f32)> = {
        let (store, main) = main_store_and_pack(dir.path(), &two_docs());
        let plain = Retriever::new(
            store,
            Box::new(common::FakeEmbedder::default()),
            "http://localhost:11434".into(),
        )
        .with_pack(Some(main));
        plain
            .search("transaction mode", &Profile::tier(1))
            .unwrap()
            .into_iter()
            .map(|h| (h.chunk_id, h.relevance))
            .collect()
    };
    let (store, main) = main_store_and_pack(dir.path(), &two_docs());
    let with = Retriever::new(
        store,
        Box::new(common::FakeEmbedder::default()),
        "http://localhost:11434".into(),
    )
    .with_pack(Some(main))
    .with_memory(Ok(None::<br8n::pack::Pack>), MemoryConfig::default());
    let hits = with.search("transaction mode", &Profile::tier(1)).unwrap();
    let after: Vec<(String, f32)> = hits
        .iter()
        .map(|h| (h.chunk_id.clone(), h.relevance))
        .collect();
    assert_eq!(before, after);
    let top = hits.first().expect("the fixture must retrieve something");
    assert_eq!(
        top.uri, "file:///a.md",
        "the pooling note answers `transaction mode` and must rank first"
    );
    assert!(top.relevance > 0.0);
    assert!(hits.iter().all(|h| h.memory.is_none()));
}

#[test]
fn a_vectorless_main_pack_beside_a_vectored_memory_pack_still_embeds_the_query() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _) = main_store_and_pack(dir.path(), &two_docs());
    let vectorless: Vec<(Record, Vec<f32>)> = store
        .all_rows_for_pack()
        .unwrap()
        .into_iter()
        .map(|(r, _)| (r, Vec::new()))
        .collect();
    let main = build_pack(&dir.path().join("vectorless"), vectorless);
    let text = "Remember the zorblax flag.";
    let mem = build_pack(
        &dir.path().join("mem"),
        vec![(
            memory_record(1, text, MemoryKind::Lesson, 0),
            common::hash_vec(text),
        )],
    );
    let queries = Arc::new(AtomicUsize::new(0));
    let emb = Box::new(common::FakeEmbedder {
        calls: Arc::new(AtomicUsize::new(0)),
        queries: queries.clone(),
    });
    let r = Retriever::new(store, emb, "http://localhost:11434".into())
        .with_pack(Some(main))
        .with_memory(Ok(Some(mem)), MemoryConfig::default());
    let hits = r.search("zorblax flag", &Profile::tier(1)).unwrap();
    assert_eq!(
        queries.load(Ordering::SeqCst),
        1,
        "the query must be embedded for the memory pack's benefit"
    );
    assert!(hits
        .iter()
        .any(|h| h.source_type == "memory" && h.relevance > 0.0));
}

#[test]
fn graph_expansion_seeds_from_main_hits_even_when_memories_outrank_them() {
    let dir = tempfile::tempdir().unwrap();
    let section = |n: usize| {
        format!(
            "## Part {n}\n\n{}\n\n",
            format!("pooling detail {n} ").repeat(120)
        )
    };
    let long = format!("# Pooling\n\n{}", (1..=8).map(section).collect::<String>());
    let docs = vec![Document::new(
        SourceType::Markdown,
        "file:///long.md",
        "Pooling",
        &long,
    )];
    let (store, main) = main_store_and_pack(dir.path(), &docs);
    assert!(main.rows() > 5, "seeds take only the top 5, so the fixture needs more chunks than that to leave room for graph expansion to find one beyond the seed set");
    let query = "pooling detail";
    let qv = common::hash_vec(query);
    let rows: Vec<(Record, Vec<f32>)> = (1..=6)
        .map(|n| {
            (
                memory_record(n, &format!("memory {n} about pooling"), MemoryKind::Fact, 0),
                qv.clone(),
            )
        })
        .collect();
    let mem = build_pack(&dir.path().join("mem"), rows);
    let r = Retriever::new(
        store,
        Box::new(common::FakeEmbedder::default()),
        "http://localhost:11434".into(),
    )
    .with_pack(Some(main))
    .with_memory(Ok(Some(mem)), MemoryConfig::default());
    let explain = r
        .search_explained(query, &Profile::tier(2), 0.0, 4000)
        .unwrap();
    let graph = explain
        .stages
        .iter()
        .find(|s| s.name == "graph")
        .expect("tier 2 runs graph expansion");
    assert!(
        !graph.hits.is_empty(),
        "seeds must come from main-pack hits, or expansion finds nothing"
    );
}

#[test]
fn episodes_decay_and_lessons_do_not() {
    let dir = tempfile::tempdir().unwrap();
    let (store, main) = main_store_and_pack(dir.path(), &two_docs());
    let text = "zorblax build flag notes";
    let old = 365 * 86_400;
    let rows = vec![
        (
            memory_record(1, text, MemoryKind::Episode, br8n::memory::now_secs() - old),
            common::hash_vec(text),
        ),
        (
            memory_record(2, text, MemoryKind::Lesson, br8n::memory::now_secs() - old),
            common::hash_vec(text),
        ),
    ];
    let mem = build_pack(&dir.path().join("mem"), rows);
    let cfg = MemoryConfig {
        episode_decay_floor: 0.5,
        episode_half_life_days: 30.0,
        ..MemoryConfig::default()
    };
    let r = Retriever::new(
        store,
        Box::new(common::FakeEmbedder::default()),
        "http://localhost:11434".into(),
    )
    .with_pack(Some(main))
    .with_memory(Ok(Some(mem)), cfg);
    let hits = r.search(text, &Profile::tier(1)).unwrap();
    let ep = hits.iter().find(|h| h.doc_id == "mem1").unwrap();
    let ls = hits.iter().find(|h| h.doc_id == "mem2").unwrap();
    assert!(
        ep.relevance < ls.relevance,
        "episode {} must be demoted below lesson {}",
        ep.relevance,
        ls.relevance
    );
    assert!((ep.relevance / ls.relevance - 0.5).abs() < 0.01);
}

#[test]
fn an_unusable_memory_pack_is_reported_and_the_main_query_still_answers() {
    let dir = tempfile::tempdir().unwrap();
    let (store, main) = main_store_and_pack(dir.path(), &two_docs());
    let r = Retriever::new(
        store,
        Box::new(common::FakeEmbedder::default()),
        "http://localhost:11434".into(),
    )
    .with_pack(Some(main))
    .with_memory(
        Err::<Option<br8n::pack::Pack>, _>(anyhow::anyhow!("model mismatch")),
        MemoryConfig::default(),
    );
    let (hits, report) = r
        .search_with_report("transaction mode", &Profile::tier(1))
        .unwrap();
    assert!(!hits.is_empty());
    assert!(report
        .memory_unavailable
        .as_deref()
        .unwrap_or("")
        .contains("model mismatch"));
}

#[test]
fn the_vector_stage_breaks_a_main_memory_relevance_tie_on_chunk_id() {
    let dir = tempfile::tempdir().unwrap();
    let query = "pgbouncer transaction mode";
    let qv = common::hash_vec(query);

    let main_record = Record {
        chunk_id: "zz_main:0".into(),
        doc_id: "zz_main".into(),
        text: "PgBouncer runs in transaction mode.".into(),
        heading_path: String::new(),
        uri: "file:///zz_main.md".into(),
        title: "zz_main".into(),
        page_no: None,
        source_type: "markdown".into(),
        inbound: 0,
        lifecycle: Default::default(),
        last_used: None,
        memory: None,
    };
    let main = build_pack(
        &dir.path().join("main-pack"),
        vec![(main_record, qv.clone())],
    );

    let mem = memory_record(
        1,
        "PgBouncer runs in transaction mode.",
        MemoryKind::Fact,
        br8n::memory::now_secs(),
    );
    let memory_pack = build_pack(&dir.path().join("memory-pack"), vec![(mem, qv.clone())]);

    let store_dir = tempfile::tempdir().unwrap();
    let store = Store::open(store_dir.path(), 4).unwrap();
    let r = Retriever::new(
        store,
        Box::new(common::FakeEmbedder::default()),
        "http://localhost:11434".into(),
    )
    .with_pack(Some(main))
    .with_memory(Ok(Some(memory_pack)), MemoryConfig::default());

    let explain = r
        .search_explained(query, &Profile::tier(1), 0.0, 100_000)
        .unwrap();
    let vector_stage = explain
        .stages
        .iter()
        .find(|s| s.name == "vector")
        .expect("the vector stage must be traced");

    assert_eq!(
        vector_stage.hits.len(),
        2,
        "both the main and the memory hit must reach the vector stage"
    );
    assert_eq!(
        vector_stage.hits[0].relevance, vector_stage.hits[1].relevance,
        "the fixture requires a genuine relevance tie between the main and the \
         memory hit, or the tie-break below proves nothing"
    );

    let order: Vec<&str> = vector_stage
        .hits
        .iter()
        .map(|h| h.chunk_id.as_str())
        .collect();
    assert_eq!(
        order,
        vec!["mem1:0", "zz_main:0"],
        "a relevance tie between a main hit and a memory hit must resolve to \
         ascending chunk_id, not the order the memory hit was appended in"
    );
}
