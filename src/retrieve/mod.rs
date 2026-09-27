pub mod fusion;
pub mod rerank;

use crate::config::Profile;
use crate::embed::Embedder;
use crate::store::{Hit, Store};
use anyhow::Result;
use std::time::Instant;

/// How much a graph neighbour counts against a direct match in fusion.
const GRAPH_WEIGHT: f32 = 0.2;

pub const MAX_RESULTS: usize = 10;

#[derive(Debug, Default)]
pub struct StageReport {
    pub stages_run: Vec<&'static str>,
    pub elapsed_ms: u128,
    /// True when the deadline forced a stage to be skipped.
    pub degraded: bool,
    /// Set when the profile asked for BM25, no pack was available, and the
    /// store's own keyword search errored (it always does now — see
    /// `Store::fts_search`). The pipeline still returns vector-only results
    /// rather than failing the whole query; this is how a caller (the hook,
    /// via stderr) finds out that happened instead of mistaking it for an
    /// honest no-match.
    pub bm25_unavailable: Option<String>,
    /// The mirror image of `bm25_unavailable`, same shape and same reason it
    /// exists: set when the pack has no vectors at all (`Pack::has_vectors`
    /// is `false` — phase 1 of an asynchronous index, see `Manifest::
    /// rows_with_vectors`). The pipeline still answers from BM25 rather than
    /// failing the whole query; without this, a caller would see the same
    /// empty-vector-stage shape as an honest no-match and have no way to
    /// tell the two apart.
    pub vectors_unavailable: Option<String>,
    pub memory_unavailable: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HitSummary {
    pub chunk_id: String,
    pub doc_id: String,
    pub title: String,
    pub heading: String,
    pub source_type: String,
    pub relevance: f32,
    pub score: f32,
    pub excerpt: String,
    pub memory_kind: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StageTrace {
    pub name: &'static str,
    pub hits: Vec<HitSummary>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Explain {
    pub stages: Vec<StageTrace>,
    /// Everything that cleared the gate AND survived diversity selection —
    /// the list the hook hands to `hook::build_context`.
    pub fused: Vec<HitSummary>,
    pub threshold: f32,
    /// The prefix of `fused` the surface's token budget actually admits: what
    /// the hook really injects. `build_context` stops at `max_tokens`, and
    /// chunks run 1024-2048 chars, so ten gated hits are routinely three or
    /// four injected ones. Both sides count with the same helper
    /// (`hook::fit_to_budget`) so this cannot drift back apart.
    pub injected: Vec<String>,
    pub degraded: bool,
    /// See `StageReport::bm25_unavailable`. `Explain` is serialized straight
    /// into the dashboard's `/api/search` response (`src/dashboard.rs`), so
    /// this reaches the browser today — but the frontend does not read it
    /// (`dashboard/src/api.ts`, `dashboard/src/tabs/SearchTab.tsx` render
    /// only `degraded`), so nothing currently shows it. It is carried here so
    /// that wiring, if it happens, has the value ready rather than needing a
    /// backend change too.
    pub bm25_unavailable: Option<String>,
    /// See `StageReport::vectors_unavailable`.
    pub vectors_unavailable: Option<String>,
    pub memory_unavailable: Option<String>,
    pub elapsed_ms: u128,
}

/// The embedding model could not be reached.
///
/// The one retrieval failure the dashboard reports as a 200 plus a banner
/// instead of a 503: Ollama being down is a stable runtime state the user can
/// see and fix, while a store error mid-query (a shadow swap landing between
/// two of this pipeline's queries) is transient and worth a retry. Without a
/// marker on the one call that talks to Ollama, `dashboard::search` had to
/// treat EVERY error from the pipeline as an embedding outage — and blamed
/// Ollama for store failures it had never asked Ollama about.
#[derive(Debug)]
pub struct EmbedUnavailable(pub anyhow::Error);

impl std::fmt::Display for EmbedUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "embedding unavailable: {}", self.0)
    }
}

impl std::error::Error for EmbedUnavailable {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.chain().nth(1)
    }
}

#[derive(Debug)]
pub struct PackRefused(pub anyhow::Error);

impl std::fmt::Display for PackRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for PackRefused {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

/// Chunks run 1024-2048 chars; 1200 is chosen so the dashboard's "why this
/// matched" panel actually shows the matching text instead of cutting a
/// keyword hit off before the word that caused the match. `HitSummary` is
/// consumed only by `Explain` (the dashboard's `/api/search`) — `bench.rs`
/// reads `HitSummary.doc_id`, never `excerpt`, and the prompt hook builds its
/// injected block from `Hit.text` via `budget.rs`/`hook.rs`, not from this
/// field — so this cap affects only what a human sees in the dashboard, never
/// what goes into a prompt.
const EXCERPT_CHARS: usize = 1200;

fn summarize(h: &Hit) -> HitSummary {
    let mut excerpt: String = h.text.chars().take(EXCERPT_CHARS).collect();
    if excerpt.len() < h.text.len() {
        excerpt.push('\u{2026}');
    }
    HitSummary {
        chunk_id: h.chunk_id.clone(),
        doc_id: h.doc_id.clone(),
        title: h.title.clone(),
        heading: h.heading_path.clone(),
        source_type: h.source_type.clone(),
        relevance: h.relevance,
        score: h.score,
        excerpt,
        memory_kind: h.memory.as_ref().map(|f| f.kind.as_str().to_string()),
    }
}

/// Where `rank_weight` reads a hit's inbound link count from.
///
/// Two backends fill one role, so they are named rather than blended: a hit
/// must be lifted by exactly one of them, and which one is decided once, at
/// construction, rather than per hit. `pack_link_counts_match_the_store`
/// (tests/it/retrieve_primitives.rs) is what pins the two to the same numbers.
/// Where a hit's lifecycle comes from, and it is NOT always the hit itself.
///
/// `Hit.lifecycle` is filled by `Pack::hydrate` from `pack.status` using the row
/// ordinal. A GRAPH-EXPANDED hit has no pack row — `Store::expand` builds it
/// straight from the database — so it arrives `Current` no matter what its
/// document declares. Without this, a superseded record pulled in by expansion
/// is injected undemoted.
///
/// STILL NEEDED after `expand` gained a kind filter, and the reason is easy to
/// get wrong. That filter stops expansion following the `supersedes` tombstone
/// pointer, but it deliberately KEEPS `wikilink` and `superseded-by` — so a
/// superseded document reached by an ordinary prose link still enters the pool,
/// and this is the only thing that demotes it. Deleting either one leaves a
/// retired record reaching a prompt at full weight.
///
/// Mirrors `Authority` deliberately, for the same reason and with the same
/// shape: the store's map is keyed by DOCUMENT and therefore covers hits with
/// no row, while the pack's per-row bytes cover everything a storeless
/// retriever can produce.
enum LifecycleSource {
    /// No store open. Every hit came from the pack and already carries its own
    /// byte, so `Hit.lifecycle` is the whole answer.
    Row,
    /// A store is open (tiers 2-4, or authority weighting). Keyed by document,
    /// so it reaches graph-expanded hits. Absent from the map means `Current` —
    /// `all_lifecycles` omits those to keep it small.
    Store(std::collections::HashMap<String, crate::pack::status::Lifecycle>),
}

enum Authority {
    /// Authority weighting is off (`[weights] authority = 0.0`), or neither
    /// backend could supply counts. `authority_lift(0, 0)` is exactly 1.0, so
    /// this multiplies nothing.
    Off,
    /// Counts read from the store's `LINKS_TO` edges, keyed by document id,
    /// plus the largest of them. Preferred whenever a store is open, because
    /// it also covers graph-expanded hits, which have no pack row.
    Store(std::collections::HashMap<String, u32>, u32),
    /// The pack path. Each hit already carries its own count on `Hit.inbound`,
    /// joined to its row ordinal when the pack hydrated it, so only the corpus
    /// maximum — `authority_lift`'s denominator — is needed globally.
    Pack(u32),
}

pub struct Retriever {
    weights: crate::config::Weights,
    /// Where a hit's lifecycle comes from. See `LifecycleSource` — the short
    /// version is that graph-expanded hits have no pack row and would
    /// otherwise never be demoted.
    lifecycles: LifecycleSource,
    /// Where inbound link counts come from. `Off` unless authority weighting
    /// is switched on, so neither backend is consulted for nothing.
    authority: Authority,
    /// `None` when a pack answers every stage the caller's profile can reach
    /// (see `Retriever::packed`). Every stage that still needs the store when
    /// no pack is present (`cosine_for`; `fts_search` too, though it always
    /// errors now that lbug's FTS index is gone — see its doc comment — so
    /// that call exists only to be caught and reported, not to serve BM25;
    /// `expand`, `inbound_link_counts` always need the store regardless of
    /// the pack) must treat this as fallible, not assume it is populated.
    store: Option<Store>,
    embedder: Box<dyn Embedder>,
    ollama_url: String,
    /// Present when a pack was published beside the index. Absent for an
    /// index built before packs existed, in which case the vector stage
    /// refuses rather than falling back to a store vector search — see
    /// `run`'s vector stage.
    pack: Option<std::sync::Arc<crate::pack::Pack>>,
    memory: Option<std::sync::Arc<crate::pack::Pack>>,
    memory_cfg: crate::config::MemoryConfig,
    memory_unavailable: Option<String>,
    now_secs: i64,
}

impl Retriever {
    pub fn new(store: Store, embedder: Box<dyn Embedder>, ollama_url: String) -> Self {
        Self {
            weights: crate::config::Config::default().weights,
            lifecycles: LifecycleSource::Row,
            authority: Authority::Off,
            store: Some(store),
            embedder,
            ollama_url,
            pack: None,
            memory: None,
            memory_cfg: Default::default(),
            memory_unavailable: None,
            now_secs: crate::memory::now_secs(),
        }
    }

    /// A retriever backed by `pack` alone — no store is ever opened.
    ///
    /// Only correct for a caller whose profile never sets `graph`: expansion
    /// still requires the store, and `search`/`search_gated`/etc. return a
    /// clear error rather than a panic or a silent empty result when a profile
    /// asks for it anyway. See `retrieve_for` in `lib.rs` for the decision of
    /// which case applies.
    ///
    /// Authority weighting used to be the second disqualifier and no longer
    /// is: `pack.links` carries the inbound counts, so `with_weights` picks
    /// `Authority::Pack` here and the lift runs with no database open.
    pub fn packed(
        pack: impl Into<std::sync::Arc<crate::pack::Pack>>,
        embedder: Box<dyn Embedder>,
        ollama_url: String,
    ) -> Self {
        Self {
            weights: crate::config::Config::default().weights,
            lifecycles: LifecycleSource::Row,
            authority: Authority::Off,
            store: None,
            embedder,
            ollama_url,
            pack: Some(pack.into()),
            memory: None,
            memory_cfg: Default::default(),
            memory_unavailable: None,
            now_secs: crate::memory::now_secs(),
        }
    }

    /// Order results with these per-source-type weights instead of the defaults.
    ///
    /// MUST be called after `with_pack`. Both constructors and both call sites
    /// (`lib.rs`, `main.rs`) already order it that way; reversed, a packed
    /// retriever would pick `Authority::Off` and silently drop the lift.
    pub fn with_weights(mut self, weights: crate::config::Weights) -> Self {
        // Resolve the lifecycle source once, for the same reason the counts are
        // resolved here: a per-query scan would sit on the hot path.
        //
        // Unconditional, unlike `authority` — there is no "lifecycle is off"
        // switch. The multipliers ship at 1.0 for three of the four positions,
        // so an all-`Current` map costs one query and multiplies by 1.0; the
        // alternative is a superseded record reaching a prompt undemoted
        // whenever expansion pulls it in, which is the defect this exists to
        // stop. A failed query degrades to `Row`, which is exactly the
        // behaviour before this existed.
        if let Some(store) = &self.store {
            if let Ok(map) = store.all_lifecycles(&Default::default()) {
                self.lifecycles = LifecycleSource::Store(map);
            }
        }

        // Resolve the counts once, only when they will be used. Doing it per
        // query would put a scan of the edge table on the hot path, and doing
        // it when authority is off would pay for a signal nobody reads.
        if weights.authority > 0.0 {
            self.authority = match (&self.store, &self.pack) {
                // A store is open, so use it: its map is keyed by document and
                // therefore covers graph-expanded hits, which carry no pack
                // row and so no `Hit.inbound`. A failed query degrades to
                // `Off`, exactly as it did when this was a tuple that stayed
                // at its default.
                (Some(store), _) => match store.inbound_link_counts() {
                    Ok(counts) => {
                        let max = counts.values().copied().max().unwrap_or(0);
                        Authority::Store(counts, max)
                    }
                    Err(_) => Authority::Off,
                },
                // No store: the pack carries the same counts, per row, and
                // this is the whole point of `pack.links` — authority no
                // longer forces `Database::new` onto the prompt path. A
                // storeless retriever can only produce pack hits, so every hit
                // `rank_weight` sees has its count already attached.
                (None, Some(pk)) => Authority::Pack(pk.max_inbound()),
                // Neither backend: nothing to weight, same as authority off.
                (None, None) => Authority::Off,
            };
        }
        self.weights = weights;
        self
    }

    /// Read vector hits from `pack` instead of the store.
    pub fn with_pack<P: Into<std::sync::Arc<crate::pack::Pack>>>(
        mut self,
        pack: Option<P>,
    ) -> Self {
        self.pack = pack.map(Into::into);
        self
    }

    pub fn with_memory<P: Into<std::sync::Arc<crate::pack::Pack>>>(
        mut self,
        opened: Result<Option<P>>,
        cfg: crate::config::MemoryConfig,
    ) -> Self {
        match opened {
            Ok(pack) => self.memory = pack.map(Into::into),
            Err(e) => self.memory_unavailable = Some(format!("{e:#}")),
        }
        self.memory_cfg = cfg;
        self
    }

    fn memory_decay(&self, hit: &Hit) -> f32 {
        match &hit.memory {
            Some(f) if f.kind == crate::memory::MemoryKind::Episode => {
                let age_days = (self.now_secs - f.created).max(0) as f32 / 86_400.0;
                self.memory_cfg.episode_decay(age_days)
            }
            _ => 1.0,
        }
    }

    /// The store, for the two call sites that fall back to it when no pack
    /// is present. `retrieve_for`/`build_retriever` guarantee the store is
    /// `Some` whenever the pack is `None` — a storeless `Retriever::packed`
    /// always carries `Some` pack — so reaching the `None` arm here means one
    /// of those constructors was bypassed. That is a construction bug, so it
    /// is reported as a clear error rather than panicking the process.
    fn require_store(&self, what: &str) -> Result<&Store> {
        self.store.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "{what} needs an open store, but this retriever has none and no pack \
                 is available either — this indicates the retriever was built without \
                 checking that a store or pack was actually available"
            )
        })
    }

    /// Combined ranking multiplier: source authority times link authority.
    ///
    /// The arithmetic is `Weights::authority_lift`'s and is unchanged by the
    /// pack; only where `(inbound, max)` comes from moved. Every branch that
    /// cannot supply a count yields `(0, 0)`, and `authority_lift(0, 0)` is
    /// exactly 1.0 — being unlinked has never been a demotion and must not
    /// become one.
    fn rank_weight(&self, hit: &Hit) -> f32 {
        let (inbound, max) = match &self.authority {
            Authority::Off => (0, 0),
            Authority::Store(counts, max) => (counts.get(&hit.doc_id).copied().unwrap_or(0), *max),
            Authority::Pack(max) => (hit.inbound, *max),
        };
        // The store's map wins when it has an entry, because it is keyed by
        // DOCUMENT and therefore covers graph-expanded hits, whose
        // `Hit.lifecycle` is `Current` by construction (no pack row to join).
        // Absent from the map means genuinely `Current`, so falling back to
        // the row's own byte is correct rather than merely convenient.
        let lifecycle = match &self.lifecycles {
            LifecycleSource::Row => hit.lifecycle,
            LifecycleSource::Store(map) => map.get(&hit.doc_id).copied().unwrap_or(hit.lifecycle),
        };
        self.weights.for_source(&hit.source_type)
            * self.weights.authority_lift(inbound, max)
            * self.weights.lifecycle_weight(lifecycle)
            * self
                .weights
                .decay_weight(&hit.source_type, hit.last_used, self.now_secs)
            * self.memory_decay(hit)
    }

    /// Weight `relevance` ITSELF and order by it — the single definition of
    /// how a hit's worth is decided, used by every path that can return.
    ///
    /// Every return in `run` must go through this before it gates or traces.
    /// The pre-fusion deadline path did neither: it returned raw `vector_hits`,
    /// so a transcript at cosine 0.78 cleared the hook's 0.70 gate on that path
    /// while its effective relevance was 0.78 x 0.45 = 0.35 and every other
    /// path cut it — the exact defect the long note at the fusion path's call
    /// site records as fixed, reachable again whenever tier 1 overran its
    /// budget. It also handed the dashboard an unweighted pool to draw a
    /// weighted gate line across.
    fn weight_and_order(&self, hits: &mut [Hit]) {
        for h in hits.iter_mut() {
            h.relevance *= self.rank_weight(h);
        }
        hits.sort_by(|a, b| {
            b.relevance
                .partial_cmp(&a.relevance)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });
    }

    /// Weight, record the pre-gate pool, then gate — the three steps every
    /// return in `run` owes its caller, in the one order that is correct.
    ///
    /// They are inseparable, so they live in one place. Weighting must precede
    /// the gate or the threshold reads a raw cosine; the trace must sit between
    /// them or the dashboard loses the hits the gate cut. Three shipped defects
    /// came from an exit path performing some of this and forgetting the rest —
    /// one forgot to weight, two forgot to trace — and each fix taught only the
    /// path in front of it. A fourth exit added later cannot repeat that: there
    /// is nothing left at the call site to forget.
    fn weight_trace_and_gate(
        &self,
        hits: &mut Vec<Hit>,
        min_relevance: f32,
        trace: Option<&mut Vec<StageTrace>>,
    ) {
        self.weight_and_order(hits);
        if let Some(t) = trace {
            t.push(StageTrace {
                name: "fused",
                hits: hits.iter().map(summarize).collect(),
            });
        }
        hits.retain(|h| h.relevance >= min_relevance);
    }

    pub fn search(&self, query: &str, p: &Profile) -> Result<Vec<Hit>> {
        Ok(self.run(query, p, 0.0, None)?.0)
    }

    /// Search, discarding anything below `min_relevance` BEFORE diversity
    /// selection runs.
    ///
    /// Callers that gate their own results must use this. Gating afterwards let
    /// MMR spend its slots on hits the caller was about to throw away, so a
    /// perfectly good match sitting just outside the diversity cut was lost for
    /// a reason that had nothing to do with how relevant it was.
    pub fn search_gated(&self, query: &str, p: &Profile, min_relevance: f32) -> Result<Vec<Hit>> {
        Ok(self.run(query, p, min_relevance, None)?.0)
    }

    /// `search_gated`, plus the stage report — so a caller can gate correctly
    /// AND tell whether the pipeline ran what the profile asked for.
    ///
    /// The hook needs both. Gating after the fact instead would undo the
    /// reason `search_gated` exists: the gate belongs before rerank and
    /// diversity, or MMR spends its slots on hits the caller will discard.
    pub fn search_gated_with_report(
        &self,
        query: &str,
        p: &Profile,
        min_relevance: f32,
    ) -> Result<(Vec<Hit>, StageReport)> {
        self.run(query, p, min_relevance, None)
    }

    pub fn search_with_report(&self, query: &str, p: &Profile) -> Result<(Vec<Hit>, StageReport)> {
        self.run(query, p, 0.0, None)
    }

    /// Search with the pipeline made visible. The collector costs nothing on
    /// the hook path — every existing caller passes `None`.
    ///
    /// Gates with `threshold` as `min_relevance`, exactly like `search_gated`
    /// does — see that method's doc comment. An earlier version passed 0.0
    /// here so the pipeline filtered nothing, then computed `injected` as a
    /// post-hoc filter over the UNGATED, post-MMR result. That let diversity
    /// selection spend a slot on a sub-threshold chunk and evict one that had
    /// actually cleared the bar, so `injected` no longer matched what
    /// `search_gated` — what the hook actually injects — would return. The
    /// pre-gate pool the dashboard wants to SHOW (so it can draw the hits the
    /// gate cut) is captured separately, as the `"fused"` stage recorded
    /// inside `run`, immediately before the gate is applied.
    ///
    /// `max_tokens` is the surface's injection budget, and it is what makes
    /// `injected` honest. Clearing the gate is not the same as being injected:
    /// `hook::build_context` stops once the budget is spent, so ten gated
    /// chunks are commonly three or four injected ones. `injected` is
    /// therefore the budget-admitted PREFIX of `fused`, counted by the same
    /// `hook::fit_to_budget` the hook renders with — the arithmetic exists
    /// once, so the column and the prompt cannot disagree. Everything the gate
    /// let through stays in `fused` (and the `"fused"` stage), so a chunk the
    /// budget dropped is still visible; it is simply not marked as injected.
    pub fn search_explained(
        &self,
        query: &str,
        p: &Profile,
        threshold: f32,
        max_tokens: usize,
    ) -> Result<Explain> {
        let t0 = Instant::now();
        let mut stages = Vec::new();
        let (hits, report) = self.run(query, p, threshold, Some(&mut stages))?;
        let fused: Vec<HitSummary> = hits.iter().map(summarize).collect();
        // `hits` is already gated at `threshold`, so it is exactly what
        // `search_gated` returns — i.e. exactly what the hook hands to
        // `build_context`. What that call would actually fit in the prompt is
        // the leading `admitted` of them.
        let admitted = crate::budget::admitted_by_budget(&hits, max_tokens);
        let injected: Vec<String> = fused
            .iter()
            .take(admitted)
            .map(|h| h.chunk_id.clone())
            .collect();
        Ok(Explain {
            stages,
            fused,
            threshold,
            injected,
            degraded: report.degraded,
            bm25_unavailable: report.bm25_unavailable,
            vectors_unavailable: report.vectors_unavailable,
            memory_unavailable: report.memory_unavailable,
            elapsed_ms: t0.elapsed().as_millis(),
        })
    }

    fn run(
        &self,
        query: &str,
        p: &Profile,
        min_relevance: f32,
        trace: Option<&mut Vec<StageTrace>>,
    ) -> Result<(Vec<Hit>, StageReport)> {
        let mut trace = trace; // reborrowable Option<&mut _>
        let t0 = Instant::now();
        let mut report = StageReport::default();
        let deadline = |r: &mut StageReport| -> bool {
            if t0.elapsed().as_millis() >= p.budget_ms as u128 {
                r.degraded = true;
                true
            } else {
                false
            }
        };

        let Some(pk) = &self.pack else {
            return Err(PackRefused(anyhow::anyhow!(
                "this index predates the retrieval pack and can no longer be searched \
                 directly — run `br8n index --compact` to rebuild the pack from the rows \
                 you already have (no re-embedding), or `br8n index --reindex` to rebuild \
                 from source"
            ))
            .into());
        };

        // Stage 1 — vector. Always runs; without it there are no results at all.
        // Tagged: this is the ONE call in the pipeline that talks to Ollama,
        // so it is the only failure a caller may report as an embedding
        // outage. Everything else that can fail below is the store.
        let embed_needed = needs_query_embed(
            Some(pk.has_vectors()),
            self.memory.as_ref().map(|m| m.has_vectors()),
        );
        let qv: Vec<f32> = if embed_needed {
            self.embedder.embed_query(query).map_err(EmbedUnavailable)?
        } else {
            Vec::new()
        };
        report.memory_unavailable = self.memory_unavailable.clone();
        // Both paths set `relevance` from a real cosine, which is the only
        // signal the gate may read. Nothing else in this function may write it.
        let main_vector_hits: Vec<Hit> = pk
            .search(&qv, p.candidates_k, p.efs)?
            .into_iter()
            .map(|(r, sim)| hit_from_record(r, sim, sim))
            .collect();
        let mut all_vector_hits = main_vector_hits.clone();
        if let Some(mem) = &self.memory {
            all_vector_hits.extend(
                mem.search(&qv, p.candidates_k, p.efs)?
                    .into_iter()
                    .map(|(r, sim)| hit_from_record(r, sim, sim)),
            );
            all_vector_hits.sort_by(|a, b| {
                b.relevance
                    .partial_cmp(&a.relevance)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.chunk_id.cmp(&b.chunk_id))
            });
            report.stages_run.push("memory");
        }
        // A pack with no vectors (phase 1 of an asynchronous index, see
        // `Manifest::rows_with_vectors`) answers `pk.search` with an honest
        // empty list rather than an error — see `Pack::search`'s doc comment
        // — so nothing above would otherwise say why the vector stage came up
        // empty. Mirrors `bm25_unavailable` below for the same reason: this
        // must not look like an honest no-match to a caller reading stderr.
        if let Some(pk) = &self.pack {
            if !pk.has_vectors() {
                report.vectors_unavailable =
                    Some("pack has no vectors yet (phase 1 of an asynchronous index)".to_string());
            }
        }
        report.stages_run.push("vector");
        if let Some(t) = trace.as_deref_mut() {
            t.push(StageTrace {
                name: "vector",
                hits: all_vector_hits.iter().map(summarize).collect(),
            });
        }

        // Tier 0 asks for nothing past vector search (no bm25, no graph, no
        // rerank). Running fusion/mmr anyway would be a no-op on quality but NOT
        // on `stages_run` — and relying on the *deadline* to skip them instead
        // is a race: measured, it passed alone and failed under load in the
        // same process (tier 0's 90ms budget sits right at the ~65-90ms warm
        // floor). Gate on what the profile asked for, not on how fast the
        // machine happened to be this run; the deadline check below still
        // covers the "asked for more but ran out of time" case genuinely.
        let wants_more = p.bm25 || p.graph.is_some() || p.rerank.is_some();
        if !wants_more || deadline(&mut report) {
            let mut hits = all_vector_hits;
            self.weight_trace_and_gate(&mut hits, min_relevance, trace.as_deref_mut());
            return Ok((truncate(hits), finish(report, t0)));
        }

        // Stage 2 — BM25.
        // Weights, not a bare list: see `rrf_weighted`. Vector and keyword are
        // direct evidence about THIS query; graph expansion is evidence about
        // what a document is connected to.
        let mut lists = vec![(1.0, all_vector_hits.clone())];
        if p.bm25 {
            // BM25 score is ordinal and unbounded — it can only ever be
            // `score`. `relevance: 0.0` means "never measured", exactly what
            // the store's own `fts_search` used to set; the measure stage
            // below is the only thing allowed to overwrite it with a real
            // cosine. See `Pack::bm25`'s doc comment for why conflating the
            // two shipped a defect class of its own.
            let fts: Option<Vec<Hit>> = match &self.pack {
                Some(pk) => Some(
                    pk.bm25(query, p.candidates_k)?
                        .into_iter()
                        .map(|(r, bm25_score)| hit_from_record(r, bm25_score, 0.0))
                        .collect(),
                ),
                // No pack: fall back to the store. `Store::fts_search` always
                // errors now — schema version 3 dropped lbug's FTS index, the
                // pack serves BM25 instead — so this is the narrow path where
                // that loss is actually felt. It must NOT propagate with `?`:
                // that would fail the whole query over a missing keyword
                // signal the vector stage never needed. Losing BM25 silently
                // (an empty list) would be indistinguishable from an honest
                // no-match, so the error is kept and reported on stderr by
                // the hook instead — see `StageReport::bm25_unavailable`.
                None => match self
                    .require_store("bm25 search")?
                    .fts_search(query, p.candidates_k)
                {
                    Ok(hits) => Some(hits),
                    Err(e) => {
                        report.bm25_unavailable = Some(e.to_string());
                        None
                    }
                },
            };
            if let Some(fts) = fts {
                lists.push((1.0, fts));
                report.stages_run.push("bm25");
                if let Some(t) = trace.as_deref_mut() {
                    let (_, fts) = lists.last().expect("bm25 list just pushed");
                    t.push(StageTrace {
                        name: "bm25",
                        hits: fts.iter().map(summarize).collect(),
                    });
                }
            }
            if let Some(mem) = &self.memory {
                let mem_fts: Vec<Hit> = mem
                    .bm25(query, p.candidates_k)?
                    .into_iter()
                    .map(|(r, s)| hit_from_record(r, s, 0.0))
                    .collect();
                if !mem_fts.is_empty() {
                    lists.push((1.0, mem_fts));
                }
            }
        }

        // Stage 3 — graph expansion from the strongest vector hits.
        if let Some(g) = p.graph {
            if !deadline(&mut report) {
                let seeds: Vec<String> = main_vector_hits
                    .iter()
                    .take(5)
                    .map(|h| h.chunk_id.clone())
                    .collect();
                // Unlike vector/bm25/measure, graph expansion has no pack arm —
                // the pack does not carry the property graph at all. A tier that
                // sets `graph` genuinely cannot be served without a store, so
                // this must fail loudly rather than silently degrade to "no
                // graph evidence", which would be indistinguishable from an
                // honest empty neighbourhood.
                let store = self.store.as_ref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "tier \"{}\" requires graph expansion, which needs an open \
                         store — this retriever was built pack-only (storeless)",
                        p.name
                    )
                })?;
                let mut expanded = store.expand(&seeds, g.hops, g.max_neighbors)?;
                if let (true, Some(pk)) = (self.weights.decay.enabled, &self.pack) {
                    for h in &mut expanded {
                        h.last_used = pk.last_used_of(&h.chunk_id);
                    }
                }
                lists.push((GRAPH_WEIGHT, expanded));
                report.stages_run.push("graph");
                // Reborrowed, not moved: the "fused" stage recorded further
                // down (the pre-gate pool) needs `trace` again after this.
                if let Some(t) = trace.as_deref_mut() {
                    let (_, graph_hits) = lists.last().expect("graph list just pushed");
                    t.push(StageTrace {
                        name: "graph",
                        hits: graph_hits.iter().map(summarize).collect(),
                    });
                }
            }
        }

        // Stage 4 — fusion. Rank-based, so mixed score scales need no normalizing.
        //
        // Guarded like every other optional stage. It used to run unconditionally,
        // so a run that had already blown its budget still paid for fusion, the
        // cosine backfill and diversity before returning. Vector hits are the
        // honest fallback: they are the only list that always exists.
        if deadline(&mut report) {
            let mut hits = all_vector_hits;
            self.weight_trace_and_gate(&mut hits, min_relevance, trace.as_deref_mut());
            return Ok((truncate(hits), finish(report, t0)));
        }
        let mut fused = fusion::rrf_weighted(lists, 60.0);
        report.stages_run.push("fusion");

        // Stage 4b — measure similarity for hits that never had one.
        //
        // BM25 and graph expansion match on words and on edges, so their hits
        // arrive with `relevance: 0.0` — "never measured", not "dissimilar".
        // Since the gate reads relevance, a keyword-only hit could not clear any
        // positive threshold at any tier, and at the fast tier (the prompt hook,
        // no reranker) nothing downstream could ever restore it. Backfilling the
        // real cosine costs one indexed lookup over the handful of hits that
        // vector search did not already return.
        let unmeasured: Vec<String> = fused
            .iter()
            .filter(|h| h.relevance <= 0.0)
            .map(|h| h.chunk_id.clone())
            .collect();
        if !unmeasured.is_empty() {
            // Same `(1 + s) / 2` scale on both arms — see `Pack::cosine_for`'s
            // doc comment and `pack_vector_search_and_pack_measure_agree_on_the_same_chunk`.
            let cos = match &self.pack {
                Some(pk) => pk.cosine_for(&qv, &unmeasured),
                None => self
                    .require_store("the measure stage")
                    .and_then(|store| store.cosine_for(&qv, &unmeasured)),
            };
            if let Ok(cos) = cos {
                for h in fused.iter_mut() {
                    if let Some(r) = cos.get(&h.chunk_id) {
                        h.relevance = *r;
                    }
                }
            }
            report.stages_run.push("measure");
        }
        if let Some(mem) = &self.memory {
            let still: Vec<String> = fused
                .iter()
                .filter(|h| h.relevance <= 0.0)
                .map(|h| h.chunk_id.clone())
                .collect();
            if !still.is_empty() {
                if let Ok(cos) = mem.cosine_for(&qv, &still) {
                    for h in fused.iter_mut() {
                        if let Some(r) = cos.get(&h.chunk_id) {
                            h.relevance = *r;
                        }
                    }
                }
            }
        }

        // Fusion decides WHICH candidates. The cosine decides their ORDER.
        //
        // RRF is ordinal: it knows a chunk placed third, not that it placed
        // third by a hair or by a mile. `relevance` is a real [0,1] similarity,
        // and once the measure stage above has one for every hit it is strictly
        // more informative than the rank sum. Measured over a 10-case golden
        // set, ordering by cosine instead of by RRF:
        //
        //     tier          RRF    cosine
        //     fast         0.80      0.90
        //     balanced     0.80      1.00
        //     thorough     0.80      1.00
        //     exhaustive   0.60      1.00
        //
        // BM25 and graph expansion still earn their place — they decide what
        // reaches this point, which is recall. It is their RANK contribution
        // that was hurting precision.
        // Weight relevance ITSELF, so the gate and the ordering agree.
        //
        // An earlier version weighted only the sort key, leaving `relevance`
        // raw so the threshold kept its calibrated meaning. That made the two
        // signals contradict each other: ordering had decided a transcript was
        // worth less, while the gate still read its unweighted cosine and let
        // it in at full strength. Measured on one query — ten hits
        // cleared 0.70 and six were transcripts, every one of them injected.
        //
        // `relevance` is therefore the EFFECTIVE relevance of a hit: how
        // similar it is, discounted by how authoritative its source is. The
        // threshold is calibrated against that, and `br8n bench` measures it.
        // Gating happens here too, BEFORE rerank and diversity, so neither
        // spends its budget or its slots on hits the caller has already decided
        // it will not use. Last use of `trace` in this function: a plain move
        // is fine, no reborrow needed.
        self.weight_trace_and_gate(&mut fused, min_relevance, trace);

        // Stage 5 — rerank.
        if let Some(rr) = &p.rerank {
            if !deadline(&mut report) {
                // Give the reranker only the time that is actually left.
                let left = (p.budget_ms as u128).saturating_sub(t0.elapsed().as_millis());
                fused = rerank::Reranker::with_budget(&self.ollama_url, &rr.model, left as u64)
                    .rerank(query, fused, rr.top_n);
                report.stages_run.push("rerank");
            }
        }

        // Stage 6 — diversity. Also guarded: MMR is O(n*k) similarity
        // comparisons over the candidate set, which is not free at tier 4.
        // Truncating in rank order is the correct degradation — it keeps the
        // best hits and only gives up the variety pass.
        if deadline(&mut report) {
            return Ok((truncate(fused), finish(report, t0)));
        }
        let out = fusion::mmr(fused, p.mmr_lambda, MAX_RESULTS);
        report.stages_run.push("mmr");

        Ok((out, finish(report, t0)))
    }

    /// `None` for a storeless, pack-only retriever (see `Retriever::packed`).
    pub fn store(&self) -> Option<&Store> {
        self.store.as_ref()
    }
}

pub(crate) fn needs_query_embed(
    main_has_vectors: Option<bool>,
    memory_has_vectors: Option<bool>,
) -> bool {
    match main_has_vectors {
        None | Some(true) => true,
        Some(false) => memory_has_vectors == Some(true),
    }
}

fn hit_from_record(r: crate::pack::records::Record, score: f32, relevance: f32) -> Hit {
    Hit {
        chunk_id: r.chunk_id,
        doc_id: r.doc_id,
        text: r.text,
        heading_path: r.heading_path,
        uri: r.uri,
        title: r.title,
        page_no: r.page_no,
        score,
        relevance,
        source_type: r.source_type,
        inbound: r.inbound,
        lifecycle: r.lifecycle,
        last_used: r.last_used,
        memory: r.memory,
    }
}

fn truncate(mut hits: Vec<Hit>) -> Vec<Hit> {
    hits.truncate(MAX_RESULTS);
    hits
}

fn finish(mut r: StageReport, t0: Instant) -> StageReport {
    r.elapsed_ms = t0.elapsed().as_millis();
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `weight_and_order` reads `self.weights` and `self.authority` and nothing
    /// else — in particular it never embeds. This makes that explicit: if the
    /// method ever grows an embed call, it gets an error instead of a model.
    struct NeverEmbeds;

    impl Embedder for NeverEmbeds {
        fn embed_documents(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
            anyhow::bail!("weight_and_order must never embed")
        }
        fn embed_query(&self, _text: &str) -> Result<Vec<f32>> {
            anyhow::bail!("weight_and_order must never embed")
        }
        fn warm(&self) -> Result<()> {
            anyhow::bail!("weight_and_order must never embed")
        }
        fn model_id(&self) -> String {
            "never".into()
        }
        fn dimensions(&self) -> usize {
            0
        }
    }

    /// Built field by field rather than through `Retriever::new`/`packed`,
    /// which each demand a backend this has no use for: weighting and ordering
    /// touch neither the store nor the pack. Constructing the struct here is
    /// what keeps `weight_and_order` private — an integration test in `tests/`
    /// cannot reach a private method, and widening the method's visibility to
    /// suit a test would be a worse trade than this unit test.
    fn weighting_retriever() -> Retriever {
        Retriever {
            weights: crate::config::Config::default().weights,
            lifecycles: LifecycleSource::Row,
            authority: Authority::Off,
            store: None,
            embedder: Box::new(NeverEmbeds),
            ollama_url: String::new(),
            pack: None,
            memory: None,
            memory_cfg: Default::default(),
            memory_unavailable: None,
            now_secs: crate::memory::now_secs(),
        }
    }

    /// A graph-expanded hit is demoted from the STORE's map, not from its own
    /// `Hit.lifecycle`.
    ///
    /// `Store::expand` builds a hit straight from the database, so it has no
    /// pack row and arrives `Current` however its document is marked. Before
    /// `LifecycleSource`, every graph-expanded hit escaped demotion.
    ///
    /// `expand`'s kind filter did not retire this. It stops the `supersedes`
    /// tombstone pointer being followed; it keeps `wikilink`, so a superseded
    /// document that another note links to in prose still arrives here, still
    /// carrying `Current`, and still needs the map.
    ///
    /// The fixture is the shape that actually occurs: the hit says `Current`
    /// (as a store hit always does) while the map says `Superseded`. A test
    /// that set `Hit.lifecycle` itself would pass with the whole
    /// `LifecycleSource` lookup deleted.
    #[test]
    fn an_unused_transcript_ranks_below_a_recently_used_one() {
        let mut r = weighting_retriever();
        r.weights.decay.enabled = true;
        r.now_secs = 1_800_000_000;
        let mut stale = hit("aa_stale:0", 0.80);
        stale.last_used = Some(r.now_secs - 730 * 86_400);
        let mut recent = hit("zz_recent:0", 0.80);
        recent.last_used = Some(r.now_secs - 3 * 86_400);

        let mut hits = vec![stale, recent];
        r.weight_and_order(&mut hits);

        assert_eq!(hits[0].chunk_id, "zz_recent:0");
        let undecayed = 0.80 * r.weights.for_source("transcript");
        assert!((hits[0].relevance - undecayed).abs() < 1e-6);
        assert!(
            (hits[1].relevance - undecayed * r.weights.decay.floor).abs() < 1e-6,
            "two years unused sits on the floor, in relevance itself: {}",
            hits[1].relevance
        );
    }

    #[test]
    fn a_graph_expanded_hit_is_demoted_from_the_stores_map_not_its_own_byte() {
        use crate::pack::status::Lifecycle;

        let mut r = weighting_retriever();
        let mut h = hit("c1", 0.80);
        // Exactly what `Store::expand` produces: no row, so no byte.
        assert_eq!(
            h.lifecycle,
            Lifecycle::Current,
            "fixture precondition: a store hit carries Current"
        );
        h.doc_id = "doc-superseded".into();

        // Source: Row — nothing knows this document is dead.
        let undemoted = r.rank_weight(&h);

        // Source: Store — the map is keyed by document, so it reaches this hit.
        r.lifecycles = LifecycleSource::Store(
            [("doc-superseded".to_string(), Lifecycle::Superseded)]
                .into_iter()
                .collect(),
        );
        let demoted = r.rank_weight(&h);

        assert!(
            demoted < undemoted,
            "the store's map must demote a hit whose own byte says Current; \
             got {demoted} against {undemoted}"
        );
        // And by the shipped amount, not merely "less" — a token demotion
        // would satisfy the line above while changing nothing that matters.
        let expected = undemoted * crate::config::Config::default().weights.superseded;
        assert!(
            (demoted - expected).abs() < 1e-6,
            "expected {expected} (undemoted x the shipped superseded weight), got {demoted}"
        );

        // A document ABSENT from the map keeps its own byte. `all_lifecycles`
        // omits Current documents to keep the map small, so absence must mean
        // Current rather than "unknown".
        let mut other = hit("c2", 0.80);
        other.doc_id = "doc-not-in-map".into();
        assert_eq!(
            r.rank_weight(&other),
            undemoted,
            "a document the map does not mention must not be demoted"
        );
    }

    fn hit(chunk_id: &str, relevance: f32) -> Hit {
        Hit {
            chunk_id: chunk_id.into(),
            doc_id: format!("doc-of-{chunk_id}"),
            text: format!("body of {chunk_id}"),
            heading_path: String::new(),
            uri: format!("file:///{chunk_id}.md"),
            title: chunk_id.into(),
            page_no: None,
            score: 0.0,
            relevance,
            // One source type across the whole fixture on purpose:
            // `rank_weight` multiplies by `weights.for_source`, so a mixed
            // fixture would quietly dissolve the tie this test is about.
            // `transcript` rather than `markdown` because its default weight
            // is 0.45, not 1.0 — a 1.0 multiplier is a fixed point, and a tie
            // that survives a no-op is not evidence the weighting ran.
            source_type: "transcript".into(),
            inbound: 0,
            lifecycle: Default::default(),
            last_used: None,
            memory: None,
        }
    }

    /// `weight_and_order` sorts on (weighted relevance DESC, chunk_id ASC), and
    /// the second key was unpinned by anything.
    ///
    /// Every path that returns from `run` goes through this method, so an
    /// unstable order here is an unstable answer for the same prompt — the
    /// same defect `rrf_output_order_is_reproducible_when_scores_tie` pins one
    /// stage earlier, at the point where the result is actually handed back.
    #[test]
    fn weight_and_order_breaks_a_relevance_tie_on_chunk_id() {
        let r = weighting_retriever();
        assert_eq!(
            r.weights.for_source("transcript"),
            0.45,
            "the fixture assumes the shipped transcript weight, which is not 1.0"
        );

        // Six hits tied at relevance 0.8 with one clear winner above them and
        // one clear loser below. The outliers are named so that sorting by
        // chunk_id ALONE would fail: "aa_last" must finish last and "zz_first"
        // must finish first.
        let build = || {
            vec![
                hit("c6", 0.8),
                hit("aa_last", 0.5),
                hit("c5", 0.8),
                hit("c4", 0.8),
                hit("zz_first", 0.9),
                hit("c3", 0.8),
                hit("c2", 0.8),
                hit("c1", 0.8),
            ]
        };
        assert_eq!(
            build()[0].chunk_id,
            "c6",
            "the input must not already be in the expected output order"
        );

        let mut hits = build();
        r.weight_and_order(&mut hits);

        let order: Vec<&str> = hits.iter().map(|h| h.chunk_id.as_str()).collect();
        assert_eq!(
            order,
            vec!["zz_first", "c1", "c2", "c3", "c4", "c5", "c6", "aa_last"],
            "hits tied on weighted relevance must come back in ascending chunk_id, \
             below the stronger hit and above the weaker one"
        );

        // The tie has to be real AFTER weighting, not merely before it: the
        // sort reads the product, so that is what must be bit-identical.
        let tied: Vec<u32> = hits
            .iter()
            .filter(|h| h.chunk_id.starts_with('c'))
            .map(|h| h.relevance.to_bits())
            .collect();
        assert_eq!(tied.len(), 6, "six hits were supposed to tie");
        assert!(
            tied.iter().all(|b| *b == tied[0]),
            "the tie is only real if every weighted relevance is the identical f32"
        );
        assert_ne!(
            f32::from_bits(tied[0]),
            0.8,
            "weighting must actually have run — 0.8 unchanged would mean the \
             multiplier was a no-op and the tie proved nothing about it"
        );

        // Reproducibility on its own would pass with the tie-break deleted:
        // `sort_by` is stable, so a fixed input gives a fixed output whatever
        // the comparator does. Asserted anyway because it is the property the
        // hook depends on; the ordering assertion above pins how it is reached.
        let owned: Vec<String> = order.iter().map(|s| (*s).to_string()).collect();
        for _ in 0..50 {
            let mut again = build();
            r.weight_and_order(&mut again);
            let ids: Vec<String> = again.into_iter().map(|h| h.chunk_id).collect();
            assert_eq!(ids, owned, "ordering must not vary between identical calls");
        }
    }

    /// The lifecycle multiplier reaches `relevance` ITSELF, so the injection
    /// gate and the ordering read one product. A demotion that moved the order
    /// but not the gate would inject a superseded record ranked last instead of
    /// not injecting it — this feature's own defect, one layer down.
    #[test]
    fn the_lifecycle_multiplier_reaches_relevance_and_the_ordering_together() {
        let mut r = weighting_retriever();
        r.weights.superseded = 0.5;

        let mut dead = hit("aa_dead", 0.80);
        dead.lifecycle = crate::pack::status::Lifecycle::Superseded;
        let mut live = hit("zz_live", 0.80);
        live.lifecycle = crate::pack::status::Lifecycle::Current;

        // `aa_dead` sorts FIRST on chunk_id, so if the weighting did nothing the
        // tie-break alone would leave it in front. It must not be in front.
        let mut hits = vec![dead, live];
        r.weight_and_order(&mut hits);

        // transcript 0.45 x superseded 0.5 x 0.80 = 0.18
        let d = hits.iter().find(|h| h.chunk_id == "aa_dead").unwrap();
        assert!(
            (d.relevance - 0.18).abs() < 1e-5,
            "0.80 x 0.45 x 0.5 = 0.18, got {}",
            d.relevance
        );
        // transcript 0.45 x current 1.0 x 0.80 = 0.36
        let l = hits.iter().find(|h| h.chunk_id == "zz_live").unwrap();
        assert!(
            (l.relevance - 0.36).abs() < 1e-5,
            "0.80 x 0.45 x 1.0 = 0.36, got {}",
            l.relevance
        );

        assert_eq!(
            hits[0].chunk_id, "zz_live",
            "the demoted hit must also SORT lower — one product drives both, \
             and `aa_dead` would win the chunk_id tie-break if it did not"
        );
    }

    /// The gate half, stated as the number the hook actually compares.
    #[test]
    fn a_demoted_hit_falls_below_the_gate_it_previously_cleared() {
        let mut r = weighting_retriever();
        r.weights.transcript = 1.0; // isolate the lifecycle factor
        r.weights.superseded = 0.9;

        let mut h = hit("dead", 0.70);
        h.lifecycle = crate::pack::status::Lifecycle::Superseded;
        let mut hits = vec![h];
        r.weight_and_order(&mut hits);

        assert!(
            hits[0].relevance < 0.66,
            "0.70 x 0.9 = 0.63 must fall below the 0.66 hook gate, got {}",
            hits[0].relevance
        );
    }

    #[test]
    fn the_query_is_embedded_unless_every_present_pack_is_vectorless() {
        use super::needs_query_embed as n;
        assert!(n(None, None));
        assert!(n(None, Some(false)));
        assert!(n(None, Some(true)));
        assert!(n(Some(true), None));
        assert!(n(Some(true), Some(false)));
        assert!(n(Some(true), Some(true)));
        assert!(!n(Some(false), None));
        assert!(!n(Some(false), Some(false)));
        assert!(n(Some(false), Some(true)));
    }
}
