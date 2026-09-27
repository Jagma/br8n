use br8n::loaders::markdown::{extract_wikilinks, MarkdownLoader};
use br8n::model::SourceType;

#[test]
fn loads_every_markdown_file_under_root() {
    let l = MarkdownLoader::new("tests/fixtures/corpus/notes");
    let docs = l.load_all().unwrap();
    assert_eq!(docs.len(), 2);
    assert!(docs.iter().all(|d| d.source_type == SourceType::Markdown));
}

#[test]
fn frontmatter_supplies_title_and_tags_and_is_stripped_from_body() {
    let l = MarkdownLoader::new("tests/fixtures/corpus/notes");
    let docs = l.load_all().unwrap();
    let pg = docs
        .iter()
        .find(|d| d.uri.ends_with("postgres.md"))
        .unwrap();

    assert_eq!(pg.title, "Postgres migration");
    assert!(pg.tags.contains(&"db".to_string()));
    assert!(
        !pg.text.contains("---"),
        "frontmatter must not reach the chunker"
    );
    assert!(pg.text.contains("Rejected approaches"));
}

#[test]
fn title_falls_back_to_first_heading_then_filename() {
    let l = MarkdownLoader::new("tests/fixtures/corpus/notes");
    let docs = l.load_all().unwrap();
    let cp = docs
        .iter()
        .find(|d| d.uri.ends_with("connection-pooling.md"))
        .unwrap();
    assert_eq!(cp.title, "Connection pooling");
}

#[test]
fn wikilinks_become_document_links() {
    let l = MarkdownLoader::new("tests/fixtures/corpus/notes");
    let docs = l.load_all().unwrap();
    let pg = docs
        .iter()
        .find(|d| d.uri.ends_with("postgres.md"))
        .unwrap();
    assert_eq!(pg.links, vec!["connection-pooling".to_string()]);
}

#[test]
fn wikilink_extraction_handles_aliases_and_anchors() {
    assert_eq!(extract_wikilinks("see [[note|alias]]"), vec!["note"]);
    assert_eq!(extract_wikilinks("see [[note#section]]"), vec!["note"]);
    assert_eq!(extract_wikilinks("see [[a]] and [[b]]"), vec!["a", "b"]);
    assert_eq!(extract_wikilinks("no links here"), Vec::<String>::new());
}

#[test]
fn code_is_not_scanned_for_wikilinks() {
    // C++ attribute syntax. Treating this as a link puts a garbage edge in the
    // graph, which then drags unrelated notes into retrieval.
    assert_eq!(
        extract_wikilinks("```cpp\n[[nodiscard]] int f();\n```"),
        Vec::<String>::new()
    );
    assert_eq!(
        extract_wikilinks("inline `[[nodiscard]]` code"),
        Vec::<String>::new()
    );
}

#[test]
fn malformed_wikilinks_yield_nothing_rather_than_garbage() {
    // Unanchored patterns capture across paragraph breaks.
    assert_eq!(
        extract_wikilinks("unclosed [[note\n\nnext paragraph with ] bracket"),
        Vec::<String>::new()
    );
}

#[test]
fn wikilinks_do_not_span_a_paragraph_boundary() {
    // `[[foo` ends one paragraph and `bar]]` starts the next. Concatenating
    // the two blocks' text with no separator would join them into a phantom
    // `[[foobar]]` link that neither paragraph actually contains.
    assert_eq!(
        extract_wikilinks("this ends with [[foo\n\nbar]] starts the next paragraph"),
        Vec::<String>::new()
    );
}

#[test]
fn wikilinks_inside_blockquotes_and_list_items_are_still_extracted() {
    // Only code is exempt from wikilink scanning (see `code_is_not_scanned_for_wikilinks`);
    // this locks in that the block-boundary separator fix didn't over-correct
    // and start exempting blockquotes or list items too.
    assert_eq!(extract_wikilinks("> see [[note]]"), vec!["note"]);
    assert_eq!(extract_wikilinks("- see [[note]]"), vec!["note"]);
}

#[test]
fn frontmatter_never_reaches_document_text_even_when_yaml_is_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.md");
    std::fs::write(
        &path,
        "---\ntitle: [unclosed\n  : : nonsense\n---\n\n# Body\n\ntext",
    )
    .unwrap();

    let doc = MarkdownLoader::load_file(&path).unwrap();
    assert!(
        !doc.text.contains("---"),
        "frontmatter delimiters leaked into text"
    );
    assert!(
        !doc.text.contains("nonsense"),
        "frontmatter body leaked into text"
    );
    assert!(doc.text.contains("text"));
}

#[test]
fn an_empty_heading_falls_through_to_the_filename() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("my-note.md");
    std::fs::write(&path, "##\n\nbody text").unwrap();
    assert_eq!(MarkdownLoader::load_file(&path).unwrap().title, "my-note");
}

/// Does the vault's decision
/// TEMPLATE survive the YAML parser, or has it been silently falling into the
/// malformed-frontmatter fallback and losing its `tags:` all along?
///
/// The fixture is the literal bytes of `_templates/decision.md`, verified byte
/// for byte against the live vault when written. Three hazards in one block:
///
///   1. `status:` carries a trailing `#` comment. A real YAML parse yields
///      `proposed`; a line-based parse yields the comment too.
///   2. `date: {{date:YYYY-MM-DD}}` starts with `{`, so YAML reads it as a FLOW
///      MAPPING whose key is itself a flow mapping — not a string. Whether
///      yaml-rust2 accepts that at all, rather than raising a `ScanError` that
///      would drop the WHOLE block into the fallback, was the open question.
///   3. Six keys have empty values, which are YAML null.
///
/// What is asserted is `tags`, not `date`: `Frontmatter` does not model `date`,
/// so the flow mapping's own value is irrelevant. What matters is whether its
/// presence poisons the parse for the fields that ARE modelled. If it does,
/// every ADR written from this template loses its tags with nothing reporting it.
#[test]
fn the_vault_decision_template_still_parses_and_keeps_its_tags() {
    let l = MarkdownLoader::new("tests/fixtures/corpus/templates");
    let docs = l.load_all().unwrap();
    let d = docs
        .iter()
        .find(|d| d.uri.ends_with("decision.md"))
        .unwrap();

    // The load-bearing assertion. `tags` is only populated on the SUCCESS path;
    // the fallback returns `Frontmatter::default()`, whose `tags` is empty. So
    // this failing means the whole block was rejected, not that one key was.
    assert_eq!(
        d.tags,
        vec!["decision".to_string()],
        "the template's frontmatter did not parse — `date: {{{{date:YYYY-MM-DD}}}}` \
         is a flow mapping and something in the block is being rejected, so every \
         note written from this template silently loses its tags"
    );

    // Frontmatter must not reach the chunker even when it contains oddities.
    assert!(
        !d.text.contains("superseded-by:"),
        "frontmatter leaked into the body"
    );
    // `title:` is empty, so the loader must fall through to the first heading
    // rather than yielding an empty title.
    assert!(
        !d.title.is_empty(),
        "an empty `title:` must fall back, not produce an empty title"
    );

    // The template's `status:` carries a trailing YAML comment, and its value
    // must arrive clean. This is the §3.3 hazard: a line-based parse yields
    // "proposed        # proposed | accepted | ...".
    assert_eq!(d.meta["status"], "proposed");
    assert_eq!(d.meta["id"], "ADR-000");
}

use br8n::loaders::markdown::is_ignored;
use std::path::Path;

/// Decision 5. Matched against EVERY path component, not just the parent, so a
/// nested `notes/_templates/x.md` is excluded too. An empty list excludes
/// nothing: the feature is opt-out.
#[test]
fn the_ignore_predicate_matches_any_path_component() {
    let ig = vec!["_templates".to_string()];

    assert!(is_ignored(Path::new("/v/_templates/decision.md"), &ig));
    assert!(is_ignored(Path::new("/v/notes/_templates/deep/x.md"), &ig));

    assert!(!is_ignored(Path::new("/v/notes/real.md"), &ig));
    // A partial match is not a match: `_templates2` is a different directory.
    assert!(!is_ignored(Path::new("/v/_templates2/x.md"), &ig));
    // Case matters. `Templates` is not defaulted precisely because a vault may
    // use it for real notes; silently case-folding would take that choice away.
    assert!(!is_ignored(Path::new("/v/_Templates/x.md"), &ig));

    assert!(!is_ignored(Path::new("/v/_templates/x.md"), &[]));
}

/// `is_ignored` is exercised directly above, but `MarkdownLoader::with_ignore`
/// — the constructor that actually threads an ignore list into `load_all`'s
/// walk — was called by nothing, not even a test. Deleting it, or deleting the
/// `is_ignored` check inside `load_all`, failed no test. This exercises both:
/// a loader built WITH a non-empty ignore list must skip the matching file,
/// and the plain `new()` walk (already covered elsewhere) stays a control.
#[test]
fn with_ignore_excludes_matching_files_from_load_all() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("_templates")).unwrap();
    std::fs::write(root.join("real.md"), "# Real\n\nBody.\n").unwrap();
    std::fs::write(root.join("_templates/tpl.md"), "# Template\n\nBody.\n").unwrap();

    let ignored = MarkdownLoader::with_ignore(root, &["_templates".to_string()]);
    let docs = ignored.load_all().unwrap();
    assert_eq!(docs.len(), 1, "the template must not be loaded");
    assert!(docs[0].uri.ends_with("real.md"));

    // An empty ignore list must exclude nothing — opt-out, not opt-in.
    let unfiltered = MarkdownLoader::with_ignore(root, &[]);
    assert_eq!(unfiltered.load_all().unwrap().len(), 2);
}

use br8n::loaders::markdown::MarkdownLoader as ML;

/// Decision 4: lifecycle frontmatter rides in `Document.meta`, the carrier that
/// already turns `domain` into a Source node and `project` into an Entity.
#[test]
fn lifecycle_frontmatter_lands_in_document_meta() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("adr.md");
    std::fs::write(
        &p,
        "---\n\
         id: ADR-0004\n\
         title: Bazel and NixOS\n\
         status: superseded        # proposed | accepted | superseded\n\
         supersedes: \n\
         superseded-by: ADR-0005\n\
         tags: [decision]\n\
         ---\n\n# Bazel and NixOS\n\nBody.\n",
    )
    .unwrap();

    let doc = ML::load_file(&p).unwrap();

    assert_eq!(doc.meta["id"], "ADR-0004");
    // The trailing YAML comment must NOT survive: a line-based parse would
    // yield "superseded        # proposed | accepted | superseded".
    assert_eq!(doc.meta["status"], "superseded");
    assert_eq!(doc.meta["superseded_by"], "ADR-0005");
    // `supersedes:` is empty — YAML null. An absent key, not an empty string,
    // so Task 3 can branch on presence alone.
    assert!(
        doc.meta.get("supersedes").is_none(),
        "an empty frontmatter value must not become an empty-string key"
    );
    // Existing behaviour must not regress.
    assert_eq!(doc.title, "Bazel and NixOS");
    assert_eq!(doc.tags, vec!["decision".to_string()]);
}

/// A note with no lifecycle keys keeps `meta` NULL, so nothing downstream has
/// to distinguish "absent" from "empty object", and `pack.rec` bytes are
/// unchanged for the overwhelming majority of documents.
#[test]
fn a_note_without_lifecycle_frontmatter_keeps_meta_null() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("plain.md");
    std::fs::write(&p, "---\ntitle: Plain\ntags: [x]\n---\n\nBody.\n").unwrap();

    let doc = ML::load_file(&p).unwrap();
    assert!(
        doc.meta.is_null(),
        "meta must stay null when no lifecycle key is present, got {:?}",
        doc.meta
    );
}

/// A QUOTED empty value must not become an empty-string key either.
///
/// This is a different code path from the unquoted `supersedes: ` the vault's
/// own template uses, and the distinction is why this test exists rather than
/// being folded into `lifecycle_frontmatter_lands_in_document_meta`:
///
///   `supersedes: `    -> YAML null  -> `Option<String>` is None
///   `supersedes: ""`  -> empty str  -> `Some("")`, which reaches the filter
///
/// `Option::filter`'s predicate never fires on `None`, so the hand-authored
/// template exercises the `is_empty()` guard not at all — every test passed
/// with that guard deleted until this one existed.
///
/// It is not a hypothetical path. Obsidian's Properties panel serialises a
/// CLEARED text property through js-yaml, which emits a quoted `''`, not a
/// bare key. A vault edited through the UI rather than by hand produces
/// exactly this, and without the guard those keys would arrive in `meta` as
/// empty strings — which Task 3 would then try to resolve as a link target.
#[test]
fn a_quoted_empty_lifecycle_value_is_dropped_like_a_null_one() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("obsidian-edited.md");
    std::fs::write(
        &p,
        "---\n\
         id: ADR-0009\n\
         title: Edited In Obsidian\n\
         supersedes: \"\"\n\
         superseded-by: \"   \"\n\
         ---\n\n# Edited In Obsidian\n\nBody.\n",
    )
    .unwrap();

    let doc = ML::load_file(&p).unwrap();

    assert_eq!(doc.meta["id"], "ADR-0009", "a real value still lands");
    assert!(
        doc.meta.get("supersedes").is_none(),
        "a quoted empty value must be dropped, not stored as \"\"; got {:?}",
        doc.meta.get("supersedes")
    );
    assert!(
        doc.meta.get("superseded_by").is_none(),
        "whitespace trims to empty and must also be dropped; got {:?}",
        doc.meta.get("superseded_by")
    );
}
