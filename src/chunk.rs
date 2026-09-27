use crate::model::{Chunk, Document};
use text_splitter::{ChunkConfig, MarkdownSplitter};

pub struct Chunker {
    target: usize,
    min: usize,
}

impl Chunker {
    pub fn new(target: usize, min: usize) -> Self {
        Self { target, min }
    }

    /// Split a document into chunks of roughly `target` tokens.
    ///
    /// "Roughly" is load-bearing. Sizing is done in CHARACTERS at four per
    /// token, which holds for English prose and does not hold for the two cases
    /// most likely to be in a developer's notes: code, where punctuation density
    /// pushes real tokens well above the estimate, and CJK, where a single
    /// character is often a whole token. A "512-token" chunk of dense code can
    /// be half again that; a CJK chunk can be four times it.
    ///
    /// This is a deliberate trade — no tokenizer dependency, no per-chunk
    /// tokenization cost during indexing — and it is safe because the embedding
    /// model truncates rather than errors on overflow. It is documented here
    /// because "512 tokens" reads as exact and is not. Swapping in a real
    /// tokenizer is a one-line change to `ChunkConfig` if bench shows drift.
    pub fn chunk(&self, doc: &Document) -> Vec<Chunk> {
        let cfg = ChunkConfig::new(self.min * 4..=self.target * 4);
        let splitter = MarkdownSplitter::new(cfg);

        let pages = doc.meta.get("pages").and_then(|p| p.as_array()).cloned();

        let items: Vec<(usize, &str)> = splitter
            .chunk_indices(&doc.text)
            // A heading is its own semantic unit, so the splitter emits chunks that
            // are nothing but a heading line. Those carry no information: they get
            // embedded, occupy index space, and can match a query while returning
            // nothing. Drop them; the heading survives as the next chunk's prefix.
            .filter(|(_, text)| !is_heading_only(text))
            .collect();

        // The splitter greedily merges a short section's heading together with
        // its own body into a single chunk (e.g. a small "# Doc" H1 plus its
        // immediate "## Alpha" child both land in the chunk that starts at
        // offset 0). In that case `offset` still points at the OUTER heading,
        // so asking for the heading path "as of offset" alone would miss the
        // INNER heading that the chunk's own prose actually belongs to. Skip
        // past any run of heading/blank lines this chunk starts with before
        // asking what section governs it.
        let heading_offsets: Vec<usize> = items
            .iter()
            .map(|(offset, text)| offset + content_start(text))
            .collect();
        let heading_paths = heading_paths_at(&doc.text, &heading_offsets);

        items
            .into_iter()
            .zip(heading_paths)
            .enumerate()
            .map(|(i, ((offset, text), heading_path))| {
                let embed_text = Chunk::plain_embed_text(&doc.title, &heading_path, text);
                Chunk {
                    id: Chunk::id(&doc.id, i as i64),
                    doc_id: doc.id.clone(),
                    ord: i as i64,
                    text: text.to_string(),
                    embed_text,
                    heading_path,
                    page_no: pages.as_ref().and_then(|p| page_for_offset(p, offset)),
                }
            })
            .collect()
    }
}

/// Strips a trailing '\r' from a line taken from `text.split('\n')`, without
/// touching byte-position bookkeeping (callers add back `raw_line.len() + 1`,
/// not `line.len() + 1` — see the CRLF note on `heading_paths_at`).
fn strip_cr(raw_line: &str) -> &str {
    raw_line.strip_suffix('\r').unwrap_or(raw_line)
}

/// Level of a setext underline: a trimmed line of only '=' (level 1) or only
/// '-' (level 2), at least one character. Anything else, including empty, is
/// `None`.
fn underline_level(trimmed: &str) -> Option<usize> {
    if trimmed.is_empty() {
        None
    } else if trimmed.chars().all(|c| c == '=') {
        Some(1)
    } else if trimmed.chars().all(|c| c == '-') {
        Some(2)
    } else {
        None
    }
}

/// If `lines[i]` is a valid setext title line — non-blank, not itself an ATX
/// heading, not itself underline-shaped — and `lines[i + 1]` is a setext
/// underline, returns the heading level. A `---` (or `===`) directly after a
/// blank line fails here because the *title candidate* (the blank line) is
/// empty, which is exactly the thematic-break case: a `---` preceded by a
/// blank line must not be read as an underline.
fn setext_level_at(lines: &[&str], i: usize) -> Option<usize> {
    let title = lines.get(i).map(|l| strip_cr(l).trim())?;
    if title.is_empty() || title.starts_with('#') || underline_level(title).is_some() {
        return None;
    }
    let underline = strip_cr(lines.get(i + 1)?).trim();
    underline_level(underline)
}

/// Reconstructs the heading ancestry (ATX and setext) above each of `offsets`,
/// e.g. "Rejected approaches" or "Results > Ablations". This is what rescues
/// chunks full of pronouns.
fn heading_paths_at(text: &str, offsets: &[usize]) -> Vec<String> {
    let mut order: Vec<usize> = (0..offsets.len()).collect();
    order.sort_by_key(|&i| offsets[i]);

    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut pos = 0usize;
    // Split on '\n' directly rather than `str::lines()`: `lines()` silently strips a
    // trailing '\r' without reporting how many bytes it dropped, which under CRLF
    // input undercounts `pos` by 1 byte per line. That drift accumulates until it can
    // wrongly pull a heading that appears *after* `offset` into the stack. Splitting
    // on '\n' keeps any '\r' in the segment, so `raw_line.len() + 1` (for the '\n')
    // always matches the real number of bytes consumed.
    let lines: Vec<&str> = text.split('\n').collect();
    let mut i = 0usize;
    let mut results = vec![String::new(); offsets.len()];
    for idx in order {
        let offset = offsets[idx];
        while i < lines.len() {
            // `>` not `>=`: a chunk usually STARTS at a heading line, and that heading
            // is the chunk's own section. Breaking at `>=` skips it and labels the
            // chunk with the PREVIOUS section — worse than no prefix, because the
            // embedding then carries a confidently wrong section name.
            if pos > offset {
                break;
            }
            let raw_line = lines[i];
            let line = strip_cr(raw_line);
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') {
                let level = trimmed.chars().take_while(|c| *c == '#').count();
                let title = trimmed[level..].trim().to_string();
                if !title.is_empty() {
                    stack.retain(|(l, _)| *l < level);
                    stack.push((level, title));
                }
                pos += raw_line.len() + 1;
                i += 1;
                continue;
            }
            if let Some(level) = setext_level_at(&lines, i) {
                let title = line.trim().to_string();
                if !title.is_empty() {
                    stack.retain(|(l, _)| *l < level);
                    stack.push((level, title));
                }
                // The underline is part of the same heading unit — consume both
                // lines' byte counts before the next `pos > offset` check.
                pos += raw_line.len() + 1;
                i += 1;
                pos += lines[i].len() + 1;
                i += 1;
                continue;
            }
            pos += raw_line.len() + 1;
            i += 1;
        }
        // Drop the document's own H1 — it duplicates doc.title in embed_text.
        let parts: Vec<String> = stack
            .iter()
            .skip_while(|(l, _)| *l == 1)
            .map(|(_, t)| t.clone())
            .collect();
        results[idx] = parts.join(" > ");
    }
    results
}

/// Byte offset, relative to the start of `text`, of the first line that is
/// neither blank, nor an ATX heading, nor a setext heading (title + underline).
/// Used to look past a chunk's own leading heading line(s) so `heading_paths_at`
/// reports the section that governs the chunk's actual prose rather than the
/// outer heading it happened to merge with.
fn content_start(text: &str) -> usize {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut pos = 0usize;
    let mut i = 0usize;
    while i < lines.len() {
        let raw_line = lines[i];
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            pos += raw_line.len() + 1;
            i += 1;
            continue;
        }
        if setext_level_at(&lines, i).is_some() {
            pos += raw_line.len() + 1;
            i += 1;
            pos += lines[i].len() + 1;
            i += 1;
            continue;
        }
        break;
    }
    pos.min(text.len())
}

/// True when a chunk contains nothing but ATX heading lines, setext heading
/// pairs (title + underline), and whitespace.
fn is_heading_only(text: &str) -> bool {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut i = 0usize;
    while i < lines.len() {
        let trimmed = strip_cr(lines[i]).trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }
        if setext_level_at(&lines, i).is_some() {
            i += 2;
            continue;
        }
        return false;
    }
    true
}

fn page_for_offset(pages: &[serde_json::Value], offset: usize) -> Option<i64> {
    let mut current = None;
    for p in pages {
        let po = p["offset"].as_i64()? as usize;
        if po <= offset {
            current = p["page"].as_i64();
        } else {
            break;
        }
    }
    current
}

#[cfg(test)]
fn heading_path_at(text: &str, offset: usize) -> String {
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut pos = 0usize;
    // Split on '\n' directly rather than `str::lines()`: `lines()` silently strips a
    // trailing '\r' without reporting how many bytes it dropped, which under CRLF
    // input undercounts `pos` by 1 byte per line. That drift accumulates until it can
    // wrongly pull a heading that appears *after* `offset` into the stack. Splitting
    // on '\n' keeps any '\r' in the segment, so `raw_line.len() + 1` (for the '\n')
    // always matches the real number of bytes consumed.
    let lines: Vec<&str> = text.split('\n').collect();
    let mut i = 0usize;
    while i < lines.len() {
        // `>` not `>=`: a chunk usually STARTS at a heading line, and that heading
        // is the chunk's own section. Breaking at `>=` skips it and labels the
        // chunk with the PREVIOUS section — worse than no prefix, because the
        // embedding then carries a confidently wrong section name.
        if pos > offset {
            break;
        }
        let raw_line = lines[i];
        let line = strip_cr(raw_line);
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            let level = trimmed.chars().take_while(|c| *c == '#').count();
            let title = trimmed[level..].trim().to_string();
            if !title.is_empty() {
                stack.retain(|(l, _)| *l < level);
                stack.push((level, title));
            }
            pos += raw_line.len() + 1;
            i += 1;
            continue;
        }
        if let Some(level) = setext_level_at(&lines, i) {
            let title = line.trim().to_string();
            if !title.is_empty() {
                stack.retain(|(l, _)| *l < level);
                stack.push((level, title));
            }
            // The underline is part of the same heading unit — consume both
            // lines' byte counts before the next `pos > offset` check.
            pos += raw_line.len() + 1;
            i += 1;
            pos += lines[i].len() + 1;
            i += 1;
            continue;
        }
        pos += raw_line.len() + 1;
        i += 1;
    }
    // Drop the document's own H1 — it duplicates doc.title in embed_text.
    let parts: Vec<String> = stack
        .into_iter()
        .skip_while(|(l, _)| *l == 1)
        .map(|(_, t)| t)
        .collect();
    parts.join(" > ")
}

#[cfg(test)]
mod self_review {
    use super::*;

    fn shipped_heading_path_at(text: &str, offset: usize) -> String {
        heading_paths_at(text, &[offset]).remove(0)
    }

    #[test]
    fn heading_path_survives_crlf_line_endings() {
        // CRLF: str::lines() strips "\r\n" (2 bytes) per line ending, but the loop
        // advances `pos` by `line.len() + 1`, assuming a bare "\n" (1 byte). Confirm
        // whether that drift throws off heading attribution.
        let text = "# Title\r\n\r\n## Section\r\n\r\nBody text here.\r\n";
        let offset = text.find("Body text").unwrap();
        assert_eq!(shipped_heading_path_at(text, offset), "Section");
    }

    #[test]
    fn heading_path_crlf_drift_does_not_pull_in_a_later_heading() {
        // With enough CRLF lines, the 1-byte-per-line undercount accumulates until
        // `pos` (which always lags the true byte offset under CRLF) is still less
        // than `offset` by the time the loop reaches a heading that, in the real
        // text, comes AFTER the target offset. That heading must NOT be included.
        let blanks = "\r\n".repeat(200);
        let text = format!("# Title\r\n{blanks}Target.\r\n## Sneaky\r\nMore.");
        let offset = text.find("Target.").unwrap();
        assert_eq!(
            shipped_heading_path_at(&text, offset),
            "",
            "a heading appearing after the real offset must not be attributed to it"
        );
    }

    #[test]
    fn heading_path_survives_multibyte_utf8_before_offset() {
        let text = "# Café Notes\n\n## Résumé\n\nBody café après.";
        let offset = text.find("Body caf").unwrap();
        assert_eq!(shipped_heading_path_at(text, offset), "Résumé");
    }

    #[test]
    fn heading_path_starting_at_h2_with_no_h1() {
        let text = "## Section A\n\nBody one.\n\n## Section B\n\nBody two.";
        let offset = text.find("Body two").unwrap();
        assert_eq!(shipped_heading_path_at(text, offset), "Section B");
    }

    #[test]
    fn heading_path_two_sibling_h1s() {
        let text = "# First\n\nBody one.\n\n# Second\n\nBody two.";
        let offset = text.find("Body two").unwrap();
        // Both H1s are dropped (they duplicate doc.title), so the path is empty
        // even though "Second" is a distinct sibling from "First".
        assert_eq!(shipped_heading_path_at(text, offset), "");
    }

    #[test]
    fn heading_path_jump_from_h1_to_h3() {
        let text = "# Title\n\n### Deep section\n\nBody text.";
        let offset = text.find("Body text").unwrap();
        assert_eq!(shipped_heading_path_at(text, offset), "Deep section");
    }

    #[test]
    fn heading_hash_in_title_text_does_not_confuse_level() {
        let text = "# Title\n\n## Issue #42\n\nBody text.";
        let offset = text.find("Body text").unwrap();
        assert_eq!(shipped_heading_path_at(text, offset), "Issue #42");
    }

    #[test]
    fn page_for_offset_matches_exact_boundary() {
        let pages = vec![
            serde_json::json!({"page": 1, "offset": 0}),
            serde_json::json!({"page": 2, "offset": 3000}),
        ];
        // Exactly on page 2's recorded start offset must report page 2, not page 1.
        assert_eq!(page_for_offset(&pages, 3000), Some(2));
        assert_eq!(page_for_offset(&pages, 2999), Some(1));
    }

    #[test]
    fn embed_text_has_no_stray_separator_when_no_headings() {
        let doc = Document::new(
            crate::model::SourceType::Markdown,
            "file:///no-headings.md",
            "Plain Doc",
            "Just a paragraph, no headings anywhere in this text at all.",
        );
        let chunks = Chunker::new(512, 256).chunk(&doc);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].heading_path, "");
        assert_eq!(
            chunks[0].embed_text,
            "Plain Doc\n\nJust a paragraph, no headings anywhere in this text at all."
        );
        assert!(!chunks[0].embed_text.contains(" > "));
    }
}

#[cfg(test)]
mod heading_paths_differential {
    use super::*;
    use std::path::{Path, PathBuf};

    fn splitter_offsets(text: &str) -> Vec<usize> {
        let cfg = ChunkConfig::new(40..=160);
        let splitter = MarkdownSplitter::new(cfg);
        splitter
            .chunk_indices(text)
            .filter(|(_, t)| !is_heading_only(t))
            .map(|(offset, t)| offset + content_start(t))
            .collect()
    }

    fn oracle_paths(text: &str, offsets: &[usize]) -> Vec<String> {
        offsets.iter().map(|&o| heading_path_at(text, o)).collect()
    }

    fn assert_matches_oracle(text: &str, offsets: &[usize]) -> usize {
        let got = heading_paths_at(text, offsets);
        let want = oracle_paths(text, offsets);
        assert_eq!(got, want, "text: {text:?}");
        offsets.len()
    }

    fn fixture_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                fixture_files(&path, out);
            } else {
                out.push(path);
            }
        }
    }

    #[test]
    fn matches_oracle_on_every_chunk_offset_of_every_fixture() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut files = Vec::new();
        fixture_files(&root, &mut files);
        assert!(!files.is_empty(), "expected fixtures under {root:?}");

        let mut total_offsets = 0usize;
        let mut checked_files = 0usize;
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let offsets = splitter_offsets(&text);
            total_offsets += assert_matches_oracle(&text, &offsets);
            checked_files += 1;
        }
        assert!(checked_files > 0, "no UTF-8 fixtures were readable");
        assert!(total_offsets > 0, "no chunk offsets were produced");
    }

    struct SplitMix64(u64);

    impl SplitMix64 {
        fn new(seed: u64) -> Self {
            Self(seed)
        }

        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
            z ^ (z >> 31)
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next_u64() % bound as u64) as usize
        }

        fn one_in(&mut self, n: u64) -> bool {
            self.next_u64().is_multiple_of(n)
        }
    }

    const LINES: &[&str] = &[
        "# Heading one",
        "## Heading two",
        "### Heading three",
        "#### Heading four",
        "##### Heading five",
        "###### Heading six",
        "#",
        "##   ",
        "   ## Indented heading",
        "\t### Tab indented heading",
        "#tag",
        "#🎉celebrate",
        "Body prose with café, façade, and naïve words.",
        "Body prose with 日本語 characters mixed in.",
        "Body prose with an emoji 🎉 in the middle.",
        "Plain body text, nothing special about it at all.",
        "#!/bin/bash",
        "# comment, not a heading in a real fence",
        "```bash",
        "```",
        "---",
        "===",
        "- a list item, not a heading",
        "> a blockquote line",
    ];

    const SETEXT_TITLES: &[&str] = &["Setext title one", "Résumé heading", "标题 unicode setext"];

    const SETEXT_UNDERLINES: &[&str] = &["===", "---", "======", "------"];

    fn generate_document(rng: &mut SplitMix64) -> String {
        let line_count = 3 + rng.below(6);
        let mut lines: Vec<&str> = Vec::new();
        while lines.len() < line_count {
            if rng.one_in(5) {
                lines.push(SETEXT_TITLES[rng.below(SETEXT_TITLES.len())]);
                lines.push(SETEXT_UNDERLINES[rng.below(SETEXT_UNDERLINES.len())]);
            } else {
                lines.push(LINES[rng.below(LINES.len())]);
            }
        }

        let mut text = String::new();
        for (i, line) in lines.iter().enumerate() {
            text.push_str(line);
            if i + 1 < lines.len() {
                text.push_str(if rng.one_in(2) { "\n\n" } else { "\n" });
            }
        }
        text.push('\n');

        if rng.one_in(2) {
            text = text.replace('\n', "\r\n");
        }
        text
    }

    fn offsets_for(text: &str) -> Vec<usize> {
        let mut offsets: Vec<usize> = (0..=text.len())
            .filter(|o| text.is_char_boundary(*o))
            .collect();
        offsets.push(text.len() + 1);
        offsets.push(text.len() + 50);
        offsets
    }

    fn shuffled_with_duplicates(rng: &mut SplitMix64, offsets: &[usize]) -> Vec<usize> {
        let mut doubled: Vec<usize> = offsets.iter().chain(offsets.iter()).copied().collect();
        for i in (1..doubled.len()).rev() {
            let j = rng.below(i + 1);
            doubled.swap(i, j);
        }
        doubled
    }

    #[test]
    fn matches_oracle_over_a_seeded_generated_corpus() {
        let docs_per_seed = 300;
        for seed in [1u64, 2, 3, 42, 1_000_003] {
            let mut rng = SplitMix64::new(seed);
            for _ in 0..docs_per_seed {
                let text = generate_document(&mut rng);
                let offsets = offsets_for(&text);
                let cache: std::collections::HashMap<usize, String> = offsets
                    .iter()
                    .map(|&o| (o, heading_path_at(&text, o)))
                    .collect();

                let got = heading_paths_at(&text, &offsets);
                let want: Vec<String> = offsets.iter().map(|o| cache[o].clone()).collect();
                assert_eq!(got, want, "seed {seed}, text: {text:?}");

                let scrambled = shuffled_with_duplicates(&mut rng, &offsets);
                let got_scrambled = heading_paths_at(&text, &scrambled);
                let want_scrambled: Vec<String> =
                    scrambled.iter().map(|o| cache[o].clone()).collect();
                assert_eq!(
                    got_scrambled, want_scrambled,
                    "seed {seed} scrambled, text: {text:?}"
                );
            }
        }
    }
}
