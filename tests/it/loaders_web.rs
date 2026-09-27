use br8n::loaders::web::WebLoader;
use br8n::model::SourceType;

const PAGE: &str = r#"
<html><head><title>Hybrid Search</title></head>
<body>
  <nav><a href="/">home</a><a href="/about">about</a></nav>
  <article>
    <h1>Hybrid Search</h1>
    <p>Combining BM25 with dense retrieval improves recall on rare terms
       such as identifiers and proper nouns, which pure vector search misses.</p>
    <p>Reciprocal rank fusion is the usual way to merge the two result lists.</p>
  </article>
  <footer>copyright 2026</footer>
</body></html>"#;

#[test]
fn extracts_article_and_drops_chrome() {
    let doc = WebLoader::from_html("https://example.com/hybrid", PAGE).unwrap();
    assert!(doc.text.contains("Reciprocal rank fusion"));
    assert!(!doc.text.contains("copyright 2026"), "footer is chrome");
    assert!(!doc.text.contains("about"), "nav is chrome");
}

// Dedicated fixture for the heading-conversion assertion below. Its <title>
// deliberately differs from the <h1> text: dom_smoothie's Readability drops
// a body heading entirely when it duplicates the page title (verified by
// direct experiment against this crate's actual dependency version), so a
// fixture where they match cannot exercise heading-to-Markdown conversion at
// all — it would trivially pass with the heading missing from the body.
const HEADING_PAGE: &str = r#"
<html><head><title>Search Techniques</title></head>
<body>
  <nav><a href="/">home</a><a href="/about">about</a></nav>
  <article>
    <h1>Hybrid Search</h1>
    <p>Combining BM25 with dense retrieval improves recall on rare terms
       such as identifiers and proper nouns, which pure vector search misses.</p>
    <p>Reciprocal rank fusion is the usual way to merge the two result lists.</p>
  </article>
  <footer>copyright 2026</footer>
</body></html>"#;

#[test]
fn output_is_markdown_not_html() {
    // Sanity-check the fixture itself, so the HTML-leak assertion below
    // can't pass just because there was never a `<p>` tag to leak.
    assert!(
        PAGE.contains("<p>"),
        "fixture must contain <p> for this test to mean anything"
    );

    let doc = WebLoader::from_html("https://example.com/hybrid", PAGE).unwrap();
    assert!(!doc.text.contains("<p>"), "raw HTML leaked into doc.text");

    // Guard against "conversion produced nothing": the paragraph prose must
    // have survived conversion, not just the absence of literal HTML tags.
    assert!(
        doc.text.contains("Reciprocal rank fusion"),
        "paragraph prose did not survive conversion: {:?}",
        doc.text
    );

    // Body heading syntax, checked separately against HEADING_PAGE (see its
    // comment for why PAGE itself can't test this). Readability re-wraps the
    // article, so an input <h1> can emerge at a different heading level
    // (here, "##"). Checking for the "# " prefix as a substring is
    // deliberately level-agnostic: "# Hybrid Search" is a substring of
    // "## Hybrid Search". Do not tighten this into an exact match on a
    // single "#".
    let heading_doc = WebLoader::from_html("https://example.com/hybrid", HEADING_PAGE).unwrap();
    assert!(
        heading_doc.text.contains("# Hybrid Search"),
        "body heading did not survive conversion to Markdown: {:?}",
        heading_doc.text
    );
}

#[test]
fn records_domain_and_source_type() {
    let doc = WebLoader::from_html("https://example.com/hybrid", PAGE).unwrap();
    assert_eq!(doc.source_type, SourceType::Web);
    assert_eq!(doc.meta["domain"], "example.com");
    assert_eq!(doc.uri, "https://example.com/hybrid");
}
