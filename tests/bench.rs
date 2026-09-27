use br8n::bench::{recall_at_k, separated_cleanly, suggest_threshold};

#[test]
fn recall_counts_a_query_as_hit_when_expected_uri_is_in_top_k() {
    let retrieved = vec![
        vec!["file:///a.md".to_string(), "file:///b.md".to_string()],
        vec!["file:///z.md".to_string()],
    ];
    let expected = vec!["file:///b.md".to_string(), "file:///y.md".to_string()];
    assert!((recall_at_k(&retrieved, &expected, 5) - 0.5).abs() < 1e-6);
}

#[test]
fn recall_counts_distinct_documents_not_chunk_positions() {
    // Retrieval returns chunks; a golden case names a document. Three chunks of
    // "x" are ONE document, so "target" is the second distinct document here —
    // even though it sits in the fourth chunk slot.
    //
    // The previous version of this test asserted 0.0 at k=2, encoding the
    // chunk-position reading. That reading made the metric move with candidate
    // count rather than with answer quality: asking the vector index for 20
    // neighbours instead of 5 returns the true nearest chunks, which cluster
    // inside one document, and scored 1.00 -> 0.40 on identical retrieval.
    let retrieved = vec![vec![
        "x".to_string(),
        "x".to_string(),
        "x".to_string(),
        "target".to_string(),
    ]];
    let expected = vec!["target".to_string()];

    assert_eq!(
        recall_at_k(&retrieved, &expected, 2),
        1.0,
        "target is the 2nd distinct document"
    );
    assert_eq!(
        recall_at_k(&retrieved, &expected, 1),
        0.0,
        "only document `x` is within the first 1 distinct document"
    );
}

#[test]
fn recall_still_misses_a_document_beyond_the_cutoff() {
    // The dedupe must not become "found anywhere in the list".
    let retrieved = vec![vec![
        "a".to_string(),
        "b".to_string(),
        "c".to_string(),
        "target".to_string(),
    ]];
    let expected = vec!["target".to_string()];

    assert_eq!(recall_at_k(&retrieved, &expected, 3), 0.0);
    assert_eq!(recall_at_k(&retrieved, &expected, 4), 1.0);
}

#[test]
fn recall_of_an_empty_set_is_zero_not_nan() {
    assert_eq!(recall_at_k(&[], &[], 5), 0.0);
}

#[test]
fn suggested_threshold_separates_relevant_from_irrelevant_scores() {
    let relevant = vec![0.80, 0.75, 0.72];
    let irrelevant = vec![0.30, 0.25, 0.41];
    let t = suggest_threshold(&relevant, &irrelevant);
    assert!(t > 0.41 && t < 0.72, "got {t}");
}

#[test]
fn threshold_falls_back_to_the_shipped_default_when_classes_overlap() {
    // The fallback was 0.55, described in the source as conservative. It is the
    // opposite: 0.55 sits BELOW the measured irrelevant floor of roughly
    // 0.565-0.669, so on an overlapping corpus `br8n bench` told the user to
    // lower their gate into the noise — and `config.rs` documents that exact
    // value as the one that admits unrelated notes.
    //
    // The previous assertion here was `(0.0..=1.0).contains(&t)`, which is true
    // of 0.55 and 0.70 alike. It could not have caught this.
    let t = suggest_threshold(&[0.5], &[0.9]);

    assert!(
        !separated_cleanly(&[0.5], &[0.9]),
        "this fixture must actually overlap, or the test proves nothing"
    );
    assert_eq!(
        t,
        br8n::config::Config::default().hook.threshold,
        "an overlapping corpus must keep the shipped default"
    );
    // 0.63 and not 0.70: the old literal encoded a floor derived from four
    // hand-checked queries. `br8n bench`'s trade-off curve, over 222 golden
    // cases at 650 documents, put the tier-1 answer floor at 0.623 and priced
    // 0.70 at 18% of answer cases. This guard's job is unchanged — the fallback
    // must not sink into the noise — and it tracks `default_threshold`'s own
    // lower bound in src/config.rs rather than restating a number.
    assert!(
        t >= 0.63,
        "the fallback must sit above the noise floor, got {t}"
    );
}

#[test]
fn clean_separation_is_reported_as_clean() {
    // The counterpart: when the classes DO separate, the number is a real
    // measurement and must be reported as one rather than as the default.
    let relevant = vec![0.80, 0.75, 0.72];
    let irrelevant = vec![0.30, 0.25, 0.41];
    assert!(separated_cleanly(&relevant, &irrelevant));
    assert_ne!(
        suggest_threshold(&relevant, &irrelevant),
        br8n::config::Config::default().hook.threshold,
        "a clean separation must yield the measured midpoint, not the default"
    );
}

#[test]
fn bench_report_roundtrips_beside_the_config() {
    // The dashboard reads the last bench run; printing and forgetting left it
    // nothing to show. The report lives beside the config so BR8N_CONFIG
    // relocates it together with everything else.
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded test; restored before returning.
    let prev = std::env::var("BR8N_CONFIG").ok();
    unsafe { std::env::set_var("BR8N_CONFIG", dir.path().join("config.toml")) };

    let tiers = vec![
        br8n::bench::BenchTier {
            tier: "fast".into(),
            recall_at_5: 0.82,
            p50_ms: 137,
            p95_ms: 161,
            absent: vec![],
        },
        br8n::bench::BenchTier {
            tier: "exhaustive".into(),
            recall_at_5: 0.89,
            p50_ms: 330,
            p95_ms: 381,
            absent: vec![],
        },
    ];
    let provenance = br8n::bench::Provenance {
        golden_sha256: br8n::bench::golden_sha256("[[case]]\nquery = \"q\"\n"),
        cases: 100,
        documents: Some(635),
        chunks: Some(33_648),
        memories: Some(0),
        no_graph: Some(false),
    };
    br8n::bench::write_report(&br8n::bench::BenchReport {
        provenance: Some(provenance.clone()),
        tiers: tiers.clone(),
    })
    .unwrap();

    assert_eq!(
        br8n::bench::report_path(),
        dir.path().join("bench-latest.json"),
        "report must sit beside the config file"
    );
    // The dashboard's accessor is unchanged and still yields the bare table.
    assert_eq!(br8n::bench::read_report(), Some(tiers.clone()));
    // ...and the provenance rides along, reachable through its own accessor.
    assert_eq!(
        br8n::bench::read_report_full(),
        Some(br8n::bench::BenchReport {
            provenance: Some(provenance),
            tiers,
        })
    );

    match prev {
        Some(v) => unsafe { std::env::set_var("BR8N_CONFIG", v) },
        None => unsafe { std::env::remove_var("BR8N_CONFIG") },
    }
}
// ---------------------------------------------------------------------------
// Case schema: several acceptable answers, and cases that must match nothing.
// ---------------------------------------------------------------------------

use br8n::bench::{
    missing_uris, parse_golden, positive_cases, recall_at_k_any, recommend_threshold,
    Recommendation, MIN_NEGATIVE_CASES,
};

#[test]
fn the_old_single_expect_shape_still_parses_and_scores_the_same() {
    // BACKWARD-COMPATIBILITY PIN. The live golden set is 196 cases in exactly
    // this shape, written before `expect_any` and `expect_none` existed. If
    // this ever needs editing to pass, that file stopped parsing.
    let golden = parse_golden(
        r#"
[[case]]
query = "why did the connection pooler drop sessions"
expect = "file:///notes/postgres.md"

[[case]]
query = "what did I decide about sourdough fermentation"
expect = "file:///notes/baking.md"
"#,
    )
    .expect("the old schema must parse unchanged");

    assert_eq!(golden.case.len(), 2);
    assert_eq!(
        golden.case[0].acceptable(),
        vec!["file:///notes/postgres.md".to_string()],
        "a lone `expect` is one acceptable document"
    );
    assert!(!golden.case[0].is_negative());

    // ...and scores exactly as it did: the second case's document is returned,
    // the first case's is not, so recall is 0.5 — the number the old
    // `recall_at_k` produced for this input.
    let expected: Vec<Vec<String>> = golden.case.iter().map(|c| c.acceptable()).collect();
    let retrieved = vec![
        vec!["file:///notes/redis.md".to_string()],
        vec!["file:///notes/baking.md".to_string()],
    ];
    assert!((recall_at_k_any(&retrieved, &expected, 5) - 0.5).abs() < 1e-6);
}

#[test]
fn a_multi_answer_case_counts_the_second_acceptable_document_as_a_hit() {
    // The defect this schema exists for: several documents genuinely answer the
    // query, retrieval returns one of them, and scoring against a single
    // `expect` called it a miss. Nothing but the SECOND listed document is in
    // the results here, so under the old code this case scored 0.
    let golden = parse_golden(
        r#"
[[case]]
query = "how do I rotate the signing keys"
expect_any = ["file:///notes/keys.md", "file:///notes/rotation-runbook.md"]
"#,
    )
    .expect("expect_any must parse");

    let case = &golden.case[0];
    assert_eq!(case.acceptable().len(), 2);

    let expected: Vec<Vec<String>> = golden.case.iter().map(|c| c.acceptable()).collect();
    let retrieved = vec![vec![
        "file:///notes/unrelated.md".to_string(),
        "file:///notes/rotation-runbook.md".to_string(),
    ]];
    assert_eq!(
        recall_at_k_any(&retrieved, &expected, 5),
        1.0,
        "the SECOND acceptable document was returned; that is a hit"
    );

    // And the distinct-DOCUMENT rule still bounds it: at k=1 only the unrelated
    // document is within the cutoff.
    assert_eq!(recall_at_k_any(&retrieved, &expected, 1), 0.0);
}

#[test]
fn expect_and_expect_any_combine_rather_than_one_shadowing_the_other() {
    let golden = parse_golden(
        r#"
[[case]]
query = "q"
expect = "file:///a.md"
expect_any = ["file:///b.md", "file:///a.md"]
"#,
    )
    .expect("both fields together must parse");
    assert_eq!(
        golden.case[0].acceptable(),
        vec!["file:///a.md".to_string(), "file:///b.md".to_string()],
        "`expect` first, duplicates collapsed"
    );
}

#[test]
fn a_negative_case_parses_is_exempt_from_the_uri_guard_and_scores_no_recall() {
    let golden = parse_golden(
        r#"
[[case]]
query = "what is the airspeed velocity of an unladen swallow"
expect_none = true

[[case]]
query = "why did the connection pooler drop sessions"
expect = "file:///notes/postgres.md"
"#,
    )
    .expect("expect_none must parse");

    let neg = &golden.case[0];
    assert!(neg.is_negative());
    assert!(
        neg.acceptable().is_empty(),
        "a negative case names no document"
    );

    // Exempt from the uri-existence guard: the corpus holds only postgres.md,
    // and the negative case contributes nothing to look up.
    let indexed = vec!["file:///notes/postgres.md".to_string()];
    assert!(
        missing_uris(&golden.case, &indexed).is_empty(),
        "a negative case must not be reported as a missing document"
    );

    // And it is excluded from recall rather than counted as a miss: one
    // positive case, found, is 1.00 — not 0.50.
    let positives = positive_cases(&golden.case);
    assert_eq!(positives.len(), 1, "only the positive case is scored");
    let expected: Vec<Vec<String>> = positives.iter().map(|c| c.acceptable()).collect();
    let retrieved = vec![vec!["file:///notes/postgres.md".to_string()]];
    assert_eq!(
        recall_at_k_any(&retrieved, &expected, 5),
        1.0,
        "the negative case must not be in the denominator"
    );
}

#[test]
fn the_uri_existence_guard_still_bails_on_a_typo_in_a_positive_case() {
    // The guard a typo'd uri otherwise defeats: it scores 0 forever and looks
    // exactly like a genuine retrieval miss. Exempting negative cases must not
    // have weakened it for anything that names a document.
    let golden = parse_golden(
        r#"
[[case]]
query = "nothing answers this"
expect_none = true

[[case]]
query = "typo in the single-expect form"
expect = "file:///notes/postgress.md"

[[case]]
query = "typo in the multi-answer form"
expect_any = ["file:///notes/postgres.md", "file:///notes/bakingg.md"]
"#,
    )
    .expect("fixture must parse");

    let indexed = vec!["file:///notes/postgres.md".to_string()];
    let missing = missing_uris(&golden.case, &indexed);
    assert_eq!(
        missing,
        vec![
            "file:///notes/postgress.md".to_string(),
            "file:///notes/bakingg.md".to_string()
        ],
        "every acceptable uri is checked, in both shapes"
    );
}

#[test]
fn a_case_that_names_nothing_and_asserts_nothing_is_rejected() {
    let err = parse_golden("[[case]]\nquery = \"q\"\n").unwrap_err();
    assert!(err.to_string().contains("names no document"), "got: {err}");
}

#[test]
fn a_case_cannot_be_both_negative_and_name_a_document() {
    let err =
        parse_golden("[[case]]\nquery = \"q\"\nexpect = \"file:///a.md\"\nexpect_none = true\n")
            .unwrap_err();
    assert!(err.to_string().contains("not both"), "got: {err}");
}

// ---------------------------------------------------------------------------
// Threshold calibration from negative cases.
// ---------------------------------------------------------------------------

#[test]
fn too_few_negative_cases_recommends_nothing_and_says_so() {
    let relevant = vec![0.80, 0.75, 0.72];
    let negative = vec![0.30];
    let keep = br8n::config::Config::default().hook.threshold;
    assert_eq!(
        recommend_threshold(&relevant, &negative, MIN_NEGATIVE_CASES - 1),
        Recommendation::TooFewNegatives {
            keep,
            negative_cases: MIN_NEGATIVE_CASES - 1
        },
        "a separator decided by a handful of maxima is not a measurement"
    );
}

#[test]
fn enough_separated_negative_cases_yield_the_measured_midpoint() {
    let relevant = vec![0.80, 0.75, 0.72];
    let negative = vec![0.30, 0.25, 0.41];
    match recommend_threshold(&relevant, &negative, MIN_NEGATIVE_CASES) {
        Recommendation::Calibrated {
            threshold,
            min_relevant,
            max_negative,
            negative_cases,
        } => {
            assert!((threshold - 0.565).abs() < 1e-5, "got {threshold}");
            assert!((min_relevant - 0.72).abs() < 1e-6);
            assert!((max_negative - 0.41).abs() < 1e-6);
            assert_eq!(negative_cases, MIN_NEGATIVE_CASES);
            assert_ne!(
                threshold,
                br8n::config::Config::default().hook.threshold,
                "a clean separation must be a measurement, not the default"
            );
        }
        other => panic!("expected a calibrated recommendation, got {other:?}"),
    }
}

#[test]
fn overlapping_negative_cases_keep_the_shipped_default() {
    let relevant = vec![0.62, 0.90];
    let negative = vec![0.30, 0.87];
    match recommend_threshold(&relevant, &negative, MIN_NEGATIVE_CASES + 3) {
        Recommendation::Overlapping { keep, .. } => assert_eq!(
            keep,
            br8n::config::Config::default().hook.threshold,
            "overlap must not move the gate"
        ),
        other => panic!("expected overlap, got {other:?}"),
    }
}

#[test]
fn enough_negative_cases_but_no_hits_at_all_recommends_nothing() {
    // Negative cases that returned no hits are the ideal outcome, and they
    // leave `max(negative)` undefined. An empty fold gives -inf, and
    // (min + -inf) / 2 is -inf: a "recommendation" of negative infinity.
    assert_eq!(
        recommend_threshold(&[0.8], &[], MIN_NEGATIVE_CASES),
        Recommendation::TooFewNegatives {
            keep: br8n::config::Config::default().hook.threshold,
            negative_cases: MIN_NEGATIVE_CASES
        }
    );
}

// ---------------------------------------------------------------------------
// Provenance: `br8n bench` refuses to compare across golden sets or corpora.
// ---------------------------------------------------------------------------

use br8n::bench::{
    comparability, golden_sha256, read_report_at, write_report_at, BenchReport, Change,
    Comparability, Provenance,
};

/// A three-tier table, so nothing here leans on a one-element fixture.
fn tiers() -> Vec<br8n::bench::BenchTier> {
    vec![
        br8n::bench::BenchTier {
            tier: "instant".into(),
            recall_at_5: 0.58,
            p50_ms: 53,
            p95_ms: 71,
            absent: vec![],
        },
        br8n::bench::BenchTier {
            tier: "fast".into(),
            recall_at_5: 0.81,
            p50_ms: 99,
            p95_ms: 141,
            absent: vec![],
        },
        br8n::bench::BenchTier {
            tier: "exhaustive".into(),
            recall_at_5: 0.90,
            p50_ms: 336,
            p95_ms: 402,
            absent: vec![],
        },
    ]
}

/// The provenance of the run every case below compares against.
fn baseline() -> Provenance {
    Provenance {
        golden_sha256: golden_sha256("[[case]]\nquery = \"a\"\nexpect = \"file:///a.md\"\n"),
        cases: 100,
        documents: Some(599),
        chunks: Some(31_204),
        memories: Some(12),
        no_graph: Some(false),
    }
}

fn previous(p: Provenance) -> BenchReport {
    BenchReport {
        provenance: Some(p),
        tiers: tiers(),
    }
}

#[test]
fn identical_provenance_compares_and_warns_about_nothing() {
    let prev = previous(baseline());
    let verdict = comparability(Some(&prev), &baseline());

    assert_eq!(verdict, Comparability::Comparable);
    assert_eq!(verdict.warning(), None, "nothing moved, so nothing to say");

    // The discriminating half. `warning() == None` on its own is also what a
    // gutted `comparability` that always answered `Comparable` would produce,
    // so this test is only worth anything if it shows the verdict CAN flip.
    // One document, out of 599, is the whole difference here.
    let moved = Provenance {
        documents: Some(600),
        ..baseline()
    };
    assert_ne!(
        comparability(Some(&prev), &moved),
        Comparability::Comparable,
        "a corpus that moved by one document is not the same corpus"
    );
    assert!(comparability(Some(&prev), &moved).warning().is_some());
}

#[test]
fn a_changed_golden_set_refuses_to_compare_and_names_it() {
    let prev = previous(baseline());
    let after_hash = golden_sha256("[[case]]\nquery = \"b\"\nexpect = \"file:///b.md\"\n");
    let current = Provenance {
        golden_sha256: after_hash.clone(),
        ..baseline()
    };

    let verdict = comparability(Some(&prev), &current);
    assert_eq!(
        verdict,
        Comparability::NotComparable(vec![Change::GoldenSet {
            before: baseline().golden_sha256,
            after: after_hash.clone(),
        }]),
        "the golden set moved and nothing else did"
    );

    let w = verdict.warning().expect("a changed golden set must warn");
    assert!(w.contains("NOT COMPARABLE"), "got:\n{w}");
    assert!(
        w.contains("golden set"),
        "the warning must name it; got:\n{w}"
    );
    assert!(
        w.contains(&baseline().golden_sha256[..12]) && w.contains(&after_hash[..12]),
        "both digests, so the reader can tell which run is which; got:\n{w}"
    );
}

#[test]
fn a_changed_case_count_refuses_to_compare_and_names_the_count() {
    // Case count cannot move without the file hash moving too, so this is the
    // shape a real run produces: both, together. The count is what tells the
    // reader WHAT changed — two digests alone say only "not the same file".
    let prev = previous(baseline());
    let current = Provenance {
        golden_sha256: golden_sha256(
            "[[case]]\nquery = \"a\"\nexpect = \"file:///a.md\"\n# more\n",
        ),
        cases: 120,
        ..baseline()
    };

    let verdict = comparability(Some(&prev), &current);
    let Comparability::NotComparable(changes) = &verdict else {
        panic!("expected a refusal, got {verdict:?}");
    };
    assert!(
        changes.contains(&Change::Cases {
            before: 100,
            after: 120
        }),
        "the count must be reported as a change of its own; got {changes:?}"
    );

    let w = verdict.warning().expect("a changed case count must warn");
    assert!(
        w.contains("golden cases      100 -> 120"),
        "old -> new, on its own line; got:\n{w}"
    );
}

#[test]
fn a_changed_document_count_refuses_to_compare_and_gives_the_direction() {
    let prev = previous(baseline());
    let current = Provenance {
        documents: Some(635),
        chunks: Some(33_648),
        ..baseline()
    };

    let verdict = comparability(Some(&prev), &current);
    assert_eq!(
        verdict,
        Comparability::NotComparable(vec![
            Change::Documents {
                before: 599,
                after: 635
            },
            Change::Chunks {
                before: 31_204,
                after: 33_648
            },
        ]),
        "documents and chunks are two changes, not one"
    );

    let w = verdict.warning().expect("a changed corpus must warn");
    assert!(
        w.contains("corpus documents  599 -> 635"),
        "old -> new, so the direction is in the block; got:\n{w}"
    );
    assert!(w.contains("corpus chunks     31204 -> 33648"), "got:\n{w}");
}

#[test]
fn a_chunk_count_that_moves_alone_is_still_a_refusal() {
    // Re-chunking moves the chunk count with the document count fixed. Checking
    // only `documents` would call this comparable, and the two numbers are
    // checked separately precisely so it cannot.
    let prev = previous(baseline());
    let current = Provenance {
        chunks: Some(33_648),
        ..baseline()
    };

    assert_eq!(
        comparability(Some(&prev), &current),
        Comparability::NotComparable(vec![Change::Chunks {
            before: 31_204,
            after: 33_648
        }]),
        "the same documents re-chunked are not the same corpus to score over"
    );
}

#[test]
fn a_changed_memory_count_refuses_to_compare_and_gives_the_direction() {
    let prev = previous(baseline());
    let current = Provenance {
        memories: Some(40),
        ..baseline()
    };

    let verdict = comparability(Some(&prev), &current);
    assert_eq!(
        verdict,
        Comparability::NotComparable(vec![Change::Memories {
            before: 12,
            after: 40,
        }]),
        "the memory count moved and nothing else did"
    );

    let w = verdict.warning().expect("a changed memory count must warn");
    assert!(
        w.contains("memory count      12 -> 40"),
        "old -> new, on its own line; got:\n{w}"
    );
}

#[test]
fn a_corpus_that_could_not_be_counted_is_not_reported_as_comparable() {
    // `count_documents` failing is not evidence that the corpus held still.
    // Folding the failure to 0 — the way the display path does — would make two
    // uncountable runs compare equal, which is the defect in reverse.
    let prev = previous(baseline());
    let current = Provenance {
        documents: None,
        chunks: None,
        ..baseline()
    };

    let verdict = comparability(Some(&prev), &current);
    assert_eq!(
        verdict,
        Comparability::NotComparable(vec![Change::CorpusUnknown])
    );
    let w = verdict.warning().expect("an unverifiable corpus must warn");
    assert!(w.contains("corpus size       unknown"), "got:\n{w}");

    // And the zero it must not silently become.
    let zeroed = Provenance {
        documents: Some(0),
        chunks: Some(0),
        ..baseline()
    };
    assert_ne!(
        comparability(Some(&prev), &zeroed),
        Comparability::Comparable,
        "an empty corpus is a changed corpus, not an unknown one"
    );
}

#[test]
fn no_previous_report_warns_about_nothing() {
    let verdict = comparability(None, &baseline());
    assert_eq!(verdict, Comparability::NoPrevious);
    assert_eq!(verdict.warning(), None);
}

#[test]
fn an_old_bare_array_report_reads_as_provenance_unknown_not_as_no_previous_run() {
    // Every machine that has ever run `br8n bench` has a bare JSON array on
    // disk. Failing to parse it would read as "no previous run", and that is
    // the ONE verdict that prints nothing at all — a too-strict parser would
    // wave through exactly the comparison this check exists to stop.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("bench-latest.json");
    std::fs::write(&p, serde_json::to_string_pretty(&tiers()).unwrap()).unwrap();

    let loaded = read_report_at(&p).expect("the pre-provenance shape must still parse");
    assert_eq!(loaded.tiers, tiers(), "the table survives the old shape");
    assert_eq!(
        loaded.provenance, None,
        "and nothing is known about what produced it"
    );

    let verdict = comparability(Some(&loaded), &baseline());
    assert_eq!(verdict, Comparability::PreviousProvenanceUnknown);
    assert_ne!(
        verdict,
        Comparability::NoPrevious,
        "a report that exists must not read as an absent one"
    );
    let w = verdict
        .warning()
        .expect("unknown provenance is not a licence to compare");
    assert!(w.contains("UNKNOWN"), "got:\n{w}");
    assert!(
        !w.contains("NOT COMPARABLE"),
        "unknown is not the same claim as incomparable; got:\n{w}"
    );
}

#[test]
fn the_new_shape_round_trips_through_the_shipped_codec() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("bench-latest.json");
    let report = previous(baseline());
    write_report_at(&p, &report).unwrap();
    assert_eq!(read_report_at(&p), Some(report));
}

#[test]
fn the_warning_stands_alone_when_pasted_somewhere_else() {
    // The wording is a requirement, not decoration: this rule was already
    // written down in CLAUDE.md and broken the same morning by someone quoting
    // two of these numbers side by side. A block that only makes sense directly
    // under the table it was printed with cannot survive that trip.
    let prev = previous(baseline());
    let current = Provenance {
        documents: Some(635),
        ..baseline()
    };
    let w = comparability(Some(&prev), &current).warning().unwrap();

    assert!(
        w.contains("br8n bench"),
        "the block must name what produced it; got:\n{w}"
    );
    assert!(
        w.contains("in either direction"),
        "it must foreclose reading a delta as a win OR a regression; got:\n{w}"
    );
    // No recall figure of THIS run appears in the block, so the block itself
    // cannot be pasted as a result. The three illustrative numbers it does
    // carry are labelled with the document counts they came from.
    for tier in tiers() {
        assert!(
            !w.contains(&format!("{:.2}", tier.recall_at_5)),
            "this run's recall must not appear inside the warning; got:\n{w}"
        );
    }
}

// ---------------------------------------------------------------------------
// The shape of the two classes, and the threshold trade-off curve.
// ---------------------------------------------------------------------------

use br8n::bench::{
    candidate_thresholds, cheapest_clean_gate, origin_breakdown, origin_of, percentile,
    recall_at_k_gated, spread, trade_off, trade_off_table, GateSamples, Origin, OriginSample,
};
use std::collections::HashSet;

fn ids(v: &[&str]) -> HashSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_percentile_is_a_value_some_hit_actually_had_not_an_interpolated_one() {
    // Nearest-rank, deliberately. A gate is set to a number, and every number
    // this report offers has to be one a hit really scored — an interpolated
    // p50 of 0.25 over this input is a relevance nothing was measured at.
    let v = [0.1f32, 0.2, 0.3, 0.4];
    let p50 = percentile(&v, 0.50).unwrap();

    assert!(
        (p50 - 0.2).abs() < 1e-6,
        "nearest-rank p50 of four values is the second, got {p50}"
    );
    assert!(
        v.contains(&p50),
        "every reported percentile must be an observed value; {p50} is not in {v:?}"
    );
    // The ends are the two figures `recommend_threshold` already prints, so
    // they must line up with it exactly.
    assert!((percentile(&v, 0.0).unwrap() - 0.1).abs() < 1e-6);
    assert!((percentile(&v, 1.0).unwrap() - 0.4).abs() < 1e-6);
    assert_eq!(
        percentile(&[], 0.5),
        None,
        "no hits is not a percentile of 0"
    );
}

#[test]
fn the_spread_shows_mass_that_the_two_extremes_hide() {
    // The whole reason this exists. `recommend_threshold` reports min and max;
    // here they are 0.10 and 0.99 while every other sample sits in 0.70-0.78,
    // so a reader given only the extremes would place the gate nowhere near
    // where the class actually lives.
    //
    // Fed UNSORTED, so the sort inside `spread` is load-bearing.
    let v = [
        0.75f32, 0.10, 0.72, 0.99, 0.70, 0.77, 0.73, 0.71, 0.78, 0.74, 0.76,
    ];
    let s = spread(&v).expect("eleven samples are a distribution");

    assert_eq!(s.n, 11);
    assert!((s.min - 0.10).abs() < 1e-6, "min {}", s.min);
    assert!((s.max - 0.99).abs() < 1e-6, "max {}", s.max);
    // The discriminating half: the quartiles must land inside the cluster the
    // extremes say nothing about.
    assert!((s.p25 - 0.71).abs() < 1e-6, "p25 {}", s.p25);
    assert!((s.p50 - 0.74).abs() < 1e-6, "p50 {}", s.p50);
    assert!((s.p75 - 0.77).abs() < 1e-6, "p75 {}", s.p75);
    assert!(
        s.p25 < s.p50 && s.p50 < s.p75,
        "the quartiles must be ordered: {s:?}"
    );
    assert_eq!(spread(&[]), None);
}

#[test]
fn the_curve_counts_a_boundary_hit_exactly_as_the_gate_does() {
    // `retrieve::run` gates with `hits.retain(|h| h.relevance >= t)`. A hit
    // sitting exactly ON the threshold is therefore ADMITTED, and an answer
    // exactly on it is NOT cut. Both sides of that boundary are here, because
    // a curve off by the boundary hits is a curve that prices a different gate
    // from the one the hook runs.
    let s = GateSamples {
        answer_hits: vec![0.70, 0.69],
        negative_hits: vec![0.70, 0.69],
        answer_case_best: vec![0.70],
        negative_case_best: vec![0.70],
        ranked: Vec::new(),
    };
    let t = trade_off(&s, &[], &[0.70]);
    let r = &t.rows[0];

    assert_eq!(
        r.negative_hits_admitted, 1,
        "a negative sitting exactly on the gate is admitted, not excluded"
    );
    assert_eq!(
        r.answer_hits_cut, 1,
        "only the answer BELOW the gate is cut; the one on it survives"
    );
    assert_eq!(
        r.negative_cases_injected, 1,
        "and the case-level count must use the same comparison"
    );
    assert_eq!(r.answer_cases_lost, 0);
}

#[test]
fn hits_and_cases_are_different_units_and_the_curve_reports_both() {
    // One positive case whose answer is spread over three chunks, two of them
    // weak. At 0.70 the gate cuts two HITS and loses no CASE — the answer is
    // still in the prompt. A curve that reported only hits would say this gate
    // cost 67% of the answers, and it costs none of them.
    //
    // The negative side is the mirror: one case with three admitted hits is
    // one injection, not three.
    let s = GateSamples {
        answer_hits: vec![0.50, 0.60, 0.90],
        negative_hits: vec![0.80, 0.85, 0.90],
        answer_case_best: vec![0.90],
        negative_case_best: vec![0.90],
        ranked: Vec::new(),
    };
    let t = trade_off(&s, &[], &[0.70]);
    let r = &t.rows[0];

    assert_eq!(
        r.answer_hits_cut, 2,
        "two of the three chunks are under 0.70"
    );
    assert_eq!(
        r.answer_cases_lost, 0,
        "but the case keeps its answer, so nothing was lost"
    );
    assert_eq!(r.negative_hits_admitted, 3);
    assert_eq!(
        r.negative_cases_injected, 1,
        "three admitted hits of one case are one injection"
    );
    assert_eq!((t.answer_hits, t.answer_cases), (3, 1));
    assert_eq!((t.negative_hits, t.negative_cases), (3, 1));
}

#[test]
fn the_recall_column_drops_sub_threshold_hits_before_the_top_five_are_counted() {
    // The column that makes the curve actionable: what a gate costs in the
    // metric the rest of the report is about.
    //
    // The answer is the SIXTH distinct document, so ungated it is outside
    // recall@5 entirely. Raise the gate past the five weak documents ahead of
    // it and it becomes the first — recall goes 0.00 -> 1.00 on the same
    // retrieval. A column that ignored its threshold argument would report
    // 0.00 for both.
    let ranked = vec![vec![
        ("file:///n1.md".to_string(), 0.60f32),
        ("file:///n2.md".to_string(), 0.61),
        ("file:///n3.md".to_string(), 0.62),
        ("file:///n4.md".to_string(), 0.63),
        ("file:///n5.md".to_string(), 0.64),
        ("file:///answer.md".to_string(), 0.90),
    ]];
    let expected = vec![vec!["file:///answer.md".to_string()]];

    assert_eq!(
        recall_at_k_gated(&ranked, &expected, 5, 0.60),
        0.0,
        "ungated, the answer is the sixth distinct document"
    );
    assert_eq!(
        recall_at_k_gated(&ranked, &expected, 5, 0.65),
        1.0,
        "gated, the five weak documents are gone and the answer is first"
    );
}

#[test]
fn the_recall_column_keeps_counting_distinct_documents_not_chunks() {
    // `recall@k` counts DOCUMENTS, and the gated column must not quietly
    // become a chunk count: four surviving chunks of one document are one
    // document, so the answer behind them is still within the top five.
    let ranked = vec![vec![
        ("file:///x.md".to_string(), 0.90f32),
        ("file:///x.md".to_string(), 0.89),
        ("file:///x.md".to_string(), 0.88),
        ("file:///x.md".to_string(), 0.87),
        ("file:///answer.md".to_string(), 0.86),
    ]];
    let expected = vec![vec!["file:///answer.md".to_string()]];
    assert_eq!(
        recall_at_k_gated(&ranked, &expected, 2, 0.50),
        1.0,
        "the answer is the second distinct document, whatever the chunk count"
    );
}

#[test]
fn the_cheapest_clean_gate_is_the_lowest_one_and_not_merely_a_clean_one() {
    // Two candidates admit nothing. The useful one is the LOWER, because every
    // step up the ladder costs answers — picking the highest clean gate would
    // pay for separation twice.
    let s = GateSamples {
        answer_hits: vec![0.60, 0.75, 0.95],
        negative_hits: vec![0.55],
        answer_case_best: vec![0.60, 0.75, 0.95],
        negative_case_best: vec![0.55],
        ranked: Vec::new(),
    };
    let t = trade_off(&s, &[], &[0.50, 0.60, 0.70]);

    // The fixture must genuinely offer a choice, or `find` and `rfind` agree
    // and this proves nothing.
    let clean: Vec<f32> = t
        .rows
        .iter()
        .filter(|r| r.negative_cases_injected == 0)
        .map(|r| r.threshold)
        .collect();
    assert_eq!(clean, vec![0.60, 0.70], "two clean candidates, not one");

    let picked = cheapest_clean_gate(&t).expect("0.60 already admits nothing");
    assert!((picked.threshold - 0.60).abs() < 1e-6, "got {picked:?}");
    assert_eq!(
        picked.answer_cases_lost, 0,
        "and the cheaper one is cheaper: 0.70 would lose an answer case"
    );

    // No clean gate exists at all when every candidate admits the negative.
    let none = trade_off(&s, &[], &[0.50]);
    assert!(cheapest_clean_gate(&none).is_none());
}

#[test]
fn a_chunk_two_stages_found_is_credited_to_the_one_that_measured_its_cosine() {
    // The precedence is the claim being made. A chunk vector search returned
    // had its cosine MEASURED by the search that returned it, whatever else
    // also found it; a chunk only BM25 found had one backfilled afterwards.
    // Reversing this would move exactly the hits the split exists to separate.
    let vector = ids(&["both", "vec_only"]);
    let keyword = ids(&["both", "kw_only", "kw_and_graph"]);
    let graph = ids(&["graph_only", "kw_and_graph"]);

    assert_eq!(origin_of("both", &vector, &keyword, &graph), Origin::Vector);
    assert_eq!(
        origin_of("vec_only", &vector, &keyword, &graph),
        Origin::Vector
    );
    assert_eq!(
        origin_of("kw_only", &vector, &keyword, &graph),
        Origin::Keyword
    );
    assert_eq!(
        origin_of("kw_and_graph", &vector, &keyword, &graph),
        Origin::Keyword,
        "keyword outranks graph for the same reason vector outranks keyword"
    );
    assert_eq!(
        origin_of("graph_only", &vector, &keyword, &graph),
        Origin::Graph
    );
    assert_eq!(
        origin_of("nowhere", &vector, &keyword, &graph),
        Origin::Unattributed,
        "a hit in no trace must be visible as unattributed, not folded into a real stage"
    );
}

#[test]
fn the_origin_breakdown_never_pools_the_two_classes() {
    // Answers and negatives sharing an origin must not share a row: the whole
    // question is whether the negatives arriving by one route score like the
    // answers arriving by the same route.
    let s = |relevance: f32, origin: Origin, negative: bool| OriginSample {
        relevance,
        origin,
        negative,
    };
    let samples = [
        s(0.90, Origin::Vector, false),
        s(0.80, Origin::Vector, false),
        s(0.70, Origin::Vector, false),
        s(0.60, Origin::Vector, true),
        s(0.50, Origin::Vector, true),
        s(0.65, Origin::Keyword, false),
    ];

    let answers = origin_breakdown(&samples, false);
    assert_eq!(
        answers.iter().map(|(o, _)| *o).collect::<Vec<_>>(),
        vec![Origin::Vector, Origin::Keyword],
        "in Origin::ALL order, and origins with no hits are skipped"
    );
    assert_eq!(
        answers[0].1.n, 3,
        "the two negatives must not be counted in"
    );
    assert!((answers[0].1.min - 0.70).abs() < 1e-6, "{:?}", answers[0].1);

    let negatives = origin_breakdown(&samples, true);
    assert_eq!(negatives.len(), 1, "the negatives produced no keyword hit");
    assert_eq!(negatives[0].1.n, 2);
    assert!(
        (negatives[0].1.max - 0.60).abs() < 1e-6,
        "{:?}",
        negatives[0].1
    );
}

#[test]
fn the_printed_curve_carries_both_denominators_on_every_row() {
    // A count with no denominator is not a rate, and this table is read to
    // choose a number. Every row must show what each count is out of.
    let s = GateSamples {
        answer_hits: vec![0.60, 0.80],
        negative_hits: vec![0.55, 0.75],
        answer_case_best: vec![0.80],
        negative_case_best: vec![0.75],
        ranked: Vec::new(),
    };
    let t = trade_off(&s, &[], &[0.70]);
    let out = trade_off_table(&t);

    assert!(out.contains("0.70"), "the candidate itself; got:\n{out}");
    assert!(
        out.contains("1/2") && out.contains("0/1"),
        "counts must be printed over their denominators; got:\n{out}"
    );
    assert!(
        out.lines().count() == 3,
        "two header lines and one row per candidate; got:\n{out}"
    );
}

#[test]
fn the_candidate_ladder_always_prices_the_gate_the_user_is_running() {
    let ladder = candidate_thresholds();
    let keep = br8n::config::Config::default().hook.threshold;

    assert!(
        ladder.iter().any(|t| (t - keep).abs() < 1e-6),
        "the shipped default {keep} must have a row; got {ladder:?}"
    );
    assert!(
        ladder.windows(2).all(|w| w[0] < w[1]),
        "ascending and free of duplicates; got {ladder:?}"
    );
    assert!(
        ladder.first().copied().unwrap() <= 0.50 && ladder.last().copied().unwrap() >= 0.90,
        "the ladder must reach past both classes; got {ladder:?}"
    );
}

use br8n::bench::AbsentRow;

/// `expect_absent` names documents that must NOT be injected. It is not
/// `expect_none`: the case has a right answer, and also a wrong one that must
/// stay out of the prompt.
#[test]
fn expect_absent_parses_alongside_a_normal_expectation() {
    let g = parse_golden(
        r#"
[[case]]
query = "what do we use to build software"
expect = "file:///d/0005.md"
expect_absent = ["file:///d/0004.md"]
"#,
    )
    .unwrap();
    assert_eq!(g.case.len(), 1);
    assert_eq!(
        g.case[0].expect_absent,
        vec!["file:///d/0004.md".to_string()]
    );
}

/// A negative case has no injected list to score, so pairing the two is a
/// contradiction — refused for the same reason `expect` + `expect_none` is.
#[test]
fn expect_absent_and_expect_none_together_are_refused() {
    let err = parse_golden(
        r#"
[[case]]
query = "nothing here"
expect_none = true
expect_absent = ["file:///d/0004.md"]
"#,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("expect_none") && err.contains("expect_absent"),
        "the error must name both keys so the user can find the line, got: {err}"
    );
}

/// A typo'd `expect_absent` uri used to bypass the uri-existence guard
/// entirely: it can never appear in a hit list, so it reads forever as
/// `rank: None, injected: false` — indistinguishable from the demotion the
/// instrument exists to detect actually working. `missing_uris` must catch it
/// exactly like a typo in `expect`/`expect_any`, without folding it into
/// `Case::acceptable()` (which would make recall count a document that must
/// NOT be retrieved).
#[test]
fn a_typo_in_expect_absent_is_reported_as_a_missing_uri() {
    let golden = parse_golden(
        r#"
[[case]]
query = "what do we use to build software"
expect = "file:///d/0005.md"
expect_absent = ["file:///d/0004-stale.md"]
"#,
    )
    .expect("fixture must parse");

    let indexed = vec!["file:///d/0005.md".to_string()];
    assert_eq!(
        missing_uris(&golden.case, &indexed),
        vec!["file:///d/0004-stale.md".to_string()],
        "the expect_absent uri must be checked against the corpus too"
    );

    // And it must not have leaked into the recall-scoring set: `acceptable()`
    // still names only the real answer, never the document that must be
    // absent from the prompt.
    assert_eq!(golden.case[0].acceptable(), vec!["file:///d/0005.md"]);
}

/// Option B: the RANK is what makes this instrument usable for choosing a
/// multiplier. A bare boolean saturates the moment the record clears the gate.
#[test]
fn an_absent_row_carries_both_the_flag_and_the_ungated_rank() {
    let r = AbsentRow {
        query: "q".into(),
        uri: "file:///d/0004.md".into(),
        injected: false,
        rank: Some(6),
    };
    assert!(!r.injected, "gated out");
    assert_eq!(
        r.rank,
        Some(6),
        "still retrieved at rank 6 ungated — that is the signal a boolean loses"
    );

    // `None` means NOT RETRIEVED AT ALL, which is a different fact from
    // `Some(n)` with `injected: false`, and must not be collapsed into it.
    let gone = AbsentRow {
        query: "q".into(),
        uri: "file:///d/0004.md".into(),
        injected: false,
        rank: None,
    };
    assert_ne!(r.rank, gone.rank);
}

#[test]
fn memory_cases_parse_beside_the_main_cases_and_default_to_none() {
    let toml = r#"
[[case]]
query = "why did we pick rust"
expect = "file:///notes/rust.md"

[[memory_case]]
kind = "fact"
text = "The notes vault lives at ~/notes/vault."
queries = ["where is the notes vault on disk"]
negatives = ["how do I bake sourdough"]
"#;
    let g = br8n::bench::parse_golden(toml).unwrap();
    assert_eq!(g.case.len(), 1);
    assert_eq!(g.memory_case.len(), 1);
    assert_eq!(
        g.memory_case[0].queries[0],
        "where is the notes vault on disk"
    );
    let plain =
        br8n::bench::parse_golden("[[case]]\nquery = \"q\"\nexpect = \"file:///a.md\"\n").unwrap();
    assert!(plain.memory_case.is_empty());
}

#[test]
fn the_memory_section_reports_hits_and_the_highest_negative() {
    let rows = vec![
        br8n::bench::MemoryOutcome {
            text: "vault".into(),
            query: "where is the vault".into(),
            hit: true,
            negative: false,
            relevance: 0.81,
        },
        br8n::bench::MemoryOutcome {
            text: "vault".into(),
            query: "how do I bake".into(),
            hit: false,
            negative: true,
            relevance: 0.58,
        },
        br8n::bench::MemoryOutcome {
            text: "tabs".into(),
            query: "indentation rule".into(),
            hit: false,
            negative: false,
            relevance: 0.61,
        },
    ];
    let s = br8n::bench::render_memory_section(&rows, 0.66);
    assert!(s.contains("memory cases (tier 1, hook gate 0.66)"));
    assert!(s.contains("1/2 queries retrieved their memory"));
    assert!(s.contains("highest negative relevance 0.580"));
    assert!(s.contains("indentation rule"), "misses are listed by query");
}

#[test]
fn the_no_graph_ablation_clears_graph_at_every_tier_and_changes_nothing_else() {
    for tier in 0..=4u8 {
        let with = br8n::config::Profile::tier(tier);
        let without = br8n::bench::without_graph(br8n::config::Profile::tier(tier));

        assert!(
            without.graph.is_none(),
            "tier {tier} must have no graph expansion"
        );

        let mut expected = with.clone();
        expected.graph = None;
        assert_eq!(
            without, expected,
            "tier {tier}: --no-graph must change graph and nothing else"
        );
    }
}

#[test]
fn a_no_graph_run_compared_against_a_graph_on_run_is_not_comparable() {
    let prev = previous(Provenance {
        no_graph: Some(false),
        ..baseline()
    });
    let current = Provenance {
        no_graph: Some(true),
        ..baseline()
    };

    let verdict = comparability(Some(&prev), &current);
    assert_eq!(
        verdict,
        Comparability::NotComparable(vec![Change::GraphAblation {
            before: false,
            after: true,
        }]),
        "a graph-on baseline compared against a --no-graph run must be flagged"
    );
    let w = verdict
        .warning()
        .expect("a graph-ablation change must warn");
    assert!(w.contains("--no-graph"), "got:\n{w}");
}

#[test]
fn a_no_graph_run_compared_against_a_pre_flag_report_is_not_comparable() {
    let prev = previous(Provenance {
        no_graph: None,
        ..baseline()
    });
    let current = Provenance {
        no_graph: Some(true),
        ..baseline()
    };

    let verdict = comparability(Some(&prev), &current);
    assert_eq!(
        verdict,
        Comparability::NotComparable(vec![Change::GraphAblation {
            before: false,
            after: true,
        }]),
        "a pre-flag report (necessarily graph-on) compared against a --no-graph \
         run must be flagged, not certified comparable"
    );
}

mod synthetic_pack {
    use br8n::bench::synthetic::{self, Query, Spec};
    use br8n::config::{Config, Profile};
    use std::collections::BTreeMap;
    use std::path::Path;

    const CHUNKS: usize = 2_000;

    fn pack_files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let path = e.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                (name, std::fs::read(&path).unwrap())
            })
            .collect()
    }

    fn build_and_ask(seed: u64, probe: &Query) -> (BTreeMap<String, Vec<u8>>, Vec<String>) {
        let cfg = Config::default();
        let dims = cfg.embed.dimensions;
        let model_id = synthetic::configured_model_id(&cfg).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let corpus = synthetic::generate(&Spec {
            chunks: CHUNKS,
            seed,
            dims,
        });
        synthetic::build_pack(dir.path(), &model_id, dims, corpus.rows).unwrap();
        let retriever =
            synthetic::hook_retriever(&cfg, dir.path(), &model_id, std::slice::from_ref(probe))
                .unwrap();
        let top10: Vec<String> = retriever
            .search(&probe.text, &Profile::tier(1))
            .unwrap()
            .into_iter()
            .take(10)
            .map(|h| h.chunk_id)
            .collect();
        (pack_files(dir.path()), top10)
    }

    fn probe_for(seed: u64) -> Query {
        synthetic::generate(&Spec {
            chunks: CHUNKS,
            seed,
            dims: Config::default().embed.dimensions,
        })
        .queries
        .remove(0)
    }

    #[test]
    fn the_same_seed_builds_the_same_pack_and_answers_the_same_and_another_seed_does_not() {
        let probe = probe_for(7);
        assert_eq!(probe, probe_for(7), "seed 7 drew two different queries");
        assert_ne!(probe, probe_for(8), "seed 8 drew seed 7's query");
        let (first_pack, first_top10) = build_and_ask(7, &probe);
        let (second_pack, second_top10) = build_and_ask(7, &probe);
        let (other_pack, other_top10) = build_and_ask(8, &probe);

        assert_eq!(first_top10.len(), 10);
        assert!(first_pack.contains_key("pack.manifest"));
        assert!(first_pack.contains_key("pack.vec"));
        assert!(
            first_pack == second_pack,
            "two builds from seed 7 differ in {:?}",
            first_pack
                .keys()
                .filter(|k| first_pack.get(*k) != second_pack.get(*k))
                .collect::<Vec<_>>()
        );
        assert_eq!(first_top10, second_top10);

        assert!(first_pack != other_pack, "seed 8 built seed 7's pack");
        assert_ne!(first_top10, other_top10);
    }

    #[test]
    fn reuse_answers_the_same_as_the_building_run_and_reports_no_build_time() {
        let cfg = Config::default();
        let model_id = synthetic::configured_model_id(&cfg).unwrap();
        let seed = 11;
        let dir = tempfile::tempdir().unwrap();

        let build_report = synthetic::run(&cfg, CHUNKS, seed, Some(dir.path())).unwrap();
        assert!(build_report.pack_build_ms.is_some());

        let probe = probe_for(seed);
        let after_build =
            synthetic::hook_retriever(&cfg, dir.path(), &model_id, std::slice::from_ref(&probe))
                .unwrap();
        let top10_after_build: Vec<String> = after_build
            .search(&probe.text, &Profile::tier(1))
            .unwrap()
            .into_iter()
            .take(10)
            .map(|h| h.chunk_id)
            .collect();

        let reuse_report = synthetic::run_reuse(&cfg, CHUNKS, seed, dir.path()).unwrap();
        assert!(reuse_report.pack_build_ms.is_none());
        assert_eq!(reuse_report.chunks, build_report.chunks);
        assert_eq!(reuse_report.seed, build_report.seed);

        let after_reuse =
            synthetic::hook_retriever(&cfg, dir.path(), &model_id, std::slice::from_ref(&probe))
                .unwrap();
        let top10_after_reuse: Vec<String> = after_reuse
            .search(&probe.text, &Profile::tier(1))
            .unwrap()
            .into_iter()
            .take(10)
            .map(|h| h.chunk_id)
            .collect();

        assert_eq!(top10_after_build, top10_after_reuse);
    }

    #[test]
    fn reuse_asks_the_queries_the_build_wrote_instead_of_regenerating_them() {
        let cfg = Config::default();
        let dir = tempfile::tempdir().unwrap();
        let build_report = synthetic::run(&cfg, CHUNKS, 13, Some(dir.path())).unwrap();
        assert!(build_report
            .tiers
            .iter()
            .all(|t| t.queries == synthetic::QUERY_COUNT));

        let queries_path = dir.path().join(synthetic::QUERIES_FILE);
        let mut written: Vec<Query> =
            serde_json::from_slice(&std::fs::read(&queries_path).unwrap()).unwrap();
        assert_eq!(written.len(), synthetic::QUERY_COUNT);
        written.truncate(3);
        std::fs::write(&queries_path, serde_json::to_vec(&written).unwrap()).unwrap();

        let reuse_report = synthetic::run_reuse(&cfg, CHUNKS, 13, dir.path()).unwrap();
        assert!(
            reuse_report.tiers.iter().all(|t| t.queries == 3),
            "reuse timed {:?} queries, not the 3 left in {}",
            reuse_report
                .tiers
                .iter()
                .map(|t| t.queries)
                .collect::<Vec<_>>(),
            synthetic::QUERIES_FILE
        );
        assert_eq!(
            reuse_report.pack_bytes, build_report.pack_bytes,
            "the queries file is not part of the pack's size"
        );
    }

    #[test]
    fn reuse_refuses_a_pack_built_with_a_different_seed_or_chunk_count() {
        let cfg = Config::default();
        let seed = 3;
        let dir = tempfile::tempdir().unwrap();
        synthetic::run(&cfg, CHUNKS, seed, Some(dir.path())).unwrap();

        let wrong_seed = synthetic::run_reuse(&cfg, CHUNKS, seed + 1, dir.path())
            .unwrap_err()
            .to_string();
        assert!(wrong_seed.contains(&seed.to_string()), "{wrong_seed}");

        let wrong_chunks = synthetic::run_reuse(&cfg, CHUNKS + 1, seed, dir.path())
            .unwrap_err()
            .to_string();
        assert!(wrong_chunks.contains(&CHUNKS.to_string()), "{wrong_chunks}");
    }

    #[test]
    fn reuse_flag_round_trips_through_the_cli_with_a_null_build_time() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("pack");
        assert_cmd::Command::cargo_bin("br8n")
            .unwrap()
            .env("BR8N_CONFIG", tmp.path().join("config.toml"))
            .env("BR8N_DB", tmp.path().join("db"))
            .args(["bench", "--synthetic", "40", "--seed", "5", "--out"])
            .arg(&out)
            .assert()
            .success();

        let assert = assert_cmd::Command::cargo_bin("br8n")
            .unwrap()
            .env("BR8N_CONFIG", tmp.path().join("config.toml"))
            .env("BR8N_DB", tmp.path().join("db"))
            .args(["bench", "--synthetic", "40", "--seed", "5", "--reuse"])
            .arg(&out)
            .args(["--json"])
            .assert()
            .success();
        let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
        let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert!(report["pack_build_ms"].is_null(), "{stdout}");
        assert_eq!(report["chunks"], 40);
        assert_eq!(report["seed"], 5);

        let mismatched = assert_cmd::Command::cargo_bin("br8n")
            .unwrap()
            .env("BR8N_CONFIG", tmp.path().join("config.toml"))
            .env("BR8N_DB", tmp.path().join("db"))
            .args(["bench", "--synthetic", "40", "--seed", "6", "--reuse"])
            .arg(&out)
            .assert()
            .failure();
        let stderr = String::from_utf8_lossy(&mismatched.get_output().stderr).into_owned();
        assert!(stderr.contains("5"), "{stderr}");
    }

    #[test]
    fn out_refuses_a_directory_that_already_holds_something() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("occupied");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("keep.me"), b"x").unwrap();
        let assert = assert_cmd::Command::cargo_bin("br8n")
            .unwrap()
            .env("BR8N_CONFIG", tmp.path().join("config.toml"))
            .env("BR8N_DB", tmp.path().join("db"))
            .args(["bench", "--synthetic", "20", "--out"])
            .arg(&out)
            .assert()
            .failure();
        let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
        assert!(stderr.contains("is not empty"), "{stderr}");
        assert_eq!(std::fs::read(out.join("keep.me")).unwrap(), b"x");
    }
}
