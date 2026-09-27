use br8n::pack::manifest::{Manifest, FORMAT};
use br8n::pack::status::{self, Lifecycle};

fn m() -> Manifest {
    Manifest {
        format: FORMAT,
        model_id: "qwen3-embedding:0.6b@512+qwen3".into(),
        dims: 512,
        rows: 27380,
        rows_with_vectors: 27380,
        vec_sha: "a".repeat(64),
        rec_sha: "b".repeat(64),
        analyzer: br8n::pack::analyze::ANALYZER.to_string(),
    }
}

#[test]
fn manifest_round_trips_through_a_file() {
    let d = tempfile::tempdir().unwrap();
    m().write(d.path()).unwrap();
    assert_eq!(Manifest::read(d.path()).unwrap(), m());
}

/// A pack built by another embedding model is not a pack this binary may read.
/// `model_id` encodes model, dimensions and prefix scheme, so a mismatch means
/// the vectors live in a different space — reading them would return confident
/// nonsense rather than an error.
#[test]
fn a_model_mismatch_is_refused_not_tolerated() {
    let err = m().validate("nomic-embed-text@768+nomic", 512).unwrap_err();
    assert!(
        err.to_string().contains("built with embedding model"),
        "the error must name the mismatch, got: {err}"
    );
}

#[test]
fn a_dimension_mismatch_is_refused() {
    let err = m()
        .validate("qwen3-embedding:0.6b@512+qwen3", 256)
        .unwrap_err();
    assert!(err.to_string().contains("dimensions"), "got: {err}");
}

/// A format bump means the reader cannot know the file's layout. Refusing is
/// the only safe answer; guessing is how a reader gets rows that are not the
/// rows it asked for.
#[test]
fn a_future_format_version_is_refused() {
    let mut future = m();
    future.format = FORMAT + 1;
    let err = future
        .validate("qwen3-embedding:0.6b@512+qwen3", 512)
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("format"), "got: {msg}");

    // The remedy has to be the CHEAP one. A format bump changes only how the
    // pack is written — the database's rows and their embeddings are untouched
    // — so `--compact` rebuilds it with no Ollama traffic, while `--reindex`
    // re-embeds the whole corpus. On the live index that was ~10 minutes
    // against ~86. This assertion exists because the message named `--reindex`
    // only, and the first person to hit it in anger followed it.
    assert!(
        msg.contains("--compact"),
        "a format mismatch must point at `br8n index --compact` first, not \
         send the user to re-embed their whole corpus: {msg}"
    );
}

#[test]
fn a_missing_manifest_is_an_error_not_a_default() {
    let d = tempfile::tempdir().unwrap();
    assert!(Manifest::read(d.path()).is_err());
}

use br8n::pack::records::{self, Record};

/// Eight unrelated one-line subjects, cycled by `i % 8`. The old template —
/// `"body of record {i}\nwith a newline"` — used only the decimal digit `i`
/// to distinguish rows, but the pack's analyzer used to delete digit
/// characters entirely (`analyze::STRIP`, before it kept them), so every row
/// analyzed to the SAME postings regardless of `i`: a BM25 query could never
/// tell them apart. Here, only index 5's subject mentions "body" and "record"
/// at all — the marker `a_built_pack_answers_bm25_from_its_own_postings`
/// below searches for — so a query for those terms can only ever match row 5,
/// deterministically, rather than relying on a digit that used not to reach
/// the postings.
const TEXT_BANK: [&str; 8] = [
    "granite countertops resist heat and scratches",
    "the orchestra rehearsed the symphony twice",
    "kubernetes reschedules pods after a node drains",
    "sourdough starter needs a daily feeding",
    "glaciers retreat faster during warm summers",
    "body of record five carries the fixture's search marker",
    "the violin section tuned before the overture",
    "compost accelerates when turned weekly",
];

fn rec(i: usize) -> Record {
    Record {
        chunk_id: format!("doc{i}:0"),
        doc_id: format!("doc{i}"),
        text: format!("{}\nwith a newline", TEXT_BANK[i % TEXT_BANK.len()]),
        heading_path: format!("# H{i}"),
        uri: format!("file:///{i}.md"),
        title: format!("Title {i}"),
        page_no: if i.is_multiple_of(2) {
            Some(i as i64)
        } else {
            None
        },
        source_type: "markdown".into(),
        // Never written to `pack.rec` (it is `#[serde(skip)]`) and never read
        // on the write path: `pack.links` is the count's single source, and
        // `Pack::hydrate` fills the field in from there on the way out. See
        // `build_pack_with_links` below.
        inbound: 0,
        // Same story as `inbound` above, but for `pack.status`: never written
        // on the write path, filled in by `Pack::hydrate` on the way out.
        lifecycle: Default::default(),
        last_used: None,
        memory: None,
    }
}

#[test]
fn records_round_trip_by_row_ordinal() {
    let d = tempfile::tempdir().unwrap();
    let rows: Vec<Record> = (0..50).map(rec).collect();
    records::write(d.path(), &rows).unwrap();

    let r = records::Reader::open(d.path()).unwrap();
    assert_eq!(r.len(), 50);
    // Read out of order: the ordinal is the join key, so random access must be
    // exact, not merely sequentially correct.
    for i in [49usize, 0, 17, 33, 1] {
        assert_eq!(r.get(i).unwrap(), rec(i), "row {i}");
    }
}

#[test]
fn an_out_of_range_row_is_an_error_not_a_panic() {
    let d = tempfile::tempdir().unwrap();
    records::write(d.path(), &[rec(0)]).unwrap();
    let r = records::Reader::open(d.path()).unwrap();
    assert!(r.get(1).is_err());
}

/// Text carries newlines and non-ASCII; a length-prefixed encoding must not
/// care. A delimiter-based one would corrupt exactly the transcripts that make
/// up most of this corpus.
/// An index with no chunks is a real state (a fresh install, or a corpus whose
/// every document was skipped). `len()` computes `idx.len() / 8 - 1`, so the
/// empty case is one subtraction away from an underflow panic on the read path.
#[test]
fn an_empty_record_set_reads_back_as_empty_not_a_panic() {
    let d = tempfile::tempdir().unwrap();
    records::write(d.path(), &[]).unwrap();
    let r = records::Reader::open(d.path()).unwrap();
    assert_eq!(r.len(), 0);
    assert!(r.is_empty());
    assert!(
        r.get(0).is_err(),
        "row 0 of an empty pack must error, not panic"
    );
}

#[test]
fn records_survive_newlines_and_unicode() {
    let d = tempfile::tempdir().unwrap();
    let mut r0 = rec(0);
    r0.text = "line one\n\nline two — em dash, emoji 🧠, tab\there".into();
    r0.title = "Ünïcödé".into();
    records::write(d.path(), &[r0.clone()]).unwrap();
    assert_eq!(records::Reader::open(d.path()).unwrap().get(0).unwrap(), r0);
}

#[test]
fn the_record_files_are_each_row_as_json_then_the_offsets_with_a_sentinel() {
    let d = tempfile::tempdir().unwrap();
    let rows: Vec<Record> = (0..5).map(rec).collect();
    records::write(d.path(), &rows).unwrap();

    let mut blob = Vec::new();
    let mut offsets = vec![0u64];
    for r in &rows {
        blob.extend(serde_json::to_vec(r).unwrap());
        offsets.push(blob.len() as u64);
    }
    let index: Vec<u8> = offsets.iter().flat_map(|o| o.to_le_bytes()).collect();
    assert_eq!(
        std::fs::read(d.path().join(records::REC_FILE)).unwrap(),
        blob
    );
    assert_eq!(
        std::fs::read(d.path().join(records::IDX_FILE)).unwrap(),
        index
    );
}

use br8n::pack::vectors;

/// Unit vectors whose NEAREST neighbour is unambiguous, so a nearest-neighbour
/// assertion is about the index and not about a tie-break.
///
/// Everything behind the nearest neighbour is the opposite: query with one of
/// these and every other row is orthogonal to it, so all of them come back at
/// one bit-identical similarity of 0.5. That seven-way tie is what
/// `pack_search_breaks_ties_by_chunk_id_ascending` below is built on.
fn basis(dims: usize, hot: usize) -> Vec<f32> {
    let mut v = vec![0.0f32; dims];
    v[hot] = 1.0;
    v
}

#[test]
fn vector_search_returns_the_nearest_row_and_a_similarity() {
    let d = tempfile::tempdir().unwrap();
    let rows: Vec<Vec<f32>> = (0..8).map(|i| basis(8, i)).collect();
    vectors::build(d.path(), 8, &rows).unwrap();

    let r = vectors::Reader::open(d.path(), 8).unwrap();
    let hits = r.search(&basis(8, 3), 1, 64).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, 3, "row 3 is the exact match");
    assert!(
        (hits[0].1 - 1.0).abs() < 1e-3,
        "an exact match must score ~1.0, got {}",
        hits[0].1
    );
}

/// `relevance` is a [0,1] cosine and gates injection. A similarity outside that
/// range would let any threshold be cleared or none be.
#[test]
fn similarities_are_in_the_unit_interval() {
    let d = tempfile::tempdir().unwrap();
    let rows: Vec<Vec<f32>> = (0..8).map(|i| basis(8, i)).collect();
    vectors::build(d.path(), 8, &rows).unwrap();
    let r = vectors::Reader::open(d.path(), 8).unwrap();
    for (_, sim) in r.search(&basis(8, 0), 8, 64).unwrap() {
        assert!((0.0..=1.0).contains(&sim), "similarity {sim} out of range");
    }
}

/// An empty query vector segfaults lbug's vector extension; whatever usearch
/// does with one, this layer must return empty rather than find out.
#[test]
fn an_empty_query_returns_empty_not_a_crash() {
    let d = tempfile::tempdir().unwrap();
    vectors::build(d.path(), 8, &[basis(8, 0)]).unwrap();
    let r = vectors::Reader::open(d.path(), 8).unwrap();
    assert!(r.search(&[], 5, 64).unwrap().is_empty());
}

#[test]
fn a_wrong_length_query_is_an_error_not_a_crash() {
    let d = tempfile::tempdir().unwrap();
    vectors::build(d.path(), 8, &[basis(8, 0)]).unwrap();
    let r = vectors::Reader::open(d.path(), 8).unwrap();
    assert!(r.search(&[1.0, 0.0], 5, 64).is_err());
}

/// A deterministic xorshift, not the `rand` crate: this tree has no direct
/// dependency on it, and a seeded generator gives every run the same corpus
/// and the same queries, which is what lets the single-threaded baseline
/// below be compared against the concurrent runs bit-for-bit.
fn seeded_unit_vector(dims: usize, seed: u64) -> Vec<f32> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
    let mut next_u64 = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut v: Vec<f32> = (0..dims)
        .map(|_| ((next_u64() >> 11) as f64 / (1u64 << 53) as f64) as f32 * 2.0 - 1.0)
        .collect();
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    for x in v.iter_mut() {
        *x /= norm;
    }
    v
}

/// `vectors::Reader::search` sets index-wide search width
/// (`change_expansion_search`, index_dense.hpp:764) and then searches
/// (index_dense.hpp:2151-2159 thread-slot reservation, :2234's "Reserve
/// capacity ahead of searches!" on exhaustion) as two separate usearch calls.
/// Concurrent callers at different `efs` race on that shared config, and a
/// caller that finds no free thread slot fails outright. `Reader::search`'s
/// `search_lock` serializes the pair; this pins that every thread still gets
/// correct, `k`-length results under real contention, not just that nothing
/// panics.
#[test]
fn concurrent_searches_at_different_efs_do_not_corrupt_or_fail() {
    let d = tempfile::tempdir().unwrap();
    let dims = 32;
    let rows: Vec<Vec<f32>> = (0..400)
        .map(|i| seeded_unit_vector(dims, i as u64))
        .collect();
    vectors::build(d.path(), dims, &rows).unwrap();
    let reader = std::sync::Arc::new(vectors::Reader::open(d.path(), dims).unwrap());

    let queries: Vec<Vec<f32>> = (0..8)
        .map(|i| seeded_unit_vector(dims, 1_000_000 + i as u64))
        .collect();
    let k = 5;
    let efs_values = [1usize, 256usize];

    let mut expected = std::collections::HashMap::new();
    for &efs in &efs_values {
        for (qi, q) in queries.iter().enumerate() {
            let hits = reader.search(q, k, efs).unwrap();
            assert_eq!(hits.len(), k);
            expected.insert((efs, qi), hits);
        }
    }

    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()) * 4;
    std::thread::scope(|scope| {
        for t in 0..threads {
            let reader = std::sync::Arc::clone(&reader);
            let queries = &queries;
            let expected = &expected;
            scope.spawn(move || {
                for i in 0..50 {
                    let efs = efs_values[(t + i) % efs_values.len()];
                    let qi = (t + i) % queries.len();
                    let hits = reader.search(&queries[qi], k, efs).unwrap();
                    assert_eq!(hits.len(), k, "thread {t} call {i} at efs={efs}");
                    assert_eq!(
                        hits,
                        expected[&(efs, qi)],
                        "thread {t} call {i} at efs={efs} query {qi} diverged from the \
                         single-threaded baseline"
                    );
                }
            });
        }
    });
}

use br8n::pack::Pack;

fn build_pack(dir: &std::path::Path, n: usize) {
    build_pack_with_links(dir, n, &Default::default());
}

/// `build_pack`, with an inbound-link-count map. Split out so the default
/// fixture keeps every record's `inbound` at 0 and the existing equality
/// assertions against `rec(i)` stay true.
fn build_pack_with_links(
    dir: &std::path::Path,
    n: usize,
    inbound: &std::collections::HashMap<String, u32>,
) {
    let rows: Vec<(Record, Vec<f32>)> = (0..n).map(|i| (rec(i), basis(8, i))).collect();
    Pack::build(
        dir,
        "qwen3-embedding:0.6b@512+qwen3",
        8,
        rows,
        inbound,
        &Default::default(),
    )
    .unwrap();
}

/// `build_pack`, with a lifecycle map. Split out for the same reason
/// `build_pack_with_links` is: the default fixture must keep every row
/// `Current` so the existing equality assertions against `rec(i)` stay true.
fn build_pack_with_lifecycles(
    dir: &std::path::Path,
    n: usize,
    lifecycles: &std::collections::HashMap<String, br8n::pack::status::Lifecycle>,
) {
    let rows: Vec<(Record, Vec<f32>)> = (0..n).map(|i| (rec(i), basis(8, i))).collect();
    Pack::build(
        dir,
        "qwen3-embedding:0.6b@512+qwen3",
        8,
        rows,
        &Default::default(),
        lifecycles,
    )
    .unwrap();
}

#[test]
fn a_pack_round_trips_search_and_hydration_together() {
    let d = tempfile::tempdir().unwrap();
    build_pack(d.path(), 8);
    let p = Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).unwrap();
    assert_eq!(p.rows(), 8);

    let hits = p.search(&basis(8, 5), 1, 64).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].0,
        rec(5),
        "the vector row must hydrate to ITS record"
    );
    assert!((hits[0].1 - 1.0).abs() < 1e-3);
}

/// `Pack::search` must order its hits by relevance DESCENDING, `chunk_id`
/// ASCENDING — the same key `weight_and_order` uses downstream, and the pack
/// is the one vector search backend the prompt hook actually runs on.
///
/// This is not about display order. `retrieve::run` seeds graph expansion from
/// `vector_hits.iter().take(5)` — the RAW list, before any weighting or
/// re-ordering — so the order here decides WHICH chunks get expanded.
///
/// The fixture is `build_pack`'s own eight orthogonal basis vectors, which
/// look tie-free but are not: queried with `basis(8, 0)`, row 0 is the exact
/// match at similarity 1.0 and the OTHER SEVEN are all orthogonal to the
/// query, so they share one bit-identical similarity of 0.5. That is a
/// genuine seven-way tie, and it is the ordinary case rather than a contrived
/// one — the pack's vector index is f16-quantized, so distinct vectors
/// routinely collapse onto identical distances.
///
/// Measured, so this test is known to fail without the sort rather than
/// assumed to: dumping the `Vec<(usize, f32)>` usearch returns for exactly
/// this query (lldb, on the built test binary) gives rows
/// `0, 7, 6, 5, 4, 3, 2, 1` — the tie group in DESCENDING row order, which is
/// descending `chunk_id`, the exact reverse of what is asserted below.
#[test]
fn pack_search_breaks_ties_by_chunk_id_ascending() {
    let d = tempfile::tempdir().unwrap();
    build_pack(d.path(), 8);
    let p = Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).unwrap();

    let q = basis(8, 0);
    let hits = p.search(&q, 8, 64).unwrap();
    assert_eq!(hits.len(), 8, "all eight rows were asked for");

    // Fixture assumption, asserted rather than assumed: rows 1..8 are each
    // orthogonal to the query, so their relevance must be ONE value, not seven
    // near-equal ones. If this ever stops holding there is no tie here and the
    // ordering assertion below would be about relevance, not the tie-break.
    assert!(
        hits[1..]
            .iter()
            .all(|h| h.1.to_bits() == hits[1].1.to_bits()),
        "fixture assumption: the seven non-matching rows must tie exactly — \
         got {:?}",
        hits.iter().map(|h| h.1).collect::<Vec<_>>()
    );
    assert!(
        hits[0].1 > hits[1].1,
        "the exact match must still outrank the tie group: relevance orders \
         first, chunk_id only breaks ties"
    );

    let ids: Vec<String> = hits.iter().map(|h| h.0.chunk_id.clone()).collect();
    let mut want = ids.clone();
    want.sort();
    assert_eq!(
        ids, want,
        "tied hits must come back in ascending chunk_id, the same key \
         weight_and_order sorts by downstream"
    );

    // Repeatability alone would be a weak assertion — it can pass by luck
    // whenever the underlying order happens to be stable — so it is here only
    // as a companion to the ordering assertion above, never as a substitute.
    let again: Vec<String> = p
        .search(&q, 8, 64)
        .unwrap()
        .into_iter()
        .map(|h| h.0.chunk_id)
        .collect();
    assert_eq!(again, ids, "the same query twice must give the same order");
}

/// The row-count check is the guard on the one invariant with no downstream
/// detection: vectors and records from different generations would attach the
/// right score to the wrong document, silently.
#[test]
fn a_row_count_disagreement_is_refused() {
    let d = tempfile::tempdir().unwrap();
    build_pack(d.path(), 8);
    let mut m = br8n::pack::manifest::Manifest::read(d.path()).unwrap();
    m.rows = 7;
    m.write(d.path()).unwrap();

    let err = Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).unwrap_err();
    assert!(
        err.to_string().contains("rows"),
        "the error must name the row disagreement, got: {err}"
    );
}

#[test]
fn a_pack_built_by_another_model_is_refused() {
    let d = tempfile::tempdir().unwrap();
    build_pack(d.path(), 4);
    assert!(Pack::open(d.path(), "nomic-embed-text@768+nomic", 8).is_err());
}

#[test]
fn a_missing_pack_is_an_error_not_an_empty_pack() {
    let d = tempfile::tempdir().unwrap();
    assert!(Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).is_err());
}

// `open_pack_beside` is what `retrieve_for` actually calls to decide between
// "no pack, degrade to the store" and "invalid pack, refuse" — the distinction
// that guards against a stale pack attaching correct-looking relevance to the
// wrong documents. `Pack::open` alone cannot tell those two cases apart (both
// fail identically), which is exactly why this wrapper exists.
use br8n::pack::open_pack_beside;

/// An index built before packs existed has no manifest at all. That must
/// degrade silently to the store, not be treated as a broken pack.
#[test]
fn open_pack_beside_with_no_manifest_degrades_to_none() {
    let d = tempfile::tempdir().unwrap();
    let got = open_pack_beside(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).unwrap();
    assert!(got.is_none(), "no manifest at all must degrade, not error");
}

/// The manifest says a pack exists, but its vector file is gone — a directory
/// that was corrupted or partially removed by hand. This must refuse, not
/// silently return `None`: `None` is what the caller reads as "there was never
/// a pack here", which is a different (and safe) fact from "there was one and
/// it broke".
#[test]
fn open_pack_beside_with_a_deleted_vector_file_is_refused() {
    let d = tempfile::tempdir().unwrap();
    build_pack(d.path(), 8);
    std::fs::remove_file(d.path().join(br8n::pack::vectors::VEC_FILE)).unwrap();

    let err = open_pack_beside(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("pack"),
        "got: {err}"
    );
    assert!(
        err.downcast_ref::<br8n::retrieve::PackRefused>().is_some(),
        "a pack that exists but does not validate must be tagged PackRefused, or the \
         dashboard and MCP surfaces show a generic \"busy\"/\"no results\" instead of \
         this refusal's own message; got: {err}"
    );
}

/// The manifest file exists but is not valid JSON — e.g. truncated by a crash
/// mid-write. Must refuse, not degrade.
#[test]
fn open_pack_beside_with_an_unparsable_manifest_is_refused() {
    let d = tempfile::tempdir().unwrap();
    build_pack(d.path(), 8);
    std::fs::write(
        d.path().join(br8n::pack::manifest::MANIFEST_FILE),
        "not json at all",
    )
    .unwrap();

    let err = open_pack_beside(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).unwrap_err();
    assert!(
        err.downcast_ref::<br8n::retrieve::PackRefused>().is_some(),
        "a pack that exists but does not validate must be tagged PackRefused, or the \
         dashboard and MCP surfaces show a generic \"busy\"/\"no results\" instead of \
         this refusal's own message; got: {err}"
    );
}

/// The happy path: a complete, valid pack opens.
#[test]
fn open_pack_beside_with_a_valid_pack_opens_it() {
    let d = tempfile::tempdir().unwrap();
    build_pack(d.path(), 8);

    let got = open_pack_beside(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).unwrap();
    assert!(got.is_some(), "a valid, complete pack must open");
    assert_eq!(got.unwrap().rows(), 8);
}

use br8n::pack::analyze;

/// The analyzer must agree with lbug's, because it produces every posting in
/// `pack.fts` and a disagreement is invisible to any test that uses the same
/// analyzer on both sides. Fixed vocabulary here; Task 7 checks the real
/// corpus against lbug's own `stem()`.
#[test]
fn the_stemmer_matches_the_cases_lbug_was_measured_on() {
    for (raw, want) in [
        ("running", "run"),
        ("pooling", "pool"),
        ("assistant", "assist"),
        ("computed", "comput"),
        ("statically", "static"),
        ("organisation", "organis"),
        ("written", "written"),
    ] {
        assert_eq!(analyze::stem(raw), want, "stem({raw})");
    }
}

/// Tokenization must collapse the same way `fts_body` does: newlines are
/// already spaces by the time text is stored, punctuation splits, case folds.
#[test]
fn analyze_splits_and_folds_like_the_stored_fts_body() {
    let got = analyze::analyze("PgBouncer runs in transaction-mode; drops session state.");
    assert!(got.contains(&"pgbouncer".to_string()), "got {got:?}");
    assert!(got.contains(&"transact".to_string()), "got {got:?}");
    assert!(
        !got.iter().any(|t| t.contains('-')),
        "punctuation must split: {got:?}"
    );
    assert!(
        !got.contains(&"in".to_string()),
        "stopwords must be dropped: {got:?}"
    );
}

/// The analyzer must keep digits. It used to delete them, bug-compatibly with
/// lbug's `simple` FTS tokenizer — a target that no longer exists, because
/// there is no FTS index and `pack.fts` is the only keyword index in the
/// product. The cost was that every literal identifier a person actually
/// searches for collapsed: ADR-0004 and ADR-0001 both indexed as `adr`.
#[test]
fn the_analyzer_keeps_digits_because_identifiers_are_what_keyword_search_is_for() {
    let t = analyze::analyze("ADR-0004 supersedes ADR-0001 at efs=200");

    assert!(
        t.contains(&"adr".to_string()) || t.iter().any(|x| x.starts_with("adr")),
        "the alphabetic part survives as before: {t:?}"
    );

    assert!(
        t.iter().any(|x| x.contains("0004")),
        "ADR-0004 must stay distinguishable from ADR-0001: {t:?}"
    );
    assert!(
        t.iter().any(|x| x.contains("0001")),
        "ADR-0001 must stay distinguishable from ADR-0004: {t:?}"
    );
    assert!(
        t.iter().any(|x| x.contains("200")),
        "efs=200's number survives: {t:?}"
    );
}

/// Alphanumeric identifiers with an embedded digit must not be split at the
/// digit boundary now that digits survive `STRIP`.
#[test]
fn v2ray_and_utf8_analyze_as_single_tokens() {
    let v2ray = analyze::analyze("v2ray");
    assert_eq!(
        v2ray.len(),
        1,
        "v2ray must not split at the digit: {v2ray:?}"
    );
    assert!(
        v2ray[0].contains('2'),
        "v2ray's digit must survive stemming: {v2ray:?}"
    );

    let utf8 = analyze::analyze("utf8");
    assert_eq!(utf8, vec!["utf8".to_string()], "utf8 must survive whole");
}

/// Bumping the analyzer without bumping its identity would serve wrong
/// postings from an existing pack with no error — the exact silent-degradation
/// failure `Manifest::validate` exists to prevent.
#[test]
fn changing_the_analyzer_changes_its_published_identity() {
    assert_ne!(
        analyze::ANALYZER,
        "porter+simple+default-stop/1",
        "the identity must change when tokenization changes, or an old pack \
         loads silently against a new analyzer"
    );
}

use br8n::pack::postings;

fn corpus() -> Vec<Vec<String>> {
    [
        "pgbouncer runs in transaction pooling mode",
        "sourdough needs a long cold ferment for flavour",
        "connection pooling reduces postgres backend connections",
        "the cold ferment develops sourdough flavour slowly",
    ]
    .iter()
    .map(|t| br8n::pack::analyze::analyze(t))
    .collect()
}

#[test]
fn postings_rank_rows_by_bm25() {
    let d = tempfile::tempdir().unwrap();
    postings::write(d.path(), &corpus()).unwrap();
    let r = postings::Reader::open(d.path()).unwrap();

    let hits = r.search("connection pooling", 10);
    assert!(!hits.is_empty(), "a matching query must return rows");
    assert_eq!(
        hits[0].0, 2,
        "row 2 mentions both terms and must rank first"
    );
    assert!(
        hits.windows(2).all(|w| w[0].1 >= w[1].1),
        "scores must descend"
    );
}

#[test]
fn a_term_in_no_row_returns_nothing() {
    let d = tempfile::tempdir().unwrap();
    postings::write(d.path(), &corpus()).unwrap();
    let r = postings::Reader::open(d.path()).unwrap();
    assert!(r.search("kubernetes", 10).is_empty());
}

/// A rare term must outrank a common one — that is the whole point of BM25,
/// and a bug in the idf term would still return plausible-looking hits.
#[test]
fn a_rare_term_beats_a_common_one() {
    let d = tempfile::tempdir().unwrap();
    postings::write(d.path(), &corpus()).unwrap();
    let r = postings::Reader::open(d.path()).unwrap();
    let rare = r.search("pgbouncer", 10)[0].1;
    let common = r.search("pooling", 10)[0].1;
    assert!(
        rare > common,
        "pgbouncer (df=1) {rare} must beat pooling (df=2) {common}"
    );
}

#[test]
fn an_empty_corpus_reads_back_as_empty_not_a_panic() {
    let d = tempfile::tempdir().unwrap();
    postings::write(d.path(), &[]).unwrap();
    let r = postings::Reader::open(d.path()).unwrap();
    assert!(r.search("anything", 5).is_empty());
}

#[test]
fn a_truncated_postings_file_is_refused() {
    let d = tempfile::tempdir().unwrap();
    postings::write(d.path(), &corpus()).unwrap();
    let p = d.path().join(postings::FTS_FILE);
    let bytes = std::fs::read(&p).unwrap();
    std::fs::write(&p, &bytes[..bytes.len() / 2]).unwrap();
    assert!(postings::Reader::open(d.path()).is_err());
}

#[test]
fn the_postings_file_is_laid_out_exactly_as_its_module_doc_specifies() {
    let rows: Vec<Vec<String>> = [
        "pool pool connection backend filler words here",
        "pool",
        "connection pool pool pool",
        "backend",
    ]
    .iter()
    .map(|r| r.split(' ').map(String::from).collect())
    .collect();
    let d = tempfile::tempdir().unwrap();
    postings::write(d.path(), &rows).unwrap();
    assert_eq!(
        std::fs::read(d.path().join(postings::FTS_FILE)).unwrap(),
        postings_as_specified(&rows)
    );
}

fn postings_as_specified(rows: &[Vec<String>]) -> Vec<u8> {
    let n = rows.len() as f32;
    let avgdl = rows.iter().map(|r| r.len() as u64).sum::<u64>() as f32 / n;
    let mut vocabulary: Vec<&str> = rows.iter().flatten().map(String::as_str).collect();
    vocabulary.sort_unstable();
    vocabulary.dedup();
    let runs: Vec<Vec<(u32, f32)>> = vocabulary
        .iter()
        .map(|&term| {
            let frequencies: Vec<(usize, f32)> = rows
                .iter()
                .enumerate()
                .map(|(row, terms)| (row, terms.iter().filter(|t| *t == term).count() as f32))
                .filter(|&(_, tf)| tf > 0.0)
                .collect();
            let df = frequencies.len() as f32;
            let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
            let mut run: Vec<(u32, f32)> = frequencies
                .into_iter()
                .map(|(row, tf)| {
                    let norm = 1.0 - 0.75 + 0.75 * (rows[row].len() as f32) / avgdl;
                    (row as u32, idf * (tf * (1.2 + 1.0)) / (tf + 1.2 * norm))
                })
                .collect();
            run.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(&b.0)));
            run
        })
        .collect();
    let blob_start = 24 + 24 * vocabulary.len();
    let mut post_off = blob_start + vocabulary.iter().map(|t| t.len()).sum::<usize>();
    let mut str_off = blob_start;
    let mut out = b"BRNPOST1".to_vec();
    out.extend((rows.len() as u32).to_le_bytes());
    out.extend((vocabulary.len() as u32).to_le_bytes());
    out.extend(avgdl.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    for (term, run) in vocabulary.iter().zip(&runs) {
        out.extend((str_off as u32).to_le_bytes());
        out.extend((term.len() as u32).to_le_bytes());
        out.extend((post_off as u64).to_le_bytes());
        out.extend((run.len() as u32).to_le_bytes());
        out.extend(0u32.to_le_bytes());
        str_off += term.len();
        post_off += run.len() * 8;
    }
    for term in &vocabulary {
        out.extend(term.as_bytes());
    }
    for (row, impact) in runs.iter().flatten() {
        out.extend(row.to_le_bytes());
        out.extend(impact.to_le_bytes());
    }
    out
}

/// A file that is the RIGHT length overall but has one corrupt term-table
/// entry must still be refused. `a_truncated_postings_file_is_refused` above
/// halves the file, which always trips the header-level `table_end` check
/// before the reader ever looks at an individual entry — it cannot exercise
/// the per-entry validation loop in `Reader::open`. This test corrupts a
/// single entry's `post_off` in an otherwise well-formed file so that ONLY
/// the per-entry loop can catch it.
#[test]
fn a_corrupt_term_entry_is_refused() {
    let d = tempfile::tempdir().unwrap();
    postings::write(d.path(), &corpus()).unwrap();
    let p = d.path().join(postings::FTS_FILE);
    let mut bytes = std::fs::read(&p).unwrap();

    // Term table starts at byte 24; each entry is 24 bytes:
    // u32 str_off, u32 str_len, u64 post_off, u32 df, u32 pad.
    // Point entry 0's post_off 4 bytes from the end of the file: it starts
    // inside the file (so it clears the `post_off >= table_end` check) but,
    // since df*8 is at least 8, its run extends past the end of the file.
    let post_off_at = 24 + 8;
    let bad_off = (bytes.len() - 4) as u64;
    bytes[post_off_at..post_off_at + 8].copy_from_slice(&bad_off.to_le_bytes());
    std::fs::write(&p, &bytes).unwrap();

    assert!(
        postings::Reader::open(d.path()).is_err(),
        "a corrupt term-table entry must be refused, not mmap'd and dereferenced"
    );
}

/// Postings from a different generation must be refused, exactly as records
/// from a different generation are. `Pack::open` already compared
/// `recs.len()` against the manifest but never checked the postings, even
/// though `postings::Reader` parses `num_rows` out of the header and simply
/// never compared it. A `pack.fts` whose row ids happen to fall IN RANGE would
/// pass every other check and let `Pack::bm25` hydrate confidently wrong
/// documents through `self.recs.get(row)`, with no error anywhere.
///
/// The header's `num_rows` is patched rather than the file truncated, because
/// truncation trips the length checks in `Reader::open` first and so cannot
/// reach this guard. Nothing else in the file changes: this is exactly the
/// shape that used to be accepted.
#[test]
fn postings_over_a_different_row_count_are_refused() {
    let d = tempfile::tempdir().unwrap();
    build_pack(d.path(), 8);

    let p = d.path().join(postings::FTS_FILE);
    let mut bytes = std::fs::read(&p).unwrap();
    // num_rows is the u32 at offset 8, straight after the 8-byte magic.
    assert_eq!(
        u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
        8,
        "fixture assumption: the postings were built over all 8 rows"
    );
    bytes[8..12].copy_from_slice(&4u32.to_le_bytes());
    std::fs::write(&p, &bytes).unwrap();

    // The postings file itself is still structurally valid — every offset and
    // term entry is untouched — so the reader opens it happily. That is what
    // makes this silent without the guard in `Pack::open`.
    assert!(
        postings::Reader::open(d.path()).is_ok(),
        "fixture assumption: only num_rows changed, so the reader still opens it"
    );

    let err = Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8)
        .expect_err("postings built over a different row count must be refused");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("postings") && msg.contains("generation"),
        "the refusal must name the postings and say why: {msg}"
    );
}

/// A pack whose postings were built by a different analyzer must be refused,
/// exactly as a pack from a different embedding model is. Wrong postings look
/// like an honest no-match, which is this project's house failure mode.
#[test]
fn an_analyzer_mismatch_is_refused() {
    let d = tempfile::tempdir().unwrap();
    build_pack(d.path(), 4);
    let mut m = br8n::pack::manifest::Manifest::read(d.path()).unwrap();
    m.analyzer = "something-else/9".into();
    m.write(d.path()).unwrap();
    let err = Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).unwrap_err();
    assert!(err.to_string().contains("analyzer"), "got: {err}");
}

#[test]
fn a_built_pack_answers_bm25_from_its_own_postings() {
    let d = tempfile::tempdir().unwrap();
    build_pack(d.path(), 8);
    let p = Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).unwrap();
    let hits = p.bm25("body of record 5", 5).unwrap();
    assert!(!hits.is_empty(), "BM25 must find the fixture text");
    assert_eq!(hits[0].0.chunk_id, "doc5:0", "and rank the right row first");
}

/// `Pack::cosine_for` must use the calibrated `(1 + s) / 2` scale, never the
/// raw cosine `s` — the same scale `vectors::Reader::search` produces, so the
/// gate and the ordering agree on a chunk's worth regardless of which stage
/// measured it.
///
/// That check has to be made OFF the fixed point. A predecessor test,
/// `pack_cosine_for_matches_the_vector_paths_scale`, compared `Pack::search`
/// against `Pack::cosine_for` for an exact self-match (query == the stored
/// basis vector), which lands at cosine 1.0 — where `(1 + s) / 2` and the raw
/// `s` both equal 1.0 and cannot be told apart. It was blind to the very scale
/// bug it existed to catch, and known to be: reverting `cosine_for` to the raw
/// dot product left it passing, `search=1 cosine_for=1`. It has been deleted
/// rather than left in the tree reading as coverage that does not exist. This
/// test uses an off-axis query at a genuine ~0.707 cosine to row 0, where the
/// two formulas diverge by ~0.15 — comfortably outside any float-precision
/// tolerance.
#[test]
fn pack_cosine_for_uses_the_calibrated_scale_not_the_raw_cosine() {
    let d = tempfile::tempdir().unwrap();
    build_pack(d.path(), 8);
    let p = Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).unwrap();

    // Equal weight on dims 0 and 1: cosine to basis(8, 0) is 1/sqrt(2).
    let mut q = vec![0.0f32; 8];
    q[0] = std::f32::consts::FRAC_1_SQRT_2;
    q[1] = std::f32::consts::FRAC_1_SQRT_2;

    let got = p.cosine_for(&q, &["doc0:0".to_string()]).unwrap();
    let m = *got.get("doc0:0").unwrap();
    let want = (1.0 + std::f32::consts::FRAC_1_SQRT_2) / 2.0;
    assert!(
        (m - want).abs() < 1e-2,
        "want calibrated relevance {want} for cosine {}, got {m}",
        std::f32::consts::FRAC_1_SQRT_2
    );
}

#[test]
fn pack_cosine_for_skips_ids_it_does_not_hold() {
    let d = tempfile::tempdir().unwrap();
    build_pack(d.path(), 4);
    let p = Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).unwrap();
    let got = p
        .cosine_for(&basis(8, 0), &["not-a-chunk".to_string()])
        .unwrap();
    assert!(
        got.is_empty(),
        "an unknown id yields no entry, not a fabricated zero"
    );
}

/// `Pack::cosine_for` binary-searches records assuming they are sorted by
/// `chunk_id` ascending — a promise `Store::all_rows_for_pack`'s `ORDER BY
/// c.id` is supposed to keep. If `Pack::build` ever received rows out of that
/// order it must refuse loudly, not publish a pack whose binary search will
/// silently return wrong-or-missing rows on the read path.
#[test]
fn build_refuses_records_not_sorted_by_chunk_id() {
    let d = tempfile::tempdir().unwrap();
    // rec(1)'s chunk_id "doc1:0" sorts after rec(0)'s "doc0:0", so writing
    // them in this order is out of sequence.
    let rows: Vec<(Record, Vec<f32>)> = vec![(rec(1), basis(8, 1)), (rec(0), basis(8, 0))];
    let err = Pack::build(
        d.path(),
        "qwen3-embedding:0.6b@512+qwen3",
        8,
        rows,
        &Default::default(),
        &Default::default(),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("not sorted by chunk_id"),
        "got: {err}"
    );
}

/// A pack may legitimately have postings and no vectors — that is what phase 1
/// of an asynchronous index publishes, so keyword search works before any
/// embedding has happened.
#[test]
fn a_pack_with_no_vectors_serves_bm25_and_reports_it() {
    let d = tempfile::tempdir().unwrap();
    build_pack(d.path(), 8);
    std::fs::remove_file(d.path().join(br8n::pack::vectors::VEC_FILE)).unwrap();
    let mut m = br8n::pack::manifest::Manifest::read(d.path()).unwrap();
    m.rows_with_vectors = 0;
    m.write(d.path()).unwrap();

    let p = Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).unwrap();
    assert!(!p.has_vectors(), "the pack must know it has none");
    assert!(
        p.search(&basis(8, 3), 5, 64).unwrap().is_empty(),
        "vector search returns nothing rather than fabricating a result"
    );
    assert!(
        !p.bm25("body", 5).unwrap().is_empty(),
        "but BM25 still answers — that is the whole point of phase 1"
    );
}

/// A missing `pack.vec` that the manifest does NOT declare is still corruption.
#[test]
fn a_missing_vector_file_the_manifest_does_not_declare_is_refused() {
    let d = tempfile::tempdir().unwrap();
    build_pack(d.path(), 8);
    std::fs::remove_file(d.path().join(br8n::pack::vectors::VEC_FILE)).unwrap();
    // manifest still claims every row has a vector
    assert!(Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).is_err());
}

/// Refusing a missing `pack.vec` must not cost this process its stdin.
///
/// usearch 2.26.1's `memory_mapped_file_t` leaves `file_descriptor_` at its
/// value-initialized `0` when `open_if_not()` fails, and its destructor's
/// `close()` guards only on `path_` — so a failed `Index::view` executes a
/// literal `::close(0)` (`include/usearch/index.hpp:2114-2128`, reached from
/// `rust/lib.cpp:252`). `vectors::Reader::open` is the only `view` call site
/// in this repository and now refuses an unopenable or empty file itself,
/// before usearch can be handed it.
///
/// The damage is not the lost stdin. With fd 0 free, the next `open` anywhere
/// in the process is handed it and Rust's `OwnedFd` takes ownership; the next
/// failed `view` closes it again, out from under that owner. That is where
/// `unexpected error during closedir: Bad file descriptor` and
/// `fatal runtime error: IO Safety violation: owned file descriptor already
/// closed` in this very test binary came from — intermittent, parallel-only,
/// and nothing to do with the test that happened to be holding the descriptor.
///
/// The probe is one-sided by construction: fds 0, 1 and 2 are open for the
/// whole life of a test binary, so a fresh `open` can only be handed one of
/// them if something closed it. It can MISS the defect under parallel
/// execution (another thread may take the freed slot first); it cannot report
/// one that is not there. The exhaustive check is not a test at all — run the
/// built binary under `lldb` with a conditional breakpoint on `close` where
/// the descriptor argument is 0 and expect zero hits.
#[cfg(unix)]
#[test]
fn a_refused_vector_file_does_not_close_this_process_s_stdin() {
    use std::os::fd::AsRawFd;

    let d = tempfile::tempdir().unwrap();
    build_pack(d.path(), 8);
    std::fs::remove_file(d.path().join(br8n::pack::vectors::VEC_FILE)).unwrap();
    assert!(
        Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).is_err(),
        "fixture assumption: the manifest still declares vectors, so this \
         must take the refusing path through vectors::Reader::open"
    );

    let probe = std::fs::File::open("/dev/null").unwrap();
    assert!(
        probe.as_raw_fd() > 2,
        "refusing a missing pack.vec freed descriptor {} — one of this \
         process's standard streams was closed",
        probe.as_raw_fd()
    );
}

/// `Pack::build` is the WRITE side of phase 1: given rows whose vectors are
/// ALL empty (`Store::all_rows_for_pack`'s convention for "no embedding
/// yet" — see its doc comment), it must publish `pack.rec`/`pack.fts` with
/// `rows_with_vectors: 0` and no `pack.vec` file on disk at all, rather than
/// refusing or fabricating one. This is the write-side half of what
/// `a_pack_with_no_vectors_serves_bm25_and_reports_it` above verifies from
/// the read side with a hand-edited manifest.
#[test]
fn build_with_every_vector_empty_publishes_no_vec_file() {
    let d = tempfile::tempdir().unwrap();
    let rows: Vec<(Record, Vec<f32>)> = (0..8).map(|i| (rec(i), Vec::new())).collect();
    Pack::build(
        d.path(),
        "qwen3-embedding:0.6b@512+qwen3",
        8,
        rows,
        &Default::default(),
        &Default::default(),
    )
    .unwrap();

    assert!(
        !d.path().join(br8n::pack::vectors::VEC_FILE).exists(),
        "phase 1 must not write pack.vec at all"
    );
    let m = br8n::pack::manifest::Manifest::read(d.path()).unwrap();
    assert_eq!(m.rows, 8);
    assert_eq!(m.rows_with_vectors, 0);

    let p = Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).unwrap();
    assert!(!p.has_vectors());
    assert!(
        !p.bm25("body", 5).unwrap().is_empty(),
        "BM25 must still answer"
    );
}

/// A pack where SOME rows have a vector and others do not must be refused,
/// not silently published with the vectorless rows dropped from search —
/// that would orphan an already-embedded chunk from vector search the moment
/// this pack is read. This is the scenario `br8n index --no-embed` reaches
/// when it runs incrementally against an index that already had some
/// embedded chunks: the unchanged ones keep their real vectors, the new ones
/// get none.
#[test]
fn build_refuses_a_mix_of_embedded_and_unembedded_rows() {
    let d = tempfile::tempdir().unwrap();
    let rows: Vec<(Record, Vec<f32>)> = (0..8)
        .map(|i| (rec(i), if i < 4 { basis(8, i) } else { Vec::new() }))
        .collect();
    let err = Pack::build(
        d.path(),
        "qwen3-embedding:0.6b@512+qwen3",
        8,
        rows,
        &Default::default(),
        &Default::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("mixed pack"), "got: {err}");
    assert!(
        !d.path().join(br8n::pack::manifest::MANIFEST_FILE).exists(),
        "a refused build must publish nothing at all"
    );
}

// ---------------------------------------------------------------------------
// pack.links — inbound link counts, joined by the row ordinal
// ---------------------------------------------------------------------------

use br8n::pack::links;

/// A `HashMap<String, u32>` shaped exactly like `Store::inbound_link_counts`
/// returns: only the documents that HAVE at least one inbound link appear.
/// Distinct counts per document, so a row that reads its neighbour's entry is
/// visible rather than accidentally right.
fn link_map() -> std::collections::HashMap<String, u32> {
    [("doc1".to_string(), 3u32), ("doc5".to_string(), 7u32)]
        .into_iter()
        .collect()
}

#[test]
fn links_round_trip_by_row_ordinal() {
    let d = tempfile::tempdir().unwrap();
    let counts: Vec<u32> = (0..50).map(|i| i as u32 * 2).collect();
    links::write(d.path(), &counts, 98).unwrap();

    let r = links::Reader::open(d.path()).unwrap();
    assert_eq!(r.rows(), 50);
    assert_eq!(r.max_inbound(), 98);
    for (i, want) in counts.iter().enumerate() {
        assert_eq!(r.get(i), *want, "row {i} read the wrong count");
    }
}

/// A row past the end reads 0 rather than panicking. `Pack::open` refuses any
/// pack whose link file disagrees with the manifest, so this is unreachable
/// through the pack — but the prompt path must not be one bad index away from
/// a panic.
#[test]
fn a_row_past_the_end_of_the_links_file_reads_zero() {
    let d = tempfile::tempdir().unwrap();
    links::write(d.path(), &[4, 5, 6], 6).unwrap();
    let r = links::Reader::open(d.path()).unwrap();
    assert_eq!(r.get(2), 6);
    assert_eq!(r.get(3), 0);
    assert_eq!(r.get(usize::MAX), 0);
}

#[test]
fn a_links_file_with_a_bad_magic_number_is_refused() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join(links::LINKS_FILE), vec![0u8; 32]).unwrap();
    let err = links::Reader::open(d.path()).unwrap_err();
    assert!(err.to_string().contains("magic"), "got: {err}");
}

/// A `pack.links` whose header claims a different number of rows than the
/// bytes hold is a truncated or over-long file, and there is no per-entry
/// structure that a later check could catch it with — it is a flat `u32`
/// array, so any offset into it decodes to something.
#[test]
fn a_truncated_links_file_is_refused() {
    let d = tempfile::tempdir().unwrap();
    links::write(d.path(), &[1, 2, 3, 4], 4).unwrap();
    let p = d.path().join(links::LINKS_FILE);
    let mut bytes = std::fs::read(&p).unwrap();
    bytes.truncate(bytes.len() - 4);
    std::fs::write(&p, bytes).unwrap();
    let err = links::Reader::open(d.path()).unwrap_err();
    assert!(err.to_string().contains("header claims"), "got: {err}");
}

/// THE refusal this file exists for.
///
/// `pack.links` is joined to `pack.rec` by the row ordinal and by nothing
/// else. A links file from another generation therefore lifts the WRONG
/// documents by entirely plausible amounts: `authority_lift` multiplies
/// `relevance`, which drives both the injection gate and the final ordering,
/// so the result is a confidently wrong ranking with no error anywhere — the
/// house failure mode. The row count is what catches it, exactly as it does
/// for the records and the postings.
///
/// Mutation-confirmed: with the `links.rows() == m.rows` guard removed from
/// `Pack::open`, this test fails — `Pack::open` returns `Ok` and serves a
/// five-row link file against eight rows of records.
#[test]
fn a_links_file_from_another_generation_is_refused() {
    let d = tempfile::tempdir().unwrap();
    build_pack(d.path(), 8);
    assert!(
        Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).is_ok(),
        "the fixture pack must open before it is corrupted"
    );

    // A links file for a five-row generation, dropped beside eight rows of
    // records. Everything else in the directory is untouched.
    links::write(d.path(), &[9, 9, 9, 9, 9], 9).unwrap();

    let err = Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("different generations") && msg.contains("--reindex"),
        "the refusal must name the cause and the remedy, got: {msg}"
    );
}

/// A missing `pack.links` is a REFUSAL too, not a degrade to "nothing is
/// linked". Zeros would make every `authority_lift` exactly 1.0, so a user
/// with authority switched on would silently get unweighted results that look
/// identical to weighted ones.
#[test]
fn a_missing_links_file_is_refused_not_treated_as_no_links() {
    let d = tempfile::tempdir().unwrap();
    build_pack(d.path(), 8);
    std::fs::remove_file(d.path().join(links::LINKS_FILE)).unwrap();
    assert!(Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).is_err());
}

/// A pack published by the previous format has no `pack.links` at all, and no
/// honest way to be read. The format check must reject it BEFORE any file is
/// opened, so the user gets the reindex instruction rather than a missing-file
/// error naming an internal filename.
#[test]
fn a_format_3_pack_is_refused_with_the_reindex_message() {
    assert_eq!(FORMAT, 4, "this test pins the bump that added pack.links");
    let mut old = m();
    old.format = 3;
    let err = old
        .validate("qwen3-embedding:0.6b@512+qwen3", 512)
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("format 3"), "got: {msg}");
    assert!(msg.contains("br8n index --reindex"), "got: {msg}");
}

/// Hydration attaches each row's OWN document's count. The fixture gives two
/// documents distinct, non-adjacent counts, so a row reading its neighbour's
/// entry — an off-by-one in the ordinal join — is a failure rather than a
/// coincidence.
#[test]
fn hydrated_records_carry_their_own_documents_inbound_count() {
    let d = tempfile::tempdir().unwrap();
    let inbound = link_map();
    build_pack_with_links(d.path(), 8, &inbound);
    let p = Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).unwrap();

    assert_eq!(p.max_inbound(), 7, "the corpus maximum comes from the map");

    // Every row, reached through vector search, which is what the prompt path
    // uses. `basis(8, i)` makes row `i` the exact match for query `i`.
    for i in 0..8 {
        let hits = p.search(&basis(8, i), 1, 64).unwrap();
        assert_eq!(hits.len(), 1);
        let want = inbound.get(&format!("doc{i}")).copied().unwrap_or(0);
        assert_eq!(
            hits[0].0.inbound, want,
            "row {i} ({}) carried the wrong inbound count",
            hits[0].0.doc_id
        );
    }
}

/// BM25 hydration must attach the count too, not only vector search. A hit
/// found by keyword and a hit found by vector are weighted by the same
/// `rank_weight`, so a count present on one and absent from the other would
/// rank the same document differently depending on which retriever found it.
#[test]
fn bm25_hits_carry_the_inbound_count_as_well() {
    let d = tempfile::tempdir().unwrap();
    build_pack_with_links(d.path(), 8, &link_map());
    let p = Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).unwrap();

    let hits = p.bm25("body of record 5", 5).unwrap();
    assert!(!hits.is_empty(), "the fixture must match something");
    let five = hits
        .iter()
        .find(|(r, _)| r.doc_id == "doc5")
        .expect("doc5 must be found by its own marker text");
    assert_eq!(five.0.inbound, 7);
}

/// `pack.rec` must not grow a copy of the count. `pack.links` is its single
/// source, so there is nothing that can disagree with it — and the record
/// blob's bytes are what they were before this field existed.
#[test]
fn the_inbound_count_is_not_written_into_the_record_blob() {
    let d = tempfile::tempdir().unwrap();
    build_pack_with_links(d.path(), 8, &link_map());
    let blob = std::fs::read(d.path().join(records::REC_FILE)).unwrap();
    let text = String::from_utf8_lossy(&blob);
    assert!(
        !text.contains("inbound"),
        "pack.rec must not carry the count; pack.links is its only source"
    );
    // Same obligation for `lifecycle`: `pack.status` is its only source, and a
    // `Record` carrying a copy in `pack.rec` would be a format break for every
    // OLD pack on disk, with no `FORMAT` bump to announce it — see
    // `status`'s module docs. Blocked only incidentally today, because
    // `Lifecycle` derives no `Serialize`; this line is what actually pins it.
    assert!(
        !text.contains("lifecycle"),
        "pack.rec must not carry the lifecycle; pack.status is its only source"
    );

    // And the reader hands back 0 for a record it decoded without the pack.
    let r = records::Reader::open(d.path()).unwrap();
    assert_eq!(r.get(5).unwrap().inbound, 0);
}

/// Decision 8's ladder, pinned. `Current` MUST be the zero discriminant: a
/// zeroed or absent status file has to read as "no demotion", never as
/// `Superseded`, which would demote every row in the corpus at once.
#[test]
fn the_lifecycle_ladder_maps_the_vaults_vocabulary() {
    assert_eq!(Lifecycle::default(), Lifecycle::Current);
    assert_eq!(Lifecycle::Current as u8, 0, "zero must mean no demotion");

    assert_eq!(
        Lifecycle::from_status(Some("accepted"), false),
        Lifecycle::Current
    );
    assert_eq!(
        Lifecycle::from_status(Some("active"), false),
        Lifecycle::Current
    );
    assert_eq!(
        Lifecycle::from_status(Some("superseded"), false),
        Lifecycle::Superseded
    );
    assert_eq!(
        Lifecycle::from_status(Some("investigating"), false),
        Lifecycle::Investigating
    );

    // Everything else, INCLUDING absent, is Proposed.
    assert_eq!(
        Lifecycle::from_status(Some("proposed"), false),
        Lifecycle::Proposed
    );
    assert_eq!(
        Lifecycle::from_status(Some("draft"), false),
        Lifecycle::Proposed
    );
    assert_eq!(
        Lifecycle::from_status(Some("shaping"), false),
        Lifecycle::Proposed
    );
    assert_eq!(
        Lifecycle::from_status(Some("banana"), false),
        Lifecycle::Proposed
    );
    assert_eq!(Lifecycle::from_status(None, false), Lifecycle::Proposed);

    // "Used once" = at least one inbound wikilink. Only lifts Proposed.
    assert_eq!(
        Lifecycle::from_status(Some("proposed"), true),
        Lifecycle::Investigating
    );
    assert_eq!(Lifecycle::from_status(None, true), Lifecycle::Investigating);
    // It must NOT rescue a superseded record.
    assert_eq!(
        Lifecycle::from_status(Some("superseded"), true),
        Lifecycle::Superseded
    );
    // Case and whitespace are the vault's, not ours to be strict about.
    assert_eq!(
        Lifecycle::from_status(Some("  Superseded "), false),
        Lifecycle::Superseded
    );

    // An unknown byte on the read path is Current, not a panic and not a demotion.
    assert_eq!(Lifecycle::from_byte(200), Lifecycle::Current);
}

/// Round-trip, and the two failure modes that matter.
#[test]
fn the_status_file_round_trips_and_refuses_a_stale_generation() {
    let d = tempfile::tempdir().unwrap();
    let rows = vec![
        Lifecycle::Current,
        Lifecycle::Superseded,
        Lifecycle::Proposed,
        Lifecycle::Investigating,
    ];
    status::write(d.path(), &rows).unwrap();

    let r = status::Reader::open(d.path()).unwrap();
    assert_eq!(r.rows(), 4);
    for (i, want) in rows.iter().enumerate() {
        assert_eq!(r.get(i), *want, "row {i}");
    }
    // Out of range is Current, not a panic — a reader must never take the
    // process down over a row it cannot explain.
    assert_eq!(r.get(999), Lifecycle::Current);

    // A truncated file is a different generation, and must be refused rather
    // than read as a shorter corpus.
    let p = d.path().join(status::STATUS_FILE);
    let bytes = std::fs::read(&p).unwrap();
    std::fs::write(&p, &bytes[..bytes.len() - 1]).unwrap();
    let err = status::Reader::open(d.path()).unwrap_err().to_string();
    assert!(
        err.contains("rows"),
        "a truncated status file must name the row mismatch, got: {err}"
    );
}

/// The status file joins by ROW ORDINAL, exactly as `pack.links` does, and
/// `hydrate` is what fills `Hit.lifecycle` from it.
#[test]
fn a_packs_lifecycle_survives_the_row_ordinal_join() {
    let d = tempfile::tempdir().unwrap();
    let mut lc = std::collections::HashMap::new();
    // `rec(i)` builds `doc_id: format!("doc{i}")` (tests/it/pack.rs:98), so row 3's
    // document is "doc3". Keyed by doc_id because that is what `Pack::build`
    // joins on, exactly as it does for the inbound counts.
    lc.insert(
        "doc3".to_string(),
        br8n::pack::status::Lifecycle::Superseded,
    );
    build_pack_with_lifecycles(d.path(), 8, &lc);

    let p = Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).unwrap();
    let hits = p.search(&basis(8, 3), 1, 64).unwrap();
    assert_eq!(hits[0].0.doc_id, rec(3).doc_id);
    // The load-bearing assertion: `hydrate` must have carried the status onto
    // the `Record` itself, not merely onto the side-file this test can also
    // read independently below. A first draft of this test asserted only the
    // `status::Reader::open` checks that follow, which stayed green with
    // `hydrate`'s `r.lifecycle = ...` line deleted entirely — mutation-tested
    // and confirmed: see the task report.
    assert_eq!(
        hits[0].0.lifecycle,
        br8n::pack::status::Lifecycle::Superseded,
        "the hydrated record must carry row 3's lifecycle, not the default"
    );
    let other = p.search(&basis(8, 4), 1, 64).unwrap();
    assert_eq!(
        other[0].0.lifecycle,
        br8n::pack::status::Lifecycle::Current,
        "a row with no lifecycle entry must hydrate as Current, not a leftover value"
    );

    let r = br8n::pack::status::Reader::open(d.path()).unwrap();
    assert_eq!(r.rows(), 8);
    assert_eq!(
        r.get(3),
        br8n::pack::status::Lifecycle::Superseded,
        "row 3 is the one marked superseded"
    );
    assert_eq!(
        r.get(4),
        br8n::pack::status::Lifecycle::Current,
        "every other row is Current, not zero-meaning-something-else"
    );
}

/// A pack with NO status file must open and read every row `Current`. This is
/// the compatibility contract that lets the side-file ship without a FORMAT
/// bump: a pack published by an older binary has no such file and is valid.
#[test]
fn a_pack_without_a_status_file_opens_and_reads_current() {
    let d = tempfile::tempdir().unwrap();
    build_pack(d.path(), 8);
    std::fs::remove_file(d.path().join(br8n::pack::status::STATUS_FILE)).unwrap();

    let p = Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8)
        .expect("a pack with no status file must still open");
    let hits = p.search(&basis(8, 2), 3, 64).unwrap();
    assert_eq!(hits.len(), 3, "search must work without the side-file");
}

/// A status file from a DIFFERENT generation must be ignored loudly, not
/// joined. The row ordinal is the only join key, so a mismatched length is the
/// only signal available that the two files disagree.
#[test]
fn a_status_file_of_the_wrong_length_is_ignored_not_joined() {
    let d = tempfile::tempdir().unwrap();
    build_pack(d.path(), 8);
    // Rewrite it as if published for a 4-row pack, every row `Superseded` —
    // a value that would be very visible if it leaked onto the real 8-row
    // pack below.
    br8n::pack::status::write(d.path(), &[br8n::pack::status::Lifecycle::Superseded; 4]).unwrap();

    let p = Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8)
        .expect("a mismatched status file must not make the pack unopenable");
    assert_eq!(p.rows(), 8);
    // The load-bearing assertion: a first draft of this test stopped at
    // `p.rows() == 8`, which says nothing about whether the mismatched
    // 4-row file got joined anyway — `p.rows()` is `records::Reader::len`,
    // wired to a completely different file. Mutation-tested and confirmed:
    // removing the `r.rows() == m.rows` guard in `Pack::open` left that
    // assertion green while every one of these rows silently read back as
    // the wrong-generation file's `Superseded`. See the task report.
    for i in 0..8 {
        let hits = p.search(&basis(8, i), 1, 64).unwrap();
        assert_eq!(
            hits[0].0.lifecycle,
            br8n::pack::status::Lifecycle::Current,
            "row {i} must read as Current — the 4-row status file must be \
             ignored entirely, not joined onto the first 4 of these 8 rows"
        );
    }
}

/// The row-count check in `status::Reader::open` is EXACT (`==`), not `>=`.
///
/// `the_status_file_round_trips_and_refuses_a_stale_generation` only exercises
/// a file made too SHORT by truncation, which a `>=` comparison would still
/// catch (a shorter file always reads as `map.len() < want`). This is the
/// other half: a file with EXTRA trailing bytes still claims the same row
/// count in its header, so only an exact-length check catches it — `>=` would
/// silently accept it and let a later `status::Reader::get` read stray bytes
/// as if they were real rows.
#[test]
fn a_status_file_with_extra_trailing_bytes_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let rows = vec![status::Lifecycle::Current, status::Lifecycle::Superseded];
    status::write(d.path(), &rows).unwrap();

    let p = d.path().join(status::STATUS_FILE);
    let mut bytes = std::fs::read(&p).unwrap();
    bytes.push(0); // one stray byte past what the header declares
    std::fs::write(&p, &bytes).unwrap();

    let err = status::Reader::open(d.path()).unwrap_err().to_string();
    assert!(
        err.contains("rows"),
        "a status file with extra trailing bytes must name the row mismatch, got: {err}"
    );
}

/// `Store::all_lifecycles` reads `Document.meta.status`, decides the ladder,
/// applies decision 8's "used once" promotion, and — critically — is small:
/// only a document that resolves to `Current` is ABSENT from the map, because
/// `Current` is also the map's implicit default on the read side
/// (`unwrap_or_default()` in every caller). A document with NO `status:` key
/// at all is NOT one of those — the ladder in `src/pack/status.rs` says a
/// status-less document is `Proposed` (or `Investigating`, with an inbound
/// wikilink), the same rung as `proposed`/`draft`/`shaping`/anything
/// unrecognised, so it must be PRESENT in the map like any other
/// non-`Current` document.
#[test]
fn store_all_lifecycles_reads_status_and_applies_used_once() {
    use br8n::model::{Document, SourceType};
    use br8n::store::Store;

    let d = tempfile::tempdir().unwrap();
    let store = Store::open(d.path(), 4).unwrap();

    let mut superseded = Document::new(SourceType::Markdown, "file:///old.md", "Old", "body");
    superseded.meta = serde_json::json!({"status": "superseded"});
    store.upsert_document(&superseded).unwrap();

    // No `status:` key at all — the common case for every note that is
    // not a decision record. Must be PRESENT in the map as `Proposed`, per the
    // ladder — not absent (which would read back as `Current`).
    let plain = Document::new(SourceType::Markdown, "file:///plain.md", "Plain", "body");
    store.upsert_document(&plain).unwrap();

    // `status: accepted` maps to `Current` too, but via a PRESENT status key
    // rather than an absent one — a different code path than `plain` above,
    // and the one the map's own `!= Lifecycle::Current` filter guards.
    let mut accepted = Document::new(SourceType::Markdown, "file:///live.md", "Live", "body");
    accepted.meta = serde_json::json!({"status": "accepted"});
    store.upsert_document(&accepted).unwrap();

    // `status: proposed` with no inbound link stays `Proposed` — present in
    // the map, since `Proposed != Current`.
    let mut proposed = Document::new(SourceType::Markdown, "file:///draft.md", "Draft", "body");
    proposed.meta = serde_json::json!({"status": "proposed"});
    store.upsert_document(&proposed).unwrap();

    // `status: proposed` WITH an inbound wikilink is lifted to `Investigating`
    // — decision 8's "used once".
    let mut used = Document::new(SourceType::Markdown, "file:///used.md", "Used", "body");
    used.meta = serde_json::json!({"status": "proposed"});
    store.upsert_document(&used).unwrap();
    store
        .link_documents(&plain.id, &used.id, "wikilink")
        .unwrap();

    let inbound = store.inbound_link_counts().unwrap();
    let lc = store.all_lifecycles(&inbound).unwrap();
    assert_eq!(lc.get(&superseded.id), Some(&status::Lifecycle::Superseded));
    assert_eq!(lc.get(&proposed.id), Some(&status::Lifecycle::Proposed));
    assert_eq!(lc.get(&used.id), Some(&status::Lifecycle::Investigating));
    // `plain` is the LINKER here (`plain -> used` above), not the target, so
    // it has zero inbound links of its own and stays on the base rung.
    assert_eq!(
        lc.get(&plain.id),
        Some(&status::Lifecycle::Proposed),
        "a document with no status: key at all must land on Proposed, per \
         the ladder — not be absent from the map, which reads back as Current"
    );
    assert_eq!(
        lc.get(&accepted.id),
        None,
        "status: accepted maps to Current and must also be ABSENT from the \
         map, even though its status key IS present — the exclusion is on \
         the resulting Lifecycle, not on whether the key was there"
    );
}

#[test]
fn a_record_without_memory_facts_serializes_exactly_as_before() {
    let r = br8n::pack::records::Record {
        chunk_id: "d:0".into(),
        doc_id: "d".into(),
        text: "t".into(),
        heading_path: String::new(),
        uri: "file:///a.md".into(),
        title: "A".into(),
        page_no: None,
        source_type: "markdown".into(),
        inbound: 0,
        lifecycle: Default::default(),
        last_used: None,
        memory: None,
    };
    let j = serde_json::to_string(&r).unwrap();
    assert!(
        !j.contains("memory"),
        "main-pack bytes must not change: {j}"
    );
    let back: br8n::pack::records::Record = serde_json::from_str(&j).unwrap();
    assert_eq!(back.memory, None);
}

#[test]
fn memory_facts_round_trip_through_the_record() {
    let facts = br8n::memory::MemoryFacts {
        kind: br8n::memory::MemoryKind::Lesson,
        created: 1_757_000_000,
        project: None,
        origin: br8n::memory::Origin::User,
        confidence: 100,
        session: None,
        source_hash: None,
        source_stamp: None,
    };
    let r = br8n::pack::records::Record {
        chunk_id: "m:0".into(),
        doc_id: "m".into(),
        text: "never comment code".into(),
        heading_path: String::new(),
        uri: "memory://lesson/abc".into(),
        title: "never comment code".into(),
        page_no: None,
        source_type: "memory".into(),
        inbound: 0,
        lifecycle: Default::default(),
        last_used: None,
        memory: Some(facts.clone()),
    };
    let back: br8n::pack::records::Record =
        serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
    assert_eq!(back.memory, Some(facts));
}

#[test]
fn pack_used_round_trips_and_absent_means_no_decay() {
    let d = tempfile::tempdir().unwrap();
    br8n::pack::used::write(d.path(), &[1_700_000_000, 0, 1_800_000_000]).unwrap();
    let r = br8n::pack::used::Reader::open(d.path(), 3)
        .unwrap()
        .unwrap();
    assert_eq!(r.get(0), Some(1_700_000_000));
    assert_eq!(
        r.get(1),
        None,
        "a zero means never used, not used at the epoch"
    );
    assert_eq!(r.get(2), Some(1_800_000_000));
    assert_eq!(r.get(3), None, "a row past the end is not a panic");

    let empty = tempfile::tempdir().unwrap();
    assert!(br8n::pack::used::Reader::open(empty.path(), 3)
        .unwrap()
        .is_none());
}

#[test]
fn pack_used_refuses_a_row_count_that_disagrees_with_the_records() {
    let d = tempfile::tempdir().unwrap();
    br8n::pack::used::write(d.path(), &[1_700_000_000, 1_700_000_001]).unwrap();
    let err = br8n::pack::used::Reader::open(d.path(), 3).unwrap_err();
    assert!(err.to_string().contains("rows"), "got: {err}");
}

#[test]
fn a_pack_hydrates_each_rows_last_use_from_the_usage_map() {
    let d = tempfile::tempdir().unwrap();
    let rows: Vec<(Record, Vec<f32>)> = (0..8).map(|i| (rec(i), basis(8, i))).collect();
    let usage: br8n::usage::Map = [(
        rec(3).doc_id,
        br8n::usage::Usage {
            first_seen: 1_700_000_000,
            last_used: Some(1_750_000_000),
        },
    )]
    .into_iter()
    .collect();
    Pack::build_with_usage(
        d.path(),
        "qwen3-embedding:0.6b@512+qwen3",
        8,
        rows,
        &Default::default(),
        &Default::default(),
        &usage,
    )
    .unwrap();

    let p = Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8).unwrap();
    let hits = p.search(&basis(8, 3), 1, 64).unwrap();
    assert_eq!(hits[0].0.doc_id, rec(3).doc_id);
    assert_eq!(hits[0].0.last_used, Some(1_750_000_000));
    assert_eq!(p.last_used_of(&rec(3).chunk_id), Some(1_750_000_000));
    let other = p.search(&basis(8, 4), 1, 64).unwrap();
    assert_eq!(other[0].0.last_used, None);

    std::fs::remove_file(d.path().join(br8n::pack::used::USED_FILE)).unwrap();
    let older = Pack::open(d.path(), "qwen3-embedding:0.6b@512+qwen3", 8)
        .expect("a pack published before pack.used existed must still open");
    assert_eq!(
        older.search(&basis(8, 3), 1, 64).unwrap()[0].0.last_used,
        None
    );
}
