/// The ledger parser must count what the hook really injected — and must not
/// count the OTHER hooks' additional context, which shares the same record
/// type. The SessionStart superpowers block in the fixture is the trap.
#[test]
fn the_audit_counts_br8n_injections_and_splits_them_by_source() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/audit-session.jsonl"
        ),
        dir.path().join("session.jsonl"),
    )
    .unwrap();

    let a = br8n::audit::audit(dir.path()).unwrap();

    assert_eq!(
        a.injections, 2,
        "two br8n injections, not three — the SessionStart block is another hook's"
    );
    assert_eq!(a.entries, 3, "three excerpt entries across the two");
    assert_eq!(a.by_scheme.get("file"), Some(&1), "one vault note");
    assert_eq!(
        a.by_scheme.get("claude-session"),
        Some(&2),
        "two transcripts"
    );
    assert_eq!(
        a.injections_with_no_note, 1,
        "the second injection is transcript-only — this is the statistic the whole command exists for"
    );
}

/// `content` is an ARRAY of strings in the real records. A parser that expects
/// a bare string silently finds nothing and reports a clean zero — the house
/// failure mode. This pins the shape.
#[test]
fn a_content_array_is_parsed_not_skipped() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("s.jsonl"),
        "{\"attachment\":{\"type\":\"hook_additional_context\",\"hookEvent\":\"UserPromptSubmit\",\"content\":[\"<br8n-context>\\n[T](file:///a.md)\\nbody\\n\"]}}\n",
    )
    .unwrap();
    let a = br8n::audit::audit(dir.path()).unwrap();
    assert_eq!(a.injections, 1, "an array-valued content field must parse");
}

#[test]
fn a_nonexistent_root_is_an_error_not_an_empty_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("definitely-not-real");

    let err = br8n::audit::audit(&missing).unwrap_err();

    assert!(
        err.to_string().contains("no transcript directory"),
        "a mistyped root must fail loudly, not report an empty ledger: {err}"
    );
}

#[test]
fn a_marker_elsewhere_on_the_line_does_not_make_another_hooks_block_an_injection() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/audit-session.jsonl"
        ),
        dir.path().join("session.jsonl"),
    )
    .unwrap();

    let a = br8n::audit::audit(dir.path()).unwrap();

    assert_eq!(
        a.injections, 2,
        "the fourth record's own content is another hook's; the marker sits in \
         the user's prose on the same line, so it must not be counted: {:?}",
        a.injections
    );
}
