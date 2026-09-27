use br8n::update::{version_line, Status, UpdateCheck};
use predicates::str::contains;

fn chk(installed: &str, latest: Option<&str>, checked_at: u64, error: Option<&str>) -> UpdateCheck {
    UpdateCheck {
        installed: installed.into(),
        latest: latest.map(str::to_string),
        url: None,
        checked_at,
        error: error.map(str::to_string),
    }
}

#[test]
fn every_version_line_variant() {
    let now = 20_000;
    assert_eq!(
        version_line("1.0.0", None, None, now),
        "1.0.0  (never checked for updates)"
    );
    assert_eq!(
        version_line(
            "1.0.0",
            Some(&chk("1.0.0", Some("1.0.0"), now - 3 * 3600, None)),
            None,
            now
        ),
        "1.0.0  (up to date, checked 3h ago)"
    );
    assert_eq!(
        version_line(
            "1.0.0",
            Some(&chk("1.0.0", Some("1.1.0"), now - 60, None)),
            None,
            now
        ),
        "1.0.0  (1.1.0 available — run br8n update)"
    );
    assert_eq!(
        version_line(
            "1.0.0",
            Some(&chk("1.0.0", None, now - 7200, Some("HTTP 403"))),
            None,
            now
        ),
        "1.0.0  (update check failed 2h ago: HTTP 403)"
    );
    let running = Status {
        pid: 1,
        started_at: 0,
        from: "1.0.0".into(),
        to: Some("1.1.0".into()),
        phase: "downloading".into(),
        message: String::new(),
        done: false,
        ok: None,
    };
    assert_eq!(
        version_line(
            "1.0.0",
            Some(&chk("1.0.0", Some("1.1.0"), now, None)),
            Some(&running),
            now
        ),
        "1.0.0  (update to 1.1.0 running: downloading)"
    );
}

fn br8n(tmp: &std::path::Path) -> assert_cmd::Command {
    let mut c = assert_cmd::Command::cargo_bin("br8n").unwrap();
    c.env("BR8N_DB", tmp.join("root/db"))
        .env("BR8N_CONFIG", tmp.join("config.toml"))
        .env("HOME", tmp)
        .env("CODEX_HOME", tmp.join(".codex"))
        .env("PATH", "/usr/bin:/bin");
    c
}

#[test]
fn status_prints_version_install_and_ollama_lines_on_an_uninstalled_machine() {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(t.path().join("root")).unwrap();
    std::fs::write(
        t.path().join("config.toml"),
        "[embed]\nollama_url = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();
    br8n(t.path())
        .arg("status")
        .assert()
        .success()
        .stdout(contains(format!(
            "version:    {}  (never checked",
            env!("CARGO_PKG_VERSION")
        )))
        .stdout(contains("install:"))
        .stdout(contains("run br8n install"))
        .stdout(contains("ollama:     unreachable at http://127.0.0.1:1"))
        .stdout(contains("ollama serve"));
}

#[test]
fn status_names_an_available_update() {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(t.path().join("root")).unwrap();
    std::fs::write(
        t.path().join("config.toml"),
        "[embed]\nollama_url = \"http://127.0.0.1:1\"\n",
    )
    .unwrap();
    chk(
        env!("CARGO_PKG_VERSION"),
        Some("99.0.0"),
        br8n::update::now(),
        None,
    )
    .write(&t.path().join("root/update.json"))
    .unwrap();
    br8n(t.path())
        .arg("status")
        .assert()
        .success()
        .stdout(contains("99.0.0 available — run br8n update"));
}

fn executable(p: &std::path::Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(p, body).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn healthy_install(dir: &std::path::Path) -> (br8n::setup::Paths, br8n::setup::claude::ClaudeCli) {
    let root = dir.join("root");
    let linkdir = dir.join("link");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("plugin/.claude-plugin")).unwrap();
    std::fs::create_dir_all(&linkdir).unwrap();
    let version = env!("CARGO_PKG_VERSION");
    executable(
        &root.join("bin/br8n"),
        &format!("#!/bin/sh\necho 'br8n {version}'\n"),
    );
    std::fs::write(
        root.join("plugin/.claude-plugin/plugin.json"),
        format!("{{\"name\":\"br8n\",\"version\":\"{version}\"}}\n"),
    )
    .unwrap();
    std::os::unix::fs::symlink(root.join("bin/br8n"), linkdir.join("br8n")).unwrap();
    let plugin = root.join("plugin");
    std::fs::write(
        dir.join("marketplaces.json"),
        format!(
            "[{{\"name\":\"br8n\",\"source\":\"directory\",\"path\":\"{}\"}}]",
            plugin.display()
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("plugins.json"),
        format!("[{{\"id\":\"br8n@br8n\",\"version\":\"{version}\"}}]"),
    )
    .unwrap();
    let claude = dir.join("claude");
    executable(
        &claude,
        &format!(
            "#!/bin/sh\ncase \"$*\" in\n\
             '--version') exit 0 ;;\n\
             'plugin marketplace list --json') cat \"{d}/marketplaces.json\" ;;\n\
             'plugin list --json') cat \"{d}/plugins.json\" ;;\n\
             *) exit 0 ;;\nesac\n",
            d = dir.display()
        ),
    );
    let paths = br8n::setup::Paths::at(&root, vec![linkdir], dir.join("cache"));
    (paths, br8n::setup::claude::ClaudeCli::at(&claude))
}

static PATH_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn checks_with_path(
    paths: &br8n::setup::Paths,
    cli: &br8n::setup::claude::ClaudeCli,
    linkdir: &std::path::Path,
) -> Vec<br8n::setup::install::InstallCheck> {
    let _serial = PATH_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let previous = std::env::var_os("PATH");
    unsafe { std::env::set_var("PATH", format!("{}:/usr/bin:/bin", linkdir.display())) };
    let checks =
        br8n::setup::install::install_checks(paths, cli, env!("CARGO_PKG_VERSION"), &paths.bin);
    match previous {
        Some(p) => unsafe { std::env::set_var("PATH", p) },
        None => unsafe { std::env::remove_var("PATH") },
    }
    checks
}

#[test]
fn a_healthy_install_passes_all_five_checks_including_the_two_that_need_claude() {
    let t = tempfile::tempdir().unwrap();
    let (paths, cli) = healthy_install(t.path());
    let checks = checks_with_path(&paths, &cli, &t.path().join("link"));
    let names: Vec<&str> = checks.iter().map(|c| c.name).collect();
    assert_eq!(
        names,
        vec!["binary", "plugin", "marketplace", "registration", "path"],
        "the claude branch must contribute marketplace and registration"
    );
    for c in &checks {
        assert!(c.ok, "{} failed: {}", c.name, c.detail);
    }
}

#[test]
fn a_marketplace_pointing_elsewhere_fails_only_the_marketplace_check() {
    let t = tempfile::tempdir().unwrap();
    let (paths, cli) = healthy_install(t.path());
    std::fs::write(
        t.path().join("marketplaces.json"),
        "[{\"name\":\"br8n\",\"source\":\"directory\",\"path\":\"/somebody/elses\"}]",
    )
    .unwrap();
    let checks = checks_with_path(&paths, &cli, &t.path().join("link"));
    let market = checks.iter().find(|c| c.name == "marketplace").unwrap();
    assert!(!market.ok, "{}", market.detail);
    assert!(
        market.detail.contains("no marketplace"),
        "{}",
        market.detail
    );
    let registration = checks.iter().find(|c| c.name == "registration").unwrap();
    assert!(registration.ok, "{}", registration.detail);
}
