use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceType {
    Markdown,
    Pdf,
    Web,
    Transcript,
    Memory,
}

impl SourceType {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceType::Markdown => "markdown",
            SourceType::Pdf => "pdf",
            SourceType::Web => "web",
            SourceType::Transcript => "transcript",
            SourceType::Memory => "memory",
        }
    }
}

/// A loaded source, always normalized to Markdown in `text`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: String,
    pub uri: String,
    pub title: String,
    pub source_type: SourceType,
    /// Markdown body. Every loader produces Markdown; there is one chunker.
    pub text: String,
    pub content_hash: String,
    /// Outbound links (wikilinks, citations) as raw targets; resolved at index time.
    pub links: Vec<String>,
    pub tags: Vec<String>,
    /// Free-form loader metadata, serialized to JSON for storage.
    pub meta: serde_json::Value,
}

impl Document {
    pub fn new(source_type: SourceType, uri: &str, title: &str, text: &str) -> Self {
        Self {
            id: Self::new_id(uri),
            uri: uri.to_string(),
            title: title.to_string(),
            source_type,
            text: text.to_string(),
            content_hash: Self::content_hash(text),
            links: Vec::new(),
            tags: Vec::new(),
            meta: serde_json::Value::Null,
        }
    }

    /// Stable across edits so a changed file updates in place rather than duplicating.
    pub fn new_id(uri: &str) -> String {
        let mut h = Sha256::new();
        h.update(uri.as_bytes());
        hex::encode(&h.finalize()[..16])
    }

    pub fn content_hash(text: &str) -> String {
        let mut h = Sha256::new();
        h.update(text.as_bytes());
        hex::encode(h.finalize())
    }
}

/// A chunk of a Document. `text` is shown to the user; `embed_text` is what gets
/// embedded and carries the heading path prefix (and optionally an enrichment blurb).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub id: String,
    pub doc_id: String,
    pub ord: i64,
    pub text: String,
    pub embed_text: String,
    pub heading_path: String,
    pub page_no: Option<i64>,
}

impl Chunk {
    pub fn id(doc_id: &str, ord: i64) -> String {
        format!("{doc_id}:{ord}")
    }

    /// The plain (non-enriched) form of `embed_text`: the document title,
    /// optionally joined with the heading path, then the chunk's own text.
    ///
    /// This is exactly the formula `Chunker::chunk` uses before
    /// `Enricher::enrich` optionally prepends a contextual blurb on top —
    /// and it is also the ONLY form `Store::chunks_without_vectors` can
    /// reconstruct later for phase 2's backfill, because `embed_text` itself
    /// is not stored on the `Chunk` row as of schema version 3 (only its
    /// hash is — see `schema.rs`). A chunk published by `br8n index
    /// --no-embed` never runs `Enricher::enrich` in the first place (see
    /// `Indexer::index_documents_with`), so for that chunk this
    /// reconstruction is exact, not an approximation; it is only an
    /// approximation for a chunk whose vector predates schema version 3's
    /// column removal under `cfg.embed.contextual = true`, a combination
    /// this codebase does not otherwise produce.
    pub fn plain_embed_text(title: &str, heading_path: &str, text: &str) -> String {
        if heading_path.is_empty() {
            format!("{title}\n\n{text}")
        } else {
            format!("{title} > {heading_path}\n\n{text}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_stable_and_content_sensitive() {
        let a = Document::new(SourceType::Markdown, "file:///n/a.md", "A", "hello world");
        let b = Document::new(SourceType::Markdown, "file:///n/a.md", "A", "hello world");
        let c = Document::new(SourceType::Markdown, "file:///n/a.md", "A", "hello WORLD");

        assert_eq!(a.content_hash, b.content_hash);
        assert_ne!(a.content_hash, c.content_hash);
    }

    #[test]
    fn document_id_derives_from_uri_not_content() {
        let a = Document::new(SourceType::Markdown, "file:///n/a.md", "A", "one");
        let b = Document::new(SourceType::Markdown, "file:///n/a.md", "A", "two");
        assert_eq!(
            a.id, b.id,
            "same uri must yield same id so edits update in place"
        );
    }

    #[test]
    fn chunk_ids_are_unique_per_ordinal() {
        assert_ne!(Chunk::id("doc1", 0), Chunk::id("doc1", 1));
        assert_eq!(Chunk::id("doc1", 0), Chunk::id("doc1", 0));
    }
}
