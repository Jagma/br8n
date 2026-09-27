//! Per-surface source weights, end to end through the real pipeline.
//!
//! `relevance` is EFFECTIVE relevance — cosine discounted by source authority —
//! and it is weighted BEFORE the gate reads it. That is deliberate (see the note
//! in `retrieve::Retriever::run`), but it means the weight decides admission and
//! not merely ordering: at the global transcript weight of 0.45 a transcript
//! needs a 1.56 cosine to clear the hook's 0.70 gate and 1.22 to clear the MCP's
//! 0.55, so no transcript could ever be injected or returned by an explicit
//! search. These tests pin the way out: the hook keeps the strict weight because
//! it fires unasked, while `[mcp.weights]` lets a deliberate search admit a
//! transcript that genuinely is the best answer.

use br8n::config::{Config, Surface};
use br8n::embed::Embedder;
use br8n::index::Indexer;
use br8n::model::{Document, SourceType};
use br8n::retrieve::Retriever;
use br8n::store::Store;

const TRANSCRIPT_URI: &str = "file:///sessions/rust-decision.jsonl";
const NOTE_URI: &str = "file:///notes/rust-adr.md";

/// Pins the cosine instead of hoping for one. Every document embeds to the same
/// unit vector and the query sits at exactly 0.70 cosine from it; the store
/// turns cosine distance into relevance as `1 - dist/2`, so every hit arrives at
/// an unweighted 0.85 — the strong end of the 0.727-0.864 band measured for real
/// matches with `qwen3-embedding:0.6b`, which is what makes the arithmetic below
/// a statement about this system rather than about a fixture.
struct FixedAngleEmbedder;

/// cos = 0.70 -> distance 0.30 -> relevance 0.85.
const UNWEIGHTED_RELEVANCE: f32 = 0.85;

impl Embedder for FixedAngleEmbedder {
    fn embed_documents(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0, 0.0, 0.0]).collect())
    }
    fn embed_query(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        Ok(vec![0.7, (1.0f32 - 0.49).sqrt(), 0.0, 0.0])
    }
    fn warm(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn model_id(&self) -> String {
        "fixed-angle@4".into()
    }
    fn dimensions(&self) -> usize {
        4
    }
}

/// One transcript and one note, equally similar to the query. The note is the
/// control: it shares the transcript's cosine exactly, so anything that happens
/// to only one of them is the weighting and nothing else.
fn corpus() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    {
        let idx = Indexer::new(
            Store::open(dir.path(), 4).unwrap(),
            Box::new(FixedAngleEmbedder),
            Config::default(),
        );
        idx.index_documents(&[
            Document::new(
                SourceType::Transcript,
                TRANSCRIPT_URI,
                "Session 2026-08-01",
                "We picked Rust for the hook because a Python start-up cost of 200ms is paid on every prompt.",
            ),
            Document::new(
                SourceType::Markdown,
                NOTE_URI,
                "ADR: Rust",
                "# ADR: Rust\n\nThe hook runs on every prompt, so start-up cost dominates.",
            ),
        ])
        .unwrap();
    }
    let store = Store::open(dir.path(), 4).unwrap();
    (dir, store)
}

fn search(cfg: &Config, store: Store, surface: Surface) -> Vec<br8n::store::Hit> {
    // `Retriever::new` with no `.with_pack(..)` now refuses the query
    // outright (see `tests/it/retrieve_profile.rs`'s
    // `tier_1_without_a_pack_refuses_rather_than_falling_back_to_the_store`),
    // so a pack is built here from the same rows a real index would publish.
    // This file exercises it only incidentally, to test per-surface
    // weighting.
    let rows = store.all_rows_for_pack().unwrap();
    let pack_dir = tempfile::tempdir().unwrap();
    br8n::pack::Pack::build(
        pack_dir.path(),
        "fixed-angle@4",
        4,
        rows,
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    let pack = br8n::pack::Pack::open(pack_dir.path(), "fixed-angle@4", 4).unwrap();

    Retriever::new(store, Box::new(FixedAngleEmbedder), String::new())
        .with_pack(Some(pack))
        .with_weights(cfg.weights_for(surface))
        .search_gated(
            "why did we pick rust",
            &cfg.profile_for(surface),
            cfg.surface(surface).threshold,
        )
        .unwrap()
}

fn relevance_of(hits: &[br8n::store::Hit], uri: &str) -> Option<f32> {
    hits.iter().find(|h| h.uri == uri).map(|h| h.relevance)
}

fn per_surface_config() -> Config {
    toml::from_str(
        r#"
        [weights]
        transcript = 0.45

        [mcp.weights]
        transcript = 0.85
        "#,
    )
    .unwrap()
}

#[test]
fn the_fixture_pins_an_unweighted_relevance_of_0_85() {
    // Everything below is arithmetic on this number, so it is asserted rather
    // than assumed. The note carries weight 1.0 at every surface.
    let (_d, store) = corpus();
    let hits = search(&per_surface_config(), store, Surface::Mcp);
    let note = relevance_of(&hits, NOTE_URI).expect("the note must be retrievable at all");
    assert!(
        (note - UNWEIGHTED_RELEVANCE).abs() < 1e-3,
        "fixture drifted: expected {UNWEIGHTED_RELEVANCE}, got {note}"
    );
}

#[test]
fn a_strong_transcript_clears_the_mcp_gate_when_that_surface_allows_it() {
    let (_d, store) = corpus();
    let cfg = per_surface_config();
    let hits = search(&cfg, store, Surface::Mcp);

    let got = relevance_of(&hits, TRANSCRIPT_URI).unwrap_or_else(|| {
        panic!(
            "a 0.85 transcript at the configured mcp weight of 0.85 must clear the {} gate",
            cfg.surface(Surface::Mcp).threshold
        )
    });
    // 0.85 * 0.85 = 0.7225, comfortably over 0.55 — and still ranked below the
    // note, which is the ordering benefit the weight was introduced for.
    assert!((got - 0.7225).abs() < 1e-3, "expected 0.7225, got {got}");
    assert_eq!(
        hits[0].uri, NOTE_URI,
        "admitting a transcript must not promote it above the note"
    );
}

#[test]
fn the_same_transcript_stays_out_of_the_hooks_unasked_injection() {
    let (_d, store) = corpus();
    let cfg = per_surface_config();
    let hits = search(&cfg, store, Surface::Hook);

    assert!(
        relevance_of(&hits, TRANSCRIPT_URI).is_none(),
        "0.85 * 0.45 = 0.3825 is under the hook's 0.70; the hook fires unasked and must stay strict"
    );
    assert!(
        relevance_of(&hits, NOTE_URI).is_some(),
        "the note must still be injected — otherwise this test proves only that the gate rejects everything"
    );
}

#[test]
fn without_the_override_the_transcript_cannot_clear_the_mcp_gate_either() {
    // The state of the world before per-surface weights, and the reason they
    // exist: one weight serving both jobs put transcripts permanently out of
    // reach of BOTH gated surfaces, leaving only the ungated CLI.
    let (_d, store) = corpus();
    let cfg: Config = toml::from_str("[weights]\ntranscript = 0.45\n").unwrap();
    let hits = search(&cfg, store, Surface::Mcp);

    assert!(
        relevance_of(&hits, TRANSCRIPT_URI).is_none(),
        "0.85 * 0.45 = 0.3825 cannot clear 0.55 — that is the bug, not the fix"
    );
    assert!(
        relevance_of(&hits, NOTE_URI).is_some(),
        "the note is unaffected by the transcript weight"
    );
}

/// Authority must be a LIFT and never a penalty.
///
/// 46% of the notes in the vault this was measured on have no inbound wikilink,
/// and transcripts have none by construction. A formulation that scaled a
/// document DOWN for being unlinked would bury most of the corpus on a property
/// it cannot have — and the leaves of a link graph are usually where the
/// answers are, not the hubs.
#[test]
fn authority_lifts_the_linked_and_never_demotes_the_rest() {
    use br8n::config::Weights;

    let off = Weights {
        authority: 0.0,
        ..br8n::config::Config::default().weights
    };
    let on = Weights {
        authority: 0.5,
        ..br8n::config::Config::default().weights
    };

    // Off: every document is untouched, however well linked.
    assert_eq!(off.authority_lift(14, 14), 1.0);
    assert_eq!(off.authority_lift(0, 14), 1.0);

    // On: unlinked is exactly neutral, never below.
    assert_eq!(
        on.authority_lift(0, 14),
        1.0,
        "an unlinked document must not be demoted"
    );

    // On: the most-linked reaches exactly 1.0 + authority, and no further.
    assert_eq!(on.authority_lift(14, 14), 1.5);

    // On: partial linkage lands between, monotonically.
    let half = on.authority_lift(7, 14);
    assert!(
        (1.0..1.5).contains(&half) && half > on.authority_lift(3, 14),
        "lift must increase with inbound links, got {half}"
    );

    // A corpus with no links at all cannot divide by zero.
    assert_eq!(on.authority_lift(0, 0), 1.0);
}

#[test]
fn authority_is_off_in_the_shipped_defaults() {
    // Opt-in: link structure is only a signal where linking is a real practice.
    assert_eq!(br8n::config::Config::default().weights.authority, 0.0);
}
