fn read(p: &str) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(p).expect(p)).expect(p)
}

#[test]
fn plugin_manifest_declares_name_and_mcp_server_and_carries_the_cargo_version() {
    let m = read("plugin/.claude-plugin/plugin.json");
    assert_eq!(m["name"], "br8n");
    assert_eq!(
        m["version"],
        env!("CARGO_PKG_VERSION"),
        "bump plugin.json with Cargo.toml: a marketplace install reads this version"
    );
    assert_eq!(
        m["mcpServers"]["br8n"]["command"],
        "${CLAUDE_PLUGIN_ROOT}/scripts/br8n.sh"
    );
    assert_eq!(m["mcpServers"]["br8n"]["args"][0], "mcp");
    let market = read("plugin/.claude-plugin/marketplace.json");
    assert_eq!(market["plugins"][0]["source"], "./");
    assert!(
        market["plugins"][0].get("version").is_none(),
        "plugin.json is the one place the plugin's version lives"
    );
}

#[test]
fn hooks_run_the_launcher_from_the_plugin_root() {
    let h = read("plugin/hooks/hooks.json");
    for (event, which) in [
        ("UserPromptSubmit", "prompt"),
        ("SessionStart", "session-start"),
    ] {
        let cmd = h["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(
            cmd,
            format!("\"${{CLAUDE_PLUGIN_ROOT}}/scripts/br8n.sh\" hook {which}"),
            "{event}"
        );
    }
}

#[test]
fn the_launcher_is_an_executable_posix_shell_script() {
    use std::os::unix::fs::PermissionsExt;
    let p = "plugin/scripts/br8n.sh";
    let mode = std::fs::metadata(p).unwrap().permissions().mode();
    assert_eq!(mode & 0o111, 0o111, "{p} must be executable: {mode:o}");
    assert!(std::fs::read_to_string(p)
        .unwrap()
        .starts_with("#!/bin/sh\n"));
}

#[test]
fn the_repository_root_is_a_marketplace_that_lists_the_plugin() {
    let market = read(".claude-plugin/marketplace.json");
    assert_eq!(market["name"], "br8n");
    assert_eq!(market["plugins"][0]["name"], "br8n");
    assert_eq!(market["plugins"][0]["source"], "./plugin");
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
