use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn launcher() -> PathBuf {
    std::fs::canonicalize("plugin/scripts/br8n.sh").unwrap()
}

fn stub_br8n(at: &Path) {
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::fs::write(at, "#!/bin/sh\necho \"ran $0 $*\"\n").unwrap();
    std::fs::set_permissions(at, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn run(home: &Path, path: &Path, extra: &[(&str, &Path)], args: &[&str]) -> Output {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg(launcher())
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("PATH", path)
        .stdin(Stdio::null());
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn br8n_on_path_is_run_with_the_launchers_arguments() {
    let t = tempfile::tempdir().unwrap();
    let bin = t.path().join("path/br8n");
    stub_br8n(&bin);
    let o = run(t.path(), &t.path().join("path"), &[], &["hook", "prompt"]);
    assert!(o.status.success(), "{o:?}");
    assert_eq!(stdout(&o), format!("ran {} hook prompt\n", bin.display()));
}

#[test]
fn an_install_in_the_data_directory_is_found_when_path_has_no_br8n() {
    let t = tempfile::tempdir().unwrap();
    let empty = t.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();

    let linux = t.path().join(".local/share/br8n/bin/br8n");
    stub_br8n(&linux);
    let o = run(t.path(), &empty, &[], &["mcp"]);
    assert_eq!(stdout(&o), format!("ran {} mcp\n", linux.display()));
    std::fs::remove_file(&linux).unwrap();

    let xdg = t.path().join("xdg/br8n/bin/br8n");
    stub_br8n(&xdg);
    let o = run(
        t.path(),
        &empty,
        &[("XDG_DATA_HOME", &t.path().join("xdg"))],
        &["mcp"],
    );
    assert_eq!(stdout(&o), format!("ran {} mcp\n", xdg.display()));
    std::fs::remove_file(&xdg).unwrap();

    let mac = t.path().join("Library/Application Support/br8n/bin/br8n");
    stub_br8n(&mac);
    let o = run(t.path(), &empty, &[], &["mcp"]);
    assert_eq!(stdout(&o), format!("ran {} mcp\n", mac.display()));
}

#[test]
fn br8n_db_points_the_launcher_at_the_install_beside_that_index() {
    let t = tempfile::tempdir().unwrap();
    let bin = t.path().join("custom/bin/br8n");
    stub_br8n(&bin);
    let o = run(
        t.path(),
        &t.path().join("empty"),
        &[("BR8N_DB", &t.path().join("custom/db"))],
        &["hook", "session-start"],
    );
    assert_eq!(
        stdout(&o),
        format!("ran {} hook session-start\n", bin.display())
    );
}

#[test]
fn without_br8n_session_start_tells_the_user_and_claude_how_to_install_it() {
    let t = tempfile::tempdir().unwrap();
    let o = run(
        t.path(),
        &t.path().join("empty"),
        &[],
        &["hook", "session-start"],
    );
    assert!(
        o.status.success(),
        "a hook that fails shows an error: {o:?}"
    );
    let v: serde_json::Value = serde_json::from_str(&stdout(&o)).unwrap();
    let shown = v["systemMessage"].as_str().unwrap();
    assert!(
        shown.contains("https://github.com/Jagma/br8n#install"),
        "{shown}"
    );
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    assert_eq!(v["hookSpecificOutput"]["additionalContext"], shown);
}

#[test]
fn without_br8n_the_prompt_hook_lets_the_prompt_through_silently() {
    let t = tempfile::tempdir().unwrap();
    let o = run(t.path(), &t.path().join("empty"), &[], &["hook", "prompt"]);
    assert!(o.status.success(), "{o:?}");
    assert!(o.stdout.is_empty() && o.stderr.is_empty(), "{o:?}");
}

#[test]
fn without_br8n_the_mcp_server_fails_and_says_why() {
    let t = tempfile::tempdir().unwrap();
    let o = run(t.path(), &t.path().join("empty"), &[], &["mcp"]);
    assert_eq!(o.status.code(), Some(1), "{o:?}");
    assert!(o.stdout.is_empty(), "stdout belongs to the MCP protocol");
    assert!(String::from_utf8_lossy(&o.stderr).contains("br8n is not installed"));
}
