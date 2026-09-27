pub mod codex;
pub mod markdown;
pub mod pdf;
pub mod transcript;
pub mod web;

use crate::model::Document;
use anyhow::Result;

/// Every loader normalizes its source to Markdown so there is one chunker.
pub trait Loader {
    fn load(&self, uri: &str) -> Result<Vec<Document>>;
}

/// A loaded document, plus what this pass could not read.
///
/// Lives HERE rather than in `loaders::pdf`, where it started: only `PdfLoader`
/// ever produces a non-trivial one, but `consider_into` wraps every markdown
/// note and every transcript in it too, so the type is the loading pool's
/// common return shape and not a PDF detail. Reading `crate::loaders::pdf::Loaded`
/// at a markdown call site suggested the wrong thing about both.
///
/// `pending_ocr` is how phase 1 tells the index that a document is incomplete.
/// It is 1-indexed and page-accurate rather than a bare boolean because the
/// same list is what a future `meta`-based backlog would need, and because
/// "which pages" is what a user reading `br8n status` actually wants.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub doc: Document,
    /// 1-indexed pages that need recognition and did not get it this pass.
    ///
    /// Always empty for a source that cannot be scanned at all.
    pub pending_ocr: Vec<u32>,
    /// Whether recognition was actually run against this file.
    ///
    /// FALSE means the engine was never called — `[pdf] ocr = "off"`, a source
    /// that is not a PDF, or the process-wide `OCR_DISABLED` latch already
    /// tripped on an earlier document. TRUE means it was called, whether it
    /// succeeded or failed.
    ///
    /// This is the difference between "OCR looked and could not read those
    /// pages" and "nothing looked", and only the first justifies recording a
    /// current stamp. See the `Err` arm of `consider_into` in `src/index.rs`.
    pub ocr_attempted: bool,
}
