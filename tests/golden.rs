//! `br8n golden` — pure functions over fixtures. No network, no Ollama, no
//! live index.
//!
//! Every test here was mutation-tested: the exact line it claims to pin was
//! broken, the test was RUN, its failure captured, and the source restored.
//! Where a test has more than one assertion, the comment says which one is
//! load-bearing, because the codebase has shipped tests whose surviving
//! assertion also passed with the feature deleted.

use br8n::bench::{golden_path, parse_golden, Case, MIN_NEGATIVE_CASES};
use br8n::golden::{
    blank_queries, check_cases, choose_seeds, contradictory_cases, duplicate_queries, fatal_errors,
    init_path, load_cases, starter_toml, title_echoes, title_overlap, unnamed_cases, verbatim_run,
    CorpusView, Doc, Problem, Report, Severity, MIN_TITLE_TERMS, SEED_CASES, TITLE_ECHO_THRESHOLD,
    VERBATIM_RUN,
};
use std::collections::HashMap;

fn case(query: &str, expect: &str) -> Case {
    Case {
        query: query.to_string(),
        expect: Some(expect.to_string()),
        expect_any: Vec::new(),
        expect_none: false,
        expect_absent: Vec::new(),
    }
}

fn negative(query: &str) -> Case {
    Case {
        query: query.to_string(),
        expect: None,
        expect_any: Vec::new(),
        expect_none: true,
        expect_absent: Vec::new(),
    }
}

fn doc(uri: &str, title: &str) -> Doc {
    Doc {
        uri: uri.to_string(),
        title: title.to_string(),
    }
}

fn titles(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(u, t)| (u.to_string(), t.to_string()))
        .collect()
}

// ---------------------------------------------------------------------------
// `init`: the file it writes must not score as a golden set.
// ---------------------------------------------------------------------------

/// THE decision this command turns on.
///
/// A seeded case written live, with `query = ""` for the user to fill in, is a
/// VALID case: `Case::validate` asks only whether a case names a document or
/// declares itself negative. And `golden_path()` prefers the file beside the
/// config over the repo fixture, so the instant `init` wrote such a file
/// `br8n bench` would start scoring 25 empty queries and printing a recall
/// number for them. Commented out, the same file parses to zero cases and
/// `bench` refuses it by name.
#[test]
fn the_starter_file_parses_as_valid_toml_and_holds_zero_live_cases() {
    let seeds = vec![
        doc("file:///notes/a.md", "Alpha"),
        doc("file:///notes/b.md", "Beta"),
    ];
    let text = starter_toml(&seeds);

    // Load-bearing. Both halves: it must PARSE (a starter file that is not
    // valid TOML fails at step 3 of its own instructions), and it must parse
    // to NOTHING.
    let parsed = parse_golden(&text).expect("the starter file must be valid TOML");
    assert!(
        parsed.case.is_empty(),
        "the starter file scored {} live case(s); every seeded case must be commented \
         out, or `br8n bench` measures empty queries",
        parsed.case.len()
    );
}

#[test]
fn the_starter_file_carries_the_real_uri_and_title_of_every_seed() {
    // Seeding real uris is the entire point: a hand-typed uri that does not
    // resolve scores 0 forever and looks exactly like a retrieval miss.
    let seeds = vec![
        doc("file:///Users/x/notes/postgres.md", "Connection pooling"),
        doc("file:///Users/x/notes/baking.md", "Sourdough"),
    ];
    let text = starter_toml(&seeds);
    for d in &seeds {
        assert!(
            text.contains(&d.uri),
            "seeded uri `{}` is missing from the starter file",
            d.uri
        );
        // The title is printed so the user can write a question they would ask
        // having FORGOTTEN it — the seed is useless without knowing what the
        // document is.
        assert!(
            text.contains(&d.title),
            "seeded title `{}` is missing from the starter file",
            d.title
        );
    }
}

#[test]
fn the_starter_file_stubs_exactly_the_negative_floor() {
    let text = starter_toml(&[doc("file:///a.md", "A")]);
    // Counted on the stub delimiter, not on `expect_none = true` anywhere in
    // the file — the header explains the three case shapes and uses that line
    // as an example, so a naive `matches()` counts six and passes for the wrong
    // reason.
    assert_eq!(
        text.matches("# --- negative ").count(),
        MIN_NEGATIVE_CASES,
        "the negative stub count must come from MIN_NEGATIVE_CASES, not a literal"
    );
    assert_eq!(
        text.lines()
            .filter(|l| l.trim() == "# expect_none = true")
            .count(),
        MIN_NEGATIVE_CASES,
        "every negative stub must be a completable case, not just a heading"
    );
}

#[test]
fn the_starter_file_states_the_rules_that_decide_whether_it_measures_anything() {
    let text = starter_toml(&[doc("file:///a.md", "A")]);
    for rule in [
        "distinct DOCUMENTS",            // recall counts documents, not chunks
        "MUST NOT ECHO DOCUMENT TITLES", // the rule nothing used to enforce
        "GENUINELY ABSENT",              // what a negative case needs
        "expect_any",                    // the three shapes
        "expect_none",
    ] {
        assert!(
            text.contains(rule),
            "the starter file no longer states `{rule}`"
        );
    }
}

/// Every line of a seeded stub, with its comment marker removed — what the
/// file's own step 1 ("uncomment a case below and write the query") produces.
///
/// Only the stub lines: the header's worked examples are indented by two more
/// spaces, so they do not match, which is checked by the case count in the test
/// below.
fn uncomment_stubs(starter: &str) -> String {
    starter
        .lines()
        .filter_map(|l| l.strip_prefix("# "))
        .filter(|l| {
            l.starts_with("[[case]]")
                || l.starts_with("query = ")
                || l.starts_with("expect = ")
                || l.starts_with("expect_any = ")
                || l.starts_with("expect_none = ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A uri is written into the file as a TOML string, and quoting it is not
/// optional.
///
/// Nothing else in this file can catch a broken quote, and that is the point:
/// the seeds are written COMMENTED OUT, so an unescaped `"` inside a comment is
/// still just a comment. The starter file parses, the tests pass, and the fault
/// surfaces later — when the user does exactly what the file told them to do
/// and `br8n bench` refuses the whole set with a syntax error on a line they
/// did not write.
#[test]
fn a_seeded_stub_parses_once_uncommented_however_the_uri_is_spelt() {
    let uri = "file:///Users/x/notes/a \"quoted\" \\ path.md";
    let text = starter_toml(&[doc(uri, "Quoting")]);

    let live = uncomment_stubs(&text);
    let parsed = parse_golden(&live)
        .unwrap_or_else(|e| panic!("uncommented stubs must parse: {e:#}\n---\n{live}\n---"));

    // The seeded stub plus the negative stubs, and nothing from the header's
    // worked examples.
    assert_eq!(parsed.case.len(), 1 + MIN_NEGATIVE_CASES);
    // Load-bearing: byte for byte. A uri that parses but comes back different
    // scores 0 forever and looks exactly like a retrieval miss.
    assert_eq!(parsed.case[0].expect.as_deref(), Some(uri));
}

/// The section that only exists when there is nothing to seed.
#[test]
fn a_starter_file_with_no_seeds_says_why_it_has_none() {
    let unseeded = starter_toml(&[]);
    // A starter file with no real uris is close to useless — every case in it
    // has to be hand-typed, which is the cost `init` exists to remove — so the
    // failure cannot be a footnote.
    assert!(
        unseeded.contains("NO DOCUMENTS WERE SEEDED"),
        "an unseeded starter file must say so in the file, not only on stderr"
    );
    assert!(
        unseeded.contains("br8n golden init --force"),
        "and must say how to fix it once the index is built"
    );
    // Load-bearing counterpart: the section is CONDITIONAL. Without this, a
    // file that always carried it passes the assertions above.
    assert!(!starter_toml(&[doc("file:///a.md", "A")]).contains("NO DOCUMENTS WERE SEEDED"));
}

#[test]
fn seeds_are_spread_across_the_corpus_rather_than_taken_from_the_front() {
    // Twelve documents in three directories. Sorting a corpus by uri groups it
    // by folder, so the first five of a sorted list are five documents from ONE
    // folder — a golden set drawn from one corner of the corpus.
    //
    // The fixture is chosen so that THREE different implementations disagree:
    //   correct   `i * len / n`   -> 0, 2, 4, 7, 9
    //   take-first                -> 0, 1, 2, 3, 4
    //   `i * (len / n)`           -> 0, 2, 4, 6, 8
    // Any of the three passes a test that only asserts "five distinct docs".
    let docs: Vec<Doc> = [
        "file:///a/01.md",
        "file:///a/02.md",
        "file:///a/03.md",
        "file:///a/04.md",
        "file:///a/05.md",
        "file:///b/01.md",
        "file:///b/02.md",
        "file:///b/03.md",
        "file:///b/04.md",
        "file:///c/01.md",
        "file:///c/02.md",
        "file:///c/03.md",
    ]
    .iter()
    .map(|u| doc(u, "t"))
    .collect();

    // Fed in an order that is not the answer, so a function that ignored the
    // sort and returned its input would be caught too.
    let mut shuffled = docs.clone();
    shuffled.reverse();

    let got: Vec<String> = choose_seeds(&shuffled, 5)
        .into_iter()
        .map(|d| d.uri)
        .collect();

    // Load-bearing.
    assert_eq!(
        got,
        vec![
            "file:///a/01.md".to_string(),
            "file:///a/03.md".to_string(),
            "file:///a/05.md".to_string(),
            "file:///b/03.md".to_string(),
            "file:///c/01.md".to_string(),
        ]
    );

    // The property the exact list exists to deliver, stated so a future edit
    // knows what it may not break. Passes for `i * (len / n)` as well, so it is
    // documentation rather than the assertion doing the work.
    let dirs: std::collections::HashSet<&str> = got.iter().map(|u| &u[8..10]).collect();
    assert_eq!(dirs.len(), 3, "seeds must span the corpus, not one folder");
}

#[test]
fn a_corpus_smaller_than_the_ask_seeds_all_of_it_exactly_once() {
    let docs = vec![
        doc("file:///c.md", "C"),
        doc("file:///a.md", "A"),
        doc("file:///b.md", "B"),
    ];
    let got: Vec<String> = choose_seeds(&docs, SEED_CASES)
        .into_iter()
        .map(|d| d.uri)
        .collect();
    assert_eq!(
        got,
        vec![
            "file:///a.md".to_string(),
            "file:///b.md".to_string(),
            "file:///c.md".to_string()
        ],
        "every document, sorted, and none twice"
    );
}

#[test]
fn the_tightest_stride_still_seeds_distinct_documents() {
    // len = n + 1 is the narrowest case where striding happens at all: the step
    // is 1.04 documents. An off-by-one here would seed the same document twice
    // and quietly shrink the set.
    let docs: Vec<Doc> = (0..SEED_CASES + 1)
        .map(|i| doc(&format!("file:///{i:03}.md"), "t"))
        .collect();
    let got = choose_seeds(&docs, SEED_CASES);
    let distinct: std::collections::HashSet<&String> = got.iter().map(|d| &d.uri).collect();
    assert_eq!(got.len(), SEED_CASES);
    assert_eq!(distinct.len(), SEED_CASES, "a document was seeded twice");
}

// ---------------------------------------------------------------------------
// The title-echo check.
// ---------------------------------------------------------------------------

/// Exactly ON the line: three of the title's five content terms.
///
/// The boundary case is the point. A fixture that overlaps by 0.2 or 1.0 gives
/// the same verdict under any threshold between them and proves nothing about
/// the one that shipped.
#[test]
fn a_query_repeating_three_of_five_title_terms_sits_exactly_on_the_line() {
    let title = "Postgres connection pooler timeout diagnosis";
    let query = "why did the connection pooler timeout";

    let overlap = title_overlap(query, title);
    // Asserting the VALUE also asserts the denominator: if the analyzer ever
    // collapsed two of those five terms into one stem, this reads 0.75 and
    // fails rather than silently re-scaling the check.
    assert!(
        (overlap - 0.6).abs() < 1e-6,
        "expected exactly 3/5, got {overlap}"
    );

    let cases = vec![case(query, "file:///pg.md")];
    let echoes = title_echoes(
        &cases,
        &titles(&[("file:///pg.md", title)]),
        TITLE_ECHO_THRESHOLD,
    );
    assert_eq!(
        echoes.len(),
        1,
        "0.60 is at the {TITLE_ECHO_THRESHOLD} line"
    );
    assert_eq!(echoes[0].case, 1);
    // Load-bearing, and the count above is NOT: this query is also a
    // three-term verbatim run, so the run trigger flags it whatever the ratio
    // does, and `echoes.len() == 1` survives changing `>=` to `>`.
    // `over_the_line` is the ratio's own verdict, and only it pins the
    // comparison.
    assert!(
        echoes[0].over_the_line,
        "exactly on the line must count as over it: the comparison is `>=`"
    );
}

/// Just UNDER: four of seven terms, 0.571 — closer to the line than to
/// anything else, and it must not be flagged.
///
/// RE-FIXTURED. This test used the query `connection pooler timeout rollback`,
/// which repeats three of the title's terms CONTIGUOUSLY and in the title's own
/// order — a literal copy of the middle of the title — and asserted that the
/// silence was correct. It was not correct; it was the false negative the
/// verbatim-run trigger now catches, and an adversarial review found this
/// assertion pinning it as intended behaviour. The boundary property the test
/// was written for is real, so the fixture is rewritten to isolate it: the same
/// four of seven terms, scattered, so the ratio is the only thing under
/// examination.
#[test]
fn a_query_repeating_four_of_seven_scattered_title_terms_stays_under_the_line() {
    let title = "Postgres connection pooler timeout diagnosis rollback runbook";
    let query = "rollback steps after the pooler dropped connections at timeout";

    let overlap = title_overlap(query, title);
    assert!(
        (overlap - 4.0 / 7.0).abs() < 1e-6,
        "expected exactly 4/7, got {overlap}"
    );
    // The half that makes this fixture, rather than the old one, a test of the
    // ratio alone.
    assert_eq!(
        verbatim_run(query, title),
        1,
        "the shared terms must be scattered, or the run trigger decides this"
    );

    let cases = vec![case(query, "file:///pg.md")];
    let echoes = title_echoes(
        &cases,
        &titles(&[("file:///pg.md", title)]),
        TITLE_ECHO_THRESHOLD,
    );
    // Load-bearing: 0.571 is below 0.60. Lowering the threshold to 0.5 flags
    // this and fails here.
    assert!(
        echoes.is_empty(),
        "0.571 is under the line and must not warn"
    );
}

/// The false negative the ratio cannot see: a verbatim copy of a long title's
/// opening words.
///
/// Measured live by an adversarial review against a real index — this exact
/// shape produced no warning at all, and it is the most literal form of "the
/// query was written by reading the title".
#[test]
fn a_verbatim_prefix_of_a_long_title_is_an_echo_the_ratio_misses() {
    let title = "Postgres connection pooler timeout diagnosis rollback runbook";
    let query = "postgres connection pooler timeout";

    // The ratio is UNDER the line and stays under it — this is not a test that
    // 0.6 was set wrong.
    let overlap = title_overlap(query, title);
    assert!(
        (overlap - 4.0 / 7.0).abs() < 1e-6,
        "expected exactly 4/7, got {overlap}"
    );
    assert!(overlap < TITLE_ECHO_THRESHOLD);

    let cases = vec![case(query, "file:///pg.md")];
    let echoes = title_echoes(
        &cases,
        &titles(&[("file:///pg.md", title)]),
        TITLE_ECHO_THRESHOLD,
    );
    // Load-bearing: flagged, and flagged by the RUN rather than by the ratio.
    // Asserting only `len() == 1` would also pass if the threshold had been
    // lowered to 0.5, which is the change this test exists to rule out.
    assert_eq!(echoes.len(), 1, "a verbatim four-term copy must warn");
    assert!(
        !echoes[0].over_the_line,
        "the ratio is 0.571, under the line"
    );
    assert_eq!(echoes[0].run, 4);
}

/// Both sides of the run boundary, on the same title.
#[test]
fn three_contiguous_title_terms_warn_and_two_do_not() {
    let long = "Postgres connection pooler timeout diagnosis rollback runbook";
    let short = "Connection pooling in production Postgres";

    // Exactly at `VERBATIM_RUN`, and at a ratio far below the line — so this
    // fails if the run trigger is raised to 4 or deleted, and it cannot be
    // satisfied by the ratio.
    assert_eq!(verbatim_run("postgres connection pooler", long), 3);
    let three = title_echoes(
        &[case("postgres connection pooler", "file:///pg.md")],
        &titles(&[("file:///pg.md", long)]),
        TITLE_ECHO_THRESHOLD,
    );
    assert_eq!(
        three.len(),
        1,
        "{VERBATIM_RUN} contiguous terms is the line"
    );
    assert!(
        !three[0].over_the_line,
        "3/7 = 0.43 is under the ratio line"
    );

    // One term below it. `connection pooling` is two contiguous terms of that
    // title AND simply the name of the subject; the two are indistinguishable,
    // so the check stays silent. This is a deliberate hole, not an oversight —
    // lowering `VERBATIM_RUN` to 2 fails here.
    assert_eq!(verbatim_run("connection pooling", short), 2);
    let two = title_echoes(
        &[case("connection pooling", "file:///pool.md")],
        &titles(&[("file:///pool.md", short)]),
        TITLE_ECHO_THRESHOLD,
    );
    assert!(
        two.is_empty(),
        "two terms is a subject name, not a copied title: {two:?}"
    );
}

/// The false POSITIVE class, measured live by the same review: every one-word
/// title flags every query that names its subject.
#[test]
fn a_one_term_title_is_never_an_echo_however_much_of_it_the_query_repeats() {
    let title = "Postgres";
    let query = "why did our postgres server run out of connections one night";

    // The ratio is 1.00 and there is nothing wrong with the ratio: one of one
    // term is the whole title. The query is not written from the title, and it
    // could not have avoided the word.
    assert!((title_overlap(query, title) - 1.0).abs() < 1e-6);

    let echoes = title_echoes(
        &[case(query, "file:///pg.md")],
        &titles(&[("file:///pg.md", title)]),
        TITLE_ECHO_THRESHOLD,
    );
    // Load-bearing: `MIN_TITLE_TERMS`. Lowering it to 1 makes this warn — with
    // advice ("ask it as if you had forgotten the title") that cannot be
    // followed, which is what makes it a false positive rather than a strict
    // one.
    assert!(
        echoes.is_empty(),
        "one content term is below MIN_TITLE_TERMS = {MIN_TITLE_TERMS}: {echoes:?}"
    );
}

/// The counterpart: the floor is on the TITLE's length, not on the overlap.
#[test]
fn the_shortest_title_the_ratio_still_judges_is_min_title_terms_long() {
    // Exactly at the floor: three content terms, all three repeated.
    let title = "Sourdough starter hydration";
    let query = "sourdough starter hydration";
    let echoes = title_echoes(
        &[case(query, "file:///bake.md")],
        &titles(&[("file:///bake.md", title)]),
        TITLE_ECHO_THRESHOLD,
    );
    // Load-bearing: raising MIN_TITLE_TERMS to 4 leaves `over_the_line` false
    // here (the run trigger would still flag the case, so the count alone does
    // not catch it).
    assert_eq!(echoes.len(), 1);
    assert!(
        echoes[0].over_the_line,
        "three terms is at MIN_TITLE_TERMS = {MIN_TITLE_TERMS} and the ratio applies"
    );
}

#[test]
fn overlap_is_measured_against_the_title_not_against_the_query() {
    // A wordy query that contains the WHOLE title is a title echo. Denominating
    // on the query (or on the union) makes the score fall as the query gets
    // longer, which is exactly backwards: here it would read 3/9 = 0.33 and
    // wave through a query that repeats every word of its target's title.
    let title = "Sourdough starter hydration";
    let query = "how much water did I settle on for the sourdough starter hydration";

    let overlap = title_overlap(query, title);
    assert!(
        (overlap - 1.0).abs() < 1e-6,
        "the whole title survives into the query; got {overlap}"
    );

    let cases = vec![case(query, "file:///bake.md")];
    let echoes = title_echoes(
        &cases,
        &titles(&[("file:///bake.md", title)]),
        TITLE_ECHO_THRESHOLD,
    );
    assert_eq!(echoes.len(), 1);
    assert!(echoes[0].over_the_line);
}

#[test]
fn a_reworded_echo_is_still_an_echo_because_terms_are_stemmed() {
    // `repotting` and `repot` are one term to BM25 and to any reader; a plain
    // word-split scores this 2/3 = 0.67 and warns for the wrong reason, or 1/2
    // on the two-term title this fixture used to use and stays silent. Using
    // the pack's own analyzer is what makes the number mean "the fraction of
    // the title's terms the query would match on".
    //
    // Three content terms, because `Repotting a monstera` has two and the ratio
    // no longer judges a title that short — the old fixture asserted a value
    // that no longer decided anything.
    let title = "Repotting a monstera in spring";
    let query = "how do I repot my monstera in spring";

    let overlap = title_overlap(query, title);
    assert!(
        (overlap - 1.0).abs() < 1e-6,
        "stemmed, the query repeats the whole title; got {overlap}"
    );
    let echoes = title_echoes(
        &[case(query, "file:///plant.md")],
        &titles(&[("file:///plant.md", title)]),
        TITLE_ECHO_THRESHOLD,
    );
    assert_eq!(
        echoes.len(),
        1,
        "and it reaches the report, not just the ratio"
    );
    assert!(echoes[0].over_the_line);
}

#[test]
fn a_title_made_only_of_stopwords_is_not_a_total_match() {
    // No content terms at all. Dividing by zero here would produce NaN, and an
    // "empty intersection over an empty set" reading would produce 1.0 and warn
    // about every case pointing at such a document.
    assert_eq!(title_overlap("anything at all", "How to do it"), 0.0);
}

#[test]
fn only_the_cases_own_target_is_checked_for_an_echo() {
    // Overlapping some unrelated document's title is not this defect: the case
    // is not scored against that document, so nothing is being tested by string
    // match. Checking every title would warn about half a cross-linked vault.
    let cases = vec![case(
        "connection pooler timeout diagnosis postgres",
        "file:///bake.md",
    )];
    let echoes = title_echoes(
        &cases,
        &titles(&[
            ("file:///bake.md", "Sourdough starter hydration"),
            (
                "file:///pg.md",
                "Postgres connection pooler timeout diagnosis",
            ),
        ]),
        TITLE_ECHO_THRESHOLD,
    );
    assert!(
        echoes.is_empty(),
        "the echoed title belongs to a document this case is not scored against"
    );
}

#[test]
fn a_case_that_is_both_negative_and_named_is_not_also_reported_as_an_echo() {
    // A contradictory case names a document AND declares that nothing should
    // match, so `Case::acceptable()` is non-empty for it. It is already an
    // error; adding a title-echo warning about the target it was never going to
    // be scored against is noise on top of a fault.
    let bad = Case {
        query: "connection pooler timeout".to_string(),
        expect: Some("file:///pg.md".to_string()),
        expect_any: Vec::new(),
        expect_none: true,
        expect_absent: Vec::new(),
    };
    let echoes = title_echoes(
        &[bad],
        &titles(&[("file:///pg.md", "Connection pooler timeout")]),
        TITLE_ECHO_THRESHOLD,
    );
    assert!(echoes.is_empty());
}

// ---------------------------------------------------------------------------
// Duplicates, blanks, contradictions.
// ---------------------------------------------------------------------------

#[test]
fn queries_differing_only_in_case_or_padding_are_one_query() {
    // Retrieval does not distinguish them, so counting them twice weights one
    // question twice inside recall while the case count suggests breadth.
    let cases = vec![
        case("Why did the pooler drop sessions", "file:///a.md"),
        case("how do I rotate the deploy key", "file:///b.md"),
        case("  why did the POOLER drop sessions  ", "file:///c.md"),
        case("how do I rotate the deploy key", "file:///d.md"),
    ];
    let dups = duplicate_queries(&cases);
    assert_eq!(dups.len(), 2, "two distinct queries are each written twice");
    // Load-bearing: the pair is (1, 3), which only holds if the comparison
    // trims and lowercases.
    assert_eq!(dups[0].1, vec![1, 3]);
    assert_eq!(dups[1].1, vec![2, 4]);
}

#[test]
fn blank_queries_are_reported_as_blank_and_not_again_as_duplicates() {
    // Five unfilled stubs are all "the same query" as each other. Reporting
    // them twice buries the one line that says what to do.
    let cases = vec![
        case("", "file:///a.md"),
        case("   ", "file:///b.md"),
        case("a real question about pooling", "file:///c.md"),
    ];
    assert_eq!(blank_queries(&cases), vec![1, 2]);
    assert!(
        duplicate_queries(&cases).is_empty(),
        "blank queries must not also be reported as duplicates of each other"
    );
}

#[test]
fn every_contradictory_case_is_reported_not_only_the_first() {
    // `parse_golden` bails on the FIRST malformed case, so a user fixing them
    // one at a time re-runs once per fault. `check` re-parses without
    // validation to hand back the whole list at once.
    let text = r#"
[[case]]
query = "one"
expect = "file:///a.md"
expect_none = true

[[case]]
query = "two"
expect = "file:///b.md"

[[case]]
query = "three"
expect_any = ["file:///c.md"]
expect_none = true
"#;
    assert!(
        parse_golden(text).is_err(),
        "the shipped parser must still refuse this file"
    );

    let (cases, refusal) = load_cases(text).expect("structurally valid TOML must still load");
    assert!(
        refusal.is_some(),
        "the shipped parser's refusal must be carried through, not swallowed — \
         `br8n bench` will refuse this file and `check` must say so"
    );
    // Load-bearing: BOTH of them, by number.
    assert_eq!(contradictory_cases(&cases), vec![1, 3]);
}

#[test]
fn a_genuine_toml_syntax_error_is_still_an_error() {
    // The fallback exists to enumerate per-case faults, not to accept anything.
    assert!(load_cases("[[case]\nquery = ").is_err());
}

// ---------------------------------------------------------------------------
// The whole check, including the path where it cannot check.
// ---------------------------------------------------------------------------

fn enumerated() -> CorpusView {
    CorpusView::Enumerated(vec![
        doc("file:///pg.md", "Connection pooling in production"),
        doc("file:///bake.md", "Sourdough"),
    ])
}

#[test]
fn a_uri_that_is_not_in_the_index_is_an_error() {
    // The counterpart of the skip test below. Without this, a `check` that
    // never looked at uris at all would pass that one.
    let cases = vec![case("something about pooling", "file:///typo.md")];
    let report = check_cases(&cases, &enumerated());
    assert_eq!(report.errors(), 1);
    assert!(
        report
            .problems
            .iter()
            .any(|p| p.text.contains("file:///typo.md")),
        "the error must name the uri: {:?}",
        report.problems
    );
    assert!(report.skipped.is_empty(), "nothing was skipped here");
}

/// The hazard this project cares about, in one test.
///
/// When the corpus cannot be enumerated the uri check and the title check
/// cannot run. Folding that into "no problems" reports a check that never ran
/// as passed, which is the house failure mode: a broken pipeline and a clean
/// result look identical.
#[test]
fn a_corpus_that_cannot_be_enumerated_is_reported_as_skipped_never_as_passed() {
    let cases = vec![
        // This uri is not in ANY corpus. With the store reachable it is an
        // error; with the store unreachable it is unknown, and unknown is not
        // the same as fine.
        case("something about pooling", "file:///typo.md"),
        negative("what is our parental leave policy"),
        negative("how do I claim expenses"),
        negative("which desk booking system do we use"),
        negative("what is the refund window"),
        negative("who administers payroll"),
    ];
    let report = check_cases(
        &cases,
        &CorpusView::Unavailable {
            reason: "no index at /nowhere/db".to_string(),
        },
    );

    // Load-bearing 1: the skip is recorded, and it names why.
    assert_eq!(report.skipped.len(), 2, "uri check and title check");
    assert!(
        report
            .skipped
            .iter()
            .any(|s| s.contains("no index at /nowhere/db")),
        "the reason the check could not run must survive into the report: {:?}",
        report.skipped
    );

    // Load-bearing 2: the unknown uri is NOT reported as present, and NOT
    // reported as missing either.
    assert!(
        !report
            .problems
            .iter()
            .any(|p| p.text.contains("file:///typo.md")),
        "a store failure must not be dressed up as a user's typo"
    );
    assert_eq!(report.errors(), 0);

    // Load-bearing 3: the summary a reader acts on must not read as a pass.
    let summary = report.summary();
    assert!(
        summary.contains("NOT a clean bill of health"),
        "summary was `{summary}`"
    );
    assert!(
        report.render().contains("SKIPPED"),
        "the rendered report must show the skip"
    );
}

#[test]
fn the_negative_floor_warns_below_it_and_is_silent_at_it() {
    let uri = "file:///pg.md";
    let mut below: Vec<Case> = (0..MIN_NEGATIVE_CASES - 1)
        .map(|i| negative(&format!("absent subject {i}")))
        .collect();
    below.push(case("how are sessions dropped under load", uri));

    let report = check_cases(&below, &enumerated());
    assert_eq!(report.warnings(), 1, "{:?}", report.problems);
    assert_eq!(report.errors(), 0);
    let w = &report.problems[0];
    assert_eq!(w.severity, Severity::Warning);
    // Say what is LOST, not just that a number is small.
    assert!(w.text.contains("TooFewNegatives"), "{}", w.text);
    assert!(w.text.contains("[hook] threshold"), "{}", w.text);

    let mut at: Vec<Case> = (0..MIN_NEGATIVE_CASES)
        .map(|i| negative(&format!("absent subject {i}")))
        .collect();
    at.push(case("how are sessions dropped under load", uri));
    // Load-bearing: exactly AT the floor is not a warning. `<` not `<=`.
    assert_eq!(
        check_cases(&at, &enumerated()).warnings(),
        0,
        "MIN_NEGATIVE_CASES is a floor that counts as met"
    );
}

#[test]
fn a_title_echo_is_a_warning_and_does_not_fail_the_command() {
    // Judgement is the user's: a query may legitimately share most of a short
    // title's words. The overlap is reported so they can decide, and the exit
    // code is not spent on it.
    let mut cases: Vec<Case> = (0..MIN_NEGATIVE_CASES)
        .map(|i| negative(&format!("absent subject {i}")))
        .collect();
    cases.push(case("connection pooling in production", "file:///pg.md"));

    let report = check_cases(&cases, &enumerated());
    assert_eq!(report.errors(), 0, "{:?}", report.problems);
    assert_eq!(report.warnings(), 1);
    let w = report
        .problems
        .iter()
        .find(|p| p.severity == Severity::Warning)
        .unwrap();
    // The overlap itself has to be in the message — a warning that says
    // "possible echo" without the number cannot be judged.
    assert!(w.text.contains("100%"), "{}", w.text);
    assert!(
        w.text.contains("Connection pooling in production"),
        "{}",
        w.text
    );
}

#[test]
fn a_clean_set_reports_nothing_and_skips_nothing() {
    // The control. Without it every check above could be satisfied by a
    // function that reports a problem for everything.
    let mut cases: Vec<Case> = (0..MIN_NEGATIVE_CASES)
        .map(|i| negative(&format!("absent subject {i}")))
        .collect();
    cases.push(case("why do sessions vanish under load", "file:///pg.md"));
    cases.push(case(
        "what did I settle on for the long ferment",
        "file:///bake.md",
    ));

    let report = check_cases(&cases, &enumerated());
    assert_eq!(report.errors(), 0, "{:?}", report.problems);
    assert_eq!(report.warnings(), 0, "{:?}", report.problems);
    assert!(report.skipped.is_empty());
    assert_eq!(report.summary(), "0 errors, 0 warnings");
    assert_eq!(report.cases, MIN_NEGATIVE_CASES + 2);
    assert_eq!(report.negatives, MIN_NEGATIVE_CASES);
}

#[test]
fn an_empty_set_is_not_also_lectured_about_negative_cases() {
    // A file with no cases has one problem — it has no cases — and the SKIPPED
    // note says that. "only 0 `expect_none` cases" underneath is a property of
    // the empty set rather than a second fault, and it is the first thing a
    // user sees after `br8n golden init`.
    let report = check_cases(&[], &enumerated());
    assert_eq!(report.warnings(), 0, "{:?}", report.problems);
    assert_eq!(report.errors(), 0);
    assert_eq!(report.cases, 0);
}

/// A file with no cases is NOT a clean file. It is a file that measures
/// nothing.
///
/// Found by an adversarial review, live and twice: a `[[cases]]` typo (plural,
/// so serde parses an unrelated table and ignores it) and an unrelated
/// `config.toml` reached through `BR8N_GOLDEN` both printed
/// `0 errors, 0 warnings` and exited 0. The printed paragraph then named the
/// benign cause — "if `br8n golden init` just wrote it, that is expected" —
/// and sent a user with a misspelt header or a wrong path looking for a
/// commented-out stub that was not there.
#[test]
fn a_file_with_no_cases_is_recorded_as_checking_nothing() {
    let report = check_cases(&[], &enumerated());

    // Load-bearing 1: recorded at all. `Report.skipped` exists precisely to
    // say "this is not a clean bill of health", and the zero-case path did not
    // use it.
    assert_eq!(report.skipped.len(), 1, "{:?}", report.skipped);

    // Load-bearing 2: the summary — the last line, and the only line most
    // readers act on — must not read as a pass.
    let summary = report.summary();
    assert!(
        summary.contains("NOT a clean bill of health"),
        "summary was `{summary}`"
    );

    // Load-bearing 3: all three causes, because the message that names only the
    // benign one is worse than no message. A user is sent to the wrong place
    // and finds nothing there.
    let note = &report.skipped[0];
    assert!(note.contains("commented out"), "{note}"); // init just wrote it
    assert!(note.contains("[[cases]]"), "{note}"); // the plural typo
    assert!(note.contains("BR8N_GOLDEN"), "{note}"); // the wrong file entirely
    assert!(
        report.render().contains("SKIPPED"),
        "the rendered report must show it"
    );
}

// ---------------------------------------------------------------------------
// The wiring. Every check below has a unit test above; these pin the block in
// `check_cases` that turns each one into a reported Problem.
//
// Written after an adversarial review deleted three of those blocks whole —
// blank, duplicate and contradictory — and watched the suite pass 24/24 with
// each one gone. Three of the six advertised checks could be removed with the
// tests green, which is this codebase's documented failure mode: a suite that
// certifies a feature it does not exercise.
// ---------------------------------------------------------------------------

/// A set that is clean except for the one fault under test, so the error count
/// is attributable. `MIN_NEGATIVE_CASES` negatives keep the floor warning
/// silent, and their queries name nothing that could echo a title.
fn clean_bed() -> Vec<Case> {
    (0..MIN_NEGATIVE_CASES)
        .map(|i| negative(&format!("absent subject {i}")))
        .collect()
}

#[test]
fn a_blank_query_is_reported_by_check_and_not_only_by_blank_queries() {
    let mut cases = clean_bed();
    cases.push(case("why do sessions vanish under load", "file:///pg.md"));
    cases.push(case("   ", "file:///bake.md"));

    let report = check_cases(&cases, &enumerated());
    // Load-bearing: it is an ERROR, it names the case by number, and it says
    // what the damage is. An empty query is embedded and scored like any other
    // — on a small corpus it scores as a HIT — so the recall figure moves for a
    // reason that has nothing to do with retrieval.
    assert_eq!(report.errors(), 1, "{:?}", report.problems);
    let p = report
        .problems
        .iter()
        .find(|p| p.severity == Severity::Error)
        .unwrap();
    assert!(p.text.starts_with("case 7:"), "{}", p.text);
    assert!(p.text.contains("the query is empty"), "{}", p.text);
    assert_eq!(
        blank_queries(&cases),
        vec![7],
        "the unit check still agrees"
    );
}

#[test]
fn duplicate_queries_are_reported_by_check_and_not_only_by_duplicate_queries() {
    let mut cases = clean_bed();
    cases.push(case("why do sessions vanish under load", "file:///pg.md"));
    cases.push(case(
        "  Why do sessions VANISH under load ",
        "file:///bake.md",
    ));

    let report = check_cases(&cases, &enumerated());
    assert_eq!(report.errors(), 1, "{:?}", report.problems);
    let p = report
        .problems
        .iter()
        .find(|p| p.severity == Severity::Error)
        .unwrap();
    // Load-bearing: BOTH case numbers, which is the whole use of the message —
    // a user cannot delete a duplicate they cannot find.
    assert!(p.text.contains("cases 6, 7"), "{}", p.text);
}

#[test]
fn contradictory_cases_are_reported_by_check_and_not_only_by_contradictory_cases() {
    let mut cases = clean_bed();
    cases.push(Case {
        query: "how are sessions dropped under load".to_string(),
        expect: Some("file:///pg.md".to_string()),
        expect_any: Vec::new(),
        expect_none: true,
        expect_absent: Vec::new(),
    });
    cases.push(Case {
        query: "what did I settle on for the long ferment".to_string(),
        expect: None,
        expect_any: vec!["file:///bake.md".to_string()],
        expect_none: true,
        expect_absent: Vec::new(),
    });

    let report = check_cases(&cases, &enumerated());
    // Load-bearing: BOTH of them. `parse_golden` bails on the first, and the
    // reason `check` re-parses without validation is to hand back the whole
    // list — which is worth nothing if `check_cases` does not print it.
    assert_eq!(report.errors(), 2, "{:?}", report.problems);
    let texts: Vec<&str> = report.problems.iter().map(|p| p.text.as_str()).collect();
    assert!(texts.iter().any(|t| t.starts_with("case 6 ")), "{texts:?}");
    assert!(texts.iter().any(|t| t.starts_with("case 7 ")), "{texts:?}");
    assert_eq!(contradictory_cases(&cases), vec![6, 7]);
}

/// `Case::validate`'s OTHER arm, which `check` had no check for at all.
///
/// The symptom was a self-contradicting command: `0 errors, 0 warnings` as the
/// last line of the report, then a non-zero exit saying the file "has 1
/// error(s) — see above", pointing at a report containing none. And only the
/// FIRST offender was ever named, by `parse_golden` — a file of five
/// half-edited stubs reported one.
#[test]
fn a_case_that_names_no_document_is_an_error_and_every_one_is_named() {
    let text = r#"
[[case]]
query = "why do sessions vanish under load"

[[case]]
query = "what did I settle on for the long ferment"
expect = "file:///bake.md"

[[case]]
query = "how do I rotate the deploy key"
"#;
    // The shipped parser refuses this file, on the first offender only.
    let refusal = format!("{:#}", parse_golden(text).unwrap_err());
    assert!(refusal.contains("case 1"), "{refusal}");
    assert!(!refusal.contains("case 3"), "{refusal}");

    let (cases, carried) = load_cases(text).expect("structurally valid TOML must still load");
    assert!(carried.is_some());
    assert_eq!(unnamed_cases(&cases), vec![1, 3]);

    let report = check_cases(&cases, &enumerated());
    // Load-bearing: two ERRORS, one per offending case, each named. The
    // negative-floor warning is expected alongside and is not an error.
    assert_eq!(report.errors(), 2, "{:?}", report.problems);
    let texts: Vec<&str> = report
        .problems
        .iter()
        .filter(|p| p.severity == Severity::Error)
        .map(|p| p.text.as_str())
        .collect();
    assert!(texts.iter().any(|t| t.starts_with("case 1 ")), "{texts:?}");
    assert!(texts.iter().any(|t| t.starts_with("case 3 ")), "{texts:?}");
    // It must say how to fix it: the three ways to complete a stub.
    assert!(texts[0].contains("expect_none = true"), "{}", texts[0]);
}

/// A blank query and an unnamed document are two faults, not one, and a stub
/// that has both gets told both.
#[test]
fn a_bare_uncommented_stub_is_reported_as_both_faults() {
    let cases = vec![Case {
        query: String::new(),
        expect: None,
        expect_any: Vec::new(),
        expect_none: false,
        expect_absent: Vec::new(),
    }];
    let report = check_cases(&cases, &enumerated());
    assert_eq!(report.errors(), 2, "{:?}", report.problems);
    assert_eq!(blank_queries(&cases), vec![1]);
    assert_eq!(unnamed_cases(&cases), vec![1]);
}

// ---------------------------------------------------------------------------
// The exit decision.
// ---------------------------------------------------------------------------

#[test]
fn the_parsers_own_refusal_fails_the_command_even_with_a_spotless_report() {
    let clean = Report {
        cases: 10,
        ..Default::default()
    };
    // Load-bearing: `check` exits non-zero when `parse_golden` refused the
    // file, whatever this module's own checks found. `br8n bench` will not run
    // on it, so `golden check` must not say it is fine.
    assert_eq!(
        fatal_errors(&clean, Some("case 4 (`x`) names no document.")),
        Some(1)
    );
    // The control, without which the assertion above is satisfied by a function
    // that always fails.
    assert_eq!(fatal_errors(&clean, None), None);

    let with_errors = Report {
        cases: 10,
        problems: vec![
            Problem {
                severity: Severity::Error,
                text: "one".into(),
            },
            Problem {
                severity: Severity::Error,
                text: "two".into(),
            },
            Problem {
                severity: Severity::Warning,
                text: "not counted".into(),
            },
        ],
        ..Default::default()
    };
    // Errors are counted, warnings are not: a title echo is a judgement call
    // and must not spend the exit code.
    assert_eq!(fatal_errors(&with_errors, None), Some(2));
    assert_eq!(fatal_errors(&with_errors, Some("refused")), Some(2));
}

/// A skipped check does not fail the command, and that is a decision.
#[test]
fn a_skipped_check_does_not_fail_the_command_but_does_deny_the_all_clear() {
    let skipped = Report {
        cases: 0,
        skipped: vec!["nothing was checked".to_string()],
        ..Default::default()
    };
    assert_eq!(fatal_errors(&skipped, None), None);
    assert!(skipped.summary().contains("NOT a clean bill of health"));
}

// ---------------------------------------------------------------------------
// Where `init` writes.
// ---------------------------------------------------------------------------

/// `init_path()` is not `golden_path()`, and the difference is the checked-in
/// fixture in this very repository.
///
/// `golden_path()` — where `bench` READS from — falls back to the repo's
/// `tests/golden.toml` when no golden set exists beside the config. An `init`
/// that wrote where `bench` reads would therefore overwrite that fixture with
/// the developer's own corpus on the first run inside this checkout.
#[test]
fn init_writes_beside_the_config_and_never_over_the_repo_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    let prev_golden = std::env::var("BR8N_GOLDEN").ok();
    let prev_cfg = std::env::var("BR8N_CONFIG").ok();
    unsafe { std::env::remove_var("BR8N_GOLDEN") };
    unsafe { std::env::set_var("BR8N_CONFIG", &cfg) };

    // Read both before restoring, and assert after: a panic here must not leak
    // process-global env into the rest of this binary.
    let read_from = golden_path();
    let write_to = init_path();

    if let Some(v) = prev_golden {
        unsafe { std::env::set_var("BR8N_GOLDEN", v) }
    }
    match prev_cfg {
        Some(v) => unsafe { std::env::set_var("BR8N_CONFIG", v) },
        None => unsafe { std::env::remove_var("BR8N_CONFIG") },
    }

    // The stakes, stated as an assertion: the file `golden_path` falls back to
    // is real and is in git.
    assert_eq!(read_from, std::path::Path::new("tests/golden.toml"));
    assert!(
        read_from.exists(),
        "the repo fixture `init` must not clobber is missing"
    );
    // Load-bearing: `init` writes beside the CONFIG.
    assert_eq!(write_to, dir.path().join("golden.toml"));
    assert_ne!(write_to, read_from);
}
