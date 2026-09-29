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

fn handshake(dir: &std::path::Path) -> std::collections::HashMap<u64, serde_json::Value> {
    use std::io::{BufRead, Write};
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_br8n"))
        .arg("mcp")
        .env("BR8N_DB", dir.join("db"))
        .env("BR8N_CONFIG", dir.join("config.toml"))
        .env("PATH", "/usr/bin:/bin")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for request in [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    ] {
        writeln!(stdin, "{request}").unwrap();
    }
    let stdout = child.stdout.take().unwrap();
    let (lines, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let mut answers = std::collections::HashMap::new();
    while !answers.contains_key(&2) {
        let Ok(line) = received.recv_timeout(std::time::Duration::from_secs(30)) else {
            break;
        };
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        if let Some(id) = v["id"].as_u64() {
            answers.insert(id, v);
        }
    }
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    answers
}

#[test]
fn the_server_names_itself_and_annotates_every_tool() {
    let t = tempfile::tempdir().unwrap();
    let answers = handshake(t.path());
    let server = &answers[&1]["result"]["serverInfo"];
    assert_eq!(server["name"], "br8n", "{server}");
    assert_eq!(server["version"], env!("CARGO_PKG_VERSION"), "{server}");

    let tools = answers[&2]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 5, "{tools:?}");
    for tool in tools {
        let a = &tool["annotations"];
        assert!(a["title"].as_str().is_some_and(|t| !t.is_empty()), "{tool}");
        assert_eq!(tool["title"], a["title"], "{tool}");
        assert!(
            a["readOnlyHint"].is_boolean() && a["destructiveHint"].is_boolean(),
            "{tool}"
        );
        let name = tool["name"].as_str().unwrap();
        let reads_only = matches!(name, "br8n_search" | "br8n_related");
        assert_eq!(a["readOnlyHint"], reads_only, "{name}");
        assert_eq!(a["destructiveHint"], name == "br8n_forget", "{name}");
    }
}
