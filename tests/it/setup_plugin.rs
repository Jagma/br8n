use br8n::setup::plugin::{render, write};
use std::path::Path;

fn rendered(rel: &str) -> String {
    let files = render(
        "9.8.7",
        Path::new("/Users/x/Library/Application Support/br8n/bin/br8n"),
    );
    let (_, bytes) = files.iter().find(|(p, _)| p == rel).unwrap_or_else(|| {
        panic!(
            "{rel} is not in the rendered plugin: {:?}",
            files.iter().map(|(p, _)| p).collect::<Vec<_>>()
        )
    });
    String::from_utf8(bytes.clone()).unwrap()
}

#[test]
fn every_embedded_file_is_rendered_and_no_placeholder_survives() {
    let files = render("9.8.7", Path::new("/opt/br8n/bin/br8n"));
    let names: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    for want in [
        ".claude-plugin/plugin.json",
        ".claude-plugin/marketplace.json",
        "hooks/hooks.json",
        "commands/br8n-index.md",
        "commands/br8n-search.md",
        "commands/br8n-status.md",
        "commands/br8n-bench.md",
        "commands/br8n-golden.md",
        "commands/br8n-add.md",
        "commands/br8n-remember.md",
        "commands/br8n-memories.md",
        "skills/br8n-retrieval/SKILL.md",
        "skills/br8n-memory/SKILL.md",
    ] {
        assert!(names.contains(&want), "missing {want} in {names:?}");
    }
    for (p, bytes) in &files {
        let s = String::from_utf8_lossy(bytes);
        assert!(!s.contains("{{"), "{p} still carries a placeholder");
    }
}

#[test]
fn the_version_and_the_binary_path_are_substituted() {
    let plugin: serde_json::Value =
        serde_json::from_str(&rendered(".claude-plugin/plugin.json")).unwrap();
    assert_eq!(plugin["version"], "9.8.7");
    assert_eq!(
        plugin["mcpServers"]["br8n"]["command"],
        "/Users/x/Library/Application Support/br8n/bin/br8n"
    );
    let market: serde_json::Value =
        serde_json::from_str(&rendered(".claude-plugin/marketplace.json")).unwrap();
    assert_eq!(market["plugins"][0]["version"], "9.8.7");
}

#[test]
fn hook_commands_quote_the_binary_path_because_it_has_a_space() {
    let hooks: serde_json::Value = serde_json::from_str(&rendered("hooks/hooks.json")).unwrap();
    let prompt = hooks["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    let start = hooks["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert_eq!(
        prompt,
        "\"/Users/x/Library/Application Support/br8n/bin/br8n\" hook prompt"
    );
    assert_eq!(
        start,
        "\"/Users/x/Library/Application Support/br8n/bin/br8n\" hook session-start"
    );
    assert_eq!(hooks["hooks"]["SessionStart"][0]["hooks"][0]["timeout"], 10);
}

#[test]
fn write_lays_the_tree_out_and_overwrites_on_a_second_call() {
    let t = tempfile::tempdir().unwrap();
    write(t.path(), "1.0.0", Path::new("/a/br8n")).unwrap();
    let first = std::fs::read_to_string(t.path().join(".claude-plugin/plugin.json")).unwrap();
    assert!(first.contains("\"1.0.0\""));
    assert!(t.path().join("commands/br8n-status.md").is_file());
    assert!(t.path().join("skills/br8n-retrieval/SKILL.md").is_file());
    write(t.path(), "2.0.0", Path::new("/b/br8n")).unwrap();
    let second = std::fs::read_to_string(t.path().join(".claude-plugin/plugin.json")).unwrap();
    assert!(second.contains("\"2.0.0\"") && second.contains("/b/br8n"));
}
