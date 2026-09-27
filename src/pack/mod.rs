//! The retrieval pack: immutable, mmap'd artifacts the prompt path reads
//! instead of opening the database.
//!
//! `usearch` is quarantined here exactly as `lbug` is quarantined in
//! `src/store/`, and for the same reason: its behaviour is established by
//! a standalone spike, not by its documentation.

pub mod analyze;
pub mod links;
pub mod manifest;
pub mod postings;
pub mod records;
pub mod status;
pub mod used;
pub mod vectors;

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

use manifest::Manifest;
use records::Record;

/// A published generation of retrieval artifacts, opened read-only.
///
/// `vecs` is `None` for a pack whose manifest declares `rows_with_vectors: 0`
/// — phase 1 of an asynchronous index, which publishes postings and records
/// before any embedding has happened. See `Pack::open` and `Pack::has_vectors`.
pub struct Pack {
    recs: records::Reader,
    vecs: Option<vectors::Reader>,
    fts: postings::Reader,
    links: links::Reader,
    status: Option<status::Reader>,
    used: Option<used::Reader>,
}

// `records::Reader` and `vectors::Reader` are mmap'd/usearch handles with no
// `Debug` of their own; a manual impl (rather than a derive, which would need
// the fields to be `Debug`) is only so `Result<Pack, _>::unwrap_err()` compiles
// in tests. Row count is the one fact worth printing.
impl std::fmt::Debug for Pack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pack")
            .field("rows", &self.recs.len())
            .finish()
    }
}

impl Pack {
    /// Write a complete generation. Callers build into a directory that is
    /// about to be renamed into place, so a partially written pack is never
    /// visible to a reader.
    ///
    /// `inbound` is `Store::inbound_link_counts` — inbound `LINKS_TO` count
    /// keyed by document id, holding only the documents that have at least
    /// one. It is flattened here into one count PER ROW (`pack.links`) so the
    /// read path joins it by the row ordinal like everything else; see
    /// `links`'s module docs for why a doc_id-keyed side table was rejected.
    /// The corpus maximum is taken over the MAP, not over the rows, so it
    /// equals the number `Retriever::with_weights` used to compute from the
    /// store even when a linked document has no chunks.
    ///
    /// `lifecycles` is `Store::all_lifecycles` — `doc_id -> Lifecycle` for the
    /// documents that declare a `status:` and are not `Current`. Flattened
    /// here into one byte PER ROW (`pack.status`), joined by the row ordinal
    /// exactly as `inbound` is, and written as a versioned SIDE-FILE rather
    /// than a pack member — see `status`'s module docs for why.
    pub fn build(
        dir: &Path,
        model_id: &str,
        dims: usize,
        rows: Vec<(Record, Vec<f32>)>,
        inbound: &HashMap<String, u32>,
        lifecycles: &HashMap<String, status::Lifecycle>,
    ) -> Result<()> {
        Self::build_with_usage(
            dir,
            model_id,
            dims,
            rows,
            inbound,
            lifecycles,
            &crate::usage::Map::new(),
        )
    }

    pub fn build_with_usage(
        dir: &Path,
        model_id: &str,
        dims: usize,
        rows: Vec<(Record, Vec<f32>)>,
        inbound: &HashMap<String, u32>,
        lifecycles: &HashMap<String, status::Lifecycle>,
        usage: &crate::usage::Map,
    ) -> Result<()> {
        let (recs, vecs): (Vec<Record>, Vec<Vec<f32>>) = rows.into_iter().unzip();
        // `records::Reader::find_by_chunk_id` binary-searches these rows, which
        // is only correct if they are sorted by `chunk_id` ascending. They are
        // supposed to be: `Store::all_rows_for_pack` ends `ORDER BY c.id`
        // precisely so the pack can rely on this order. A silent violation
        // here would make the binary search return wrong-or-missing rows on
        // the read path with nothing to say why, so it is checked once, at
        // build time, for one comparison per row.
        for w in recs.windows(2) {
            anyhow::ensure!(
                w[0].chunk_id <= w[1].chunk_id,
                "pack records are not sorted by chunk_id ({:?} appears before {:?}) — \
                 Pack::cosine_for's binary search requires the ORDER BY c.id that \
                 Store::all_rows_for_pack promises",
                w[0].chunk_id,
                w[1].chunk_id
            );
        }
        // `Store::all_rows_for_pack` includes a chunk with no embedding as an
        // EMPTY vector rather than skipping it (it must still be searchable
        // by BM25), so `dims`-length is what actually distinguishes "has a
        // vector" from "does not" here.
        let rows_with_vectors = vecs.iter().filter(|v| v.len() == dims).count();
        // Three shapes reach this point: every row has a vector (the normal
        // case), no row does (phase 1 of an asynchronous index — see
        // `Manifest::rows_with_vectors`), or a MIX of the two — reachable if
        // `--no-embed` runs incrementally against an index that already had
        // some embedded chunks: unchanged chunks keep their real vectors,
        // newly written ones get none. usearch itself COULD hold a sparse
        // index (its keys are explicit row ordinals, not required to be
        // contiguous — skipping the empty rows here and adding only the real
        // ones at their true position is technically buildable). This
        // refuses the mix anyway, because nothing on the READ side is ready
        // for it yet: `Pack::cosine_for` calls `vectors::Reader::vector(row)`
        // for a hit found by BM25 or graph expansion, and that call hard-
        // errors (`ensure!(n == 1, ...)`) the moment `row` was never added —
        // turning one BM25 hit that happens to land on an unembedded chunk
        // into a failure of the WHOLE measure stage. Teaching that path to
        // treat a missing row as "unmeasurable" (the same way it already
        // treats an id the pack does not hold at all) is what sparse support
        // would need, and it is deliberately out of scope here rather than
        // shipped half-verified. Refuse loudly instead of publishing
        // something the read side cannot serve correctly.
        let builds_vectors = match rows_with_vectors {
            0 => {
                // Phase 1: no `pack.vec` at all, matching `Pack::open`'s
                // expectation that a manifest declaring zero vectors need not
                // find the file on disk.
                false
            }
            n if n == recs.len() => true,
            n => anyhow::bail!(
                "pack build: {n} of {} rows have a vector — a mixed pack is not \
                 supported (see Pack::build's doc comment on why). Run `br8n index \
                 --backfill` to finish embedding the rest before the next publish, or \
                 `br8n index --reindex --no-embed` to discard and republish vectorless",
                recs.len()
            ),
        };
        records::write(dir, &recs)?;
        // The same field the store derives `fts_body` from, with newlines
        // collapsed to spaces first — see `Store::insert_chunks`. A literal
        // '\n' left in place welds its two neighbouring words into one
        // unsearchable token (`analyze::tokenize`'s doc comment), so every
        // row would lose the first term after each line break.
        let mut postings = postings::Builder::default();
        for r in &recs {
            postings.add_row(&analyze::analyze(&r.text.replace(['\n', '\r'], " ")));
        }
        postings.write(dir)?;
        // One count per row, in row-ordinal order, from the document each row
        // belongs to. A document absent from the map has no inbound links —
        // `Store::inbound_link_counts` only returns the ones that do — and 0
        // is what `authority_lift` already treats as "not linked", so the
        // absence needs no special case.
        let link_counts: Vec<u32> = recs
            .iter()
            .map(|r| inbound.get(&r.doc_id).copied().unwrap_or(0))
            .collect();
        links::write(
            dir,
            &link_counts,
            inbound.values().copied().max().unwrap_or(0),
        )?;
        // Same row-ordinal join as `link_counts` above.
        let statuses: Vec<status::Lifecycle> = recs
            .iter()
            .map(|r| lifecycles.get(&r.doc_id).copied().unwrap_or_default())
            .collect();
        status::write(dir, &statuses)?;
        let last_used: Vec<i64> = recs
            .iter()
            .map(|r| usage.get(&r.doc_id).and_then(|u| u.last_used).unwrap_or(0))
            .collect();
        used::write(dir, &last_used)?;
        let rows = recs.len();
        drop(recs);
        if builds_vectors {
            vectors::build(dir, dims, &vecs)?;
        }
        Manifest {
            format: manifest::FORMAT,
            model_id: model_id.to_string(),
            dims,
            rows,
            rows_with_vectors,
            analyzer: analyze::ANALYZER.to_string(),
            // INERT. Nothing writes these and nothing reads them; they are not
            // defence in depth and must not be read as such. The only
            // cross-file guard is the row count checked in `open`.
            //
            // They are not populated because there is no consumer that could
            // afford them: verifying a content hash in `open` would mean
            // hashing the whole record blob and vector index on every prompt,
            // and `open` is the hot path this pack exists to make cheap — the
            // same milliseconds `efs` and the reader-DDL fix were spent
            // recovering. A populated-but-never-read field would mislead more
            // than an empty one, not less.
            //
            // What can actually desynchronise the two files today: nothing.
            // `build` writes both from one collection in one call, and
            // `reindex_swap` publishes the directory by a single rename. The
            // row count catches the reachable case — a directory corrupted or
            // hand-assembled afterwards. Whether to drop these fields in a
            // format bump, or keep them as offline diagnostic metadata `open`
            // never reads, is an open decision.
            vec_sha: String::new(),
            rec_sha: String::new(),
        }
        .write(dir)
    }

    /// Open and validate. Every failure here is a REFUSAL, never a degraded
    /// open: a pack that disagrees with its manifest would return the right
    /// relevance attached to the wrong document, and nothing downstream could
    /// tell.
    ///
    /// The ONE exception: `pack.vec` is opened only when the manifest says
    /// `rows_with_vectors > 0`. A pack with none declared and no file on disk
    /// is phase 1 of an asynchronous index — postings and records published
    /// before any embedding has happened — and that is a state to serve, not
    /// refuse. A pack that DOES declare vectors but is missing `pack.vec`
    /// still refuses below, because `vectors::Reader::open` errors on the
    /// missing file exactly as it always has.
    pub fn open(dir: &Path, model_id: &str, dims: usize) -> Result<Pack> {
        let m = Manifest::read(dir)?;
        m.validate(model_id, dims)?;
        let recs = records::Reader::open(dir)?;
        let vecs = if m.rows_with_vectors > 0 {
            Some(vectors::Reader::open(dir, dims)?)
        } else {
            None
        };
        let fts = postings::Reader::open(dir)?;
        let links = links::Reader::open(dir)?;
        anyhow::ensure!(
            recs.len() == m.rows,
            "pack has {} record rows but its manifest claims {} — the vector \
             index and the records are from different generations; run \
             `br8n index --reindex`",
            recs.len(),
            m.rows
        );
        anyhow::ensure!(
            fts.rows() == m.rows,
            "pack has postings over {} rows but its manifest claims {} — the \
             postings and the records are from different generations; run \
             `br8n index --reindex`",
            fts.rows(),
            m.rows
        );
        // Same check, same reason, for the link counts: they are joined to the
        // records by the row ordinal and by nothing else, so a `pack.links`
        // from another generation would lift the wrong documents by the right
        // amounts — a wrong ORDER, silently, with the gate and the ordering
        // agreeing on it. Refuse.
        anyhow::ensure!(
            links.rows() == m.rows,
            "pack has link counts over {} rows but its manifest claims {} — the \
             link counts and the records are from different generations; run \
             `br8n index --reindex`",
            links.rows(),
            m.rows
        );
        // OPTIONAL, unlike every other pack member: a pack published by a binary
        // that predates this file is valid and must open. Absent means "every
        // row Current" — never zeros meaning something else, which is why
        // `Lifecycle::Current` is the zero discriminant.
        //
        // Deliberately checked AFTER the three `ensure!`s above: on a
        // genuinely inconsistent pack (records/postings/links disagreeing
        // with the manifest), the real cause must reach the user first. A
        // `pack.status` row-count mismatch is comparatively harmless — it
        // only degrades ranking — so its warning must never print ahead of a
        // hard error naming the actual generation mismatch.
        let status = match status::Reader::open(dir) {
            Ok(r) if r.rows() == m.rows => Some(r),
            Ok(r) => {
                eprintln!(
                    "br8n: pack.status covers {} rows but the pack has {} — \
                     ignoring it; every document will rank as current. \
                     Run `br8n index --compact` to republish.",
                    r.rows(),
                    m.rows
                );
                None
            }
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
            {
                None
            }
            Err(e) => {
                eprintln!(
                    "br8n: pack.status unreadable ({e}); every document will rank as current"
                );
                None
            }
        };
        let used = match used::Reader::open(dir, m.rows) {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "br8n: {e:#}; ignoring it, so no transcript decays. \
                     Run `br8n index --compact` to republish."
                );
                None
            }
        };
        Ok(Pack {
            recs,
            vecs,
            fts,
            links,
            status,
            used,
        })
    }

    /// Read row `row`'s record and attach its inbound link count and lifecycle.
    ///
    /// The one place these files are joined, and it is the row ordinal that
    /// joins them — `records::Reader::get`, `links::Reader::get`, and
    /// `status::Reader::get` are indexed by the same number, in the same call,
    /// with no key derived from the record's contents in between. `status` is
    /// `None` for a pack published before this file existed, in which case
    /// every row reads `Lifecycle::Current` — the zero-discriminant no-op.
    fn hydrate(&self, row: usize) -> Result<Record> {
        let mut r = self.recs.get(row)?;
        r.inbound = self.links.get(row);
        r.lifecycle = self
            .status
            .as_ref()
            .map_or(Default::default(), |s| s.get(row));
        r.last_used = self.used.as_ref().and_then(|u| u.get(row));
        Ok(r)
    }

    pub fn last_used_of(&self, chunk_id: &str) -> Option<i64> {
        let used = self.used.as_ref()?;
        let row = self.recs.find_by_chunk_id(chunk_id).ok()??;
        used.get(row)
    }

    /// The largest inbound link count in the corpus — `authority_lift`'s
    /// denominator, which is global rather than per-hit. See
    /// `links::Reader::max_inbound`.
    pub fn max_inbound(&self) -> u32 {
        self.links.max_inbound()
    }

    /// Whether this pack can serve vector search at all. `false` for phase 1
    /// of an asynchronous index — see `Pack::open`'s doc comment.
    pub fn has_vectors(&self) -> bool {
        self.vecs.is_some()
    }

    pub fn rows(&self) -> usize {
        self.recs.len()
    }

    pub fn record(&self, row: usize) -> Result<Record> {
        self.hydrate(row)
    }

    /// Search, then hydrate only the rows that survived.
    ///
    /// `efs` is threaded straight through to `vectors::Reader::search` — see
    /// its doc comment — so a caller on the pack path gets explicit per-tier
    /// control over search effort rather than a fixed default.
    ///
    /// Returns an empty result — never an error — when this pack has no
    /// vectors (`Pack::has_vectors` is `false`). A partial index is a
    /// declared state, and the caller (`Retriever::run`) must fall through to
    /// BM25 rather than fail the whole query over a stage that legitimately
    /// has nothing to contribute yet.
    ///
    /// The result is sorted by similarity DESCENDING, `chunk_id` ASCENDING,
    /// the same key `weight_and_order` uses downstream — this pack is the one
    /// vector search backend now, so nothing else needs to agree with it, but
    /// its own order must still be deterministic across calls.
    ///
    /// This is not about display order. `retrieve::run` seeds graph expansion
    /// from `vector_hits.iter().take(5)` — the RAW list, before any weighting
    /// or re-ordering — so an unstable order here changes WHICH chunks get
    /// expanded from run to run, and the pack is the path the prompt hook
    /// takes on every installation that has one. usearch returns its top-k
    /// best-first, but says nothing about ties, and ties are ordinary here
    /// rather than exotic: the index is f16-quantized, so distinct vectors
    /// routinely collapse onto bit-identical distances. Sorting in Rust,
    /// after hydration, rather than trusting the order the top-k heap
    /// happens to pop.
    pub fn search(&self, q: &[f32], k: usize, efs: usize) -> Result<Vec<(Record, f32)>> {
        let Some(vecs) = &self.vecs else {
            return Ok(Vec::new());
        };
        let mut hits: Vec<(Record, f32)> = vecs
            .search(q, k, efs)?
            .into_iter()
            .map(|(row, sim)| Ok((self.hydrate(row)?, sim)))
            .collect::<Result<Vec<_>>>()?;
        hits.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.chunk_id.cmp(&b.0.chunk_id))
        });
        Ok(hits)
    }

    /// BM25 search, then hydrate only the rows that survived.
    ///
    /// The `f32` returned is a BM25 score: ordinal and unbounded, never a
    /// `[0,1]` relevance. The caller must put it on `Hit.score`, never on
    /// `Hit.relevance` — that field means "never measured" (0.0) until the
    /// post-fusion measure stage backfills a real cosine, and every defect
    /// class this project has shipped came from conflating the two.
    ///
    /// `collect::<Result<Vec<_>>>()` below means one out-of-range row fails
    /// the whole result rather than silently dropping a hit — matching
    /// `search` above, and deliberately: a shortened result here is
    /// indistinguishable from an honest one.
    pub fn bm25(&self, query: &str, k: usize) -> Result<Vec<(Record, f32)>> {
        self.fts
            .search(query, k)
            .into_iter()
            .map(|(row, score)| Ok((self.hydrate(row)?, score)))
            .collect()
    }

    /// Cosine similarity between `query` and each named chunk's stored
    /// embedding — the pack's replacement for `Store::cosine_for`, which is
    /// the LAST store call left on the tier-1 path (see module docs on why
    /// this task exists).
    ///
    /// Each requested id is located by `records::Reader::find_by_chunk_id`, a
    /// binary search over rows ordered by `chunk_id` (see `Pack::build`'s
    /// sortedness guard) — ~log2(rows) decodes per id instead of scanning the
    /// whole pack. A linear scan measured at 65.9ms for a 20-id request
    /// against the live 31k-row corpus, against 13.1ms for `Store::cosine_for`
    /// — 5x slower than the call this stage exists to replace; the binary
    /// search is what makes this stage a net win again.
    ///
    /// Uses the SAME `(1 + s) / 2` scale as `vectors::Reader::search` and
    /// `Store::cosine_for` — see `pack_vector_search_and_pack_measure_agree_on_the_same_chunk`
    /// in `tests/it/retrieve_primitives.rs` for what happens when the two
    /// writers of `relevance` disagree. An id this pack does not hold is
    /// left ABSENT from the map, never inserted at 0.0: `relevance` gates
    /// injection, and a fabricated zero is indistinguishable from a measured
    /// one.
    pub fn cosine_for(&self, query: &[f32], chunk_ids: &[String]) -> Result<HashMap<String, f32>> {
        let mut out = HashMap::new();
        // No vectors at all (phase 1, see `Pack::open`): every id is
        // unmeasurable, same as an id this pack does not hold at all — left
        // ABSENT below, never inserted at 0.0.
        let Some(vecs) = &self.vecs else {
            return Ok(out);
        };
        if query.is_empty() || chunk_ids.is_empty() {
            return Ok(out);
        }
        for id in chunk_ids {
            let Some(row) = self.recs.find_by_chunk_id(id)? else {
                continue;
            };
            let v = vecs.vector(row)?;
            if v.len() != query.len() {
                continue;
            }
            let dot: f32 = v.iter().zip(query).map(|(a, b)| a * b).sum();
            // (1 + s) / 2, NOT the raw cosine — see the doc comment above.
            out.insert(id.clone(), ((1.0 + dot) / 2.0).clamp(0.0, 1.0));
        }
        Ok(out)
    }
}

fn write_buffered(
    path: &Path,
    body: impl FnOnce(&mut std::io::BufWriter<std::fs::File>) -> std::io::Result<()>,
) -> Result<()> {
    let written = std::fs::File::create(path).and_then(|file| {
        let mut out = std::io::BufWriter::new(file);
        body(&mut out)?;
        out.into_inner().map_err(|e| e.into_error()).map(drop)
    });
    written.with_context(|| format!("write {}", path.display()))
}

/// Open the pack beside `db`, distinguishing "no pack was ever published" from
/// "a pack exists but cannot be trusted".
///
/// `Ok(None)` is a degrade: an index built before packs existed still opens,
/// falling back to a store-backed retriever that refuses loudly once a query
/// actually runs. `Err` is a refusal: a manifest is present but the pack it
/// names does not validate (wrong model, wrong dimensions, a missing or
/// truncated file, a row-count disagreement between the vector index and the
/// records) — reading it anyway would attach correct-looking relevance to the
/// wrong documents, and nothing downstream could tell. The distinction turns on
/// the manifest file alone, checked BEFORE `Pack::open` runs: `open` fails
/// identically whether the manifest is simply absent or is present but broken,
/// so the presence check has to happen out here for the two cases to be told
/// apart at all.
pub fn open_pack_beside(db: &Path, model_id: &str, dims: usize) -> Result<Option<Pack>> {
    if !db.join(manifest::MANIFEST_FILE).exists() {
        return Ok(None);
    }
    Pack::open(db, model_id, dims).map(Some).map_err(|e| {
        crate::retrieve::PackRefused(e.context("the pack beside this index is unusable")).into()
    })
}
