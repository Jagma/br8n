use crate::chunk::Chunker;
use crate::config::Config;
use crate::embed::Embedder;
use crate::enrich::Enricher;
use crate::model::Document;
use crate::store::Store;
use anyhow::{bail, Context, Result};

#[derive(Debug, Default, Clone, Copy)]
pub struct IndexStats {
    pub added: usize,
    pub updated: usize,
    pub skipped: usize,
    pub deleted: usize,
    pub chunks: usize,
    /// How much of `chunks` a re-index actually wrote, from `ChunkDelta`
    /// summed across every changed document — the whole point of
    /// `replace_chunks` is that this is usually far smaller than `chunks`.
    pub chunks_written: usize,
    /// Chunks whose row and vector were reused unchanged.
    pub chunks_reused: usize,
    /// Chunks deleted because their text changed or they no longer exist.
    pub chunks_pruned: usize,
}

/// Live ingestion progress.
///
/// Counting DOCUMENTS is close to useless here: a transcript can be three
/// orders of magnitude larger than a note, so "40/330" tells you nothing about
/// how much work is left. This tracks BYTES of text, which is what actually
/// determines how many embedding round-trips remain.
///
/// Emits on a timer rather than every N documents, for the same reason — one
/// huge document can otherwise go minutes without a line.
struct Progress {
    total_bytes: u64,
    done_bytes: u64,
    total_docs: usize,
    done_docs: usize,
    chunks: usize,
    started: std::time::Instant,
    last: std::time::Instant,
    published: std::time::Instant,
    /// Documents covered by the most recent emitted line, so the closing line
    /// does not repeat one the timer already printed.
    emitted_at: usize,
    tty: bool,
    /// Where to publish machine-readable progress, so `br8n status` can report
    /// a run it did not start. The `SessionStart` indexer is detached and its
    /// stderr goes to a log file — without this there is no way to ask how far
    /// a background index has got.
    state_path: Option<std::path::PathBuf>,
    quiet: bool,
}

impl Progress {
    fn new_at(docs: &[Document], state_path: Option<std::path::PathBuf>, quiet: bool) -> Progress {
        use std::io::IsTerminal;
        let now = std::time::Instant::now();
        Progress {
            total_bytes: docs.iter().map(|d| d.text.len() as u64).sum(),
            done_bytes: 0,
            total_docs: docs.len(),
            done_docs: 0,
            chunks: 0,
            started: now,
            // Force the first tick to print, so something appears immediately
            // rather than after the first interval.
            last: now - std::time::Duration::from_secs(10),
            published: now - std::time::Duration::from_secs(10),
            emitted_at: usize::MAX,
            tty: std::io::stderr().is_terminal(),
            state_path,
            quiet,
        }
    }

    /// Publish the current state as JSON. Best-effort: a progress file that
    /// cannot be written must never stop an index.
    fn publish(&self, done: bool) {
        let Some(path) = &self.state_path else { return };
        if done {
            let _ = std::fs::remove_file(path);
            return;
        }
        let elapsed = self.started.elapsed().as_secs_f64();
        let eta = if self.done_bytes > 0 && self.done_bytes < self.total_bytes {
            ((self.total_bytes - self.done_bytes) as f64) * (elapsed / self.done_bytes as f64)
        } else {
            0.0
        };
        let pct = if self.total_bytes == 0 {
            100.0
        } else {
            self.done_bytes as f64 / self.total_bytes as f64 * 100.0
        };
        let json = format!(
            r#"{{"pct":{:.1},"docs_done":{},"docs_total":{},"chunks":{},"bytes_done":{},"bytes_total":{},"elapsed_s":{:.0},"eta_s":{:.0},"pid":{}}}"#,
            pct,
            self.done_docs,
            self.total_docs,
            self.chunks,
            self.done_bytes,
            self.total_bytes,
            elapsed,
            eta,
            std::process::id()
        );
        let _ = std::fs::write(path, json);
    }

    fn advance(&mut self, doc: &Document, chunks: usize) {
        self.done_bytes += doc.text.len() as u64;
        self.done_docs += 1;
        self.chunks += chunks;
        // A terminal can take frequent in-place redraws; a log file gets one
        // line every two seconds so it stays readable.
        let interval = if self.tty { 200 } else { 2000 };
        if self.last.elapsed().as_millis() >= interval {
            self.emit(false);
            // Publish on the same 2s cadence regardless of terminal, so a
            // status query never reads a file that is minutes stale.
            if self.published.elapsed().as_millis() >= 2000 {
                self.publish(false);
                self.published = std::time::Instant::now();
            }
            self.last = std::time::Instant::now();
        }
    }

    fn emit(&mut self, final_line: bool) {
        if self.quiet {
            return;
        }
        // On a log the timer may already have printed this exact state; a
        // terminal redraws in place, so repeating there is free.
        if !self.tty && self.emitted_at == self.done_docs {
            return;
        }
        self.emitted_at = self.done_docs;
        use std::io::Write;
        let elapsed = self.started.elapsed().as_secs_f64();
        let pct = if self.total_bytes == 0 {
            100.0
        } else {
            self.done_bytes as f64 / self.total_bytes as f64 * 100.0
        };
        let mb = |b: u64| b as f64 / 1_048_576.0;
        let rate = if elapsed > 0.0 {
            self.chunks as f64 / elapsed
        } else {
            0.0
        };
        // ETA from the byte rate, not the chunk rate: bytes are the thing we
        // know the total of.
        let eta = if self.done_bytes > 0 && self.done_bytes < self.total_bytes {
            let per_byte = elapsed / self.done_bytes as f64;
            let left = (self.total_bytes - self.done_bytes) as f64 * per_byte;
            format!("{}m{:02}s left", (left as u64) / 60, (left as u64) % 60)
        } else {
            "—".into()
        };

        let bar = {
            let filled = ((pct / 100.0) * 24.0).round() as usize;
            format!("{}{}", "█".repeat(filled), "·".repeat(24 - filled.min(24)))
        };

        let line = format!(
            "{bar} {pct:5.1}%  {}/{} docs  {} chunks  {:.1}/{:.1} MB  {rate:.0} ch/s  {eta}",
            self.done_docs,
            self.total_docs,
            self.chunks,
            mb(self.done_bytes),
            mb(self.total_bytes),
        );

        let mut err = std::io::stderr();
        if self.tty {
            // \r redraw, padded so a shorter line cannot leave debris behind.
            let _ = write!(err, "\r\x1b[2K{line}");
            if final_line {
                let _ = writeln!(err);
            }
        } else {
            let _ = writeln!(err, "br8n: {line}");
        }
        let _ = err.flush();
    }
}

pub struct Indexer {
    /// Where a long run publishes progress for `br8n status` to read.
    progress_path: Option<std::path::PathBuf>,
    store: Store,
    embedder: Box<dyn Embedder>,
    config: Config,
    quiet: bool,
    yields_to_queries: bool,
}

impl Indexer {
    pub fn new(store: Store, embedder: Box<dyn Embedder>, config: Config) -> Self {
        Self {
            progress_path: None,
            store,
            embedder,
            config,
            quiet: false,
            yields_to_queries: true,
        }
    }

    /// Publish progress to `path` while indexing. Set by `reindex_swap`, which
    /// is the only path long enough for anyone to want to watch it.
    pub fn with_progress(mut self, path: std::path::PathBuf) -> Self {
        self.progress_path = Some(path);
        self
    }

    pub fn silent(mut self) -> Self {
        self.quiet = true;
        self
    }

    pub fn without_yielding_to_queries(mut self) -> Self {
        self.yields_to_queries = false;
        self
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn embedder(&self) -> &dyn crate::embed::Embedder {
        self.embedder.as_ref()
    }

    /// Index and query embeddings must come from the same model, so a mismatch
    /// stops the run rather than silently producing an unsearchable index.
    ///
    /// Also checks the on-disk schema version — extending this mechanism
    /// rather than adding a second one. `embed_model` being unset is how a
    /// brand-new store is told apart from an old one: a store that has never
    /// been indexed has neither key set, and both get stamped together for
    /// the first time. A store that already has `embed_model` set but no
    /// `schema_version` is an OLD index written before schema versioning
    /// existed — not a fresh store — so it is refused with the same remedy as
    /// a model mismatch rather than silently adopting the new version, which
    /// would leave stale rows (e.g. an old `embed_text` column an old binary
    /// wrote) misread by code that now expects the new shape.
    fn check_model(&self) -> Result<()> {
        let mine = self.embedder.model_id();
        let is_first_index = self.store.get_meta("embed_model")?.is_none();
        match self.store.get_meta("embed_model")? {
            None => self.store.set_meta("embed_model", &mine)?,
            Some(stored) if stored == mine => {}
            Some(stored) => bail!(
                "index was built with embedding model `{stored}` but `{mine}` is configured; \
                 run `br8n index --reindex` to rebuild from scratch"
            ),
        }

        let my_schema = crate::store::schema::SCHEMA_VERSION;
        match self.store.get_meta("schema_version")? {
            None if is_first_index => self.store.set_meta("schema_version", my_schema)?,
            None => bail!(
                "index predates schema version `{my_schema}` (chunks no longer store `fts_body`, \
                 and lbug's FTS index is gone with it — the pack serves BM25 now); \
                 run `br8n index --reindex` to rebuild from scratch"
            ),
            Some(stored) if stored == my_schema => {}
            Some(stored) => bail!(
                "index schema is version `{stored}` but this binary needs `{my_schema}`; \
                 run `br8n index --reindex` to rebuild from scratch"
            ),
        }
        Ok(())
    }

    pub fn index_documents(&self, docs: &[Document]) -> Result<IndexStats> {
        self.index_documents_with(docs, true)
    }

    /// `index_documents`, with the option to skip the embedder entirely —
    /// phase 1 of an asynchronous index (`br8n index --no-embed`). Every
    /// new or changed chunk is still chunked, still linked to its document,
    /// still written with the same `embed_hash` it would always get — only
    /// the `embedding` value itself is left NULL (see `Store::insert_chunks`'s
    /// empty-vector convention). `backfill_vectors` fills it in later.
    ///
    /// Contextual enrichment (`cfg.embed.contextual`) is skipped too when
    /// `embed` is false, not merely the embedding call — two reasons. First,
    /// phase 1 exists to publish with NO model round trip at all, and
    /// enrichment is its own model call. Second, and more importantly:
    /// `embed_text` itself is not stored on the `Chunk` row (only its hash
    /// is — see `schema.rs`), so `Store::chunks_without_vectors` can only
    /// ever reconstruct the PLAIN form later (`Chunk::plain_embed_text`). If
    /// enrichment ran here, the `embed_hash` written now would be a hash of
    /// enriched text that no later reconstruction could ever reproduce —
    /// silently and permanently out of reach. Skipping enrichment here keeps
    /// the stored hash equal to the hash of exactly what `backfill_vectors`
    /// will actually embed.
    pub fn index_documents_with(&self, docs: &[Document], embed: bool) -> Result<IndexStats> {
        self.check_model()?;

        let target = self.config.embed.chunk_tokens.max(64);
        let chunker = Chunker::new(target, target / 2);
        let enricher = Enricher::new(&self.config.embed);
        let mut stats = IndexStats::default();

        // Indexing embeds every chunk of every changed document, which is slow
        // on a first run — a 102MB transcript corpus is tens of minutes. Without
        // visible progress it is indistinguishable from a hang, and the previous
        // heartbeat counted documents, which says nothing when one document can
        // be a thousand times larger than another.
        let mut progress = Progress::new_at(docs, self.progress_path.clone(), self.quiet);

        // Chunking and embedding run on a pool; writes stay on this thread.
        //
        // Embedding is a network round-trip per batch of chunks, and the loop
        // used to do nothing else while it waited — on a 102MB transcript
        // corpus that is tens of minutes of an idle CPU. The store connection
        // is not shareable, so writes cannot be parallelised, but they are a
        // rounding error next to the embedding. Workers produce
        // (document, chunks, vectors); this thread consumes and writes.
        //
        // Hashes are fetched ONCE up front rather than per document: the skip
        // decision needs the store, and asking per document would have pulled
        // every worker back onto this thread for a lookup that is a single
        // query for the whole corpus.
        let known = self.store.all_doc_hashes()?;
        // The database path, not a single marker path: markers are per-reader
        // now, so yielding means "is ANY reader's marker fresh?" — see
        // `query_marker_path`.
        let qdb = Config::db_path();
        let qdb = &qdb;
        let yields = self.yields_to_queries;

        // Existing embeddings for the documents about to be rebuilt, so an
        // append-only source pays only for what was appended. Fetched here,
        // once, because the store belongs to this thread and the pool below
        // cannot reach it. Only documents that actually changed are loaded —
        // for a growing transcript that is one document, and reusing its
        // chunks turns a full re-embed into a handful of new ones.
        //
        // Keyed by a hash of `embed_text`, not the text itself: the store
        // keeps only `embed_hash` (see Task 2 on `embed_text` -> `embed_hash`),
        // so this side of the join hashes its own candidate text to match.
        let mut reusable: std::collections::HashMap<String, Vec<f32>> = Default::default();
        for doc in docs.iter() {
            let unchanged = known.get(&doc.id).is_some_and(|h| *h == doc.content_hash);
            if unchanged {
                continue;
            }
            if let Ok(existing) = self.store.embeddings_by_hash(&doc.id) {
                reusable.extend(existing);
            }
        }
        let reusable = &reusable;
        let workers = self.config.embed.concurrency.max(1).min(docs.len().max(1));
        let next = std::sync::atomic::AtomicUsize::new(0);

        type Built = (usize, Vec<crate::model::Chunk>, Vec<Vec<f32>>);
        // Bounded, so a fast pool cannot build an unbounded backlog of vectors
        // in memory while the writer catches up.
        let (tx, rx) = std::sync::mpsc::sync_channel::<Result<Built>>(workers * 2);

        std::thread::scope(|scope| -> Result<()> {
            for _ in 0..workers {
                let tx = tx.clone();
                let next = &next;
                let known = &known;
                let chunker = &chunker;
                let enricher = &enricher;
                let embedder = &self.embedder;
                scope.spawn(move || loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(doc) = docs.get(i) else { break };

                    if known.get(&doc.id).is_some_and(|h| *h == doc.content_hash) {
                        if tx.send(Ok((i, Vec::new(), Vec::new()))).is_err() {
                            break;
                        }
                        continue;
                    }

                    let mut chunks = chunker.chunk(doc);
                    if chunks.is_empty() {
                        if tx.send(Ok((i, Vec::new(), Vec::new()))).is_err() {
                            break;
                        }
                        continue;
                    }
                    if embed {
                        if let Err(e) = enricher.enrich(doc, &mut chunks) {
                            let _ = tx.send(Err(e));
                            break;
                        }
                    }
                    if !embed {
                        // Phase 1: no model round trip at all. Every chunk
                        // gets the empty-vector sentinel `insert_chunks`
                        // reads as "no embedding yet" — see its doc comment.
                        let vecs = vec![Vec::new(); chunks.len()];
                        if tx.send(Ok((i, chunks, vecs))).is_err() {
                            break;
                        }
                        continue;
                    }
                    // Embed only text we have no vector for. Identical text
                    // always yields an identical embedding from the same model,
                    // and `model_id` is stamped in the index and checked on
                    // open, so a reused vector can never come from another
                    // model. An append-only source therefore pays for what was
                    // appended instead of for the whole document.
                    //
                    // `reusable` is keyed by a hash of `embed_text`, not the
                    // text itself (the store keeps only `embed_hash`), so each
                    // chunk's hash is computed once up front and used for both
                    // the reuse lookup below and the fill-in loop after embedding.
                    let hashes: Vec<String> = chunks
                        .iter()
                        .map(|c| crate::model::Document::content_hash(&c.embed_text))
                        .collect();
                    let need: Vec<String> = chunks
                        .iter()
                        .zip(&hashes)
                        .filter(|(_, h)| !reusable.contains_key(*h))
                        .map(|(c, _)| c.embed_text.clone())
                        .collect();
                    // A prompt is waiting on this same single-slot queue:
                    // let it through. See `query_marker_path`.
                    if yields {
                        yield_to_queries(qdb, std::time::Duration::from_secs(5));
                    }
                    match embedder.embed_documents(&need) {
                        Ok(fresh) => {
                            // Rebuild in chunk order from the cache and the
                            // freshly embedded batch. A document can repeat the
                            // same text twice, so newly embedded vectors are
                            // memoised as we go.
                            let mut it = fresh.into_iter();
                            let mut seen: std::collections::HashMap<String, Vec<f32>> =
                                Default::default();
                            let mut vecs: Vec<Vec<f32>> = Vec::with_capacity(chunks.len());
                            for h in hashes.iter() {
                                if let Some(v) = reusable.get(h) {
                                    vecs.push(v.clone());
                                } else if let Some(v) = seen.get(h) {
                                    vecs.push(v.clone());
                                } else if let Some(v) = it.next() {
                                    seen.insert(h.clone(), v.clone());
                                    vecs.push(v);
                                } else {
                                    break;
                                }
                            }
                            if vecs.len() != chunks.len() {
                                let _ = tx.send(Err(anyhow::anyhow!(
                                    "embedding count mismatch: {} chunks, {} vectors",
                                    chunks.len(),
                                    vecs.len()
                                )));
                                break;
                            }
                            if tx.send(Ok((i, chunks, vecs))).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e));
                            break;
                        }
                    }
                });
            }
            // Dropped so the receiver ends once every worker has finished.
            drop(tx);

            for msg in rx {
                let (i, chunks, vecs) = msg?;
                let doc = &docs[i];

                if chunks.is_empty() {
                    // Either unchanged, or it produced no chunks at all. Both
                    // still upsert the Document node so a later run sees it as
                    // indexed rather than re-adding it forever; the unchanged
                    // case is distinguished by the hash we already looked up.
                    if known.get(&doc.id).is_some_and(|h| *h == doc.content_hash) {
                        stats.skipped += 1;
                    } else {
                        if known.contains_key(&doc.id) {
                            stats.updated += 1;
                        } else {
                            stats.added += 1;
                        }
                        self.store.upsert_document(doc)?;
                    }
                    progress.advance(doc, 0);
                    continue;
                }

                if known.contains_key(&doc.id) {
                    stats.updated += 1;
                } else {
                    stats.added += 1;
                }

                debug_assert_eq!(chunks.len(), vecs.len());
                // upsert_document only replaces the Document node now;
                // replace_chunks decides which of this document's chunks
                // actually need rewriting, so an appended paragraph costs one
                // new chunk instead of the whole document.
                self.store.upsert_document(doc)?;
                let delta = self.store.replace_chunks(&doc.id, &chunks, &vecs)?;
                stats.chunks += chunks.len();
                stats.chunks_written += delta.inserted;
                stats.chunks_reused += delta.kept;
                stats.chunks_pruned += delta.deleted;
                progress.advance(doc, chunks.len());
            }
            Ok(())
        })?;
        progress.emit(true);
        progress.publish(true);

        Ok(stats)
    }

    /// Removes documents whose source URI is no longer present in `live_uris`.
    ///
    /// Refuses to empty a populated index. `discover` swallows loader errors and
    /// `Config::load` never fails — it returns defaults with `sources: []` — so an
    /// empty `live_uris` is indistinguishable from a typo'd source path, a moved
    /// directory, or an unmounted drive. Pruning on that signal would silently
    /// delete a corpus that took hours to build. Rebuilding is what
    /// `br8n index --reindex` is for, and that is an explicit choice.
    /// True when `discover` is CAPABLE of enumerating this URI under the
    /// current config. Only such documents may be pruned.
    ///
    /// A document discovery cannot see is not "missing" — it is out of band.
    /// `br8n add` writes two kinds of those: web clippings (an `https://` URI
    /// that lives nowhere on disk) and files from outside every configured
    /// root. Both used to be deleted by the very next `br8n index`, which
    /// `SessionStart` spawns with stderr closed on every session — so anything
    /// you added was destroyed within minutes, with nothing printed anywhere.
    fn discoverable(&self, uri: &str) -> bool {
        uri_is_discoverable(&self.config, uri)
    }
}

/// Free-function form of `Indexer::discoverable`, so `reindex_swap` can decide
/// whether anything needs pruning without building an `Indexer` first.
fn uri_is_discoverable(cfg: &Config, uri: &str) -> bool {
    // Transcripts are a managed source. When enabled, discovery enumerates all
    // of them, so absence really does mean deleted; when disabled, the user
    // asked for them gone and pruning is how that takes effect.
    if crate::loaders::transcript::SessionAgent::from_uri(uri).is_some() {
        return true;
    }
    match uri.strip_prefix("file://") {
        Some(path) => {
            let path = std::path::Path::new(path);
            cfg.sources.iter().any(|root| {
                root.canonicalize()
                    .map(|root| path.starts_with(root))
                    .unwrap_or(false)
            })
        }
        // Web clippings, and anything else added out of band.
        None => false,
    }
}

impl Indexer {
    pub fn prune_missing(&self, live_uris: &[String]) -> Result<usize> {
        // Restrict to reachable documents BEFORE the empty-index guard, so an
        // index holding nothing but web clippings does not read as "populated"
        // and block a legitimate prune.
        let prunable: Vec<(String, String)> = self
            .store
            .all_doc_uris()?
            .into_iter()
            .filter(|(_, uri)| self.discoverable(uri))
            .collect();

        if live_uris.is_empty() && !prunable.is_empty() {
            bail!(
                "refusing to prune: discovery found no documents, but the index holds {}. \
                 Check `sources` in {}. To rebuild deliberately, run `br8n index --reindex`.",
                prunable.len(),
                Config::config_path().display()
            );
        }

        let live: std::collections::HashSet<&str> = live_uris.iter().map(|s| s.as_str()).collect();
        let mut removed = 0;
        for (id, uri) in prunable {
            if !live.contains(uri.as_str()) {
                self.store.delete_document(&id)?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Wikilinks are written by hand and resolve by title or filename stem, so this
    /// runs after all documents exist — a link may point forward to a note
    /// processed later in the same batch, or written in a previous run.
    /// Unresolvable targets are dropped silently: a link to a note you have not
    /// written yet is normal, not an error, and this must never log-spam.
    ///
    /// Also writes TYPED `LINKS_TO {kind}` edges for the lifecycle relations a
    /// document declares in frontmatter — `supersedes` and `superseded-by` —
    /// resolved through the same `by_key` map and the same fail-closed rule as a
    /// wikilink. These sit BESIDE the prose wikilink rather than replacing it:
    /// lbug MERGEs on the relationship property, so a pair carrying both has two
    /// edges. Unlike a wikilink, a relation that resolves to nothing is REPORTED
    /// on stderr — a wikilink to an unwritten note is normal, but a
    /// `superseded-by:` naming nothing is an authoring error worth surfacing.
    ///
    /// Also attaches `DERIVED_FROM` edges to a document's source domain and
    /// `MENTIONS` edges to a transcript's project entity. Both `domain` and
    /// `project` come from loader-populated `meta` and can be empty strings
    /// (no host on the URL, no `cwd` line in the transcript) — an empty value
    /// is guarded out so every domain-less/project-less document doesn't
    /// collapse onto one degenerate hub node that retrieval would then treat
    /// as connecting them all.
    pub fn resolve_links(&self, docs: &[Document]) -> Result<usize> {
        // Built from the STORE, not from `docs`. `br8n add` calls this with a
        // single document; resolving against only that slice would make every
        // wikilink unresolvable, and re-adding an existing note would destroy the
        // edges it already had (upsert_document DETACH DELETEs its node first).
        //
        // Collect CANDIDATES first, then keep only unambiguous keys. Two notes
        // titled "README" — or two `README.md` files in different directories —
        // is the norm, not an edge case. Overwriting on collision would make
        // `[[README]]` resolve to whichever row the scan happened to yield last,
        // non-deterministically, and link the reader to the wrong project's note.
        // An ambiguous link must fail CLOSED, the same way an unresolvable one does.
        let mut candidates: std::collections::HashMap<String, std::collections::HashSet<String>> =
            Default::default();
        for (id, title, uri) in self.store.all_doc_keys()? {
            let title = title.trim();
            if !title.is_empty() {
                candidates
                    .entry(title.to_lowercase())
                    .or_default()
                    .insert(id.clone());
            }
            if let Some(stem) = uri.rsplit('/').next().and_then(|f| f.split('.').next()) {
                let stem = stem.trim();
                if !stem.is_empty() {
                    candidates
                        .entry(stem.to_lowercase())
                        .or_default()
                        .insert(id.clone());
                }
            }
        }
        // `id` as a THIRD key, subject to the same unambiguity rule as title and
        // stem: two records claiming `ADR-0004` resolve to neither. Sourced from
        // `docs` rather than `all_doc_keys`, which does not project `meta`.
        //
        // This widens what a `[[wikilink]]` can resolve to as well as what a
        // frontmatter relation can. That is intended — `[[ADR-0004]]` is a
        // reasonable thing to write — and measured to be inert on the current
        // vault, which contains zero links in that form.
        for d in docs {
            if let Some(fid) = d.meta.get("id").and_then(|v| v.as_str()) {
                let fid = fid.trim();
                if !fid.is_empty() {
                    candidates
                        .entry(fid.to_lowercase())
                        .or_default()
                        .insert(d.id.clone());
                }
            }
        }
        let by_key: std::collections::HashMap<String, String> = candidates
            .into_iter()
            .filter(|(_, ids)| ids.len() == 1)
            .map(|(k, ids)| (k, ids.into_iter().next().expect("len checked")))
            .collect();

        let mut n = 0;
        for d in docs {
            for target in &d.links {
                if let Some(to) = by_key.get(target.trim().to_lowercase().as_str()) {
                    if to != &d.id {
                        self.store.link_documents(&d.id, to, "wikilink")?;
                        n += 1;
                    }
                }
            }
            // Declared lifecycle relations become TYPED edges beside the prose
            // wikilink. The `kind` uses the vault's own hyphen spelling so an
            // edge in the dashboard reads the same as the frontmatter that
            // produced it.
            //
            // Resolved through the same `by_key` map and the same fail-closed
            // rule: a relation naming an id that is ambiguous, absent, or the
            // document itself writes no edge. A dangling `superseded-by:` is an
            // authoring error in the vault, and inventing an edge for it would
            // put a wrong answer in the graph.
            for (meta_key, kind) in [
                ("supersedes", "supersedes"),
                ("superseded_by", "superseded-by"),
            ] {
                let Some(target) = d.meta.get(meta_key).and_then(|v| v.as_str()) else {
                    continue;
                };
                let target = target.trim().to_lowercase();
                if target.is_empty() {
                    continue;
                }
                match by_key.get(target.as_str()) {
                    Some(to) if to != &d.id => {
                        self.store.link_documents(&d.id, to, kind)?;
                        n += 1;
                    }
                    _ => eprintln!(
                        "br8n: {} declares `{}: {}` which resolves to no unique document",
                        d.uri, meta_key, target
                    ),
                }
            }
            if let Some(domain) = d.meta.get("domain").and_then(|v| v.as_str()) {
                if !domain.is_empty() {
                    self.store.attach_source(&d.id, domain)?;
                }
            }
            if let Some(project) = d.meta.get("project").and_then(|v| v.as_str()) {
                if !project.is_empty() {
                    let first = crate::model::Chunk::id(&d.id, 0);
                    self.store.mention_entity(&first, project, "project")?;
                }
            }
        }
        Ok(n)
    }

    /// Titles/filename stems that map to more than one document, and therefore
    /// resolve to nothing in `resolve_links` (an ambiguous wikilink fails CLOSED,
    /// same as an unresolvable one). Exists so the CLI can print a single summary
    /// line after indexing instead of leaving the drop invisible.
    ///
    /// Recomputes a candidate map rather than changing `resolve_links`'s
    /// `Result<usize>` signature, which Tasks 21 and 23 already depend on. Cost is
    /// one more scan of `all_doc_keys`, bounded by corpus size, run once per
    /// `index`/`add` invocation — not per document.
    ///
    /// NOT the same map any more, and the difference is a known blind spot.
    /// `resolve_links` also folds in each document's frontmatter `id`, which this
    /// cannot see: `all_doc_keys` does not project `meta`. So an `id` colliding
    /// with another document's title or stem makes that key unresolvable there
    /// while staying invisible here, and the CLI's ambiguity line will not
    /// mention it. Closing that would mean projecting `meta` through
    /// `all_doc_keys` — a wider change than the report is worth, and the
    /// frontmatter-relation case is separately reported by `resolve_links`'s own
    /// "resolves to no unique document" warning. Left as a documented gap rather
    /// than an unnoticed one.
    pub fn ambiguous_titles(&self) -> Result<Vec<String>> {
        let mut candidates: std::collections::HashMap<String, std::collections::HashSet<String>> =
            Default::default();
        for (id, title, uri) in self.store.all_doc_keys()? {
            let title = title.trim();
            if !title.is_empty() {
                candidates
                    .entry(title.to_lowercase())
                    .or_default()
                    .insert(id.clone());
            }
            if let Some(stem) = uri.rsplit('/').next().and_then(|f| f.split('.').next()) {
                let stem = stem.trim();
                if !stem.is_empty() {
                    candidates
                        .entry(stem.to_lowercase())
                        .or_default()
                        .insert(id.clone());
                }
            }
        }
        let mut ambiguous: Vec<String> = candidates
            .into_iter()
            .filter(|(_, ids)| ids.len() > 1)
            .map(|(k, _)| k)
            .collect();
        ambiguous.sort();
        Ok(ambiguous)
    }
}

/// What a discovery pass found, split by whether it had to be read.
///
/// A failing loader is skipped rather than aborting the run — one bad PDF must
/// not stop an index. Those skips are RETURNED, not just printed: `SessionStart`
/// spawns the indexer with stderr redirected, so a scanned PDF used to vanish
/// from search with no trace anywhere. `reindex_swap` persists them for
/// `br8n status` to report.
#[derive(Default)]
pub struct Discovered {
    /// New or changed. These were parsed and must be re-embedded.
    pub docs: Vec<Document>,
    /// Present and provably unchanged, so never opened. Still counted as live
    /// so pruning does not delete them.
    pub unchanged: Vec<String>,
    /// Changed on disk, and deliberately NOT read this run: a session
    /// transcript some live session is still appending to. See
    /// `TranscriptLoader::SETTLE`.
    ///
    /// Distinct from `unchanged`, which means "provably identical to what is
    /// already indexed". These are the opposite — known to differ, and left
    /// for a later run on purpose. Still live, so pruning does not delete
    /// them.
    pub deferred: Vec<String>,
    /// Reasons files were not indexed, for `br8n status`.
    pub skipped: Vec<String>,
    /// Files a `Config::ignore` entry excluded this run. Reported to the user
    /// because an exclusion nobody can see is indistinguishable from an empty
    /// directory, and this tool's failure mode is silence.
    pub ignored: usize,
    /// URIs of PDFs that still owe recognition after this pass.
    ///
    /// Normally empty in an `OcrPass::Run` pass: that pass either reads the
    /// pages or records the file as attempted, and the anti-retry rule in
    /// `consider_into` stamps the attempted failures so they are not tried
    /// forever. The exception is a file recognition never actually reached —
    /// `OCR_DISABLED` latched off partway through the run — which stays here
    /// precisely so `live_uris` keeps `prune_missing` off a document nothing
    /// looked at.
    ///
    /// Always empty when `[pdf] ocr = "off"`, in either pass: there is no
    /// second pass to hand the work to, so queueing would be a backlog that
    /// nothing can drain.
    pub ocr_pending: Vec<String>,
}

impl Discovered {
    /// Every URI that exists on disk right now — the input `prune_missing`
    /// needs. Omitting `unchanged` here would delete the entire corpus.
    pub fn live_uris(&self) -> Vec<String> {
        let mut v: Vec<String> = self.docs.iter().map(|d| d.uri.clone()).collect();
        v.extend(self.unchanged.iter().cloned());
        // `deferred` belongs here for the same reason `unchanged` does, and
        // more urgently. `uri_is_discoverable` returns TRUE for every
        // `claude-session://` URI, so a transcript missing from this list is
        // deleted outright by `prune_missing` — and a deferred transcript is
        // normally already in the index from an earlier run. Leave it out and
        // every live session churns its own transcript out of the corpus and
        // back in again, which is worse than the re-chunking the deferral
        // exists to avoid.
        v.extend(self.deferred.iter().cloned());
        // `ocr_pending` belongs here for the same reason `deferred` does, and
        // the failure mode is identical in shape, not just in kind. A fully
        // scanned PDF already indexed from an earlier run yields
        // `Err(PdfError::Scanned)` when phase 1 loads it with OCR off, so it
        // never reaches `docs` — its `Err` arm only pushes to `ocr_pending`
        // and `skipped`. Leave it out of `live_uris` and `prune_missing`
        // deletes the very document this two-pass feature exists to keep:
        // the one that is entirely scanned images and has no text layer to
        // fall back on. A mixed PDF is unaffected, because its readable pages
        // still land in `docs`.
        v.extend(self.ocr_pending.iter().cloned());
        v
    }
}

/// One worker's share of a discovery walk.
///
/// `consider` used to capture `&mut found` and `&mut fresh` directly, which
/// made it an `FnMut` holding two unique borrows — neither `Sync` nor `Clone`,
/// so it could not be handed to a pool at all. Each worker fills its own
/// `Partial`; the main thread merges them after the join and sorts once.
#[derive(Default)]
struct Partial {
    found: Discovered,
    fresh: std::collections::HashMap<String, String>,
}

impl Partial {
    fn merge(&mut self, other: Partial) {
        self.found.docs.extend(other.found.docs);
        self.found.unchanged.extend(other.found.unchanged);
        self.found.deferred.extend(other.found.deferred);
        self.found.skipped.extend(other.found.skipped);
        self.found.ocr_pending.extend(other.found.ocr_pending);
        // A count, not a list: summed rather than concatenated.
        //
        // No worker ever sets it — `ignored` is counted on the WALK thread,
        // before the pool starts, and installed on the merged result
        // afterwards — so in practice both sides of this `+=` are zero. The
        // line is here to keep `merge` TOTAL: a field silently dropped by a
        // merge is the kind of thing that only shows up once something else
        // starts filling it in.
        self.found.ignored += other.found.ignored;
        self.fresh.extend(other.fresh);
    }
}

/// What kind of file a walk entry is, decided once so the pool does not
/// re-inspect extensions.
///
/// No `PartialEq`/`Eq`: nothing compares two `Kind`s, every use is a `match`.
/// `Settling` below is the one that IS compared, which is presumably where the
/// derives came from.
#[derive(Clone, Copy)]
enum Kind {
    Markdown,
    Pdf,
    Session(crate::loaders::transcript::SessionAgent),
}

/// Whether a source is written incrementally by a still-running process, so a
/// file that changed moments ago is probably not finished changing.
///
/// Only transcripts are. A markdown note or a PDF arrives in one save from an
/// editor; there is no process appending to it, so waiting would only delay
/// the note the user just wrote.
///
/// A property of the SOURCE KIND. `Deferral` is the other half — a property of
/// the RUN — and a file is only ever left behind when both say so.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Settling {
    /// Read it as soon as it changes.
    No,
    /// Leave it for a later run while it is still being written.
    Wait,
}

/// Whether THIS RUN is allowed to leave a still-being-written transcript for a
/// later one.
///
/// Deferring is only ever safe when something else is still serving the file.
/// An incremental run seeds its shadow from the live index, so the version
/// read last time survives untouched and "later" really means later. A run
/// that builds from nothing has no such fallback: a file it does not read is
/// a file that is not in the index it publishes.
///
/// `br8n index --reindex` was exactly that case and it lost data. `FromScratch`
/// passes an EMPTY stamp map (see `reindex_swap_with`), so every transcript
/// looks changed, every live session's transcript is deferred, and the shadow
/// is published without them. `live_uris` keeping them out of `prune_missing`'s
/// way does not help — there is nothing to prune, because the rebuilt index
/// never had them. Reproduced end to end: 3 documents before, 2 after, and the
/// third did not come back for as long as its session kept appending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deferral {
    /// Leave it. The caller keeps what is already indexed, so a version one
    /// run out of date is still there to answer with.
    Allowed,
    /// Read it now, however fresh — the caller has no previous version to fall
    /// back on, so deferring can only lose the file.
    Forbidden,
}

/// Whether THIS PASS may recognize scanned pages.
///
/// Orthogonal to `[pdf] ocr`, which says whether the USER wants recognition at
/// all. This says whether the pass currently running is the one that does it.
/// Phase 1 publishes without waiting for OCR, so it always skips; phase 2 is
/// the pass that pays for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcrPass {
    /// Load PDFs from the text layer only, whatever `[pdf] ocr` says, and
    /// record what was left unread in `Discovered::ocr_pending`.
    Skip,
    /// Recognize as `[pdf] ocr` directs.
    Run,
}

/// A cheap fingerprint of a file: modification time and size.
///
/// This is the `git status` trick. Content hashing already stops unchanged
/// documents being re-embedded, but only AFTER the file has been read, parsed
/// and chunked — and the transcript corpus measured 157MB of JSONL, re-parsed
/// in full on every single run, including runs where nothing had changed.
/// Comparing a stat against the previous stat skips the read entirely.
///
/// Mtime can lie (a restored backup, a clock change). That is safe here
/// because it only decides whether to LOOK at a file; the content hash still
/// decides whether to re-embed, so a false "unchanged" is the only risk and it
/// resolves the moment the file is touched again.
pub(crate) fn stamp(path: &std::path::Path) -> Option<String> {
    let m = std::fs::metadata(path).ok()?;
    let secs = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(format!("{secs}:{}", m.len()))
}

/// Marker a query-serving process drops while it embeds, so bulk indexing
/// yields the (single-slot) Ollama queue to it.
///
/// Ollama runs embedding with one server slot, so a query that arrives behind
/// a bulk batch waits for the whole queue. Measured: 7.8s, 15.3s and 7.9s for
/// a single query embed during a large index, against the hook's query
/// budget — so for the duration of any real index, the prompt hook silently
/// injected nothing. That window bit three separate times (two contaminated
/// benchmarks, then the user's first live test of the plugin) before this fix.
///
/// The protocol is files and no IPC: a reader writes ITS OWN marker before its
/// query and removes it when its last query on this process finishes; each
/// indexing worker sleeps between batches while ANY marker beside the database
/// is fresh.
///
/// One marker per reader PROCESS (`db.qwait.<pid>`), not one shared file. A
/// single shared path cannot be removed safely: the hook and the dashboard's
/// `/api/search` both announce on it with no refcount, so the dashboard's
/// guard dropping first deleted the hook's still-live marker and indexing
/// stopped yielding to it. Never removing it instead made every finished query
/// leave a fresh marker behind, and the indexer then slept the full staleness
/// window (~3s) at the next document boundary rather than resuming in ~200ms —
/// about a minute of dead time over a 389-document index with twenty prompts
/// in it. Per-PID files have neither problem: a guard only ever removes the
/// file it wrote, and within one process a refcount keeps the file alive until
/// the last concurrent guard is gone.
///
/// A crashed reader still leaves a marker, so only a RECENT mtime counts:
/// staleness bounds the wait even when nobody cleans up.
pub fn query_marker_path(db: &std::path::Path) -> std::path::PathBuf {
    db.with_extension(format!("qwait.{}", std::process::id()))
}

/// How fresh a marker must be to still mean "a query is running".
const MARKER_FRESH: std::time::Duration = std::time::Duration::from_secs(3);

/// Live `QueryPriority` guards in THIS process, per marker path. The file is
/// one per process, so it may only be removed once the last guard holding it
/// is gone — two dashboard searches served on two threads share a pid and
/// therefore a path, the way the hook and the dashboard used to share one
/// globally. Keyed by path rather than a bare counter because a process may
/// legitimately announce against more than one database (the test binary
/// does).
static ANNOUNCED: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, usize>>,
> = std::sync::LazyLock::new(Default::default);

/// RAII guard: this process's marker is present on disk for the lifetime of a
/// query embed, and removed when the last live guard drops.
pub struct QueryPriority(std::path::PathBuf);

impl QueryPriority {
    pub fn announce(db: &std::path::Path) -> QueryPriority {
        let p = query_marker_path(db);
        let mut live = ANNOUNCED.lock().unwrap_or_else(|e| e.into_inner());
        *live.entry(p.clone()).or_insert(0) += 1;
        let _ = std::fs::write(&p, b"q");
        QueryPriority(p)
    }
}

impl Drop for QueryPriority {
    fn drop(&mut self) {
        // Only the LAST guard on this path removes the file, and it only ever
        // removes this process's own marker — another reader's marker is never
        // touched, which is what made removal unsafe before.
        let mut live = ANNOUNCED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = live.get_mut(&self.0) {
            *n -= 1;
            if *n == 0 {
                live.remove(&self.0);
                let _ = std::fs::remove_file(&self.0);
            }
        }
    }
}

fn any_marker_within(db: &std::path::Path, within: std::time::Duration) -> bool {
    let (Some(dir), Some(stem)) = (db.parent(), db.file_stem().and_then(|s| s.to_str())) else {
        return false;
    };
    let prefix = format!("{stem}.qwait.");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.file_name()
            .to_str()
            .is_some_and(|n| n.starts_with(&prefix))
            && e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age < within)
    })
}

/// True while any reader process has a fresh marker beside `db`.
fn a_query_is_waiting(db: &std::path::Path) -> bool {
    any_marker_within(db, MARKER_FRESH)
}

pub fn sweep_dead_query_markers(db: &std::path::Path) {
    let (Some(dir), Some(stem)) = (db.parent(), db.file_stem().and_then(|s| s.to_str())) else {
        return;
    };
    let prefix = format!("{stem}.qwait.");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let owner = entry
            .file_name()
            .to_str()
            .and_then(|n| n.strip_prefix(&prefix))
            .and_then(|pid| pid.parse::<u32>().ok());
        if owner.is_some_and(|pid| !pid_alive(pid)) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub fn a_query_was_seen_within(db: &std::path::Path, within: std::time::Duration) -> bool {
    any_marker_within(db, within)
}

/// Sleep while any reader's query marker is fresh, up to `cap`.
fn yield_to_queries(db: &std::path::Path, cap: std::time::Duration) {
    let start = std::time::Instant::now();
    while start.elapsed() < cap {
        if !a_query_is_waiting(db) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Where the stat map lives: a plain file beside the database, NOT inside it.
///
/// It was originally a row in `IndexMeta`, which meant the pre-flight check had
/// to open the live store to read it — and LadybugDB takes an exclusive OS file
/// lock, so that reopened exactly the reader-blocking window the shadow swap
/// exists to close. `the_hook_can_still_read_during_a_reindex` caught it:
/// `status` reported 0 documents while an index was starting up.
///
/// A side file also makes the no-op path touch no database at all.
pub(crate) fn stamp_path(db: &std::path::Path) -> std::path::PathBuf {
    db.with_extension("stamps")
}

fn load_stamps(db: &std::path::Path) -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(stamp_path(db))
        .ok()
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

fn save_stamps(db: &std::path::Path, m: &std::collections::HashMap<String, String>) {
    if let Ok(j) = serde_json::to_string(m) {
        let _ = std::fs::write(stamp_path(db), j);
    }
}

/// How long ago the last SUCCESSFUL index published, or `None` if none ever has.
///
/// `save_stamps` runs only after `publish_shadow` returns, so `db.stamps`'
/// mtime is a "last good run" marker that is DERIVED rather than journalled —
/// the same pattern as the stamp map itself and as `chunks_without_vectors`.
/// There is no separate state file that can disagree with what happened.
///
/// `hook::session_start_decision` rate-limits on this. One property matters
/// there and is easy to miss: the unchanged-corpus fast path in
/// `reindex_swap_with` returns BEFORE `save_stamps`, so a no-op run does not
/// advance the mtime. The limit therefore suppresses exactly the runs that
/// cost something and leaves the free ones alone — and, read the other way
/// round, it does not bind at all while every run is free. See
/// `hook::REINDEX_INTERVAL` for why that second reading matters.
///
/// Every failure — no file, an unreadable mtime, a clock that moved backwards
/// so `elapsed()` errors — reads as `None`, which means "index". Refusing to
/// index because a timestamp looked odd is the failure that hides.
pub fn last_index_age(db: &std::path::Path) -> Option<std::time::Duration> {
    std::fs::metadata(stamp_path(db))
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()
}

/// Canonical, non-overlapping source roots.
///
/// `sources` is user-written and nothing ever deduplicated it. Two entries
/// where one contains the other — `["~/notes", "~/notes/work"]` — walk the
/// overlapping subtree TWICE, and both walks build the same canonical URI
/// because `consider` canonicalizes before formatting it. Two `Document`s then
/// share an id (`Document::new_id` hashes the URI) and race each other through
/// `upsert_document`'s `DETACH DELETE`.
///
/// This is what makes `found.docs.sort_by(|a, b| a.uri.cmp(&b.uri))` a TOTAL
/// order rather than a stable sort with an unpinned tie — the same class as the
/// five `chunk_id` tie-breaks CLAUDE.md lists.
///
/// Canonicalization happens AFTER the caller's existence check, never instead
/// of it: `canonicalize` fails on a missing path, and that check's message is
/// the one a user with a typo in `sources` should see.
///
/// REFUSES the run on a root it cannot canonicalize, rather than dropping it.
/// This used to be `filter_map(.. .ok())`, which is the same silent discard the
/// existence check above exists to prevent, reached through a different call:
/// a root that vanishes from the walk contributes zero documents, and
/// `prune_missing` then deletes every indexed document under it. `exists()`
/// passing while `canonicalize` fails is narrow — a directory whose search
/// permission was just removed, a symlink loop, an unmount racing the walk —
/// but the CONSEQUENCE is identical to a missing root, so the answer has to be
/// identical too. Reporting and continuing would leave the corpus-deleting half
/// of the behaviour intact and merely narrate it.
fn distinct_roots(sources: &[std::path::PathBuf]) -> Result<Vec<std::path::PathBuf>> {
    let mut canon: Vec<std::path::PathBuf> = Vec::with_capacity(sources.len());
    for p in sources {
        canon.push(p.canonicalize().with_context(|| {
            format!(
                "configured source `{}` exists but cannot be resolved. Check its \
                 permissions, or fix `sources` in {}. Refusing to index, because \
                 pruning against a partial scan would delete everything under \
                 this path.",
                p.display(),
                Config::config_path().display()
            )
        })?);
    }
    canon.sort();
    canon.dedup();

    // Sorted lexically, a containing root always precedes what it contains, so
    // one forward pass is enough.
    let mut kept: Vec<std::path::PathBuf> = Vec::new();
    for root in canon {
        if !kept.iter().any(|k| root.starts_with(k)) {
            kept.push(root);
        }
    }
    Ok(kept)
}

/// The `[pdf]` settings THIS pass loads with, which are not always the user's.
///
/// `Skip` forces `ocr = off` whatever was configured: that pass publishes the
/// text layer now and phase 2 recognizes afterwards. `Run` passes the user's
/// settings through untouched.
///
/// Free and pure so the forcing is testable without PDFium, ONNX Runtime or a
/// scanned fixture — which matters more than it looks. A test that drives
/// `discover_stat_first_with` with `ocr = auto` can only OBSERVE the forcing on
/// a machine where those dylibs resolve: without them `Auto` falls back to the
/// text layer by itself, the results are identical, and every assertion passes
/// whether or not `Skip` did anything at all.
fn pdf_config_for(pass: OcrPass, cfg: &crate::config::PdfConfig) -> crate::config::PdfConfig {
    match pass {
        OcrPass::Skip => crate::config::PdfConfig {
            ocr: crate::config::OcrMode::Off,
            ..cfg.clone()
        },
        OcrPass::Run => cfg.clone(),
    }
}

/// One file, considered against the previous run's stamp.
///
/// Free rather than a closure so a pool can call it: the closure captured
/// `&mut found` and `&mut fresh`, which made it neither `Sync` nor `Clone`.
/// The accumulators are now the caller's, one per worker.
#[allow(clippy::too_many_arguments)]
fn consider_into(
    mine: &mut Partial,
    cfg: &Config,
    db: &std::path::Path,
    stamps: &std::collections::HashMap<String, String>,
    defer: Deferral,
    pass: OcrPass,
    path: &std::path::Path,
    kind: Kind,
) {
    use crate::loaders::{markdown::MarkdownLoader, pdf::PdfLoader, transcript::TranscriptLoader};

    let (scheme, settling) = match kind {
        Kind::Markdown | Kind::Pdf => ("file://", Settling::No),
        Kind::Session(agent) => (agent.scheme(), Settling::Wait),
    };
    // A path that cannot be canonicalized is dropped here, and the drop has to
    // be VISIBLE. The URI is built FROM `canon`, so a file that fails this call
    // reaches none of `docs`, `unchanged`, `deferred` or `ocr_pending` — there
    // is no name to put in them. It used to reach `skipped` either, so the run
    // reported a corpus one document short with nothing anywhere saying which
    // one, which is the house failure mode exactly.
    //
    // It is not data LOSS today, but only by accident: `uri_is_discoverable`
    // canonicalizes each configured root the same way and fails the same way,
    // so `prune_missing` leaves an already-indexed copy of this document alone.
    // That symmetry is luck, not design — nothing states it and nothing tests
    // it — and it never told the user anything either way.
    let canon = match path.canonicalize() {
        Ok(canon) => canon,
        Err(e) => {
            // Both channels, in the loader-failure arm's own format: stderr
            // for the `db.log` the detached `SessionStart` indexer writes,
            // and the returned record for `br8n status`.
            eprintln!("br8n: skipping {}: {e}", path.display());
            mine.found.skipped.push(format!("{}: {e}", path.display()));
            return;
        }
    };
    let uri = format!("{scheme}{}", canon.display());
    let now = stamp(path);
    if let (Some(now), Some(prev)) = (&now, stamps.get(&uri)) {
        if now == prev {
            mine.found.unchanged.push(uri.clone());
            mine.fresh.insert(uri, now.clone());
            return;
        }
    }
    // The file differs from what is indexed — but a transcript whose
    // session is still alive will differ again in a few seconds, so
    // reading it now buys a re-chunk of a file that is not finished.
    //
    // Nothing is lost, and the stamp map is the whole reason: what
    // gets recorded here is the PREVIOUS fingerprint — the version
    // actually in the index — never the current one. The next run
    // therefore still sees the file as changed and reads it. Recording
    // the current stamp instead would mark the file up to date without
    // ever having read it, and its new content would be unreachable
    // until some later write moved the mtime again. A transcript never
    // seen before records nothing at all, which says the same thing.
    //
    // "Nothing is lost" holds only because a later run has an index to
    // pick the file up INTO. `Deferral::Forbidden` is the caller saying
    // it does not — see `Deferral` — and it is the other half of this
    // condition rather than a refinement of it.
    if settling == Settling::Wait && defer == Deferral::Allowed {
        if let Some(age) = TranscriptLoader::settling_for(path) {
            mine.found.deferred.push(uri.clone());
            if let Some(prev) = stamps.get(&uri) {
                mine.fresh.insert(uri, prev.clone());
            }
            // Both channels, exactly like the loader-failure arm
            // below: stderr for the `db.log` that the detached
            // `SessionStart` indexer writes into, and the returned
            // record for `br8n status`. A skip nobody can see is the
            // house failure mode.
            let line = format!(
                "{}: still being written ({}s ago), waiting for {}s of quiet",
                path.display(),
                age.as_secs(),
                TranscriptLoader::SETTLE.as_secs()
            );
            eprintln!("br8n: deferring {line}");
            mine.found.skipped.push(line);
            return;
        }
    }
    // THE POOL-WIDE QUERY GATE, and it is now every source type rather than
    // only PDFs.
    //
    // Positioned HERE, after both early returns above, rather than at the
    // top of the function or in the worker loop that calls it: the unchanged
    // fast path above never opens the file, and CLAUDE.md states "a no-op
    // run costs 0s" — gating before that path would make an unchanged file
    // pay a `read_dir` of the database directory and park on work that was
    // never going to happen. `SessionStart` fires an index every session, so
    // most files on most runs take that fast path. Putting the gate here
    // instead means it only ever blocks a document about to actually be
    // read.
    //
    // `yield_to_queries` does not compound: every worker polls the SAME
    // filesystem predicate and returns within one 100ms poll of the marker
    // going stale, which the embed pool at index.rs:414 already demonstrates
    // with four workers. What used to break the contract was the LENGTH of a
    // unit — one uninterruptible OCR call of minutes, during which parking
    // meant nothing. Phase 1 no longer recognizes, so every unit here is a
    // parse and the workers park together at boundaries that arrive quickly.
    //
    // Phase 2 does recognize, and its OCR runs behind OCR_GATE one document
    // at a time, so it keeps the old single-threaded shape this contract was
    // written for.
    yield_to_queries(db, std::time::Duration::from_secs(5));
    let loaded = match kind {
        Kind::Markdown => MarkdownLoader::load_file(path).map(|doc| crate::loaders::Loaded {
            doc,
            pending_ocr: Vec::new(),
            ocr_attempted: false,
        }),
        Kind::Session(agent) => agent.load_session(path).map(|doc| crate::loaders::Loaded {
            doc,
            pending_ocr: Vec::new(),
            ocr_attempted: false,
        }),
        Kind::Pdf => PdfLoader::load_file_reporting(path, &pdf_config_for(pass, &cfg.pdf)),
    };
    // Whether the USER wants recognition at all, which is a different question
    // from whether THIS pass performs it (`pass`).
    //
    // Every deferral below is conditional on this, because `reindex_swap_inner`
    // gates phase 2 on exactly the same expression. Withholding a stamp for a
    // pass that will never run leaves a backlog nothing can drain: a fully
    // scanned PDF keeps `ocr_backlog > 0` forever, which blocks the
    // unchanged-corpus fast path on EVERY run (a seed copy, a rebuild and a
    // full pack build on every `SessionStart`), and a mixed PDF is re-read and
    // re-chunked every run because it never receives a current stamp. `off`
    // promises the pre-OCR behaviour exactly: dropped pages are reported, the
    // document is stamped, and nothing is queued.
    let ocr_wanted = cfg.pdf.ocr != crate::config::OcrMode::Off;
    match loaded {
        Ok(loaded) => {
            // A document that still owes recognition must NOT record a
            // current stamp: the stamp gap is the entire backlog
            // mechanism, and recording one here marks the file up to
            // date without ever having read those pages. Carrying the
            // PREVIOUS stamp forward is what `Settling::Wait` does for
            // a live transcript, for the same reason.
            //
            // In a `Run` pass the file has normally had its turn, so it is
            // stamped whatever is still pending — unless recognition was
            // never actually ATTEMPTED (the `OCR_DISABLED` latch tripped on an
            // earlier document), in which case this file has had no turn at
            // all and the gap must stay open.
            let owes_recognition = ocr_wanted
                && !loaded.pending_ocr.is_empty()
                && match pass {
                    OcrPass::Skip => true,
                    OcrPass::Run => !loaded.ocr_attempted,
                };
            if owes_recognition {
                mine.found.ocr_pending.push(uri.clone());
                if let Some(prev) = stamps.get(&uri) {
                    mine.fresh.insert(uri.clone(), prev.clone());
                }
            } else if let Some(n) = now {
                mine.fresh.insert(uri.clone(), n);
            }
            mine.found.docs.push(loaded.doc);
        }
        Err(e) => {
            // A fully scanned PDF has no good pages at all, so it
            // produces no document — but it is still a file the OCR
            // pass should try, and the stamp gap is how it gets
            // another turn.
            //
            // `Some(attempted)` is a scanned failure and carries whether
            // recognition was actually run; `None` is any other loader
            // failure, which is not this case at all.
            let scanned = match e.downcast_ref::<crate::loaders::pdf::PdfError>() {
                Some(crate::loaders::pdf::PdfError::Scanned { ocr_attempted, .. }) => {
                    Some(*ocr_attempted)
                }
                _ => None,
            };
            // A `Run` pass re-queues a scanned PDF in exactly one case, and it
            // is the case with the data-loss consequence. `OCR_DISABLED` is
            // process-wide, so on a machine missing PDFium or ONNX Runtime the
            // FIRST scanned PDF trips the latch and every one after it is
            // never attempted — same `Err(Scanned)`, no recognition behind it.
            // Left out of `ocr_pending` those URIs vanish from `live_uris`,
            // and `prune_missing` DELETES the indexed copy; the anti-retry
            // stamp below then makes that deletion PERMANENT, recoverable only
            // by `--reindex`. Keeping the URI here is what holds the pruner
            // off, exactly as it does in phase 1.
            let requeue = ocr_wanted
                && match (scanned, pass) {
                    (Some(_), OcrPass::Skip) => true,
                    (Some(attempted), OcrPass::Run) => !attempted,
                    (None, _) => false,
                };
            if requeue {
                mine.found.ocr_pending.push(uri.clone());
                if let Some(prev) = stamps.get(&uri) {
                    mine.fresh.insert(uri.clone(), prev.clone());
                }
            } else if scanned == Some(true) && pass == OcrPass::Run {
                // THE ANTI-RETRY RULE, and the one place this
                // deliberately departs from the old behaviour — but
                // only for the ONE failure it exists for: a PDF that
                // was genuinely attempted with recognition enabled and
                // still yielded nothing (`PdfError::Scanned`, no
                // PDFium/ONNX Runtime, or a page that stays image-only
                // after OCR). A loader failure normally records no
                // stamp, so the file is reconsidered next run. With a
                // second pass that becomes an infinite retry for THIS
                // failure: a scanned PDF on a machine with no ONNX
                // Runtime would be attempted every session forever.
                // Recognition was genuinely attempted here, so the
                // file is marked seen and is retried when it CHANGES,
                // not on a loop. It is reported in `skipped` either
                // way.
                //
                // "Genuinely attempted" is now CHECKED (`Some(true)`)
                // rather than assumed from the pass. It used to be
                // assumed, and a latched-off run therefore stamped a
                // file recognition had never touched.
                //
                // Any OTHER failure in a `Run` pass — a locked file, a
                // partial write, an out-of-memory, a failed markdown
                // or transcript load — is NOT this case, and must
                // fall through to recording no stamp at all, exactly
                // like an ordinary incremental pass. Recording a
                // stamp for a transient failure would make the file
                // silently disappear from the index until its mtime
                // or size next changes.
                if let Some(n) = now {
                    mine.fresh.insert(uri.clone(), n);
                }
            }
            eprintln!("br8n: skipping {}: {e}", path.display());
            mine.found.skipped.push(format!("{}: {e}", path.display()));
        }
    }
}

/// Three-argument shim over `discover_stat_first_with`, fixed at `OcrPass::Skip`.
///
/// Exists because thirteen call sites in `tests/` use this shape, and because
/// `Skip` is the answer every caller but phase 2 wants — including `discover`,
/// below. See `discover_stat_first_with` for what `stamps`, `defer`, and `pass`
/// each mean.
pub fn discover_stat_first(
    cfg: &Config,
    stamps: &std::collections::HashMap<String, String>,
    defer: Deferral,
) -> Result<(Discovered, std::collections::HashMap<String, String>)> {
    discover_stat_first_with(cfg, stamps, defer, OcrPass::Skip)
}

/// Walk the configured sources, reading only what changed since last time.
///
/// `stamps` is the previous run's stat map, keyed by URI. A file whose mtime
/// and size both match is not opened at all.
///
/// `defer` is an argument rather than an inference from `stamps` because the
/// two states that must behave differently look identical from in here: a
/// first-ever index and a `--reindex` both arrive with an empty map. The first
/// may safely defer (nothing is lost — a later run picks the file up), the
/// second may not (it publishes over a live index that HAD the file). Only the
/// caller knows which it is, so only the caller may decide. See `Deferral`.
///
/// `pass` says whether THIS call may recognize scanned PDF pages. `Skip`
/// loads PDFs from the text layer only and records anything left unread in
/// `Discovered::ocr_pending`, carrying its PREVIOUS stamp forward so a later
/// `Run` pass sees it as still changed and reads it again. `Run` recognizes as
/// `[pdf] ocr` directs, and never populates `ocr_pending` by construction. See
/// `OcrPass`.
pub fn discover_stat_first_with(
    cfg: &Config,
    stamps: &std::collections::HashMap<String, String>,
    defer: Deferral,
    pass: OcrPass,
) -> Result<(Discovered, std::collections::HashMap<String, String>)> {
    let roots = crate::loaders::transcript::SessionRoots::from_env();
    discover_stat_first_at(cfg, stamps, defer, pass, &roots)
}

pub fn discover_stat_first_at(
    cfg: &Config,
    stamps: &std::collections::HashMap<String, String>,
    defer: Deferral,
    pass: OcrPass,
    sessions: &crate::loaders::transcript::SessionRoots,
) -> Result<(Discovered, std::collections::HashMap<String, String>)> {
    // Validate roots BEFORE walking. An unreadable root is a broken config or an
    // unmounted drive; walking it yields zero documents, and `prune_missing`
    // would then delete every document that lives under it. A root that exists
    // but is empty is a different thing — the user really did delete those
    // notes. Existence is the discriminator; emptiness is not.
    for root in &cfg.sources {
        anyhow::ensure!(
            root.exists(),
            "configured source `{}` does not exist. Fix `sources` in {}, or remove \
             the entry. Refusing to index, because pruning against a partial scan \
             would delete everything under this path.",
            root.display(),
            Config::config_path().display()
        );
    }

    let mut found = Discovered::default();
    // The seed the workers' partials merge INTO. Empty here and never written
    // between here and the merge — `HashMap::new()` rather than
    // `Default::default()` so that says itself, since a reader otherwise has to
    // scan forward to learn that nothing is being carried in.
    let fresh: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    // Yielding is per DOCUMENT: `process_pdf_with_ocr` is one batch call, so a
    // single very large scanned PDF remains one uninterruptible unit.
    let db_for_yield = Config::db_path();

    // Walk first, load second. The walk is stat-bound and cheap; loading is
    // what costs. Collecting also turns the work into a plain slice, so the
    // pool below is the same shape as `index_documents_with`'s.
    let mut ignored = 0usize;
    let mut work: Vec<(std::path::PathBuf, Kind)> = Vec::new();
    // Not `cfg.sources` — see `distinct_roots`. The existence check above
    // still runs against the raw list, so a broken entry still refuses; this
    // call refuses too, on the narrower failure `exists()` cannot see.
    let roots = distinct_roots(&cfg.sources)?;
    for root in &roots {
        for entry in ignore::WalkBuilder::new(root).build().flatten() {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let p = entry.path();
            if crate::loaders::markdown::is_ignored(p, &cfg.ignore) {
                ignored += 1;
                continue;
            }
            match p.extension().and_then(|e| e.to_str()) {
                // No yield here any more: the walk only stats, and the gate
                // moved onto the loading loop below where the cost actually is.
                Some("md") => work.push((p.to_path_buf(), Kind::Markdown)),
                Some("pdf") => work.push((p.to_path_buf(), Kind::Pdf)),
                _ => {}
            }
        }
    }
    let max_transcript_age = cfg
        .index_transcripts_max_age_days
        .map(|days| std::time::Duration::from_secs(u64::from(days) * 86_400));
    for (agent, transcripts) in sessions.enabled(cfg) {
        if !transcripts.exists() {
            continue;
        }
        for entry in ignore::WalkBuilder::new(transcripts).build().flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(max_age) = max_transcript_age {
                let age = std::fs::metadata(p)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|m| m.elapsed().ok());
                if age.is_some_and(|age| age > max_age) {
                    continue;
                }
            }
            work.push((p.to_path_buf(), Kind::Session(agent)));
        }
    }

    // Printed, not silent. An exclusion nobody can see is indistinguishable
    // from an empty directory, and this tool's failure mode is silence.
    // `found.ignored` is set in here too, not on a line after: `found` starts
    // at 0, so nesting the assignment costs nothing when `ignored == 0`, and
    // it means this whole block cannot be deleted for looking like dead
    // reporting without also zeroing the count a test asserts on.
    if ignored > 0 {
        eprintln!(
            "br8n: skipped {ignored} file(s) matching `ignore = {:?}`",
            cfg.ignore
        );
        found.ignored = ignored;
    }

    let workers = cfg.embed.concurrency.max(1).min(work.len().max(1));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let work = &work;
    let partials: Vec<Partial> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                let next = &next;
                let db_for_yield = &db_for_yield;
                scope.spawn(move || {
                    let mut mine = Partial::default();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some((path, kind)) = work.get(i) else {
                            break;
                        };
                        // The query gate now lives inside `consider_into`,
                        // after its unchanged-fast-path and settling-deferral
                        // returns — see the comment there for why.
                        consider_into(
                            &mut mine,
                            cfg,
                            db_for_yield,
                            stamps,
                            defer,
                            pass,
                            path,
                            *kind,
                        );
                    }
                    mine
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                // A panicked worker FAILS THE RUN. It must not degrade to a
                // partial corpus: `prune_missing` deletes what discovery did
                // not return, so silently losing one worker's share would
                // delete those documents from the index.
                h.join()
                    .map_err(|_| anyhow::anyhow!("a document loader panicked"))
            })
            .collect::<Result<Vec<_>>>()
    })?;

    let mut merged = Partial { found, fresh };
    for p in partials {
        merged.merge(p);
    }
    let mut found = merged.found;
    let fresh = merged.fresh;

    found.docs.sort_by(|a, b| a.uri.cmp(&b.uri));
    found.unchanged.sort();
    found.deferred.sort();
    // Sorted for the same reason as the three above: `skipped` is persisted
    // with `set_meta` and rendered by `br8n status`, so walk order is
    // user-visible output. Parallel loading makes walk order nondeterministic.
    found.skipped.sort();
    found.ocr_pending.sort();
    Ok((found, fresh))
}

/// `discover_reporting`, discarding the skip record. For callers that only
/// need the documents.
///
/// `Deferral::Forbidden`, and that is not a detail. This enumerates the whole
/// corpus for a caller that is not publishing an index — `br8n add` uses it so
/// `resolve_links` can resolve against every document rather than the one being
/// added. Deferring here would silently drop every transcript touched in the
/// last ten minutes, INCLUDING the session the user is typing the `br8n add`
/// into, and the caller has no way to see that its corpus came back short.
pub fn discover(cfg: &Config) -> Result<Vec<Document>> {
    Ok(
        discover_stat_first(cfg, &Default::default(), Deferral::Forbidden)?
            .0
            .docs,
    )
}

/// Prevents two `br8n index` runs at once.
///
/// The shadow swap solves reader-vs-writer; this solves writer-vs-writer. They
/// are separate problems and the swap does not subsume this one.
///
/// Two indexers used to be serialised only by accident: both opened the same
/// `db.new`, so LadybugDB's file lock rejected the second. But the `copy_dir`
/// seeding and the `remove_dir_all`/`rename` orchestration all run OUTSIDE that
/// lock. A peer reproduced the resulting damage on a 20-note corpus with two
/// staggered indexers: B failed with a bare `No such file or directory` — an
/// `io::Error`, not a LadybugDB message — and afterwards only `db.old` survived.
/// `br8n status` then reported 0 documents. B's `remove_dir_all(&shadow)` had
/// deleted A's in-progress shadow after A already renamed `live` to `old`, so
/// A's final rename found nothing to move.
///
/// Hence: acquire before the FIRST filesystem mutation, not merely before the
/// database is opened.
///
/// A lock whose owning pid is gone is stale and gets reclaimed. A crashed run
/// must not wedge indexing forever and leave the user's notes silently
/// unsearchable with no explanation.
pub struct IndexLock(std::path::PathBuf);

impl IndexLock {
    pub fn acquire(db: &std::path::Path) -> Option<IndexLock> {
        let path = db.with_extension("lock");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let staged = StagedPidfile::write(&path)?;
        for _ in 0..2 {
            match std::fs::hard_link(&staged.0, &path) {
                Ok(()) => return Some(IndexLock(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if IndexLock::is_held(db) {
                        return None;
                    }
                    let _ = std::fs::remove_file(&path);
                }
                Err(_) => return None,
            }
        }
        None
    }
}

struct StagedPidfile(std::path::PathBuf);

impl StagedPidfile {
    fn write(lock: &std::path::Path) -> Option<StagedPidfile> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let pid = std::process::id();
        let staged = StagedPidfile(lock.with_extension(format!("lock.{pid}.{n}")));
        std::fs::write(&staged.0, pid.to_string()).ok()?;
        Some(staged)
    }
}

impl Drop for StagedPidfile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl IndexLock {
    /// Is another process holding the writer lock right now?
    pub fn is_held(db: &std::path::Path) -> bool {
        std::fs::read_to_string(db.with_extension("lock"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .is_some_and(pid_alive)
    }
}

/// Public wrapper so the CLI can discard a progress file left by a dead run.
pub fn pid_is_alive(pid: u32) -> bool {
    pid_alive(pid)
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Is a process with this pid running?
///
/// Errs toward "alive": a false negative steals the lock from a running
/// indexer, which is the failure this whole mechanism exists to prevent, while
/// a false positive only makes the next run skip.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // Signal 0 performs error checking without sending a signal.
    if unsafe { libc::kill(pid as i32, 0) } == 0 {
        return true;
    }
    // EPERM means the process EXISTS but belongs to another user — we are not
    // permitted to signal it. Treating that as dead let one user's `br8n`
    // reclaim a lock another user's live indexer was holding. Only ESRCH ("no
    // such process") actually means dead.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Without `kill`, liveness cannot be probed. Returning `false` here declared
/// every lock stale, so the lock did nothing at all on these platforms;
/// returning `true` makes a crashed run require one manual lockfile deletion
/// instead of silently allowing concurrent writers.
#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true
}

#[cfg(unix)]
pub fn lower_priority() {
    let who = 0 as libc::id_t;
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, who, 10);
    }
    if std::env::var_os("BR8N_REPORT_NICENESS").is_some() {
        let niceness = unsafe { libc::getpriority(libc::PRIO_PROCESS, who) };
        eprintln!("br8n: niceness {niceness}");
    }
}

#[cfg(not(unix))]
pub fn lower_priority() {}

/// Recursively copies a directory. Seeds the shadow index from the live one so
/// incremental indexing keeps working across the swap, and takes the
/// consistent snapshot `br8n::backup` archives — both callers need
/// `graph.kz` and `graph.kz.wal` copied as a pair.
pub fn copy_dir(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("create {}", dst.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .with_context(|| format!("copy {} -> {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

/// Which corpus `reindex_swap_with` builds the shadow from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildMode {
    /// Seed the shadow from the live index, discover what changed on disk,
    /// and write only that. What a plain `br8n index` does.
    Incremental,
    /// Discard the existing index: skip the seed copy, so every document is
    /// re-read from disk and re-embedded. What `--reindex` means.
    FromScratch,
    /// Rebuild the shadow from the LIVE STORE's own rows — never the corpus,
    /// never the embedder — reusing every stored embedding verbatim. What
    /// `--compact` means: lbug cannot reclaim space on its own (`CHECKPOINT`
    /// frees 0 bytes, `VACUUM` does not exist), so this is the only way to
    /// reclaim it, and it must not cost a single embedding round trip.
    Compact,
}

/// Builds a complete index in a sibling directory, then swaps it in with a
/// rename. Readers keep the old database open until the instant of the swap.
///
/// This exists because LadybugDB takes an exclusive OS file lock: a second
/// process cannot open the database at all while an indexer holds it. Indexing
/// in place therefore disables the prompt hook for the entire run, silently —
/// the hook still exits 0 and simply returns nothing. Measured: three probes
/// during an in-place reindex all returned 0 bytes.
pub fn reindex_swap(cfg: &Config) -> Result<IndexStats> {
    reindex_swap_with(cfg, RebuildMode::Incremental, true)
}

/// `reindex_swap`, with the option to discard the existing index or to
/// compact it instead of touching the corpus at all — see `RebuildMode`.
///
/// `FromScratch` skips the seed copy, so the shadow is built from nothing and
/// every document is re-read and re-embedded. It deliberately does NOT delete
/// the live index first: doing that before the lock was taken destroyed the
/// directory a concurrent indexer was mid-swap on. Not seeding achieves the
/// same result and stays inside the lock, and the live index remains readable
/// for the whole rebuild.
///
/// `embed = false` is `--no-embed`: phase 1 of an asynchronous index. It is
/// ignored for `RebuildMode::Compact`, which never touches the embedder at
/// all regardless — there is nothing for it to skip.
pub fn reindex_swap_with(cfg: &Config, mode: RebuildMode, embed: bool) -> Result<IndexStats> {
    reindex_swap_inner(cfg, mode, embed, OcrPass::Skip)
}

/// Body of `reindex_swap_with`, with the OCR pass made explicit so phase 2
/// (`ocr_pass`) can call back into it without exposing a fourth argument on
/// the public function. `pass` is threaded into discovery and used to guard
/// phase 2 itself — see the `pass == OcrPass::Skip` check below, which is
/// what stops phase 2 from starting a phase 3.
fn reindex_swap_inner(
    cfg: &Config,
    mode: RebuildMode,
    embed: bool,
    pass: OcrPass,
) -> Result<IndexStats> {
    let live = Config::db_path();

    // Before ANY filesystem mutation — see IndexLock's note on where the race is.
    // Held for the whole build-and-swap; released on drop, including error paths.
    let _lock = IndexLock::acquire(&live).ok_or_else(|| {
        anyhow::anyhow!("another `br8n index` is already running; skipping this run")
    })?;
    sweep_dead_query_markers(&live);

    match crate::usage::fold(&live) {
        Ok(0) => {}
        Ok(n) => eprintln!("br8n: folded {n} usage records"),
        Err(e) => eprintln!("br8n: could not fold usage records — {e:#}"),
    }

    if mode == RebuildMode::Compact {
        // Compaction never touches the corpus, so none of the discovery /
        // stamp-fingerprint machinery below applies to it at all — it reads
        // the live store and nothing else. See `compact_swap`.
        return compact_swap(cfg, &live);
    }
    let embedder = crate::embed::for_config(&cfg.embed)?;
    let from_scratch = mode == RebuildMode::FromScratch;

    // Cheap pass first, before touching a single byte on disk.
    //
    // A run where nothing changed used to cost a 231MB directory copy and a
    // 157MB re-parse of the transcript corpus, every time — and `SessionStart`
    // fires one on every session. Statting the corpus against the previous
    // run's fingerprints costs a few hundred `stat` calls, and when it comes
    // back empty there is nothing to build and nothing to swap.
    // `--reindex` means "discard the existing index and rebuild from nothing".
    // Loading the real stamp map here would make `discover_stat_first` see
    // every document as unchanged (their mtimes/sizes have not moved), so
    // `probe.docs` would come back empty while the shadow starts genuinely
    // empty (seeding is skipped below for `from_scratch`) — publishing an
    // empty index over the real one. An empty stamp map makes every document
    // look new instead, so `from_scratch` actually re-reads the corpus.
    let stamps = if from_scratch {
        std::collections::HashMap::new()
    } else {
        load_stamps(&live)
    };
    // And for the same reason, `from_scratch` must not DEFER either. The empty
    // stamp map above makes every live transcript look changed; deferring one
    // then means it is missing from a shadow that was built from nothing, so
    // "left for a later run" is really "deleted, and not back until the
    // session ends". `live_uris` cannot save it — `prune_missing` runs against
    // a store that never contained it. See `Deferral`.
    let defer = if from_scratch {
        Deferral::Forbidden
    } else {
        Deferral::Allowed
    };
    let (probe, probe_stamps) = discover_stat_first_with(cfg, &stamps, defer, pass)?;
    // Kept across the publish below: the second pass runs only if this one
    // left work, and only after the index this run built is already live.
    let ocr_backlog = probe.ocr_pending.len();

    // The stamp map is a complete record of what was on disk last run, so it
    // answers the deletion question too: if the new map has exactly the same
    // keys, nothing was added, changed OR removed. No database access needed,
    // which is the point — the pre-flight must not take the store's lock.
    let unchanged_corpus = !stamps.is_empty()
        && probe.docs.is_empty()
        && probe_stamps.len() == stamps.len()
        && probe_stamps.keys().all(|k| stamps.contains_key(k));

    // The pack is a separate artifact from the database and from the stamp
    // file: an upgrade can land on a machine whose corpus has not changed
    // (so the fast path above looks free) but whose live index predates the
    // pack ever being built. Taking the fast path there would leave `db.stamps`
    // populated forever with no manifest on disk, and `Pack::open` refuses to
    // read without one — so retrieval would return nothing, forever, with the
    // hook still exiting 0. One `exists()` call keeps the genuine no-op free.
    let pack_present = live.join(crate::pack::manifest::MANIFEST_FILE).exists();

    // Deferred transcripts carry their PREVIOUS fingerprint forward (see
    // `consider`), so a run whose only change is a live session's own
    // transcript still lands on this fast path: the key sets match and
    // `probe.docs` is empty. That is the intended shape — the deferral is
    // supposed to make the common case free, not to force a full rebuild. The
    // count below has to include them or the summary under-reports.
    //
    // KNOWN GAP, and it is a reporting one. Persisting the skip record is a
    // `set_meta` write, which needs the store, which takes LadybugDB's
    // exclusive file lock — the very thing this fast path and `db.stamps`
    // exist to avoid taking on a no-op run (see `stamp_path`'s note on what
    // reading stamps from `IndexMeta` cost). So on a run where a live
    // transcript is the ONLY change, `br8n status` still shows the previous
    // run's skip list, not this one's.
    //
    // AND THAT RUN IS NOT THE EXCEPTION — in the steady state this deferral
    // was built for it is EVERY run, so the gap is PERMANENT, not occasional.
    // An earlier draft of this comment said "on a run where…", which reads as
    // rare. It is not: N live sessions with nothing else changing is exactly
    // the case the deferral targets, and then every run reaches this return,
    // no run ever has real work, and `br8n status` reports `skipped: none`
    // for as long as that lasts while N transcripts are deferred. Reproduced.
    // The only surviving channel in that state is stderr — and therefore
    // `db.log`, where the `SessionStart` indexer's output goes — which carries
    // `br8n: deferring …` on every such run;
    // `the_deferral_line_reaches_stderr_on_a_real_run` pins that, and it is
    // load-bearing precisely because it is the only channel left. `br8n
    // status` catches up as soon as any run has real work to do. Closing the
    // gap properly means giving `status` a lock-free channel to read — a side
    // file like `db.progress` — not opening the store here.
    // A PDF that pass one could not read past its text layer never reaches
    // `probe.docs` and is NEW, so it has no previous stamp to omit — the key
    // sets above match and `unchanged_corpus` reads true even though the file
    // is sitting in `ocr_pending`, wholly unrecognised. Without this clause,
    // dropping a fully-scanned PDF into an otherwise-settled corpus takes this
    // fast path forever: not this run, not any later run, until some other
    // file changes and drags the corpus onto the slow path incidentally.
    // `SessionStart` firing an index on every session is exactly why a genuine
    // no-op must stay free, but a backlog is work outstanding, not a no-op —
    // only phase 2 below can clear it, so a nonzero backlog must always reach
    // that block.
    if !from_scratch && unchanged_corpus && pack_present && ocr_backlog == 0 {
        return Ok(IndexStats {
            skipped: probe.unchanged.len() + probe.deferred.len(),
            ..Default::default()
        });
    }

    // Filled by the build below and written only after the swap succeeds.
    let fresh_stamps: std::collections::HashMap<String, String>;
    let shadow = live.with_extension("new");
    let _ = std::fs::remove_dir_all(&shadow);

    // Seed from the live index so stored content hashes survive. Without this
    // every document looks new and every chunk is re-embedded on every run:
    // three consecutive runs over an unchanged 25-note corpus each reported
    // `25 added, 0 skipped` at ~10s instead of skipping all 25 in under a
    // second. `SessionStart` indexes every session, so that is permanent
    // overhead. The copy is negligible beside it — 4.3 MB in ~0.00s.
    if live.exists() && !from_scratch {
        copy_dir(&live, &shadow)?;
    }

    let indexed_docs: std::collections::HashSet<String>;
    let stats = {
        let store = Store::open(&shadow, cfg.embed.dimensions)?;
        let idx = Indexer::new(store, embedder, cfg.clone())
            .with_progress(live.with_extension("progress"));
        // Reuse the probe: discovery already ran above and re-running it would
        // parse every changed file a second time.
        let found = probe;
        fresh_stamps = probe_stamps;
        let uris = found.live_uris();
        let mut stats = idx.index_documents_with(&found.docs, embed)?;
        // Files the stat pass skipped never reach `index_documents`, so it
        // cannot count them. Without this the summary reports "0 skipped" on a
        // run that skipped hundreds.
        stats.skipped += found.unchanged.len() + found.deferred.len();
        // Only the changed documents need their links rebuilt. Unchanged ones
        // still have theirs, and `resolve_links` resolves against the whole
        // store rather than the slice it is handed.
        idx.resolve_links(&found.docs)?;
        idx.prune_missing(&uris)?;
        // Written after the swap succeeds, not here — a stamp map saved for an
        // index that never landed would make the next run skip real work.
        let skipped = found.skipped;
        // Persist rather than print. The `SessionStart` indexer's stderr goes to
        // a log file the user never opens; `br8n status` is where they look.
        let _ = idx.store().set_meta(
            "skipped",
            &serde_json::to_string(&skipped).unwrap_or_default(),
        );

        // Build the pack INTO the shadow directory, so the rename below
        // publishes the database and the pack as one generation. Built after
        // the swap instead, there would be a window where a reader sees a new
        // database and a stale pack — and the row ordinal is the only thing
        // tying vectors to records, so that reader gets correct scores on the
        // wrong documents, silently.
        //
        // A pack that fails to build must abort the swap. Publishing a database
        // with no pack would leave retrieval reading nothing while the index
        // looks healthy.
        let pack_rows = idx.store().all_rows_for_pack()?;
        indexed_docs = doc_ids_of(&pack_rows);
        // Read AFTER `resolve_links` above, so the counts describe the edges
        // this generation actually publishes. They go into `pack.links` so
        // authority weighting can run without opening the database — see
        // `pack::links`.
        let inbound = idx.store().inbound_link_counts()?;
        // Same reasoning as `inbound` above: read after `resolve_links`, from
        // the generation this publish actually describes, and carried onto
        // the pack so the prompt path can rank by lifecycle without opening
        // the database — see `pack::status`.
        let lifecycles = idx.store().all_lifecycles(&inbound)?;
        crate::pack::Pack::build_with_usage(
            &shadow,
            &idx.embedder().model_id(),
            cfg.embed.dimensions,
            pack_rows,
            &inbound,
            &lifecycles,
            &usage_for_pack(&live),
        )?;

        stats
        // `idx` drops here, releasing the shadow database's file lock before
        // the rename. Renaming a directory whose lock is still held leaves the
        // swapped-in copy unopenable.
    };

    publish_shadow(&live, &shadow)?;
    save_stamps(&live, &fresh_stamps);
    prune_usage_to(&live, &indexed_docs);
    // Clear the progress file on the success path too. `publish(true)` already
    // removes it, but a failure between there and here would leave a stale
    // file claiming a run is still going.
    let _ = std::fs::remove_file(live.with_extension("progress"));

    // PHASE 2. The index is already published, so from here nothing this does
    // can lose the run — but `IndexLock` is a pidfile keyed by PID, not a
    // reentrant lock, so `_lock` above must be dropped EXPLICITLY here: it is
    // a plain local still in scope at this point, and `ocr_pass` below opens
    // its own `IndexLock` on the same path. Left implicit, that acquire reads
    // the pidfile THIS process just wrote, sees its own pid via `pid_alive`,
    // and refuses — phase 2 would fail every single time with "another
    // `br8n index` is already running", never actually running. Verified by
    // acquiring the same path twice in one process: the second call returns
    // `None`.
    drop(_lock);
    //
    // Not a queue and not a worker — a second ordinary incremental pass. Phase
    // 1 withheld a current stamp from every PDF that still owed pages, so this
    // pass sees exactly those as changed and everything else as unchanged, and
    // reads exactly those with recognition on.
    //
    // Guarded hard, because a second pass is not cheap: it pays another seed
    // copy, another stat walk, and another full pack build (which now includes
    // `all_lifecycles`). A corpus with no scanned pages must never reach it.
    // `pass == OcrPass::Skip` is what stops phase 2 from starting a phase 3.
    if pass == OcrPass::Skip && ocr_backlog > 0 && cfg.pdf.ocr != crate::config::OcrMode::Off {
        eprintln!("br8n: {ocr_backlog} PDF(s) still need recognition; running the OCR pass");
        match ocr_pass(cfg, embed) {
            // Phase 2's counts are NOT folded into `stats`, and must not be:
            // `stats` describes the generation this call published, and phase
            // 2 published a different one after it. But the caller prints
            // `stats` and nothing else, so without this line every document
            // recovered by recognition is invisible in the reported totals —
            // silent under-reporting on the one path this feature adds, in a
            // codebase whose house failure mode is silence. Stderr rather than
            // stdout because that is where the rest of phase 2's output goes,
            // and because the `SessionStart` indexer's stderr is what reaches
            // `db.log`.
            Ok(s) => eprintln!(
                "br8n: OCR pass: {} added, {} updated, {} chunks ({} written, {} reused)",
                s.added, s.updated, s.chunks, s.chunks_written, s.chunks_reused
            ),
            // Never fatal. The index is live and correct without this; the
            // backlog survives in the stamp gap and the next run retries it.
            Err(e) => eprintln!("br8n: OCR pass did not complete: {e}"),
        }
    }

    Ok(stats)
}

/// Recognize the PDFs phase 1 left, and republish.
///
/// Deliberately a plain call back into the incremental path rather than a
/// bespoke pipeline: the stamp gap has already narrowed the work to exactly
/// the pending files, so the ordinary pass IS the OCR pass. The only
/// difference is the policy handed to discovery.
///
/// `embed` is threaded through from the caller's own choice rather than
/// hardcoded — a user who ran `br8n index --no-embed` deliberately deferred
/// embedding (Ollama may not even be running), and phase 2 must not embed
/// behind that choice just because recognition finished. It reuses whatever
/// `embed` `reindex_swap_inner` was already called with.
///
/// Runs with `IndexLock` free. If a second `br8n index` has taken it, this
/// returns that error, the caller logs it, and the backlog survives untouched
/// in the stamp gap for the next run.
fn ocr_pass(cfg: &Config, embed: bool) -> Result<IndexStats> {
    reindex_swap_inner(cfg, RebuildMode::Incremental, embed, OcrPass::Run)
}

/// What one `backfill_vectors` call did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BackfillStats {
    /// Chunks embedded and written this call.
    pub embedded: usize,
    /// Chunks still without a vector when this call returned — either the
    /// `budget` ran out, or `IndexLock` was contended and no work happened
    /// at all. `0` means the backlog is fully drained.
    pub remaining: usize,
    /// Whether the pack was rebuilt and republished this call. Only true
    /// when `remaining` reached `0` as a direct result of this call's own
    /// work — never on a call that found the backlog already empty and did
    /// nothing, which would otherwise republish on every idle poll.
    pub republished: bool,
}

/// How many chunks one embedding round trip handles. Bounds both the request
/// sent to Ollama and how long a single `IndexLock` slice runs for — see
/// `backfill_vectors`'s doc comment.
const BACKFILL_BATCH: usize = 64;

/// Phase 2 of an asynchronous index: embeds the backlog `--no-embed` (or a
/// previous, interrupted `--backfill`) left behind, then republishes the pack
/// once every chunk has a vector.
///
/// Every batch gets its OWN `IndexLock` acquire/release AND its own open/close
/// of the live `Store` — never one lock, and never one connection, held for
/// the whole run. Two different exclusions are at stake, and both need this:
/// `IndexLock` only keeps a second `br8n index` from writing at the same
/// time, but it is LadybugDB's OWN file lock that actually blocks a reader
/// (the hook, `br8n status`, `br8n search`) from opening the database AT
/// ALL while any process holds it open (see `Store::open_existing`'s doc
/// comment on the shadow swap existing for exactly this reason). Releasing
/// `IndexLock` between batches while holding one `Store` open across all of
/// them would leave the corpus just as unsearchable for the whole run as the
/// 86-minute synchronous embed this task exists to replace — dropping the
/// `Store` itself at the end of every batch, not merely the pidfile, is what
/// actually gives a waiting reader its window.
///
/// The pending set is read fresh from the store on every batch
/// (`Store::chunks_without_vectors`) — never carried in memory across
/// batches, and never across calls. A process killed mid-run leaves no
/// progress to lose, because none of it ever lived anywhere but the rows
/// already written back; the next call (or the next `SessionStart`) resumes
/// from exactly what the store holds, not from what this process remembers.
///
/// Runs until either the backlog is empty or `budget` elapses, whichever
/// comes first — a resumable slice, not an all-or-nothing transaction. Once
/// a call observes the backlog reach empty, it rebuilds and republishes the
/// pack via `reindex_swap_with(_, RebuildMode::Compact, _)` — the same
/// machinery `br8n index --compact` uses to rebuild from the live store's
/// own rows with no re-embedding, which is exactly the property this last
/// step needs: every chunk now has a stored vector, so compaction's
/// `all_rows_for_pack` pass reads a pack with `rows_with_vectors == rows`.
pub fn backfill_vectors(cfg: &Config, budget: std::time::Duration) -> Result<BackfillStats> {
    let live = Config::db_path();
    let start = std::time::Instant::now();
    let mut stats = BackfillStats::default();

    loop {
        if start.elapsed() >= budget {
            break;
        }
        let _lock = IndexLock::acquire(&live).ok_or_else(|| {
            anyhow::anyhow!("another `br8n index` is already running; skipping this run")
        })?;
        let store = Store::open(&live, cfg.embed.dimensions)?;
        let idx = Indexer::new(store, crate::embed::for_config(&cfg.embed)?, cfg.clone());
        idx.check_model()?;

        let pending = idx.store().chunks_without_vectors(BACKFILL_BATCH)?;
        if pending.is_empty() {
            // Nothing left — `idx` (and its `Store`) and `_lock` drop here.
            break;
        }

        let ids: Vec<String> = pending.iter().map(|(id, _)| id.clone()).collect();
        let texts: Vec<String> = pending.into_iter().map(|(_, text)| text).collect();

        // A prompt waiting on Ollama's single embedding slot takes priority —
        // the same mechanism the synchronous embed loop in
        // `index_documents_with` uses. See `yield_to_queries`.
        yield_to_queries(&live, std::time::Duration::from_secs(5));
        let vecs = idx.embedder().embed_documents(&texts)?;
        anyhow::ensure!(
            vecs.len() == ids.len(),
            "backfill: embedded {} vectors for {} chunks",
            vecs.len(),
            ids.len()
        );
        for (id, v) in ids.iter().zip(vecs.iter()) {
            idx.store().set_chunk_embedding(id, v)?;
        }
        stats.embedded += ids.len();
        // `idx` (its `Store`) and `_lock` drop at the end of this iteration,
        // before the next one re-acquires either — see the doc comment above
        // on why dropping the `Store` itself, not just the lock, is required.
    }

    let store = Store::open_existing(&live, cfg.embed.dimensions)
        .context("backfill: could not reopen the live index to check the backlog")?;
    stats.remaining = store.count_chunks_without_vectors()? as usize;
    drop(store);

    if stats.remaining == 0 && stats.embedded > 0 {
        // Every chunk now has a vector — rebuild and republish so `pack.vec`
        // appears. `RebuildMode::Compact` is the existing `--compact` path:
        // it rebuilds from the live store's own rows with no corpus re-read
        // and no re-embedding, which is exactly right here since embedding is
        // already done.
        reindex_swap_with(cfg, RebuildMode::Compact, true)?;
        stats.republished = true;
    }

    Ok(stats)
}

/// Swaps `shadow` into `live` with a double rename, and its ENOTEMPTY retry.
///
/// Factored out of `reindex_swap_with` so `compact_swap` publishes its own
/// shadow through the identical dance rather than a second copy of it that
/// could drift. The caller must have already dropped whatever `Store` built
/// the shadow — renaming a directory whose database lock is still held
/// leaves the swapped-in copy unopenable.
pub(crate) fn publish_shadow(live: &std::path::Path, shadow: &std::path::Path) -> Result<()> {
    let old = live.with_extension("old");
    let _ = std::fs::remove_dir_all(&old);
    if live.exists() {
        std::fs::rename(live, &old)?;
    }
    // Between the two renames `live` does not exist. A reader that opened the
    // database in that window used to CREATE an empty one there, and this
    // rename would then fail with ENOTEMPTY — aborting the swap and leaving
    // that empty database as the live index, with the real one stranded in
    // `.old`. Readers now open non-creating (`Store::open_existing`); this
    // retry covers whatever still loses the race. `old` is not removed until
    // the swap has actually succeeded, so a hard failure is recoverable.
    if let Err(first) = std::fs::rename(shadow, live) {
        let _ = std::fs::remove_dir_all(live);
        std::fs::rename(shadow, live).with_context(|| {
            format!(
                "could not swap the new index into {} ({first}); \
                 the previous index is intact at {}",
                live.display(),
                old.display()
            )
        })?;
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

/// Everything `compact_swap` needs from the live database, captured in one
/// pass so the `Store` handle here goes out of scope before the shadow index
/// is built.
#[doc(hidden)]
pub struct SourceSnapshot {
    pub model_id: String,
    pub meta: Vec<(String, String)>,
    pub docs: Vec<crate::store::query::DocumentRow>,
    pub chunks: Vec<crate::store::query::ChunkRow>,
    pub next_chunk: Vec<(String, String)>,
    pub links_to: Vec<(String, String, String)>,
    pub mentions: Vec<(String, String, String)>,
    pub derived_from: Vec<(String, String)>,
}

fn read_source(cfg: &Config, live: &std::path::Path) -> Result<SourceSnapshot> {
    let src = Store::open_existing(live, cfg.embed.dimensions)
        .context("compact: no live index to compact — run `br8n index` first")?;
    let model_id = src.get_meta("embed_model")?.ok_or_else(|| {
        anyhow::anyhow!(
            "compact: the live index has no `embed_model` recorded; run `br8n index` first"
        )
    })?;
    Ok(SourceSnapshot {
        model_id,
        meta: src.all_meta()?,
        docs: src.all_document_rows()?,
        chunks: src.all_chunk_rows()?,
        next_chunk: src.all_next_chunk_edges()?,
        links_to: src.all_links_to_edges()?,
        mentions: src.all_mentions_edges()?,
        derived_from: src.all_derived_from_edges()?,
    })
}

/// Test-only path to `read_source`; `compact_swap` stays private.
#[doc(hidden)]
pub fn read_source_for_test(cfg: &Config, live: &std::path::Path) -> Result<SourceSnapshot> {
    read_source(cfg, live)
}

fn doc_ids_of(
    rows: &[(crate::pack::records::Record, Vec<f32>)],
) -> std::collections::HashSet<String> {
    rows.iter()
        .map(|(record, _)| record.doc_id.clone())
        .collect()
}

fn prune_usage_to(live: &std::path::Path, indexed_docs: &std::collections::HashSet<String>) {
    match crate::usage::retain_indexed(live, indexed_docs) {
        Ok(0) => {}
        Ok(n) => eprintln!("br8n: dropped usage records for {n} documents no longer indexed"),
        Err(e) => eprintln!("br8n: could not prune usage records — {e:#}"),
    }
}

/// Rebuilds the shadow from the LIVE STORE's own rows — documents, chunks
/// (with their stored embeddings), and every edge — instead of from the
/// corpus. This is what `--compact` means: `reindex_swap_with(_, FromScratch)`
/// already builds a fresh shadow and publishes it by rename, but it re-reads
/// and re-embeds the whole corpus, which costs hours at ~40 chunks/s. Reading
/// the live database instead costs no Ollama time at all, and it cannot lose
/// a document that is still indexed but whose source file has moved or been
/// deleted — compaction never consults the filesystem.
///
/// Called from inside `reindex_swap_with`, after the lock is already held, so
/// it does not acquire one itself.
///
/// `db.stamps` (the corpus stat fingerprint) is deliberately left untouched:
/// compaction never looks at the filesystem, so the previous run's
/// fingerprints are still exactly correct. Overwriting them — with an empty
/// map, say — would make the next real `br8n index` re-read (though not
/// re-embed; content hashes are preserved) every document for no reason.
fn usage_for_pack(live: &std::path::Path) -> crate::usage::Map {
    crate::usage::load(live).unwrap_or_else(|e| {
        eprintln!("br8n: usage map unreadable, publishing with no decay — {e:#}");
        crate::usage::Map::new()
    })
}

fn compact_swap(cfg: &Config, live: &std::path::Path) -> Result<IndexStats> {
    let snap = read_source(cfg, live)?;
    let model_id = snap.model_id.clone();

    let shadow = live.with_extension("new");
    let _ = std::fs::remove_dir_all(&shadow);

    let indexed_docs: std::collections::HashSet<String>;
    let stats = {
        let dst = Store::open(&shadow, cfg.embed.dimensions)?;

        // `embed_model`/`schema_version` (and anything future) carry across
        // verbatim — compaction never touches the embedder or the schema, so
        // there is nothing here to re-derive them from.
        for (k, v) in snap.meta {
            dst.set_meta(&k, &v)?;
        }

        let docs = snap.docs;
        for d in &docs {
            dst.create_document_row(d)?;
        }

        let chunks = snap.chunks;
        for c in &chunks {
            dst.create_chunk_row(c)?;
        }
        for (a, b) in snap.next_chunk {
            dst.link_next_chunk(&a, &b)?;
        }
        for (from, to, kind) in snap.links_to {
            dst.link_documents(&from, &to, &kind)?;
        }
        for (chunk_id, name, kind) in snap.mentions {
            dst.mention_entity(&chunk_id, &name, &kind)?;
        }
        for (doc_id, domain) in snap.derived_from {
            dst.attach_source(&doc_id, &domain)?;
        }

        // Built into the shadow before the rename, exactly like the
        // corpus-based path — so the database and the pack publish as one
        // generation. A pack that fails to build must abort the swap.
        let pack_rows = dst.all_rows_for_pack()?;
        indexed_docs = doc_ids_of(&pack_rows);
        // From `dst`: every `LINKS_TO` edge was copied above, and the pack
        // must describe the database it is published beside.
        let inbound = dst.inbound_link_counts()?;
        // From `dst`, same reasoning as `inbound` above.
        let lifecycles = dst.all_lifecycles(&inbound)?;
        crate::pack::Pack::build_with_usage(
            &shadow,
            &model_id,
            cfg.embed.dimensions,
            pack_rows,
            &inbound,
            &lifecycles,
            &usage_for_pack(live),
        )?;

        IndexStats {
            // Nothing was added, updated or pruned — compaction copies the
            // live rows unchanged, so every document is reported the same
            // way an incremental run reports one it found nothing new in.
            skipped: docs.len(),
            chunks: chunks.len(),
            chunks_reused: chunks.len(),
            ..Default::default()
        }
        // `dst` drops here, releasing the shadow database's file lock
        // before the rename below.
    };

    publish_shadow(live, &shadow)?;
    prune_usage_to(live, &indexed_docs);
    Ok(stats)
}

/// Total bytes of every file under `dir`, recursively. Best-effort: an
/// unreadable entry contributes 0 rather than failing the whole walk — this
/// is a reporting figure (`br8n index --compact`'s before/after line), not a
/// correctness input.
pub fn dir_size(dir: &std::path::Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return total;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if let Ok(ft) = entry.file_type() {
            if ft.is_dir() {
                total += dir_size(&path);
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Partial::merge` is the ONLY route a worker's findings take to the
    /// caller, and it is a hand-written field-by-field copy — so a field added
    /// to `Discovered` and forgotten here is silently discarded by the pool,
    /// with nothing to show for it but a document that quietly never got
    /// indexed. `ocr_pending` was exactly such an addition.
    ///
    /// A unit test rather than an integration one because `Partial` is
    /// private, and widening its visibility to satisfy a test would be the
    /// worse trade. Every field of `other` is populated with a value distinct
    /// from `self`'s, so a merge that drops one, or copies the wrong one, is
    /// visible.
    #[test]
    fn merge_carries_every_field_of_a_workers_partial() {
        let mut into = Partial::default();
        into.found.docs.push(Document::new(
            crate::model::SourceType::Markdown,
            "file:///mine.md",
            "Mine",
            "body",
        ));
        into.found.unchanged.push("file:///mine-unchanged".into());
        into.found.deferred.push("file:///mine-deferred".into());
        into.found.skipped.push("mine: skipped".into());
        into.found
            .ocr_pending
            .push("file:///mine-pending.pdf".into());
        into.found.ignored = 2;
        into.fresh.insert("file:///mine.md".into(), "1:1".into());

        let mut other = Partial::default();
        other.found.docs.push(Document::new(
            crate::model::SourceType::Pdf,
            "file:///theirs.pdf",
            "Theirs",
            "body",
        ));
        other
            .found
            .unchanged
            .push("file:///theirs-unchanged".into());
        other.found.deferred.push("file:///theirs-deferred".into());
        other.found.skipped.push("theirs: skipped".into());
        other
            .found
            .ocr_pending
            .push("file:///theirs-pending.pdf".into());
        other.found.ignored = 3;
        other
            .fresh
            .insert("file:///theirs.pdf".into(), "2:2".into());

        into.merge(other);

        let uris: Vec<&str> = into.found.docs.iter().map(|d| d.uri.as_str()).collect();
        assert_eq!(uris, ["file:///mine.md", "file:///theirs.pdf"]);
        assert_eq!(
            into.found.unchanged,
            ["file:///mine-unchanged", "file:///theirs-unchanged"]
        );
        assert_eq!(
            into.found.deferred,
            ["file:///mine-deferred", "file:///theirs-deferred"]
        );
        assert_eq!(into.found.skipped, ["mine: skipped", "theirs: skipped"]);
        assert_eq!(
            into.found.ocr_pending,
            ["file:///mine-pending.pdf", "file:///theirs-pending.pdf"]
        );
        // Summed, not concatenated and not overwritten — 3 alone would be a
        // clobber and 2 alone a dropped field.
        assert_eq!(into.found.ignored, 5);
        assert_eq!(
            into.fresh.get("file:///theirs.pdf").map(String::as_str),
            Some("2:2"),
            "the worker's stamps must arrive too"
        );
        assert_eq!(
            into.fresh.get("file:///mine.md").map(String::as_str),
            Some("1:1"),
            "and must not displace the ones already here"
        );
    }

    /// A path `consider_into` cannot canonicalize must land in `skipped`.
    ///
    /// It used to `return` on `Err`, recording NOTHING — so the file reached
    /// none of `docs`, `unchanged`, `deferred`, `ocr_pending` or `skipped`, and
    /// `br8n status` reported a corpus one document short with no line
    /// anywhere naming it. That it was not also DATA LOSS was luck:
    /// `uri_is_discoverable` canonicalizes the same path, fails the same way,
    /// and so keeps `prune_missing` off an already-indexed copy.
    ///
    /// A unit test rather than an integration one because reaching this arm
    /// through `discover_stat_first` needs the walk to hand over a path that
    /// then fails to canonicalize, and `ignore::WalkBuilder` does not follow
    /// symlinks, so a dangling link is filtered by `is_file()` before it ever
    /// gets here (`a_dangling_symlink_is_filtered_by_the_walk_before_the_loader_sees_it`
    /// in `tests/index.rs` pins that filtering, and says so). `consider_into`
    /// is private, so this is the level at which the arm is reachable at all.
    ///
    /// Mutation: restore `let Ok(canon) = path.canonicalize() else { return };`
    /// and this fails on the length assertion.
    #[test]
    fn a_canonicalize_failure_is_recorded_in_the_skip_list() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-note.md");
        let mut mine = Partial::default();

        consider_into(
            &mut mine,
            &Config::default(),
            dir.path(),
            &std::collections::HashMap::new(),
            Deferral::Forbidden,
            OcrPass::Skip,
            &missing,
            Kind::Markdown,
        );

        assert_eq!(
            mine.found.skipped.len(),
            1,
            "a path that cannot be canonicalized must be reported, not dropped"
        );
        assert!(
            mine.found.skipped[0].starts_with(&format!("{}: ", missing.display())),
            "the record must name the file, in the loader-failure arm's format: {:?}",
            mine.found.skipped[0]
        );
        // Nothing else may be claimed for a file that was never read.
        assert!(mine.found.docs.is_empty());
        assert!(mine.found.unchanged.is_empty());
        assert!(mine.found.deferred.is_empty());
        assert!(mine.found.ocr_pending.is_empty());
        assert!(mine.fresh.is_empty());
    }

    /// A source root that cannot be canonicalized must REFUSE the run.
    ///
    /// `filter_map(|p| p.canonicalize().ok())` dropped it instead, which is the
    /// same silent discard the existence check in `discover_stat_first_with`
    /// refuses loudly and for the same reason: a root missing from the walk
    /// contributes zero documents, and `prune_missing` deletes every indexed
    /// document under it.
    ///
    /// The fixture is a root that does NOT exist, because that is the only way
    /// to fail `canonicalize` without root privileges or an unmount — this
    /// function's caller checks existence first, so in production the failure
    /// arrives by the narrower routes named in `distinct_roots`' comment. What
    /// is under test is the arm, not the cause.
    ///
    /// Mutation: put the `filter_map` back (returning `Ok(kept)`) and the
    /// `is_err` assertion fails — the missing root is silently dropped and the
    /// good one comes back alone.
    #[test]
    fn a_source_root_that_cannot_be_canonicalized_refuses_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().to_path_buf();
        let bad = dir.path().join("unmounted-drive");

        let ok = distinct_roots(std::slice::from_ref(&good)).expect("a real root must resolve");
        assert_eq!(ok.len(), 1, "the control arm must still dedup to one root");

        let err = distinct_roots(&[good, bad.clone()])
            .expect_err("an unresolvable root must refuse the run, not vanish from it");
        let msg = format!("{err:#}");
        assert!(
            msg.contains(&bad.display().to_string()),
            "the refusal must name the offending root: {msg}"
        );
        assert!(
            msg.contains("Refusing to index"),
            "and must say why, like the existence check does: {msg}"
        );
    }

    /// The Skip pass loads PDFs with `ocr = off`, whatever the user configured.
    ///
    /// This is the half of that guarantee an integration test CANNOT pin on a
    /// machine without PDFium and ONNX Runtime: with `ocr = auto` and the
    /// dylibs absent, recognition falls back to the text layer by itself, so a
    /// Skip pass and a Run pass produce identical results and every assertion
    /// downstream passes whether or not `Skip` forced anything. Here the
    /// forcing is the only thing under test, and no dylib is involved.
    ///
    /// The `Run` arm is not decoration: without it a `_ => Off` would pass.
    ///
    /// Mutation: change the `Skip` arm to `cfg.clone()` and the first assertion
    /// fails; change `Run` to force `Off` and the second does.
    #[test]
    fn the_skip_pass_forces_ocr_off_and_the_run_pass_does_not() {
        use crate::config::{OcrMode, PdfConfig};
        let user = PdfConfig {
            ocr: OcrMode::Auto,
            ..PdfConfig::default()
        };

        assert_eq!(
            pdf_config_for(OcrPass::Skip, &user).ocr,
            OcrMode::Off,
            "phase 1 publishes the text layer; recognition is phase 2's job"
        );
        assert_eq!(
            pdf_config_for(OcrPass::Run, &user).ocr,
            OcrMode::Auto,
            "phase 2 must load with what the user actually asked for"
        );
        // Everything else is passed through, so the forcing cannot quietly
        // reset a DPI or a model directory alongside the mode.
        assert_eq!(pdf_config_for(OcrPass::Skip, &user).dpi, user.dpi);
    }

    #[test]
    fn a_deleted_files_stamp_does_not_survive_the_next_save() {
        let corpus = tempfile::tempdir().unwrap();
        std::fs::write(corpus.path().join("a.md"), "# A\n\nOne.").unwrap();
        let b_path = corpus.path().join("b.md");
        std::fs::write(&b_path, "# B\n\nTwo.").unwrap();
        let cfg = Config {
            sources: vec![corpus.path().to_path_buf()],
            index_transcripts: false,
            ..Config::default()
        };
        let db = corpus.path().join("db");

        let (_found, fresh) = discover_stat_first_with(
            &cfg,
            &std::collections::HashMap::new(),
            Deferral::Allowed,
            OcrPass::Skip,
        )
        .unwrap();
        assert_eq!(fresh.len(), 2);
        save_stamps(&db, &fresh);

        std::fs::remove_file(&b_path).unwrap();
        let stamps = load_stamps(&db);
        let (_found2, fresh2) =
            discover_stat_first_with(&cfg, &stamps, Deferral::Allowed, OcrPass::Skip).unwrap();
        save_stamps(&db, &fresh2);

        let reloaded = load_stamps(&db);
        assert_eq!(reloaded.len(), 1);
        assert!(reloaded.keys().all(|uri| !uri.ends_with("b.md")));
    }
}
