//! Inbound link counts, one per row, joined by the row ordinal.
//!
//! ```text
//! magic "BRNLINK1" | u32 rows | u32 max_inbound        16 B
//! rows x u32 inbound count, in row-ordinal order
//! ```
//!
//! This is the pack's copy of `Store::inbound_link_counts`, which is the ONLY
//! reason authority weighting used to force a `Database::new` on the prompt
//! path — measured at 0.22-0.27s per tier-1 query with `authority = 0.3`
//! against 0.10-0.12s with it off, on the same binary and the same index.
//!
//! Stored PER ROW rather than per document on purpose. The row ordinal is the
//! only join key between pack files (see `Manifest`'s doc comment), and a
//! doc_id-keyed side table would have introduced a second, weaker one: a
//! doc_id survives a re-index, so a stale table would attach a plausible count
//! to a document that no longer has those links, with nothing able to notice.
//! A per-row array cannot desynchronise unnoticed, because its length is the
//! row count the manifest already pins — `Pack::open` refuses on a mismatch
//! exactly as it does for the records and the postings.
//!
//! The count is a DOCUMENT property, so every chunk of one document carries
//! the same value. That redundancy costs 4 bytes per chunk (124 KB over the
//! live 31k-row index) and buys the ordinal join; the alternative buys nothing
//! back but the bytes.
//!
//! `max_inbound` is in the header rather than recomputed from the array
//! because the two are not the same number: `Store::inbound_link_counts`
//! returns a map over DOCUMENTS, and its maximum is taken over that map. A
//! document with inbound links but no chunks contributes to that maximum and
//! to no row here. Recomputing from rows would quietly change
//! `authority_lift`'s denominator, and the lift arithmetic must not change —
//! only where the counts come from.

use anyhow::{Context, Result};
use memmap2::Mmap;
use std::path::Path;

pub const LINKS_FILE: &str = "pack.links";

const MAGIC: &[u8; 8] = b"BRNLINK1";
const HEADER: usize = 16;

/// Write the per-row counts. `counts[i]` is the inbound link count of row
/// `i`'s DOCUMENT — the same ordinal `records::Reader::get`,
/// `vectors::Reader::search` and `postings::write` use.
///
/// `max_inbound` is passed in rather than derived; see the module docs.
pub fn write(dir: &Path, counts: &[u32], max_inbound: u32) -> Result<()> {
    let mut buf: Vec<u8> = Vec::with_capacity(HEADER + counts.len() * 4);
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&(counts.len() as u32).to_le_bytes());
    buf.extend_from_slice(&max_inbound.to_le_bytes());
    for c in counts {
        buf.extend_from_slice(&c.to_le_bytes());
    }
    let p = dir.join(LINKS_FILE);
    std::fs::write(&p, &buf).with_context(|| format!("write {}", p.display()))
}

pub struct Reader {
    map: Mmap,
    num_rows: u32,
    max_inbound: u32,
}

// `Mmap` has no `Debug`, so this is a manual impl rather than a derive — only
// so `Result<Reader, _>::unwrap_err()` compiles in tests.
impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("links::Reader")
            .field("rows", &self.num_rows)
            .field("max_inbound", &self.max_inbound)
            .finish()
    }
}

impl Reader {
    /// SAFETY: the pack is immutable once published — `reindex_swap` builds it
    /// in a shadow directory and installs it by rename, so the bytes behind
    /// this mapping are never modified in place.
    pub fn open(dir: &Path) -> Result<Reader> {
        let p = dir.join(LINKS_FILE);
        let f = std::fs::File::open(&p).with_context(|| format!("open {}", p.display()))?;
        let map = unsafe { Mmap::map(&f) }.context("mmap pack.links")?;
        anyhow::ensure!(
            map.len() >= HEADER,
            "{} is truncated: {} bytes, need at least {HEADER} for the header",
            p.display(),
            map.len()
        );
        anyhow::ensure!(
            &map[0..8] == MAGIC,
            "{} has a bad magic number",
            p.display()
        );
        let num_rows = u32::from_le_bytes(map[8..12].try_into().unwrap());
        let max_inbound = u32::from_le_bytes(map[12..16].try_into().unwrap());
        let want = (num_rows as usize)
            .checked_mul(4)
            .and_then(|b| b.checked_add(HEADER))
            .context("pack.links row count overflows usize")?;
        // Exact, not `>=`. A file longer than its header claims is as much a
        // sign of a wrong generation as a short one, and this array carries no
        // per-entry structure that a later check could catch it with.
        anyhow::ensure!(
            map.len() == want,
            "{} is {} bytes but its header claims {num_rows} rows ({want} bytes)",
            p.display(),
            map.len()
        );
        Ok(Reader {
            map,
            num_rows,
            max_inbound,
        })
    }

    pub fn rows(&self) -> usize {
        self.num_rows as usize
    }

    /// The largest inbound count in the corpus — `authority_lift`'s
    /// denominator. See the module docs for why it is stored, not derived.
    pub fn max_inbound(&self) -> u32 {
        self.max_inbound
    }

    /// Inbound links for `row`'s document. A row past the end reads 0, which
    /// `authority_lift` turns into a multiplier of exactly 1.0 — the same
    /// answer as "this document is not linked". `Pack::open` has already
    /// refused any pack whose row count disagrees with the manifest, so this
    /// branch is unreachable through the pack; it exists so a bug here cannot
    /// become a panic on the prompt path.
    pub fn get(&self, row: usize) -> u32 {
        if row >= self.rows() {
            return 0;
        }
        let o = HEADER + row * 4;
        u32::from_le_bytes(self.map[o..o + 4].try_into().unwrap())
    }
}
