#[cfg(feature = "ocr")]
use crate::config::OcrMode;
use crate::config::PdfConfig;
use crate::loaders::Loaded;
use crate::model::{Document, SourceType};
use anyhow::Result;
use std::path::Path;
#[cfg(feature = "ocr")]
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("`{path}` appears to be a scanned PDF with no reliable text layer; skipping (OCR not enabled)")]
    Scanned {
        path: String,
        /// Whether recognition was actually run against this file before it
        /// was given up on.
        ///
        /// The caller cannot infer it from `[pdf] ocr` alone: `OCR_DISABLED`
        /// is a PROCESS-WIDE latch, so the second scanned PDF of a run on a
        /// machine with no ONNX Runtime is never attempted at all even though
        /// the user asked for recognition. `src/index.rs` stamps an attempted
        /// failure (the anti-retry rule) and must NOT stamp this one — a
        /// stamp here makes a document that was never even looked at
        /// permanently invisible. See the `Err` arm of `consider_into`.
        ocr_attempted: bool,
    },
    #[error("`{path}` yielded no extractable text")]
    Empty { path: String },
}

/// One page of final Markdown, whatever produced it.
///
/// This exists so the assembly below never has to know whether a page came
/// from the text layer or from recognition — and so it can be tested without
/// PDFium, ONNX Runtime, or a filesystem.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedPage {
    /// 1-indexed, as stored and displayed.
    pub number: u32,
    pub markdown: String,
    /// Whether recognition contributed any of this page's text.
    pub ocr: bool,
}

/// Usable pages plus the 1-indexed pages that were dropped.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Extraction {
    pub pages: Vec<LoadedPage>,
    pub dropped: Vec<u32>,
}

/// OCR is off for the rest of this process.
///
/// This is not tidiness, it is the only thing preventing a crash. `ort`
/// returns a recoverable error the first time it cannot load ONNX Runtime and
/// then PANICS on the next attempt ("`OrtGetApiBase` must be present in ONNX
/// Runtime dylib"), which would abort an entire `br8n index` run because one
/// scanned PDF happened to be in the vault. Measured in a standalone spike.
#[cfg(feature = "ocr")]
static OCR_DISABLED: AtomicBool = AtomicBool::new(false);

/// One recognition at a time, process-wide.
///
/// NOT a throughput decision. `OCR_DISABLED` above is a check-then-act latch —
/// read before the call, written after it fails — so N threads all read
/// `false`, all enter, and all reach `ort::init_from`. What that costs is
/// worse than the documented panic: `ort`'s own `OnceLock` initializes through
/// `call_once_force`, whose `Err` arm returns normally WITHOUT writing the
/// slot, so the `Once` is marked COMPLETED with its data still
/// `MaybeUninit::uninit()`. Every later `init_from` then returns `Ok` over
/// uninitialized memory, and the `OrtGetApiBase` panic CLAUDE.md records is a
/// symptom of that rather than a guarantee.
///
/// The cost of serializing is close to nothing. PDFium puts every call behind
/// one process-wide mutex of its own, `oar-ocr` runs its own worker pool
/// inside a single `recognize`, and a single-page document takes that
/// function's sequential branch and uses one worker however many exist.
#[cfg(feature = "ocr")]
static OCR_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Announce the first recognition once per process, not once per document.
#[cfg(feature = "ocr")]
static ANNOUNCED: std::sync::Once = std::sync::Once::new();

/// Warn about a costly DPI once per process, not once per document.
#[cfg(feature = "ocr")]
static DPI_WARNED: std::sync::Once = std::sync::Once::new();

/// Where a native library usually lives when a package manager put it there.
///
/// Probing is the NORMAL path on macOS, not a fallback: the crate's discovery
/// chain checks the executable's own directory, a `target/pdfium/...` path,
/// and a bare `dlopen`, and `/opt/homebrew/lib` is on none of those — a bare
/// `dlopen` does not search it either. Without this, OCR would look enabled
/// and never once run.
fn probe(explicit: Option<&Path>, file: &str) -> Option<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(p) = explicit {
        candidates.push(p.to_path_buf());
    }
    for prefix in ["/opt/homebrew/lib", "/usr/local/lib", "/usr/lib"] {
        candidates.push(Path::new(prefix).join(file));
    }
    candidates.into_iter().find(|p| p.exists())
}

/// The two libraries OCR needs, as (env var, file name, configured override).
fn libraries(cfg: &PdfConfig) -> [(&'static str, &'static str, Option<&Path>); 2] {
    [
        (
            "PDFIUM_LIB_PATH",
            "libpdfium.dylib",
            cfg.pdfium_lib_path.as_deref(),
        ),
        (
            "ORT_DYLIB_PATH",
            "libonnxruntime.dylib",
            cfg.ort_dylib_path.as_deref(),
        ),
    ]
}

/// Where a library will be loaded from, if anywhere. An environment variable
/// always wins, exactly as `resolve_ocr_libraries` honours it.
fn locate(var: &str, explicit: Option<&Path>, file: &str) -> Option<std::path::PathBuf> {
    if let Some(set) = std::env::var_os(var) {
        // Existence is checked even here. Reporting a variable's value back as
        // "found" would turn a typo'd path into a confident lie, which is the
        // failure this whole line of output exists to prevent.
        let set = std::path::PathBuf::from(set);
        return set.exists().then_some(set);
    }
    probe(explicit, file)
}

/// Point the OCR loaders at their libraries. CALL THIS ONCE, FROM `main`,
/// BEFORE ANY THREAD IS SPAWNED.
///
/// This used to live in the loader, behind a `Once`, which put
/// `std::env::set_var` in a running multi-threaded program. POSIX `setenv` is
/// not thread-safe against a concurrent `getenv`, and the MCP server already
/// races it: `br8n_index` runs on one `spawn_blocking` thread of a
/// `new_multi_thread` runtime (src/mcp.rs:156, :181) while `br8n_search` on
/// another reads `BR8N_DB` and builds a `reqwest` client that reads the proxy
/// variables (src/mcp.rs:90). Parallel document loading widens that to every
/// worker. The `Once` did not help: it serializes the WRITERS against each
/// other and against nothing else in the process.
///
/// Both paths stay environment variables because neither library offers
/// anything else at this distance. `process_pdf_with_ocr` builds its own
/// renderer with `PdfiumRenderer::load()` and never exposes it, so
/// `PDFIUM_LIB_PATH` is the only steering br8n has; `ort` has no path-taking
/// API at all.
///
/// A variable the user already set always wins, exactly as before.
///
/// Library consumers that bypass `main` must call this themselves, while
/// single-threaded, or OCR sees only what the environment already carries.
pub fn resolve_ocr_libraries(cfg: &PdfConfig) {
    for (var, file, explicit) in libraries(cfg) {
        if std::env::var_os(var).is_some() {
            continue;
        }
        if let Some(found) = probe(explicit, file) {
            std::env::set_var(var, found);
        }
    }
}

/// Whether each OCR library can be found, for `br8n status`.
///
/// Reports location only. A library that is present can still fail to load —
/// a version mismatch, say — so this must never be phrased as "OCR works".
/// Finding nothing, on the other hand, is conclusive.
pub fn ocr_library_paths(cfg: &PdfConfig) -> Vec<(&'static str, Option<std::path::PathBuf>)> {
    libraries(cfg)
        .into_iter()
        .map(|(var, file, explicit)| {
            let name = if file.contains("pdfium") {
                "pdfium"
            } else {
                "onnxruntime"
            };
            (name, locate(var, explicit, file))
        })
        .collect()
}

/// Turn OCR off for the rest of the run and say why, exactly once.
#[cfg(feature = "ocr")]
fn disable_ocr(path: &Path, err: &dyn std::fmt::Display) {
    OCR_DISABLED.store(true, Ordering::Relaxed);
    eprintln!(
        "br8n: {}: OCR unavailable, falling back to text-layer extraction for the \
         rest of this run: {err}\n\
         br8n: install PDFium and ONNX Runtime, or set PDFIUM_LIB_PATH and \
         ORT_DYLIB_PATH (or `[pdf] pdfium_lib_path` / `ort_dylib_path` in config)",
        path.display()
    );
}

/// The DPI at which an A4 page renders past pdf-inspector's escalation
/// threshold.
///
/// `detect_boxes` skips its 960px standard detection pass when a page's
/// longest side exceeds 1920px and runs the 2560px escalated detector instead.
/// A4 is 842pt tall, so it crosses at 1920 / 842 * 72 = 164. US Letter is
/// 792pt and crosses at 174.
#[cfg(feature = "ocr")]
const ESCALATION_DPI_A4: f32 = 164.0;

/// The one-line warning for a DPI past the cliff, or `None`.
///
/// Judged from CONFIGURATION ALONE, and the text says so. Neither
/// `extract_pages_markdown` nor `OcrPdfResult` reports a page's dimensions, so
/// nothing here can know the real page sizes without a separate inspection
/// pass. It therefore uses the EARLIER of the two thresholds, which makes it
/// conservative: a corpus of only US Letter pages between 164 and 174 gets a
/// warning it does not strictly need. That false positive is the deliberate
/// trade — paying
/// several times the per-page cost silently is the house failure mode.
#[cfg(feature = "ocr")]
fn dpi_warning(dpi: f32) -> Option<String> {
    (dpi > ESCALATION_DPI_A4).then(|| {
        format!(
            "br8n: [pdf] dpi = {dpi} renders an A4 page past 1920px, where OCR \
             switches from the 960px detector to the 2560px one and costs \
             substantially more per page. Judged from the configured DPI alone, \
             because page sizes are not visible here — a US Letter page does not \
             cross until 174."
        )
    })
}

/// Build the crate's options from `[pdf]`.
///
/// Separate and pure so the wiring is testable without PDFium, ONNX Runtime,
/// or a scanned fixture on disk.
#[cfg(feature = "ocr")]
fn ocr_pdf_options(
    cfg: &PdfConfig,
    mode: pdf_inspector::vision::OcrMode,
) -> pdf_inspector::vision::OcrPdfOptions {
    let mut ocr = pdf_inspector::vision::OcrOptions::new()
        .mode(mode)
        .minimum_confidence(cfg.ocr_min_confidence);
    if let Some(dir) = &cfg.model_dir {
        ocr = ocr.model_directory(dir.clone());
    }
    ocr = ocr.model_downloads(match cfg.model_downloads {
        crate::config::ModelDownloads::IfMissing => {
            pdf_inspector::vision::ModelDownloadPolicy::IfMissing
        }
        crate::config::ModelDownloads::Offline => {
            pdf_inspector::vision::ModelDownloadPolicy::Offline
        }
    });
    pdf_inspector::vision::OcrPdfOptions::new()
        .render(pdf_inspector::vision::RenderOptions::new().dpi(cfg.dpi))
        .ocr(ocr)
}

pub struct PdfLoader;

impl PdfLoader {
    /// Load with default settings, which means OCR in `auto` mode.
    pub fn load_file(path: &Path) -> Result<Document> {
        Self::load_file_with(path, &PdfConfig::default())
    }

    pub fn load_file_with(path: &Path, cfg: &PdfConfig) -> Result<Document> {
        Self::load_file_reporting(path, cfg).map(|loaded| loaded.doc)
    }

    /// `load_file_with`, keeping the record of what it could not read.
    ///
    /// `load_file_with` reports dropped pages on stderr and returns only the
    /// Document, which is enough for a single-pass index and not enough for a
    /// two-pass one. This is the same work, with `Extraction::dropped` carried
    /// out to the caller instead of discarded.
    pub fn load_file_reporting(path: &Path, cfg: &PdfConfig) -> Result<Loaded> {
        let (recognized, ocr_attempted) = Self::extract_with_ocr(path, cfg);
        let extracted = match recognized {
            Some(e) => e,
            None => Self::extract_native(path)?,
        };

        if extracted.pages.is_empty() {
            // Indexing an OCR-less scan embeds noise that is very hard to debug
            // later, so bail rather than store it.
            //
            // `ocr_attempted` rides out with the error because this is the one
            // failure the two-pass indexer treats specially, and it must not
            // treat "recognition tried and failed" the same as "recognition
            // never ran" — see `PdfError::Scanned`.
            anyhow::bail!(PdfError::Scanned {
                path: path.display().to_string(),
                ocr_attempted,
            });
        }

        // A mixed document must not silently drop content: the good pages
        // still index fine, so this isn't an error, but the user needs to know
        // pages are missing.
        if !extracted.dropped.is_empty() {
            let pages = extracted
                .dropped
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            eprintln!(
                "br8n: {}: skipping scanned/empty page(s) {pages} (no reliable text layer)",
                path.display()
            );
        }

        let (text, pages) = assemble(&extracted.pages);

        // Unreachable while `pages` is non-empty and every page carries
        // non-blank Markdown, which both extractors guarantee. Kept as a guard
        // so a future change to either filter cannot silently index a blank
        // document.
        if text.trim().is_empty() {
            anyhow::bail!(PdfError::Empty {
                path: path.display().to_string()
            });
        }

        // The crate exposes no document title; the filename is the honest source.
        let title = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("untitled")
            .to_string();

        let uri = format!("file://{}", path.canonicalize()?.display());
        let mut doc = Document::new(SourceType::Pdf, &uri, &title, &text);
        doc.meta = serde_json::json!({ "pages": pages });
        Ok(Loaded {
            doc,
            pending_ocr: extracted.dropped,
            ocr_attempted,
        })
    }

    /// The text layer alone. Exactly the behaviour that shipped before OCR.
    fn extract_native(path: &Path) -> Result<Extraction> {
        // `None` = every page, in document order.
        let extracted = pdf_inspector::extract_pages_markdown(path, None)?;
        let mut out = Extraction::default();
        for page in &extracted.pages {
            // `PageMarkdown.page` is 0-indexed; everything downstream is 1-indexed.
            let number = page.page + 1;
            if page.needs_ocr || page.markdown.trim().is_empty() {
                out.dropped.push(number);
            } else {
                out.pages.push(LoadedPage {
                    number,
                    markdown: page.markdown.clone(),
                    ocr: false,
                });
            }
        }
        Ok(out)
    }

    /// Recognition, or `None` when OCR is off, latched off, or just failed —
    /// paired with whether the engine was actually CALLED.
    ///
    /// Returning `None` rather than an error is deliberate: every one of those
    /// cases must fall through to the text layer, which is what a machine
    /// without the native libraries has always done.
    ///
    /// The second half of the pair is the distinction that `None` alone
    /// destroys. `(None, false)` is "nothing looked at this file" — the mode
    /// is `Off`, or the process-wide latch already tripped on an earlier
    /// document. `(None, true)` is "recognition ran and failed". They lead to
    /// opposite decisions in the indexer, and conflating them deleted
    /// documents: see `PdfError::Scanned::ocr_attempted`.
    #[cfg(feature = "ocr")]
    fn extract_with_ocr(path: &Path, cfg: &PdfConfig) -> (Option<Extraction>, bool) {
        // Held across the latch read AND the call, so no thread can be inside
        // recognition when another flips the latch. A poisoned gate is
        // recovered rather than propagated: a panic in one document must not
        // disable OCR by making every later lock fail.
        let _gate = OCR_GATE.lock().unwrap_or_else(|e| e.into_inner());
        let mode = match ocr_mode_for(cfg, OCR_DISABLED.load(Ordering::Relaxed)) {
            Some(m) => m,
            // Not attempted: the user turned OCR off, or an earlier document
            // in this process already found the libraries unusable.
            None => return (None, false),
        };
        let options = ocr_pdf_options(cfg, mode);

        match pdf_inspector::vision::process_pdf_with_ocr(path, options) {
            Ok(result) => {
                if !result.pages_routed_to_ocr.is_empty() {
                    // The first recognition on a cold cache spends ~20 seconds
                    // fetching a model with nothing on stderr. Silence there is
                    // indistinguishable from a hang.
                    ANNOUNCED.call_once(|| {
                        eprintln!(
                            "br8n: recognizing scanned page(s); the first run may \
                             download an OCR model"
                        );
                    });

                    // Warned here, gated on `pages_routed_to_ocr` being
                    // non-empty, rather than at config load or before this
                    // call: `cfg.ocr` defaults to `Auto`, so this function
                    // runs for every PDF, and OCR being enabled says nothing
                    // about whether a page was actually rasterized. Only
                    // this condition means recognition genuinely happened,
                    // which is the only time the DPI cliff can be reached.
                    if let Some(warning) = dpi_warning(cfg.dpi) {
                        DPI_WARNED.call_once(|| eprintln!("{warning}"));
                    }
                }
                (Some(fused_to_extraction(&result)), true)
            }
            Err(e) => {
                disable_ocr(path, &e);
                // Attempted, and failed. THIS document really was looked at,
                // so the anti-retry stamp is honest for it; the ones that
                // follow it in this process are the ones that are not.
                (None, true)
            }
        }
    }

    #[cfg(not(feature = "ocr"))]
    fn extract_with_ocr(_path: &Path, _cfg: &PdfConfig) -> (Option<Extraction>, bool) {
        (None, false)
    }
}

/// Whether to recognize at all, and in which mode.
///
/// Pure and separately testable ON PURPOSE. When this lived inline, a test of
/// the latch could not tell "OCR was skipped because it is latched off" from
/// "OCR ran and failed" — and it duly passed with the latch deleted. The
/// distinction matters because a second attempt after a failed ONNX Runtime
/// load does not return an error, it panics and takes the index run with it.
#[cfg(feature = "ocr")]
fn ocr_mode_for(cfg: &PdfConfig, latched_off: bool) -> Option<pdf_inspector::vision::OcrMode> {
    if latched_off {
        return None;
    }
    match cfg.ocr {
        OcrMode::Off => None,
        OcrMode::Auto => Some(pdf_inspector::vision::OcrMode::Auto),
        OcrMode::Force => Some(pdf_inspector::vision::OcrMode::Force),
    }
}

/// Map a completed OCR run onto the loader's own page type.
#[cfg(feature = "ocr")]
fn fused_to_extraction(result: &pdf_inspector::vision::OcrPdfResult) -> Extraction {
    use pdf_inspector::vision::PageContentSource;
    let mut out = Extraction::default();
    for page in &result.pages {
        // `page_number` is ALREADY 1-indexed. Adding one here — as the
        // native path must, because its input is 0-indexed — would shift
        // every stored page citation by one, silently, since nothing
        // downstream validates a page number against the document.
        let number = page.page_number;
        if page.markdown.trim().is_empty() {
            out.dropped.push(number);
            continue;
        }
        out.pages.push(LoadedPage {
            number,
            markdown: page.markdown.clone(),
            ocr: !matches!(page.provenance.source, PageContentSource::Native),
        });
    }
    out
}

/// Concatenate pages and record where each one starts.
///
/// Pure: no filesystem, no native libraries. The page-number and offset
/// bookkeeping that citations depend on is tested through here.
fn assemble(pages: &[LoadedPage]) -> (String, Vec<serde_json::Value>) {
    let mut text = String::new();
    let mut meta = Vec::new();
    for page in pages {
        meta.push(serde_json::json!({
            "page": page.number,
            "offset": text.len(),
            "ocr": page.ocr,
        }));
        text.push_str(&page.markdown);
        text.push_str("\n\n");
    }
    (text, meta)
}

impl super::Loader for PdfLoader {
    fn load(&self, uri: &str) -> Result<Vec<Document>> {
        let path = uri.strip_prefix("file://").unwrap_or(uri);
        Ok(vec![Self::load_file(Path::new(path))?])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "ocr")]
    use pdf_inspector::vision::OcrMode as PdfiumOcrMode;

    fn page(number: u32, markdown: &str, ocr: bool) -> LoadedPage {
        LoadedPage {
            number,
            markdown: markdown.into(),
            ocr,
        }
    }

    #[test]
    fn assemble_records_page_numbers_and_running_offsets() {
        let (text, meta) = assemble(&[page(1, "alpha", false), page(2, "beta", true)]);

        assert_eq!(text, "alpha\n\nbeta\n\n");
        assert_eq!(meta[0]["page"], 1);
        assert_eq!(meta[0]["offset"], 0);
        assert_eq!(meta[0]["ocr"], false);
        // "alpha" + "\n\n" = 7 bytes before the second page begins.
        assert_eq!(meta[1]["page"], 2);
        assert_eq!(meta[1]["offset"], 7);
        assert_eq!(meta[1]["ocr"], true);
    }

    #[test]
    fn assemble_preserves_true_page_numbers_across_a_gap() {
        // Page 2 was dropped. Pages 1 and 3 must keep their real numbers, or
        // every citation after the gap points at the wrong page.
        let (_, meta) = assemble(&[page(1, "first", false), page(3, "third", false)]);

        assert_eq!(meta[0]["page"], 1);
        assert_eq!(meta[1]["page"], 3);
    }

    #[test]
    #[cfg(feature = "ocr")]
    fn a_latched_failure_stops_every_later_ocr_attempt() {
        // The one that matters. `ort` errors on its first failed load of ONNX
        // Runtime and then PANICS on the next, so "never attempt again" is a
        // crash-safety property, not an optimization.
        let auto = PdfConfig::default();
        assert_eq!(auto.ocr, OcrMode::Auto, "default must be the OCR-on mode");

        assert_eq!(ocr_mode_for(&auto, false), Some(PdfiumOcrMode::Auto));
        assert_eq!(
            ocr_mode_for(&auto, true),
            None,
            "latched off must win over an enabled config"
        );
    }

    #[test]
    #[cfg(feature = "ocr")]
    fn disable_ocr_sets_the_latch() {
        // Pairs with the test above: that one proves a latched run skips OCR,
        // this one proves a failure is what latches it.
        OCR_DISABLED.store(false, Ordering::Relaxed);
        disable_ocr(Path::new("x.pdf"), &"simulated load failure");
        assert!(OCR_DISABLED.load(Ordering::Relaxed));
        OCR_DISABLED.store(false, Ordering::Relaxed);
    }

    #[test]
    #[cfg(feature = "ocr")]
    fn ocr_off_is_honoured_even_when_nothing_has_failed() {
        let off = PdfConfig {
            ocr: OcrMode::Off,
            ..PdfConfig::default()
        };
        assert_eq!(ocr_mode_for(&off, false), None);
    }

    #[test]
    #[cfg(feature = "ocr")]
    fn force_mode_reaches_the_engine_as_force() {
        let force = PdfConfig {
            ocr: OcrMode::Force,
            ..PdfConfig::default()
        };
        assert_eq!(ocr_mode_for(&force, false), Some(PdfiumOcrMode::Force));
    }

    #[test]
    fn a_library_path_that_does_not_exist_is_reported_as_missing() {
        // Regression: `locate` used to hand back whatever the environment
        // variable said without checking it, so a typo'd path made
        // `br8n status` report the library as found. Status output that
        // confidently lies is worse than no status output.
        let key = "PDFIUM_LIB_PATH";
        let prior = std::env::var_os(key);
        std::env::set_var(key, "/definitely/not/here/libpdfium.dylib");

        let reported = ocr_library_paths(&PdfConfig::default());
        let pdfium = reported
            .iter()
            .find(|(name, _)| *name == "pdfium")
            .expect("pdfium must be reported on");
        let seen = pdfium.1.clone();

        match prior {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        assert_eq!(seen, None, "a non-existent path must not count as found");
    }

    #[test]
    fn probe_prefers_an_explicit_path_over_the_usual_prefixes() {
        let dir = tempfile::tempdir().unwrap();
        let explicit = dir.path().join("libpdfium.dylib");
        std::fs::write(&explicit, b"x").unwrap();

        assert_eq!(
            probe(Some(&explicit), "libpdfium.dylib"),
            Some(explicit.clone())
        );
        // A name that exists in no prefix must not be invented.
        assert_eq!(probe(None, "libnot-a-real-library.dylib"), None);
    }

    #[test]
    #[cfg(feature = "ocr")]
    fn configured_dpi_reaches_the_render_options() {
        // The whole point of the key: `OcrPdfOptions::new().ocr(..)` leaves
        // `render` at the crate default and nothing in `[pdf]` could change it.
        let cfg = PdfConfig {
            dpi: 120.0,
            ..PdfConfig::default()
        };
        let options = ocr_pdf_options(&cfg, pdf_inspector::vision::OcrMode::Auto);
        assert_eq!(options.render.dpi, 120.0);
        assert_eq!(
            ocr_pdf_options(&PdfConfig::default(), pdf_inspector::vision::OcrMode::Auto)
                .render
                .dpi,
            150.0,
            "the shipped default must stay at the crate's 150"
        );
    }

    #[test]
    #[cfg(feature = "ocr")]
    fn the_dpi_warning_fires_only_above_the_a4_cliff() {
        // `detect_boxes` abandons the 960px standard pass above 1920px and runs
        // the 2560px escalated detector instead. A4 is 842pt tall and crosses at
        // 1920/842*72 = 164 DPI; US Letter is 792pt and crosses at 174.
        assert!(dpi_warning(150.0).is_none(), "the default must be silent");
        assert!(
            dpi_warning(164.0).is_none(),
            "the boundary itself is not past it"
        );
        assert!(
            dpi_warning(165.0).is_some(),
            "just past A4's cliff must warn"
        );
        let warning = dpi_warning(200.0).expect("200 DPI must warn");
        assert!(
            warning.contains("174"),
            "the warning must disclose that Letter crosses later, since the \
             check cannot see real page sizes: {warning}"
        );

        // RECOMPUTED, because both numbers above are hand-typed — 164 in the
        // constant and 174 in the format string — so every assertion so far
        // catches a documentation regression and none of them catches an
        // arithmetic one. A page of `height_pt` renders to
        // `height_pt / 72 * dpi` pixels, so it crosses the escalation cliff at
        // `ESCALATION_PX / (height_pt / 72)` DPI. Floored, because the DPI
        // BELOW the crossing is the last one that is still safe.
        const ESCALATION_PX: f32 = 1920.0;
        let crossing = |height_pt: f32| (ESCALATION_PX / (height_pt / 72.0)).floor();
        assert_eq!(
            crossing(842.0),
            ESCALATION_DPI_A4,
            "the A4 constant must be the arithmetic, not a number that once was"
        );
        assert_eq!(
            crossing(792.0),
            174.0,
            "and the US Letter figure the message quotes must be too"
        );
    }

    #[test]
    #[cfg(feature = "ocr")]
    fn offline_reaches_the_engine_as_offline() {
        use pdf_inspector::vision::ModelDownloadPolicy;
        assert_eq!(
            ocr_pdf_options(&PdfConfig::default(), pdf_inspector::vision::OcrMode::Auto)
                .ocr
                .model_downloads,
            ModelDownloadPolicy::IfMissing,
            "the default must keep today's behaviour"
        );
        let offline = PdfConfig {
            model_downloads: crate::config::ModelDownloads::Offline,
            ..PdfConfig::default()
        };
        assert_eq!(
            ocr_pdf_options(&offline, pdf_inspector::vision::OcrMode::Auto)
                .ocr
                .model_downloads,
            ModelDownloadPolicy::Offline,
        );
    }

    #[test]
    #[cfg(feature = "ocr")]
    fn the_ocr_path_actually_takes_the_gate() {
        // This is the test that pins the WIRING, not just the primitive.
        // `extract_with_ocr` takes `OCR_GATE` at its very top, before the
        // `ocr_mode_for(...)?` early return — so a call into it must block for
        // as long as this thread holds the gate, regardless of whether OCR
        // ends up running, is latched off, or fails. `paper.pdf` is an
        // ordinary text PDF with no scanned pages, so this needs neither
        // PDFium nor ONNX Runtime and stays cheap; `OcrMode::Auto` just means
        // the production code path is actually entered instead of
        // short-circuited by config.
        //
        // Mutation-tested: deleting the production
        // `let _gate = OCR_GATE.lock().unwrap_or_else(|e| e.into_inner());`
        // line makes this test fail (the spawned thread finishes immediately,
        // even while this thread holds the gate).
        use std::sync::atomic::{AtomicBool, Ordering as O};
        use std::sync::mpsc;
        use std::sync::Arc;
        use std::time::Duration;

        let held = OCR_GATE.lock().unwrap_or_else(|e| e.into_inner());

        let done = Arc::new(AtomicBool::new(false));
        let done_writer = Arc::clone(&done);
        let (finished_tx, finished_rx) = mpsc::channel::<()>();

        let handle = std::thread::spawn(move || {
            let cfg = PdfConfig {
                ocr: OcrMode::Auto,
                ..PdfConfig::default()
            };
            let _ = PdfLoader::load_file_with(Path::new("tests/fixtures/corpus/paper.pdf"), &cfg);
            done_writer.store(true, O::SeqCst);
            let _ = finished_tx.send(());
        });

        // Bounded wait, never an unbounded one: if the gate is not being
        // taken, the spawned thread finishes almost instantly; if it is, it
        // cannot even start extraction until we release the lock below.
        let finished_while_gate_held = finished_rx.recv_timeout(Duration::from_millis(500)).is_ok();

        assert!(
            !finished_while_gate_held && !done.load(O::SeqCst),
            "PdfLoader::load_file_with finished while this thread held \
             OCR_GATE; extract_with_ocr must take the gate before doing \
             anything else, and the production line that does so is missing"
        );

        // Always release before joining, or a genuinely-blocked thread would
        // hang this test forever.
        drop(held);

        handle.join().expect("spawned loader thread must not panic");
        assert!(
            done.load(O::SeqCst),
            "load must complete once the gate is released"
        );
    }
}
