use br8n::hook::PromptAgent;
use br8n::setup::agents::files::{backup_path, Unparseable};
use br8n::setup::agents::{
    self, api, claude_desktop::ClaudeDesktop, codex, codex::Codex, cursor::Cursor, gemini::Gemini,
    Agent, AgentEnv, AgentError, ConnectOptions, Status,
};
use br8n::setup::Paths;
use std::path::{Path, PathBuf};

struct Fx {
    _t: tempfile::TempDir,
    home: PathBuf,
    env: AgentEnv,
}

fn fixture() -> Fx {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let paths = Paths::at(
        &home.join(".local/share/br8n"),
        vec![],
        home.join(".claude/plugins/cache/br8n"),
    );
    std::fs::create_dir_all(paths.bin.parent().unwrap()).unwrap();
    std::fs::write(&paths.bin, "#!/bin/sh\n").unwrap();
    let env = AgentEnv::at(&home, paths, vec![]);
    Fx { _t: t, home, env }
}

fn bin(fx: &Fx) -> String {
    fx.env.bin_str()
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

fn json(p: &Path) -> serde_json::Value {
    serde_json::from_str(&read(p)).unwrap()
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn none() -> ConnectOptions {
    ConnectOptions::default()
}

#[test]
fn connecting_cursor_creates_the_entry_and_connecting_again_changes_nothing() {
    let fx = fixture();
    let file = Cursor::config_file(&fx.env);
    assert_eq!(Cursor.status(&fx.env), Status::NotConnected);

    let change = Cursor.connect(&fx.env, &none()).unwrap();
    assert_eq!(change.files, vec![file.clone()]);
    assert!(change.backups.is_empty(), "nothing existed to back up");
    let v = json(&file);
    assert_eq!(v["mcpServers"]["br8n"]["command"], bin(&fx));
    assert_eq!(v["mcpServers"]["br8n"]["args"], serde_json::json!(["mcp"]));
    assert_eq!(v["mcpServers"]["br8n"]["type"], "stdio");
    assert_eq!(Cursor.status(&fx.env), Status::Connected);

    let before = read(&file);
    let again = Cursor.connect(&fx.env, &none()).unwrap();
    assert!(again.is_empty(), "{again:?}");
    assert_eq!(read(&file), before);
    assert!(!backup_path(&file).exists());
}

const DESKTOP: &str = r#"{
    "globalShortcut": "Ctrl+Space",
    "mcpServers": {
        "other": {
            "command": "/usr/bin/other",
            "args": ["serve"],
            "env": { "Z": "1", "A": "2" }
        }
    },
    "zeta": 1
}
"#;

#[test]
fn a_json_connect_keeps_every_other_key_in_order_and_backs_up_the_original_once() {
    let fx = fixture();
    let file = ClaudeDesktop::config_file(&fx.env);
    write(&file, DESKTOP);

    let change = ClaudeDesktop.connect(&fx.env, &none()).unwrap();
    let backup = backup_path(&file);
    assert_eq!(change.backups, vec![backup.clone()]);
    assert_eq!(read(&backup), DESKTOP);

    let text = read(&file);
    let order = [
        "globalShortcut",
        "mcpServers",
        "other",
        "\"Z\"",
        "\"A\"",
        "br8n",
        "zeta",
    ];
    let at: Vec<usize> = order.iter().map(|k| text.find(k).unwrap()).collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "order lost:\n{text}");
    assert!(
        text.contains("\n    \"globalShortcut\""),
        "indent lost:\n{text}"
    );

    let gone = ClaudeDesktop.disconnect(&fx.env).unwrap();
    assert_eq!(gone.files, vec![file.clone()]);
    assert!(
        gone.backups.is_empty(),
        "the first backup is never replaced"
    );
    let v = json(&file);
    assert!(v["mcpServers"].get("br8n").is_none());
    assert_eq!(v["mcpServers"]["other"]["command"], "/usr/bin/other");
    assert_eq!(v["globalShortcut"], "Ctrl+Space");
    assert_eq!(v["zeta"], 1);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(DESKTOP).unwrap(),
        v,
        "disconnect restores the original content"
    );

    ClaudeDesktop.connect(&fx.env, &none()).unwrap();
    assert_eq!(
        read(&backup),
        DESKTOP,
        "a later write never clobbers the backup"
    );
}

#[test]
fn a_file_that_does_not_parse_is_refused_and_left_byte_identical() {
    let fx = fixture();
    let file = Cursor::config_file(&fx.env);
    let garbage = "{ \"mcpServers\": { // a comment\n";
    write(&file, garbage);

    let err = Cursor.connect(&fx.env, &none()).unwrap_err();
    assert!(err.downcast_ref::<Unparseable>().is_some(), "{err:#}");
    assert!(format!("{err:#}").contains(&file.display().to_string()));
    assert_eq!(read(&file), garbage);
    assert!(!backup_path(&file).exists());
    assert!(matches!(Cursor.status(&fx.env), Status::Broken(_)));

    Cursor.disconnect(&fx.env).unwrap_err();
    assert_eq!(read(&file), garbage);

    let (code, v) = api::connect(&fx.env, br#"{"id":"cursor"}"#);
    assert_eq!(code, 409, "{v}");
    assert_eq!(read(&file), garbage);
}

#[test]
fn a_non_object_top_level_is_refused() {
    let fx = fixture();
    let file = Gemini::config_file(&fx.env);
    write(&file, "[1, 2]\n");
    assert!(Gemini.connect(&fx.env, &none()).is_err());
    assert_eq!(read(&file), "[1, 2]\n");
}

#[test]
fn an_entry_pointing_at_another_binary_or_other_args_is_stale_and_connect_repairs_it() {
    let fx = fixture();
    let file = Cursor::config_file(&fx.env);
    write(
        &file,
        r#"{"mcpServers":{"br8n":{"command":"/old/br8n","args":["mcp"],"env":{"K":"v"}}}}"#,
    );
    match Cursor.status(&fx.env) {
        Status::Stale(r) => assert!(r.contains("/old/br8n"), "{r}"),
        s => panic!("expected stale, got {s:?}"),
    }

    write(
        &file,
        &format!(
            r#"{{"mcpServers":{{"br8n":{{"command":"{}","args":["serve"],"env":{{"K":"v"}}}}}}}}"#,
            bin(&fx)
        ),
    );
    match Cursor.status(&fx.env) {
        Status::Stale(r) => assert!(r.contains("args"), "{r}"),
        s => panic!("expected stale, got {s:?}"),
    }

    let change = Cursor.connect(&fx.env, &none()).unwrap();
    assert_eq!(change.files, vec![file.clone()]);
    assert_eq!(Cursor.status(&fx.env), Status::Connected);
    assert_eq!(json(&file)["mcpServers"]["br8n"]["env"]["K"], "v");
}

#[test]
fn a_matching_entry_whose_binary_is_missing_is_broken() {
    let fx = fixture();
    Cursor.connect(&fx.env, &none()).unwrap();
    std::fs::remove_file(fx.env.bin()).unwrap();
    match Cursor.status(&fx.env) {
        Status::Broken(r) => assert!(r.contains("br8n install"), "{r}"),
        s => panic!("expected broken, got {s:?}"),
    }
}

const CODEX_TOML: &str = r#"# my codex settings
model = "o3" # the good one

[mcp_servers.other]
# keep this server
command = "other"
args = ["x"]

[profiles.fast]
model = "mini"
"#;

#[test]
fn codex_keeps_comments_adds_the_server_and_hook_and_disconnect_restores_the_file() {
    let fx = fixture();
    let config = Codex::config_file(&fx.env);
    write(&config, CODEX_TOML);

    let change = Codex.connect(&fx.env, &none()).unwrap();
    assert!(change.files.contains(&config));
    assert!(change.files.contains(&Codex::hooks_file(&fx.env)));
    assert_eq!(change.backups, vec![backup_path(&config)]);
    let text = read(&config);
    for line in CODEX_TOML.lines() {
        assert!(text.contains(line), "lost `{line}`:\n{text}");
    }
    assert!(text.contains("[mcp_servers.br8n]"), "{text}");
    let parsed: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(
        parsed["mcp_servers"]["br8n"]["command"].as_str(),
        Some(bin(&fx).as_str())
    );
    assert_eq!(
        parsed["mcp_servers"]["br8n"]["args"][0].as_str(),
        Some("mcp")
    );
    assert_eq!(
        parsed["mcp_servers"]["other"]["command"].as_str(),
        Some("other")
    );

    let hooks = json(&Codex::hooks_file(&fx.env));
    let cmd = hooks["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(cmd, format!("\"{}\" hook prompt --agent codex", bin(&fx)));
    assert_eq!(
        hooks["hooks"]["UserPromptSubmit"][0]["hooks"][0]["timeout"],
        5
    );
    assert_eq!(Codex.status(&fx.env), Status::Connected);
    assert!(
        !Codex::instructions_file(&fx.env).exists(),
        "instructions are opt-in"
    );

    assert!(Codex.connect(&fx.env, &none()).unwrap().is_empty());

    Codex.disconnect(&fx.env).unwrap();
    assert_eq!(read(&config), CODEX_TOML);
    assert_eq!(Codex.status(&fx.env), Status::NotConnected);
}

#[test]
fn codex_reports_a_missing_hook_or_another_binary_as_stale() {
    let fx = fixture();
    write(
        &Codex::config_file(&fx.env),
        &format!(
            "[mcp_servers.br8n]\ncommand = \"{}\"\nargs = [\"mcp\"]\n",
            bin(&fx)
        ),
    );
    match Codex.status(&fx.env) {
        Status::Stale(r) => assert!(r.contains("prompt hook is missing"), "{r}"),
        s => panic!("expected stale, got {s:?}"),
    }
    Codex.connect(&fx.env, &none()).unwrap();
    write(
        &Codex::config_file(&fx.env),
        "[mcp_servers.br8n]\ncommand = \"/elsewhere/br8n\"\nargs = [\"mcp\"]\n",
    );
    match Codex.status(&fx.env) {
        Status::Stale(r) => assert!(r.contains("/elsewhere/br8n"), "{r}"),
        s => panic!("expected stale, got {s:?}"),
    }

    Codex.connect(&fx.env, &none()).unwrap();
    assert_eq!(Codex.status(&fx.env), Status::Connected);
    write(
        &Codex::hooks_file(&fx.env),
        r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"\"/elsewhere/br8n\" hook prompt --agent codex"}]}]}}"#,
    );
    match Codex.status(&fx.env) {
        Status::Stale(r) => assert!(r.contains("UserPromptSubmit hook runs"), "{r}"),
        s => panic!("expected stale, got {s:?}"),
    }
    Codex.connect(&fx.env, &none()).unwrap();
    let hooks = json(&Codex::hooks_file(&fx.env));
    let groups = hooks["hooks"]["UserPromptSubmit"].as_array().unwrap();
    assert_eq!(
        groups.len(),
        1,
        "the stale hook is repointed, not duplicated"
    );
    assert_eq!(
        groups[0]["hooks"][0]["command"],
        format!("\"{}\" hook prompt --agent codex", bin(&fx))
    );
}

#[test]
fn codex_refuses_a_config_that_is_not_toml() {
    let fx = fixture();
    let config = Codex::config_file(&fx.env);
    write(&config, "model = \n[[[");
    let err = Codex.connect(&fx.env, &none()).unwrap_err();
    assert!(err.downcast_ref::<Unparseable>().is_some(), "{err:#}");
    assert_eq!(read(&config), "model = \n[[[");
    assert!(
        !Codex::hooks_file(&fx.env).exists(),
        "nothing is written when one file is refused"
    );
}

#[test]
fn the_agents_md_block_is_added_and_removed_leaving_the_text_around_it() {
    let fx = fixture();
    let md = Codex::instructions_file(&fx.env);
    let mine = "# My rules\n\nAlways run the tests.\n";
    write(&md, mine);

    let opts = ConnectOptions { instructions: true };
    Codex.connect(&fx.env, &opts).unwrap();
    let text = read(&md);
    assert!(text.starts_with(mine), "{text}");
    assert!(text.contains(codex::BLOCK_START) && text.contains(codex::BLOCK_END));
    assert!(text.contains("br8n_search") && text.contains("br8n_remember"));
    assert_eq!(Codex.instructions(&fx.env), Some(true));

    assert!(Codex.connect(&fx.env, &opts).unwrap().is_empty());

    Codex.disconnect(&fx.env).unwrap();
    assert_eq!(read(&md), mine);
    assert_eq!(Codex.instructions(&fx.env), Some(false));

    let middle = format!("top\n{}bottom\n", codex::block_text());
    assert_eq!(codex::without_block(&middle).unwrap(), "top\nbottom\n");
    let edited = format!(
        "top\n{}\nold words\n{}\nbottom\n",
        codex::BLOCK_START,
        codex::BLOCK_END
    );
    assert_eq!(
        codex::with_block(&edited),
        format!("top\n{}bottom\n", codex::block_text())
    );
}

const GEMINI: &str = r#"{
  "theme": "dark",
  "hooks": {
    "BeforeAgent": [
      { "hooks": [ { "type": "command", "command": "mine.sh", "timeout": 100 } ] }
    ],
    "SessionStart": [
      { "hooks": [ { "type": "command", "command": "start.sh" } ] }
    ]
  }
}
"#;

#[test]
fn gemini_gets_the_server_and_a_before_agent_hook_beside_the_users_own() {
    let fx = fixture();
    let file = Gemini::config_file(&fx.env);
    write(&file, GEMINI);

    Gemini.connect(&fx.env, &none()).unwrap();
    let v = json(&file);
    assert_eq!(v["mcpServers"]["br8n"]["command"], bin(&fx));
    let groups = v["hooks"]["BeforeAgent"].as_array().unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0]["hooks"][0]["command"], "mine.sh");
    assert_eq!(
        groups[1]["hooks"][0]["command"],
        format!("\"{}\" hook prompt --agent gemini", bin(&fx))
    );
    assert_eq!(groups[1]["hooks"][0]["timeout"], 5000);
    assert_eq!(Gemini.status(&fx.env), Status::Connected);

    Gemini.disconnect(&fx.env).unwrap();
    assert_eq!(
        json(&file),
        serde_json::from_str::<serde_json::Value>(GEMINI).unwrap()
    );
}

#[test]
fn instructions_are_refused_for_an_agent_that_has_no_instructions_file() {
    let fx = fixture();
    let err =
        agents::connect(&Cursor, &fx.env, &ConnectOptions { instructions: true }).unwrap_err();
    assert!(matches!(
        err.downcast_ref::<AgentError>(),
        Some(AgentError::BadOption(_))
    ));
    assert!(!Cursor::config_file(&fx.env).exists());
}

#[test]
fn the_api_lists_every_agent_and_round_trips_a_connect() {
    let fx = fixture();
    let v = api::list(&fx.env);
    let ids: Vec<&str> = v["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["claude-code", "codex", "claude-desktop", "cursor", "gemini"]
    );
    for a in v["agents"].as_array().unwrap() {
        assert!(a["name"].is_string());
        assert!(a["detected"]["installed"].is_boolean());
        assert!(a["status"]["state"].is_string());
        assert!(a["capabilities"]["mcp"].as_bool().unwrap());
    }
    let codex = &v["agents"][1];
    assert_eq!(codex["instructions"], false);
    assert_eq!(codex["capabilities"]["prompt_hook"], true);
    assert_eq!(codex["capabilities"]["transcripts"], true);
    assert!(v["snippets"]["mcp_json"]
        .as_str()
        .unwrap()
        .contains(&bin(&fx)));
    assert!(v["snippets"]["codex_toml"]
        .as_str()
        .unwrap()
        .contains("[mcp_servers.br8n]"));

    let (code, v) = api::connect(&fx.env, br#"{"id":"cursor"}"#);
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["agent"]["status"]["state"], "connected");
    assert_eq!(
        v["change"]["files"][0],
        Cursor::config_file(&fx.env).display().to_string()
    );
    assert_eq!(v["change"]["backups"], serde_json::json!([]));

    let (code, v) = api::disconnect(&fx.env, br#"{"id":"cursor"}"#);
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["agent"]["status"]["state"], "not_connected");

    assert_eq!(api::connect(&fx.env, br#"{"id":"vim"}"#).0, 404);
    assert_eq!(api::connect(&fx.env, b"not json").0, 400);
    assert_eq!(api::connect(&fx.env, br#"{"id":""}"#).0, 400);
    assert_eq!(
        api::connect(&fx.env, br#"{"id":"cursor","instructions":true}"#).0,
        400
    );
    assert_eq!(api::connect(&fx.env, br#"{"id":"claude-code"}"#).0, 409);
}

#[test]
fn install_offers_detected_agents_and_quiet_only_repairs_stale_ones() {
    let fx = fixture();
    std::fs::create_dir_all(fx.home.join(".cursor")).unwrap();
    write(
        &Gemini::config_file(&fx.env),
        r#"{"mcpServers":{"br8n":{"command":"/old/br8n","args":["mcp"]}}}"#,
    );

    let quiet = agents::offer_after_install(&fx.env, true, true, |_| panic!("never asks"));
    assert!(quiet.warnings.is_empty(), "{:?}", quiet.warnings);
    assert_eq!(Cursor.status(&fx.env), Status::NotConnected);
    assert_eq!(Gemini.status(&fx.env), Status::Connected);

    let declined = agents::offer_after_install(&fx.env, false, false, |_| false);
    assert_eq!(Cursor.status(&fx.env), Status::NotConnected);
    assert!(declined
        .lines
        .iter()
        .any(|l| l.contains("br8n connect cursor")));

    agents::offer_after_install(&fx.env, true, false, |_| panic!("--yes never asks"));
    assert_eq!(Cursor.status(&fx.env), Status::Connected);
    assert_eq!(
        ClaudeDesktop.status(&fx.env),
        Status::NotConnected,
        "not detected"
    );

    let r = agents::disconnect_all_but_claude_code(&fx.env);
    assert_eq!(r.lines.len(), 2, "{:?}", r.lines);
    assert_eq!(Cursor.status(&fx.env), Status::NotConnected);
    assert_eq!(Gemini.status(&fx.env), Status::NotConnected);
}

#[test]
fn a_config_reached_through_a_symlink_is_edited_in_place() {
    let fx = fixture();
    let real = fx.home.join("dotfiles/mcp.json");
    write(&real, "{}\n");
    let link = Cursor::config_file(&fx.env);
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    Cursor.connect(&fx.env, &none()).unwrap();
    assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(json(&real)["mcpServers"]["br8n"]["command"], bin(&fx));
}

#[test]
fn each_agent_gets_its_own_hook_envelope_and_claude_codes_is_unchanged() {
    assert_eq!(
        PromptAgent::ClaudeCode.envelope(Some("ctx")).unwrap(),
        r#"{"hookSpecificOutput":{"additionalContext":"ctx","hookEventName":"UserPromptSubmit"}}"#
    );
    assert_eq!(PromptAgent::ClaudeCode.envelope(None), None);
    assert_eq!(
        PromptAgent::Codex.envelope(Some("ctx")),
        PromptAgent::ClaudeCode.envelope(Some("ctx"))
    );
    assert_eq!(
        PromptAgent::Gemini.envelope(Some("ctx")).unwrap(),
        r#"{"hookSpecificOutput":{"additionalContext":"ctx"}}"#
    );
    assert_eq!(PromptAgent::Gemini.envelope(None).unwrap(), "{}");
    let codex_stdin = r#"{"session_id":"s","cwd":"/","hook_event_name":"UserPromptSubmit","turn_id":"t","prompt":"how did I fix it"}"#;
    assert_eq!(
        PromptAgent::Codex.prompt_of(codex_stdin).as_deref(),
        Some("how did I fix it")
    );
    let gemini_stdin = r#"{"session_id":"s","hook_event_name":"BeforeAgent","timestamp":"x","prompt":"my notes on raft"}"#;
    assert_eq!(
        PromptAgent::Gemini.prompt_of(gemini_stdin).as_deref(),
        Some("my notes on raft")
    );
}
