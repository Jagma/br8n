use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Mutex;
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

pub const VEC_FILE: &str = "pack.vec";

/// Build options. `connectivity`/`expansion_add` are usearch's HNSW build
/// parameters; `expansion_search` is its query-time effort — the same knob lbug
/// calls `efs`, whose cost dominates a vector query (see CLAUDE.md).
///
/// `expansion_search: 64` here is only the value a freshly built or opened
/// index starts with, not what a query actually runs at: `Reader::search`
/// calls `change_expansion_search` with the caller's per-tier `efs` before
/// every search, passing it explicitly rather than trusting a fixed default.
fn options(dims: usize) -> IndexOptions {
    IndexOptions {
        dimensions: dims,
        metric: MetricKind::Cos,
        // f16, not i8: measured on the real corpus, f16 is faster AND more
        // accurate here because Apple Silicon has native fp16 SIMD while the
        // i8 path pays dequantization, as a standalone spike measured.
        quantization: ScalarKind::F16,
        connectivity: 32,
        expansion_add: 200,
        expansion_search: 256,
        multi: false,
    }
}

/// Build the index over `rows`, where a row's POSITION is its identity — the
/// same ordinal `records::Reader::get` takes. usearch stores the vectors inside
/// its own file (149 B/row of graph on top), so no separate vector array is
/// needed or wanted.
pub fn build(dir: &Path, dims: usize, rows: &[Vec<f32>]) -> Result<()> {
    let idx = Index::new(&options(dims)).context("create usearch index")?;
    idx.reserve(rows.len().max(1))
        .context("reserve usearch index")?;
    for (i, v) in rows.iter().enumerate() {
        anyhow::ensure!(
            v.len() == dims,
            "row {i} has {} dimensions, expected {dims}",
            v.len()
        );
        idx.add(i as u64, v)
            .with_context(|| format!("add row {i}"))?;
    }
    let p = dir.join(VEC_FILE);
    idx.save(p.to_str().context("pack path is not utf-8")?)
        .with_context(|| format!("write {}", p.display()))
}

pub struct Reader {
    idx: Index,
    dims: usize,
    search_lock: Mutex<()>,
}

impl Reader {
    /// `view` MMAPS the index rather than loading it: measured +0.0 MB resident
    /// for a 13.2 MB file, with queries identical to a fully in-memory index.
    /// That property is what lets the hook open this per prompt.
    pub fn open(dir: &Path, dims: usize) -> Result<Reader> {
        let idx = Index::new(&options(dims)).context("create usearch index")?;
        let p = dir.join(VEC_FILE);
        // NEVER hand `Index::view` a path it cannot open and map. This guard
        // is not defensive tidiness; without it usearch closes THIS PROCESS'S
        // file descriptor 0.
        //
        // `NativeIndex::view` (usearch 2.26.1, `rust/lib.cpp:252`) builds a
        // `memory_mapped_file_t` from the path and calls `open_if_not()`.
        // Three failure paths inside it — `open()` fails (the file is missing
        // or unreadable), `fstat()` fails, or `mmap()` fails (which it does
        // with EINVAL on a ZERO-length file) — return an error WITHOUT ever
        // assigning `file_descriptor_`, which is value-initialized to `0`,
        // and WITHOUT clearing `path_`. The destructor then runs `close()`,
        // whose only guard is `if (!path_)`, so it executes
        // `::close(file_descriptor_)` — a literal `::close(0)`
        // (`include/usearch/index.hpp:2114-2128`).
        //
        // What follows is worse than losing stdin. With fd 0 free, the next
        // `open` anywhere in the process is handed 0 and Rust's `OwnedFd`
        // takes ownership of it; a later failed `view` closes fd 0 again, out
        // from under that owner.
        //
        // Measured, not inferred, by running the built `tests/pack` binary
        // under lldb with a conditional breakpoint on `close` where the fd
        // argument is 0. The FIRST hit is
        // `~memory_mapped_file_t <- NativeIndex::view <- vectors::Reader::open`
        // on a missing `pack.vec`; of the 127 `close(0)` calls in one
        // `--test-threads=1` run, 115 are Rust std closing an fd 0 it was
        // handed after that. At `--test-threads=8` the same binary died with
        // `fatal runtime error: IO Safety violation: owned file descriptor
        // already closed` — the SIGABRT a third observer saw — and the race is
        // what produces `unexpected error during closedir: Bad file
        // descriptor` when the descriptor usearch steals happens to be the
        // one a `TempDir` teardown is walking. It reproduces only under
        // parallelism because that is what puts a live descriptor in the slot
        // usearch closes.
        //
        // This is the only `Index::view` call site in the repository, so
        // refusing here closes the hole completely. Opening the file
        // ourselves rather than asking `Path::exists` is deliberate: it
        // covers an unreadable file as well as an absent one, which is
        // exactly the set `open()` fails on inside usearch. The window
        // between this check and `view` is not a hazard — a published pack is
        // immutable and is installed by rename, never modified in place.
        //
        // `vectors::build`'s `idx.save(path)` needs no such guard: it goes
        // through `output_file_t`, whose `close()` is guarded on the `FILE*`
        // itself and clears it (`index.hpp:1934`).
        let probe = std::fs::File::open(&p).with_context(|| format!("open {}", p.display()))?;
        let len = probe
            .metadata()
            .with_context(|| format!("stat {}", p.display()))?
            .len();
        drop(probe);
        anyhow::ensure!(
            len > 0,
            "{} is empty — a zero-length vector index cannot be mapped; \
             run `br8n index --reindex`",
            p.display()
        );
        idx.view(p.to_str().context("pack path is not utf-8")?)
            .with_context(|| format!("mmap {}", p.display()))?;
        Ok(Reader {
            idx,
            dims,
            search_lock: Mutex::new(()),
        })
    }

    /// Returns `(row, similarity)` with similarity a [0,1] value — the value
    /// that becomes `Hit.relevance`. usearch's `Cos` metric returns cosine
    /// DISTANCE `d = 1 - cos_sim`, range `[0,2]`.
    ///
    /// This deliberately does NOT return the raw cosine (`1.0 - dist`), even
    /// though that would be the mathematically natural conversion. It uses
    /// the same `1.0 - dist / 2.0` formula lbug's own vector search used
    /// before this pack existed (`src/store/query.rs`'s retired
    /// `vector_search`), which maps cosine `[-1,1]` onto `[0,1]` so an
    /// orthogonal pair scores 0.5 rather than 0.0. Every downstream number is
    /// calibrated against THAT scale: the hook's 0.70 injection gate, the
    /// measured irrelevant-hit floor (~0.565-0.669), recorded top-hit
    /// relevances (0.82-0.85), and every `br8n bench` recall figure.
    /// Changing this formula re-scales every relevance in the system without
    /// changing a single threshold.
    ///
    /// `efs` is usearch's query-time search effort (`expansion_search`,
    /// `options()`'s build-time default of 64 above is only a starting point
    /// for a freshly built index). It is set here, per call, via
    /// `Index::change_expansion_search` rather than left at the value the
    /// index was built with — see the call site in `Retriever::run` for why a
    /// per-tier value must reach it explicitly on every search.
    pub fn search(&self, q: &[f32], k: usize, efs: usize) -> Result<Vec<(usize, f32)>> {
        if q.is_empty() || k == 0 {
            return Ok(Vec::new());
        }
        anyhow::ensure!(
            q.len() == self.dims,
            "query has {} dimensions, pack has {}",
            q.len(),
            self.dims
        );
        // Never search less broadly than the caller asked to receive: with
        // efs < k the HNSW traversal cannot surface k neighbours, so the
        // query would silently return short.
        let _guard = self.search_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.idx.change_expansion_search(efs.max(k));
        let m = self.idx.search(q, k).context("usearch search")?;
        Ok(m.keys
            .iter()
            .zip(m.distances.iter())
            .map(|(key, dist)| (*key as usize, (1.0 - dist / 2.0).clamp(0.0, 1.0)))
            .collect())
    }

    /// Reconstruct row `N`'s stored vector, dequantized back to `f32`.
    ///
    /// Verified against a throwaway probe (build -> save -> `view()` a SEPARATE
    /// `Index` handle -> `get::<f32>`) before this was written: usearch's `get`
    /// works on a `view()`-opened (mmap'd) index exactly as it does on one that
    /// built the data, with the same f16 dequantization error the spike
    /// measured on the real corpus (~8e-5 max) — here observed as ~7e-5 on a synthetic normalized vector. This is
    /// what lets `Pack::cosine_for` compute a real cosine for a BM25-only hit
    /// without reopening the store.
    pub fn vector(&self, row: usize) -> Result<Vec<f32>> {
        let mut buf = vec![0f32; self.dims];
        let n = self
            .idx
            .get::<f32>(row as u64, &mut buf)
            .with_context(|| format!("reconstruct row {row}"))?;
        anyhow::ensure!(
            n == 1,
            "row {row} has {n} stored vectors, expected exactly 1 (multi: false)"
        );
        Ok(buf)
    }
}
