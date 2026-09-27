use br8n::config::{OcrMode, PdfConfig};
use br8n::loaders::pdf::{PdfError, PdfLoader};

/// Text-layer extraction only.
///
/// Every test below that asserts pre-OCR behaviour passes this explicitly. The
/// alternative — relying on the default — would make the result depend on
/// whether PDFium and ONNX Runtime happen to be installed on the machine
/// running the suite, which is precisely the kind of invisible dependency this
/// project keeps getting bitten by.
fn no_ocr() -> PdfConfig {
    PdfConfig {
        ocr: OcrMode::Off,
        ..PdfConfig::default()
    }
}

fn load(name: &str, cfg: &PdfConfig) -> anyhow::Result<br8n::model::Document> {
    PdfLoader::load_file_with(
        std::path::Path::new(&format!("tests/fixtures/corpus/{name}")),
        cfg,
    )
}

/// The OCR tests below bypass `main`, so they must resolve the native library
/// paths themselves — `resolve_ocr_libraries`'s doc comment says exactly this
/// of any consumer that does not go through the binary. Before the env write
/// moved to `main`, `extract_with_ocr` did it lazily on this path.
fn ocr_ready() -> PdfConfig {
    let cfg = PdfConfig::default();
    br8n::loaders::pdf::resolve_ocr_libraries(&cfg);
    cfg
}

#[test]
fn extracts_text_pdf_to_markdown_with_page_offsets() {
    let doc = load("paper.pdf", &no_ocr()).unwrap();
    assert!(!doc.text.trim().is_empty());
    let pages = doc.meta["pages"].as_array().expect("page offsets recorded");
    assert!(!pages.is_empty());
    assert!(pages[0]["page"].as_i64().is_some());
    assert!(pages[0]["offset"].as_i64().is_some());
}

#[test]
fn page_offsets_point_at_the_start_of_each_page_in_a_multi_page_document() {
    // The single-page fixture cannot exercise this, and a wrong offset here
    // silently mis-attributes citations ("p. 4" pointing at page 3's text).
    let doc = load("two-page.pdf", &no_ocr()).unwrap();
    let pages = doc.meta["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 2, "fixture must span two pages");

    // Pages are stored 1-indexed for display, converted from pdf-inspector's 0-indexed value.
    assert_eq!(pages[0]["page"].as_i64(), Some(1));
    assert_eq!(pages[1]["page"].as_i64(), Some(2));

    let off1 = pages[0]["offset"].as_u64().unwrap() as usize;
    let off2 = pages[1]["offset"].as_u64().unwrap() as usize;
    assert!(off2 > off1, "page 2 must start after page 1");
    assert!(doc.text[off1..].contains("PAGE ONE MARKER"));
    assert!(
        doc.text[off2..].starts_with("```\nPAGE TWO MARKER")
            || doc.text[off2..].contains("PAGE TWO MARKER")
    );
    assert!(
        !doc.text[off2..].contains("PAGE ONE MARKER"),
        "page 2's offset must not point back into page 1"
    );
}

#[test]
fn mixed_pdf_keeps_good_pages_with_true_original_page_numbers() {
    // Three pages: 1 and 3 have a real text layer, page 2 was rasterized to
    // strip its text (pdf-inspector reports it via `needs_ocr`/empty markdown).
    let doc = load("mixed.pdf", &no_ocr()).unwrap();

    // Not rejected outright: partial signal beats total rejection.
    let pages = doc.meta["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 2, "only the two good pages should be indexed");

    // True original page numbers, not renumbered to close the gap left by
    // dropping page 2: a citation to "p. 3" must still mean page 3.
    assert_eq!(pages[0]["page"].as_i64(), Some(1));
    assert_eq!(pages[1]["page"].as_i64(), Some(3));

    let off1 = pages[0]["offset"].as_u64().unwrap() as usize;
    let off3 = pages[1]["offset"].as_u64().unwrap() as usize;
    assert!(doc.text[off1..].contains("MIXED PAGE ONE MARKER"));
    assert!(doc.text[off3..].contains("MIXED PAGE THREE MARKER"));
    assert!(
        !doc.text.contains("MIXED PAGE TWO MARKER"),
        "the scanned middle page must not be indexed"
    );
}

#[test]
fn scanned_pdf_is_rejected_loudly_when_ocr_is_off() {
    let err = load("scanned.pdf", &no_ocr()).unwrap_err();
    assert!(matches!(
        err.downcast_ref::<PdfError>(),
        Some(PdfError::Scanned { .. })
    ));
}

#[test]
fn text_pages_are_never_marked_as_recognized() {
    // The `ocr` flag is what tells a reader whether to trust a chunk. A text
    // page wrongly flagged would be as misleading as an unflagged scan.
    let doc = load("two-page.pdf", &no_ocr()).unwrap();
    for page in doc.meta["pages"].as_array().unwrap() {
        assert_eq!(page["ocr"], false);
    }
}

// --- Recognition. Requires PDFium and ONNX Runtime; never runs in CI. ---
//
// Run deliberately:
//   cargo test --test loaders_pdf -- --ignored

#[test]
#[ignore = "needs PDFium and ONNX Runtime installed"]
fn ocr_recovers_text_from_a_scanned_pdf() {
    let doc = load("scanned.pdf", &ocr_ready()).unwrap();

    // The fixture's page carries this sentence. Recognizing it end to end is
    // the entire point of the feature.
    assert!(
        doc.text
            .to_lowercase()
            .contains("retrieval augmented generation"),
        "OCR should have recovered the page text, got: {:?}",
        doc.text
    );
    let pages = doc.meta["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0]["page"].as_i64(), Some(1));
    assert_eq!(pages[0]["ocr"], true, "the page must be marked recognized");
}

#[test]
#[ignore = "needs PDFium and ONNX Runtime installed"]
fn ocr_fills_the_gap_in_a_mixed_pdf_without_renumbering() {
    let doc = load("mixed.pdf", &ocr_ready()).unwrap();
    let pages = doc.meta["pages"].as_array().unwrap();

    assert_eq!(pages.len(), 3, "the scanned middle page should now be read");
    assert_eq!(pages[0]["page"].as_i64(), Some(1));
    assert_eq!(pages[1]["page"].as_i64(), Some(2));
    assert_eq!(pages[2]["page"].as_i64(), Some(3));

    // Only the middle page was recognized; the others keep their text layer.
    assert_eq!(pages[0]["ocr"], false);
    assert_eq!(pages[1]["ocr"], true);
    assert_eq!(pages[2]["ocr"], false);

    assert!(doc.text.contains("MIXED PAGE TWO MARKER"));

    // Offsets must still be ascending once a page is inserted into the gap.
    let offsets: Vec<u64> = pages
        .iter()
        .map(|p| p["offset"].as_u64().unwrap())
        .collect();
    assert!(
        offsets.windows(2).all(|w| w[1] > w[0]),
        "offsets: {offsets:?}"
    );
}

#[test]
#[ignore = "needs PDFium and ONNX Runtime installed"]
fn ocr_enabled_mode_does_not_perturb_a_text_pdf() {
    // NOT an OCR-success test: a text PDF short-circuits recognition either
    // way, so this passes whether or not PDFium and ONNX Runtime resolve.
    //
    // Enabling OCR must not perturb documents that never needed it. If this
    // ever fails, recognition has started running where it should not.
    let off = load("paper.pdf", &no_ocr()).unwrap();
    let on = load("paper.pdf", &ocr_ready()).unwrap();
    assert_eq!(off.text, on.text);
}

/// The env write moved out of the loader and into `main`, because
/// `std::env::set_var` races any concurrent `getenv` and the MCP server
/// already runs indexing and search on different threads of one runtime.
///
/// Serialized against the other env test in this file by running them in one
/// test: Rust runs tests in parallel threads, and two tests mutating the same
/// variable are exactly the race under repair.
#[test]
fn resolve_ocr_libraries_sets_only_what_is_unset() {
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("libpdfium.dylib");
    std::fs::write(&fake, b"x").unwrap();

    let prior_pdfium = std::env::var_os("PDFIUM_LIB_PATH");
    let prior_ort = std::env::var_os("ORT_DYLIB_PATH");

    // Unset: the explicit config path must win and be written.
    std::env::remove_var("PDFIUM_LIB_PATH");
    let cfg = br8n::config::PdfConfig {
        pdfium_lib_path: Some(fake.clone()),
        ..br8n::config::PdfConfig::default()
    };
    br8n::loaders::pdf::resolve_ocr_libraries(&cfg);
    let wrote = std::env::var_os("PDFIUM_LIB_PATH");

    // Already set: an existing value must never be overwritten.
    std::env::set_var("PDFIUM_LIB_PATH", "/set/by/the/user.dylib");
    br8n::loaders::pdf::resolve_ocr_libraries(&cfg);
    let kept = std::env::var_os("PDFIUM_LIB_PATH");

    match prior_pdfium {
        Some(v) => std::env::set_var("PDFIUM_LIB_PATH", v),
        None => std::env::remove_var("PDFIUM_LIB_PATH"),
    }
    match prior_ort {
        Some(v) => std::env::set_var("ORT_DYLIB_PATH", v),
        None => std::env::remove_var("ORT_DYLIB_PATH"),
    }

    assert_eq!(
        wrote.as_deref(),
        Some(fake.as_os_str()),
        "an unset variable is resolved"
    );
    assert_eq!(
        kept.as_deref(),
        Some(std::ffi::OsStr::new("/set/by/the/user.dylib")),
        "a variable the user set must never be overwritten"
    );
}

/// Phase 1 loads with OCR off and has to know which PDFs still owe work. The
/// dropped pages were previously printed to stderr and discarded, so the
/// caller could not tell a clean text PDF from a mixed one.
#[test]
fn load_file_reporting_names_the_pages_that_still_need_ocr() {
    // `mixed.pdf` has a text layer on some pages and a scan on another.
    let mixed = br8n::loaders::pdf::PdfLoader::load_file_reporting(
        std::path::Path::new("tests/fixtures/corpus/mixed.pdf"),
        &no_ocr(),
    )
    .expect("the good pages still load");
    assert!(
        !mixed.pending_ocr.is_empty(),
        "a mixed PDF read without OCR still owes its scanned pages"
    );
    assert!(
        !mixed.doc.text.trim().is_empty(),
        "its good pages are still indexed"
    );

    // A clean text PDF owes nothing.
    let clean = br8n::loaders::pdf::PdfLoader::load_file_reporting(
        std::path::Path::new("tests/fixtures/corpus/paper.pdf"),
        &no_ocr(),
    )
    .expect("a text PDF loads");
    assert!(
        clean.pending_ocr.is_empty(),
        "a clean text PDF must not be queued for OCR, got {:?}",
        clean.pending_ocr
    );
}
