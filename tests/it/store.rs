use crate::common;

use br8n::model::{Chunk, Document, SourceType};
use br8n::store::Store;

fn tmpdb() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn schema_initializes_and_roundtrips_a_document() {
    let dir = tmpdb();
    let store = Store::open(dir.path(), 4).unwrap();

    let doc = Document::new(SourceType::Markdown, "file:///a.md", "A", "hello");
    store.upsert_document(&doc).unwrap();

    assert_eq!(
        store.doc_hash(&doc.id).unwrap(),
        Some(doc.content_hash.clone())
    );
}

#[test]
fn upsert_replaces_rather_than_duplicates() {
    let dir = tmpdb();
    let store = Store::open(dir.path(), 4).unwrap();

    let v1 = Document::new(SourceType::Markdown, "file:///a.md", "A", "one");
    let v2 = Document::new(SourceType::Markdown, "file:///a.md", "A", "two");
    store.upsert_document(&v1).unwrap();
    store.upsert_document(&v2).unwrap();

    assert_eq!(
        store.doc_hash(&v1.id).unwrap(),
        Some(v2.content_hash.clone())
    );
    assert_eq!(store.count_documents().unwrap(), 1);
}

#[test]
fn chunks_are_linked_to_document_and_to_each_other() {
    let dir = tmpdb();
    let store = Store::open(dir.path(), 4).unwrap();

    let doc = Document::new(SourceType::Markdown, "file:///a.md", "A", "hello");
    store.upsert_document(&doc).unwrap();

    let chunks: Vec<Chunk> = (0..3)
        .map(|i| Chunk {
            id: Chunk::id(&doc.id, i),
            doc_id: doc.id.clone(),
            ord: i,
            text: format!("chunk {i}"),
            embed_text: format!("A > chunk {i}"),
            heading_path: "A".into(),
            page_no: None,
        })
        .collect();
    let vecs = vec![vec![1.0, 0.0, 0.0, 0.0]; 3];

    store.insert_chunks(&doc.id, &chunks, &vecs).unwrap();

    assert_eq!(store.count_chunks().unwrap(), 3);
    // chunk 0 -> chunk 1 -> chunk 2 adjacency, used instead of text overlap
    assert_eq!(
        store.next_chunk(&Chunk::id(&doc.id, 0)).unwrap(),
        Some(Chunk::id(&doc.id, 1))
    );
    assert_eq!(store.next_chunk(&Chunk::id(&doc.id, 2)).unwrap(), None);
}

#[test]
fn deleting_a_document_prunes_its_chunks() {
    let dir = tmpdb();
    let store = Store::open(dir.path(), 4).unwrap();

    let doc = Document::new(SourceType::Markdown, "file:///a.md", "A", "hello");
    store.upsert_document(&doc).unwrap();
    let chunks = vec![Chunk {
        id: Chunk::id(&doc.id, 0),
        doc_id: doc.id.clone(),
        ord: 0,
        text: "c".into(),
        embed_text: "c".into(),
        heading_path: "".into(),
        page_no: None,
    }];
    store
        .insert_chunks(&doc.id, &chunks, &[vec![1.0, 0.0, 0.0, 0.0]])
        .unwrap();

    store.delete_document(&doc.id).unwrap();

    assert_eq!(store.count_documents().unwrap(), 0);
    assert_eq!(
        store.count_chunks().unwrap(),
        0,
        "orphan chunks poison retrieval"
    );
}

#[test]
fn meta_roundtrips_for_model_stamping() {
    let dir = tmpdb();
    let store = Store::open(dir.path(), 4).unwrap();
    store
        .set_meta("embed_model", "qwen3-embedding:0.6b")
        .unwrap();
    assert_eq!(
        store.get_meta("embed_model").unwrap().as_deref(),
        Some("qwen3-embedding:0.6b")
    );
    assert_eq!(store.get_meta("nope").unwrap(), None);
}

#[test]
fn all_doc_uris_returns_every_documents_id_and_uri() {
    let dir = tmpdb();
    let store = Store::open(dir.path(), 4).unwrap();

    let a = Document::new(SourceType::Markdown, "file:///a.md", "A", "hello");
    let b = Document::new(SourceType::Markdown, "file:///b.md", "B", "world");
    store.upsert_document(&a).unwrap();
    store.upsert_document(&b).unwrap();

    let mut pairs = store.all_doc_uris().unwrap();
    pairs.sort();
    let mut expected = vec![(a.id.clone(), a.uri.clone()), (b.id.clone(), b.uri.clone())];
    expected.sort();
    assert_eq!(pairs, expected);
}

#[test]
fn values_containing_cypher_syntax_are_stored_literally_not_executed() {
    let dir = tmpdb();
    let store = Store::open(dir.path(), 4).unwrap();

    let nasty = "it\'s a note'}) DETACH DELETE d MATCH (x:Document) CREATE (y:Document {id:'pwned";
    let doc = Document::new(SourceType::Markdown, "file:///a.md", nasty, "body");
    store.upsert_document(&doc).unwrap();

    assert_eq!(
        store.count_documents().unwrap(),
        1,
        "no injected node was created"
    );
    assert_eq!(
        store.doc_hash(&doc.id).unwrap(),
        Some(doc.content_hash.clone())
    );
}

#[test]
fn graph_snapshot_reports_nodes_edges_and_counts() {
    let dir = tempfile::tempdir().unwrap();
    let store = br8n::store::Store::open(dir.path(), 4).unwrap();
    let idx = br8n::index::Indexer::new(
        store,
        Box::new(common::FakeEmbedder {
            calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            queries: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }),
        br8n::config::Config::default(),
    );
    let mut a = br8n::model::Document::new(
        br8n::model::SourceType::Markdown,
        "file:///n/a.md",
        "Alpha",
        "# Alpha\n\nSee [[Beta]] for more.",
    );
    // `Document::new` never parses wikilinks out of `text` — only the markdown
    // loader does that (`extract_wikilinks`, run over a real file). Constructing
    // the Document directly, as every wikilink test in this crate does, means
    // `.links` must be set by hand for `resolve_links` to have anything to walk.
    a.links = vec!["Beta".into()];
    let b = br8n::model::Document::new(
        br8n::model::SourceType::Markdown,
        "file:///n/b.md",
        "Beta",
        "# Beta\n\nStands alone.",
    );
    idx.index_documents(&[a.clone(), b.clone()]).unwrap();
    idx.resolve_links(&[a.clone(), b.clone()]).unwrap();

    let snap = idx.store().graph_snapshot().unwrap();
    assert_eq!(snap.nodes.len(), 2);
    let alpha = snap.nodes.iter().find(|n| n.title == "Alpha").unwrap();
    let beta = snap.nodes.iter().find(|n| n.title == "Beta").unwrap();
    assert_eq!(alpha.source_type, "markdown");
    assert!(
        alpha.chunks >= 1,
        "chunk counts must be real, not zero-filled"
    );
    assert_eq!(beta.inbound, 1, "Beta has exactly one inbound wikilink");
    // `graph_snapshot` now projects the edge's real `r.kind` instead of the
    // hardcoded table name `"links_to"` (see `tests/dashboard.rs`'s
    // `the_graph_api_reports_an_edges_real_kind_not_a_hardcoded_label`), so a
    // plain wikilink is reported as `"wikilink"`, not `"links_to"`.
    assert!(
        snap.edges
            .iter()
            .any(|e| e.kind == "wikilink" && e.from == alpha.id && e.to == beta.id),
        "the Alpha->Beta wikilink must appear as a wikilink edge"
    );

    let by_source = idx.store().counts_by_source().unwrap();
    assert_eq!(by_source.get("markdown"), Some(&2));
}

/// A reader must not create anything. `Store::open` runs `create_dir_all`
/// plus 12 `CREATE ... IF NOT EXISTS` DDL statements; a reader that does the
/// same inside the shadow-swap rename window once left an empty database AS
/// the live index. `open_existing` therefore refuses a path with no database
/// rather than bringing one into being.
#[test]
fn open_existing_creates_nothing_when_there_is_no_index() {
    let dir = tmpdb();
    let missing = dir.path().join("not-an-index");

    // `Store` does not implement `Debug` (it wraps raw lbug FFI handles), so
    // `Result::expect_err` cannot be used here — go through `Result::err`
    // (`Option<anyhow::Error>`) instead, which only needs `E: Debug`.
    let err = br8n::store::Store::open_existing(&missing, 4)
        .err()
        .expect("opening a non-existent index must fail, not create one");
    assert!(
        err.to_string().contains("no index at"),
        "the error must tell the user to run `br8n index`, got: {err}"
    );
    assert!(
        !missing.exists(),
        "open_existing must not have created the directory"
    );
}

/// A reader opens an index that a writer built, and can query it — without
/// re-running the schema DDL that only the writer needs.
#[test]
fn open_existing_can_read_an_index_without_rebuilding_its_schema() {
    let dir = tmpdb();
    {
        let store = br8n::store::Store::open(dir.path(), 4).unwrap();
        let doc = Document::new(SourceType::Markdown, "file:///a.md", "A", "hello");
        store.upsert_document(&doc).unwrap();
    }

    let reader = br8n::store::Store::open_existing(dir.path(), 4).unwrap();
    let doc_id = br8n::model::Document::new_id("file:///a.md");
    assert!(
        reader.doc_hash(&doc_id).unwrap().is_some(),
        "a reader must see what the writer wrote"
    );
}

/// The chunk table must not store the embedded text a second time.
///
/// `embed_text` is the chunk's text plus a title/heading prefix. Storing it
/// whole doubled the corpus's dominant field to answer one question — has the
/// embedding input changed — which a hash answers in 64 bytes. Measured: a
/// clean rebuild went from 5,745 B/row to 3,406 B/row.
///
/// `chunk_hashes` already hashes whatever it reads, so asserting only that its
/// return value is 64 characters and doesn't contain the source text would
/// pass even against a column that still holds the raw text — that assertion
/// is behaviorally identical whether the store hashes on read or hashes once
/// on write. The DDL check below is what actually distinguishes the two: it
/// fails on the pre-change schema (no `embed_hash` column, `embed_text`
/// present) and passes only once the column itself has been renamed.
#[test]
fn the_chunk_table_stores_a_hash_not_the_embedded_text() {
    let ddl = br8n::store::schema::ddl(4);
    let chunk_ddl = ddl
        .iter()
        .find(|s| s.contains("Chunk("))
        .expect("Chunk table DDL must be present");
    assert!(
        chunk_ddl.contains("embed_hash"),
        "Chunk table must have an embed_hash column: {chunk_ddl}"
    );
    assert!(
        !chunk_ddl.contains("embed_text"),
        "Chunk table must not store embed_text a second time: {chunk_ddl}"
    );

    let dir = tmpdb();
    let store = Store::open(dir.path(), 4).unwrap();
    let doc = Document::new(SourceType::Markdown, "file:///a.md", "A", "x");
    store.upsert_document(&doc).unwrap();

    let body = "a distinctive sentence that would be easy to find on disk";
    let c = Chunk {
        id: Chunk::id(&doc.id, 0),
        doc_id: doc.id.clone(),
        ord: 0,
        text: body.into(),
        embed_text: format!("A > \n\n{body}"),
        heading_path: String::new(),
        page_no: None,
    };
    let v = vec![br8n::embed::normalize(vec![1.0, 0.0, 0.0, 0.0])];
    store
        .insert_chunks(&doc.id, std::slice::from_ref(&c), &v)
        .unwrap();

    let hashes = store.chunk_hashes(&doc.id).unwrap();
    assert_eq!(hashes.len(), 1, "the chunk must have a hash");
    let h = hashes.values().next().unwrap();
    assert_eq!(h.len(), 64, "a sha256 hex digest, not the text: {h}");
    assert!(!h.contains("distinctive"), "the text must not be the hash");
    assert_eq!(
        h,
        &br8n::model::Document::content_hash(&c.embed_text),
        "chunk_hashes must return exactly the hash of the stored embed_text input"
    );
}

/// `fts_body` existed only to give lbug's FTS index newline-free text to read
/// (see the retired `schema::FTS_INDEX`'s comment); schema version 3 dropped
/// both it and that index, since the pack serves BM25 now. This is the DDL
/// half of that removal — `the_store_says_bm25_moved_rather_than_returning_
/// nothing` in `tests/it/retrieve_primitives.rs` pins the behavioural half,
/// that asking the store for keyword search errors instead of silently
/// returning nothing.
#[test]
fn the_chunk_table_no_longer_stores_fts_body() {
    let ddl = br8n::store::schema::ddl(4);
    let chunk_ddl = ddl
        .iter()
        .find(|s| s.contains("Chunk("))
        .expect("Chunk table DDL must be present");
    assert!(
        !chunk_ddl.contains("fts_body"),
        "Chunk table must not store fts_body any more: {chunk_ddl}"
    );
}

/// `br8n index --compact`'s rebuild-from-store path is
/// `Store::all_chunk_rows` feeding `Store::create_chunk_row` — this pins
/// that round trip at the level compaction actually operates on, rather than
/// through the CLI.
///
/// A corruption here would NOT show up in
/// `compaction_shrinks_the_database_and_keeps_every_document`
/// (`tests/it/reindex_safety.rs`): that test only checks document count and
/// file size, and never reads a vector back. `embed_text` is gone as of
/// schema version 3 (see `schema.rs`), so if compaction ever recomputed or
/// discarded the embedding instead of carrying it across, there would be
/// nothing left to recompute it FROM — the exact "re-embedding is an
/// outage" failure mode this task exists to prevent.
#[test]
fn compaction_carries_the_exact_stored_embedding_and_hash_across() {
    let src_dir = tmpdb();
    let src = Store::open(src_dir.path(), 4).unwrap();
    let doc = Document::new(SourceType::Markdown, "file:///a.md", "A", "hello world");
    src.upsert_document(&doc).unwrap();
    let chunk = Chunk {
        id: Chunk::id(&doc.id, 0),
        doc_id: doc.id.clone(),
        ord: 0,
        text: "hello world".into(),
        embed_text: "hello world".into(),
        heading_path: "A".into(),
        page_no: None,
    };
    let original = vec![0.25_f32, 0.5, -0.75, 0.125];
    src.insert_chunks(&doc.id, &[chunk], std::slice::from_ref(&original))
        .unwrap();

    let src_rows = src.all_chunk_rows().unwrap();
    assert_eq!(src_rows.len(), 1, "sanity check: one chunk was inserted");

    // Exactly what `compact_swap` does: copy every document row, then every
    // chunk row, into a fresh store.
    let dst_dir = tmpdb();
    let dst = Store::open(dst_dir.path(), 4).unwrap();
    for d in src.all_document_rows().unwrap() {
        dst.create_document_row(&d).unwrap();
    }
    for c in &src_rows {
        dst.create_chunk_row(c).unwrap();
    }

    let dst_rows = dst.all_chunk_rows().unwrap();
    assert_eq!(dst_rows.len(), 1);
    assert_eq!(
        dst_rows[0].embedding, original,
        "compaction must carry the exact stored embedding across, not \
         recompute or discard it — re-embedding costs hours at ~40 chunks/s, \
         so a compaction that does it is an outage, not a cleanup"
    );
    assert_eq!(
        dst_rows[0].embed_hash, src_rows[0].embed_hash,
        "compaction must carry embed_hash across unchanged too — embed_text \
         is gone as of schema version 3, so there is no text left to \
         recompute it from"
    );
}

/// `create_chunk_row` is compaction's write side, and it is the second place
/// (after `insert_chunks`) a NULL `embedding` can enter the table. It must
/// write the row back as a real NULL — so the backlog survives and
/// `--backfill` can still find it — rather than as the zero-length
/// `FLOAT[4]` that `float_array(&[])` produced before.
///
/// Reached directly rather than through `Store::all_chunk_rows`, because that
/// read side refuses a NULL-embedding row outright (`compaction: chunk row
/// missing a required field`) — so `br8n index --compact` on a
/// half-backfilled index REFUSES today rather than corrupting anything. That
/// refusal is deliberate and left alone; this pins the write side so the
/// invariant does not depend on it staying that way.
///
/// Mutation evidence (run): restoring the single unconditional `CREATE ...
/// embedding: $emb` branch in `Store::create_chunk_row` makes this test fail
/// at the second `create_chunk_row` with lbug's `Binder exception: Cannot
/// change parameter expression data type from FLOAT[0] to FLOAT[4]` — the
/// row could not be copied at all.
#[test]
fn create_chunk_row_writes_a_missing_embedding_as_null_not_an_empty_array() {
    let dir = tmpdb();
    let doc = Document::new(SourceType::Markdown, "file:///a.md", "A", "hello");
    let store = Store::open(dir.path(), 4).unwrap();
    store.create_document_row(&doc_row(&doc)).unwrap();

    // One embedded row, indexed, exactly as a compaction shadow would be
    // partway through copying a half-backfilled store.
    store
        .create_chunk_row(&br8n::store::query::ChunkRow {
            id: Chunk::id(&doc.id, 0),
            doc_id: doc.id.clone(),
            ord: 0,
            text: "chunk 0".into(),
            embed_hash: "h0".into(),
            heading_path: "A".into(),
            page_no: None,
            embedding: vec![1.0, 0.0, 0.0, 0.0],
        })
        .unwrap();

    store
        .create_chunk_row(&br8n::store::query::ChunkRow {
            id: Chunk::id(&doc.id, 1),
            doc_id: doc.id.clone(),
            ord: 1,
            text: "chunk 1".into(),
            embed_hash: "h1".into(),
            heading_path: "A".into(),
            page_no: None,
            embedding: Vec::new(),
        })
        .unwrap();

    assert_eq!(
        store.count_chunks_without_vectors().unwrap(),
        1,
        "a row copied with no embedding must read back as NULL, or \
         `br8n index --backfill` can never find it again"
    );
}

/// Minimal `DocumentRow` for the `create_chunk_row` test above.
fn doc_row(doc: &Document) -> br8n::store::query::DocumentRow {
    br8n::store::query::DocumentRow {
        id: doc.id.clone(),
        uri: doc.uri.clone(),
        title: doc.title.clone(),
        source_type: doc.source_type.as_str().to_string(),
        content_hash: doc.content_hash.clone(),
        indexed_at: 0,
        meta: "{}".into(),
        tags: Vec::new(),
    }
}

/// The design's ladder (`src/pack/status.rs`'s module docs) says a document
/// with NO `status:` key lands on `Proposed` — the same rung as `proposed`,
/// `draft`, `shaping`, and anything unrecognised — and is lifted to
/// `Investigating` by decision 8's "used once" rule if something links to it.
///
/// `all_lifecycles` got this wrong: it `continue`s past any document whose
/// `meta` has no `status` key, which leaves that document ABSENT from the map
/// entirely. The caller's `unwrap_or_default()` then hands back `Current` —
/// the WRONG rung — for every note in the vault that carries no frontmatter,
/// which is most of them. This is invisible today only because `Current` and
/// `Proposed` both ship at multiplier 1.0.
#[test]
fn a_document_with_no_status_key_lands_on_proposed_not_current() {
    use br8n::pack::status::Lifecycle;

    let dir = tmpdb();
    let store = Store::open(dir.path(), 4).unwrap();

    // No `status` key at all — `meta` stays the literal `null` most documents
    // ship with.
    let bare = Document::new(SourceType::Markdown, "file:///bare.md", "Bare", "body");
    store.upsert_document(&bare).unwrap();

    // Same: no `status` key, but something links to it — decision 8's "used
    // once" must lift it from `Proposed` to `Investigating`.
    let linked = Document::new(SourceType::Markdown, "file:///linked.md", "Linked", "body");
    store.upsert_document(&linked).unwrap();
    let linker = Document::new(SourceType::Markdown, "file:///linker.md", "Linker", "body");
    store.upsert_document(&linker).unwrap();
    store
        .link_documents(&linker.id, &linked.id, "wikilink")
        .unwrap();

    let inbound = store.inbound_link_counts().unwrap();
    let lifecycles = store.all_lifecycles(&inbound).unwrap();

    assert_eq!(
        lifecycles.get(&bare.id).copied(),
        Some(Lifecycle::Proposed),
        "a document with no status key must map to Proposed, not be absent \
         from the map (which reads back as Current) — got {lifecycles:?}"
    );
    assert_eq!(
        lifecycles.get(&linked.id).copied(),
        Some(Lifecycle::Investigating),
        "a status-less document with an inbound wikilink must be lifted to \
         Investigating — got {lifecycles:?}"
    );
    // The `!= Current` filter that keeps the map small must still hold: the
    // linker itself declares no status and has no inbound link, so it is
    // Proposed too, not Current, and must still be present.
    assert_eq!(
        lifecycles.get(&linker.id).copied(),
        Some(Lifecycle::Proposed)
    );
}

#[test]
fn store_open_drops_a_legacy_chunk_vec_index() {
    let dir = tmpdb();

    {
        let store = Store::open(dir.path(), 4).unwrap();
        let doc = Document::new(SourceType::Markdown, "file:///a.md", "A", "hello world");
        store.upsert_document(&doc).unwrap();
        let chunk = Chunk {
            id: Chunk::id(&doc.id, 0),
            doc_id: doc.id.clone(),
            ord: 0,
            text: "hello world".into(),
            embed_text: "A > hello world".into(),
            heading_path: "A".into(),
            page_no: None,
        };
        store
            .insert_chunks(&doc.id, &[chunk], &[vec![1.0, 0.0, 0.0, 0.0]])
            .unwrap();
        assert!(
            !store.has_legacy_vector_index().unwrap(),
            "a freshly opened store must start with no chunk_vec index"
        );
    }

    {
        let db = lbug::Database::new(dir.path().join("graph.kz"), lbug::SystemConfig::default())
            .unwrap();
        let conn = lbug::Connection::new(&db).unwrap();
        let _ = conn.query("INSTALL vector");
        conn.query("LOAD EXTENSION vector").unwrap();
        conn.query("CALL CREATE_VECTOR_INDEX('Chunk','chunk_vec','embedding', metric := 'cosine')")
            .unwrap();
        let mut rows = conn.query("CALL SHOW_INDEXES() RETURN *").unwrap();
        assert!(
            rows.any(|row| row
                .get(1)
                .is_some_and(|v| format!("{v:?}").contains("chunk_vec"))),
            "test setup must actually create the legacy chunk_vec index"
        );
    }

    let store = Store::open(dir.path(), 4).unwrap();
    assert!(
        !store.has_legacy_vector_index().unwrap(),
        "Store::open must drop a legacy chunk_vec index found on disk — \
         leaving it risks a SIGSEGV the next time a chunk is written with no embedding"
    );
}

#[test]
fn one_process_can_hold_more_stores_open_than_lbugs_default_reservation_allows() {
    let dirs: Vec<tempfile::TempDir> = (0..32).map(|_| tempfile::tempdir().unwrap()).collect();
    let open: Vec<Store> = dirs
        .iter()
        .map(|d| Store::open(d.path(), 4).unwrap())
        .collect();
    assert_eq!(open.len(), 32);
}
