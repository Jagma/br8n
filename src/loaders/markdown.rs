use crate::model::{Document, SourceType};
use anyhow::Result;
use once_cell::sync::Lazy;
use regex::Regex;
use std::path::{Path, PathBuf};

// Requires a closing `]]` and excludes `[` from the target. The earlier
// unanchored pattern `\[\[([^\]\|#]+)` swallowed everything up to the next
// `]`/`|`/`#` — an unclosed `[[note` captured across paragraph breaks.
//
// Also excludes `\n`: `extract_wikilinks` inserts a newline at every block
// boundary (paragraph/heading/list-item end) precisely so a dangling `[[foo`
// at the end of one block can never join a stray `bar]]` at the start of the
// next into a phantom `foobar` link. That guarantee only holds if the token
// itself cannot span the separator.
static WIKILINK: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[\[([^\[\]|#\n]+)(?:[|#][^\[\]\n]*)?\]\]").unwrap());

/// YAML frontmatter we care about. Everything else in the block is ignored.
///
/// The lifecycle fields are `Option<String>` rather than `String` because every
/// one of them is routinely present-but-empty in the vault's own template
/// (`supersedes:` with no value), and an empty YAML value is NULL, not "".
/// `deserialize_option` maps `Pod::Null` to `visit_none`; a plain `String`
/// would fail the whole block and drop `title` and `tags` with it.
#[derive(Debug, Default, serde::Deserialize)]
struct Frontmatter {
    title: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    /// The record's own stable identifier, e.g. `ADR-0004`. Distinct from
    /// `Document.id`, which is a content hash.
    id: Option<String>,
    /// Lifecycle position: `proposed`, `accepted`, `superseded`, ... Free text
    /// here; `Lifecycle::from_status` in Task 4 is what maps it to a position,
    /// and unknown values map to `Proposed`.
    status: Option<String>,
    supersedes: Option<String>,
    #[serde(rename = "superseded-by")]
    superseded_by: Option<String>,
}

/// Wikilink targets found in prose. Code is excluded: C++'s `[[nodiscard]]`
/// attribute inside a fenced block is not a link to a note called "nodiscard",
/// and treating it as one puts a garbage `LINKS_TO` edge in the graph, which
/// then drags unrelated notes into retrieval via graph expansion.
///
/// Verified behaviour:
///   "[[note]]"                        -> ["note"]
///   "[[note|alias]]" / "[[note#sec]]" -> ["note"]
///   "```cpp\n[[nodiscard]] int f();\n```" -> []
///   "inline `[[nodiscard]]` code"     -> []
///   "unclosed [[note\n\nmore ] text"  -> []
pub fn extract_wikilinks(text: &str) -> Vec<String> {
    use pulldown_cmark::{Event, Parser, Tag, TagEnd};

    let mut prose = String::new();
    let mut in_code = false;
    for ev in Parser::new(text) {
        match ev {
            Event::Start(Tag::CodeBlock(_)) => in_code = true,
            Event::End(TagEnd::CodeBlock) => in_code = false,
            // Inline code arrives as Event::Code and is simply never collected.
            Event::Text(t) if !in_code => prose.push_str(&t),
            Event::SoftBreak | Event::HardBreak if !in_code => prose.push('\n'),
            // Block boundaries: text from one paragraph/heading/list item must
            // never concatenate directly onto the text of the next. Without
            // this, `[[foo` ending one block and `bar]]` starting the next
            // join into a phantom `[[foobar]]` link.
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item) if !in_code => {
                prose.push('\n')
            }
            _ => {}
        }
    }

    WIKILINK
        .captures_iter(&prose)
        .map(|c| c[1].trim().to_string())
        .collect()
}

/// Is this path excluded by `Config::ignore`?
///
/// Compared against every path COMPONENT, exactly, case-sensitively. Exact so
/// `_templates2` is not swept up by `_templates`; case-sensitive because
/// `Templates` is deliberately not in the default list — a vault may keep real
/// notes there, and case-folding would silently decide otherwise.
///
/// A free function rather than a `MarkdownLoader` method because there are TWO
/// walkers: `MarkdownLoader::load_all` and `index::discover_stat_first`. Only
/// the second one indexes the user's corpus; a change to the first alone would
/// pass its own tests and do nothing.
pub fn is_ignored(path: &std::path::Path, ignore: &[String]) -> bool {
    !ignore.is_empty()
        && path.components().any(|c| {
            c.as_os_str()
                .to_str()
                .is_some_and(|s| ignore.iter().any(|i| i == s))
        })
}

pub struct MarkdownLoader {
    root: PathBuf,
    ignore: Vec<String>,
}

impl MarkdownLoader {
    pub fn new<P: AsRef<Path>>(root: P) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            ignore: Vec::new(),
        }
    }

    pub fn with_ignore(root: impl AsRef<Path>, ignore: &[String]) -> Self {
        let mut l = Self::new(root);
        l.ignore = ignore.to_vec();
        l
    }

    pub fn load_all(&self) -> Result<Vec<Document>> {
        let mut out = Vec::new();
        // `ignore` honours .gitignore / .ignore. `.build()` is the sequential walker;
        // `.build_parallel()` would be the parallel one.
        for entry in ignore::WalkBuilder::new(&self.root).build() {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            if is_ignored(path, &self.ignore) {
                continue;
            }
            match Self::load_file(path) {
                Ok(doc) => out.push(doc),
                Err(e) => eprintln!("br8n: skipping {}: {e}", path.display()),
            }
        }
        out.sort_by(|a, b| a.uri.cmp(&b.uri));
        Ok(out)
    }

    pub fn load_file(path: &Path) -> Result<Document> {
        let raw = std::fs::read_to_string(path)?;

        // gray_matter 0.3: `parse` is generic and fallible. A typed struct beats
        // the old untyped `Pod` indexing — malformed frontmatter degrades to
        // defaults instead of silently yielding the wrong type.
        //
        // Destructure rather than constructing a fallback `ParsedEntity`: that
        // struct's shape is not ours to depend on, and this needs no import.
        // Verified: malformed YAML yields an empty `Frontmatter` and the body
        // intact, never a panic.
        let matter = gray_matter::Matter::<gray_matter::engine::YAML>::new();
        let (body, fm) = match matter.parse::<Frontmatter>(&raw) {
            Ok(p) => (p.content, p.data.unwrap_or_default()),
            // The constraint is that frontmatter never reaches `Document.text`
            // on ANY path. Returning `raw` here would leak the `---` block into
            // every chunk of the document.
            Err(_) => (strip_frontmatter_block(&raw), Frontmatter::default()),
        };

        let fm_title = fm.title.filter(|t| !t.trim().is_empty());
        let tags = fm.tags;

        let title = fm_title
            .or_else(|| first_heading(&body))
            .unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("untitled")
                    .to_string()
            });

        // Lifecycle relations into `meta`, the carrier `index.rs` already reads
        // for `domain` (-> Source node) and `project` (-> Entity). Only
        // non-empty values become keys, so a document declaring none of them
        // keeps `meta` NULL and its `pack.rec` bytes unchanged.
        let mut lifecycle = serde_json::Map::new();
        for (key, value) in [
            ("id", &fm.id),
            ("status", &fm.status),
            ("supersedes", &fm.supersedes),
            ("superseded_by", &fm.superseded_by),
        ] {
            if let Some(v) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                lifecycle.insert(key.to_string(), serde_json::Value::String(v.to_string()));
            }
        }

        let uri = format!("file://{}", path.canonicalize()?.display());
        let mut doc = Document::new(SourceType::Markdown, &uri, &title, &body);
        if !lifecycle.is_empty() {
            doc.meta = serde_json::Value::Object(lifecycle);
        }
        doc.links = extract_wikilinks(&body);
        doc.tags = tags;
        Ok(doc)
    }
}

/// Fallback frontmatter stripper for when the YAML parser rejects the block.
/// Removes a leading `---` fence up to the next line that is exactly `---` or
/// `...`. An unterminated opener is left alone — a file that merely starts with
/// a horizontal rule is not frontmatter, and eating it would destroy the note.
fn strip_frontmatter_block(raw: &str) -> String {
    let s = raw.trim_start_matches('\u{feff}');
    if !s.starts_with("---") {
        return s.to_string();
    }
    let rest = match s.split_once('\n') {
        Some((_, r)) => r,
        None => return s.to_string(),
    };
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        let t = line.trim_end();
        if t == "---" || t == "..." {
            return rest[offset + line.len()..].to_string();
        }
        offset += line.len();
    }
    s.to_string()
}

fn first_heading(md: &str) -> Option<String> {
    use pulldown_cmark::{Event, Parser, Tag, TagEnd};
    let mut parser = Parser::new(md);
    while let Some(ev) = parser.next() {
        if let Event::Start(Tag::Heading { .. }) = ev {
            let mut text = String::new();
            for ev in parser.by_ref() {
                match ev {
                    Event::Text(t) | Event::Code(t) => text.push_str(&t),
                    Event::End(TagEnd::Heading(_)) => {
                        // An empty heading must fall through to the filename,
                        // not become an empty-string title.
                        let t = text.trim();
                        return if t.is_empty() {
                            None
                        } else {
                            Some(t.to_string())
                        };
                    }
                    _ => {}
                }
            }
        }
    }
    None
}

impl super::Loader for MarkdownLoader {
    fn load(&self, uri: &str) -> Result<Vec<Document>> {
        let path = uri.strip_prefix("file://").unwrap_or(uri);
        Ok(vec![Self::load_file(Path::new(path))?])
    }
}
