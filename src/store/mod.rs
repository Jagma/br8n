pub mod query;
pub mod schema;

pub use query::{Hit, StatusSnapshot};

use crate::model::{Chunk, Document};
use anyhow::{Context, Result};
use lbug::{LogicalType, Value};
use std::path::Path;

// Spike 1: the database path is a single FILE, not a directory. `remove_dir_all`
// silently no-ops on it; cleanup uses `remove_file`.
//
// Adaptation from the brief: `Store::open`'s `path` argument is the store's
// *data directory* (this is what `Config::db_path()` returns, and what
// `tests/it/store.rs` passes as `dir.path()` from `tempfile::tempdir()` — an
// already-existing directory). Passing that directory straight to
// `lbug::Database::new` fails at runtime with "Database path cannot be a
// directory", which is exactly the single-file constraint the spike found.
// So `path` is created as a directory and the actual lbug file lives at a
// fixed name inside it (`DB_FILE`), keeping the single-file constraint while
// giving callers a stable, directory-shaped path to point config/backup
// tooling at.
const DB_FILE: &str = "graph.kz";

const MAX_DB_BYTES: u64 = 1 << 40;

fn system_config() -> lbug::SystemConfig {
    lbug::SystemConfig::default().max_db_size(MAX_DB_BYTES)
}

// Adaptation from the brief: `lbug::Connection<'a>` borrows from `&'a Database`,
// which makes `{ _db: Database, conn: Connection }` self-referential and
// unrepresentable in safe Rust (E0106 missing lifetime specifier when compiled
// against real lbug 0.19.1). Fixed by heap-allocating the Database in a `Box`
// (a stable address that survives the Store being moved) and extending the
// borrow to `'static` with a documented unsafe block.
//
// A safe self-referential wrapper (`self_cell`/`ouroboros`) was evaluated and
// rejected: both would force `Store::exec` to stop returning
// `lbug::QueryResult<'static>` and instead hand results out only through a
// `with_dependent`-style closure, because `lbug::Connection<'a>` holds its FFI
// handle in an `UnsafeCell`, which makes it *invariant* over `'a`, not
// covariant — `self_cell`'s `#[covariant]` compile-time check
// (`_assert_covariance`) would reject it outright, and the `not_covariant`
// fallback cannot let a `QueryResult<'a>` escape its closure at all. Since
// every one of Tasks 15-23 calls `exec` expecting today's signature, that
// rewrite would ripple through every query method in this file for no
// soundness gain: no safe wrapper can manufacture an actually-`'static`
// borrow from a non-'static owner either — that's the same lifetime-extension
// this `unsafe` performs, just relocated.
//
// So the `unsafe` stays, and what used to be a comment-enforced invariant is
// now structurally enforced instead: both fields are wrapped in
// `ManuallyDrop` and dropped explicitly, in the required order, in `Drop`
// below. Drop order in Rust is normally *declaration order*, which a future
// field reorder could silently change with no compiler complaint. Routing
// both drops through an explicit `Drop` impl removes that dependency
// entirely — reordering the fields below can no longer change which one is
// dropped first, because nothing about drop order is derived from their
// declaration position anymore.
pub struct Store {
    conn: std::mem::ManuallyDrop<lbug::Connection<'static>>,
    _db: std::mem::ManuallyDrop<Box<lbug::Database>>,
}

impl Drop for Store {
    fn drop(&mut self) {
        // SAFETY: `conn` borrows from `_db` via the `'static` lifetime
        // manufactured in `connect` below, so `conn` must be dropped before
        // `_db`. Each field is wrapped in `ManuallyDrop`, so the compiler does
        // not auto-drop them (in whatever order it likes) after this method
        // returns; dropping them explicitly, in this order, here, is the only
        // place drop order is decided. `conn` must stay first in this list.
        unsafe {
            std::mem::ManuallyDrop::drop(&mut self.conn);
            std::mem::ManuallyDrop::drop(&mut self._db);
        }
    }
}

/// What a re-index actually wrote, so callers can report it and tests can pin it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChunkDelta {
    pub inserted: usize,
    pub deleted: usize,
    pub kept: usize,
}

impl Store {
    pub fn open(path: &Path, dims: usize) -> Result<Store> {
        std::fs::create_dir_all(path)
            .with_context(|| format!("create store directory {}", path.display()))?;
        let db = Box::new(
            lbug::Database::new(path.join(DB_FILE), system_config())
                .context("open ladybug database")?,
        );
        let conn = Store::connect(&db).context("open connection")?;
        let store = Store {
            conn: std::mem::ManuallyDrop::new(conn),
            _db: std::mem::ManuallyDrop::new(db),
        };
        store.load_extensions()?;
        store.init_schema(dims)?;
        store.drop_legacy_vector_index()?;
        Ok(store)
    }

    /// Open an index that already exists, creating nothing.
    ///
    /// Every reader must use this. `reindex_swap` renames the live directory
    /// away and back, and in that window `Store::open`'s `create_dir_all` left
    /// an empty database at the live path — which then made the swap's second
    /// rename fail with ENOTEMPTY, aborting it and leaving that empty database
    /// AS the live index. Readers now get a plain error and degrade to no
    /// results, which is what the hook already does with one.
    ///
    /// The existence check narrows the window rather than closing it outright;
    /// `reindex_swap` retries the rename to cover whatever still loses the race.
    ///
    /// This deliberately does NOT call `open`. A reader has no business running
    /// `create_dir_all`, `INSTALL`, or the 12 `CREATE ... IF NOT EXISTS`
    /// statements in `schema::ddl` — those are write-side setup, they cost
    /// ~21ms of every hook invocation, and DDL executed by a reader is a write
    /// against a database the reader does not hold the `IndexLock` for.
    pub fn open_existing(path: &Path, dims: usize) -> Result<Store> {
        // `dims` is unused on the read path and the parameter is kept only so
        // readers and writers open with the same signature. Nothing here
        // validates it: `check_model` folds dims into `model_id` and would
        // reject a mismatch, but its only caller is `Indexer::index_documents`
        // — the WRITE path. A reader opened against an index built at other
        // dimensions has no vector search of its own left to get wrong — that
        // now lives entirely in the pack, whose manifest check refuses a
        // dimension mismatch before any file is opened.
        let _ = dims;
        anyhow::ensure!(
            path.join(DB_FILE).exists(),
            "no index at {} — run `br8n index` first",
            path.display()
        );
        let db = Box::new(
            lbug::Database::new(path.join(DB_FILE), system_config())
                .context("open ladybug database")?,
        );
        let conn = Store::connect(&db).context("open connection")?;
        let store = Store {
            conn: std::mem::ManuallyDrop::new(conn),
            _db: std::mem::ManuallyDrop::new(db),
        };
        store.load_extensions_for_read()?;
        Ok(store)
    }

    /// Load — but do not install — the extensions a reader needs, and FAIL
    /// if they will not load.
    ///
    /// `load_extensions` swallows errors with `let _ =`, which is right on the
    /// write path (a fresh database installs them) and wrong here. Without the
    /// `vector` extension every `QUERY_VECTOR_INDEX` call errors, retrieval
    /// returns nothing, and the hook exits 0 — the exact shape of a silent
    /// pipeline break that is indistinguishable from an honest no-match. The
    /// dlopen'd extensions also need the export-dynamic linker flag; when that
    /// is missing this is the line that says so out loud.
    fn load_extensions_for_read(&self) -> Result<()> {
        self.exec("LOAD EXTENSION vector", vec![])
            .context("load the `vector` extension — retrieval cannot run without it")?;
        Ok(())
    }

    /// SAFETY: `db` is a `Box<Database>` owned by the `Store` being constructed
    /// and never reallocated (Box contents are heap-stable across moves of the
    /// Box itself). The returned `Connection<'static>` is stored in the same
    /// `Store` and, by field declaration order, is always dropped before `_db`
    /// — so the borrow this transmute manufactures is never actually
    /// outlived by its referent.
    fn connect(db: &lbug::Database) -> Result<lbug::Connection<'static>, lbug::Error> {
        let db_static: &'static lbug::Database = unsafe { &*(db as *const lbug::Database) };
        lbug::Connection::new(db_static)
    }

    /// THE query entrypoint. Every value reaches Cypher through `params`, never
    /// through `format!`. `cypher` must be a literal or built only from
    /// compile-time constants.
    pub(crate) fn exec(
        &self,
        cypher: &str,
        params: Vec<(&str, Value)>,
    ) -> Result<lbug::QueryResult<'static>> {
        let mut stmt = self
            .conn
            .prepare(cypher)
            .with_context(|| format!("prepare failed: {cypher}"))?;
        self.conn
            .execute(&mut stmt, params)
            .with_context(|| format!("execute failed: {cypher}"))
    }

    /// `CALL DROP_VECTOR_INDEX(...)` (like `CALL CREATE_VECTOR_INDEX(...)`
    /// before it) fails through `Connection::prepare` with "Connection
    /// Exception: We do not support prepare multiple statements", confirmed
    /// against real lbug 0.19.1 — `conn.query()` on the identical string
    /// succeeds. The `&'static str` parameter keeps this to callers passing a
    /// literal or a module constant, which is what every current caller does.
    fn exec_literal(&self, cypher: &'static str) -> Result<lbug::QueryResult<'static>> {
        self.conn
            .query(cypher)
            .with_context(|| format!("query failed: {cypher}"))
    }

    fn load_extensions(&self) -> Result<()> {
        // Verified in Spike 1: the syntax is `LOAD EXTENSION <name>`, and each
        // statement must be its own call — a single `;`-separated string silently
        // runs only the first. Requires -Wl,-export_dynamic (see Global Constraints).
        // `fts` is gone: nothing builds or queries lbug's FTS index any more —
        // the pack serves BM25 now (see `schema.rs`'s comment on the retired
        // `FTS_INDEX`/`FTS_DROP` constants). Dropping this extension from the
        // WRITE path is safe only because of a gate upstream, not because an
        // unloaded index is harmless: in lbug 0.19.1 an INSERT or DELETE
        // against a table carrying an index whose extension is not loaded
        // throws, and every schema-version-2 database still carries
        // `chunk_fts`. `Indexer::check_model` refuses a v2 database before
        // the first `Chunk` write, so a v2 index hits that reindex message
        // instead of the lbug exception — a caller that writes to the store
        // without going through `check_model` first would need to re-check
        // this.
        for stmt in ["INSTALL vector", "LOAD EXTENSION vector"] {
            let _ = self.exec(stmt, vec![]);
        }
        Ok(())
    }

    pub fn init_schema(&self, dims: usize) -> Result<()> {
        // `dims` is a usize from config, not user text — safe to format, and DDL
        // cannot take a parameter for an array width.
        for stmt in schema::ddl(dims) {
            self.exec(&stmt, vec![])
                .with_context(|| format!("ddl: {stmt}"))?;
        }
        Ok(())
    }

    /// Whether `chunk_vec` is on the `Chunk` table right now.
    ///
    /// Answered by `CALL SHOW_INDEXES()`, whose columns were verified against
    /// real lbug 0.19.1: `[table, index_name, type, properties, ...]`. Not
    /// cached — this runs at most once per `Store::open`, not once per write.
    pub fn has_legacy_vector_index(&self) -> Result<bool> {
        let mut rows = self.exec("CALL SHOW_INDEXES() RETURN *", vec![])?;
        Ok(rows.any(|row| {
            row.first().and_then(as_string).as_deref() == Some("Chunk")
                && row.get(1).and_then(as_string).as_deref() == Some("chunk_vec")
        }))
    }

    fn drop_legacy_vector_index(&self) -> Result<()> {
        if !self.has_legacy_vector_index()? {
            return Ok(());
        }
        self.exec_literal(schema::LEGACY_VEC_DROP).context(
            "could not drop the legacy `chunk_vec` vector index found on this database — \
             leaving it in place risks a SIGSEGV the next time a chunk is written with no \
             embedding (see `schema::LEGACY_VEC_DROP`)",
        )?;
        Ok(())
    }

    /// Replaces the Document node and re-links its own edges (`TAGGED`) — it no
    /// longer deletes this document's chunks first. Chunk lifetime belongs to
    /// `replace_chunks`, which is why this went from calling `delete_document`
    /// (whose first step is `DETACH DELETE` on every chunk) to a narrower
    /// delete that touches only the `Document` node. `upsert_document` used to
    /// throw every chunk away on every call, so a transcript that gained one
    /// message rewrote all ~50 of its chunks; lbug cannot reclaim the space the
    /// old rows occupied, so the file grew by the whole document every run.
    ///
    /// `DETACH DELETE d` below still removes every edge touching the OLD
    /// Document node, including the `HAS_CHUNK` edges to chunks that are about
    /// to be kept unchanged — it just leaves the Chunk nodes themselves alone.
    /// `replace_chunks` restores the `HAS_CHUNK` edge for every kept chunk (see
    /// its comment) precisely to repair that severance; without it, a kept
    /// chunk's row and vector would survive but become unreachable from the
    /// document, which every retrieval query joins through.
    pub fn upsert_document(&self, d: &Document) -> Result<()> {
        // Capture the INBOUND links before the delete, and restore them after.
        //
        // `DETACH DELETE d` removes edges in BOTH directions. Outbound ones are
        // meant to go — this document's own links, tags and source are rebuilt
        // from `d` on the way back. Inbound ones are collateral damage: they
        // belong to OTHER documents, they are not this call's to discard, and
        // nothing else rebuilds them. `resolve_links` writes edges in
        // `for d in docs` over the documents it is handed, and the incremental
        // path hands it only the CHANGED ones — so an unchanged A that links to
        // a changed B lost `A -> B` on B's re-index and never got it back.
        //
        // Measured on the live index before this: the vault held 67 resolvable
        // inbound wikilinks and the store held 44 — 23 destroyed, and the loss
        // was monotonic, since every re-index of a linked-to document dropped
        // more. `br8n add` already carried a fix for the same defect by
        // re-resolving against the whole corpus (see `src/main.rs`); doing that
        // on the incremental path would mean a full `discover`, re-parsing every
        // source file, which is exactly the cost the stat fingerprint exists to
        // avoid. Repairing the edge here is O(inbound) and needs no re-parse.
        //
        // This is the same shape, and the same remedy, as the `HAS_CHUNK`
        // severance `replace_chunks` repairs with `MERGE` — see its comment.
        // `LINKS_TO` is the only edge table with `TO Document`, so it is the
        // only inbound kind there is to save.
        let inbound = string_pairs(self.exec(
            "MATCH (a:Document)-[r:LINKS_TO]->(:Document {id: $id}) RETURN a.id, r.kind",
            vec![("id", Value::String(d.id.clone()))],
        )?);
        self.exec(
            "MATCH (d:Document {id: $id}) DETACH DELETE d",
            vec![("id", Value::String(d.id.clone()))],
        )?;
        let meta = serde_json::to_string(&d.meta)?;
        self.exec(
            "CREATE (d:Document {id: $id, uri: $uri, title: $title, \
             source_type: $st, content_hash: $hash, indexed_at: $at, meta: $meta})",
            vec![
                ("id", Value::String(d.id.clone())),
                ("uri", Value::String(d.uri.clone())),
                ("title", Value::String(d.title.clone())),
                ("st", Value::String(d.source_type.as_str().to_string())),
                ("hash", Value::String(d.content_hash.clone())),
                ("at", Value::Int64(now_secs())),
                ("meta", Value::String(meta)),
            ],
        )?;
        for t in &d.tags {
            self.exec(
                "MERGE (:Tag {name: $name})",
                vec![("name", Value::String(t.clone()))],
            )?;
            self.exec(
                "MATCH (d:Document {id: $id}), (t:Tag {name: $name}) CREATE (d)-[:TAGGED]->(t)",
                vec![
                    ("id", Value::String(d.id.clone())),
                    ("name", Value::String(t.clone())),
                ],
            )?;
        }
        // Restore what the delete took from other documents. `MERGE`, not
        // `CREATE`: the source document may be re-indexed in the same run and
        // re-resolve the same link itself, and a second `CREATE` would leave two
        // identical edges — the duplicate-edge defect `link_documents` is
        // already `MERGE` to avoid.
        for (from, kind) in inbound {
            self.link_documents(&from, &d.id, &kind)?;
        }
        Ok(())
    }

    /// `vecs[i]` empty (`len() == 0`) is the sentinel for "no embedding yet" —
    /// phase 1 of an asynchronous index (`br8n index --no-embed`) writes
    /// every new chunk this way, and `Store::chunks_without_vectors` finds
    /// them again later by the `c.embedding IS NULL` this produces. A real
    /// embedding is never zero-length (the embedder always returns exactly
    /// `dims` floats), so the two cases cannot collide.
    ///
    /// The `embedding` property is OMITTED from the CREATE map entirely in
    /// that case, rather than set to some placeholder value: verified
    /// against real lbug 0.19.1 that an omitted property on `CREATE` reads
    /// back as NULL, and that `all_rows_for_pack`'s `as_f32_vec` (which
    /// matches only `Value::Array`/`Value::List`) already treats that NULL
    /// as "no vector" — the same predicate `chunks_without_vectors` queries
    /// with `IS NULL`.
    pub fn insert_chunks(&self, doc_id: &str, chunks: &[Chunk], vecs: &[Vec<f32>]) -> Result<()> {
        anyhow::ensure!(chunks.len() == vecs.len(), "chunk/vector count mismatch");
        for (c, v) in chunks.iter().zip(vecs) {
            let ehash = crate::model::Document::content_hash(&c.embed_text);
            if v.is_empty() {
                self.exec(
                    "CREATE (c:Chunk {id: $id, doc_id: $doc, ord: $ord, text: $text, \
                     embed_hash: $ehash, heading_path: $hp, page_no: $page})",
                    vec![
                        ("id", Value::String(c.id.clone())),
                        ("doc", Value::String(c.doc_id.clone())),
                        ("ord", Value::Int64(c.ord)),
                        ("text", Value::String(c.text.clone())),
                        ("ehash", Value::String(ehash)),
                        ("hp", Value::String(c.heading_path.clone())),
                        ("page", Value::Int64(c.page_no.unwrap_or(-1))),
                    ],
                )?;
            } else {
                self.exec(
                    "CREATE (c:Chunk {id: $id, doc_id: $doc, ord: $ord, text: $text, \
                     embed_hash: $ehash, heading_path: $hp, page_no: $page, \
                     embedding: $emb})",
                    vec![
                        ("id", Value::String(c.id.clone())),
                        ("doc", Value::String(c.doc_id.clone())),
                        ("ord", Value::Int64(c.ord)),
                        ("text", Value::String(c.text.clone())),
                        ("ehash", Value::String(ehash)),
                        ("hp", Value::String(c.heading_path.clone())),
                        ("page", Value::Int64(c.page_no.unwrap_or(-1))),
                        ("emb", float_array(v)),
                    ],
                )?;
            }
            self.exec(
                "MATCH (d:Document {id: $doc}), (c:Chunk {id: $id}) CREATE (d)-[:HAS_CHUNK]->(c)",
                vec![
                    ("doc", Value::String(doc_id.to_string())),
                    ("id", Value::String(c.id.clone())),
                ],
            )?;
        }
        // NEXT_CHUNK adjacency replaces text overlap.
        for w in chunks.windows(2) {
            self.exec(
                "MATCH (a:Chunk {id: $a}), (b:Chunk {id: $b}) CREATE (a)-[:NEXT_CHUNK]->(b)",
                vec![
                    ("a", Value::String(w[0].id.clone())),
                    ("b", Value::String(w[1].id.clone())),
                ],
            )?;
        }
        Ok(())
    }

    /// Fills in the vector for a chunk published with none — phase 2 of an
    /// asynchronous index (`backfill_vectors` in `index.rs`). Touches only
    /// `embedding`; text, hashes and every edge are left exactly as phase 1
    /// wrote them.
    ///
    /// Verified against real lbug 0.19.1 (not merely assumed from the CREATE
    /// side): `SET` on a row whose `embedding` was NULL persists across a
    /// fresh `Store::open`, which is what lets `backfill_vectors` close and
    /// reopen the store between batches without losing the update.
    pub fn set_chunk_embedding(&self, chunk_id: &str, v: &[f32]) -> Result<()> {
        self.exec(
            "MATCH (c:Chunk {id: $id}) SET c.embedding = $emb",
            vec![
                ("id", Value::String(chunk_id.to_string())),
                ("emb", float_array(v)),
            ],
        )?;
        Ok(())
    }

    /// Deletes one chunk and every edge touching it (`HAS_CHUNK`, `NEXT_CHUNK`,
    /// `MENTIONS`). `DETACH DELETE` is required, not optional: a chunk removed
    /// without its edges leaves dangling relationships that `expand` would
    /// still traverse.
    pub fn delete_chunk(&self, id: &str) -> Result<()> {
        self.exec(
            "MATCH (c:Chunk {id: $id}) DETACH DELETE c",
            vec![("id", Value::String(id.to_string()))],
        )?;
        Ok(())
    }

    /// Write only the chunks that changed.
    ///
    /// `upsert_document` used to delete every chunk of the document and the
    /// re-insert wrote them all back under the same ids. lbug cannot reclaim the
    /// space the old rows occupied — `CHECKPOINT` frees 0 bytes and `VACUUM` does
    /// not exist — so the file grew by the whole document on every run. Measured on
    /// the live index: ~584 KB of file per net-new chunk, against ~6 KB of content.
    ///
    /// A chunk is unchanged when its id is present and the hash of its `embed_text`
    /// matches. That is the same text the embedding was computed from, so an
    /// unchanged hash means both the row and its vector are still correct.
    pub fn replace_chunks(
        &self,
        doc_id: &str,
        chunks: &[Chunk],
        vecs: &[Vec<f32>],
    ) -> Result<ChunkDelta> {
        anyhow::ensure!(chunks.len() == vecs.len(), "chunk/vector count mismatch");
        let existing = self.chunk_hashes(doc_id)?;
        let mut delta = ChunkDelta::default();

        let incoming: std::collections::HashSet<&str> =
            chunks.iter().map(|c| c.id.as_str()).collect();
        for id in existing.keys() {
            if !incoming.contains(id.as_str()) {
                self.delete_chunk(id)?;
                delta.deleted += 1;
            }
        }

        for (c, v) in chunks.iter().zip(vecs) {
            let want = crate::model::Document::content_hash(&c.embed_text);
            match existing.get(&c.id) {
                Some(have) if *have == want => {
                    // The row and its vector are still correct, but
                    // `upsert_document`'s `DETACH DELETE` on the old Document
                    // node severed the `HAS_CHUNK` edge to this chunk along
                    // with everything else — restore it (idempotently; `MERGE`
                    // rather than `CREATE`, so a chunk kept across several
                    // runs in a row never accumulates duplicate edges).
                    self.exec(
                        "MATCH (d:Document {id: $doc}), (c:Chunk {id: $id}) \
                         MERGE (d)-[:HAS_CHUNK]->(c)",
                        vec![
                            ("doc", Value::String(doc_id.to_string())),
                            ("id", Value::String(c.id.clone())),
                        ],
                    )?;
                    delta.kept += 1;
                    continue;
                }
                Some(_) => {
                    self.delete_chunk(&c.id)?;
                    delta.deleted += 1;
                }
                None => {}
            }
            self.insert_chunks(doc_id, std::slice::from_ref(c), std::slice::from_ref(v))?;
            delta.inserted += 1;
        }

        // NEXT_CHUNK adjacency depends on neighbours that may not have
        // changed, so it is rebuilt from the full final chunk list rather
        // than the changed subset — a chunk that kept its own hash can still
        // have gained or lost a neighbour.
        self.exec(
            "MATCH (:Chunk {doc_id: $doc})-[r:NEXT_CHUNK]->(:Chunk) DELETE r",
            vec![("doc", Value::String(doc_id.to_string()))],
        )?;
        for w in chunks.windows(2) {
            self.exec(
                "MATCH (a:Chunk {id: $a}), (b:Chunk {id: $b}) CREATE (a)-[:NEXT_CHUNK]->(b)",
                vec![
                    ("a", Value::String(w[0].id.clone())),
                    ("b", Value::String(w[1].id.clone())),
                ],
            )?;
        }

        Ok(delta)
    }

    /// Creates a `Document` node with EXACTLY the given fields, including
    /// `indexed_at` — which `upsert_document` stamps with `now_secs()` on
    /// every call, but which compaction must carry across unchanged, since
    /// copying an existing generation is not a new indexing event.
    ///
    /// Unlike `upsert_document`, this does not `DETACH DELETE` first: it
    /// assumes the destination has no node with this id yet, which is true
    /// only for a fresh compaction shadow. Tags are recreated as `TAGGED`
    /// edges the same way `upsert_document` creates them.
    pub fn create_document_row(&self, row: &query::DocumentRow) -> Result<()> {
        self.exec(
            "CREATE (d:Document {id: $id, uri: $uri, title: $title, source_type: $st, \
             content_hash: $hash, indexed_at: $at, meta: $meta})",
            vec![
                ("id", Value::String(row.id.clone())),
                ("uri", Value::String(row.uri.clone())),
                ("title", Value::String(row.title.clone())),
                ("st", Value::String(row.source_type.clone())),
                ("hash", Value::String(row.content_hash.clone())),
                ("at", Value::Int64(row.indexed_at)),
                ("meta", Value::String(row.meta.clone())),
            ],
        )?;
        for t in &row.tags {
            self.exec(
                "MERGE (:Tag {name: $name})",
                vec![("name", Value::String(t.clone()))],
            )?;
            self.exec(
                "MATCH (d:Document {id: $id}), (t:Tag {name: $name}) CREATE (d)-[:TAGGED]->(t)",
                vec![
                    ("id", Value::String(row.id.clone())),
                    ("name", Value::String(t.clone())),
                ],
            )?;
        }
        Ok(())
    }

    /// Creates a `Chunk` node with the EXACT stored `embed_hash` and
    /// `embedding` — never recomputed. A normal `insert_chunks` derives
    /// `embed_hash` from `embed_text`, but that field is not stored anywhere
    /// as of schema version 3 (see `schema.rs`), so a row read back from the
    /// store (`Store::all_chunk_rows`) has no text left to recompute it from;
    /// carrying the hash across is the only correct option, and it is also
    /// the point of compaction — no embedding is ever recomputed either.
    ///
    /// Also creates the `HAS_CHUNK` edge from `row.doc_id`. `NEXT_CHUNK`
    /// adjacency is NOT created here — it is rebuilt separately, from the
    /// edges read off the live store (`Store::all_next_chunk_edges`), not
    /// re-derived from insertion order.
    pub fn create_chunk_row(&self, row: &query::ChunkRow) -> Result<()> {
        if row.embedding.is_empty() {
            // Compaction reads its rows back from a live store, and that
            // store may still be mid-backfill (`br8n index --compact` is not
            // gated on the backlog being empty), so a row with no embedding
            // reaches here. The property is OMITTED, the same way
            // `insert_chunks` omits it, rather than handed `float_array(&[])`.
            // That is not a silent wrong value: lbug 0.19.1 REFUSES it
            // outright with `Binder exception: Cannot change parameter
            // expression data type from FLOAT[0] to FLOAT[4]` (observed while
            // mutation-testing
            // `create_chunk_row_writes_a_missing_embedding_as_null_not_an_
            // empty_array`). So the old code could not copy such a row at
            // all; this branch is what lets it be copied, as the NULL that
            // `chunks_without_vectors` looks for, keeping the backfill
            // backlog intact across the copy.
            self.exec(
                "CREATE (c:Chunk {id: $id, doc_id: $doc, ord: $ord, text: $text, \
                 embed_hash: $ehash, heading_path: $hp, page_no: $page})",
                vec![
                    ("id", Value::String(row.id.clone())),
                    ("doc", Value::String(row.doc_id.clone())),
                    ("ord", Value::Int64(row.ord)),
                    ("text", Value::String(row.text.clone())),
                    ("ehash", Value::String(row.embed_hash.clone())),
                    ("hp", Value::String(row.heading_path.clone())),
                    ("page", Value::Int64(row.page_no.unwrap_or(-1))),
                ],
            )?;
        } else {
            self.exec(
                "CREATE (c:Chunk {id: $id, doc_id: $doc, ord: $ord, text: $text, \
                 embed_hash: $ehash, heading_path: $hp, page_no: $page, embedding: $emb})",
                vec![
                    ("id", Value::String(row.id.clone())),
                    ("doc", Value::String(row.doc_id.clone())),
                    ("ord", Value::Int64(row.ord)),
                    ("text", Value::String(row.text.clone())),
                    ("ehash", Value::String(row.embed_hash.clone())),
                    ("hp", Value::String(row.heading_path.clone())),
                    ("page", Value::Int64(row.page_no.unwrap_or(-1))),
                    ("emb", float_array(&row.embedding)),
                ],
            )?;
        }
        self.exec(
            "MATCH (d:Document {id: $doc}), (c:Chunk {id: $id}) CREATE (d)-[:HAS_CHUNK]->(c)",
            vec![
                ("doc", Value::String(row.doc_id.clone())),
                ("id", Value::String(row.id.clone())),
            ],
        )?;
        Ok(())
    }

    /// Creates one `NEXT_CHUNK` edge between two chunks that already exist.
    /// Compaction uses this to rebuild adjacency from the pairs read off the
    /// live store (`Store::all_next_chunk_edges`) rather than re-deriving it
    /// from ordinal order, so a sequence with a gap is reproduced exactly.
    pub fn link_next_chunk(&self, from: &str, to: &str) -> Result<()> {
        self.exec(
            "MATCH (a:Chunk {id: $a}), (b:Chunk {id: $b}) CREATE (a)-[:NEXT_CHUNK]->(b)",
            vec![
                ("a", Value::String(from.to_string())),
                ("b", Value::String(to.to_string())),
            ],
        )?;
        Ok(())
    }

    pub fn delete_document(&self, id: &str) -> Result<()> {
        self.exec(
            "MATCH (:Document {id: $id})-[:HAS_CHUNK]->(c:Chunk) DETACH DELETE c",
            vec![("id", Value::String(id.to_string()))],
        )?;
        self.exec(
            "MATCH (d:Document {id: $id}) DETACH DELETE d",
            vec![("id", Value::String(id.to_string()))],
        )?;
        Ok(())
    }

    pub fn doc_hash(&self, id: &str) -> Result<Option<String>> {
        let r = self.exec(
            "MATCH (d:Document {id: $id}) RETURN d.content_hash",
            vec![("id", Value::String(id.to_string()))],
        )?;
        Ok(first_string(r))
    }

    /// Every document's content hash, in one query.
    ///
    /// The indexer needs this for all documents before it can decide which to
    /// skip. Asking per document (`doc_hash`) forced that decision onto the
    /// thread that owns the store, which serialised the whole pipeline behind
    /// a lookup that is one query for the entire corpus.
    pub fn all_doc_hashes(&self) -> Result<std::collections::HashMap<String, String>> {
        let mut out = std::collections::HashMap::new();
        let rows = match self.exec("MATCH (d:Document) RETURN d.id, d.content_hash", vec![]) {
            Ok(r) => r,
            Err(_) => return Ok(out),
        };
        for row in rows {
            if let (Some(id), Some(h)) = (
                row.first().and_then(as_string),
                row.get(1).and_then(as_string),
            ) {
                out.insert(id, h);
            }
        }
        Ok(out)
    }

    pub fn all_doc_uris(&self) -> Result<Vec<(String, String)>> {
        let r = self.exec("MATCH (d:Document) RETURN d.id, d.uri", vec![])?;
        Ok(string_pairs(r))
    }

    /// Every indexed document's `(id, title, uri)`, for building the wikilink
    /// lookup in `Indexer::resolve_links` from the whole corpus rather than from
    /// whatever slice the caller happened to pass. Takes no parameters, but still
    /// goes through `exec` like every other query.
    pub fn all_doc_keys(&self) -> Result<Vec<(String, String, String)>> {
        let r = self.exec("MATCH (d:Document) RETURN d.id, d.title, d.uri", vec![])?;
        Ok(r.filter_map(|row| {
            Some((
                as_string(row.first()?)?,
                as_string(row.get(1)?)?,
                as_string(row.get(2)?)?,
            ))
        })
        .collect())
    }

    pub fn next_chunk(&self, chunk_id: &str) -> Result<Option<String>> {
        let r = self.exec(
            "MATCH (:Chunk {id: $id})-[:NEXT_CHUNK]->(n:Chunk) RETURN n.id",
            vec![("id", Value::String(chunk_id.to_string()))],
        )?;
        Ok(first_string(r))
    }

    pub fn set_meta(&self, k: &str, v: &str) -> Result<()> {
        self.exec(
            "MATCH (m:IndexMeta {key: $k}) DELETE m",
            vec![("k", Value::String(k.to_string()))],
        )?;
        self.exec(
            "CREATE (:IndexMeta {key: $k, value: $v})",
            vec![
                ("k", Value::String(k.to_string())),
                ("v", Value::String(v.to_string())),
            ],
        )?;
        Ok(())
    }

    pub fn get_meta(&self, k: &str) -> Result<Option<String>> {
        let r = self.exec(
            "MATCH (m:IndexMeta {key: $k}) RETURN m.value",
            vec![("k", Value::String(k.to_string()))],
        )?;
        Ok(first_string(r))
    }

    pub fn count_documents(&self) -> Result<i64> {
        Ok(first_i64(self.exec("MATCH (d:Document) RETURN count(d)", vec![])?).unwrap_or(0))
    }

    pub fn count_chunks(&self) -> Result<i64> {
        Ok(first_i64(self.exec("MATCH (c:Chunk) RETURN count(c)", vec![])?).unwrap_or(0))
    }

    /// Idempotent by design. `resolve_links` runs over EVERY document on every
    /// index run, including ones skipped by the content hash — and `upsert_document`
    /// only clears edges for documents that actually changed. With `CREATE`, a
    /// user whose SessionStart hook indexes daily accumulates one duplicate edge
    /// per wikilink per session, without bound. Measured: 4 index runs produced
    /// 4 identical LINKS_TO edges between the same pair.
    pub fn link_documents(&self, from: &str, to: &str, kind: &str) -> Result<()> {
        self.exec(
            "MATCH (a:Document {id: $from}), (b:Document {id: $to}) \
             MERGE (a)-[:LINKS_TO {kind: $kind}]->(b)",
            vec![
                ("from", Value::String(from.to_string())),
                ("to", Value::String(to.to_string())),
                ("kind", Value::String(kind.to_string())),
            ],
        )?;
        Ok(())
    }

    /// Documents this one links to, for ONE edge kind.
    ///
    /// `kind` is required rather than defaulted because the question is
    /// genuinely ambiguous now: `LINKS_TO` carries prose wikilinks and the
    /// lifecycle relations the loader materialises, and a caller that does not
    /// say which it means is asking a question with two answers. Before those
    /// relations existed this took no parameter and eleven assertions in
    /// `tests/it/index_graph.rs` read "A links to B" — true then, and quietly
    /// wrong afterwards.
    ///
    /// No production caller today; this is test surface, and the reason to fix
    /// it anyway is that a test asserting the wrong thing is worse than no
    /// test. `inbound_link_counts` settled the same question the same way for
    /// the reader that IS on the retrieval path.
    pub fn linked_docs(&self, id: &str, kind: &str) -> Result<Vec<String>> {
        let r = self.exec(
            "MATCH (:Document {id: $id})-[r:LINKS_TO]->(b:Document) \
             WHERE r.kind = $kind RETURN b.id",
            vec![
                ("id", Value::String(id.to_string())),
                ("kind", Value::String(kind.to_string())),
            ],
        )?;
        Ok(r.filter_map(|row| as_string(row.first()?)).collect())
    }

    /// `MERGE`s the `Source` node so repeated calls for the same domain don't
    /// duplicate it, then creates a `DERIVED_FROM` edge from the document.
    pub fn attach_source(&self, doc_id: &str, domain: &str) -> Result<()> {
        self.exec(
            "MERGE (:Source {domain: $domain})",
            vec![("domain", Value::String(domain.to_string()))],
        )?;
        self.exec(
            "MATCH (d:Document {id: $id}), (s:Source {domain: $domain}) \
             MERGE (d)-[:DERIVED_FROM]->(s)",
            vec![
                ("id", Value::String(doc_id.to_string())),
                ("domain", Value::String(domain.to_string())),
            ],
        )?;
        Ok(())
    }

    pub fn source_domain(&self, doc_id: &str) -> Result<Option<String>> {
        let r = self.exec(
            "MATCH (:Document {id: $id})-[:DERIVED_FROM]->(s:Source) RETURN s.domain",
            vec![("id", Value::String(doc_id.to_string()))],
        )?;
        Ok(first_string(r))
    }

    /// `MERGE`s the `Entity` node (keyed by a deterministic `kind:name` id so
    /// repeated mentions of the same entity collapse onto one node), then
    /// creates a `MENTIONS` edge from the chunk.
    pub fn mention_entity(&self, chunk_id: &str, name: &str, kind: &str) -> Result<()> {
        let eid = format!("{kind}:{name}");
        self.exec(
            "MERGE (:Entity {id: $eid, name: $name, kind: $kind})",
            vec![
                ("eid", Value::String(eid.clone())),
                ("name", Value::String(name.to_string())),
                ("kind", Value::String(kind.to_string())),
            ],
        )?;
        self.exec(
            "MATCH (c:Chunk {id: $cid}), (e:Entity {id: $eid}) MERGE (c)-[:MENTIONS]->(e)",
            vec![
                ("cid", Value::String(chunk_id.to_string())),
                ("eid", Value::String(eid)),
            ],
        )?;
        Ok(())
    }
}

/// Builds the FLOAT[N] value for an embedding. Verified in Spike 1: it is
/// `Value::Array` with an explicit LogicalType discriminant, NOT `Value::List`.
pub(crate) fn float_array(v: &[f32]) -> Value {
    Value::Array(
        LogicalType::Float,
        v.iter().map(|f| Value::Float(*f)).collect(),
    )
}

/// Read a stored `FLOAT[N]` back into a Rust vector.
///
/// The write side (`float_array`) is `Value::Array`; the read side has to
/// accept `Value::List` too, because the binding does not guarantee which
/// variant a projected array column comes back as.
pub(crate) fn as_f32_vec(v: &Value) -> Option<Vec<f32>> {
    let items = match v {
        Value::Array(_, items) | Value::List(_, items) => items,
        _ => return None,
    };
    items
        .iter()
        .map(|x| match x {
            Value::Float(f) => Some(*f),
            Value::Double(d) => Some(*d as f32),
            _ => None,
        })
        .collect()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// Row extraction. Verified in Spike 1: `QueryResult` is
// `Iterator<Item = Vec<Value>>`. Rows are INDEXED and PATTERN MATCHED — there is
// no `row.get(n)?.as_string()` accessor. These four helpers are the only place
// that shape is known.
pub(crate) fn as_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

pub(crate) fn as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Int64(i) => Some(*i),
        _ => None,
    }
}

pub(crate) fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Double(d) => Some(*d),
        Value::Float(f) => Some(*f as f64),
        Value::Int64(i) => Some(*i as f64),
        _ => None,
    }
}

fn first_string(mut r: lbug::QueryResult<'_>) -> Option<String> {
    r.next().and_then(|row| row.first().and_then(as_string))
}

fn first_i64(mut r: lbug::QueryResult<'_>) -> Option<i64> {
    r.next().and_then(|row| row.first().and_then(as_i64))
}

fn string_pairs(r: lbug::QueryResult<'_>) -> Vec<(String, String)> {
    r.filter_map(|row| Some((as_string(row.first()?)?, as_string(row.get(1)?)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    // FTS has no equivalent here any more: schema version 3 dropped
    // `fts_body` and lbug's FTS index along with it (the pack serves BM25
    // now), so `Store::fts_search` errors outright rather than querying an
    // index this store no longer builds — see its own tests.
    use super::*;
    use crate::model::{Chunk, Document, SourceType};

    /// `backfill_vectors` (`index.rs`) is resumable ONLY because
    /// `chunks_without_vectors` re-derives its answer from the store on every
    /// call — never from anything held in memory across calls. This pins
    /// that directly: embed one of three pending chunks, then confirm the
    /// VERY NEXT call reflects it, without any store reopen in between.
    ///
    /// Mutation-tested by hand: replacing the query in
    /// `chunks_without_vectors` with a `once_cell`-style cache computed on
    /// the first call and reused after (returning the same 3 chunks both
    /// times, ignoring the `set_chunk_embedding` in between) makes this test
    /// fail its second assertion (`after.len()` stays 3, not 2) — a clean,
    /// fast, deterministic failure with no test-runner hang. See the task 3
    /// report for what was actually run.
    #[test]
    fn chunks_without_vectors_reflects_a_write_made_since_the_last_call() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 4).unwrap();
        let doc = Document::new(SourceType::Markdown, "file:///a.md", "A", "hello");
        store.upsert_document(&doc).unwrap();
        let chunks: Vec<Chunk> = (0..3)
            .map(|i| Chunk {
                id: Chunk::id(&doc.id, i),
                doc_id: doc.id.clone(),
                ord: i,
                text: format!("chunk {i}"),
                embed_text: format!("chunk {i}"),
                heading_path: "".into(),
                page_no: None,
            })
            .collect();
        // The empty-vector sentinel: published with no embedding, exactly
        // like `br8n index --no-embed` writes a real chunk.
        let vecs = vec![Vec::new(); chunks.len()];
        store.insert_chunks(&doc.id, &chunks, &vecs).unwrap();

        let before = store.chunks_without_vectors(10).unwrap();
        assert_eq!(before.len(), 3, "all three must start pending");

        // Embed exactly one, the way one batch of `backfill_vectors` does.
        store
            .set_chunk_embedding(&before[0].0, &[1.0, 0.0, 0.0, 0.0])
            .unwrap();

        let after = store.chunks_without_vectors(10).unwrap();
        assert_eq!(
            after.len(),
            2,
            "chunks_without_vectors must re-derive the backlog from the \
             store on every call, not cache it — a cached answer would still \
             report 3 pending here"
        );
        assert!(
            !after.iter().any(|(id, _)| id == &before[0].0),
            "the chunk that was just embedded must not still be reported pending"
        );
    }

    /// `link_documents`'s doc comment explains why its edge uses `MERGE`, not
    /// `CREATE`: unbounded duplicate accumulation across repeated `br8n
    /// index` runs is the exact defect that made `MERGE` necessary. That
    /// guarantee is tested for `LINKS_TO`
    /// (`resolve_links_is_idempotent_across_repeated_index_runs` in
    /// `tests/it/index_graph.rs`), but `attach_source`'s `DERIVED_FROM` edge and
    /// `mention_entity`'s `MENTIONS` edge use the identical `MERGE` pattern
    /// with no equivalent guard. This test counts edges directly via `exec`
    /// (available here, inside the crate, unlike from an integration test)
    /// rather than through a public accessor that only proves "at least one
    /// exists".
    #[test]
    fn attach_source_and_mention_entity_are_idempotent_across_repeated_runs() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 4).unwrap();

        let doc = Document::new(SourceType::Web, "https://example.com/x", "X", "body");
        store.upsert_document(&doc).unwrap();
        let chunk = Chunk {
            id: Chunk::id(&doc.id, 0),
            doc_id: doc.id.clone(),
            ord: 0,
            text: "hello".into(),
            embed_text: "hello".into(),
            heading_path: "".into(),
            page_no: None,
        };
        store
            .insert_chunks(
                &doc.id,
                std::slice::from_ref(&chunk),
                &[vec![1.0, 0.0, 0.0, 0.0]],
            )
            .unwrap();

        for _ in 0..4 {
            store.attach_source(&doc.id, "example.com").unwrap();
            store.mention_entity(&chunk.id, "Alice", "person").unwrap();
        }

        let mut derived = store
            .exec(
                "MATCH (:Document {id: $id})-[r:DERIVED_FROM]->(:Source) RETURN count(r)",
                vec![("id", Value::String(doc.id.clone()))],
            )
            .unwrap();
        assert_eq!(
            derived.next().and_then(|row| row.first().and_then(as_i64)),
            Some(1),
            "repeated attach_source calls must not accumulate duplicate DERIVED_FROM edges"
        );

        let mut mentions = store
            .exec(
                "MATCH (:Chunk {id: $id})-[r:MENTIONS]->(:Entity) RETURN count(r)",
                vec![("id", Value::String(chunk.id.clone()))],
            )
            .unwrap();
        assert_eq!(
            mentions.next().and_then(|row| row.first().and_then(as_i64)),
            Some(1),
            "repeated mention_entity calls must not accumulate duplicate MENTIONS edges"
        );
    }
}
