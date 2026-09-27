pub mod synthetic;

use crate::config::{Config, Profile, Surface};
use anyhow::{Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::Path;

/// One `expect_absent` document, at one tier, for one case.
///
/// Two fields rather than one, and that is the whole of decision 2's "option
/// B". `injected` is the pass/fail the feature exists for; `rank` keeps moving
/// after `injected` saturates, which is what lets a multiplier be read off a
/// curve instead of bisected against a boolean.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AbsentRow {
    pub query: String,
    pub uri: String,
    /// Survived BOTH the threshold and the token budget — i.e. the hook would
    /// have put it in the prompt.
    ///
    /// A close approximation, not exact: the real hook applies the relevance
    /// gate BEFORE MMR diversity selection (`src/retrieve/mod.rs`), while this
    /// filters `hits`, which is already the MMR-selected list. Pre-existing —
    /// `recall_at_k_gated` and the trade-off curve share the same
    /// approximation — and not fixed here.
    pub injected: bool,
    /// 1-indexed position in the UNGATED hit list. `None` means the document
    /// was not retrieved at all at this tier, which is a different fact from
    /// "retrieved and gated out" and must never be collapsed into it.
    pub rank: Option<usize>,
}

/// One row of the bench table, persisted so `br8n dashboard` can show the
/// last run. Printing alone left the dashboard nothing to read.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BenchTier {
    pub tier: String,
    pub recall_at_5: f32,
    pub p50_ms: u64,
    pub p95_ms: u64,
    /// Per-case, never aggregated. The multiplier that removes the dead record
    /// from one case is the same multiplier applied to the cases that WANT it,
    /// and a mean would report that trade as a win.
    #[serde(default)]
    pub absent: Vec<AbsentRow>,
}

#[derive(Debug, Deserialize)]
pub struct Golden {
    /// `default` so that a golden file with no `[[case]]` entries parses to an
    /// EMPTY set rather than failing deserialization.
    ///
    /// Without it, `run`'s own "no [[case]] entries — nothing to benchmark"
    /// bail below was unreachable: serde refused such a file first, with
    /// `missing field \`case\``, wrapped in "could not parse as TOML". A user
    /// who has a golden file and has not filled it in was told their TOML was
    /// malformed. It is not — it is empty, which is a different problem with a
    /// different fix, and `run` already had the right words for it.
    ///
    /// `br8n golden init` makes this the common path: it writes every seeded
    /// case COMMENTED OUT on purpose (see `golden::starter_toml`), so the file
    /// it produces holds zero cases until the user fills one in.
    ///
    /// IT IS A TRADE, NOT A PURE FIX, and the losing half is worth stating.
    /// Without it, a file whose array header is misspelt `[[cases]]` failed
    /// deserialization with `missing field \`case\``: badly worded, but loud and
    /// located. With it, that file — and any other TOML document, including a
    /// `config.toml` reached by a wrong `BR8N_GOLDEN` — parses as a golden set
    /// holding no cases. `bench` still refuses it by name, but `golden check`
    /// summarised it as `0 errors, 0 warnings` and exited 0 until
    /// `golden::check_cases` learnt to record a zero-case file as SKIPPED. That
    /// record, not this attribute, is what keeps a file that is not a golden
    /// set from reading as a clean one.
    #[serde(default)]
    pub case: Vec<Case>,
    #[serde(default)]
    pub memory_case: Vec<MemoryCase>,
}

/// One golden case: a query, and what counts as answering it.
///
/// Three shapes, and the first is the only one that existed before:
///
/// ```toml
/// [[case]]                      # one acceptable answer
/// query = "..."
/// expect = "file:///notes/a.md"
///
/// [[case]]                      # several genuinely correct answers
/// query = "..."
/// expect_any = ["file:///notes/a.md", "file:///notes/b.md"]
///
/// [[case]]                      # nothing in the corpus should answer this
/// query = "..."
/// expect_none = true
/// ```
///
/// `expect` is retained rather than renamed because the live golden set is 196
/// cases in that shape; every field is `#[serde(default)]` so a file written
/// against the old schema parses byte-for-byte identically and scores the same.
///
/// The multi-answer form exists because scoring against ONE document counted
/// every other correct answer as a miss, so every recall figure this project
/// has published reads lower than the retriever actually performed. The
/// negative form exists because `expect` was a required `String`: a case that
/// should match nothing could not be written at all, which is why
/// `br8n bench` has never had a clean class to calibrate the hook gate
/// against.
#[derive(Debug, Deserialize)]
pub struct Case {
    pub query: String,
    /// The single-answer form. Equivalent to a one-element `expect_any`.
    #[serde(default)]
    pub expect: Option<String>,
    /// Several acceptable documents; recall counts a hit if ANY appears in the
    /// top k.
    #[serde(default)]
    pub expect_any: Vec<String>,
    /// This query should match nothing. Feeds threshold calibration instead of
    /// recall.
    #[serde(default)]
    pub expect_none: bool,
    /// Documents that must NOT reach the prompt for this query. Scored against
    /// the GATED, budget-admitted list — the one the hook actually injects —
    /// while recall stays ungated (decision 2b), so no historical recall figure
    /// moves.
    #[serde(default)]
    pub expect_absent: Vec<String>,
}

impl Case {
    /// Every document that counts as answering this case, `expect` first.
    pub fn acceptable(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::with_capacity(1 + self.expect_any.len());
        if let Some(e) = &self.expect {
            out.push(e.clone());
        }
        for e in &self.expect_any {
            if !out.contains(e) {
                out.push(e.clone());
            }
        }
        out
    }

    /// A case asserting that nothing should match. It names no document, so it
    /// is exempt from the uri-existence guard and contributes nothing to
    /// recall — its hits are the irrelevant class the gate is calibrated on.
    pub fn is_negative(&self) -> bool {
        self.expect_none
    }

    /// A case is well-formed if it names at least one document OR declares
    /// itself negative, and never both. Silently accepting `expect_none = true`
    /// alongside an `expect` would make it ambiguous which class the case's
    /// hits belong to, and a case with neither would score 0 forever — the same
    /// failure the uri-existence guard exists to prevent.
    fn validate(&self, idx: usize) -> Result<()> {
        if self.expect_none && !self.expect_absent.is_empty() {
            anyhow::bail!(
                "case {n} (`{q}`) sets `expect_none = true` and also names {c} \
                 `expect_absent` document(s). A negative case injects nothing to \
                 be absent FROM — use one or the other.",
                n = idx + 1,
                q = self.query,
                c = self.expect_absent.len()
            );
        }
        let named = self.acceptable();
        match (self.expect_none, named.is_empty()) {
            (true, true) | (false, false) => Ok(()),
            (true, false) => anyhow::bail!(
                "case {n} (`{q}`) sets `expect_none = true` and also names {c} document(s). \
                 A case either asserts that nothing matches, or lists what should — not both.",
                n = idx + 1,
                q = self.query,
                c = named.len()
            ),
            (false, true) => anyhow::bail!(
                "case {n} (`{q}`) names no document. Add `expect = \"file:///...\"`, \
                 `expect_any = [\"file:///...\"]`, or `expect_none = true`.",
                n = idx + 1,
                q = self.query
            ),
        }
    }
}

/// Parse a golden set and reject malformed cases before anything is measured.
pub fn parse_golden(text: &str) -> Result<Golden> {
    let golden: Golden = toml::from_str(text)?;
    for (i, case) in golden.case.iter().enumerate() {
        case.validate(i)?;
    }
    Ok(golden)
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct MemoryCase {
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub queries: Vec<String>,
    #[serde(default)]
    pub negatives: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct MemoryOutcome {
    pub text: String,
    pub query: String,
    pub hit: bool,
    pub negative: bool,
    pub relevance: f32,
}

pub fn render_memory_section(rows: &[MemoryOutcome], gate: f32) -> String {
    let positives: Vec<&MemoryOutcome> = rows.iter().filter(|r| !r.negative).collect();
    let hits = positives.iter().filter(|r| r.hit).count();
    let highest_negative = rows
        .iter()
        .filter(|r| r.negative)
        .map(|r| r.relevance)
        .fold(0.0f32, f32::max);
    let mut s = format!("\nmemory cases (tier 1, hook gate {gate:.2})\n");
    s.push_str(&format!(
        "  {hits}/{} queries retrieved their memory\n",
        positives.len()
    ));
    s.push_str(&format!(
        "  highest negative relevance {highest_negative:.3}\n"
    ));
    for r in positives.iter().filter(|r| !r.hit) {
        s.push_str(&format!(
            "  miss  {:<44} best {:.3}\n",
            r.query, r.relevance
        ));
    }
    s
}

pub fn memory_section(cfg: &Config, cases: &[MemoryCase], gate: f32) -> Result<String> {
    let root = std::env::temp_dir().join(format!("br8n-bench-memory-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let section = memory_section_at(&root, cfg, cases, gate);
    let _ = std::fs::remove_dir_all(&root);
    section
}

fn memory_section_at(root: &Path, cfg: &Config, cases: &[MemoryCase], gate: f32) -> Result<String> {
    for c in cases {
        let kind = crate::memory::MemoryKind::parse(&c.kind).ok_or_else(|| {
            anyhow::anyhow!(
                "memory_case kind `{}` is not lesson, fact or episode",
                c.kind
            )
        })?;
        let outcome = crate::memory::remember_at(
            root,
            cfg,
            crate::embed::for_config(&cfg.embed)?,
            crate::memory::Remember {
                kind,
                text: c.text.clone(),
                title: None,
                project: None,
                confidence: 100,
                origin: crate::memory::Origin::User,
                session: None,
                source_hash: None,
                source_stamp: None,
                created: None,
            },
        )?;
        match outcome {
            crate::memory::Outcome::Saved { .. } | crate::memory::Outcome::Replaced { .. } => {}
            other => anyhow::bail!(
                "memory_case `{}` was not stored ({}), so its queries would report a \
                 retrieval miss that is really a golden-set problem",
                c.text.chars().take(60).collect::<String>(),
                other.describe(kind)
            ),
        }
    }
    let model_id = crate::embed::for_config(&cfg.embed)?.model_id();
    let pack = crate::pack::Pack::open(
        &crate::memory::pack_dir(root),
        &model_id,
        cfg.embed.dimensions,
    )?;
    let profile = Profile::tier(1);
    let retriever = crate::retrieve_for_profile(cfg, Surface::Hook, &profile)?
        .with_memory(Ok(Some(pack)), cfg.memory.clone());
    let mut rows = Vec::new();
    for c in cases {
        let id =
            crate::memory::memory_id(crate::memory::MemoryKind::parse(&c.kind).unwrap(), &c.text);
        for (q, negative) in c
            .queries
            .iter()
            .map(|q| (q, false))
            .chain(c.negatives.iter().map(|q| (q, true)))
        {
            let hits = retriever.search(q, &profile)?;
            let best = hits
                .iter()
                .filter(|h| crate::memory::id_from_uri(&h.uri) == Some(id.as_str()))
                .map(|h| h.relevance)
                .fold(0.0f32, f32::max);
            rows.push(MemoryOutcome {
                text: c.text.clone(),
                query: q.clone(),
                hit: best >= gate,
                negative,
                relevance: best,
            });
        }
    }
    Ok(render_memory_section(&rows, gate))
}

/// The cases recall is scored over: everything that names a document.
///
/// A negative case has no document to find, so counting it would move recall
/// for a reason that has nothing to do with retrieval quality — it would be a
/// guaranteed miss in the denominator. This is the selection `run` uses, so a
/// test that calls it is testing the shipped path rather than a copy of it.
pub fn positive_cases(cases: &[Case]) -> Vec<&Case> {
    cases.iter().filter(|c| !c.is_negative()).collect()
}

/// Acceptable and `expect_absent` uris that are not in the corpus.
///
/// A golden case whose expected uri does not exist reports recall 0 forever and
/// looks identical to a genuine retrieval miss, so a typo is invisible. Negative
/// cases name nothing and are exempt — which is not a hole in the guard, since
/// there is no uri to typo.
///
/// `expect_absent` uris are checked here too, deliberately NOT by folding them
/// into `Case::acceptable()` — that would make recall count a document the
/// case asserts must NOT be retrieved, which inverts the metric. Without this,
/// a typo'd `expect_absent` uri can never be found in a hit list, so it reads
/// forever as `rank: None, injected: false` — indistinguishable from the
/// demotion this instrument exists to measure actually working.
pub fn missing_uris(cases: &[Case], indexed: &[String]) -> Vec<String> {
    let mut missing = Vec::new();
    for case in cases {
        for uri in case.acceptable().iter().chain(case.expect_absent.iter()) {
            if !indexed.contains(uri) && !missing.contains(uri) {
                missing.push(uri.clone());
            }
        }
    }
    missing
}

/// Recall over the first `k` distinct DOCUMENTS, not the first `k` chunks.
///
/// Retrieval returns chunks and a golden case names a document, so counting
/// raw positions measures the wrong thing: when four chunks of one document
/// fill the top five, only two documents can possibly match, and a run that
/// found the right document as chunks 1-4 scores the same as one that missed
/// it. Worse, it moves with candidate count — asking the vector index for 20
/// neighbours instead of 5 returns the TRUE nearest chunks, which cluster
/// inside a document, and measured 1.00 -> 0.40 on identical retrieval.
/// The metric was ranking chunk spread, not answer quality.
pub fn recall_at_k(retrieved: &[Vec<String>], expected: &[String], k: usize) -> f32 {
    let any: Vec<Vec<String>> = expected.iter().map(|e| vec![e.clone()]).collect();
    recall_at_k_any(retrieved, &any, k)
}

/// Recall where each case may have SEVERAL acceptable documents.
///
/// A hit is counted when ANY acceptable document appears within the first `k`
/// distinct documents. Scoring against a single `expect` counted every other
/// genuinely correct answer as a miss, which depressed every recall figure this
/// project has quoted; `recall_at_k` is now the one-element case of this
/// function, so the distinct-DOCUMENT rule above is shared rather than
/// duplicated.
pub fn recall_at_k_any(retrieved: &[Vec<String>], expected: &[Vec<String>], k: usize) -> f32 {
    if retrieved.is_empty() || expected.is_empty() {
        return 0.0;
    }
    let hits = retrieved
        .iter()
        .zip(expected)
        .filter(|(got, want)| {
            let mut seen: Vec<&String> = Vec::with_capacity(k);
            for uri in got.iter() {
                if !seen.contains(&uri) {
                    seen.push(uri);
                    if seen.len() > k {
                        break;
                    }
                }
                if want.contains(uri) {
                    return seen.len() <= k;
                }
            }
            false
        })
        .count();
    hits as f32 / expected.len() as f32
}

/// Midpoint between the weakest relevant score and the strongest irrelevant one.
///
/// When the classes overlap there is no clean separator, so this falls back to
/// the shipped hook default. It used to fall back to 0.55, described as
/// "conservative" — but 0.55 sits BELOW the measured irrelevant floor of
/// roughly 0.565-0.669, which is the one value `config.rs` documents as
/// admitting unrelated notes. The overlap case is exactly when the corpus has
/// told you nothing, so the answer has to be "keep the default", not "drop the
/// gate under the noise".
pub fn suggest_threshold(relevant: &[f32], irrelevant: &[f32]) -> f32 {
    let min_rel = relevant.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_irr = irrelevant.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    if min_rel.is_finite() && max_irr.is_finite() && min_rel > max_irr {
        (min_rel + max_irr) / 2.0
    } else {
        Config::default().hook.threshold
    }
}

/// Whether the two classes separated cleanly at all. When they do not, the
/// suggested number is a fallback, and saying so is the difference between a
/// measurement and a guess wearing two decimal places.
pub fn separated_cleanly(relevant: &[f32], irrelevant: &[f32]) -> bool {
    let min_rel = relevant.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_irr = irrelevant.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    min_rel.is_finite() && max_irr.is_finite() && min_rel > max_irr
}

/// How many negative cases it takes before a recommendation is a measurement.
///
/// The separator is `max(irrelevant)` — a MAXIMUM, so it is decided by the
/// single worst case in the set, and one more negative case can only move it
/// upward. With one or two such cases the number is that one query's outlier
/// dressed as a calibration; the gate it would set is not supported by
/// anything. Five is not derived from this corpus — nothing has ever measured
/// it, because until now the schema could not express a negative case at all —
/// it is a floor chosen to be plainly too small to be a coincidence and plainly
/// cheap to reach. It is a lower bound on honesty, not a sufficiency claim: the
/// report prints the count so the reader can judge.
pub const MIN_NEGATIVE_CASES: usize = 5;

/// What the negative cases actually support.
#[derive(Debug, PartialEq)]
pub enum Recommendation {
    /// The relevant class sits entirely above every hit for a query that should
    /// match nothing. This is the only variant that is a measurement.
    Calibrated {
        threshold: f32,
        min_relevant: f32,
        max_negative: f32,
        negative_cases: usize,
    },
    /// Enough negative cases, but a query that should match nothing still
    /// scored as high as a real answer. Keep the shipped default.
    Overlapping {
        keep: f32,
        min_relevant: f32,
        max_negative: f32,
        negative_cases: usize,
    },
    /// Too few negative cases to say anything. Keep the shipped default and say
    /// so, rather than deriving two decimal places from one query.
    TooFewNegatives { keep: f32, negative_cases: usize },
}

/// Recommend a hook threshold from the negative cases.
///
/// `relevant` are the scores of hits that answered a positive case; `negative`
/// are the scores of every hit returned for a case declaring `expect_none`.
/// That second class is the point: the old calibration called every non-target
/// hit of a positive case "irrelevant", and in a vault that cross-links on
/// purpose most of those are still on topic, so the classes overlapped by
/// construction and no number could ever come out. A query that should match
/// nothing has no such ambiguity.
pub fn recommend_threshold(
    relevant: &[f32],
    negative: &[f32],
    negative_cases: usize,
) -> Recommendation {
    let keep = Config::default().hook.threshold;
    if negative_cases < MIN_NEGATIVE_CASES {
        return Recommendation::TooFewNegatives {
            keep,
            negative_cases,
        };
    }
    let min_relevant = relevant.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_negative = negative.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    if !min_relevant.is_finite() || !max_negative.is_finite() {
        // Enough negative cases, but one class produced no hits at all. Nothing
        // to compare, so nothing to recommend.
        return Recommendation::TooFewNegatives {
            keep,
            negative_cases,
        };
    }
    if min_relevant > max_negative {
        Recommendation::Calibrated {
            threshold: (min_relevant + max_negative) / 2.0,
            min_relevant,
            max_negative,
            negative_cases,
        }
    } else {
        Recommendation::Overlapping {
            keep,
            min_relevant,
            max_negative,
            negative_cases,
        }
    }
}

// ---------------------------------------------------------------------------
// What the gate is actually separating, and what each candidate costs.
//
// `Recommendation` above answers ONE question — do the two classes separate —
// and on a cross-linked corpus the answer is always no, so it reports a
// minimum, a maximum, and no number to ship. Those two figures are single
// samples out of hundreds per class: they say where the tails end and nothing
// about where the mass sits, which is the only thing choosing a gate depends
// on. Everything below describes the same two pools instead of reducing them,
// and then prices every candidate on BOTH sides at once.
// ---------------------------------------------------------------------------

/// Nearest-rank percentiles of one class's weighted `relevance`.
#[derive(Debug, Clone, PartialEq)]
pub struct Spread {
    pub n: usize,
    pub min: f32,
    pub p10: f32,
    pub p25: f32,
    pub p50: f32,
    pub p75: f32,
    pub p90: f32,
    pub max: f32,
}

/// Nearest-rank percentile of an ASCENDING-sorted slice.
///
/// Nearest-rank rather than interpolated, and that is the point: every value
/// this returns is a relevance some hit actually had, so every row of the
/// distribution is a number the gate could really have been compared against.
/// An interpolated p90 is a value no hit ever scored.
///
/// `q` is clamped, so 0.0 is the minimum and 1.0 the maximum — the same two
/// numbers `recommend_threshold` reports, now as the ends of a shape rather
/// than as the whole of one.
pub fn percentile(sorted: &[f32], q: f64) -> Option<f32> {
    if sorted.is_empty() {
        return None;
    }
    let n = sorted.len();
    let rank = (q.clamp(0.0, 1.0) * n as f64).ceil() as usize;
    Some(sorted[rank.saturating_sub(1).min(n - 1)])
}

/// Sort a copy and read the seven percentiles off it.
pub fn spread(values: &[f32]) -> Option<Spread> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(Spread {
        n: v.len(),
        min: percentile(&v, 0.00)?,
        p10: percentile(&v, 0.10)?,
        p25: percentile(&v, 0.25)?,
        p50: percentile(&v, 0.50)?,
        p75: percentile(&v, 0.75)?,
        p90: percentile(&v, 0.90)?,
        max: percentile(&v, 1.00)?,
    })
}

/// One tier's contribution to the pooled calibration classes.
///
/// `recommend_threshold` pools all five tiers into one `min_relevant` and one
/// `max_negative`, so either extreme may come from a tier the hook never runs —
/// and on this corpus the answer floor does exactly that. Nothing in the report
/// used to say so, and a gate justified by a tier nobody queries is not
/// calibrated against anything.
#[derive(Debug, Clone, PartialEq)]
pub struct TierClasses {
    pub tier: String,
    pub answer_hits: usize,
    pub min_answer: Option<f32>,
    pub negative_hits: usize,
    pub max_negative: Option<f32>,
}

/// Everything one tier's trade-off curve is computed from.
///
/// A struct rather than five positional slices because they must all come from
/// the SAME tier: mixing the hit pool of one tier with the case bests of
/// another produces a curve that describes no configuration that exists.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GateSamples {
    /// Weighted relevance of every hit on a document a positive case accepts.
    pub answer_hits: Vec<f32>,
    /// Weighted relevance of every hit returned for an `expect_none` case.
    pub negative_hits: Vec<f32>,
    /// One value per positive case that retrieved an acceptable document: the
    /// best of them. Cases that retrieved none are absent, not zero — see
    /// `TradeOff::answer_cases`.
    pub answer_case_best: Vec<f32>,
    /// One value per negative case that returned anything at all: the best of
    /// those hits.
    pub negative_case_best: Vec<f32>,
    /// One entry per positive case, in the same order as the `expected` slice
    /// the curve is scored against: the documents this case returned, each
    /// with the weighted relevance the gate compares. Drives the recall column.
    pub ranked: Vec<Vec<(String, f32)>>,
}

/// One candidate threshold, and what it costs on each side of the gate.
///
/// Counted with `>=`, because that is exactly what the gate does —
/// `retrieve::run`'s `hits.retain(|h| h.relevance >= min_relevance)`. A row
/// computed with `>` would be wrong by the hits sitting on the boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct TradeOffRow {
    pub threshold: f32,
    /// Hits returned for an `expect_none` case that this gate lets through.
    pub negative_hits_admitted: usize,
    /// Hits on a document a positive case accepts that this gate cuts.
    pub answer_hits_cut: usize,
    /// `expect_none` cases with at least one admitted hit — queries the hook
    /// would inject something into although the corpus answers nothing.
    pub negative_cases_injected: usize,
    /// Positive cases whose every acceptable hit is cut: retrieval found the
    /// answer and the gate threw all of it away.
    pub answer_cases_lost: usize,
    /// recall@5 with sub-threshold hits removed before the top five distinct
    /// documents are counted.
    ///
    /// An APPROXIMATION of what the shipped pipeline scores at this gate, and
    /// NOT a bound in either direction. `Retriever::search_gated` applies the
    /// gate BEFORE diversity selection, so MMR then picks from a different
    /// pool — and MMR's relevance term is a hit's RANK inside that pool, not
    /// its score, so a smaller pool renormalises every candidate and can change
    /// which ten survive. This column can only delete from the list MMR already
    /// chose.
    ///
    /// An earlier draft of this comment argued the difference could only run
    /// one way and called the column a lower bound. Measuring it settled that:
    /// against `search_gated` over eight gates on the live index it matched to
    /// four decimal places at seven of them and disagreed at one — gate 0.68,
    /// exact 0.7755 against 0.7908 here — which is the direction the
    /// lower-bound argument had ruled out. Close enough to read a curve from,
    /// not close enough to quote as the gated figure for a specific gate.
    pub recall_at_5: f32,
}

/// The curve, with the denominators it is read against.
///
/// The two units answer different questions and neither substitutes for the
/// other. HITS are what the gate compares, so they are the honest denominator
/// for "how much of each class does this admit". CASES are what a user
/// notices: an answer spread over four chunks loses nothing when its weakest
/// chunk is cut, and a negative case injects noise once however many of its
/// hits clear the bar.
#[derive(Debug, Clone, PartialEq)]
pub struct TradeOff {
    pub rows: Vec<TradeOffRow>,
    pub negative_hits: usize,
    pub answer_hits: usize,
    /// Negative cases that returned at least one hit. A case that returned
    /// nothing cannot be injected at any threshold, so counting it would put a
    /// constant in the denominator that no gate can move.
    pub negative_cases: usize,
    /// Positive cases that retrieved at least one acceptable document. A case
    /// whose answer was never retrieved is already lost before the gate sees
    /// it; including it would charge the threshold for a retrieval miss.
    pub answer_cases: usize,
}

/// The ladder of candidate gates the report walks.
///
/// Fine through 0.60-0.80, where every calibration this corpus has produced has
/// landed, and coarse outside it. The shipped default is spliced in if it is
/// ever moved off the ladder, so the report always carries a row for the gate
/// the user is actually running.
pub fn candidate_thresholds() -> Vec<f32> {
    let mut t = vec![
        0.50, 0.55, 0.60, 0.62, 0.64, 0.66, 0.68, 0.70, 0.72, 0.74, 0.76, 0.78, 0.80, 0.85, 0.90,
    ];
    let keep = Config::default().hook.threshold;
    if !t.iter().any(|x| (x - keep).abs() < 1e-6) {
        t.push(keep);
        t.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    }
    t
}

/// recall@5 as the gate would leave it: hits under `threshold` are dropped
/// before the first `k` DISTINCT documents are counted.
///
/// Shares `recall_at_k_any` rather than re-deriving the distinct-document rule,
/// so the gated column and the table above it cannot come to mean two different
/// things. See `TradeOffRow::recall_at_5` for why this is a lower bound.
pub fn recall_at_k_gated(
    ranked: &[Vec<(String, f32)>],
    expected: &[Vec<String>],
    k: usize,
    threshold: f32,
) -> f32 {
    let kept: Vec<Vec<String>> = ranked
        .iter()
        .map(|hits| {
            hits.iter()
                .filter(|(_, rel)| *rel >= threshold)
                .map(|(uri, _)| uri.clone())
                .collect()
        })
        .collect();
    recall_at_k_any(&kept, expected, k)
}

/// Price every candidate gate on both sides.
pub fn trade_off(s: &GateSamples, expected: &[Vec<String>], thresholds: &[f32]) -> TradeOff {
    let rows = thresholds
        .iter()
        .map(|&threshold| TradeOffRow {
            threshold,
            negative_hits_admitted: s.negative_hits.iter().filter(|&&x| x >= threshold).count(),
            answer_hits_cut: s.answer_hits.iter().filter(|&&x| x < threshold).count(),
            negative_cases_injected: s
                .negative_case_best
                .iter()
                .filter(|&&x| x >= threshold)
                .count(),
            answer_cases_lost: s
                .answer_case_best
                .iter()
                .filter(|&&x| x < threshold)
                .count(),
            recall_at_5: recall_at_k_gated(&s.ranked, expected, 5, threshold),
        })
        .collect();
    TradeOff {
        rows,
        negative_hits: s.negative_hits.len(),
        answer_hits: s.answer_hits.len(),
        negative_cases: s.negative_case_best.len(),
        answer_cases: s.answer_case_best.len(),
    }
}

fn pct(part: usize, whole: usize) -> String {
    if whole == 0 {
        return "   -".to_string();
    }
    format!("{:3.0}%", 100.0 * part as f64 / whole as f64)
}

/// The curve as a printable block. A function rather than inline `println!`s so
/// a test can read it without capturing stdout.
pub fn trade_off_table(t: &TradeOff) -> String {
    let mut s = String::from(
        "  gate   negative hits   negative cases     answer hits    answer cases   recall\n\
         \x20        admitted         injected            cut             lost         @5\n",
    );
    for r in &t.rows {
        s.push_str(&format!(
            "  {:.2}  {:>5}/{:<5}{}  {:>4}/{:<4}{}  {:>5}/{:<5}{}  {:>4}/{:<4}{}   {:>5.2}\n",
            r.threshold,
            r.negative_hits_admitted,
            t.negative_hits,
            pct(r.negative_hits_admitted, t.negative_hits),
            r.negative_cases_injected,
            t.negative_cases,
            pct(r.negative_cases_injected, t.negative_cases),
            r.answer_hits_cut,
            t.answer_hits,
            pct(r.answer_hits_cut, t.answer_hits),
            r.answer_cases_lost,
            t.answer_cases,
            pct(r.answer_cases_lost, t.answer_cases),
            r.recall_at_5,
        ));
    }
    s
}

/// The lowest gate on the ladder that admits no negative case at all.
/// `None` when no candidate reaches zero.
pub fn cheapest_clean_gate(t: &TradeOff) -> Option<&TradeOffRow> {
    t.rows.iter().find(|r| r.negative_cases_injected == 0)
}

// ---------------------------------------------------------------------------
// Which stage produced a hit.
// ---------------------------------------------------------------------------

/// Which retrieval stage put a chunk into the final list.
///
/// This exists to test a specific suspicion: that `relevance` is one scale
/// carrying two different kinds of claim. Vector search MEASURES a cosine and
/// returns the chunk because of it. BM25 and graph expansion return a chunk for
/// reasons that are not a cosine at all, and the post-fusion measure stage then
/// backfills one — so for those hits the number the gate reads is an answer to
/// a question nobody asked. If the classes were two populations sharing one
/// scale, splitting them here would show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    /// Returned by vector search — whatever else also returned it. The
    /// precedence is deliberate: a chunk BM25 also found still had its cosine
    /// measured by the nearest-neighbour search that returned it, so its
    /// relevance is a real measurement and belongs in this class.
    Vector,
    /// BM25 found it and vector search did not, so its relevance was
    /// backfilled by the measure stage.
    Keyword,
    /// Graph expansion only: neither retriever acting on the query returned it.
    Graph,
    /// In the final list and in none of the stage traces. Should stay empty; it
    /// is a bucket rather than an assumption because silently mis-attributing a
    /// hit is the failure this whole table exists to expose.
    Unattributed,
}

impl Origin {
    pub fn label(&self) -> &'static str {
        match self {
            Origin::Vector => "vector",
            Origin::Keyword => "keyword",
            Origin::Graph => "graph",
            Origin::Unattributed => "unattributed",
        }
    }

    pub const ALL: [Origin; 4] = [
        Origin::Vector,
        Origin::Keyword,
        Origin::Graph,
        Origin::Unattributed,
    ];
}

/// Attribute one chunk to the stage that produced it.
///
/// Takes the three chunk-id sets rather than the `StageTrace` list, so the rule
/// can be tested without building a retriever.
pub fn origin_of(
    chunk_id: &str,
    vector: &std::collections::HashSet<String>,
    keyword: &std::collections::HashSet<String>,
    graph: &std::collections::HashSet<String>,
) -> Origin {
    if vector.contains(chunk_id) {
        Origin::Vector
    } else if keyword.contains(chunk_id) {
        Origin::Keyword
    } else if graph.contains(chunk_id) {
        Origin::Graph
    } else {
        Origin::Unattributed
    }
}

/// One calibration hit, with the stage that produced it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OriginSample {
    pub relevance: f32,
    pub origin: Origin,
    /// True when the hit was returned for a case asserting `expect_none`.
    pub negative: bool,
}

/// Per-origin spreads for one class, in `Origin::ALL` order, skipping origins
/// that produced no hits.
pub fn origin_breakdown(samples: &[OriginSample], negative: bool) -> Vec<(Origin, Spread)> {
    Origin::ALL
        .iter()
        .filter_map(|&o| {
            let v: Vec<f32> = samples
                .iter()
                .filter(|s| s.negative == negative && s.origin == o)
                .map(|s| s.relevance)
                .collect();
            spread(&v).map(|sp| (o, sp))
        })
        .collect()
}

/// Where the golden set lives.
///
/// This was the cwd-relative `tests/golden.toml`, so `br8n bench` only worked
/// from inside the development repository — while the docs tell every user to
/// run it to calibrate their threshold. It now resolves beside the config,
/// falls back to the repo copy so development is unaffected, and can be pointed
/// anywhere with `BR8N_GOLDEN`.
pub fn golden_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("BR8N_GOLDEN") {
        return std::path::PathBuf::from(p);
    }
    let beside_config = Config::config_path().with_file_name("golden.toml");
    if beside_config.exists() {
        return beside_config;
    }
    std::path::PathBuf::from("tests/golden.toml")
}

/// Beside the config, so `BR8N_CONFIG` relocates both together.
pub fn report_path() -> std::path::PathBuf {
    Config::config_path().with_file_name("bench-latest.json")
}

// ---------------------------------------------------------------------------
// Provenance: what a recall figure was measured against.
// ---------------------------------------------------------------------------

/// The three things that decide whether two bench runs mean the same thing.
///
/// A recall@5 figure is only interpretable against the golden set it was scored
/// on and the corpus it was scored over, and the printed table says neither.
/// The rule used to live in prose alone, and prose did not hold: the person who
/// wrote "recall figures from different corpus states are not comparable" into
/// CLAUDE.md presented two such numbers as a comparison the same morning.
///
/// Nothing here is a quality signal. It exists to answer one question — may
/// these two numbers be subtracted — and the answer is no far more often than
/// it looks.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Provenance {
    /// sha256 of the golden file's CONTENTS, not its path. The path is the same
    /// string across every edit of the live set, and `BR8N_GOLDEN` can point
    /// two runs at two different files, so the path distinguishes nothing.
    pub golden_sha256: String,
    /// Cases in the golden set, `expect_none` ones included.
    ///
    /// It cannot move without `golden_sha256` moving too, so it detects no
    /// change the hash misses — it is recorded because a pair of hexadecimal
    /// digests tells a reader THAT the set changed and nothing whatever about
    /// how, and "100 cases -> 120 cases" does.
    pub cases: usize,
    /// Documents in the corpus at the time of the run, or `None` when the store
    /// could not be counted.
    ///
    /// Absent, never zero. A fabricated count is worse than a missing one: two
    /// genuinely incomparable runs that both failed to count would compare
    /// equal at 0 documents and this whole check would wave them through, which
    /// is precisely the defect it exists to stop.
    pub documents: Option<i64>,
    /// Chunks in the corpus at the time of the run. `None` as above.
    pub chunks: Option<i64>,
    #[serde(default)]
    pub memories: Option<usize>,
    #[serde(default)]
    pub no_graph: Option<bool>,
}

/// sha256 of a golden set's text, hex-encoded.
pub fn golden_sha256(text: &str) -> String {
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    hex::encode(h.finalize())
}

/// What `bench-latest.json` holds: the table, and what produced it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BenchReport {
    /// `None` for a report written before provenance was recorded. That is a
    /// third state, distinct from both "no previous run" and "a previous run
    /// that matches" — the numbers exist and nothing is known about them — and
    /// collapsing it into either would either invent a comparison or discard a
    /// real one.
    #[serde(default)]
    pub provenance: Option<Provenance>,
    pub tiers: Vec<BenchTier>,
}

/// One thing that moved between two runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    GoldenSet {
        before: String,
        after: String,
    },
    Cases {
        before: usize,
        after: usize,
    },
    Documents {
        before: i64,
        after: i64,
    },
    Chunks {
        before: i64,
        after: i64,
    },
    Memories {
        before: usize,
        after: usize,
    },
    /// One of the two runs could not count its corpus, so the corpus cannot be
    /// shown to have held still. Not the same as "it changed", and treated the
    /// same way on purpose: an unverifiable corpus is not a comparable one.
    CorpusUnknown,
    GraphAblation {
        before: bool,
        after: bool,
    },
}

impl Change {
    /// One line of the warning, carrying the direction of the move.
    pub fn line(&self) -> String {
        match self {
            Change::GoldenSet { before, after } => format!(
                "golden set        sha {} -> sha {}",
                short_sha(before),
                short_sha(after)
            ),
            Change::Cases { before, after } => {
                format!("golden cases      {before} -> {after}")
            }
            Change::Documents { before, after } => {
                format!("corpus documents  {before} -> {after}")
            }
            Change::Chunks { before, after } => {
                format!("corpus chunks     {before} -> {after}")
            }
            Change::Memories { before, after } => {
                format!("memory count      {before} -> {after}")
            }
            Change::CorpusUnknown => {
                "corpus size       unknown for one of the two runs — the store could not be counted"
                    .to_string()
            }
            Change::GraphAblation { before, after } => {
                format!("--no-graph        {before} -> {after}")
            }
        }
    }
}

fn short_sha(s: &str) -> &str {
    let n = s.len().min(12);
    &s[..n]
}

/// Whether this run's numbers may be set beside the previous run's.
#[derive(Debug, Clone, PartialEq)]
pub enum Comparability {
    /// No previous report on this machine. Nothing to compare, nothing to warn.
    NoPrevious,
    /// A previous report exists but predates provenance being recorded.
    PreviousProvenanceUnknown,
    /// Same golden set, same case count, same corpus.
    Comparable,
    /// At least one of the three moved. Never empty.
    NotComparable(Vec<Change>),
}

impl Comparability {
    /// The warning, or `None` when there is nothing to warn about.
    ///
    /// Written to survive being pasted somewhere with none of the surrounding
    /// output — a commit message, an issue, a chat. So it names the tool, says
    /// what moved and in which direction, and explicitly forecloses the reading
    /// it exists to prevent: that the gap between two recall figures across
    /// this boundary is a win or a regression. It quotes no recall figure OF
    /// THIS RUN at all, so the block cannot itself be mistaken for a result.
    pub fn warning(&self) -> Option<String> {
        match self {
            Comparability::NoPrevious | Comparability::Comparable => None,
            Comparability::PreviousProvenanceUnknown => Some(
                "br8n bench: COMPARABILITY WITH THE PREVIOUS RUN IS UNKNOWN.\n\
                 The previous bench-latest.json on this machine was written before br8n\n\
                 bench recorded what it measured against, so the golden set and the corpus\n\
                 size behind those numbers are not known. Any difference between the two\n\
                 runs' recall figures is unexplained, not evidence about retrieval quality\n\
                 in either direction. This run recorded its own provenance, so the next one\n\
                 can be checked."
                    .to_string(),
            ),
            Comparability::NotComparable(changes) => {
                let mut s = String::from(
                    "br8n bench: NOT COMPARABLE WITH THE PREVIOUS RUN ON THIS MACHINE.\n\
                     recall@5 is scored against a fixed golden set over a corpus that moves,\n\
                     so two runs measure the same thing only when both held still. These did\n\
                     not, between the previous run and this one:\n",
                );
                for c in changes {
                    s.push_str("  ");
                    s.push_str(&c.line());
                    s.push('\n');
                }
                s.push_str(
                    "A difference between the two runs' recall figures is therefore not\n\
                     evidence about retrieval quality, in either direction. recall@5 moves\n\
                     whenever either side of the measurement moves, with no code change at\n\
                     all: with the golden set held fixed and only the corpus growing,\n\
                     exhaustive read 0.89 at 599 documents, 0.86 at 604, and 0.88 at 635\n\
                     across one day. To compare two builds, run each against the same corpus\n\
                     and the same golden set.",
                );
                Some(s)
            }
        }
    }
}

/// Compare this run's provenance against the previous report's.
///
/// Documents and chunks are checked SEPARATELY rather than as one "corpus"
/// value. They move independently — a re-chunking changes the chunk count with
/// the document count fixed — and either one moving is enough to make recall@5
/// drift.
pub fn comparability(previous: Option<&BenchReport>, current: &Provenance) -> Comparability {
    let Some(previous) = previous else {
        return Comparability::NoPrevious;
    };
    let Some(prev) = previous.provenance.as_ref() else {
        return Comparability::PreviousProvenanceUnknown;
    };

    let mut changes = Vec::new();
    if prev.golden_sha256 != current.golden_sha256 {
        changes.push(Change::GoldenSet {
            before: prev.golden_sha256.clone(),
            after: current.golden_sha256.clone(),
        });
    }
    if prev.cases != current.cases {
        changes.push(Change::Cases {
            before: prev.cases,
            after: current.cases,
        });
    }
    if let (Some(before), Some(after)) = (prev.documents, current.documents) {
        if before != after {
            changes.push(Change::Documents { before, after });
        }
    }
    if let (Some(before), Some(after)) = (prev.chunks, current.chunks) {
        if before != after {
            changes.push(Change::Chunks { before, after });
        }
    }
    if let (Some(before), Some(after)) = (prev.memories, current.memories) {
        if before != after {
            changes.push(Change::Memories { before, after });
        }
    }
    let before = prev.no_graph.unwrap_or(false);
    let after = current.no_graph.unwrap_or(false);
    if before != after {
        changes.push(Change::GraphAblation { before, after });
    }
    // An uncountable corpus on either side is reported once, after the counts
    // that WERE readable, so a run that knows its document count and not its
    // chunk count still shows the document move.
    if prev.documents.is_none()
        || current.documents.is_none()
        || prev.chunks.is_none()
        || current.chunks.is_none()
    {
        changes.push(Change::CorpusUnknown);
    }

    if changes.is_empty() {
        Comparability::Comparable
    } else {
        Comparability::NotComparable(changes)
    }
}

// ---------------------------------------------------------------------------
// Report I/O.
// ---------------------------------------------------------------------------

pub fn write_report(report: &BenchReport) -> Result<()> {
    write_report_at(&report_path(), report)
}

/// Path-taking form, so a test can exercise the shipped codec without setting
/// `BR8N_CONFIG` — a process-wide mutation that races every other test in the
/// same binary.
pub fn write_report_at(path: &std::path::Path, report: &BenchReport) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, serde_json::to_string_pretty(report)?)?;
    Ok(())
}

/// The tier table alone. `br8n dashboard` reads this and has never wanted
/// anything else; provenance reaches callers through `read_report_full`.
pub fn read_report() -> Option<Vec<BenchTier>> {
    read_report_full().map(|r| r.tiers)
}

pub fn read_report_full() -> Option<BenchReport> {
    read_report_at(&report_path())
}

pub fn read_report_at(path: &std::path::Path) -> Option<BenchReport> {
    parse_report(&std::fs::read_to_string(path).ok()?)
}

/// Decode either on-disk shape.
///
/// Before provenance existed the file was a bare JSON array of tiers, and one
/// is sitting on every machine that has ever run `br8n bench`. Reading it must
/// not fail: a failed read is indistinguishable from no previous run, and "no
/// previous run" is the one verdict that prints no warning at all — so a
/// too-strict parser would silently wave through exactly the comparison this
/// module exists to stop. The array shape decodes with `provenance: None`,
/// which is its own verdict.
pub fn parse_report(text: &str) -> Option<BenchReport> {
    if let Ok(report) = serde_json::from_str::<BenchReport>(text) {
        return Some(report);
    }
    let tiers: Vec<BenchTier> = serde_json::from_str(text).ok()?;
    Some(BenchReport {
        provenance: None,
        tiers,
    })
}

/// Score one case's `expect_absent` documents against what the hook would
/// actually inject.
///
/// Pure, and extracted from `run`'s sweep so it can be unit-tested — the
/// commit that introduced it claimed this could only be checked on a live
/// run, which was wrong: the inputs are just hits, a threshold and a budget.
///
/// Decision 2/2b. ONE search, TWO views of it: recall reads `hits` ungated
/// exactly as it always has, and only this scoring applies the gate.
/// `admitted_by_budget` is the helper the hook itself reaches through
/// `build_context`, so "injected" cannot drift from what the hook means by
/// it.
fn score_absent(
    query: &str,
    hits: &[crate::store::Hit],
    expect_absent: &[String],
    threshold: f32,
    max_tokens: usize,
) -> Vec<AbsentRow> {
    let kept: Vec<crate::store::Hit> = hits
        .iter()
        .filter(|h| h.relevance >= threshold)
        .cloned()
        .collect();
    let admitted = crate::budget::admitted_by_budget(&kept, max_tokens);
    let mut rows = Vec::with_capacity(expect_absent.len());
    for uri in expect_absent {
        rows.push(AbsentRow {
            query: query.to_string(),
            uri: uri.clone(),
            injected: kept[..admitted].iter().any(|h| &h.uri == uri),
            // UNGATED rank, 1-indexed. This is the half that keeps giving
            // signal after `injected` goes false.
            rank: hits.iter().position(|h| &h.uri == uri).map(|i| i + 1),
        });
    }
    rows
}

pub fn without_graph(mut p: crate::config::Profile) -> crate::config::Profile {
    p.graph = None;
    p
}

pub fn run(cfg: &Config, no_graph: bool) -> Result<()> {
    let golden_file = golden_path();
    let golden_text = std::fs::read_to_string(&golden_file).with_context(|| {
        format!(
            "could not read golden set at `{p}`.\n\
             `br8n bench` measures recall against a set of (query, expected document)\n\
             pairs; without it there is nothing to measure. Create `{p}` with\n\
             entries like:\n\n\
             [[case]]\n\
             query = \"...\"\n\
             expect = \"file:///notes/your-note.md\"\n\n\
             A case may name several acceptable answers with\n\
             `expect_any = [\"file:///a.md\", \"file:///b.md\"]`, or assert that the\n\
             corpus should answer nothing with `expect_none = true` — those are\n\
             what calibrate the hook threshold.\n\n\
             Or point `BR8N_GOLDEN` at one you already have.",
            p = golden_file.display()
        )
    })?;
    let golden: Golden = parse_golden(&golden_text)
        .with_context(|| format!("could not parse `{}` as TOML", golden_file.display()))?;

    if golden.case.is_empty() {
        anyhow::bail!(
            "`{}` has no [[case]] entries — nothing to benchmark",
            golden_file.display()
        );
    }

    // Hook weights: the headline output below is a suggested `[hook] threshold`,
    // and a threshold is only meaningful against the same weighted `relevance`
    // the hook will compare it to. Identical to the global table unless
    // `[hook.weights]` is set.
    // This sweeps ALL FIVE tiers below on one retriever, including tiers 2-4
    // which set `graph` — so it cannot use `retrieve_for`, which decides
    // store-vs-pack against a single profile (the hook's own default, tier 1,
    // which needs no graph). Forcing the decision against tier 4's profile
    // guarantees a store is opened regardless of what the hook itself is
    // configured to. Getting this wrong would not error loudly: a storeless
    // retriever's `search` returns `Err` for a graph tier, and the sweep below
    // swallows that into an empty hit list, so every one of tiers 2-4 would
    // silently report recall 0.0 — indistinguishable from a genuine miss.
    let retriever = crate::retrieve_for_profile(cfg, Surface::Hook, &Profile::tier(4))?;

    // Recall is scored over the POSITIVE cases only; a case that should match
    // nothing has no document to find, so counting it would move recall for a
    // reason unrelated to retrieval quality. Its hits go to calibration below.
    let positives: Vec<&Case> = positive_cases(&golden.case);
    let expected: Vec<Vec<String>> = positives.iter().map(|c| c.acceptable()).collect();
    let negative_cases = golden.case.len() - positives.len();

    // Fail loudly, not silently, when the golden set's documents were never
    // indexed: a golden case whose expected uri does not exist in the corpus
    // will report recall 0 forever and look identical to a genuine retrieval
    // miss. `tests/golden.toml` ships with two placeholder cases pointing at
    // `file:///notes/postgres.md` and `file:///notes/baking.md`; a user who
    // has not replaced them deserves a clear message, not a silently useless
    // table.
    //
    // The guard needs the corpus enumerated, and there are two ways not to get
    // it: no store at all, or a store that fails the read. Neither is the same
    // as "these documents are absent", and folding them into an empty list
    // with `.ok().unwrap_or_default()` makes the bench bail listing EVERY case
    // as missing — a store failure wearing the costume of a user error, told
    // to fix a golden set that was never wrong. When the corpus cannot be
    // enumerated the guard is SKIPPED and says so on stderr; the bench then
    // runs unguarded, which is what it did before the guard existed.
    //
    // `retrieve_for_profile` above is forced to tier 4's profile, whose
    // `graph` makes `needs_store` true, so `store()` is `Some` by
    // construction today. The `None` arm is not dead defensiveness — it is
    // what keeps this correct if that forcing is ever relaxed.
    //
    // The (doc_id, uri) PAIRS are kept, not only the uris the guard needs. The
    // stage-attribution pass below reaches its hits through `Explain`, whose
    // `HitSummary` carries a `doc_id` and no uri, so telling an answer from a
    // negative there needs the reverse of this map. Throwing the ids away and
    // re-opening the store for them would enumerate the same table twice.
    let indexed_docs: Option<Vec<(String, String)>> = match retriever.store() {
        None => {
            eprintln!(
                "br8n: no store on this retriever, so the golden set's uris cannot be\n\
                 checked against the corpus. A typo'd uri will score 0 and look like a\n\
                 retrieval miss."
            );
            None
        }
        Some(s) => match s.all_doc_uris() {
            Ok(uris) => Some(uris),
            Err(e) => {
                eprintln!(
                    "br8n: could not list the corpus ({e}), so the golden set's uris cannot\n\
                     be checked. A typo'd uri will score 0 and look like a retrieval miss."
                );
                None
            }
        },
    };
    let missing = indexed_docs
        .as_ref()
        .map(|docs| {
            let uris: Vec<String> = docs.iter().map(|(_id, uri)| uri.clone()).collect();
            missing_uris(&golden.case, &uris)
        })
        .unwrap_or_default();
    if !missing.is_empty() {
        anyhow::bail!(
            "`{p}` references document(s) not found in the index:\n  {m}\n\n\
             Either index those documents first (`br8n index`), or edit `{p}`\n\
             to reference documents that actually exist in your corpus. The shipped\n\
             golden file is a starting point, not a fixture — its two sample cases\n\
             (file:///notes/postgres.md, file:///notes/baking.md) will not exist unless\n\
             you created notes at those paths.",
            p = golden_file.display(),
            m = missing.join("\n  ")
        );
    }

    // What this run is measuring against, captured from the same store the
    // sweep below queries. `count_documents`/`count_chunks` return `Result`,
    // and the failure is kept as `None` rather than folded to 0 the way
    // `status_snapshot` folds it: a display path may render an uncounted store
    // as "0 documents", but a provenance record may not, because two runs that
    // both failed to count would then compare equal and be waved through.
    let (documents, chunks) = match retriever.store() {
        Some(s) => (s.count_documents().ok(), s.count_chunks().ok()),
        None => (None, None),
    };
    let memories = crate::memory::count_at(&crate::memory::default_root(), cfg.embed.dimensions)
        .ok()
        .map(|n| n as usize);
    let provenance = Provenance {
        golden_sha256: golden_sha256(&golden_text),
        cases: golden.case.len(),
        documents,
        chunks,
        memories,
        no_graph: Some(no_graph),
    };

    // The previous report is read BEFORE this run overwrites it, and the
    // verdict is printed ABOVE the table. Below the table it would be read
    // after the numbers had already been believed, and a paste that begins at
    // the table would drop it entirely — which is the failure mode that made
    // this a tool check instead of a paragraph in CLAUDE.md.
    let verdict = comparability(read_report_full().as_ref(), &provenance);
    match verdict.warning() {
        Some(w) => println!("{w}\n"),
        None => {
            if verdict == Comparability::Comparable {
                println!(
                    "comparable with the previous run: same golden set ({} cases), \
                     same corpus.\n",
                    provenance.cases
                );
            }
        }
    }

    println!(
        "{:<12} {:>10} {:>10} {:>10}",
        "tier", "recall@5", "p50 ms", "p95 ms"
    );

    let mut relevant_scores = Vec::new();
    let mut irrelevant_scores = Vec::new();
    let mut negative_scores = Vec::new();
    // One `GateSamples` per tier. The curve below is drawn from the HOOK's tier
    // alone, because that is the only tier the gate it sets will ever run at;
    // the pooled distributions keep their five-tier meaning so they still
    // describe the two numbers `recommend_threshold` prints.
    let mut per_tier_samples: Vec<GateSamples> = Vec::new();
    let mut per_tier: Vec<TierClasses> = Vec::new();
    let mut origin_samples: Vec<OriginSample> = Vec::new();
    let mut origin_ms: u128 = 0;
    let mut report: Vec<BenchTier> = Vec::new();

    // Where the stage-attribution pass runs, and why at one tier only. It needs
    // a SECOND query per case: `search_explained` carries the stage traces, but
    // its `HitSummary` has no uri, so it cannot also drive the recall table
    // above — and routing the shipped recall metric through a doc_id -> uri
    // translation would hide a whole class of disagreement behind a lookup.
    // Running it at all five tiers would therefore roughly double this tool's
    // wall clock. The gate being calibrated belongs to the hook, and the hook
    // runs exactly one tier.
    let hook_tier = cfg.quality_for(Surface::Hook);
    let hook_tokens = cfg.surface(Surface::Hook).max_tokens;
    // uri -> doc_id, the reverse of the enumeration the guard above used. Empty
    // when the corpus could not be enumerated, which switches attribution off
    // rather than letting it silently classify every hit as a non-answer.
    let uri_to_doc: std::collections::HashMap<String, String> = indexed_docs
        .map(|docs| {
            docs.into_iter()
                .map(|(id, uri)| (uri, id))
                .collect::<std::collections::HashMap<_, _>>()
        })
        .unwrap_or_default();

    for tier in 0..=4u8 {
        let profile = if no_graph {
            without_graph(Profile::tier(tier))
        } else {
            Profile::tier(tier)
        };
        let mut all_uris = Vec::new();
        let mut timings = Vec::new();
        let mut samples = GateSamples::default();
        let mut absent_rows: Vec<AbsentRow> = Vec::new();

        for case in &golden.case {
            let t0 = std::time::Instant::now();
            let hits = retriever.search(&case.query, &profile).unwrap_or_default();
            timings.push(t0.elapsed().as_millis() as u64);

            let want = case.acceptable();
            // The best hit of this case's OWN class at this tier. `None` when
            // the case returned nothing of that class — a retrieval miss, which
            // no threshold can be charged for.
            let mut best_answer: Option<f32> = None;
            let mut best_negative: Option<f32> = None;
            for h in &hits {
                // Calibrate against `relevance` — the field the hook's gate
                // actually compares. Calibrating on `score` would derive a
                // threshold for a scale nothing tests against.
                if case.is_negative() {
                    // Nothing should have answered this query, so every hit is
                    // irrelevant by construction rather than by assumption.
                    negative_scores.push(h.relevance);
                    samples.negative_hits.push(h.relevance);
                    best_negative =
                        Some(best_negative.map_or(h.relevance, |b: f32| b.max(h.relevance)));
                } else if want.contains(&h.uri) {
                    relevant_scores.push(h.relevance);
                    samples.answer_hits.push(h.relevance);
                    best_answer =
                        Some(best_answer.map_or(h.relevance, |b: f32| b.max(h.relevance)));
                } else {
                    irrelevant_scores.push(h.relevance);
                }
            }
            if let Some(b) = best_answer {
                samples.answer_case_best.push(b);
            }
            if let Some(b) = best_negative {
                samples.negative_case_best.push(b);
            }
            if !case.expect_absent.is_empty() {
                absent_rows.extend(score_absent(
                    &case.query,
                    &hits,
                    &case.expect_absent,
                    cfg.hook.threshold,
                    cfg.hook.max_tokens,
                ));
            }

            if !case.is_negative() {
                all_uris.push(hits.iter().map(|h| h.uri.clone()).collect::<Vec<_>>());
                // Same list, carrying the relevance the gate compares, so the
                // curve's recall column can re-score it under each candidate.
                samples
                    .ranked
                    .push(hits.iter().map(|h| (h.uri.clone(), h.relevance)).collect());
            }

            // Stage attribution. Deliberately OUTSIDE `timings`: p50 and p95
            // must stay a measurement of the pipeline, not of this tool.
            if tier == hook_tier && !uri_to_doc.is_empty() {
                let t1 = std::time::Instant::now();
                if let Ok(ex) = retriever.search_explained(&case.query, &profile, 0.0, hook_tokens)
                {
                    let stage = |name: &str| -> std::collections::HashSet<String> {
                        ex.stages
                            .iter()
                            .filter(|s| s.name == name)
                            .flat_map(|s| s.hits.iter().map(|h| h.chunk_id.clone()))
                            .collect()
                    };
                    let (vec_ids, kw_ids, graph_ids) =
                        (stage("vector"), stage("bm25"), stage("graph"));
                    let want_docs: std::collections::HashSet<String> = want
                        .iter()
                        .filter_map(|uri| uri_to_doc.get(uri).cloned())
                        .collect();
                    let negative = case.is_negative();
                    for h in &ex.fused {
                        // The same two classes as the pools above, reached
                        // through doc_id because `HitSummary` carries no uri.
                        // A positive case's non-target hits are dropped: they
                        // are not what the gate is calibrated against.
                        if !negative && !want_docs.contains(&h.doc_id) {
                            continue;
                        }
                        origin_samples.push(OriginSample {
                            relevance: h.relevance,
                            origin: origin_of(&h.chunk_id, &vec_ids, &kw_ids, &graph_ids),
                            negative,
                        });
                    }
                }
                origin_ms += t1.elapsed().as_millis();
            }
        }

        per_tier.push(TierClasses {
            tier: profile.name.to_string(),
            answer_hits: samples.answer_hits.len(),
            min_answer: samples.answer_hits.iter().copied().reduce(f32::min),
            negative_hits: samples.negative_hits.len(),
            max_negative: samples.negative_hits.iter().copied().reduce(f32::max),
        });
        per_tier_samples.push(samples);

        timings.sort_unstable();
        let p = |q: f64| {
            timings
                .get(((timings.len() as f64 - 1.0) * q) as usize)
                .copied()
                .unwrap_or(0)
        };
        // `recall_at_k_any`, not `recall_at_k`: `expected` now carries EVERY
        // acceptable document per case. The persisted report and the printed
        // table share this one value, so the dashboard cannot show a different
        // number from the table it was computed beside.
        let recall = recall_at_k_any(&all_uris, &expected, 5);
        report.push(BenchTier {
            tier: profile.name.to_string(),
            recall_at_5: recall,
            p50_ms: p(0.50),
            p95_ms: p(0.95),
            absent: absent_rows,
        });
        println!(
            "{:<12} {:>10.2} {:>10} {:>10}",
            profile.name,
            recall,
            p(0.50),
            p(0.95)
        );
    }

    if !golden.memory_case.is_empty() {
        match memory_section(cfg, &golden.memory_case, cfg.hook.threshold) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("br8n: memory section skipped — {e:#}"),
        }
    }

    // Per case, never averaged. A run where `expect_absent` improves while the
    // historical cases regress is the feature FAILING, and an aggregate reports
    // it as a win.
    // `.get`, not `[]`. `Config::quality_for` does NOT clamp — only
    // `Profile::tier` does — so `[hook] quality = 9` in a config file reaches
    // here as 9 and indexes past a 5-element vector. That is a panic in a
    // measurement tool, after it has already printed its table. The sibling
    // read further down already uses `.get` for the same reason.
    let hook_rows: &[AbsentRow] = report
        .get(hook_tier as usize)
        .map(|t| t.absent.as_slice())
        .unwrap_or(&[]);
    if !hook_rows.is_empty() {
        println!("\nexpect_absent at tier {hook_tier} (the tier the hook queries):");
        println!("  {:<44} {:<9} ungated rank", "query", "injected");
        for r in hook_rows {
            let q: String = r.query.chars().take(42).collect();
            println!(
                "  {:<44} {:<9} {}",
                q,
                if r.injected { "YES" } else { "no" },
                r.rank
                    .map_or("not retrieved".to_string(), |n| n.to_string())
            );
        }
        println!(
            "  NOTE: recall above is UNGATED; this table is GATED. They answer \
             different questions and do not contradict each other."
        );
    }

    // Best-effort: a bench that cannot persist its table still prints it.
    // Provenance is written WITH the table and never separately — a table on
    // disk with no record of what produced it is the state `parse_report` has
    // to tolerate, not one this binary should create.
    if let Err(e) = write_report(&BenchReport {
        provenance: Some(provenance),
        tiers: report,
    }) {
        eprintln!("br8n: could not write {}: {e}", report_path().display());
    }

    // The old `suggest_threshold`/`separated_cleanly` pair has not been deleted
    // — it moved into the `TooFewNegatives` arm below, which is the only arm a
    // golden set with no `expect_none` cases can reach. That is what keeps the
    // output for the live 196-case set what it was.
    match recommend_threshold(&relevant_scores, &negative_scores, negative_cases) {
        Recommendation::Calibrated {
            threshold,
            min_relevant,
            max_negative,
            negative_cases,
        } => println!(
            "\nsuggested hook threshold: {threshold:.2}\n  \
             measured over {negative_cases} negative case(s): answers scored down to \
             {min_relevant:.3},\n  \
             queries that should match nothing scored up to {max_negative:.3}.\n  \
             add to {}:\n  [hook]\n  threshold = {threshold:.2}",
            Config::config_path().display(),
        ),
        Recommendation::Overlapping {
            keep,
            min_relevant,
            max_negative,
            negative_cases,
        } => println!(
            "\nno separator: over {negative_cases} negative case(s), queries that should\n\
             match nothing scored up to {max_negative:.3} while real answers scored down to\n\
             {min_relevant:.3}. Any gate that admits the answers admits the noise, so the\n\
             default of {keep:.2} stands until the corpus or the ranking changes."
        ),
        Recommendation::TooFewNegatives {
            keep,
            negative_cases,
        } => {
            // No calibration is possible, so report the in-corpus classes for
            // what they are worth and say plainly what is missing. The
            // "irrelevant" class here is every non-target hit of a positive
            // case, which in a cross-linked vault is mostly still on topic —
            // it overlaps by construction, and more positive cases only
            // lengthen both tails.
            let suggested = suggest_threshold(&relevant_scores, &irrelevant_scores);
            if separated_cleanly(&relevant_scores, &irrelevant_scores) {
                println!(
                    "\nsuggested hook threshold: {suggested:.2}\n  add to {}:\n  [hook]\n  threshold = {suggested:.2}",
                    Config::config_path().display(),
                );
            } else {
                println!(
                    "\nrelevant and irrelevant results overlap on this corpus, so there is no\n\
                     clean separator to recommend. Keeping the default of {suggested:.2}.\n\
                     Add more cases to {} — a handful of notes cannot separate them.",
                    golden_file.display()
                );
            }
            println!(
                "\nthe gate itself is uncalibrated: {negative_cases} of the cases in {p} assert\n\
                 that nothing should match, and it takes {need} to measure a separator. Without\n\
                 them \"irrelevant\" means every non-target hit, most of which are still on topic.\n\
                 Add cases like:\n\n\
                 [[case]]\n\
                 query = \"something your corpus genuinely has no answer for\"\n\
                 expect_none = true\n\n\
                 Until then the shipped default of {keep:.2} is unvalidated, not wrong.",
                p = golden_file.display(),
                need = MIN_NEGATIVE_CASES,
            );
        }
    }

    // The evidence under the verdict above.
    //
    // `Recommendation` answers ONE question and, when the classes overlap,
    // prints a minimum and a maximum and stops. Two single samples out of
    // hundreds are not something a reader can choose a gate from, and choosing
    // one is the whole job. What follows describes the same two pools instead
    // of reducing them, says which TIER each of those two extremes came from,
    // splits both classes by the stage that produced the hit, and finally
    // prices every candidate gate on both sides at once.
    //
    // Printed only when both classes actually produced hits. A golden set with
    // no `expect_none` cases reaches `TooFewNegatives` above and gets exactly
    // the output it got before.
    if !relevant_scores.is_empty() && !negative_scores.is_empty() {
        println!("\nwhat the gate is separating, pooled over all five tiers");
        println!(
            "  answers   = every hit on a document a positive case accepts\n  \
             negatives = every hit returned for a case asserting `expect_none`\n  \
             both weighted exactly as the hook's gate reads them"
        );
        println!(
            "\n  {:<10} {:>6} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
            "class", "n", "min", "p10", "p25", "p50", "p75", "p90", "max"
        );
        for (name, v) in [
            ("answers", &relevant_scores),
            ("negatives", &negative_scores),
        ] {
            if let Some(s) = spread(v) {
                println!(
                    "  {:<10} {:>6} {:>8.3} {:>8.3} {:>8.3} {:>8.3} {:>8.3} {:>8.3} {:>8.3}",
                    name, s.n, s.min, s.p10, s.p25, s.p50, s.p75, s.p90, s.max
                );
            }
        }

        println!(
            "\n  which tier each of those extremes came from — the hook runs tier {hook_tier}\n  \
             {:<12} {:>8} {:>9} {:>10} {:>9}",
            "tier", "answers", "lowest", "negatives", "highest"
        );
        for t in &per_tier {
            let fmt = |v: Option<f32>| match v {
                Some(x) => format!("{x:.3}"),
                None => "-".to_string(),
            };
            println!(
                "  {:<12} {:>8} {:>9} {:>10} {:>9}",
                t.tier,
                t.answer_hits,
                fmt(t.min_answer),
                t.negative_hits,
                fmt(t.max_negative)
            );
        }

        if origin_samples.is_empty() {
            println!(
                "\n  stage attribution was skipped: the corpus could not be enumerated, so a\n  \
                 hit's doc_id could not be matched to the documents a case accepts."
            );
        } else {
            println!(
                "\n  what produced each hit, at tier {hook_tier} only (cost {origin_ms} ms)\n  \
                 `vector` measured its own cosine and returned the chunk because of it;\n  \
                 `keyword` and `graph` had one backfilled afterwards by the measure stage.\n\n  \
                 {:<10} {:<13} {:>6} {:>8} {:>8} {:>8}",
                "class", "origin", "n", "min", "p50", "max"
            );
            for (class, negative) in [("answers", false), ("negatives", true)] {
                for (origin, s) in origin_breakdown(&origin_samples, negative) {
                    println!(
                        "  {:<10} {:<13} {:>6} {:>8.3} {:>8.3} {:>8.3}",
                        class,
                        origin.label(),
                        s.n,
                        s.min,
                        s.p50,
                        s.max
                    );
                }
            }
        }

        // The curve, at the hook's own tier. Pooling five tiers here would
        // price a gate against four configurations it will never run at — and
        // the tier table above shows those four disagreeing about where the
        // answer floor is.
        if let Some(samples) = per_tier_samples.get(hook_tier as usize) {
            let curve = trade_off(samples, &expected, &candidate_thresholds());
            println!(
                "\nthreshold trade-off at tier {hook_tier}, the tier the hook queries\n  \
                 hits are what the gate compares; cases are what a user notices. An answer\n  \
                 spread over four chunks loses nothing when its weakest chunk is cut, and a\n  \
                 negative case injects noise once however many of its hits clear the bar.\n  \
                 Case denominators count only cases that retrieved something of that class\n  \
                 at all, so a retrieval miss is never charged to the threshold. recall@5 is\n  \
                 an APPROXIMATION, not a bound: the real pipeline gates before diversity\n  \
                 selection, so MMR renormalises a different pool. Measured against it at\n  \
                 eight gates, this column matched at seven and was 0.015 high at one.\n"
            );
            print!("{}", trade_off_table(&curve));
            match cheapest_clean_gate(&curve) {
                Some(r) => println!(
                    "  the lowest gate on this ladder that injects into no negative case is \
                     {:.2};\n  it costs {} of {} answer case(s) ({}) and takes recall@5 from \
                     {:.2} to {:.2}.",
                    r.threshold,
                    r.answer_cases_lost,
                    curve.answer_cases,
                    pct(r.answer_cases_lost, curve.answer_cases).trim(),
                    curve
                        .rows
                        .first()
                        .map(|f| f.recall_at_5)
                        .unwrap_or(r.recall_at_5),
                    r.recall_at_5,
                ),
                None => println!(
                    "  no gate on this ladder shuts every negative case out: the highest \
                     candidate\n  ({:.2}) still admits {} of {}.",
                    curve.rows.last().map(|r| r.threshold).unwrap_or(0.0),
                    curve
                        .rows
                        .last()
                        .map(|r| r.negative_cases_injected)
                        .unwrap_or(0),
                    curve.negative_cases,
                ),
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod score_absent_tests {
    use super::*;
    use crate::store::Hit;

    /// Same fixture shape as `retrieve::mod::tests::hit` — one field varies
    /// (`relevance`), everything else is fixed so the tie-break and gate logic
    /// under test are the only things that can move the assertion.
    fn hit(uri: &str, relevance: f32) -> Hit {
        Hit {
            chunk_id: uri.into(),
            doc_id: format!("doc-of-{uri}"),
            text: "x".repeat(50),
            heading_path: String::new(),
            uri: uri.into(),
            title: uri.into(),
            page_no: None,
            score: 0.0,
            relevance,
            source_type: "markdown".into(),
            inbound: 0,
            lifecycle: Default::default(),
            last_used: None,
            memory: None,
        }
    }

    /// The commit that introduced `expect_absent` claimed its scoring could
    /// only be checked on a live run. It could not: the logic is pure, and
    /// this fixture proves it by putting the target BELOW the gate at ungated
    /// position 3 — a fact only visible if `rank` is read off the UNGATED hit
    /// list. Read it off the gated list instead (the mutation this test
    /// exists to catch) and the target has already been filtered out, so its
    /// position collapses to `None`.
    #[test]
    fn rank_is_read_from_the_ungated_list_not_the_gated_one() {
        let hits = vec![
            hit("file:///a.md", 0.90),      // kept, rank 1
            hit("file:///b.md", 0.80),      // kept, rank 2
            hit("file:///target.md", 0.50), // BELOW threshold, rank 3, ungated only
            hit("file:///d.md", 0.75),      // kept, rank 4
        ];
        let expect_absent = vec!["file:///target.md".to_string()];

        let rows = score_absent("q", &hits, &expect_absent, 0.70, 10_000);

        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].rank,
            Some(3),
            "rank must be the target's position in the UNGATED hit list"
        );
        assert!(
            !rows[0].injected,
            "the target's relevance is below the threshold, so it must not be injected"
        );
    }
}
