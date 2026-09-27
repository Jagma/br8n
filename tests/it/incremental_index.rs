use br8n::config::Config;
use br8n::embed::Embedder;
use br8n::index::Indexer;
use br8n::model::Document;
use br8n::store::Store;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Counts TEXTS embedded, not calls. The distinction is the whole point: a
/// re-index of a grown document makes the same number of calls either way, and
/// only the text count shows whether unchanged chunks were re-embedded.
struct CountingEmbedder {
    texts: Arc<AtomicUsize>,
}
impl Embedder for CountingEmbedder {
    fn embed_documents(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.texts.fetch_add(texts.len(), Ordering::SeqCst);
        Ok(texts.iter().map(|t| hash4(t)).collect())
    }
    fn embed_query(&self, t: &str) -> anyhow::Result<Vec<f32>> {
        Ok(hash4(t))
    }
    fn warm(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn model_id(&self) -> String {
        "counting@4".into()
    }
    fn dimensions(&self) -> usize {
        4
    }
}
fn hash4(s: &str) -> Vec<f32> {
    let b = s.as_bytes();
    br8n::embed::normalize(
        (0..4)
            .map(|i| b.iter().skip(i).step_by(4).map(|x| *x as f32).sum())
            .collect(),
    )
}

/// Chunking targets 512 tokens, sized as 2048 characters, so each section here
/// is deliberately long enough to be a chunk on its own.
fn body(sections: usize) -> String {
    let filler = "pooling vectors retrieval embedding transcript fusion latency graph ".repeat(40);
    (0..sections)
        .map(|i| format!("## Section {i}\n\n{filler}\n"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn appending_to_a_document_only_embeds_the_new_text() {
    // A session transcript grows by a few messages and used to cost a full
    // re-embed of every chunk it already had. Measured on a real corpus, a run
    // where only the live transcript had changed took 405 seconds; reusing the
    // existing vectors took it to 9.
    let dir = tempfile::tempdir().unwrap();
    let texts = Arc::new(AtomicUsize::new(0));
    let store = Store::open(dir.path(), 4).unwrap();
    let idx = Indexer::new(
        store,
        Box::new(CountingEmbedder {
            texts: texts.clone(),
        }),
        Config::default(),
    );

    let first = Document::new(
        br8n::model::SourceType::Markdown,
        "file:///grow.md",
        "Grow",
        &body(8),
    );
    let stats = idx.index_documents(std::slice::from_ref(&first)).unwrap();
    let initial_chunks = stats.chunks;
    let after_first = texts.load(Ordering::SeqCst);
    assert!(initial_chunks >= 6, "fixture must produce several chunks");
    assert_eq!(
        after_first, initial_chunks,
        "the first index must embed every chunk exactly once"
    );

    // Same text, plus more at the end — exactly what an append looks like.
    let grown = Document::new(
        br8n::model::SourceType::Markdown,
        "file:///grow.md",
        "Grow",
        &body(10),
    );
    let stats2 = idx.index_documents(std::slice::from_ref(&grown)).unwrap();
    let added = texts.load(Ordering::SeqCst) - after_first;

    assert_eq!(stats2.updated, 1, "the document must be seen as changed");
    assert!(
        added < stats2.chunks,
        "re-embedded {added} texts for {} chunks — unchanged chunks were not reused",
        stats2.chunks
    );
    assert!(
        added <= 4,
        "appending two sections should embed a handful of chunks, not {added}"
    );
}

/// A document that gains one chunk must rewrite one chunk, not all of them.
///
/// `upsert_document` deletes every chunk of the document and the re-insert
/// writes them all back under the same ids. lbug cannot reclaim the space the
/// old rows occupied, so the file grows by the whole document on every run —
/// measured on the live index at ~584 KB per net-new chunk against ~6 KB of
/// actual content.
#[test]
fn reindexing_a_grown_document_rewrites_only_what_changed() {
    let dir = tempfile::tempdir().unwrap();
    let store = br8n::store::Store::open(dir.path(), 4).unwrap();
    let doc =
        br8n::model::Document::new(br8n::model::SourceType::Markdown, "file:///a.md", "A", "x");
    store.upsert_document(&doc).unwrap();

    let mk = |n: usize| -> (Vec<br8n::model::Chunk>, Vec<Vec<f32>>) {
        let cs: Vec<_> = (0..n)
            .map(|i| br8n::model::Chunk {
                id: br8n::model::Chunk::id(&doc.id, i as i64),
                doc_id: doc.id.clone(),
                ord: i as i64,
                text: format!("paragraph {i} about connection pooling"),
                embed_text: format!("paragraph {i} about connection pooling"),
                heading_path: String::new(),
                page_no: None,
            })
            .collect();
        let vs = vec![br8n::embed::normalize(vec![1.0, 0.0, 0.0, 0.0]); n];
        (cs, vs)
    };

    let (c5, v5) = mk(5);
    let d = store.replace_chunks(&doc.id, &c5, &v5).unwrap();
    assert_eq!(
        (d.inserted, d.deleted, d.kept),
        (5, 0, 0),
        "first write inserts all"
    );

    // Same five chunks, plus a sixth. Only the sixth is new.
    let (c6, v6) = mk(6);
    let d = store.replace_chunks(&doc.id, &c6, &v6).unwrap();
    assert_eq!(
        (d.inserted, d.deleted, d.kept),
        (1, 0, 5),
        "a document that grew by one chunk must write one chunk"
    );
}

/// A chunk whose text changed must be rewritten; its neighbours must not.
#[test]
fn only_the_edited_chunk_is_rewritten() {
    let dir = tempfile::tempdir().unwrap();
    let store = br8n::store::Store::open(dir.path(), 4).unwrap();
    let doc =
        br8n::model::Document::new(br8n::model::SourceType::Markdown, "file:///a.md", "A", "x");
    store.upsert_document(&doc).unwrap();

    let mk = |third: &str| -> (Vec<br8n::model::Chunk>, Vec<Vec<f32>>) {
        let bodies = ["one", "two", third];
        let cs: Vec<_> = bodies
            .iter()
            .enumerate()
            .map(|(i, b)| br8n::model::Chunk {
                id: br8n::model::Chunk::id(&doc.id, i as i64),
                doc_id: doc.id.clone(),
                ord: i as i64,
                text: b.to_string(),
                embed_text: b.to_string(),
                heading_path: String::new(),
                page_no: None,
            })
            .collect();
        let vs = vec![br8n::embed::normalize(vec![1.0, 0.0, 0.0, 0.0]); 3];
        (cs, vs)
    };

    let (a, va) = mk("three");
    store.replace_chunks(&doc.id, &a, &va).unwrap();
    let (b, vb) = mk("three, revised");
    let d = store.replace_chunks(&doc.id, &b, &vb).unwrap();
    assert_eq!(
        (d.inserted, d.deleted, d.kept),
        (1, 1, 2),
        "one chunk changed: one out, one in, two untouched"
    );
}

/// A document that lost chunks must delete them.
#[test]
fn chunks_removed_from_a_document_are_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let store = br8n::store::Store::open(dir.path(), 4).unwrap();
    let doc =
        br8n::model::Document::new(br8n::model::SourceType::Markdown, "file:///a.md", "A", "x");
    store.upsert_document(&doc).unwrap();

    let mk = |n: usize| -> (Vec<br8n::model::Chunk>, Vec<Vec<f32>>) {
        let cs: Vec<_> = (0..n)
            .map(|i| br8n::model::Chunk {
                id: br8n::model::Chunk::id(&doc.id, i as i64),
                doc_id: doc.id.clone(),
                ord: i as i64,
                text: format!("para {i}"),
                embed_text: format!("para {i}"),
                heading_path: String::new(),
                page_no: None,
            })
            .collect();
        (
            cs,
            vec![br8n::embed::normalize(vec![1.0, 0.0, 0.0, 0.0]); n],
        )
    };

    let (c4, v4) = mk(4);
    store.replace_chunks(&doc.id, &c4, &v4).unwrap();
    let (c2, v2) = mk(2);
    let d = store.replace_chunks(&doc.id, &c2, &v2).unwrap();
    assert_eq!(
        (d.inserted, d.deleted, d.kept),
        (0, 2, 2),
        "two chunks removed"
    );
}

mod query_priority {
    use br8n::index::{query_marker_path, QueryPriority};

    #[test]
    fn the_marker_appears_while_a_query_is_announced_and_is_gone_after() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db");
        let marker = query_marker_path(&db);

        assert!(!marker.exists(), "no marker before a query");
        let p = QueryPriority::announce(&db);
        assert!(marker.exists(), "marker present while the guard lives");

        drop(p);
        // The marker MUST go when the query does. A guard that left it behind
        // (the previous, deliberately-empty `Drop`) made every finished query
        // look like a running one for the next 3 seconds, so an indexer that
        // should have resumed in ~200ms slept out the whole staleness window
        // at its next document boundary — about a minute across a 389-document
        // index with twenty prompts in it. Removal is safe now because the
        // path is this process's own (`db.qwait.<pid>`), not one shared file
        // every reader announces on.
        assert!(
            !marker.exists(),
            "a finished query must not leave a fresh marker throttling the indexer"
        );
    }

    #[test]
    fn a_second_readers_marker_is_a_separate_file() {
        // The hazard the empty `Drop` was avoiding: the hook is mid-embed
        // while the dashboard's `/api/search` finishes its own query. With one
        // shared path and no refcount, the dashboard's guard deleted the
        // marker the hook was still relying on and indexing stopped yielding
        // to it. Per-reader paths remove the hazard rather than the removal:
        // no guard can touch another reader's file, because it does not know
        // its name — it only ever writes and deletes its own pid's.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db");
        let mine = query_marker_path(&db);

        // Another reader process, simulated by writing its marker directly:
        // same database, different pid, therefore a different file.
        let theirs = db.with_extension(format!("qwait.{}", std::process::id() + 1));
        assert_ne!(mine, theirs, "markers must be per reader, not shared");
        std::fs::write(&theirs, b"q").unwrap();

        drop(QueryPriority::announce(&db));
        assert!(
            !mine.exists(),
            "this process cleans up after its own query..."
        );
        assert!(
            theirs.exists(),
            "...and can never delete another reader's marker"
        );
    }

    #[test]
    fn overlapping_guards_in_one_process_keep_the_marker_until_the_last_drops() {
        // Both announcers CAN be one process — two dashboard searches served
        // on two threads share a pid and therefore a marker path. The
        // refcount is what keeps the first one to finish from pulling the file
        // out from under the second.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db");
        let marker = query_marker_path(&db);

        let first = QueryPriority::announce(&db);
        let second = QueryPriority::announce(&db);
        assert!(marker.exists(), "marker present while both guards live");

        drop(second);
        assert!(
            marker.exists(),
            "a guard finishing must not remove the marker a still-live guard \
             in the same process depends on"
        );

        drop(first);
        assert!(
            !marker.exists(),
            "the last guard out removes the file — nothing is left to throttle \
             the indexer"
        );
    }
}

/// A chunk the diff KEPT must still be reachable from its document.
///
/// `replace_chunks` skips rewriting a chunk whose `embed_text` hash is
/// unchanged — that is the whole point, since rewriting it would cost an embed
/// and grow a file lbug cannot reclaim. But `upsert_document` runs first and
/// `DETACH DELETE`s the old Document node, which severs `HAS_CHUNK` to every
/// chunk including the kept ones. Every retrieval read joins
/// `MATCH (d:Document)-[:HAS_CHUNK]->(c)` — `hydrate` below among them — so an
/// orphaned chunk vanishes from retrieval with no error at all.
///
/// Removing the `MERGE` that restores the edge failed NO test across five
/// binaries and 57 tests when this was written. This is that test.
#[test]
fn chunks_kept_by_the_diff_are_still_reachable_from_their_document() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), 4).unwrap();
    let doc = Document::new(
        br8n::model::SourceType::Markdown,
        "file:///a.md",
        "Pooling",
        "x",
    );

    // Three chunks; the third differs between the two writes so the first two
    // are KEPT and the third is rewritten.
    let build = |third: &str| -> (Vec<br8n::model::Chunk>, Vec<Vec<f32>>) {
        let bodies = [
            "connection pooling with pgbouncer",
            "transaction mode",
            third,
        ];
        let cs: Vec<_> = bodies
            .iter()
            .enumerate()
            .map(|(i, b)| br8n::model::Chunk {
                id: br8n::model::Chunk::id(&doc.id, i as i64),
                doc_id: doc.id.clone(),
                ord: i as i64,
                text: (*b).to_string(),
                embed_text: (*b).to_string(),
                heading_path: String::new(),
                page_no: None,
            })
            .collect();
        let vs: Vec<Vec<f32>> = bodies.iter().map(|b| hash4(b)).collect();
        (cs, vs)
    };

    let (a, va) = build("idle session handling");
    store.upsert_document(&doc).unwrap();
    store.replace_chunks(&doc.id, &a, &va).unwrap();

    let (b, vb) = build("idle session handling, revised");
    store.upsert_document(&doc).unwrap();
    let delta = store.replace_chunks(&doc.id, &b, &vb).unwrap();
    assert_eq!(delta.kept, 2, "two chunks must have been kept");

    // The kept chunks must still be retrievable. `hydrate` joins through
    // exactly the `HAS_CHUNK` edge `upsert_document` severs, so a chunk
    // orphaned from its document comes back empty rather than erroring.
    let kept_id = br8n::model::Chunk::id(&doc.id, 0);
    let hits = store.hydrate(std::slice::from_ref(&kept_id)).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "the kept chunk is orphaned from its document and has vanished from \
         retrieval"
    );
    assert_eq!(hits[0].chunk_id, kept_id);
    assert!(
        !hits[0].uri.is_empty(),
        "a hit whose document join failed carries no uri"
    );
}
