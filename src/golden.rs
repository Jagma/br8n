//! `br8n golden` — build a golden set out of your own index, and check it.
//!
//! `br8n bench` is the only objective signal this project has for whether a
//! change to retrieval helped or hurt, and it is useless without a golden set.
//! The barrier to writing one is not the schema — it is that every case must
//! name a uri that resolves in the USER'S index, so a hundred cases means a
//! hundred hand-typed paths, each of which scores 0 forever if it is wrong and
//! looks exactly like a retrieval miss while doing it.
//!
//! `init` therefore seeds from `Store::all_doc_keys`: real uris, real titles,
//! nothing to type. `check` is the other half — the rules that decide whether a
//! set measures retrieval at all live in prose in CLAUDE.md and nothing has
//! ever enforced them.
//!
//! Everything here is deliberately split into pure functions over `&[Case]`
//! plus one thin IO shell, because the interesting failures (a query that
//! echoes its own target's title, a corpus that could not be enumerated) are
//! exactly the ones that cannot be reached from a test that needs a live store.

use crate::bench::{golden_path, missing_uris, parse_golden, Case, Golden, MIN_NEGATIVE_CASES};
use crate::config::Config;
use crate::pack::analyze::analyze;
use crate::store::Store;
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// The corpus, as this module needs to see it.
// ---------------------------------------------------------------------------

/// One indexed document: what a golden case can name, and what it is called.
///
/// The title is carried because the title-echo check needs it and
/// `all_doc_uris` does not return it — that method is `(doc_id, uri)`, not
/// `(uri, title)`. `all_doc_keys` is `(doc_id, title, uri)` and is the one
/// query that answers both halves of `check` at once.
#[derive(Debug, Clone, PartialEq)]
pub struct Doc {
    pub uri: String,
    pub title: String,
}

/// Whether the corpus could be enumerated, and if not, why not.
///
/// This type exists to make the skip path unrepresentable-as-success. Folding a
/// store failure into an empty document list would make `missing_uris` report
/// EVERY case as missing — a store failure wearing the costume of a user error
/// — and folding it into "no problems" would report a check that never ran as
/// passed. That second one is the house failure mode (silent degradation) in
/// its purest form, so the absent case carries its reason and `check_cases`
/// records it as SKIPPED rather than as either verdict.
#[derive(Debug, Clone)]
pub enum CorpusView {
    Enumerated(Vec<Doc>),
    Unavailable { reason: String },
}

impl CorpusView {
    fn docs(&self) -> Option<&[Doc]> {
        match self {
            CorpusView::Enumerated(d) => Some(d),
            CorpusView::Unavailable { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Problems.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// The set is measuring something other than what it claims. Exit non-zero.
    Error,
    /// Judgement call, reported with the evidence so the user can make it.
    Warning,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub severity: Severity,
    /// First line, and any continuation lines already indented by the builder.
    pub text: String,
}

impl Problem {
    fn error(text: impl Into<String>) -> Self {
        Problem {
            severity: Severity::Error,
            text: text.into(),
        }
    }
    fn warning(text: impl Into<String>) -> Self {
        Problem {
            severity: Severity::Warning,
            text: text.into(),
        }
    }
}

/// What `check` found, and what it could not look at.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub problems: Vec<Problem>,
    /// Checks that did not run. Never empty and silent at the same time: the
    /// summary line reads differently when this is non-empty, because "no
    /// problems found" and "no problems found among the checks that ran" are
    /// different claims and only the second one was established.
    pub skipped: Vec<String>,
    pub cases: usize,
    pub negatives: usize,
    /// Documents enumerated, or `None` when the corpus could not be read.
    pub corpus_documents: Option<usize>,
}

impl Report {
    pub fn errors(&self) -> usize {
        self.problems
            .iter()
            .filter(|p| p.severity == Severity::Error)
            .count()
    }
    pub fn warnings(&self) -> usize {
        self.problems
            .iter()
            .filter(|p| p.severity == Severity::Warning)
            .count()
    }

    /// The last line, which is the only line most readers will act on.
    ///
    /// Three outcomes, not two. A run with no errors but a skipped check has
    /// NOT established that the set is clean, and saying so is the entire
    /// reason `CorpusView::Unavailable` carries a reason instead of being an
    /// empty vector.
    ///
    /// "did not run" rather than "could not run": a file with no cases skips
    /// every per-case check for a reason that is not an outage, and both
    /// reasons print through this one line.
    pub fn summary(&self) -> String {
        let (e, w) = (self.errors(), self.warnings());
        let counts = format!(
            "{e} error{}, {w} warning{}",
            if e == 1 { "" } else { "s" },
            if w == 1 { "" } else { "s" }
        );
        if !self.skipped.is_empty() {
            format!(
                "{counts} — and {n} check{} did not run, so this is NOT a clean bill of health",
                if self.skipped.len() == 1 { "" } else { "s" },
                n = self.skipped.len()
            )
        } else {
            counts
        }
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for s in &self.skipped {
            out.push_str(&format!("SKIPPED {s}\n"));
        }
        if !self.skipped.is_empty() && !self.problems.is_empty() {
            out.push('\n');
        }
        // Blank line between problems: every message here is several lines and
        // wraps its continuations under the tag, so run together they read as
        // one paragraph and the count at the bottom is the only way to tell how
        // many there were.
        for (i, p) in self.problems.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let tag = match p.severity {
                Severity::Error => "ERROR  ",
                Severity::Warning => "WARNING",
            };
            out.push_str(&format!("{tag} {}\n", p.text));
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&self.summary());
        out
    }
}

// ---------------------------------------------------------------------------
// The individual checks, as pure functions.
// ---------------------------------------------------------------------------

/// 1-based case numbers whose query is empty or only whitespace.
///
/// `parse_golden` accepts these: `validate` asks only whether a case names a
/// document or declares itself negative, and `query = ""` satisfies neither
/// arm's concern. `br8n bench` then embeds the empty string and scores the
/// case against whatever comes back.
///
/// The first draft of this comment, and of the message `check_cases` prints,
/// said that folded "a guaranteed miss" into recall. MEASURED, IT DOES NOT: a
/// 4-case set with two empty queries, run against a 6-document scratch corpus,
/// read recall@5 = 1.00 at all five tiers — the empty queries scored as HITS,
/// because with six documents in the corpus almost any result contains the
/// target inside the first five DISTINCT documents. Which direction it moves
/// recall depends on the corpus, so the honest statement is the weaker and more
/// alarming one: an empty query is scored exactly like a real one, and the
/// number it contributes is not a measurement of anything.
///
/// This is why `init` writes its seeded cases COMMENTED OUT: an uncommented
/// stub with an unfilled query parses as a perfectly valid golden set.
pub fn blank_queries(cases: &[Case]) -> Vec<usize> {
    cases
        .iter()
        .enumerate()
        .filter(|(_, c)| c.query.trim().is_empty())
        .map(|(i, _)| i + 1)
        .collect()
}

/// Queries that appear more than once, with every 1-based case number.
///
/// Compared on `trim().to_lowercase()`: retrieval does not distinguish two
/// queries differing only in case or trailing space, so counting them as two
/// cases weights that one question twice in recall while looking like breadth.
///
/// Blank queries are excluded. They are all "duplicates" of each other by
/// construction, and reporting a file's five unfilled stubs twice — once as
/// blank, once as duplicates — buries the one line that says what to do.
pub fn duplicate_queries(cases: &[Case]) -> Vec<(String, Vec<usize>)> {
    let mut order: Vec<String> = Vec::new();
    let mut seen: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, c) in cases.iter().enumerate() {
        let key = c.query.trim().to_lowercase();
        if key.is_empty() {
            continue;
        }
        let entry = seen.entry(key.clone()).or_default();
        if entry.is_empty() {
            order.push(key);
        }
        entry.push(i + 1);
    }
    order
        .into_iter()
        .filter_map(|k| {
            let v = seen.remove(&k)?;
            if v.len() > 1 {
                Some((k, v))
            } else {
                None
            }
        })
        .collect()
}

/// 1-based case numbers that both declare `expect_none` and name a document.
///
/// `Case::validate` already rejects this, so on the normal path this returns
/// empty — but it BAILS on the first one, and `check`'s contract is to report
/// every problem in one pass. `check` falls back to an unvalidated parse when
/// `parse_golden` refuses (see `load_cases`) precisely so this can enumerate
/// all of them instead of the user fixing one, re-running, and finding the
/// next.
pub fn contradictory_cases(cases: &[Case]) -> Vec<usize> {
    cases
        .iter()
        .enumerate()
        .filter(|(_, c)| c.is_negative() && !c.acceptable().is_empty())
        .map(|(i, _)| i + 1)
        .collect()
}

/// 1-based case numbers that name no document and do not declare themselves
/// negative.
///
/// `Case::validate`'s OTHER arm, and the one `check` used to have no check for
/// at all. The consequence was not a missing warning but a self-contradicting
/// command: `parse_golden` refused such a file, `check` fell back to the
/// unvalidated parse, found nothing to report, printed `0 errors, 0 warnings`
/// as its last line — the only line most readers act on — and then exited
/// saying the file "has 1 error(s) — see above", pointing at a report
/// containing none.
///
/// It is also the most likely fault in a file `init` wrote. Completing a stub
/// takes three uncommented lines (`[[case]]`, `query`, `expect`) and the
/// natural two-of-three edit — uncomment the case, write the query, forget the
/// expectation — lands exactly here. Like `contradictory_cases` this enumerates
/// every one of them, because `parse_golden` bails on the first and a file of
/// five half-edited stubs otherwise reports one.
pub fn unnamed_cases(cases: &[Case]) -> Vec<usize> {
    cases
        .iter()
        .enumerate()
        .filter(|(_, c)| !c.is_negative() && c.acceptable().is_empty())
        .map(|(i, _)| i + 1)
        .collect()
}

/// The fraction of a document title's content terms that the query repeats.
///
/// Above this, `check` warns. Chosen, not measured — there is no labelled set
/// of "echoing" queries to measure against — so the reasoning is the whole
/// justification and the number is a warning rather than an error:
///
///   * 0.6 requires a strict MAJORITY of the title: 2 of 3, 3 of 5, 4 of 6.
///   * 0.5 is too eager. A two-term title ("Postgres tuning") would be flagged
///     by any query naming its subject at all, and a query has to name its
///     subject to be a query.
///   * 1.0 is too lax: it only catches a verbatim echo of the whole title, and
///     the failure this is about — writing the query by reading the title —
///     usually drops one word.
///
/// EVERY LINE OF THAT REASONING ASSUMES A TITLE OF AT LEAST THREE CONTENT
/// TERMS, and the first version of this check did not notice. The argument
/// against 0.5 — "any query naming its subject would be flagged" — applies
/// verbatim to a ONE-term title at 0.6, because one shared term is 100% of it.
/// `Postgres`, `Sourdough`, `Monstera`, `Onboarding` and `README` are all one
/// content term, and the check warned about
/// `why did our postgres server run out of connections one night` against the
/// title `Postgres`, advising the user to "ask it the way you would if you had
/// forgotten what the note was called" — which cannot be done. The threshold
/// was not too low; the DENOMINATOR was too short. See `MIN_TITLE_TERMS`.
pub const TITLE_ECHO_THRESHOLD: f32 = 0.6;

/// Below this many content terms, a title is not judged by the ratio at all.
///
/// The ratio's denominator is the title's own length, so the scale it offers
/// gets coarser as titles get shorter: how much of a title you may repeat
/// before crossing 0.6 is 0% at one term, 50% at two, and 33% at three. At one
/// or two terms there is no room between "named the subject" and "copied the
/// title" — a query MUST name its subject, and for `Postgres` that is the whole
/// title. Warning there is a false positive by construction, and no threshold
/// fixes it because the two behaviours produce identical evidence.
///
/// Three is where a query can name the subject and still leave the title
/// unrepeated, so the ratio starts to carry information. This is a floor on
/// the TITLE, not on the query: a long title is still judged, however short
/// the query.
///
/// Short titles are not thereby unchecked — `VERBATIM_RUN` still applies to
/// them — but a run needs three terms too, so a one- or two-term title is in
/// practice exempt. That is the honest consequence: an echo of a two-word title
/// is not distinguishable from a question about its subject.
pub const MIN_TITLE_TERMS: usize = 3;

/// How many of a title's content terms, contiguous and in the title's order,
/// count as a verbatim copy however small a fraction of the title they are.
///
/// The ratio alone is blind to the most literal form of this defect: copying
/// the opening words of a long title. `sourdough starter hydration` is the
/// first three terms of `Sourdough starter hydration ratio experiments log`
/// and scores 0.50; `postgres connection pooler timeout` is the first four of
/// `Postgres connection pooler timeout diagnosis rollback runbook` and scores
/// 0.571. Both are under the line, both are the failure this check exists for,
/// and both were silent.
///
/// Three, not two. `connection pooling` is two contiguous terms of
/// `Connection pooling in production Postgres`, and it is also just the name of
/// the subject — the same ambiguity `MIN_TITLE_TERMS` describes, so the same
/// answer. That leaves two-term prefixes of long titles undetected, which is a
/// known and accepted hole rather than an oversight.
///
/// Contiguity is measured on ANALYZED terms, so the stopwords the analyzer drops
/// do not break a run: `connection pooling production` is a three-run of
/// `Connection pooling in production Postgres`. That is the right granularity —
/// dropping "in" is exactly the edit someone makes while copying a title.
pub const VERBATIM_RUN: usize = 3;

/// How much of `title` the query repeats, in [0,1].
///
/// Denominated on the TITLE, not on the union or on the query. The failure
/// being detected is "this query was written by copying the title", so the
/// question is what fraction of the title survived into the query. A Jaccard or
/// query-denominated score falls as the query gets longer, which would let a
/// wordy query that contains the entire title score low — the wrong way round.
///
/// Terms come from `pack::analyze::analyze`, the same tokenizer, stopword list
/// and Porter stemmer that produced every posting in `pack.fts`. That makes the
/// number mean something specific rather than being a bespoke word-overlap
/// heuristic: it is the fraction of the title's terms that BM25 would match the
/// query on. It also makes `repotting` and `repot` the same term, which a naive
/// split would miss and which is exactly how a title gets echoed in practice.
///
/// A title made entirely of stopwords ("How to do it") has no content terms and
/// scores 0.0 — no evidence of an echo — rather than dividing by zero or
/// treating an empty intersection over an empty set as a total match.
pub fn title_overlap(query: &str, title: &str) -> f32 {
    let title_terms = title_terms(title);
    if title_terms.is_empty() {
        return 0.0;
    }
    let q: HashSet<String> = analyze(query).into_iter().collect();
    let shared = title_terms.iter().filter(|t| q.contains(*t)).count();
    shared as f32 / title_terms.len() as f32
}

/// A title's DISTINCT content terms. The ratio's denominator, and the thing
/// `MIN_TITLE_TERMS` counts — so both read the same number and cannot disagree
/// about how long a title is.
pub fn title_terms(title: &str) -> Vec<String> {
    let mut terms: Vec<String> = analyze(title);
    terms.sort();
    terms.dedup();
    terms
}

/// The longest run of the title's content terms that appears contiguously, and
/// in the title's own order, inside the query.
///
/// Order matters and position does not: this is the longest common contiguous
/// subsequence of the two analyzed term SEQUENCES, so it finds a copied prefix,
/// a copied suffix, and a copied middle alike. Terms are compared after
/// stemming and stopword removal, which is what makes
/// `how do I repot my monstera` a two-run of `Repotting a monstera`.
pub fn verbatim_run(query: &str, title: &str) -> usize {
    let q = analyze(query);
    let t = analyze(title);
    if q.is_empty() || t.is_empty() {
        return 0;
    }
    // Rolling two rows of the standard LCS-substring table. `prev[j]` is the
    // run ending at query term i-1 and title term j-1.
    let mut prev = vec![0usize; t.len() + 1];
    let mut best = 0usize;
    for qt in &q {
        let mut cur = vec![0usize; t.len() + 1];
        for (j, tt) in t.iter().enumerate() {
            if qt == tt {
                cur[j + 1] = prev[j] + 1;
                best = best.max(cur[j + 1]);
            }
        }
        prev = cur;
    }
    best
}

#[derive(Debug, Clone, PartialEq)]
pub struct TitleEcho {
    /// 1-based case number.
    pub case: usize,
    pub query: String,
    pub uri: String,
    pub title: String,
    /// The fraction of the title's distinct content terms the query repeats.
    /// Reported even when it is not what triggered the warning, because a
    /// reader judging the warning needs both numbers.
    pub overlap: f32,
    /// The longest contiguous, in-order run of title terms inside the query.
    pub run: usize,
    /// Whether the RATIO is what fired: over the line AND on a title long
    /// enough for the ratio to mean anything. False at overlap 1.00 for a
    /// one-term title — see `MIN_TITLE_TERMS`.
    pub over_the_line: bool,
}

/// Every (case, target document) pair where the query looks written from the
/// title: it repeats at least `threshold` of the title's terms, or copies
/// `VERBATIM_RUN` of them contiguously.
///
/// TWO triggers, because one ratio cannot cover both shapes. The ratio catches
/// a query that takes most of a title's words in any order and misses a
/// verbatim copy of a long title's opening; the run catches the copy and says
/// nothing about a reworded echo. Either alone leaves a class of this defect
/// silent, and both were measured silent before they were both here.
///
/// Only the case's OWN targets are considered. A query that happens to overlap
/// some unrelated document's title is not testing string matching against the
/// thing it is scored on, so it is not this defect.
///
/// A case naming a uri the corpus does not contain is skipped here — that is
/// the missing-uri error's job, and inventing an empty title for it would score
/// 0.0 and read as "checked, fine".
pub fn title_echoes(
    cases: &[Case],
    titles: &HashMap<String, String>,
    threshold: f32,
) -> Vec<TitleEcho> {
    let mut out = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        if case.is_negative() {
            continue;
        }
        for uri in case.acceptable() {
            let Some(title) = titles.get(&uri) else {
                continue;
            };
            let overlap = title_overlap(&case.query, title);
            let run = verbatim_run(&case.query, title);
            let over_the_line = title_terms(title).len() >= MIN_TITLE_TERMS && overlap >= threshold;
            if over_the_line || run >= VERBATIM_RUN {
                out.push(TitleEcho {
                    case: i + 1,
                    query: case.query.clone(),
                    uri,
                    title: title.clone(),
                    overlap,
                    run,
                    over_the_line,
                });
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The whole check, as one pure function.
// ---------------------------------------------------------------------------

fn quoted(q: &str) -> String {
    let short: String = q.chars().take(60).collect();
    if short.chars().count() < q.chars().count() {
        format!("`{short}...`")
    } else {
        format!("`{short}`")
    }
}

/// Run every check that `corpus` supports, and record the ones it does not.
pub fn check_cases(cases: &[Case], corpus: &CorpusView) -> Report {
    let negatives = cases.iter().filter(|c| c.is_negative()).count();
    let mut r = Report {
        cases: cases.len(),
        negatives,
        corpus_documents: corpus.docs().map(|d| d.len()),
        ..Default::default()
    };

    // 6. Blank queries. First, because every other per-case message quotes the
    //    query and a blank one quotes as ``.
    for n in blank_queries(cases) {
        r.problems.push(Problem::error(format!(
            "case {n}: the query is empty.\n\
             \x20       `br8n bench` will embed the empty string, score the case against\n\
             \x20       whatever comes back, and fold the result into recall. It does not\n\
             \x20       reliably score as a miss — on a small corpus it scores as a HIT —\n\
             \x20       so the recall figure moves for a reason that has nothing to do\n\
             \x20       with retrieval."
        )));
    }

    // 2. Duplicates.
    for (q, ns) in duplicate_queries(cases) {
        let list = ns
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        r.problems.push(Problem::error(format!(
            "cases {list} ask the same question:\n\
             \x20         {}\n\
             \x20       Recall would weight that one question {n} times, while the case\n\
             \x20       count suggests breadth.",
            quoted(&q),
            n = ns.len()
        )));
    }

    // 7. Names nothing at all. `Case::validate`'s other arm, and the most
    //    likely partial edit of what `init` writes.
    for n in unnamed_cases(cases) {
        r.problems.push(Problem::error(format!(
            "case {n} names no document and does not say `expect_none = true`.\n\
             \x20       `br8n bench` will not run at all: `parse_golden` refuses the whole\n\
             \x20       file for this. It refuses on the FIRST one, so fixing this case only\n\
             \x20       reveals the next — every one of them is listed here instead.\n\
             \x20       Add `expect = \"file:///...\"`, `expect_any = [\"file:///...\"]`, or\n\
             \x20       `expect_none = true` if nothing in the corpus should answer it."
        )));
    }

    // 5. Both negative and named. Normally caught by `parse_golden`; this
    //    enumerates every one of them instead of the first.
    for n in contradictory_cases(cases) {
        r.problems.push(Problem::error(format!(
            "case {n} sets `expect_none = true` AND names a document.\n\
             \x20       A case either asserts that nothing matches, or lists what should —\n\
             \x20       never both, because its hits would belong to two classes at once."
        )));
    }

    // 1 and 3 both need the corpus.
    match corpus.docs() {
        None => {
            let reason = match corpus {
                CorpusView::Unavailable { reason } => reason.clone(),
                CorpusView::Enumerated(_) => unreachable!(),
            };
            r.skipped.push(format!(
                "the expected uris were NOT checked against the index, because the corpus\n\
                 \x20       could not be enumerated: {reason}\n\
                 \x20       A uri that is not in the index scores 0 forever and is\n\
                 \x20       indistinguishable from a retrieval miss."
            ));
            r.skipped.push(
                "no query was checked for echoing its target's title, for the same reason:\n\
                 \x20       titles come from the same enumeration."
                    .to_string(),
            );
        }
        Some(docs) => {
            let uris: Vec<String> = docs.iter().map(|d| d.uri.clone()).collect();
            for uri in missing_uris(cases, &uris) {
                r.problems.push(Problem::error(format!(
                    "a case names a document that is not in the index:\n\
                     \x20         {uri}\n\
                     \x20       It will score 0 forever, which looks exactly like a\n\
                     \x20       retrieval miss."
                )));
            }
            let titles: HashMap<String, String> = docs
                .iter()
                .map(|d| (d.uri.clone(), d.title.clone()))
                .collect();
            for e in title_echoes(cases, &titles, TITLE_ECHO_THRESHOLD) {
                // Which of the two triggers fired is the most useful sentence
                // in the message: a reader told "57%" about a warning cannot
                // see why 57% warned, and the answer is that the percentage is
                // not what caught it.
                let evidence = match (e.over_the_line, e.run >= VERBATIM_RUN) {
                    (true, true) => format!(
                        "It repeats {pct:.0}% of that title's content terms, over the \
                         {thr:.0}% line,\n\
                         \x20       and {run} of them run together in the title's own order.",
                        pct = e.overlap * 100.0,
                        thr = TITLE_ECHO_THRESHOLD * 100.0,
                        run = e.run,
                    ),
                    (true, false) => format!(
                        "It repeats {pct:.0}% of that title's content terms, over the \
                         {thr:.0}% line.",
                        pct = e.overlap * 100.0,
                        thr = TITLE_ECHO_THRESHOLD * 100.0,
                    ),
                    (false, true) => format!(
                        "{run} of the title's content terms run together in the query, in the\n\
                         \x20       title's own order — that much of it is copied verbatim. The \
                         ratio\n\
                         \x20       is only {pct:.0}%, under the {thr:.0}% line, so the percentage \
                         is not what\n\
                         \x20       caught this.",
                        run = e.run,
                        pct = e.overlap * 100.0,
                        thr = TITLE_ECHO_THRESHOLD * 100.0,
                    ),
                    // `title_echoes` emits nothing unless one of them fired.
                    (false, false) => unreachable!(),
                };
                r.problems.push(Problem::warning(format!(
                    "case {c} looks like it was written from its own target's title:\n\
                     \x20         query: {q}\n\
                     \x20         title: `{t}`\n\
                     \x20         uri:   {u}\n\
                     \x20       {evidence}\n\
                     \x20       A query that echoes a title tests string matching, not\n\
                     \x20       retrieval, and it will keep passing after a change that broke\n\
                     \x20       everything else. Ask it the way you would if you had forgotten\n\
                     \x20       what the note was called.",
                    c = e.case,
                    q = quoted(&e.query),
                    t = e.title,
                    u = e.uri,
                )));
            }
        }
    }

    // 0. No cases at all.
    //
    // Recorded as SKIPPED rather than as "nothing found", because every check
    // above ran over an empty list and found nothing — which is true, and is
    // not the same claim as "this set is clean". Left unrecorded it summarised
    // as `0 errors, 0 warnings` and exited 0: a clean bill of health for a file
    // that is not a golden set, which is this project's house failure mode with
    // the file in the role of the pipeline.
    //
    // The message must NOT name only the benign cause. Three faults land here
    // and the first version of this text described one of them, sending a user
    // whose file is misspelt or whose path is wrong to look for a commented-out
    // stub that is not there.
    if cases.is_empty() {
        r.skipped.push(
            "every per-case check, because this file holds NO CASES. Nothing was\n\
             \x20       examined, so nothing was cleared. Three different faults land here\n\
             \x20       and only the first is benign:\n\
             \x20         * `br8n golden init` wrote this file and its seeded cases are\n\
             \x20           still commented out. Expected — uncomment a stub, write the\n\
             \x20           query, and run this again.\n\
             \x20         * the array header is misspelt. It is `[[case]]`, singular;\n\
             \x20           `[[cases]]` is an unrelated table that is parsed and ignored,\n\
             \x20           so a full file reads as an empty one.\n\
             \x20         * this is not your golden set. Check the path printed above, and\n\
             \x20           `BR8N_GOLDEN` if it is set — any valid TOML file at all reads\n\
             \x20           as a golden set with no cases.\n\
             \x20       `br8n bench` refuses a file with no cases, so nothing downstream\n\
             \x20       will measure anything either."
                .to_string(),
        );
    }

    // 4. Enough negatives to calibrate.
    //
    // Not on an empty file. A set with no cases at all has exactly one problem
    // — it has no cases — and the SKIPPED note above says that; adding
    // "only 0 negative cases" underneath is a property of the empty set, not a
    // second fault, and it is the first thing a user sees after
    // `br8n golden init`.
    if !cases.is_empty() && negatives < MIN_NEGATIVE_CASES {
        r.problems.push(Problem::warning(format!(
            "only {negatives} `expect_none` case(s); {MIN_NEGATIVE_CASES} is the floor.\n\
             \x20       Below it `br8n bench` cannot calibrate the `[hook] threshold` at\n\
             \x20       all: `recommend_threshold` returns `TooFewNegatives`, keeps the\n\
             \x20       shipped default, and the trade-off table's \"negative cases\n\
             \x20       injected\" column is decided by one or two queries. The separator\n\
             \x20       is a MAXIMUM over the negative class, so one more negative case can\n\
             \x20       only move it up — with one case it is that query's outlier wearing\n\
             \x20       a calibration's clothes."
        )));
    }

    r
}

// ---------------------------------------------------------------------------
// Loading, including the fallback that lets every problem be reported at once.
// ---------------------------------------------------------------------------

/// Parse for checking: validated if it validates, unvalidated if it does not.
///
/// `parse_golden` is the shipped parser and its per-case rejection is the
/// message `bench` gives, so it runs first and its verdict is what a clean file
/// gets. But it bails on the FIRST malformed case, and `check` exists to hand
/// back the whole list. When it refuses on a case rather than on the TOML, the
/// file is still structurally a golden set, so it is re-parsed without
/// validation and every check runs over the result — `contradictory_cases` then
/// finds all of them.
///
/// A genuine TOML syntax error is not recoverable that way, so `parse_golden`'s
/// error is returned as-is.
pub fn load_cases(text: &str) -> Result<(Vec<Case>, Option<String>)> {
    match parse_golden(text) {
        Ok(g) => Ok((g.case, None)),
        Err(e) => match toml::from_str::<Golden>(text) {
            Ok(g) => Ok((g.case, Some(format!("{e:#}")))),
            Err(_) => Err(e),
        },
    }
}

/// Enumerate the corpus, or say why not.
///
/// `Store::open_existing` never creates, which is the reader contract; a
/// missing index, a store that will not open, and a store that will not answer
/// are three different reasons and all three are reported as text rather than
/// folded into an empty list.
pub fn corpus_view(cfg: &Config) -> CorpusView {
    let db = Config::db_path();
    let store = match Store::open_existing(&db, cfg.embed.dimensions) {
        Ok(s) => s,
        Err(e) => {
            return CorpusView::Unavailable {
                reason: format!("{e:#}"),
            }
        }
    };
    match store.all_doc_keys() {
        Ok(keys) => CorpusView::Enumerated(
            keys.into_iter()
                .map(|(_id, title, uri)| Doc { uri, title })
                .collect(),
        ),
        Err(e) => CorpusView::Unavailable {
            reason: format!("{e:#}"),
        },
    }
}

// ---------------------------------------------------------------------------
// `br8n golden check`
// ---------------------------------------------------------------------------

/// The exit decision: `Some(n)` means fail with `n` errors, `None` means pass.
///
/// Split out of `check` so it can be pinned at all. In `check` it sat between a
/// file read and a store open, so no test could reach it, and the arm that has
/// nothing to do with `Report` — the shipped parser's own refusal — was
/// therefore unpinned.
///
/// `unvalidated` carries `parse_golden`'s message when `check` fell back to an
/// unvalidated parse. That refusal is fatal on its own: `br8n bench` will not
/// run on the file whatever this report says, so `check` must not exit 0 on it.
///
/// Since `check_cases` grew `unnamed_cases`, both of `Case::validate`'s arms
/// produce errors of their own, so the composition can no longer reach
/// `(0 errors, Some(refusal))` — that state is unreachable from `check`, and
/// the test below builds it directly rather than pretending otherwise. The arm
/// stays because it is the parser's verdict, not this module's, and a future
/// `validate` arm with no matching check here would otherwise exit 0 while
/// `bench` refuses the same file.
pub fn fatal_errors(report: &Report, unvalidated: Option<&str>) -> Option<usize> {
    let n = report.errors().max(usize::from(unvalidated.is_some()));
    if n > 0 {
        Some(n)
    } else {
        None
    }
}

pub fn check(cfg: &Config) -> Result<()> {
    let path = golden_path();
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "could not read golden set at `{p}`. Run `br8n golden init` to write a\n\
             starter set seeded with real documents from your own index.",
            p = path.display()
        )
    })?;
    let (cases, unvalidated) = load_cases(&text)
        .with_context(|| format!("could not parse `{}` as TOML", path.display()))?;

    let corpus = corpus_view(cfg);
    let report = check_cases(&cases, &corpus);

    println!("golden set: {}", path.display());
    println!(
        "cases:      {} ({} answer, {} negative)",
        report.cases,
        report.cases - report.negatives,
        report.negatives
    );
    match report.corpus_documents {
        Some(n) => println!("corpus:     {n} documents"),
        None => println!("corpus:     could not be enumerated"),
    }
    println!();
    if let Some(msg) = &unvalidated {
        // The shipped parser refused this file. `bench` will refuse it too, and
        // with this exact text — said here so the two commands cannot disagree
        // about whether the set is usable.
        println!("`br8n bench` will refuse this file. `parse_golden` says:\n  {msg}\n");
    }
    // A zero-case file used to get a paragraph here explaining the benign
    // cause. It is now a SKIPPED entry inside the report, so it prints above
    // the summary that contradicts it, names the two non-benign causes as well,
    // and can be tested.
    println!("{}", report.render());

    if let Some(n) = fatal_errors(&report, unvalidated.as_deref()) {
        anyhow::bail!("`{}` has {n} error(s) — see above", path.display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `br8n golden init`
// ---------------------------------------------------------------------------

/// How many documents `init` seeds.
///
/// Not 5 and not 200. Five is `MIN_NEGATIVE_CASES`, a floor for one class, and
/// a 5-case golden set measures noise; 200 stubs is a file nobody reads to the
/// bottom, and the seeds are only useful if the user actually looks at each one
/// to write a question about it. 25 is a sitting's work, is five times the
/// floor, and the expensive part — 25 correct paths — is already done. The
/// maintainer's own set is 222 cases; the file says so, and says to keep going.
pub const SEED_CASES: usize = 25;

/// Where `init` writes.
///
/// NOT `golden_path()`, and the difference is load-bearing: `golden_path` falls
/// back to the repo's `tests/golden.toml` when no file exists beside the
/// config, so an `init` that wrote there would overwrite a checked-in fixture
/// with the developer's own corpus on the first run inside this repository.
/// `init` writes where `golden_path` will START looking — `BR8N_GOLDEN` if
/// set, otherwise beside the config — which is the location that makes the new
/// file the one `bench` picks up.
pub fn init_path() -> PathBuf {
    if let Ok(p) = std::env::var("BR8N_GOLDEN") {
        return PathBuf::from(p);
    }
    Config::config_path().with_file_name("golden.toml")
}

/// `n` documents spread across the corpus, deterministically.
///
/// Sorted by uri, then sampled at an even stride rather than taken from the
/// front. Sorting groups a corpus by directory and source, so the first `n` of
/// a sorted list are `n` documents from ONE folder — a golden set drawn from
/// one corner of the corpus measures that corner. Striding walks the whole uri
/// space, so the seeds span directories, source types and topics.
///
/// Deterministic (no sampling, no shuffle) so `init --force` twice produces the
/// same file, and so a user comparing two runs is comparing runs.
pub fn choose_seeds(docs: &[Doc], n: usize) -> Vec<Doc> {
    let mut sorted: Vec<Doc> = docs.to_vec();
    sorted.sort_by(|a, b| a.uri.cmp(&b.uri));
    if sorted.len() <= n || n == 0 {
        return sorted;
    }
    // `i * len / n` for i in 0..n. With len > n the step is strictly greater
    // than one, so the indices are strictly increasing and no document is
    // seeded twice.
    (0..n)
        .map(|i| sorted[i * sorted.len() / n].clone())
        .collect()
}

fn quote_toml(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The starter file: schema, rules, and `seeds.len()` commented cases.
///
/// The seeded cases are COMMENTED OUT, and that is the central decision here.
/// Written live, with `query = ""` for the user to fill in, the file would
/// parse as a perfectly valid golden set the instant it is written — and
/// because `golden_path()` prefers the file beside the config over the repo
/// fixture, `br8n bench` would immediately start scoring 25 empty queries and
/// printing a recall number for them. A fabricated measurement that looks like
/// a real one is the failure mode this project is organised against.
///
/// Commented, the file parses to ZERO cases, and `bench` refuses it by name:
/// "no [[case]] entries — nothing to benchmark". Every case the user completes
/// makes the set one case bigger and still honest.
pub fn starter_toml(seeds: &[Doc]) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "\
# Your golden set: queries, and the document each one should find.
#
# `br8n bench` scores retrieval against this file and nothing else. It is the
# only objective signal for whether a change to chunking, fusion or ranking
# helped or hurt.
#
# THIS FILE MEASURES NOTHING YET. Every case below is commented out, so it
# parses to zero cases and `br8n bench` will refuse it. That is deliberate: a
# case with an empty query is a VALID case, and a valid case with an empty
# query gets scored — the empty string is embedded, whatever comes back is
# compared against the expected document, and the result lands in recall
# looking exactly like a real one. It does not even reliably score as a miss:
# on a small corpus an empty query scores as a HIT. Uncomment a stub, write
# the query, and the set is one honest case big.
#
#   1. Uncomment a case below and write the query.
#   2. `br8n golden check`   — catches uris that no longer resolve, duplicate
#                               queries, and queries that echo their target's
#                               title.
#   3. `br8n bench`          — measures.
#
# ---------------------------------------------------------------------------
# THE THREE SHAPES OF A CASE
# ---------------------------------------------------------------------------
#
#   [[case]]
#   query  = \"why did the connection pooler drop sessions\"
#   expect = \"file:///notes/postgres.md\"          one acceptable answer
#
#   [[case]]
#   query      = \"where did I write down the deploy rollback steps\"
#   expect_any = [\"file:///runbook.md\", \"file:///deploy.md\"]
#                                                 several; ANY one is a hit
#
#   [[case]]
#   query       = \"what is our parental leave policy\"
#   expect_none = true                            nothing should answer this
#
# A case is never both: `expect_none` alongside `expect` is refused, because
# its hits would belong to two classes at once.
#
# A FOURTH shape measures memory instead of the corpus, and it is scored in its
# own section of `br8n bench` — never folded into the recall table above,
# because memory text runs 10-2000 plain characters against 1024-2048 possibly
# enriched chunks, and mixing the two populations is exactly the mistake this
# project has shipped phantom regressions from before:
#
#   [[memory_case]]
#   kind      = \"fact\"
#   text      = \"The notes vault lives at ~/notes/vault.\"
#   queries   = [\"where is the notes vault on disk\"]      should retrieve it
#   negatives = [\"how do I bake sourdough\"]              should not
#
# ---------------------------------------------------------------------------
# THE RULES THAT DECIDE WHETHER THIS MEASURES ANYTHING
# ---------------------------------------------------------------------------
#
# recall@5 counts distinct DOCUMENTS, not chunks. Four chunks of one note are
# one document, so a case is a hit when its document is among the first five
# DISTINCT documents returned. You do not need to name a chunk, a heading or a
# page — name the document.
#
# QUERIES MUST NOT ECHO DOCUMENT TITLES. A title is in the text that was
# indexed, so a query that repeats it is scored on string matching rather than
# on retrieval, and it will keep passing after a change that broke everything
# else. Each stub below prints its document's title for one reason: so you can
# ask the question you would have asked if you had forgotten that title.
# `br8n golden check` warns when a query repeats {pct:.0}% or more of its own
# target's title.
#
# NEGATIVE CASES NEED GENUINELY ABSENT SUBJECTS. `expect_none` is the only
# class that can calibrate the `[hook] threshold`: it is what a query with no
# answer scores, and the gate has to sit above that. So the subject must be
# absent from the CORPUS, not merely off-topic — if any note mentions it in
# passing, the case measures that note. Check before you write one. Two traps
# that have already cost this project a calibration:
#   * A note saying \"topic X lives elsewhere\" is not an answer to a question
#     about X, so \"we don't document that here\" does not disqualify a subject.
#   * If you index your own Claude Code transcripts, the session in which you
#     WROTE these queries becomes a document containing all of them. Write
#     negatives about subjects that have never come up in a session, or
#     measure with `index_transcripts = false`.
#
# {min} negative cases is the floor below which nothing can be calibrated. It
# is a floor, not a target.
#
# ---------------------------------------------------------------------------
# HOW BIG
# ---------------------------------------------------------------------------
#
# {seeded} documents are seeded below, spread across your corpus by uri. That
# is a start, not a set — the maintainer's own is 222 cases. Keep adding cases
# as retrieval misses things; a miss you noticed and did not write down is a
# regression you will not detect.
#
# One more rule, and it is the one people break: a recall figure belongs to
# the golden set AND the corpus it was measured against. Change this file and
# the previous number is not comparable, however tempting the subtraction.
# `br8n bench` records a hash of this file with every run and will tell you.

",
        pct = TITLE_ECHO_THRESHOLD * 100.0,
        min = MIN_NEGATIVE_CASES,
        seeded = seeds.len(),
    ));

    if seeds.is_empty() {
        s.push_str(
            "\
# ---------------------------------------------------------------------------
# NO DOCUMENTS WERE SEEDED
# ---------------------------------------------------------------------------
#
# Your index could not be read, or contains no documents, so there are no real
# uris to seed from. That matters: a case must name a uri that resolves in
# YOUR index, and a hand-typed one that does not scores 0 forever while
# looking exactly like a retrieval miss.
#
# Run `br8n index`, then `br8n golden init --force` to rewrite this file
# with real documents.

",
        );
    }

    for (i, d) in seeds.iter().enumerate() {
        s.push_str(&format!(
            "# --- {n} {bar}\n\
             # {title}\n\
             # [[case]]\n\
             # query = \"\"\n\
             # expect = {uri}\n\n",
            n = i + 1,
            bar = "-".repeat(66usize.saturating_sub(format!("{}", i + 1).len())),
            title = d.title,
            uri = quote_toml(&d.uri),
        ));
    }

    s.push_str(&format!(
        "\
# ---------------------------------------------------------------------------
# NEGATIVE CASES — subjects your corpus genuinely does not cover
# ---------------------------------------------------------------------------
#
# These name no document, so nothing can be seeded: only you know what is
# absent. {min} is the floor, and the floor is not the goal.

",
        min = MIN_NEGATIVE_CASES
    ));
    for i in 0..MIN_NEGATIVE_CASES {
        s.push_str(&format!(
            "# --- negative {n} {bar}\n\
             # [[case]]\n\
             # query = \"\"\n\
             # expect_none = true\n\n",
            n = i + 1,
            bar = "-".repeat(57usize.saturating_sub(format!("{}", i + 1).len())),
        ));
    }
    s
}

pub fn init(cfg: &Config, force: bool) -> Result<()> {
    let path = init_path();
    if path.exists() && !force {
        anyhow::bail!(
            "`{p}` already exists — refusing to overwrite it.\n\n\
             That file may be the only copy of hours of work, and a golden set is not\n\
             reproducible: it is your questions about your documents. Move it aside, or\n\
             pass `--force` if you are certain.",
            p = path.display()
        );
    }

    let corpus = corpus_view(cfg);
    let seeds = match &corpus {
        CorpusView::Enumerated(docs) => choose_seeds(docs, SEED_CASES),
        CorpusView::Unavailable { reason } => {
            // Loud, and the file still gets written — with a section saying
            // exactly this instead of seeded cases. A starter file with no real
            // uris is close to useless, so the failure must not be a footnote.
            eprintln!(
                "br8n: the index could not be read, so no real document uris could be\n\
                 seeded into the starter file: {reason}\n\
                 Run `br8n index`, then `br8n golden init --force`."
            );
            Vec::new()
        }
    };

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("could not create `{}`", dir.display()))?;
    }
    let body = starter_toml(&seeds);
    std::fs::write(&path, &body)
        .with_context(|| format!("could not write `{}`", path.display()))?;

    println!("wrote {}", path.display());
    println!(
        "  {} document(s) seeded from your index, spread across it by uri",
        seeds.len()
    );
    println!("  {MIN_NEGATIVE_CASES} negative-case stubs");
    println!();
    println!(
        "Every case is COMMENTED OUT, so this file parses to zero cases and `br8n bench`\n\
         will refuse it until you fill some in. That is on purpose: a case with an empty\n\
         query still gets scored, and whatever it scores is a fabricated result sitting\n\
         inside a real recall number."
    );
    println!();
    println!("Next: edit it, then `br8n golden check`.");
    Ok(())
}
