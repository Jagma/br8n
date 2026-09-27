use crate::store::Hit;
use std::collections::HashMap;

/// Reciprocal Rank Fusion. Rank-based, so it merges lists whose scores are on
/// completely different scales (cosine similarity vs unbounded BM25) without
/// any normalization step to tune.
pub fn rrf(lists: Vec<Vec<Hit>>, k: f32) -> Vec<Hit> {
    rrf_weighted(lists.into_iter().map(|l| (1.0, l)).collect(), k)
}

/// Reciprocal rank fusion with a weight per input list.
///
/// Equal weights let a document win by APPEARING in many lists rather than by
/// ranking well in any of them, and graph expansion makes that failure routine:
/// it reaches whatever is well-connected, so a hub note (measured on one vault:
/// 8 inbound wikilinks) is pulled in for almost every query, lands in both the
/// graph and keyword lists, and outscores the document that actually answers
/// the question but appears only in the vector list. Measured: a question
/// answered by a decision record returned it at tier 1, and an unrelated hub
/// note once graph expansion was enabled at tier 2.
///
/// Graph expansion is a recall mechanism. It should be able to surface a
/// document the other retrievers missed, not to promote one they both ranked
/// below something else.
pub fn rrf_weighted(lists: Vec<(f32, Vec<Hit>)>, k: f32) -> Vec<Hit> {
    let mut scores: HashMap<String, f32> = HashMap::new();
    let mut best: HashMap<String, Hit> = HashMap::new();

    for (weight, list) in lists {
        for (rank, hit) in list.into_iter().enumerate() {
            *scores.entry(hit.chunk_id.clone()).or_insert(0.0) += weight / (k + rank as f32 + 1.0);
            // Keep the strongest similarity any signal offered for this chunk. A
            // chunk found by both vector and BM25 must not lose its cosine just
            // because the BM25 copy (relevance 0.0) was seen first.
            match best.entry(hit.chunk_id.clone()) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    if hit.relevance > e.get().relevance {
                        e.get_mut().relevance = hit.relevance;
                    }
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(hit);
                }
            }
        }
    }

    let mut out: Vec<Hit> = best
        .into_iter()
        .map(|(id, mut h)| {
            h.score = scores[&id];
            h
        })
        .collect();
    // Tie-break on chunk_id. RRF produces ties constantly — any two chunks
    // appearing in exactly one list at the same rank score identically — and
    // `best` is drained from a randomly-seeded HashMap, so a score-only sort
    // inherits that random order. Measured before this fix: 200 identical
    // fusions produced 200 DIFFERENT orderings. That makes the hook inject
    // different context for the same prompt, and makes `br8n bench` measure
    // noise rather than retrieval quality.
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.chunk_id.cmp(&b.chunk_id))
    });
    out
}

/// Maximal marginal relevance: trade result quality against variety.
///
/// The relevance term comes from each hit's RANK in the incoming list, not from
/// its score value. Both scales that reach here are ordinal, and one of them
/// collapses: RRF spans roughly 0.013-0.049, and after reranking every
/// surviving hit scores exactly 1.0. Normalising the VALUE therefore mapped
/// every reranked hit to the same number, which left the penalty as the only
/// term that varied — so MMR stopped ranking and simply returned the most
/// mutually-dissimilar set it could find. Measured on a real corpus: a query
/// whose answer sat at relevance 0.716 was ranked below a 0.660 boilerplate
/// chunk, because the boilerplate happened to be less similar to its
/// neighbours.
///
/// Rank never degenerates: the input is already sorted, positions are always
/// distinct, and lambda genuinely trades rank against variety at every tier.
pub fn mmr(hits: Vec<Hit>, lambda: f32, take: usize) -> Vec<Hit> {
    let n = hits.len();
    // Position 0 -> 1.0, last -> 0.0. A single hit has no ranking to express.
    let mut norm: Vec<f32> = (0..n)
        .map(|i| {
            if n <= 1 {
                1.0
            } else {
                1.0 - (i as f32 / (n - 1) as f32)
            }
        })
        .collect();

    let mut remaining = hits;
    let mut selected: Vec<Hit> = Vec::new();

    while selected.len() < take && !remaining.is_empty() {
        let mut best_idx = 0;
        let mut best_val = f32::NEG_INFINITY;

        for (i, cand) in remaining.iter().enumerate() {
            let max_sim = selected
                .iter()
                .map(|s| similarity(cand, s))
                .fold(0.0f32, f32::max);
            let val = lambda * norm[i] - (1.0 - lambda) * max_sim;
            if val > best_val {
                best_val = val;
                best_idx = i;
            }
        }
        norm.remove(best_idx);
        selected.push(remaining.remove(best_idx));
    }
    selected
}

/// Cheap proxy: same document is a strong redundancy signal, plus token overlap.
/// Avoids keeping embeddings in memory just to diversify.
///
/// The two signals are capped asymmetrically (0.6 flat same-document penalty
/// vs. a 0.4 cap on cross-document token overlap), so near-identical text in
/// two *different* documents is penalised less than merely-overlapping text
/// within the *same* document. That is fine for the cheap-proxy design intent,
/// but it means the "one verbose note cannot occupy the whole budget"
/// guarantee `mmr` documents above only holds *within* a document — two
/// documents that happen to duplicate the same passage are not caught by this
/// proxy the same way.
fn similarity(a: &Hit, b: &Hit) -> f32 {
    let doc_penalty = if a.doc_id == b.doc_id { 0.6 } else { 0.0 };
    let at: std::collections::HashSet<&str> = a.text.split_whitespace().collect();
    let bt: std::collections::HashSet<&str> = b.text.split_whitespace().collect();
    let overlap = if at.is_empty() || bt.is_empty() {
        0.0
    } else {
        at.intersection(&bt).count() as f32 / at.union(&bt).count() as f32
    };
    (doc_penalty + overlap * 0.4).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Hit;

    fn hit(id: &str, doc: &str, text: &str) -> Hit {
        Hit {
            chunk_id: id.into(),
            doc_id: doc.into(),
            text: text.into(),
            heading_path: String::new(),
            uri: format!("file:///{doc}.md"),
            title: doc.into(),
            page_no: None,
            score: 0.0,
            relevance: 0.0,
            source_type: "markdown".into(),
            inbound: 0,
            lifecycle: Default::default(),
            last_used: None,
            memory: None,
        }
    }

    #[test]
    fn rrf_ranks_an_item_appearing_in_both_lists_above_either_list_leader() {
        let a = vec![hit("1", "d1", "x"), hit("2", "d2", "y")];
        let b = vec![hit("3", "d3", "z"), hit("2", "d2", "y")];
        let fused = rrf(vec![a, b], 60.0);
        assert_eq!(
            fused[0].chunk_id, "2",
            "consensus beats a single strong rank"
        );
    }

    #[test]
    fn rrf_output_order_is_reproducible_when_scores_tie() {
        // Eight hits each appearing once at the same rank => all scores equal.
        // Without a tiebreaker this returned a different permutation every call.
        let run = || -> Vec<String> {
            let lists: Vec<Vec<Hit>> = (0..8)
                .map(|i| vec![hit(&i.to_string(), &format!("d{i}"), "x")])
                .collect();
            rrf(lists, 60.0).into_iter().map(|h| h.chunk_id).collect()
        };
        let first = run();
        for _ in 0..50 {
            assert_eq!(
                run(),
                first,
                "fused order must not vary between identical calls"
            );
        }
        // And it must be the documented order, not merely stable by accident.
        assert_eq!(first, vec!["0", "1", "2", "3", "4", "5", "6", "7"]);
    }

    #[test]
    fn rrf_deduplicates_by_chunk_id() {
        let a = vec![hit("1", "d1", "x")];
        let b = vec![hit("1", "d1", "x")];
        assert_eq!(rrf(vec![a, b], 60.0).len(), 1);
    }

    #[test]
    fn rrf_keeps_the_max_relevance_across_merged_duplicates_vector_first() {
        // Same chunk found by both vector (relevance 0.8) and BM25 (relevance
        // 0.0). Fusion must keep the cosine similarity regardless of which
        // list's copy of the hit happened to be inserted into `best` first.
        let mut vector_hit = hit("1", "d1", "x");
        vector_hit.relevance = 0.8;
        let fts_hit = hit("1", "d1", "x");
        assert_eq!(fts_hit.relevance, 0.0);

        let fused = rrf(vec![vec![vector_hit], vec![fts_hit]], 60.0);
        assert_eq!(fused.len(), 1);
        assert_eq!(
            fused[0].relevance, 0.8,
            "vector's cosine must survive the merge"
        );
    }

    #[test]
    fn rrf_keeps_the_max_relevance_across_merged_duplicates_fts_first() {
        let mut vector_hit = hit("1", "d1", "x");
        vector_hit.relevance = 0.8;
        let fts_hit = hit("1", "d1", "x");

        // Same pair, opposite list order: the BM25 copy (relevance 0.0) is
        // inserted into `best` first this time.
        let fused = rrf(vec![vec![fts_hit], vec![vector_hit]], 60.0);
        assert_eq!(fused.len(), 1);
        assert_eq!(
            fused[0].relevance, 0.8,
            "vector's cosine must survive the merge regardless of list order"
        );
    }

    #[test]
    fn rrf_on_empty_input_returns_empty() {
        assert!(rrf(vec![], 60.0).is_empty());
        assert!(rrf(vec![vec![], vec![]], 60.0).is_empty());
    }

    #[test]
    fn mmr_avoids_returning_many_chunks_from_one_document() {
        let hits = vec![
            hit("1", "same", "alpha"),
            hit("2", "same", "alpha"),
            hit("3", "same", "alpha"),
            hit("4", "other", "beta"),
        ];
        let out = mmr(hits, 0.5, 2);
        let docs: Vec<&str> = out.iter().map(|h| h.doc_id.as_str()).collect();
        assert!(
            docs.contains(&"other"),
            "diversity must surface the second document"
        );
    }

    #[test]
    fn mmr_with_lambda_one_preserves_pure_relevance_order() {
        let mut hits = vec![hit("1", "a", "x"), hit("2", "b", "y")];
        hits[0].score = 0.9;
        hits[1].score = 0.8;
        let out = mmr(hits, 1.0, 2);
        assert_eq!(out[0].chunk_id, "1");
        assert_eq!(out[1].chunk_id, "2");
    }

    #[test]
    fn mmr_take_larger_than_input_returns_everything() {
        assert_eq!(mmr(vec![hit("1", "a", "x")], 0.5, 10).len(), 1);
    }

    /// Three hits at genuine RRF scale: two from one document, one from another.
    fn rrf_scale_corpus() -> Vec<Hit> {
        let mut hits = vec![
            hit("a1", "docA", "alpha alpha"),
            hit("a2", "docA", "bravo bravo"),
            hit("b1", "docB", "charlie charlie"),
        ];
        hits[0].score = 0.049; // the arithmetic ceiling of RRF at k=60
        hits[1].score = 0.032;
        hits[2].score = 0.016;
        hits
    }

    #[test]
    fn mmr_lambda_actually_trades_relevance_against_variety() {
        // Lambda was a dead knob. `score` is an RRF rank value spanning roughly
        // 0.013-0.049, while `similarity` starts at 0.6 for two chunks of the
        // same document. At lambda 0.7 that is 0.034 of relevance against 0.18
        // of penalty, so the penalty won every time and MMR returned one chunk
        // per document at EVERY configured lambda — which also silently undid
        // graph expansion, whose whole purpose is fetching adjacent chunks.
        let high = mmr(rrf_scale_corpus(), 0.9, 2);
        let low = mmr(rrf_scale_corpus(), 0.2, 2);

        assert_eq!(
            high.iter().map(|h| h.chunk_id.as_str()).collect::<Vec<_>>(),
            vec!["a1", "a2"],
            "a high lambda must favour rank, keeping both strong chunks of one document"
        );
        assert_eq!(
            low.iter().map(|h| h.chunk_id.as_str()).collect::<Vec<_>>(),
            vec!["a1", "b1"],
            "a low lambda must favour variety, reaching for the second document"
        );
    }

    #[test]
    fn tied_scores_still_respect_the_incoming_order() {
        // The case that actually bites: after reranking, every surviving hit
        // scores exactly 1.0. Deriving the relevance term from that VALUE made
        // them indistinguishable, so the penalty became the only signal and MMR
        // returned the most mutually-dissimilar set instead of the best one —
        // measured on a real corpus, a 0.716 answer ranked below a 0.660
        // boilerplate chunk. Rank cannot tie, so the best hit stays first.
        let mut hits = vec![
            hit("best", "docA", "alpha alpha"),
            hit("mid", "docA", "alpha beta"),
            hit("far", "docB", "zulu zulu"),
        ];
        for h in hits.iter_mut() {
            h.score = 1.0; // what rerank produces
        }

        let out = mmr(hits, 0.7, 3);

        assert_eq!(
            out[0].chunk_id, "best",
            "the top-ranked hit must stay top even when every score ties"
        );
        assert!(
            out.iter().any(|h| h.doc_id == "docB"),
            "diversity must still operate below the first pick"
        );
    }
}
