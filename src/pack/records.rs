use anyhow::{Context, Result};
use memmap2::Mmap;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

pub const REC_FILE: &str = "pack.rec";
pub const IDX_FILE: &str = "pack.recidx";

/// Everything a `Hit` needs that is not a score. Hydration reads one of these
/// per surviving row, so nothing here is on the hot path until the pipeline has
/// already chosen its ten results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub chunk_id: String,
    pub doc_id: String,
    pub text: String,
    pub heading_path: String,
    pub uri: String,
    pub title: String,
    pub page_no: Option<i64>,
    pub source_type: String,
    /// How many documents link to THIS row's document — `authority_lift`'s
    /// numerator, and the only reason authority weighting used to force a
    /// store open on the prompt path.
    ///
    /// NOT stored in `pack.rec`. It is `#[serde(skip)]` because `pack.links`
    /// is its single source: `Pack::hydrate` fills it in from that file using
    /// the row ordinal it already has in hand, so there are never two copies
    /// of the number that could disagree, and `pack.rec`'s bytes are exactly
    /// what they were before this field existed.
    ///
    /// Consequently it is 0 on a `Record` that has not come through
    /// `Pack::hydrate` — including every `Record` on the WRITE path
    /// (`Store::all_rows_for_pack` never sets it, and `Pack::build` ignores
    /// it in favour of the count map it is passed).
    #[serde(skip)]
    pub inbound: u32,
    /// This row's document lifecycle, carried the same way as `inbound`:
    /// `#[serde(skip)]`, never written into `pack.rec`, and filled in by
    /// `Pack::hydrate` from `pack.status` using the row ordinal it already
    /// has in hand. `Lifecycle::Current` (the zero discriminant) on a
    /// `Record` that has not come through hydrate — including every `Record`
    /// on the write path, where `Pack::build` reads lifecycle from the
    /// `doc_id`-keyed map it is passed, not from this field.
    #[serde(skip)]
    pub lifecycle: super::status::Lifecycle,
    #[serde(skip)]
    pub last_used: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<crate::memory::MemoryFacts>,
}

/// Write rows in ordinal order, plus a u64 offset table.
///
/// The offset table is what makes `get` O(1) without parsing anything before
/// row N. Records are length-prefixed by the encoding itself (JSON lines would
/// break on the newlines that fill this corpus's transcripts).
pub fn write(dir: &Path, rows: &[Record]) -> Result<()> {
    let mut offsets: Vec<u64> = Vec::with_capacity(rows.len() + 1);
    let mut end = 0u64;
    super::write_buffered(&dir.join(REC_FILE), |out| {
        for r in rows {
            offsets.push(end);
            let bytes = serde_json::to_vec(r)?;
            out.write_all(&bytes)?;
            end += bytes.len() as u64;
        }
        Ok(())
    })?;
    // A trailing sentinel means `get` needs no special case for the last row.
    offsets.push(end);
    super::write_buffered(&dir.join(IDX_FILE), |out| {
        for o in &offsets {
            out.write_all(&o.to_le_bytes())?;
        }
        Ok(())
    })
}

pub struct Reader {
    rec: Mmap,
    idx: Mmap,
}

impl Reader {
    /// SAFETY: the pack is immutable once published — `reindex_swap` builds it
    /// in a shadow directory and installs it by rename, so the bytes behind
    /// this mapping are never modified in place. A reader either maps the whole
    /// old generation or the whole new one.
    pub fn open(dir: &Path) -> Result<Reader> {
        let rec_f = std::fs::File::open(dir.join(REC_FILE))
            .with_context(|| format!("open {}", dir.join(REC_FILE).display()))?;
        let idx_f = std::fs::File::open(dir.join(IDX_FILE))
            .with_context(|| format!("open {}", dir.join(IDX_FILE).display()))?;
        let rec = unsafe { Mmap::map(&rec_f) }.context("mmap pack.rec")?;
        let idx = unsafe { Mmap::map(&idx_f) }.context("mmap pack.recidx")?;
        anyhow::ensure!(
            idx.len() % 8 == 0 && idx.len() >= 8,
            "pack.recidx is truncated ({} bytes)",
            idx.len()
        );
        Ok(Reader { rec, idx })
    }

    pub fn len(&self) -> usize {
        self.idx.len() / 8 - 1
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn offset(&self, i: usize) -> u64 {
        let b = &self.idx[i * 8..i * 8 + 8];
        u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
    }

    pub fn get(&self, row: usize) -> Result<Record> {
        anyhow::ensure!(row < self.len(), "row {row} is past the end of the pack");
        let (start, end) = (self.offset(row) as usize, self.offset(row + 1) as usize);
        anyhow::ensure!(
            end <= self.rec.len() && start <= end,
            "pack.rec is truncated"
        );
        serde_json::from_slice(&self.rec[start..end])
            .with_context(|| format!("decode record {row}"))
    }

    /// Find the row whose `chunk_id` matches, by binary search.
    ///
    /// REQUIRES rows to be sorted by `chunk_id` ascending — `Pack::build`
    /// checks that invariant once at write time (see its doc comment) so this
    /// method never has to. Records are written in the order
    /// `Store::all_rows_for_pack` returns them, which is `ORDER BY c.id`
    /// precisely so this search is valid.
    ///
    /// Returns `Ok(None)` when no row has this id — never a wrong neighbour.
    /// This is `Pack::cosine_for`'s replacement for the linear scan it used to
    /// do: ~log2(rows) decodes instead of up to the whole pack.
    pub fn find_by_chunk_id(&self, chunk_id: &str) -> Result<Option<usize>> {
        let mut lo = 0usize;
        let mut hi = self.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let rec = self.get(mid)?;
            match rec.chunk_id.as_str().cmp(chunk_id) {
                std::cmp::Ordering::Equal => return Ok(Some(mid)),
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        Ok(None)
    }
}
