use br8n::setup::claude::ClaudeCli;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn stub(dir: &Path, marketplaces: &str, plugins: &str, exit: i32) -> PathBuf {
    std::fs::write(dir.join("marketplaces.json"), marketplaces).unwrap();
    std::fs::write(dir.join("plugins.json"), plugins).unwrap();
    let script = format!(
        "#!/bin/sh\n\
         echo \"$*\" >> \"{d}/calls\"\n\
         case \"$*\" in\n\
           '--version') echo 'stub'; exit 0 ;;\n\
           'plugin marketplace list --json') cat \"{d}/marketplaces.json\"; exit 0 ;;\n\
           'plugin list --json') cat \"{d}/plugins.json\"; exit 0 ;;\n\
           *) echo 'stub refused' >&2; exit {exit} ;;\n\
         esac\n",
        d = dir.display()
    );
    let p = dir.join("claude");
    std::fs::write(&p, script).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn calls(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

const MARKETS: &str = r#"[{"name":"br8n","source":"directory","path":"/old/clone","installLocation":"/old/clone"},{"name":"official","source":"github","repo":"anthropics/x","installLocation":"/x"}]"#;
const PLUGINS: &str = r#"[{"id":"br8n@br8n","version":"0.2.0","scope":"user","enabled":true,"installPath":"/c/br8n/br8n/0.2.0"}]"#;

#[test]
fn lists_parse_the_json_shapes_the_real_cli_prints() {
    let t = tempfile::tempdir().unwrap();
    let cli = ClaudeCli::at(&stub(t.path(), MARKETS, PLUGINS, 0));
    assert!(cli.available());
    let m = cli.marketplaces().unwrap();
    assert_eq!(m.len(), 2);
    assert_eq!(m[0].name, "br8n");
    assert_eq!(m[0].source, "directory");
    assert_eq!(m[0].path.as_deref(), Some(Path::new("/old/clone")));
    assert_eq!(m[1].path, None);
    let p = cli.plugins().unwrap();
    assert_eq!(p[0].id, "br8n@br8n");
    assert_eq!(p[0].version, "0.2.0");
    assert_eq!(
        p[0].install_path.as_deref(),
        Some(Path::new("/c/br8n/br8n/0.2.0"))
    );
}

#[test]
fn mutating_calls_pass_the_exact_argv() {
    let t = tempfile::tempdir().unwrap();
    let cli = ClaudeCli::at(&stub(t.path(), "[]", "[]", 0));
    cli.marketplace_add(Path::new("/r/plugin")).unwrap();
    cli.marketplace_remove("br8n").unwrap();
    cli.plugin_install("br8n@br8n").unwrap();
    cli.plugin_update("br8n@br8n").unwrap();
    cli.plugin_uninstall("br8n@br8n").unwrap();
    assert_eq!(
        calls(t.path()),
        vec![
            "plugin marketplace add /r/plugin",
            "plugin marketplace remove br8n",
            "plugin install br8n@br8n",
            "plugin update br8n@br8n",
            "plugin uninstall br8n@br8n",
        ]
    );
}

#[test]
fn a_failing_call_reports_the_command_and_the_stderr() {
    let t = tempfile::tempdir().unwrap();
    let cli = ClaudeCli::at(&stub(t.path(), "[]", "[]", 3));
    let err = cli.plugin_install("br8n@br8n").unwrap_err().to_string();
    assert!(err.contains("claude plugin install br8n@br8n"), "{err}");
    assert!(err.contains("stub refused"), "{err}");
}

#[test]
fn a_missing_program_is_not_available_and_lists_error() {
    let cli = ClaudeCli::at(Path::new("/nonexistent/claude"));
    assert!(!cli.available());
    assert!(cli.marketplaces().is_err());
}
