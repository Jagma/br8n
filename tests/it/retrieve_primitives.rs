use crate::common;
use br8n::model::{Document, SourceType};
use br8n::pack::status::Lifecycle;

fn seeded() -> (tempfile::TempDir, br8n::index::Indexer) {
    let (d, idx, _calls) = common::setup();
    let docs = vec![
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
    ];
    idx.index_documents(&docs).unwrap();
    (d, idx)
}

// `fts_search_matches_exact_terms_vectors_would_miss`,
// `fts_only_hit_carries_zero_relevance`, and
// `fts_search_on_a_nonsense_term_returns_empty_not_an_error` used to live
// here, exercising the store's own `fts_search` against lbug's FTS index.
// That index is gone as of schema version 3 — `Store::fts_search` now always
// errors (see `the_store_says_bm25_moved_rather_than_returning_nothing`
// below) — so those three tests were deleted rather than rewritten: the
// behaviour they protected moved to the pack, and lives on there.
// `tests/it/pack.rs`'s `postings_rank_rows_by_bm25` and
// `a_rare_term_beats_a_common_one` cover exact-term / ranking; this file's
// own `tier_1_bm25_stage_hits_carry_zero_relevance_before_measure` covers the
// zero-relevance invariant, through the pack path the pipeline actually uses.

#[test]
fn hydrate_resolves_chunk_ids_to_full_hits() {
    // The chunk id is computed directly from the fixture's own document uri
    // rather than found by a search — `seeded()`'s first document is
    // deterministic, so `Chunk::id` reproduces the id `index_documents` wrote
    // without needing any retriever to find it first. What this pins is
    // `hydrate` alone: does the id round-trip to the same chunk's fields.
    let (_d, idx) = seeded();
    let doc_id = br8n::model::Document::new_id("file:///a.md");
    let chunk_id = br8n::model::Chunk::id(&doc_id, 0);
    let rehydrated = idx
        .store()
        .hydrate(std::slice::from_ref(&chunk_id))
        .unwrap();
    assert_eq!(rehydrated.len(), 1);
    assert_eq!(rehydrated[0].chunk_id, chunk_id);
    assert_eq!(rehydrated[0].title, "Pooling");
}

/// `Retriever` itself must read the pack, not just `Pack::search` in
/// isolation. The store handed to the retriever here is opened on an empty,
/// never-indexed directory, so it has nothing to fall back to — any hit
/// `search_gated` returns can only have come from the pack, which is exactly
/// what proves the wiring in `run`'s vector stage, not the fallback branch.
#[test]
fn retriever_reads_vector_hits_from_the_pack_when_one_is_present() {
    let (_d, idx) = seeded();
    let rows = idx.store().all_rows_for_pack().unwrap();

    let pack_dir = tempfile::tempdir().unwrap();
    br8n::pack::Pack::build(
        pack_dir.path(),
        "fake@4",
        4,
        rows,
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    let pack = br8n::pack::Pack::open(pack_dir.path(), "fake@4", 4).unwrap();

    let empty_dir = tempfile::tempdir().unwrap();
    let empty_store = br8n::store::Store::open(empty_dir.path(), 4).unwrap();

    let retriever = br8n::retrieve::Retriever::new(
        empty_store,
        Box::new(common::FakeEmbedder {
            calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            queries: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }),
        "http://localhost:11434".into(),
    )
    .with_pack(Some(pack));

    let profile = br8n::config::Profile {
        name: "pack-arm-test",
        candidates_k: 2,
        efs: 32,
        bm25: false,
        graph: None,
        rerank: None,
        mmr_lambda: 0.7,
        budget_ms: 1000,
    };

    let hits = retriever.search_gated("pooling", &profile, 0.01).unwrap();
    assert!(
        !hits.is_empty(),
        "an empty store with a real pack attached must still gate hits through, \
         proving the vector stage read the pack"
    );

    // The pack arm in `Retriever::run` assigns ten `Hit` fields by hand from a
    // `Record`; five of them (`text`, `heading_path`, `uri`, `title`,
    // `source_type`) are `String`, so a transposition (e.g. `uri: r.title`)
    // compiles cleanly and would corrupt every citation the hook emits.
    // `!hits.is_empty()` alone cannot catch that. Ground truth is computed
    // directly here — the same cosine the pack itself computes, over the
    // same rows and the same query embedding `FakeEmbedder` deterministically
    // produces — independent of the pack's own search/hydrate path under
    // test.
    let qv = common::hash_vec("pooling");
    let rows = idx.store().all_rows_for_pack().unwrap();
    let expected = rows
        .iter()
        .max_by(|(_, a), (_, b)| {
            dot(&qv, a)
                .partial_cmp(&dot(&qv, b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(r, _)| r)
        .expect("fixture must have at least one row");
    assert_eq!(
        hits[0].chunk_id, expected.chunk_id,
        "pack arm must map chunk_id from the matching record field"
    );
    assert_eq!(
        hits[0].uri, expected.uri,
        "pack arm must map uri from the matching record field, not a swapped one"
    );
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// TWO roles may write `Hit.relevance`: `vectors::Reader::search`
/// (`src/pack/vectors.rs`) for vector search, and `Pack::cosine_for`
/// (`src/pack/mod.rs`) for the post-fusion measure stage. Both must produce
/// the same number for the same chunk and the same query, or the gate and
/// the ordering read whichever wrote last.
///
/// The defect this guards against is real: `Store::cosine_for` once returned
/// the raw cosine `s` rather than `(1 + s) / 2`, so at s = 0.4 it said 0.40
/// while vector search said 0.70 for the same similarity — a backfilled
/// BM25-only hit was systematically under-ranked against a vector hit and cut
/// by a threshold it had actually cleared. It hid because it only bites when
/// the measure stage is busy, and a stale FTS index meant few BM25-only hits
/// ever reached it.
#[test]
fn pack_vector_search_and_pack_measure_agree_on_the_same_chunk() {
    let (_d, idx) = seeded();
    let qv = br8n::embed::normalize(vec![1.0, 0.0, 0.0, 0.0]);
    let rows = idx.store().all_rows_for_pack().unwrap();

    let dir = tempfile::tempdir().unwrap();
    br8n::pack::Pack::build(
        dir.path(),
        "fake@4",
        4,
        rows,
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    let p = br8n::pack::Pack::open(dir.path(), "fake@4", 4).unwrap();

    let hits = p.search(&qv, 2, 64).unwrap();
    assert!(!hits.is_empty(), "fixture must return something to compare");

    let ids: Vec<String> = hits.iter().map(|(r, _)| r.chunk_id.clone()).collect();
    let measured = p.cosine_for(&qv, &ids).unwrap();

    for (r, sim) in &hits {
        let m = measured
            .get(&r.chunk_id)
            .unwrap_or_else(|| panic!("no measured relevance for {}", r.chunk_id));
        assert!(
            (sim - m).abs() < 1e-2,
            "pack vector search says {sim} and pack measure says {m} for {} — the gate \
             and the ordering read whichever wrote last",
            r.chunk_id
        );
    }
}

/// With a pack present, tier 1 must answer from it alone. The store here is an
/// EMPTY directory, so any hit proves the pack served the whole pipeline —
/// vector, bm25 and the measure backfill.
#[test]
fn tier_1_answers_from_the_pack_without_the_store() {
    let (_d, idx) = seeded();
    let rows = idx.store().all_rows_for_pack().unwrap();
    let packdir = tempfile::tempdir().unwrap();
    br8n::pack::Pack::build(
        packdir.path(),
        "fake@4+plain",
        4,
        rows,
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    let pack = br8n::pack::Pack::open(packdir.path(), "fake@4+plain", 4).unwrap();

    let empty = tempfile::tempdir().unwrap();
    let store = br8n::store::Store::open(empty.path(), 4).unwrap();
    let r = br8n::retrieve::Retriever::new(
        store,
        Box::new(common::FakeEmbedder {
            calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            queries: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }),
        "http://127.0.0.1:1".into(),
    )
    .with_pack(Some(pack));

    let (hits, report) = r
        .search_with_report("pooling", &br8n::config::Profile::tier(1))
        .unwrap();
    assert!(
        !hits.is_empty(),
        "the pack must serve tier 1 with an empty store"
    );
    assert!(
        report.stages_run.contains(&"bm25"),
        "bm25 must have run from the pack, got {:?}",
        report.stages_run
    );
}

/// The test above (`tier_1_answers_from_the_pack_without_the_store`) passes
/// even before BM25 and the measure stage are wired to the pack: tier 1's
/// candidates_k (20) exceeds this fixture's two chunks, so vector search alone
/// (already pack-backed since stage 1a) fills `hits`, and `"bm25"` lands in
/// `stages_run` whether the store's `fts_search` finds anything or not — an
/// empty store's FTS index still returns `Ok([])`, not an error. That test
/// cannot tell "bm25 ran against the pack" apart from "bm25 ran against an
/// empty store and found nothing" — it does not fail before this task's
/// implementation exists, so it does not drive it.
///
/// This test drives it instead. The pack holds only Pooling/Baking. A SEPARATE
/// store holds a third, unrelated document with a term that appears nowhere
/// in the pack. If BM25 or the measure backfill still read that store instead
/// of the pack, a query for the store-only term finds it by exact keyword
/// match and the measure stage gives it a real cosine — so it surfaces in
/// results. Once both stages are pack-backed, that store is never consulted at
/// all, so the term can never be found.
#[test]
fn tier_1_bm25_and_measure_never_see_a_document_that_only_the_store_holds() {
    let (_d, idx) = seeded();
    let rows = idx.store().all_rows_for_pack().unwrap();
    let packdir = tempfile::tempdir().unwrap();
    br8n::pack::Pack::build(
        packdir.path(),
        "fake@4+plain",
        4,
        rows,
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    let pack = br8n::pack::Pack::open(packdir.path(), "fake@4+plain", 4).unwrap();

    // A store with a document the pack has never seen, holding a term that
    // appears nowhere in the pack's corpus.
    let (rogue_dir, rogue_idx, _calls) = common::setup();
    rogue_idx
        .index_documents(&[Document::new(
            SourceType::Markdown,
            "file:///rogue.md",
            "Rogue",
            "# Rogue\n\nZzyzxquokka is a word that appears nowhere else.",
        )])
        .unwrap();
    drop(rogue_idx); // release the write connection before reopening it
    let rogue_store = br8n::store::Store::open(rogue_dir.path(), 4).unwrap();

    let r = br8n::retrieve::Retriever::new(
        rogue_store,
        Box::new(common::FakeEmbedder {
            calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            queries: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }),
        "http://127.0.0.1:1".into(),
    )
    .with_pack(Some(pack));

    let (hits, report) = r
        .search_with_report("zzyzxquokka", &br8n::config::Profile::tier(1))
        .unwrap();
    assert!(
        report.stages_run.contains(&"bm25"),
        "bm25 must still run, got {:?}",
        report.stages_run
    );
    assert!(
        hits.iter().all(|h| h.title != "Rogue"),
        "the store-only document must never surface once bm25 and measure \
         read the pack instead: got {:?}",
        hits.iter().map(|h| &h.title).collect::<Vec<_>>()
    );
}

/// Mutation guard for the signal boundary CLAUDE.md opens with: BM25 is
/// unbounded, so it may only ever land on `Hit.score`. `relevance: 0.0` on
/// the pack's bm25 arm means "never measured" until the post-fusion measure
/// stage backfills a real cosine.
///
/// This does not go through the gate: with only two chunks in the corpus and
/// `candidates_k` (20) exceeding that, every chunk also comes back from
/// vector search, and `rrf_weighted` keeps the MAX relevance across lists for
/// a chunk found by both — so a raw BM25 score smaller than the chunk's own
/// vector relevance would hide behind that max and this test would not catch
/// it. The bm25 stage's OWN trace, captured before fusion ever runs, has no
/// such blind spot: it is what the code under test actually produced.
#[test]
fn tier_1_bm25_stage_hits_carry_zero_relevance_before_measure() {
    let (_d, idx) = seeded();
    let rows = idx.store().all_rows_for_pack().unwrap();
    let packdir = tempfile::tempdir().unwrap();
    br8n::pack::Pack::build(
        packdir.path(),
        "fake@4+plain",
        4,
        rows,
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    let pack = br8n::pack::Pack::open(packdir.path(), "fake@4+plain", 4).unwrap();

    let empty = tempfile::tempdir().unwrap();
    let store = br8n::store::Store::open(empty.path(), 4).unwrap();
    let r = br8n::retrieve::Retriever::new(
        store,
        Box::new(common::FakeEmbedder {
            calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            queries: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }),
        "http://127.0.0.1:1".into(),
    )
    .with_pack(Some(pack));

    let explain = r
        .search_explained("pooling", &br8n::config::Profile::tier(1), 0.0, 100_000)
        .unwrap();
    let bm25_stage = explain
        .stages
        .iter()
        .find(|s| s.name == "bm25")
        .expect("tier 1 must run a bm25 stage");
    assert!(
        !bm25_stage.hits.is_empty(),
        "fixture must produce at least one bm25 hit to make this assertion meaningful"
    );
    for h in &bm25_stage.hits {
        assert_eq!(
            h.relevance, 0.0,
            "pack bm25 hit {} carries relevance {} before the measure stage — \
             BM25 is unbounded and must never write relevance directly",
            h.chunk_id, h.relevance
        );
    }
}

/// A retriever with a pack and NO store must answer tier 1, and must fail
/// cleanly rather than panic at a tier that needs graph expansion.
#[test]
fn a_storeless_retriever_serves_tier_1_and_refuses_tier_2() {
    let (_d, idx) = seeded();
    let rows = idx.store().all_rows_for_pack().unwrap();
    let packdir = tempfile::tempdir().unwrap();
    br8n::pack::Pack::build(
        packdir.path(),
        "fake@4+plain",
        4,
        rows,
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    let pack = br8n::pack::Pack::open(packdir.path(), "fake@4+plain", 4).unwrap();

    let r = br8n::retrieve::Retriever::packed(
        pack,
        Box::new(common::FakeEmbedder::default()),
        "http://127.0.0.1:1".into(),
    );

    assert!(
        !r.search("pooling", &br8n::config::Profile::tier(1))
            .unwrap()
            .is_empty(),
        "tier 1 needs no store"
    );
    assert!(
        r.search("pooling", &br8n::config::Profile::tier(2))
            .is_err(),
        "tier 2 needs graph expansion and must say so, not panic or return empty"
    );
}

/// BM25 lives in the pack now. The store's `fts_search` is gone, and asking for
/// it must SAY so rather than return an empty list — an empty keyword result is
/// indistinguishable from an honest no-match, which is how this project has
/// shipped silent degradation before.
#[test]
fn the_store_says_bm25_moved_rather_than_returning_nothing() {
    let (_d, idx) = seeded();
    let err = idx
        .store()
        .fts_search("pooling", 5)
        .expect_err("fts_search must not silently return empty");
    assert!(
        err.to_string().contains("pack"),
        "the error must point at the pack, got: {err}"
    );
}

/// An embedder that always fails, so a test using it can prove the pipeline
/// never called it — a fake embedder that succeeds cannot demonstrate that,
/// since success is silent either way. `br8n index --no-embed` publishes
/// exactly the pack shape this test builds: postings and no vectors at all.
struct AlwaysFailsEmbedder;
impl br8n::embed::Embedder for AlwaysFailsEmbedder {
    fn embed_documents(&self, _texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!("embedder must never be called on a vectorless pack")
    }
    fn embed_query(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        anyhow::bail!("embedder must never be called on a vectorless pack")
    }
    fn warm(&self) -> anyhow::Result<()> {
        anyhow::bail!("embedder must never be called on a vectorless pack")
    }
    fn model_id(&self) -> String {
        "fake@4".into()
    }
    fn dimensions(&self) -> usize {
        4
    }
}

/// The gap this task exists to close: `br8n index --no-embed` publishes a
/// pack with postings and no vectors at all (`Pack::has_vectors` false), and
/// a BM25-only query against it must never need Ollama. Before the fix,
/// `Retriever::run` called `self.embedder.embed_query` unconditionally before
/// any stage, so this exact query failed with `EmbedUnavailable` even though
/// BM25 alone could answer it. `AlwaysFailsEmbedder` is the only construction
/// that proves the call never happens — a fake embedder that succeeds would
/// pass whether or not the bug were present.
#[test]
fn a_vectorless_pack_answers_bm25_with_no_embedder_available_at_all() {
    let (_d, idx) = seeded();
    let rows = idx.store().all_rows_for_pack().unwrap();
    // Phase 1 of an asynchronous index: same rows, every vector emptied —
    // exactly what `br8n index --no-embed` publishes. See
    // `build_with_every_vector_empty_publishes_no_vec_file` in tests/it/pack.rs
    // for the write-side guarantee this relies on.
    let vectorless_rows: Vec<(br8n::pack::records::Record, Vec<f32>)> = rows
        .into_iter()
        .map(|(rec, _v)| (rec, Vec::new()))
        .collect();
    let packdir = tempfile::tempdir().unwrap();
    br8n::pack::Pack::build(
        packdir.path(),
        "fake@4+plain",
        4,
        vectorless_rows,
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    let pack = br8n::pack::Pack::open(packdir.path(), "fake@4+plain", 4).unwrap();
    assert!(!pack.has_vectors(), "fixture must actually be vectorless");

    let r = br8n::retrieve::Retriever::packed(
        pack,
        Box::new(AlwaysFailsEmbedder),
        "http://127.0.0.1:1".into(),
    );

    let hits = r
        .search("pooling", &br8n::config::Profile::tier(1))
        .expect("a vectorless pack must answer from bm25 without ever calling the embedder");
    assert!(
        !hits.is_empty(),
        "expected a bm25 hit for \"pooling\" against the PgBouncer fixture doc"
    );
}

/// A corpus with real wikilinks, so inbound counts are DIFFERENT per document
/// rather than uniformly zero: two notes point at Beta and one points at
/// Gamma, so Beta=2, Gamma=1, and Alpha/Delta have none and are absent from
/// the map entirely (`inbound_link_counts` returns only documents that have
/// at least one).
///
/// `Document::new` never parses wikilinks out of `text` — only the markdown
/// loader does, over a real file — so `.links` is set by hand, exactly as
/// every other wikilink test in this crate does.
fn seeded_with_links() -> (tempfile::TempDir, br8n::index::Indexer) {
    let (d, idx, _calls) = common::setup();
    let mut alpha = Document::new(
        SourceType::Markdown,
        "file:///alpha.md",
        "Alpha",
        "# Alpha\n\nPgBouncer runs in transaction mode. See [[Beta]] and [[Gamma]].",
    );
    alpha.links = vec!["Beta".into(), "Gamma".into()];
    let mut delta = Document::new(
        SourceType::Markdown,
        "file:///delta.md",
        "Delta",
        "# Delta\n\nSourdough needs a long cold ferment. See [[Beta]].",
    );
    delta.links = vec!["Beta".into()];
    let beta = Document::new(
        SourceType::Markdown,
        "file:///beta.md",
        "Beta",
        "# Beta\n\nConnection pooling drops idle session state under load.",
    );
    let gamma = Document::new(
        SourceType::Markdown,
        "file:///gamma.md",
        "Gamma",
        "# Gamma\n\nThe starter needs feeding twice daily at room temperature.",
    );
    let docs = vec![alpha, delta, beta, gamma];
    idx.index_documents(&docs).unwrap();
    idx.resolve_links(&docs).unwrap();
    (d, idx)
}

/// The pack and the store must agree about how many documents link to a given
/// one, exactly as they must agree about relevance. Two backends now fill this
/// role — `Store::inbound_link_counts` for the store path and `pack.links` for
/// the storeless one — and `authority_lift` multiplies `relevance` itself, so
/// a disagreement moves both the injection gate and the final ordering with
/// nothing downstream able to notice.
///
/// Checked through the READ path (`Pack::search` hydration), not against the
/// map `Pack::build` was handed, so the assertion covers the whole
/// write-mmap-read round trip and the row-ordinal join inside it. The fixture
/// gives Beta 2, Gamma 1 and the other two 0, so a row that picked up its
/// neighbour's entry fails rather than coincidentally matching.
#[test]
fn pack_link_counts_match_the_store() {
    let (_d, idx) = seeded_with_links();
    let from_store = idx.store().inbound_link_counts().unwrap();
    assert_eq!(
        from_store.values().copied().max(),
        Some(2),
        "the fixture must actually have links, or this test proves nothing"
    );

    let dir = tempfile::tempdir().unwrap();
    let rows = idx.store().all_rows_for_pack().unwrap();
    br8n::pack::Pack::build(
        dir.path(),
        "fake@4+plain",
        4,
        rows,
        &from_store,
        &Default::default(),
    )
    .unwrap();
    let p = br8n::pack::Pack::open(dir.path(), "fake@4+plain", 4).unwrap();

    assert_eq!(
        p.max_inbound(),
        from_store.values().copied().max().unwrap_or(0),
        "the denominator authority_lift divides by must be the same number"
    );

    // Every row in the pack, reached the way the prompt path reaches them.
    let qv = br8n::embed::normalize(vec![1.0, 0.0, 0.0, 0.0]);
    let hits = p.search(&qv, p.rows(), 64).unwrap();
    assert_eq!(hits.len(), p.rows(), "the fixture must cover every row");
    for (rec, _) in &hits {
        let want = from_store.get(&rec.doc_id).copied().unwrap_or(0);
        assert_eq!(
            rec.inbound, want,
            "pack says {} inbound for {}, store says {want}",
            rec.inbound, rec.doc_id
        );
    }

    // And the fixture really does distinguish documents, so the loop above
    // could not have passed on all-zeros.
    let linked: Vec<u32> = hits.iter().map(|(r, _)| r.inbound).collect();
    assert!(
        linked.contains(&2) && linked.contains(&0),
        "the fixture must contain both linked and unlinked documents, got {linked:?}"
    );
}

/// The point of the whole change: authority weighting with NO store open.
///
/// The retriever here is `Retriever::packed`, which holds no `Store` at all,
/// so the lift can only have come from `pack.links`. Compared against the same
/// pack with `authority = 0.0`, which is the shipped default and must be
/// untouched by any of this.
#[test]
fn authority_lifts_a_packed_hit_with_no_store_open() {
    let (_d, idx) = seeded_with_links();
    let from_store = idx.store().inbound_link_counts().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let rows = idx.store().all_rows_for_pack().unwrap();
    br8n::pack::Pack::build(
        dir.path(),
        "fake@4+plain",
        4,
        rows,
        &from_store,
        &Default::default(),
    )
    .unwrap();

    let profile = br8n::config::Config::default().profile_for(br8n::config::Surface::Hook);

    let run = |authority: f32| -> Vec<br8n::store::Hit> {
        let pack = br8n::pack::Pack::open(dir.path(), "fake@4+plain", 4).unwrap();
        let weights = br8n::config::Weights {
            authority,
            ..br8n::config::Config::default().weights
        };
        let r = br8n::retrieve::Retriever::packed(
            pack,
            Box::new(common::FakeEmbedder {
                calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                queries: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }),
            "http://localhost:11434".into(),
        )
        .with_weights(weights);
        assert!(
            r.store().is_none(),
            "this test is meaningless if a store is open"
        );
        r.search("connection pooling drops idle session state", &profile)
            .unwrap()
    };

    let off = run(0.0);
    let on = run(0.5);
    assert!(!off.is_empty(), "the fixture must return something");
    assert_eq!(off.len(), on.len(), "authority changes weights, not recall");

    let beta_id = br8n::model::Document::new_id("file:///beta.md");
    let rel = |hits: &[br8n::store::Hit], doc: &str| -> f32 {
        hits.iter()
            .find(|h| h.doc_id == doc)
            .unwrap_or_else(|| panic!("{doc} must be among the hits"))
            .relevance
    };

    // Beta has the most inbound links in this corpus, so it is the one the
    // lift reaches its ceiling on: exactly `1.0 + authority`.
    let lifted = rel(&on, &beta_id) / rel(&off, &beta_id);
    assert!(
        (lifted - 1.5).abs() < 1e-4,
        "the most-linked document must be lifted by exactly 1 + authority, got {lifted}"
    );

    // And an unlinked one must not move at all — being unlinked is not a
    // demotion. Alpha links out but nothing links to it.
    let alpha_id = br8n::model::Document::new_id("file:///alpha.md");
    assert_eq!(
        rel(&on, &alpha_id),
        rel(&off, &alpha_id),
        "authority must never demote a document that simply has no inbound links"
    );
}

/// The shipped posture: THREE multipliers are 1.0 and `superseded` is 0.88.
///
/// The feature is ON by default. That reverses the original design, which
/// shipped it inert, and the reversal is deliberate: measured on the live index
/// on 2026-09-05, the superseded ADR-0004 sits at 0.7420 against a 0.66 hook
/// gate, so it is injected into prompts as if current. 0.88 removes it.
///
/// The window is NARROW and measured: `0.880 < m < 0.889`. Below it the golden
/// case "we used to run two build tools" loses ADR-0004 from its top five;
/// above it the superseded record stays above the gate and keeps being
/// injected. 0.88 is one of very few values inside, and both bounds were
/// verified by running the binary eight to twelve times, not by arithmetic —
/// a third case that appeared to bound this turned out to return ADR-0004 in
/// only 8 of 12 identical runs at m = 1.0, so a single measurement of it was
/// worthless.
#[test]
fn superseded_ships_on_at_zero_point_88_and_the_rest_stay_inert() {
    let w = br8n::config::Config::default().weights;
    assert_eq!(w.lifecycle_weight(Lifecycle::Current), 1.0);
    assert_eq!(w.lifecycle_weight(Lifecycle::Investigating), 1.0);
    assert_eq!(
        w.lifecycle_weight(Lifecycle::Proposed),
        1.0,
        "PERMANENT, not a placeholder: most notes carry no `status:` and land \
         on Proposed, so anything below 1.0 here demotes the whole corpus"
    );
    assert_eq!(
        w.lifecycle_weight(Lifecycle::Superseded),
        0.88,
        "the ONLY multiplier that is not 1.0. Changing it is a retrieval change \
         for every user on upgrade: re-measure both bounds on the live index \
         before touching it, because the design's original window is already \
         closed and a number that looks reasonable can cost a golden case"
    );
}

/// Setting the dial moves exactly one position.
#[test]
fn setting_superseded_leaves_the_other_three_alone() {
    let mut w = br8n::config::Config::default().weights;
    w.superseded = 0.95;
    assert_eq!(w.lifecycle_weight(Lifecycle::Superseded), 0.95);
    assert_eq!(w.lifecycle_weight(Lifecycle::Current), 1.0);
    assert_eq!(w.lifecycle_weight(Lifecycle::Proposed), 1.0);
    assert_eq!(w.lifecycle_weight(Lifecycle::Investigating), 1.0);
}
