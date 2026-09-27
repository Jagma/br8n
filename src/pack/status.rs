//! Per-row lifecycle status, one byte per row, joined by the row ordinal.
//!
//! ```text
//! magic "BRNSTAT1" | u32 rows | u32 version      16 B
//! rows x u8 lifecycle, in row-ordinal order
//! ```
//!
//! A SIDE-FILE, not a pack member: `pack::manifest::FORMAT` is unchanged, so a
//! binary that predates this file opens the pack normally and never looks for
//! it. That is the whole point of the design — a `FORMAT` bump is a live
//! outage with a ~10 minute repair on a large index, and this feature does not
//! need one.
//!
//! Two properties are load-bearing:
//!
//! 1. **`Lifecycle::Current` is the ZERO discriminant.** A zeroed, absent, or
//!    unreadable status file must read as "no demotion anywhere". Were
//!    `Superseded` zero, a missing file would demote the entire corpus and the
//!    hook would still exit 0.
//! 2. **The row count is in this file's own header** and is checked against the
//!    manifest by `Pack::open`. The row ordinal is the only join key between
//!    pack files, so a stale generation would otherwise attach plausible
//!    statuses to changed rows with nothing able to detect it — the same
//!    reasoning that keeps `pack.links` per-row rather than doc_id-keyed.

use anyhow::{Context, Result};
use memmap2::Mmap;
use std::path::Path;

pub const STATUS_FILE: &str = "pack.status";

const MAGIC: &[u8; 8] = b"BRNSTAT1";
const HEADER: usize = 16;
/// Bumped when the BYTE LAYOUT changes. Distinct from `manifest::FORMAT`, which
/// this file deliberately does not touch.
pub const VERSION: u32 = 1;

/// Where a document sits in its lifecycle. Ordered from "no demotion" to "most
/// demoted"; only `Superseded` demotes today (decision 8 ships `Proposed` at
/// 1.0, because most notes have no `status:` at all and land there).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[repr(u8)]
pub enum Lifecycle {
    /// `accepted`, `active` — and every row of an absent status file.
    #[default]
    Current = 0,
    /// `investigating`, or a `Proposed` record with at least one inbound
    /// wikilink ("used once", decision 8).
    Investigating = 1,
    /// `proposed`, `draft`, `shaping`, anything unrecognised, and anything with
    /// no `status:` key — which is every markdown note that is not a decision record.
    Proposed = 2,
    /// `superseded`. The only value that demotes.
    Superseded = 3,
}

impl Lifecycle {
    /// Map the vault's free-text `status:` onto the ladder.
    ///
    /// `has_inbound_link` is decision 8's "used once": a `Proposed` record that
    /// another note links to is `Investigating`. An inbound wikilink is the only
    /// usage signal the store holds — "retrieved once" would need a query log
    /// this tool does not keep.
    pub fn from_status(status: Option<&str>, has_inbound_link: bool) -> Lifecycle {
        let base = match status.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("accepted") | Some("active") => Lifecycle::Current,
            Some("investigating") => Lifecycle::Investigating,
            Some("superseded") => Lifecycle::Superseded,
            _ => Lifecycle::Proposed,
        };
        match (base, has_inbound_link) {
            (Lifecycle::Proposed, true) => Lifecycle::Investigating,
            (b, _) => b,
        }
    }

    /// Read path. An unrecognised byte is `Current`: a file written by a newer
    /// binary must degrade to "no demotion", never to a demotion nobody asked
    /// for and never to a panic.
    pub fn from_byte(b: u8) -> Lifecycle {
        match b {
            1 => Lifecycle::Investigating,
            2 => Lifecycle::Proposed,
            3 => Lifecycle::Superseded,
            _ => Lifecycle::Current,
        }
    }
}

/// Write one byte per row, in row-ordinal order — the same ordinal
/// `records::Reader::get` and `links::Reader::get` use.
pub fn write(dir: &Path, rows: &[Lifecycle]) -> Result<()> {
    let mut buf: Vec<u8> = Vec::with_capacity(HEADER + rows.len());
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&(rows.len() as u32).to_le_bytes());
    buf.extend_from_slice(&VERSION.to_le_bytes());
    buf.extend(rows.iter().map(|l| *l as u8));
    let p = dir.join(STATUS_FILE);
    std::fs::write(&p, &buf).with_context(|| format!("write {}", p.display()))
}

pub struct Reader {
    map: Mmap,
    num_rows: u32,
}

impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("status::Reader")
            .field("rows", &self.num_rows)
            .finish()
    }
}

impl Reader {
    /// SAFETY: the pack is immutable once published — it is built in a shadow
    /// directory and installed by rename, so these bytes are never modified in
    /// place.
    pub fn open(dir: &Path) -> Result<Reader> {
        let p = dir.join(STATUS_FILE);
        let f = std::fs::File::open(&p).with_context(|| format!("open {}", p.display()))?;
        let map = unsafe { Mmap::map(&f) }.context("mmap pack.status")?;
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
        let version = u32::from_le_bytes(map[12..16].try_into().unwrap());
        anyhow::ensure!(
            version == VERSION,
            "{} is version {version}, this binary writes {VERSION} — \
             run `br8n index --compact` to republish it",
            p.display()
        );
        let want = (num_rows as usize)
            .checked_add(HEADER)
            .context("pack.status row count overflows usize")?;
        // Exact, not `>=`. A file longer than its header claims is as much a
        // sign of a wrong generation as a short one, and a flat byte array
        // carries no per-entry structure a later check could catch it with.
        anyhow::ensure!(
            map.len() == want,
            "{} is {} bytes but its header claims {num_rows} rows ({want} bytes)",
            p.display(),
            map.len()
        );
        Ok(Reader { map, num_rows })
    }

    pub fn rows(&self) -> usize {
        self.num_rows as usize
    }

    /// `Current` for a row out of range. A reader must not take the process
    /// down over a row it cannot explain, and `Current` is the no-op.
    pub fn get(&self, row: usize) -> Lifecycle {
        if row >= self.num_rows as usize {
            return Lifecycle::Current;
        }
        Lifecycle::from_byte(self.map[HEADER + row])
    }
}
