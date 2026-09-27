use crate::common;
use br8n::config::Profile;

/// A token budget no fixture in this file can exhaust, for the tests that are
/// about the GATE rather than about the injection budget. `search_explained`
/// reports `injected` as what `hook::build_context` would really fit in the
/// prompt, so a small budget would truncate the set these tests reason about;
/// `injected_stops_at_the_token_budget_like_the_hook_does` covers the other
/// side deliberately.
const AMPLE_TOKENS: usize = 100_000;

/// Stage-gating tests must not race the deadline. They assert WHICH stages a
/// profile enables; `a_zero_budget_still_returns_results_and_reports_degradation`
/// separately asserts what happens when time runs out. Measured: tier 1's real
/// 130ms budget is close enough to the ~65-90ms warm floor that stage 1 alone can
/// exceed it under parallel test load, which made these tests fail about 1 run in 8.
fn gating_profile(tier: u8) -> Profile {
    let mut p = Profile::tier(tier);
    p.budget_ms = 60_000;
    p
}

/// Tier 0 runs BM25 as of 2026-08-31. It used to stop after `vector`, and this
/// test used to assert exactly that.
///
/// The flag was flipped on measurement, not taste. A/B on the live index (635
/// documents, 33,648 chunks), same binary, same 100-case golden set, the only
/// difference being `bm25` in the tier-0 arm: recall@5 0.42 -> 0.58 for a p50
/// of 46ms -> 53ms. Tiers 1-4 were identical to the digit in both runs
/// (0.81/0.83/0.85/0.88), which is what attributes the gain to this flag alone.
///
/// That also retired a claim in CLAUDE.md that tier 0 sat 0.10 below the store
/// "and structurally always will", because BM25 "cannot compensate where it is
/// not run". The reasoning was circular: the gap existed BECAUSE the flag was
/// off. At 0.58 tier 0 now beats the store's 0.52.
///
/// What still defines tier 0 is not "one signal" — it is that nothing here
/// opens a database or calls a model: no graph expansion, no reranker. That is
/// the invariant worth pinning, so it is what this asserts.
#[test]
fn tier_0_runs_bm25_but_never_opens_a_store_or_a_model() {
    let (_d, r) = common::retriever_with_corpus();
    let (_hits, report) = r.search_with_report("pooling", &gating_profile(0)).unwrap();
    assert!(
        !report.degraded,
        "a gating test must not be decided by the clock"
    );
    assert!(report.stages_run.contains(&"vector"));
    assert!(
        report.stages_run.contains(&"bm25"),
        "tier 0 runs BM25 as of 2026-08-31: +0.16 recall@5 for +7ms"
    );
    assert!(
        !report.stages_run.contains(&"graph"),
        "graph expansion needs the store, which tier 0 must never open"
    );
    assert!(
        !report.stages_run.contains(&"rerank"),
        "reranking is an LLM round trip, which tier 0 must never make"
    );
}

#[test]
fn tier_1_adds_bm25_and_fusion_but_no_graph_or_rerank() {
    let (_d, r) = common::retriever_with_corpus();
    let (_hits, report) = r.search_with_report("pooling", &gating_profile(1)).unwrap();
    assert!(
        !report.degraded,
        "a gating test must not be decided by the clock"
    );
    assert!(report.stages_run.contains(&"vector"));
    assert!(report.stages_run.contains(&"bm25"));
    assert!(report.stages_run.contains(&"fusion"));
    assert!(!report.stages_run.contains(&"graph"));
    assert!(!report.stages_run.contains(&"rerank"));
}

#[test]
fn tier_2_adds_graph_expansion() {
    let (_d, r) = common::retriever_with_corpus();
    let (_hits, report) = r.search_with_report("pooling", &gating_profile(2)).unwrap();
    assert!(
        !report.degraded,
        "a gating test must not be decided by the clock"
    );
    assert!(report.stages_run.contains(&"graph"));
}

#[test]
fn a_zero_budget_still_returns_results_and_reports_degradation() {
    let (_d, r) = common::retriever_with_corpus();
    let mut p = Profile::tier(4);
    p.budget_ms = 0;
    let (hits, report) = r.search_with_report("pooling", &p).unwrap();
    assert!(report.degraded, "blown budget must be reported, not hidden");
    assert!(
        !hits.is_empty(),
        "degradation must still return the vector results"
    );
}

/// Tier 1 asks for bm25 (see `tier_1_adds_bm25_and_fusion_but_no_graph_or_rerank`
/// above); a deadline that has already passed by the time that stage would
/// run must skip it rather than overrun the budget.
///
/// Despite its former name (`a_degraded_retrieval_is_reported_on_stderr`),
/// this test never touches `src/hook.rs` and never observes stderr — it only
/// asserts on the `StageReport` `search_with_report` returns in-process, so
/// deleting the `eprintln!` in `hook.rs`'s `run_prompt` would leave it green.
/// That `eprintln!` — the hook's only observable trace of a pipeline that
/// quietly stopped running BM25, since the hook exits 0 whatever happens and
/// a degraded run then looks identical to an honest no-match — is covered
/// separately, end to end against the real binary, by
/// `a_blown_budget_prints_the_degraded_line_on_stderr` in
/// `tests/it/hook_contract.rs`. Measured on the live index, tier 1 ran
/// vector-only for weeks because embed+vector exceeded its 220ms budget, and
/// the only symptom was worse results.
#[test]
fn a_zero_budget_at_tier_1_skips_bm25_when_the_deadline_trips() {
    let (_d, r) = common::retriever_with_corpus();
    let mut p = br8n::config::Profile::tier(1);
    p.budget_ms = 0; // force the deadline to trip immediately

    let (_hits, report) = r.search_with_report("pooling", &p).unwrap();
    assert!(report.degraded, "a zero budget must degrade");
    assert!(
        !report.stages_run.contains(&"bm25"),
        "tier 1 asked for bm25 and a zero budget must have skipped it"
    );
}

#[test]
fn tier_1_without_a_pack_refuses_rather_than_falling_back_to_the_store() {
    let (dir, idx, _calls) = common::setup();
    idx.index_documents(&[
        br8n::model::Document::new(
            br8n::model::SourceType::Markdown,
            "file:///a.md",
            "Pooling",
            "# Pooling\n\nPgBouncer runs in transaction mode and drops session state.",
        ),
        br8n::model::Document::new(
            br8n::model::SourceType::Markdown,
            "file:///b.md",
            "Baking",
            "# Baking\n\nSourdough needs a long cold ferment for flavour.",
        ),
    ])
    .unwrap();
    drop(idx); // release the write connection before reopening it read-only

    let store = br8n::store::Store::open(dir.path(), 4).unwrap();
    let r = br8n::retrieve::Retriever::new(
        store,
        Box::new(common::FakeEmbedder::default()),
        "http://127.0.0.1:1".into(),
    );
    // No `.with_pack(..)` — this is the packless path, which now refuses.

    let err = r
        .search_with_report("pooling", &gating_profile(1))
        .expect_err("a packless index must refuse, not fall back to a store vector search");
    assert!(
        err.downcast_ref::<br8n::retrieve::PackRefused>().is_some(),
        "the refusal must be tagged PackRefused, not a plain error, or the dashboard \
         routes it down the transient 503 path instead of showing the repair; got: {err}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("--compact"),
        "the refusal must tell the user how to repair it; got: {msg}"
    );
}

struct FailingEmbedder;
impl br8n::embed::Embedder for FailingEmbedder {
    fn embed_documents(&self, _texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!("embedder must not be called")
    }
    fn embed_query(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        anyhow::bail!("embedder must not be called")
    }
    fn warm(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn model_id(&self) -> String {
        "failing@4".into()
    }
    fn dimensions(&self) -> usize {
        4
    }
}

#[test]
fn a_packless_retriever_refuses_before_it_ever_calls_the_embedder() {
    let (dir, idx, _calls) = common::setup();
    idx.index_documents(&[br8n::model::Document::new(
        br8n::model::SourceType::Markdown,
        "file:///a.md",
        "Pooling",
        "# Pooling\n\nPgBouncer runs in transaction mode and drops session state.",
    )])
    .unwrap();
    drop(idx); // release the write connection before reopening it read-only

    let store = br8n::store::Store::open(dir.path(), 4).unwrap();
    let r = br8n::retrieve::Retriever::new(
        store,
        Box::new(FailingEmbedder),
        "http://127.0.0.1:1".into(),
    );

    let err = r
        .search_with_report("pooling", &gating_profile(1))
        .expect_err("a packless index must refuse");
    assert!(
        err.downcast_ref::<br8n::retrieve::PackRefused>().is_some(),
        "the packless refusal must win before an embed round trip is ever attempted \
         — an embedder that always fails must still surface PackRefused, not \
         EmbedUnavailable; got: {err}"
    );
}

#[test]
fn search_on_an_empty_index_returns_empty_not_an_error() {
    let (_d, r) = common::empty_retriever();
    assert!(r.search("anything", &Profile::tier(3)).unwrap().is_empty());
}

#[test]
fn results_are_capped_by_mmr_take() {
    let (_d, r) = common::retriever_with_corpus();
    assert!(r.search("pooling", &Profile::tier(1)).unwrap().len() <= 10);
}

#[test]
fn hits_no_vector_search_measured_still_get_a_real_similarity() {
    // BM25 matches on words and graph expansion on edges, so neither produces a
    // similarity of its own. Those hits carried `relevance: 0.0` — which does not
    // mean "dissimilar", it means "never measured" — and the hook gates on
    // relevance. An exact-keyword match was therefore structurally uninjectable
    // at the fast tier, where there is no reranker to restore it: the surface
    // that fires automatically could never use the retriever that finds exact
    // phrases.
    //
    // `candidates_k = 1` is what forces the case: vector search measures one
    // chunk, BM25 contributes a different one, and that second hit is the one
    // that used to arrive at the gate with nothing to show.
    let (_d, r) = common::retriever_with_corpus();
    let mut p = gating_profile(1);
    p.candidates_k = 1;

    let (hits, report) = r.search_with_report("pgbouncer", &p).unwrap();

    assert!(
        report.stages_run.contains(&"measure"),
        "this fixture must actually produce an unmeasured hit, or the test proves \
         nothing; stages were {:?}",
        report.stages_run
    );
    assert!(hits.len() > 1, "BM25 must have contributed a second chunk");
    for h in &hits {
        assert!(
            h.relevance > 0.0,
            "every hit must carry a measured similarity; {} had {}",
            h.chunk_id,
            h.relevance
        );
        assert!(
            h.relevance <= 1.0,
            "relevance must stay a [0,1] similarity, got {}",
            h.relevance
        );
    }
}

#[test]
fn the_gate_runs_before_diversity_selection() {
    // Gating after MMR let diversity spend its slots on hits the caller was
    // about to discard, so a gateable match just outside the diversity cut was
    // lost for a reason unrelated to how relevant it was.
    let (_d, r) = common::retriever_with_corpus();

    let all = r.search("pooling", &gating_profile(2)).unwrap();
    assert!(!all.is_empty(), "ungated search must return something");

    // A threshold above everything must return nothing at all...
    let none = r.search_gated("pooling", &gating_profile(2), 1.01).unwrap();
    assert!(none.is_empty(), "nothing can clear an impossible threshold");

    // ...and a threshold below everything must not change the result set.
    let same = r.search_gated("pooling", &gating_profile(2), 0.0).unwrap();
    assert_eq!(
        same.iter().map(|h| h.chunk_id.clone()).collect::<Vec<_>>(),
        all.iter().map(|h| h.chunk_id.clone()).collect::<Vec<_>>(),
        "a zero threshold must be identical to no gate"
    );
}

#[test]
fn search_explained_traces_stages_and_gates_like_the_hook() {
    // The dashboard's pipeline view IS this payload. The hook path passes no
    // collector and must stay byte-identical — the rest of this suite pins it.
    let (_d, r) = common::retriever_with_corpus();
    let ex = r
        .search_explained("pooling", &gating_profile(1), 0.70, AMPLE_TOKENS)
        .unwrap();

    let names: Vec<&str> = ex.stages.iter().map(|s| s.name).collect();
    assert!(names.contains(&"vector"), "stages were {names:?}");
    assert!(
        names.contains(&"bm25"),
        "tier 1 runs bm25; stages were {names:?}"
    );
    assert!(!ex.fused.is_empty());
    assert_eq!(ex.threshold, 0.70);

    // This fixture's fused hits sit around 0.98-0.99 relevance, well above the
    // 0.70 threshold, so a correct filter always injects something here. If
    // `injected` were empty, every assertion below would be vacuously true and
    // an over-filtering mutation (e.g. `relevance > 2.0`) would sail through
    // undetected — which is exactly the escape this test used to allow.
    assert!(
        !ex.injected.is_empty(),
        "this fixture's hits clear 0.70 comfortably; an empty `injected` here \
         means the filter is broken, not that nothing qualified"
    );

    // Check the SET, not just that each member clears the bar: every fused hit
    // at or above threshold must be injected, and nothing else may be. This
    // catches both an over-filtering mutation (raising the bar drops
    // qualifying hits out of `injected`) and an under-filtering one (lowering
    // it lets disqualified hits into `injected`).
    let expected: std::collections::BTreeSet<&str> = ex
        .fused
        .iter()
        .filter(|h| h.relevance >= ex.threshold)
        .map(|h| h.chunk_id.as_str())
        .collect();
    let actual: std::collections::BTreeSet<&str> =
        ex.injected.iter().map(|id| id.as_str()).collect();
    assert_eq!(
        actual, expected,
        "injected must be exactly the fused hits at or above threshold"
    );

    for id in &ex.injected {
        let h = ex
            .fused
            .iter()
            .find(|h| &h.chunk_id == id)
            .expect("injected ids come from fused");
        assert!(h.relevance >= 0.70, "{id} injected at {}", h.relevance);
    }
    for h in &ex.fused {
        // 1200, not 200: raised so the dashboard's "why this matched" panel
        // shows the text that actually caused a match instead of cutting a
        // keyword hit off before the word that mattered (see
        // `retrieve::EXCERPT_CHARS`). +1 allows for the appended ellipsis
        // when the source chunk is longer than the cap.
        assert!(
            h.excerpt.chars().count() <= 1201,
            "excerpts are capped: {}",
            h.excerpt.chars().count()
        );
    }

    // The set check above cannot by itself catch a filter that ignores the
    // `threshold` argument and instead always lets everything through (e.g.
    // hardcoding `relevance >= 0.0`): this fixture's two fused hits both sit
    // at ~0.98 relevance, so at threshold 0.70 an under-filtering bug injects
    // the exact same set a correct filter would. Re-running with a threshold
    // no fused hit can clear closes that gap: a correct filter injects
    // nothing, while a filter that ignores its argument keeps injecting both.
    let impossible = r
        .search_explained("pooling", &gating_profile(1), 1.01, AMPLE_TOKENS)
        .unwrap();
    assert!(
        impossible.injected.is_empty(),
        "an impossible threshold must inject nothing; got {:?}",
        impossible.injected
    );
}

/// Always answers with the same fixed vector, regardless of the input text.
/// This fixture controls MMR's inputs directly — chunk vectors, chunk text,
/// and doc_id, inserted straight through the store — so the query embedding
/// only needs to be fixed and known; nothing here depends on what text an
/// embedder would actually produce.
struct FixedEmbedder(Vec<f32>);
impl br8n::embed::Embedder for FixedEmbedder {
    fn embed_documents(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| self.0.clone()).collect())
    }
    fn embed_query(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        Ok(self.0.clone())
    }
    fn warm(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn model_id(&self) -> String {
        "fixed@4".into()
    }
    fn dimensions(&self) -> usize {
        4
    }
}

/// A corpus engineered to reproduce the exact defect `search_gated`'s doc
/// comment warns about: MMR spending diversity slots on sub-threshold chunks
/// and evicting above-threshold ones, when the gate runs on the whole pool
/// AFTER diversity selection instead of BEFORE it.
///
/// One document ("relevant") with 12 chunks, each near the query vector —
/// well above any realistic threshold — but mutually REDUNDANT: MMR's
/// same-document penalty (`fusion::similarity`) is a flat 0.6 regardless of
/// embedding, so twelve chunks of the same document compete with each other
/// for diversity slots. A second document ("distractor") with 3 chunks, each
/// far from the query — below threshold — but mutually DIVERSE from the
/// first document (0 same-document penalty against it). 15 candidates total,
/// more than `MAX_RESULTS` (10), so MMR must actually choose which to drop.
fn mmr_eviction_corpus() -> (
    tempfile::TempDir,
    br8n::retrieve::Retriever,
    br8n::config::Profile,
    String,
    String,
) {
    use br8n::embed::normalize;
    use br8n::model::{Chunk, Document, SourceType};
    use br8n::store::Store;

    let dir = tempfile::tempdir().unwrap();
    let qv = normalize(vec![1.0, 0.0, 0.0, 0.0]);
    let relevant = Document::new(SourceType::Markdown, "file:///relevant.md", "Relevant", "x");
    let distractor = Document::new(
        SourceType::Markdown,
        "file:///distractor.md",
        "Distractor",
        "x",
    );
    {
        let store = Store::open(dir.path(), 4).unwrap();
        store.upsert_document(&relevant).unwrap();
        store.upsert_document(&distractor).unwrap();

        let (mut chunks, mut vecs) = (Vec::new(), Vec::new());
        for i in 0..12i64 {
            chunks.push(Chunk {
                id: Chunk::id(&relevant.id, i),
                doc_id: relevant.id.clone(),
                ord: i,
                text: format!("relevant chunk unique-{i} body text"),
                embed_text: format!("relevant chunk unique-{i} body text"),
                heading_path: String::new(),
                page_no: None,
            });
            // Close to the query vector; a tiny per-chunk perturbation keeps
            // the sort order deterministic without moving any of them across
            // the (much larger) gap to the distractor group below.
            vecs.push(normalize(vec![1.0, 0.02 * (i as f32 + 1.0), 0.0, 0.0]));
        }
        store.insert_chunks(&relevant.id, &chunks, &vecs).unwrap();

        let (mut dchunks, mut dvecs) = (Vec::new(), Vec::new());
        for i in 0..3i64 {
            dchunks.push(Chunk {
                id: Chunk::id(&distractor.id, i),
                doc_id: distractor.id.clone(),
                ord: i,
                text: format!("distractor chunk unrelated-{i} body text"),
                embed_text: format!("distractor chunk unrelated-{i} body text"),
                heading_path: String::new(),
                page_no: None,
            });
            dvecs.push(normalize(vec![0.2, 1.0, 0.05 * (i as f32 + 1.0), 0.0]));
        }
        store
            .insert_chunks(&distractor.id, &dchunks, &dvecs)
            .unwrap();
    }

    let store = Store::open(dir.path(), 4).unwrap();
    let rows = store.all_rows_for_pack().unwrap();
    let pack_dir = dir.path().join("pack");
    std::fs::create_dir_all(&pack_dir).unwrap();
    br8n::pack::Pack::build(
        &pack_dir,
        "fixed@4",
        4,
        rows,
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    let pack = br8n::pack::Pack::open(&pack_dir, "fixed@4", 4).unwrap();
    let retriever =
        br8n::retrieve::Retriever::new(store, Box::new(FixedEmbedder(qv)), "unused".into())
            .with_pack(Some(pack));

    // No graph, no rerank: isolate the fusion+MMR interaction the finding is
    // about. `bm25: true` only to make `wants_more` true so the pipeline
    // actually reaches fusion and MMR instead of taking tier 0's vector-only
    // shortcut. The pack answers bm25 for a query neither fixture chunk's
    // text contains ("zzz-no-fts-match-zzz" below), so it contributes nothing
    // to `lists` — the pack's bm25 arm not erroring the way the deleted
    // store fallback did is exactly why this fixture needs the pack at all.
    let profile = br8n::config::Profile {
        name: "mmr-eviction-fixture",
        candidates_k: 50,
        efs: 100, // must be >= candidates_k; not otherwise load-bearing for this fixture
        bm25: true,
        graph: None,
        rerank: None,
        mmr_lambda: 0.7,
        budget_ms: 60_000,
    };

    (dir, retriever, profile, relevant.id, distractor.id)
}

#[test]
fn search_explained_injected_matches_search_gated_even_when_mmr_must_evict() {
    // This is the finding's mutation-checked assertion: for the same
    // query/profile/threshold, what the dashboard reports as `injected` must
    // be exactly what `search_gated` — what the hook actually injects —
    // returns. `search_explained_traces_stages_and_gates_like_the_hook`
    // above already asserts a version of this, but its two-document fixture
    // never gives MMR more than `MAX_RESULTS` candidates, so MMR never has
    // to evict anything and the pre-fix bug (gate applied only to the
    // ungated, post-MMR set) is vacuously invisible to it. This fixture has
    // 15 candidates for 10 slots, engineered so MMR's diversity term, if
    // allowed to run on the ungated pool, prefers a below-threshold
    // "distractor" chunk over an above-threshold "relevant" one.
    let (_d, r, profile, relevant_id, distractor_id) = mmr_eviction_corpus();
    let query = "zzz-no-fts-match-zzz";

    // Calibrate the threshold from the fixture itself rather than guessing a
    // magic number: `search_explained(.., 0.0)`'s gate is a no-op, so the new
    // "fused" stage it records — captured in `run` BEFORE the gate, see
    // `src/retrieve/mod.rs` — carries the full pre-gate, pre-eviction pool,
    // sorted by relevance. It must contain all 15 candidates, with the
    // "relevant" group entirely ahead of the "distractor" group.
    let calibration = r
        .search_explained(query, &profile, 0.0, AMPLE_TOKENS)
        .unwrap();
    let fused_stage = calibration
        .stages
        .iter()
        .find(|s| s.name == "fused")
        .expect("run() must record a pre-gate \"fused\" stage")
        .hits
        .clone();
    assert_eq!(
        fused_stage.len(),
        15,
        "fixture must supply 15 candidates spanning both groups; got {}",
        fused_stage.len()
    );
    let relevant_floor = fused_stage[11].relevance;
    let distractor_ceiling = fused_stage[12].relevance;
    assert!(
        relevant_floor > distractor_ceiling,
        "fixture's relevant group must outrank its distractor group entirely; \
         floor {relevant_floor} vs ceiling {distractor_ceiling}"
    );
    let threshold = (relevant_floor + distractor_ceiling) / 2.0;

    let gated = r.search_gated(query, &profile, threshold).unwrap();
    let explained = r
        .search_explained(query, &profile, threshold, AMPLE_TOKENS)
        .unwrap();

    let gated_ids: Vec<String> = gated.iter().map(|h| h.chunk_id.clone()).collect();
    assert_eq!(
        explained.injected, gated_ids,
        "search_explained's `injected` must be exactly what search_gated \
         returns — the payload the hook actually injects"
    );
    assert_eq!(
        explained
            .fused
            .iter()
            .map(|h| h.chunk_id.clone())
            .collect::<Vec<_>>(),
        gated_ids,
        "Explain.fused must also match the hook's payload; the pre-gate pool \
         belongs in the \"fused\" stage trace, not in this field"
    );

    // Prove the fixture actually forces an eviction, not just that the two
    // calls happen to agree: MMR had 12 above-threshold candidates for 10
    // slots, so it must fill every slot from the "relevant" document and
    // must not let a single below-threshold "distractor" chunk through.
    assert_eq!(
        gated_ids.len(),
        br8n::retrieve::MAX_RESULTS,
        "12 above-threshold candidates for 10 slots must fill every slot"
    );
    for h in &gated {
        assert_eq!(
            h.doc_id, relevant_id,
            "no below-threshold distractor chunk may reach the hook's payload"
        );
        // A separate claim from the doc_id above, at chunk granularity: the
        // payload's own numbers must clear the bar the caller asked for. (The
        // line this replaces asserted `doc_id != distractor_id`, which the
        // assertion above already implies and which therefore could never
        // fail on its own.)
        assert!(
            h.relevance >= threshold,
            "{} reached the payload at {}, below the {threshold} gate",
            h.chunk_id,
            h.relevance
        );
    }
    // ...and the distractor really was in the pool the gate ran on, so what
    // kept it out was the threshold rather than the fixture never offering it.
    assert!(
        fused_stage.iter().any(|h| h.doc_id == distractor_id),
        "the pre-gate pool must contain the distractor document"
    );
}

/// A corpus for the deadline paths: one TRANSCRIPT document whose chunks sit
/// almost exactly on the query vector (raw cosine ~1.0, so they clear any
/// realistic gate unweighted) and one MARKDOWN document whose chunks are far
/// from it but carry the words the BM25 query is built from.
///
/// The transcript weight is what the test turns on: at 0.45 those chunks are
/// worth 0.45 effective relevance, under the hook's default 0.70 gate, while
/// their raw cosine is over it.
fn deadline_corpus() -> (tempfile::TempDir, br8n::retrieve::Retriever, String) {
    use br8n::embed::normalize;
    use br8n::model::{Chunk, Document, SourceType};
    use br8n::store::Store;

    let dir = tempfile::tempdir().unwrap();
    let qv = normalize(vec![1.0, 0.0, 0.0, 0.0]);
    let transcript = Document::new(
        SourceType::Transcript,
        "file:///session.jsonl",
        "Session",
        "x",
    );
    let notes = Document::new(SourceType::Markdown, "file:///notes.md", "Notes", "x");
    {
        let store = Store::open(dir.path(), 4).unwrap();
        store.upsert_document(&transcript).unwrap();
        store.upsert_document(&notes).unwrap();

        let (mut tc, mut tv) = (Vec::new(), Vec::new());
        for i in 0..2i64 {
            tc.push(Chunk {
                id: Chunk::id(&transcript.id, i),
                doc_id: transcript.id.clone(),
                ord: i,
                text: format!("spoken aside number {i} with none of the keywords"),
                embed_text: format!("spoken aside number {i}"),
                heading_path: String::new(),
                page_no: None,
            });
            tv.push(normalize(vec![1.0, 0.001 * (i as f32 + 1.0), 0.0, 0.0]));
        }
        store.insert_chunks(&transcript.id, &tc, &tv).unwrap();

        let (mut nc, mut nv) = (Vec::new(), Vec::new());
        for i in 0..6i64 {
            nc.push(Chunk {
                id: Chunk::id(&notes.id, i),
                doc_id: notes.id.clone(),
                ord: i,
                text: format!("pooling pgbouncer transaction paragraph {i}"),
                embed_text: format!("pooling pgbouncer transaction paragraph {i}"),
                heading_path: String::new(),
                page_no: None,
            });
            nv.push(normalize(vec![0.05, 1.0, 0.02 * (i as f32 + 1.0), 0.0]));
        }
        store.insert_chunks(&notes.id, &nc, &nv).unwrap();
    }

    let store = Store::open(dir.path(), 4).unwrap();
    // BM25 moved to the pack in schema version 3 (`Store::fts_search` always
    // errors now), so the deadline this test needs to force has to come from
    // `Pack::bm25` instead — built here from the same rows, the way a real
    // index always publishes one alongside the store.
    let rows = store.all_rows_for_pack().unwrap();
    let pack_dir = dir.path().join("pack");
    std::fs::create_dir_all(&pack_dir).unwrap();
    br8n::pack::Pack::build(
        &pack_dir,
        "fixed@4",
        4,
        rows,
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    let pack = br8n::pack::Pack::open(&pack_dir, "fixed@4", 4).unwrap();
    let weights = br8n::config::Weights {
        markdown: 1.0,
        pdf: 1.0,
        web: 1.0,
        transcript: 0.45,
        memory: 1.0,
        authority: 0.0,
        current: 1.0,
        investigating: 1.0,
        proposed: 1.0,
        superseded: 1.0,
        decay: Default::default(),
    };
    let retriever = br8n::retrieve::Retriever::new(store, Box::new(FixedEmbedder(qv)), "x".into())
        .with_weights(weights)
        .with_pack(Some(pack));
    (dir, retriever, transcript.id)
}

#[test]
fn the_pre_fusion_deadline_return_gates_and_traces_weighted_relevance() {
    // The OTHER early return: vector and BM25 ran, the budget is gone before
    // fusion, and `run` bails out with the vector hits. Nothing reached this
    // path with a collector before — `tier_0_still_records_the_pre_gate_fused_
    // stage` covers the tier-0 shortcut only — so deleting this path's
    // "fused" push left the suite green, and so did returning raw, unweighted
    // cosines from it. Both halves are pinned here.
    //
    // Forcing the path without a clock to control: the deadline is checked
    // after vector search (passes) and again after BM25 (must trip), so the
    // BM25 call has to dominate the budget. BM25 moved to the pack in schema
    // version 3 (`Store::fts_search` always errors now), so `deadline_corpus`
    // attaches a `Pack` and it is `Pack::bm25` — `postings::Reader::search`,
    // which re-tokenizes the query and does one hashmap accumulation per
    // term occurrence — whose cost this test leans on instead. It scales the
    // same way `fts_search` used to, linearly with the number of query TERMS:
    // measured on this machine, 4k terms 47.6ms, 20k 220.7ms, 60k 659.3ms,
    // 180k 2.01s, while vector search over this eight-chunk corpus is ~3ms.
    // 60k terms against a 100ms budget: the first check has ~30x of headroom
    // and the second overshoots by ~6.6x. The path is asserted below, not
    // assumed, so a machine that defeats those margins fails loudly instead
    // of passing vacuously.
    let (_d, r, transcript_id) = deadline_corpus();
    let query = vec!["pooling pgbouncer transaction"; 60_000].join(" ");
    let profile = br8n::config::Profile {
        name: "pre-fusion-deadline-fixture",
        candidates_k: 2,
        efs: 32, // must be >= candidates_k; not otherwise load-bearing for this fixture
        bm25: true,
        graph: None,
        rerank: None,
        mmr_lambda: 0.7,
        budget_ms: 100,
    };
    // The hook's own default gate. The transcript chunks measure ~1.0 raw and
    // ~0.45 weighted, so it sits between the two.
    let threshold = 0.70;

    let ex = r
        .search_explained(&query, &profile, threshold, AMPLE_TOKENS)
        .unwrap();

    let hits_of = |name: &str| -> Vec<br8n::retrieve::HitSummary> {
        ex.stages
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.hits.clone())
            .unwrap_or_default()
    };
    let ids = |hs: &[br8n::retrieve::HitSummary]| -> std::collections::BTreeSet<String> {
        hs.iter().map(|h| h.chunk_id.clone()).collect()
    };

    let vector = hits_of("vector");
    let bm25 = hits_of("bm25");
    let fused = hits_of("fused");

    // 1. This really is the pre-fusion deadline return, not one of the other
    //    two paths. BM25 ran (so not the tier-0 shortcut), the budget blew,
    //    and the pool is still exactly the vector hits — fusion would have
    //    folded BM25's chunks in, and every one of them is a chunk vector
    //    search did not return.
    assert!(ex.degraded, "the budget must actually have blown");
    assert!(!bm25.is_empty(), "BM25 must have run before the deadline");
    assert!(
        ids(&bm25).is_disjoint(&ids(&vector)),
        "fixture must make BM25 contribute chunks vector search did not, or \
         step 2 cannot tell the two paths apart"
    );
    assert_eq!(
        ids(&fused),
        ids(&vector),
        "this must be the pre-fusion return: a pool containing BM25's chunks \
         means fusion ran and the test is measuring the wrong path"
    );

    // 2. The pre-gate pool is recorded at all (half of the fix).
    assert!(
        !fused.is_empty(),
        "the pre-fusion deadline return must record a non-empty \"fused\" stage"
    );

    // 3. And it is WEIGHTED, not raw. The "vector" stage is traced before
    //    weighting, so the same chunk must appear in "fused" at 0.45x of it.
    for v in &vector {
        let f = fused
            .iter()
            .find(|f| f.chunk_id == v.chunk_id)
            .expect("same pool, same ids");
        assert_eq!(v.doc_id, transcript_id, "vector top-k is the transcript");
        let want = v.relevance * 0.45;
        assert!(
            (f.relevance - want).abs() < 1e-5,
            "{} is a transcript chunk: {} raw must be traced as {want} weighted, \
             got {}",
            f.chunk_id,
            v.relevance,
            f.relevance
        );
    }
    assert!(
        fused.windows(2).all(|w| w[0].relevance >= w[1].relevance),
        "the pool must be ordered by the relevance it will be gated on"
    );

    // 4. The consequence the user would have seen: unweighted, these
    //    transcript chunks measure ~1.0 and sail through the hook's 0.70
    //    gate; weighted they are worth 0.45 and every other path cuts them.
    //    This path must cut them too.
    assert!(
        vector.iter().all(|v| v.relevance > threshold),
        "fixture must put the transcript ABOVE the gate unweighted, or this \
         assertion proves nothing; got {:?}",
        vector.iter().map(|v| v.relevance).collect::<Vec<_>>()
    );
    assert!(
        ex.injected.is_empty(),
        "a 0.45-weighted transcript must not clear a 0.70 gate; injected {:?}",
        ex.injected
    );
    assert!(
        ex.fused.is_empty(),
        "nothing cleared the gate, so the returned payload must be empty"
    );
}

#[test]
fn injected_stops_at_the_token_budget_like_the_hook_does() {
    // `injected` claims to be "what the hook actually injects". The hook's
    // `build_context` stops once `max_tokens` is spent, so a query where ten
    // chunks clear the gate really injects three or four — and the dashboard
    // used to draw ten injected cards for it. Both sides now count with
    // `hook::fit_to_budget`, and this test is what holds them together.
    let (_d, r, _) = deadline_corpus();
    let profile = gating_profile(0);
    let gated = r.search_gated("anything", &profile, 0.0).unwrap();
    assert!(
        gated.len() >= 3,
        "fixture must gate in several chunks, got {}",
        gated.len()
    );

    // Small enough to admit some but not all of them.
    let max_tokens = 30;
    let ex = r
        .search_explained("anything", &profile, 0.0, max_tokens)
        .unwrap();
    assert_eq!(
        ex.fused.len(),
        gated.len(),
        "everything that cleared the gate stays visible in `fused`"
    );
    assert!(
        !ex.injected.is_empty() && ex.injected.len() < ex.fused.len(),
        "the budget must admit some but not all of {} gated chunks; admitted {}",
        ex.fused.len(),
        ex.injected.len()
    );

    // The claim in full: the injected set is exactly the chunks whose text
    // the hook's own prompt block contains.
    let ctx = br8n::hook::build_context(&gated, max_tokens).unwrap();
    for h in &ex.fused {
        let injected = ex.injected.contains(&h.chunk_id);
        let in_prompt = ctx.contains(h.excerpt.trim());
        assert_eq!(
            injected,
            in_prompt,
            "{} is {} `injected` but {} the prompt the hook would build",
            h.chunk_id,
            if injected { "in" } else { "not in" },
            if in_prompt { "is in" } else { "is not in" }
        );
    }
}

#[test]
fn tier_0_still_records_the_pre_gate_fused_stage() {
    // Tier 0 takes the `!wants_more` shortcut in `run` — it never reaches the
    // main fusion path where the "fused" trace is normally recorded. That
    // path now gates with the real `min_relevance` (the whole point of the
    // fix that introduced the trace), so without its own push the Search tab
    // drew a gate divider with nothing below it on this path: the "what
    // nearly made it" view the trace exists to preserve vanished exactly on
    // the tier where a plain vector search is most likely to run.
    //
    // This fixture's two chunks measure 0.9845536 and 0.9832326 against
    // "pooling" (see the debug run this threshold was calibrated from); 0.984
    // sits strictly between them so one clears the gate and one does not.
    let (_d, r) = common::retriever_with_corpus();
    let threshold = 0.984;
    let ex = r
        .search_explained("pooling", &gating_profile(0), threshold, AMPLE_TOKENS)
        .unwrap();

    let fused_stage = ex
        .stages
        .iter()
        .find(|s| s.name == "fused")
        .expect("tier 0 must record a pre-gate \"fused\" stage");
    assert!(
        !fused_stage.hits.is_empty(),
        "the pre-gate stage must not be empty"
    );
    assert!(
        fused_stage.hits.iter().any(|h| h.relevance < threshold),
        "the pre-gate stage must still show hits the gate cut; got {:?}",
        fused_stage
            .hits
            .iter()
            .map(|h| h.relevance)
            .collect::<Vec<_>>()
    );
    // And the gated result itself must actually have dropped that hit.
    assert!(
        ex.injected.len() < fused_stage.hits.len(),
        "the threshold must have excluded at least one candidate"
    );
}

/// `efs` is the HNSW search-effort knob. Measured on the live index, cost is
/// flat in `k` and driven almost entirely by this number: uncontended, efs=200
/// (lbug's default, which is what an unset parameter gets) cost ~56ms against
/// ~21ms at efs=50, for 99.3% vs 97.1% top-20 agreement with efs=800.
///
/// End to end at tier 1, that is worth a mean 40.1ms against a 464.5ms mean
/// baseline (8.6%), over 10 interleaved paired runs of two binaries identical
/// but for this parameter, faster in 9 of 10 pairs. Quality is unchanged, and
/// not merely in count: for the same query both binaries returned the same 10
/// URIs in the same order with relevance equal to 6 decimal places, across 8
/// distinct documents. Modest, because most of a query is process start,
/// `Database::new` and the embed; those are what the retrieval pack removes.
///
/// These values are therefore a deliberate quality/latency choice per tier,
/// not an arbitrary constant, and a change to them must be a change to this
/// test.
#[test]
fn each_tier_sets_a_deliberate_search_effort() {
    assert_eq!(
        Profile::tier(0).efs,
        32,
        "instant trades agreement for latency"
    );
    assert_eq!(
        Profile::tier(1).efs,
        50,
        "fast: 97.1% agreement at a third of the cost"
    );
    assert_eq!(Profile::tier(2).efs, 64);
    assert_eq!(Profile::tier(3).efs, 100);
    assert_eq!(
        Profile::tier(4).efs,
        200,
        "exhaustive keeps the old default"
    );
}

/// `efs` must be at least `k` or the index cannot return the requested number
/// of neighbours.
#[test]
fn search_effort_is_never_below_the_candidate_count() {
    for tier in 0..=4u8 {
        let p = Profile::tier(tier);
        assert!(
            p.efs >= p.candidates_k,
            "tier {} asks for {} candidates with only {} search effort",
            tier,
            p.candidates_k,
            p.efs
        );
    }
}

/// Fusion must behave when the vector list is EMPTY, which is what a partially
/// embedded index produces. RRF scores by RANK, so an absent list is not the
/// same as a short one, and the gate reads `relevance`, which a chunk with no
/// vector does not have.
///
/// The spec calls this out by name: during backfill RRF sees fewer vector
/// candidates than usual, and the fused ordering must be VERIFIED, not assumed
/// to degrade gracefully.
#[test]
fn fusion_is_sane_when_no_chunk_has_a_vector() {
    // Same two-document corpus and fixture pattern as
    // `tier_1_without_a_pack_refuses_rather_than_falling_back_to_the_store`
    // above: `common::setup()` gives a store-backed `Indexer` with a
    // network-free `FakeEmbedder`, indexed here, then read back through
    // `all_rows_for_pack` to build a pack exactly as `reindex_swap` would.
    let (_d, idx, _calls) = common::setup();
    idx.index_documents(&[
        br8n::model::Document::new(
            br8n::model::SourceType::Markdown,
            "file:///a.md",
            "Pooling",
            "# Pooling\n\nPgBouncer runs in transaction mode and drops session state.",
        ),
        br8n::model::Document::new(
            br8n::model::SourceType::Markdown,
            "file:///b.md",
            "Baking",
            "# Baking\n\nSourdough needs a long cold ferment for flavour.",
        ),
    ])
    .unwrap();
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

    // Strip the vectors, exactly as phase 1 publishes.
    std::fs::remove_file(dir.path().join(br8n::pack::vectors::VEC_FILE)).unwrap();
    let mut m = br8n::pack::manifest::Manifest::read(dir.path()).unwrap();
    m.rows_with_vectors = 0;
    m.write(dir.path()).unwrap();
    let pack = br8n::pack::Pack::open(dir.path(), "fake@4", 4).unwrap();
    assert!(
        !pack.has_vectors(),
        "test setup must actually strip vectors"
    );

    // A fresh, empty store: tier 1 with a pack attached must answer entirely
    // from the pack, never touching this store.
    let store_dir = tempfile::tempdir().unwrap();
    let store = br8n::store::Store::open(store_dir.path(), 4).unwrap();
    let r = br8n::retrieve::Retriever::new(
        store,
        Box::new(common::FakeEmbedder::default()),
        "http://127.0.0.1:1".into(),
    )
    .with_pack(Some(pack));

    let p = gating_profile(1);
    let (hits, report) = r.search_with_report("pooling", &p).unwrap();

    assert!(
        !report.stages_run.contains(&"vector") || hits.iter().all(|h| h.relevance == 0.0),
        "a vectorless pack must not produce a measured relevance"
    );
    assert!(
        report.vectors_unavailable.is_some(),
        "a vectorless pack is a live, shipped state during --no-embed / --backfill — the \
         report must name it, or a caller (the hook, `br8n search`) cannot tell it apart \
         from an honest no-match"
    );
    assert!(
        !hits.is_empty(),
        "BM25 must still answer — that is the point of publishing before embedding"
    );
    for h in &hits {
        assert_eq!(
            h.relevance, 0.0,
            "chunk {} has no vector, so it can have no similarity — a fabricated \
             value here would clear the injection gate on nothing",
            h.chunk_id
        );
    }

    // And therefore nothing clears a real gate.
    let gated = r.search_gated("pooling", &p, 0.70).unwrap();
    assert!(
        gated.is_empty(),
        "nothing has a measured similarity, so nothing may clear 0.70"
    );
}

#[test]
fn a_graph_expanded_transcript_chunk_decays_with_its_document() {
    use br8n::embed::normalize;
    use br8n::model::{Chunk, Document, SourceType};
    use br8n::store::Store;

    let dir = tempfile::tempdir().unwrap();
    let transcript = Document::new(
        SourceType::Transcript,
        "file:///session.jsonl",
        "Session",
        "x",
    );
    let qv = normalize(vec![1.0, 0.0, 0.0, 0.0]);
    {
        let store = Store::open(dir.path(), 4).unwrap();
        store.upsert_document(&transcript).unwrap();
        let chunks: Vec<Chunk> = (0..2i64)
            .map(|i| Chunk {
                id: Chunk::id(&transcript.id, i),
                doc_id: transcript.id.clone(),
                ord: i,
                text: format!("spoken aside number {i}"),
                embed_text: format!("spoken aside number {i}"),
                heading_path: String::new(),
                page_no: None,
            })
            .collect();
        let vecs = vec![qv.clone(), normalize(vec![0.0, 1.0, 0.0, 0.0])];
        store.insert_chunks(&transcript.id, &chunks, &vecs).unwrap();
    }

    let store = Store::open(dir.path(), 4).unwrap();
    let pack_dir = dir.path().join("pack");
    std::fs::create_dir_all(&pack_dir).unwrap();
    let two_years_ago = br8n::memory::now_secs() - 730 * 86_400;
    let usage: br8n::usage::Map = [(
        transcript.id.clone(),
        br8n::usage::Usage {
            first_seen: two_years_ago,
            last_used: Some(two_years_ago),
        },
    )]
    .into_iter()
    .collect();
    br8n::pack::Pack::build_with_usage(
        &pack_dir,
        "fixed@4",
        4,
        store.all_rows_for_pack().unwrap(),
        &Default::default(),
        &Default::default(),
        &usage,
    )
    .unwrap();
    let pack = br8n::pack::Pack::open(&pack_dir, "fixed@4", 4).unwrap();

    let mut weights = br8n::config::Config::default().weights;
    weights.decay.enabled = true;
    let expected_weight = weights.for_source("transcript") * weights.decay.floor;
    let r = br8n::retrieve::Retriever::new(store, Box::new(FixedEmbedder(qv)), "x".into())
        .with_weights(weights)
        .with_pack(Some(pack));

    let profile = Profile {
        name: "graph-decay-fixture",
        candidates_k: 1,
        efs: 32,
        bm25: false,
        graph: Some(br8n::config::GraphExpansion {
            hops: 1,
            max_neighbors: 5,
        }),
        rerank: None,
        mmr_lambda: 0.7,
        budget_ms: 60_000,
    };
    let ex = r
        .search_explained("spoken aside", &profile, 0.0, AMPLE_TOKENS)
        .unwrap();
    let stage = |name: &str| {
        ex.stages
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.hits.clone())
            .unwrap_or_default()
    };
    let neighbour = Chunk::id(&transcript.id, 1);
    assert!(
        !stage("vector").iter().any(|h| h.chunk_id == neighbour),
        "fixture precondition: the neighbour must not be a vector hit"
    );
    assert!(
        stage("graph").iter().any(|h| h.chunk_id == neighbour),
        "fixture precondition: the neighbour arrives through graph expansion"
    );
    let fused = stage("fused");
    let got = fused
        .iter()
        .find(|h| h.chunk_id == neighbour)
        .expect("the expanded neighbour reaches the weighted pool")
        .relevance;
    assert!(
        (got - 0.5 * expected_weight).abs() < 1e-4,
        "an orthogonal neighbour measures 0.5, and its document has sat unused for two \
         years, so it must weigh 0.5 x transcript x floor = {}; got {got}",
        0.5 * expected_weight
    );
}
