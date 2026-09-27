fn read(p: &str) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(p).expect(p)).expect(p)
}

#[test]
fn plugin_manifest_declares_name_and_mcp_server_and_takes_its_version_from_cargo() {
    let m = read("plugin/.claude-plugin/plugin.json");
    assert_eq!(m["name"], "br8n");
    assert_eq!(m["version"], "{{VERSION}}");
    assert_eq!(m["mcpServers"]["br8n"]["command"], "{{BR8N_BIN}}");
    assert_eq!(m["mcpServers"]["br8n"]["args"][0], "mcp");
    let market = read("plugin/.claude-plugin/marketplace.json");
    assert_eq!(market["plugins"][0]["version"], "{{VERSION}}");
}

#[test]
fn hooks_reference_the_binary_placeholder_not_the_plugin_root() {
    let h = read("plugin/hooks/hooks.json");
    for event in ["UserPromptSubmit", "SessionStart"] {
        let cmd = h["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(cmd.starts_with("\"{{BR8N_BIN}}\" hook "), "{event}: {cmd}");
        assert!(!cmd.contains("CLAUDE_PLUGIN_ROOT"), "{event}: {cmd}");
    }
}

#[test]
fn every_command_file_has_frontmatter_with_a_description() {
    for name in [
        "br8n-index",
        "br8n-search",
        "br8n-status",
        "br8n-bench",
        "br8n-golden",
        "br8n-add",
        "br8n-remember",
        "br8n-memories",
    ] {
        let p = format!("plugin/commands/{name}.md");
        let s = std::fs::read_to_string(&p).expect(&p);
        assert!(s.starts_with("---"), "{p} needs frontmatter");
        assert!(s.contains("description:"), "{p} needs a description");
    }
}

#[test]
fn skill_has_a_trigger_oriented_description() {
    let s = std::fs::read_to_string("plugin/skills/br8n-retrieval/SKILL.md").unwrap();
    assert!(s.starts_with("---"));
    assert!(s.contains("name: br8n-retrieval"));
    assert!(s.to_lowercase().contains("use when"));
}

#[test]
fn the_memory_skill_names_both_tools_and_when_to_use_them() {
    let s = std::fs::read_to_string("plugin/skills/br8n-memory/SKILL.md").unwrap();
    assert!(s.starts_with("---"));
    assert!(s.contains("name: br8n-memory"));
    assert!(s.to_lowercase().contains("use when"));
    assert!(s.contains("br8n_remember") && s.contains("br8n_forget"));
    assert!(s.contains("<br8n-lessons>"));
}
