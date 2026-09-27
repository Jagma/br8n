use br8n::mcp::Br8nTools;

#[test]
fn search_tool_formats_hits_with_citable_sources() {
    let out = Br8nTools::format_hits(&[br8n::store::Hit {
        chunk_id: "c1".into(),
        doc_id: "d1".into(),
        text: "PgBouncer runs in transaction mode.".into(),
        heading_path: "Pooling > Modes".into(),
        uri: "file:///notes/pool.md".into(),
        title: "Pooling".into(),
        page_no: Some(4),
        score: 0.83,
        relevance: 0.83,
        source_type: "markdown".into(),
        inbound: 0,
        lifecycle: Default::default(),
        last_used: None,
        memory: None,
    }]);
    assert!(out.contains("file:///notes/pool.md"));
    assert!(out.contains("Pooling > Modes"));
    assert!(out.contains("p. 4"));
    assert!(out.contains("PgBouncer"));
}

#[test]
fn empty_results_produce_an_explicit_message_not_a_blank_string() {
    let out = Br8nTools::format_hits(&[]);
    assert!(!out.trim().is_empty());
    assert!(out.to_lowercase().contains("no "));
}

#[test]
fn quality_argument_is_clamped_to_the_valid_tier_range() {
    assert_eq!(Br8nTools::clamp_quality(Some(99)), 4);
    assert_eq!(Br8nTools::clamp_quality(Some(2)), 2);
    assert_eq!(
        Br8nTools::clamp_quality(None),
        3,
        "defaults to the mcp surface tier"
    );
}

#[test]
fn remember_arguments_resolve_scope_and_confidence() {
    use br8n::mcp::{resolve_remember, RememberArgs};
    let cwd = std::path::PathBuf::from("/Users/x/repo");
    let r = resolve_remember(
        RememberArgs {
            kind: "lesson".into(),
            text: "Never comment code.".into(),
            title: None,
            scope: None,
            confidence: None,
        },
        &cwd,
    )
    .unwrap();
    assert_eq!(r.project, Some(cwd.clone()));
    assert_eq!(r.confidence, 80);
    assert_eq!(r.origin, br8n::memory::Origin::Claude);
    let g = resolve_remember(
        RememberArgs {
            kind: "fact".into(),
            text: "The user is Sam.".into(),
            title: None,
            scope: Some("global".into()),
            confidence: Some(100),
        },
        &cwd,
    )
    .unwrap();
    assert_eq!(g.project, None);
    assert_eq!(g.confidence, 100);
    assert!(resolve_remember(
        RememberArgs {
            kind: "wish".into(),
            text: "x".into(),
            title: None,
            scope: None,
            confidence: None
        },
        &cwd
    )
    .is_err());
}

#[test]
fn the_tool_router_lists_the_memory_tools() {
    let names = br8n::mcp::Br8nTools::tools();
    assert!(names.contains(&"br8n_remember".to_string()), "{names:?}");
    assert!(names.contains(&"br8n_forget".to_string()), "{names:?}");
}

#[test]
fn a_pack_refusal_surfaces_its_own_message_instead_of_no_results() {
    let refused = br8n::retrieve::PackRefused(anyhow::anyhow!(
        "this index predates the retrieval pack and can no longer be searched \
         directly — run `br8n index --compact` to rebuild the pack from the rows \
         you already have (no re-embedding), or `br8n index --reindex` to rebuild \
         from source"
    ));
    let text = Br8nTools::format_search_failure(anyhow::Error::new(refused));
    assert!(
        text.contains("--compact"),
        "a packless index must surface its own refusal naming the repair, not a \
         silent \"no results\"; got: {text:?}"
    );
    assert!(
        !text.to_lowercase().contains("no matching notes"),
        "the refusal must not be swallowed into the generic empty-results message; got: {text:?}"
    );
}

#[test]
fn a_non_refusal_search_failure_still_answers_no_results() {
    let text = Br8nTools::format_search_failure(anyhow::anyhow!("transient store error"));
    assert!(
        text.to_lowercase().contains("no matching notes"),
        "an ordinary failure must still degrade to the explicit empty-results message; \
         got: {text:?}"
    );
}
