use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use super::analyze;

/// Bumped whenever the layout of any pack file changes. A reader that does not
/// recognise the version refuses the pack rather than guessing its layout.
///
/// Bumped to 2 for the `analyzer` field below: a manifest written by format 1
/// has no such field, and a reader must refuse it rather than assume one.
///
/// Bumped to 3 for `rows_with_vectors` below: a manifest written by format 2
/// has no such field, and `serde` would otherwise have to invent a default —
/// most plausibly 0, which would make a COMPLETE format-2 pack look exactly
/// like a declared-vectorless one and silently drop its vector search.
/// Refusing is the only safe answer, same reasoning as the format-2 bump.
///
/// Bumped to 4 for `pack.links`, the per-row inbound link counts that let
/// authority weighting run without opening the database. A format-3 pack has
/// no such file, and there is no honest way to read one: inventing zeros would
/// silently turn every `authority_lift` into 1.0, so a user who has authority
/// switched on would get UNWEIGHTED results that look exactly like weighted
/// ones. That is the house failure mode — refuse and say `br8n index
/// --reindex`, which is what `validate` below does before any file is opened.
pub const FORMAT: u32 = 4;
pub const MANIFEST_FILE: &str = "pack.manifest";

/// What a reader must agree with before it may trust a byte of the pack.
///
/// `rows` and the checksums exist for one reason: the row ordinal is the ONLY
/// join key between the vector index and the record file. If those two files
/// ever come from different generations, every result is silently wrong — the
/// right relevance attached to the wrong document. There is no downstream check
/// that would catch it, so it is caught here or not at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    /// `model@dims+scheme`, exactly as `Embedder::model_id` produces it.
    pub model_id: String,
    pub dims: usize,
    pub rows: usize,
    /// How many of `rows` actually carry a vector. Ordinarily equal to `rows`
    /// — the store still embeds everything before publishing. It exists for
    /// the one case where it is not: phase 1 of an asynchronous index
    /// publishes `pack.fts` and `pack.rec` with `pack.vec` not written at
    /// all, so keyword search works before any embedding has happened. `0`
    /// here is what tells `Pack::open` that a missing `pack.vec` is a
    /// DECLARED state rather than corruption — see its doc comment. A value
    /// strictly between `0` and `rows` is not currently produced by anything
    /// in this codebase; nothing here assumes it cannot occur, but nothing
    /// exercises it either.
    pub rows_with_vectors: usize,
    pub vec_sha: String,
    pub rec_sha: String,
    /// `analyze::ANALYZER`, exactly. `pack.fts`'s postings are produced by
    /// running that analyzer over every record's text; a pack read with a
    /// different one returns wrong postings with no error — the same class
    /// of silent failure as reading vectors from a different embedding
    /// model, checked by `model_id` above. Given the same treatment here.
    pub analyzer: String,
}

impl Manifest {
    pub fn write(&self, dir: &Path) -> Result<()> {
        let p = dir.join(MANIFEST_FILE);
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&p, json).with_context(|| format!("write {}", p.display()))
    }

    pub fn read(dir: &Path) -> Result<Manifest> {
        let p = dir.join(MANIFEST_FILE);
        let s = std::fs::read_to_string(&p)
            .with_context(|| format!("no pack manifest at {}", p.display()))?;
        serde_json::from_str(&s).with_context(|| format!("parse {}", p.display()))
    }

    /// Refuse rather than degrade. Every branch here is a condition under which
    /// reading the pack would produce confident, wrong results.
    pub fn validate(&self, model_id: &str, dims: usize) -> Result<()> {
        // `--compact`, NOT `--reindex`. A format bump changes only how the pack
        // is written; the database's rows and their stored embeddings are
        // untouched, and `--compact` rebuilds the pack from those with no
        // Ollama traffic at all. Measured on the live index when FORMAT went
        // 3 -> 4: 33,648 chunks reused, 0 written, ~10 minutes, against the ~86
        // minutes a full re-embed of the same corpus costs. This message used to
        // name `--reindex` and sent the first person to hit it — the author of
        // the bump — down the expensive path on their own index.
        anyhow::ensure!(
            self.format == FORMAT,
            "pack format {} is not readable by this binary (expects {FORMAT}); \
             run `br8n index --compact` to rebuild the pack from the rows you \
             already have (no re-embedding), or `br8n index --reindex` to \
             rebuild from source",
            self.format
        );
        anyhow::ensure!(
            self.model_id == model_id,
            "pack was built with embedding model `{}` but `{model_id}` is \
             configured; run `br8n index --reindex` to rebuild",
            self.model_id
        );
        anyhow::ensure!(
            self.dims == dims,
            "pack has {} dimensions but {dims} are configured; \
             run `br8n index --reindex` to rebuild",
            self.dims
        );
        anyhow::ensure!(
            self.analyzer == analyze::ANALYZER,
            "pack was built with analyzer `{}` but this binary's analyzer is \
             `{}`; a pack built by one analyzer and queried by another \
             returns wrong postings with no error — run `br8n index --compact` \
             to rebuild the pack from the rows you already have (no \
             re-embedding), or `br8n index --reindex` to rebuild from source",
            self.analyzer,
            analyze::ANALYZER
        );
        anyhow::ensure!(
            self.rows_with_vectors <= self.rows,
            "pack manifest claims {} rows have vectors out of only {} rows total",
            self.rows_with_vectors,
            self.rows
        );
        Ok(())
    }
}
