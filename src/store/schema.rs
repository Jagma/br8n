/// Schema version of the on-disk `Chunk` table, bumped whenever a change to
/// stored columns makes an existing index unreadable in the new shape rather
/// than merely additive. `Indexer::check_model` compares this the same way it
/// already compares `embed_model`, and demands `br8n index --reindex` on a
/// mismatch — an old index has no way to backfill a column that was dropped
/// (this version's change: `fts_body` is gone, and with it lbug's FTS index —
/// the pack serves BM25 now; see `Store::fts_search`), so it must be refused
/// rather than read wrongly.
pub const SCHEMA_VERSION: &str = "3";

/// DDL. `dims` must match the embedding model's output width — `embedding`
/// is a fixed-size FLOAT array.
pub fn ddl(dims: usize) -> Vec<String> {
    vec![
        "CREATE NODE TABLE IF NOT EXISTS Document(\
            id STRING PRIMARY KEY, uri STRING, title STRING, source_type STRING, \
            content_hash STRING, indexed_at INT64, meta STRING)"
            .into(),
        format!(
            "CREATE NODE TABLE IF NOT EXISTS Chunk(\
                id STRING PRIMARY KEY, doc_id STRING, ord INT64, text STRING, \
                embed_hash STRING, heading_path STRING, page_no INT64, \
                embedding FLOAT[{dims}])"
        ),
        "CREATE NODE TABLE IF NOT EXISTS Entity(id STRING PRIMARY KEY, name STRING, kind STRING)"
            .into(),
        "CREATE NODE TABLE IF NOT EXISTS Tag(name STRING PRIMARY KEY)".into(),
        "CREATE NODE TABLE IF NOT EXISTS Source(domain STRING PRIMARY KEY)".into(),
        "CREATE NODE TABLE IF NOT EXISTS IndexMeta(key STRING PRIMARY KEY, value STRING)".into(),
        "CREATE REL TABLE IF NOT EXISTS HAS_CHUNK(FROM Document TO Chunk)".into(),
        "CREATE REL TABLE IF NOT EXISTS NEXT_CHUNK(FROM Chunk TO Chunk)".into(),
        "CREATE REL TABLE IF NOT EXISTS LINKS_TO(FROM Document TO Document, kind STRING)".into(),
        "CREATE REL TABLE IF NOT EXISTS MENTIONS(FROM Chunk TO Entity)".into(),
        "CREATE REL TABLE IF NOT EXISTS TAGGED(FROM Document TO Tag)".into(),
        "CREATE REL TABLE IF NOT EXISTS DERIVED_FROM(FROM Document TO Source)".into(),
    ]
}

/// Removes the legacy HNSW index a pre-fix binary built. `chunk_vec` and a
/// NULL `embedding` in the same table are not merely inefficient — they are a
/// process-killing combination, and the only way to write one is to remove
/// the other. See `Store::drop_legacy_vector_index` for the mechanism.
///
/// Verified by spike against real lbug 0.19.1 rather than taken from
/// documentation: the call exists, it succeeds, `CALL SHOW_INDEXES()` stops
/// listing `chunk_vec` afterwards, and the index can be created again later
/// over the same table. Two facts that shape every
/// caller: it must go through `exec_literal` (`prepare` refuses it), and
/// dropping an index that is NOT there is an ERROR, not a no-op — so it must
/// be guarded by an existence check rather than fired blind.
pub const LEGACY_VEC_DROP: &str = "CALL DROP_VECTOR_INDEX('Chunk','chunk_vec')";

// lbug's FTS index (and the `fts_body` column it read) is gone as of schema
// version 3. It could not index `text` directly — the FTS extension's
// tokenizer drops the token immediately adjacent (on either side) to a
// literal '\n', and chunks routinely contain '\n' (the splitter merges a
// short heading with its body, e.g. "# Pooling\n\nPgBouncer runs..."
// verbatim) — so `fts_body` existed only to give it newline-free text to
// read. That whole path is retired now that the pack serves BM25 from its
// own impact-ordered postings (`src/pack/`), verified against lbug's own
// `stem(w,'porter')` over the live corpus with 0 mismatches across 37,358
// distinct token types. `Store::fts_search` errors, naming the pack, rather
// than silently returning nothing — see its doc comment.
