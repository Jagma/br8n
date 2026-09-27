use br8n::chunk::Chunker;
use br8n::model::{Document, SourceType};

fn long_doc() -> Document {
    let body = format!(
        "# Postgres migration\n\n## Rejected approaches\n\n{}\n\n## Chosen approach\n\n{}",
        "Lock contention made this unworkable. ".repeat(80),
        "We used logical replication instead. ".repeat(80),
    );
    Document::new(
        SourceType::Markdown,
        "file:///a.md",
        "Postgres migration",
        &body,
    )
}

#[test]
fn splits_long_documents_into_multiple_chunks() {
    let chunks = Chunker::new(512, 256).chunk(&long_doc());
    assert!(chunks.len() >= 2, "got {}", chunks.len());
}

#[test]
fn chunks_are_ordered_and_uniquely_identified() {
    let doc = long_doc();
    let chunks = Chunker::new(512, 256).chunk(&doc);
    for (i, c) in chunks.iter().enumerate() {
        assert_eq!(c.ord, i as i64);
        assert_eq!(c.id, br8n::model::Chunk::id(&doc.id, i as i64));
    }
}

#[test]
fn embed_text_carries_title_and_heading_path_but_display_text_does_not() {
    let chunks = Chunker::new(512, 256).chunk(&long_doc());
    // Pick a chunk with real body text, not merely the first heading match.
    let c = chunks
        .iter()
        .find(|c| c.heading_path.contains("Rejected") && c.text.contains("Lock contention"))
        .expect("a Rejected-approaches chunk with body text");

    assert!(c.embed_text.starts_with("Postgres migration"));
    assert!(c.embed_text.contains("Rejected approaches"));
    assert!(c.text.contains("Lock contention"));
    assert!(
        !c.text.starts_with("Postgres migration >"),
        "prefix is for embedding only"
    );
}

#[test]
fn chunks_do_not_overlap_adjacency_is_a_graph_edge() {
    let chunks = Chunker::new(512, 256).chunk(&long_doc());
    for w in chunks.windows(2) {
        let tail: String = w[0]
            .text
            .chars()
            .rev()
            .take(60)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        assert!(
            !w[1].text.starts_with(&tail),
            "chunks must not duplicate text"
        );
    }
}

#[test]
fn a_chunk_is_labelled_with_its_own_section_not_the_previous_one() {
    let body = format!(
        "# Doc\n\n## Alpha\n\n{}\n\n## Beta\n\n{}",
        "alpha body. ".repeat(120),
        "beta body. ".repeat(120)
    );
    let doc = Document::new(SourceType::Markdown, "file:///l.md", "Doc", &body);
    for c in Chunker::new(512, 256).chunk(&doc) {
        if c.text.contains("beta body") {
            assert_eq!(
                c.heading_path, "Beta",
                "beta content mislabelled as a prior section"
            );
        }
        if c.text.contains("alpha body") {
            assert_eq!(
                c.heading_path, "Alpha",
                "alpha content mislabelled as a prior section"
            );
        }
    }
}

#[test]
fn heading_only_chunks_are_dropped() {
    let body = format!(
        "# Doc\n\n## Alpha\n\n{}\n\n## Beta\n\n{}",
        "alpha body. ".repeat(120),
        "beta body. ".repeat(120)
    );
    let doc = Document::new(SourceType::Markdown, "file:///h.md", "Doc", &body);
    let chunks = Chunker::new(512, 256).chunk(&doc);
    for c in &chunks {
        let has_prose = c
            .text
            .lines()
            .map(str::trim)
            .any(|l| !l.is_empty() && !l.starts_with('#'));
        assert!(has_prose, "chunk {} is heading-only: {:?}", c.ord, c.text);
    }
    // ord must stay contiguous after filtering — NEXT_CHUNK edges chain on it.
    for (i, c) in chunks.iter().enumerate() {
        assert_eq!(c.ord, i as i64);
    }
}

#[test]
fn short_documents_yield_exactly_one_chunk() {
    let doc = Document::new(
        SourceType::Markdown,
        "file:///s.md",
        "Short",
        "# Short\n\nOne line.",
    );
    assert_eq!(Chunker::new(512, 256).chunk(&doc).len(), 1);
}

#[test]
fn setext_headings_label_their_own_section_not_the_previous_one() {
    // "Doc" is a level-1 setext heading (duplicates doc.title, so it is
    // dropped from heading_path the same way a leading "# Doc" ATX H1 is).
    // "Alpha Title" / "Beta Title" are level-2 setext headings (dash
    // underline), mirroring the "## Alpha" / "## Beta" ATX structure in
    // `a_chunk_is_labelled_with_its_own_section_not_the_previous_one`.
    let body = format!(
        "Doc\n===\n\nAlpha Title\n-----------\n\n{}\n\nBeta Title\n----------\n\n{}",
        "alpha body. ".repeat(120),
        "beta body. ".repeat(120)
    );
    let doc = Document::new(SourceType::Markdown, "file:///setext.md", "Doc", &body);
    let chunks = Chunker::new(512, 256).chunk(&doc);
    let mut saw_alpha = false;
    let mut saw_beta = false;
    for c in &chunks {
        if c.text.contains("alpha body") {
            assert_eq!(c.heading_path, "Alpha Title", "alpha content mislabelled");
            saw_alpha = true;
        }
        if c.text.contains("beta body") {
            assert_eq!(c.heading_path, "Beta Title", "beta content mislabelled");
            saw_beta = true;
        }
    }
    assert!(
        saw_alpha && saw_beta,
        "expected both sections to appear in chunks"
    );
}

#[test]
fn setext_only_chunks_are_dropped() {
    let body = format!(
        "Alpha Title\n===========\n\n{}\n\nBeta Title\n----------\n\n{}",
        "alpha body. ".repeat(120),
        "beta body. ".repeat(120)
    );
    let doc = Document::new(SourceType::Markdown, "file:///setext-only.md", "Doc", &body);
    let chunks = Chunker::new(512, 256).chunk(&doc);
    for c in &chunks {
        let has_prose = c.text.lines().map(str::trim).any(|l| {
            !l.is_empty() && !l.starts_with('#') && !l.chars().all(|c| c == '=' || c == '-')
        });
        assert!(
            has_prose,
            "chunk {} is setext-heading-only: {:?}",
            c.ord, c.text
        );
    }
    // ord must stay contiguous after filtering — NEXT_CHUNK edges chain on it.
    for (i, c) in chunks.iter().enumerate() {
        assert_eq!(c.ord, i as i64);
    }
}

#[test]
fn thematic_break_after_blank_line_is_not_a_heading() {
    // The `---` here sits after a blank line, mid-section — a thematic break
    // (horizontal rule), not a setext underline for anything. Prose after it
    // must still be attributed to the enclosing "## Alpha" section, not reset
    // to empty and not attributed to "---" itself.
    let body = format!(
        "# Doc\n\n## Alpha\n\n{}\n\n---\n\n{}",
        "alpha body. ".repeat(120),
        "more prose after the horizontal rule. ".repeat(120)
    );
    let doc = Document::new(
        SourceType::Markdown,
        "file:///thematic-break.md",
        "Doc",
        &body,
    );
    let chunks = Chunker::new(512, 256).chunk(&doc);
    let mut saw_prose_after_break = false;
    for c in &chunks {
        if c.text.contains("more prose after the horizontal rule") {
            assert_eq!(
                c.heading_path, "Alpha",
                "thematic break must not reset or replace the enclosing section"
            );
            saw_prose_after_break = true;
        }
    }
    assert!(
        saw_prose_after_break,
        "expected a chunk with prose after the thematic break"
    );
}

#[test]
fn mixed_atx_and_setext_headings_are_each_attributed_correctly() {
    let body = format!(
        "# Doc\n\n## Alpha\n\n{}\n\nBeta Title\n----------\n\n{}",
        "alpha body. ".repeat(120),
        "beta body. ".repeat(120)
    );
    let doc = Document::new(SourceType::Markdown, "file:///mixed.md", "Doc", &body);
    let chunks = Chunker::new(512, 256).chunk(&doc);
    let mut saw_alpha = false;
    let mut saw_beta = false;
    for c in &chunks {
        if c.text.contains("alpha body") {
            assert_eq!(c.heading_path, "Alpha", "ATX-derived section mislabelled");
            saw_alpha = true;
        }
        if c.text.contains("beta body") {
            assert_eq!(
                c.heading_path, "Beta Title",
                "setext-derived section mislabelled"
            );
            saw_beta = true;
        }
    }
    assert!(
        saw_alpha && saw_beta,
        "expected both sections to appear in chunks"
    );
}

#[test]
fn pdf_chunks_carry_page_numbers() {
    let mut doc = Document::new(
        SourceType::Pdf,
        "file:///p.pdf",
        "Paper",
        &"word ".repeat(2000),
    );
    doc.meta =
        serde_json::json!({ "pages": [{"page": 1, "offset": 0}, {"page": 2, "offset": 3000}] });
    let chunks = Chunker::new(512, 256).chunk(&doc);
    assert!(chunks.iter().any(|c| c.page_no == Some(1)));
    assert!(chunks.iter().any(|c| c.page_no == Some(2)));
}
