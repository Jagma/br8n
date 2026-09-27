use br8n::loaders::transcript::TranscriptLoader;
use br8n::model::SourceType;

#[test]
fn one_document_per_session_with_project_metadata() {
    let l = TranscriptLoader::new("tests/fixtures/corpus/projects");
    let docs = l.load_all().unwrap();
    assert_eq!(docs.len(), 1);
    let d = &docs[0];
    assert_eq!(d.source_type, SourceType::Transcript);
    assert_eq!(d.meta["project"], "/Users/x/myapp");
}

#[test]
fn turns_render_as_markdown_with_role_headings() {
    let l = TranscriptLoader::new("tests/fixtures/corpus/projects");
    let d = &l.load_all().unwrap()[0];
    assert!(d.text.contains("## user"));
    assert!(d.text.contains("## assistant"));
    assert!(d.text.contains("why is the pooler dropping connections?"));
    assert!(d.text.contains("PgBouncer in transaction mode"));
}

#[test]
fn assistant_content_blocks_are_flattened_to_text() {
    let l = TranscriptLoader::new("tests/fixtures/corpus/projects");
    let d = &l.load_all().unwrap()[0];
    assert!(
        !d.text.contains("\"type\":\"text\""),
        "raw json must not leak into the index"
    );
}

#[test]
fn urls_and_bare_tokens_do_not_become_phantom_file_mentions() {
    // These become MENTIONS graph edges. A URL fragment or a stray "1.rs" in
    // prose creates an Entity node for a file that does not exist, and graph
    // expansion then pulls unrelated material into results.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    let text =
        "See src/db/pool.rs and 1.rs, version 1.2.3, https://example.com/index.js, Cargo.lock";
    std::fs::write(
        &path,
        format!(r#"{{"type":"user","message":{{"role":"user","content":"{text}"}},"cwd":"/x"}}"#),
    )
    .unwrap();

    let doc = TranscriptLoader::load_session(&path).unwrap();
    let files: Vec<String> = doc.meta["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();

    assert!(files.contains(&"src/db/pool.rs".to_string()));
    assert!(
        files.contains(&"Cargo.lock".to_string()),
        "real files must still match"
    );
    assert!(
        !files.iter().any(|f| f.contains("example.com")),
        "url leaked in as a file"
    );
    assert!(
        !files.contains(&"1.rs".to_string()),
        "bare numeric stem is not a file reference"
    );
    assert!(
        !files.iter().any(|f| f.starts_with("1.2")),
        "version string is not a file"
    );
}

#[test]
fn scheme_less_hosts_are_not_mistaken_for_source_files() {
    // `docs.rs` appears constantly in Rust transcripts and looks exactly like a
    // source filename. A bare `example.com/index.js` has the same shape as a path.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    let text = "see example.com/index.js and docs.rs/serde/latest and www.foo.org/bar.py \
                but keep src/main.rs and main.rs and README.md";
    std::fs::write(
        &path,
        format!(r#"{{"type":"user","message":{{"role":"user","content":"{text}"}},"cwd":"/x"}}"#),
    )
    .unwrap();

    let doc = TranscriptLoader::load_session(&path).unwrap();
    let files: Vec<String> = doc.meta["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();

    assert!(!files.iter().any(|f| f.contains("example.com")));
    assert!(
        !files.contains(&"docs.rs".to_string()),
        "docs.rs is a website, not a source file"
    );
    assert!(!files.iter().any(|f| f.contains("foo.org")));
    // Real references, including a bare filename, must survive.
    assert!(files.contains(&"src/main.rs".to_string()));
    assert!(files.contains(&"main.rs".to_string()));
    assert!(files.contains(&"README.md".to_string()));
}

#[test]
fn file_paths_mentioned_are_captured_for_graph_edges() {
    let l = TranscriptLoader::new("tests/fixtures/corpus/projects");
    let d = &l.load_all().unwrap()[0];
    let files: Vec<String> = d.meta["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(files.contains(&"src/db/pool.rs".to_string()));
}
