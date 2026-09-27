//! Flat, mmap-able, impact-ordered BM25 postings — ported from
//! a standalone spike (`Postings::open`, `lookup`, `posting`). The spike
//! measured it: full accumulation
//! over 31k rows costs 0.495 ms against `fts_search`'s 14.9 ms, f32 impacts
//! reproduce an f64 reference in identical order on all 100 golden queries,
//! and the file is 612 B/row at 17,400 rows/s to build.
//!
//! ```text
//! magic "BRNPOST1" | u32 rows | u32 terms | f32 avgdl | u32 pad   24 B
//! term table: terms x 24 B { u32 str_off, u32 str_len, u64 post_off, u32 df, u32 pad }
//! term string blob, lexicographically sorted
//! postings: per term, df x (u32 row, f32 impact), IMPACT-DESCENDING
//! ```
//!
//! The stored impact is a term's COMPLETE BM25 contribution for that row —
//! `idf * tf * (k1+1) / (tf + k1 * (1 - b + b * dl/avgdl))` — so a query needs
//! no document statistics at read time: it is a sum of pre-scored postings,
//! never a similarity or a relevance. That score belongs on `Hit.score`
//! (ordinal, unbounded) — a later task's problem, not this one's.

use anyhow::{Context, Result};
use memmap2::Mmap;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use super::analyze::analyze;

pub const FTS_FILE: &str = "pack.fts";

const MAGIC: &[u8; 8] = b"BRNPOST1";
const HEADER: usize = 24;
const TERM_ENTRY: usize = 24;

const K1: f32 = 1.2;
const B: f32 = 0.75;

/// Build the postings file over `rows`, where `rows[i]` is the already
/// analyzed terms of row `i` — the same ordinal `records::Reader::get` and
/// `vectors::Reader::search` use. The caller chooses the row order; this
/// module never reorders it.
pub fn write(dir: &Path, rows: &[Vec<String>]) -> Result<()> {
    let mut builder = Builder::default();
    for row in rows {
        builder.add_row(row);
    }
    builder.write(dir)
}

#[derive(Default)]
pub struct Builder {
    row_lengths: Vec<u32>,
    postings_by_term: HashMap<String, Vec<(u32, u32)>>,
}

impl Builder {
    pub fn add_row(&mut self, terms: &[String]) {
        // Pass 1: term frequencies per row, and document lengths.
        let row = self.row_lengths.len() as u32;
        self.row_lengths.push(terms.len() as u32);
        let mut term_frequencies: HashMap<&str, u32> = HashMap::new();
        for t in terms {
            *term_frequencies.entry(t.as_str()).or_default() += 1;
        }
        for (term, tf) in term_frequencies {
            match self.postings_by_term.get_mut(term) {
                Some(postings) => postings.push((row, tf)),
                None => {
                    self.postings_by_term
                        .insert(term.to_owned(), vec![(row, tf)]);
                }
            }
        }
    }

    pub fn write(self, dir: &Path) -> Result<()> {
        let Builder {
            row_lengths: dl,
            postings_by_term,
        } = self;
        let n = dl.len();
        let total_len: u64 = dl.iter().map(|&x| u64::from(x)).sum();
        // An empty corpus has no rows to average over; avgdl is unused when there
        // are no postings to score, so any finite value is safe.
        let avgdl = if n == 0 {
            0.0
        } else {
            total_len as f32 / n as f32
        };

        // Pass 2: impacts, sorted impact-descending within each term's run. Keys
        // are sorted lexicographically so the reader can binary search the term
        // table.
        let mut terms: Vec<(String, Vec<(u32, u32)>)> = postings_by_term.into_iter().collect();
        terms.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let nf = n as f32;
        let runs: Vec<(String, Vec<(u32, f32)>)> = terms
            .into_iter()
            .map(|(term, plist)| {
                let df = plist.len() as f32;
                let idf = (1.0 + (nf - df + 0.5) / (df + 0.5)).ln();
                let mut run: Vec<(u32, f32)> = plist
                    .into_iter()
                    .map(|(row, tf)| {
                        let tf = tf as f32;
                        let norm = 1.0 - B + B * (dl[row as usize] as f32) / avgdl;
                        (row, idf * (tf * (K1 + 1.0)) / (tf + K1 * norm))
                    })
                    .collect();
                run.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(&b.0)));
                (term, run)
            })
            .collect();

        let blob_len: usize = runs.iter().map(|(term, _)| term.len()).sum();
        let blob_start = HEADER + runs.len() * TERM_ENTRY;
        let post_start = blob_start + blob_len;

        // Serialise: header, term table, string blob, postings.
        super::write_buffered(&dir.join(FTS_FILE), |out| {
            out.write_all(MAGIC)?;
            out.write_all(&(n as u32).to_le_bytes())?;
            out.write_all(&(runs.len() as u32).to_le_bytes())?;
            out.write_all(&avgdl.to_le_bytes())?;
            out.write_all(&0u32.to_le_bytes())?;
            let mut str_off = blob_start;
            let mut post_off = post_start;
            for (term, run) in &runs {
                out.write_all(&(str_off as u32).to_le_bytes())?;
                out.write_all(&(term.len() as u32).to_le_bytes())?;
                out.write_all(&(post_off as u64).to_le_bytes())?;
                out.write_all(&(run.len() as u32).to_le_bytes())?;
                out.write_all(&0u32.to_le_bytes())?;
                str_off += term.len();
                post_off += run.len() * 8;
            }
            for (term, _) in &runs {
                out.write_all(term.as_bytes())?;
            }
            for (_, run) in &runs {
                for (row, imp) in run {
                    out.write_all(&row.to_le_bytes())?;
                    out.write_all(&imp.to_le_bytes())?;
                }
            }
            Ok(())
        })
    }
}

pub struct Reader {
    map: Mmap,
    num_rows: u32,
    num_terms: u32,
}

impl Reader {
    /// SAFETY: the pack is immutable once published — `reindex_swap` builds it
    /// in a shadow directory and installs it by rename, so the bytes behind
    /// this mapping are never modified in place. A reader either maps the
    /// whole old generation or the whole new one.
    ///
    /// Every offset trusted below is validated against the mapped length
    /// first: this is mmap'd memory, so an out-of-bounds slice is a segfault,
    /// not a panic. A truncated or corrupt file must produce `Err`.
    pub fn open(dir: &Path) -> Result<Reader> {
        let p = dir.join(FTS_FILE);
        let f = std::fs::File::open(&p).with_context(|| format!("open {}", p.display()))?;
        let map = unsafe { Mmap::map(&f) }.with_context(|| format!("mmap {}", p.display()))?;

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
        let num_terms = u32::from_le_bytes(map[12..16].try_into().unwrap());

        let table_bytes = (num_terms as usize)
            .checked_mul(TERM_ENTRY)
            .context("term table size overflows usize")?;
        let table_end = HEADER
            .checked_add(table_bytes)
            .context("term table extends past addressable size")?;
        anyhow::ensure!(
            table_end <= map.len(),
            "{} is truncated: header claims {num_terms} terms ({table_bytes} B) \
             but the file has only {} B after the header",
            p.display(),
            map.len().saturating_sub(HEADER)
        );

        let reader = Reader {
            map,
            num_rows,
            num_terms,
        };

        // Validate every term entry up front: each string and posting run
        // must lie inside the file, and neither may start before the end of
        // the term table. This is the only place a bad offset from disk is
        // caught before it is dereferenced.
        for i in 0..num_terms as usize {
            let (str_off, str_len, post_off, df) = reader.entry(i);
            let str_end = str_off
                .checked_add(str_len)
                .context("term string overflows")?;
            anyhow::ensure!(
                str_off >= table_end && str_end <= reader.map.len(),
                "{} is truncated: term {i}'s string [{str_off},{str_end}) is out of bounds",
                p.display()
            );
            let post_bytes = df.checked_mul(8).context("posting run overflows")?;
            let post_end = post_off
                .checked_add(post_bytes)
                .context("posting run extends past addressable size")?;
            anyhow::ensure!(
                post_off >= table_end && post_end <= reader.map.len(),
                "{} is truncated: term {i}'s postings [{post_off},{post_end}) are out of bounds",
                p.display()
            );
        }

        Ok(reader)
    }

    fn entry(&self, i: usize) -> (usize, usize, usize, usize) {
        let o = HEADER + i * TERM_ENTRY;
        let m = &self.map;
        let str_off = u32::from_le_bytes(m[o..o + 4].try_into().unwrap()) as usize;
        let str_len = u32::from_le_bytes(m[o + 4..o + 8].try_into().unwrap()) as usize;
        let post_off = u64::from_le_bytes(m[o + 8..o + 16].try_into().unwrap()) as usize;
        let df = u32::from_le_bytes(m[o + 16..o + 20].try_into().unwrap()) as usize;
        (str_off, str_len, post_off, df)
    }

    fn term_at(&self, i: usize) -> &[u8] {
        let (o, l, _, _) = self.entry(i);
        &self.map[o..o + l]
    }

    /// Postings for `term`, raw bytes already sorted by impact descending —
    /// `df` pairs of `(u32 row, f32 impact)`, read via [`posting`].
    fn lookup(&self, term: &str) -> Option<&[u8]> {
        let key = term.as_bytes();
        let (mut lo, mut hi) = (0usize, self.num_terms as usize);
        while lo < hi {
            let mid = (lo + hi) / 2;
            match self.term_at(mid).cmp(key) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    let (_, _, off, df) = self.entry(mid);
                    return Some(&self.map[off..off + df * 8]);
                }
            }
        }
        None
    }

    /// BM25 search: analyze `query` the same way rows were analyzed at build
    /// time, sum each matching term's stored per-row impact by addition, and
    /// return the top `k` rows best-first. The returned `f32` is a BM25
    /// score — ordinal and unbounded, never a `[0,1]` relevance.
    ///
    /// Every posting in a matching term's run is walked and accumulated —
    /// there is no early-exit cap on how far into a run this goes. That is
    /// deliberate, not an oversight: full accumulation over 31k rows costs
    /// 0.495 ms (see the module doc), so there is nothing to buy by cutting
    /// it short there, and runs are kept impact-descending because that
    /// order is the only lever available to keep a 10M-row query flat if a
    /// cap ever becomes necessary. What cap, if any, would hold quality at
    /// 10M rows is NOT measured — the spike is explicit
    /// that its cap table is a 31k-row measurement that must not be assumed
    /// to hold at that scale.
    /// The row count this postings file was BUILT over. `Pack::open` compares
    /// it against the manifest: a `pack.fts` from a different generation whose
    /// row ids happen to fall in range would hydrate confidently wrong
    /// documents through `Pack::bm25`, with no error anywhere — the same
    /// silent-degradation shape the row-count check on records guards against.
    pub fn rows(&self) -> usize {
        self.num_rows as usize
    }

    pub fn search(&self, query: &str, k: usize) -> Vec<(usize, f32)> {
        if k == 0 || self.num_rows == 0 {
            return Vec::new();
        }
        let mut acc: HashMap<u32, f32> = HashMap::new();
        for term in analyze(query) {
            if let Some(run) = self.lookup(&term) {
                for i in 0..run.len() / 8 {
                    let (row, impact) = posting(run, i);
                    *acc.entry(row).or_default() += impact;
                }
            }
        }
        let mut hits: Vec<(usize, f32)> = acc.into_iter().map(|(r, s)| (r as usize, s)).collect();
        hits.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        hits.truncate(k);
        hits
    }
}

#[inline]
fn posting(run: &[u8], i: usize) -> (u32, f32) {
    let o = i * 8;
    (
        u32::from_le_bytes(run[o..o + 4].try_into().unwrap()),
        f32::from_le_bytes(run[o + 4..o + 8].try_into().unwrap()),
    )
}
