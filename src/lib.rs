pub mod audit;
#[cfg(feature = "backup")]
pub mod backup;
pub mod bench;
pub mod budget;
pub mod chunk;
pub mod config;
pub mod dashboard;
pub mod doctor;
pub mod embed;
pub mod enrich;
pub mod env_file;
pub mod golden;
pub mod hook;
pub mod index;
pub mod loaders;
pub mod mcp;
pub mod memory;
pub mod model;
pub mod pack;
pub mod retrieve;
pub mod setup;
pub mod store;
pub mod update;
pub mod usage;

use anyhow::Result;
use std::sync::Arc;

/// A retriever weighted the way `surface` asked to be weighted, built against
/// that surface's own configured quality tier.
///
/// The surface is not optional, because every caller in here gates its results
/// and the weights multiply the very field the gate reads. A retriever built
/// without knowing which surface will use it is one whose threshold is being
/// compared against a scale nobody configured.
///
/// Equivalent to `retrieve_for_profile(cfg, surface, &cfg.profile_for(surface))`.
/// Callers that search at a profile OTHER than the surface's own default —
/// the dashboard's ad hoc tier override, `br8n bench`'s sweep across all five
/// tiers on one retriever — must call `retrieve_for_profile` directly with the
/// profile they will actually use, or the store-vs-pack decision below is made
/// against the wrong tier.
pub fn retrieve_for(cfg: &config::Config, surface: config::Surface) -> Result<retrieve::Retriever> {
    let profile = cfg.profile_for(surface);
    retrieve_for_profile(cfg, surface, &profile)
}

/// `retrieve_for`, but the store-vs-pack decision is made against `profile`
/// rather than `surface`'s configured default profile. See `retrieve_for`'s
/// doc comment for when that distinction matters.
pub fn retrieve_for_profile(
    cfg: &config::Config,
    surface: config::Surface,
    profile: &config::Profile,
) -> Result<retrieve::Retriever> {
    let db = config::Config::db_path();
    let embedder = embed::for_config(&cfg.embed)?;
    let model_id = embedder.model_id();
    // A missing pack is not an error: an index built before packs existed
    // still opens, and the store-backed retriever it gets refuses loudly once
    // a query actually runs. A pack that exists but does not validate IS an
    // error — reported here rather than swallowed, because a stale pack
    // silently attaches correct-looking relevance to the wrong documents. See
    // `pack::open_pack_beside` for the missing-vs-invalid split.
    //
    // Looked up before any decision about the store: this is a filesystem
    // check plus an mmap, not a database connection, so paying for it costs
    // nothing on the path this function exists to make store-free.
    let pack = pack::open_pack_beside(&db, &model_id, cfg.embed.dimensions)?.map(Arc::new);
    let memory = crate::memory::open_pack(cfg, &model_id).map(|m| m.map(Arc::new));
    retrieve_from_opened(cfg, surface, profile, embedder, pack, memory)
}

pub fn retrieve_from_opened(
    cfg: &config::Config,
    surface: config::Surface,
    profile: &config::Profile,
    embedder: Box<dyn embed::Embedder>,
    pack: Option<Arc<pack::Pack>>,
    memory: Result<Option<Arc<pack::Pack>>>,
) -> Result<retrieve::Retriever> {
    let db = config::Config::db_path();
    // RE-ENABLED, reversing the MEASURED HOLD below, now that the caller
    // tells us whether the profile can be served without a store at all.
    //
    // On the 100-case golden set, same corpus, quiet machine, bench repeatable
    // to 0.01, the pack loses recall at every tier where it matters:
    //
    //     tier         store   pack    latency
    //     instant      0.52    0.42    -48%
    //     fast (hook)  0.83    0.76    -17%
    //     balanced     0.85    0.80    -17%
    //     thorough     0.86    0.84    -22%
    //     exhaustive   0.87    0.87    -24%
    //
    // Seven cases in a hundred at the hook's tier, and that recall loss is
    // exactly why an earlier change turned the pack back off: stage 1a's own BM25
    // still came from lbug, so the store opened anyway and the 110-202ms of
    // `Database::new` the pack exists to remove was never actually removed —
    // a bad trade of recall for latency nobody collected. Tasks 1-5 of stage
    // 1b moved BM25 and the measure stage onto the pack too, and THIS task
    // makes the store optional in the first place, so a tier that needs
    // neither graph expansion nor authority weighting (tier 1, the hook's
    // default) now removes the `Database::new` cost for real. The recall
    // table above has not been re-measured since; task 7 re-benches it and
    // returns the decision — re-enabling here is necessary for that
    // measurement to be possible at all, not a verdict that the trade is now
    // good.
    //
    // The pack is still BUILT on every index run and still validated on open:
    // it is what exposed both the stale FTS index and the split relevance
    // scale, and a pack that disagrees with its manifest must still refuse
    // rather than serve the right score against the wrong document.
    //
    // Graph expansion is the only thing left that a pack cannot serve.
    //
    // Authority weighting used to be the other half of this condition, and it
    // was the expensive half: ANY non-zero `authority` sent EVERY tier down
    // the store path, because `inbound_link_counts` lived only in the
    // database. Measured on the live index, same binary, five runs each:
    // 0.22-0.27s of tier-1 wall clock with `authority = 0.3` against
    // 0.10-0.12s with `0.0` — roughly half the prompt latency for a signal
    // whose recall effect measured as a wash. The counts are now published in
    // `pack.links`, joined by the row ordinal like every other pack file, so
    // authority costs nothing here and is a real option again.
    //
    // A pack must actually be present for that: with none, the counts are
    // still store-only, so `authority > 0.0` opens the store exactly as before
    // rather than silently dropping the lift.
    let needs_store =
        profile.graph.is_some() || (cfg.weights_for(surface).authority > 0.0 && pack.is_none());

    let retriever = if !needs_store {
        match pack {
            // The store-free path this task exists for: no `Database::new`,
            // no `LOAD EXTENSION`, nothing but the mmap'd pack.
            Some(pk) => retrieve::Retriever::packed(pk, embedder, cfg.embed.ollama_url.clone()),
            // No pack validated (an index built before packs existed, or one
            // that failed validation above would already have returned `Err`)
            // — vector/bm25/measure still need somewhere to read from.
            None => {
                let store = store::Store::open_existing(&db, cfg.embed.dimensions)?;
                retrieve::Retriever::new(store, embedder, cfg.embed.ollama_url.clone())
            }
        }
    } else {
        // Graph expansion, or authority weighting with no pack to read the
        // counts from: the store opens regardless. The pack is still attached
        // when present — vector/bm25/measure read it either way, and the
        // `Database::new` cost is already sunk once `needs_store` is true.
        // `with_weights` then prefers the STORE's link counts over the pack's,
        // because graph-expanded hits carry no pack row.
        let store = store::Store::open_existing(&db, cfg.embed.dimensions)?;
        retrieve::Retriever::new(store, embedder, cfg.embed.ollama_url.clone()).with_pack(pack)
    };

    Ok(retriever
        .with_memory(memory, cfg.memory.clone())
        .with_weights(cfg.weights_for(surface)))
}

pub fn related_for(cfg: &config::Config, uri: &str) -> Result<Vec<store::Hit>> {
    let store = store::Store::open_existing(&config::Config::db_path(), cfg.embed.dimensions)?;
    let doc_id = model::Document::new_id(uri);
    store.expand(&[model::Chunk::id(&doc_id, 0)], 2, 10)
}

/// Re-index everything. This is what the MCP `br8n_index` tool calls.
///
/// It delegates to `index::reindex_swap` rather than writing to the live
/// database directly. Writing directly skipped BOTH protections the CLI path
/// has: the `IndexLock` (so an MCP index racing the `SessionStart` indexer
/// could interleave filesystem mutations) and the shadow swap (so the hook,
/// a separate process, got nothing at all for the duration — LadybugDB holds
/// an exclusive OS file lock). Same entrypoint, same guarantees.
pub fn index_now(cfg: &config::Config) -> Result<index::IndexStats> {
    index::reindex_swap(cfg)
}
