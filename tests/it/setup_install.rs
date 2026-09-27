use br8n::setup::claude::ClaudeCli;
use br8n::setup::install::{install, uninstall, InstallOpts, UninstallOpts};
use br8n::setup::Paths;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

fn executable(p: &Path, body: &str) {
    std::fs::write(p, body).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn stub_claude(dir: &Path) -> ClaudeCli {
    std::fs::write(dir.join("marketplaces.json"), "[]").unwrap();
    std::fs::write(dir.join("plugins.json"), "[]").unwrap();
    let p = dir.join("claude");
    executable(
        &p,
        &format!(
            "#!/bin/sh\necho \"$*\" >> \"{d}/calls\"\ncase \"$*\" in\n\
             '--version') exit 0 ;;\n\
             'plugin marketplace list --json') cat \"{d}/marketplaces.json\" ;;\n\
             'plugin list --json') cat \"{d}/plugins.json\" ;;\n\
             *) exit 0 ;;\nesac\n",
            d = dir.display()
        ),
    );
    ClaudeCli::at(&p)
}

fn calls(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

struct Fx {
    _t: tempfile::TempDir,
    root: PathBuf,
    stubdir: PathBuf,
    paths: Paths,
    opts: InstallOpts,
}

fn fixture(version_printed: &str) -> Fx {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("root");
    let stubdir = t.path().join("stub");
    let linkdir = t.path().join("linkdir");
    std::fs::create_dir_all(&stubdir).unwrap();
    std::fs::create_dir_all(&linkdir).unwrap();
    let exe = stubdir.join("br8n-src");
    executable(&exe, &format!("#!/bin/sh\necho 'br8n {version_printed}'\n"));
    let paths = Paths::at(&root, vec![linkdir], t.path().join("cache/br8n"));
    let opts = InstallOpts {
        exe,
        version: "1.2.3".to_string(),
        config_path: t.path().join("cfg/config.toml"),
        claude: stub_claude(&stubdir),
        yes: true,
        quiet: true,
        ollama_url: "http://127.0.0.1:1".to_string(),
        models: vec!["qwen3-embedding:0.6b".to_string()],
        confirm: |_| false,
        pull: |_| Ok(()),
    };
    Fx {
        _t: t,
        root,
        stubdir,
        paths,
        opts,
    }
}

#[test]
fn a_first_install_lays_everything_out_and_registers() {
    let fx = fixture("1.2.3");
    let report = install(&fx.paths, &fx.opts).unwrap();
    assert!(fx.paths.bin.is_file(), "binary placed");
    assert_eq!(
        std::fs::read(&fx.paths.bin).unwrap(),
        std::fs::read(&fx.opts.exe).unwrap()
    );
    let plugin =
        std::fs::read_to_string(fx.root.join("plugin/.claude-plugin/plugin.json")).unwrap();
    assert!(
        plugin.contains("\"1.2.3\"")
            && plugin.contains(&fx.paths.bin.to_string_lossy().to_string())
    );
    let link = fx.paths.link_dirs[0].join("br8n");
    assert_eq!(std::fs::read_link(&link).unwrap(), fx.paths.bin);
    assert!(fx.opts.config_path.is_file(), "starter config written");
    assert!(std::fs::read_to_string(&fx.opts.config_path)
        .unwrap()
        .contains("sources"));
    assert_eq!(
        calls(&fx.stubdir),
        vec![
            "--version".to_string(),
            "plugin marketplace list --json".to_string(),
            format!("plugin marketplace add {}", fx.paths.plugin.display()),
            "plugin list --json".to_string(),
            "plugin install br8n@br8n".to_string(),
        ]
    );
    assert_eq!(report.warnings.len(), 2, "{:?}", report.warnings);
    assert!(
        report.warnings.iter().any(|w| w.contains("is not on PATH")),
        "{:?}",
        report.warnings
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("Ollama is not reachable")),
        "{:?}",
        report.warnings
    );
}

#[test]
fn a_second_install_copies_no_binary_keeps_the_config_and_updates_the_plugin() {
    let fx = fixture("1.2.3");
    install(&fx.paths, &fx.opts).unwrap();
    std::fs::write(&fx.opts.config_path, "sources = [\"/mine\"]\n").unwrap();
    let before_ino = std::fs::metadata(&fx.paths.bin).unwrap().ino();

    let stray = fx.paths.plugin.join("commands/old.md");
    std::fs::create_dir_all(stray.parent().unwrap()).unwrap();
    std::fs::write(&stray, "stale").unwrap();

    std::fs::write(
        fx.stubdir.join("marketplaces.json"),
        format!(
            r#"[{{"name":"br8n","source":"directory","path":"{}"}}]"#,
            fx.paths.plugin.display()
        ),
    )
    .unwrap();
    std::fs::write(
        fx.stubdir.join("plugins.json"),
        r#"[{"id":"br8n@br8n","version":"1.2.3"}]"#,
    )
    .unwrap();
    std::fs::remove_file(fx.stubdir.join("calls")).unwrap();

    let opts = InstallOpts {
        exe: fx.paths.bin.clone(),
        ..clone_opts(&fx.opts)
    };
    install(&fx.paths, &opts).unwrap();
    assert_eq!(
        std::fs::metadata(&fx.paths.bin).unwrap().ino(),
        before_ino,
        "binary must not be re-copied when it is already in place"
    );
    assert_eq!(
        std::fs::read_to_string(&fx.opts.config_path).unwrap(),
        "sources = [\"/mine\"]\n"
    );
    assert!(
        !stray.exists(),
        "stale plugin file must be removed by re-install"
    );
    assert_eq!(
        calls(&fx.stubdir),
        vec![
            "--version",
            "plugin marketplace list --json",
            "plugin list --json",
            "plugin update br8n@br8n"
        ]
    );
}

fn clone_opts(o: &InstallOpts) -> InstallOpts {
    InstallOpts {
        exe: o.exe.clone(),
        version: o.version.clone(),
        config_path: o.config_path.clone(),
        claude: o.claude.clone(),
        yes: o.yes,
        quiet: o.quiet,
        ollama_url: o.ollama_url.clone(),
        models: o.models.clone(),
        confirm: o.confirm,
        pull: o.pull,
    }
}

#[test]
fn a_marketplace_named_br8n_pointing_elsewhere_is_repointed() {
    let fx = fixture("1.2.3");
    std::fs::write(
        fx.stubdir.join("marketplaces.json"),
        r#"[{"name":"br8n","source":"directory","path":"/old/clone"}]"#,
    )
    .unwrap();
    let report = install(&fx.paths, &fx.opts).unwrap();
    let c = calls(&fx.stubdir);
    let remove = c
        .iter()
        .position(|l| l == "plugin marketplace remove br8n")
        .expect("remove");
    let add = c
        .iter()
        .position(|l| l == &format!("plugin marketplace add {}", fx.paths.plugin.display()))
        .expect("add");
    assert!(remove < add, "remove must precede add: {c:?}");
    assert!(
        report.lines.iter().any(|l| l.contains("/old/clone")),
        "{:?}",
        report.lines
    );
}

#[test]
fn a_placed_binary_that_reports_the_wrong_version_is_a_hard_failure() {
    let fx = fixture("0.0.9");
    let err = install(&fx.paths, &fx.opts).unwrap_err().to_string();
    assert!(err.contains("0.0.9") && err.contains("1.2.3"), "{err}");
}

#[test]
fn a_missing_claude_cli_is_a_warning_and_everything_else_still_happens() {
    let mut fx = fixture("1.2.3");
    fx.opts.claude = ClaudeCli::at(Path::new("/nonexistent/claude"));
    let report = install(&fx.paths, &fx.opts).unwrap();
    assert!(fx.paths.bin.is_file() && fx.paths.plugin.join("hooks/hooks.json").is_file());
    assert!(
        report.warnings.iter().any(|w| w.contains("Claude Code")),
        "{:?}",
        report.warnings
    );
}

#[test]
fn the_link_dir_is_created_when_missing_and_an_existing_wrong_link_is_replaced() {
    let fx = fixture("1.2.3");
    let dir = fx.paths.link_dirs[0].clone();
    std::fs::remove_dir(&dir).unwrap();
    install(&fx.paths, &fx.opts).unwrap();
    assert_eq!(std::fs::read_link(dir.join("br8n")).unwrap(), fx.paths.bin);
    std::os::unix::fs::symlink("/elsewhere", dir.join("br8n")).ok();
    std::fs::remove_file(dir.join("br8n")).unwrap();
    std::os::unix::fs::symlink("/elsewhere", dir.join("br8n")).unwrap();
    install(&fx.paths, &fx.opts).unwrap();
    assert_eq!(std::fs::read_link(dir.join("br8n")).unwrap(), fx.paths.bin);
}

fn installed_fixture() -> Fx {
    let fx = fixture("1.2.3");
    install(&fx.paths, &fx.opts).unwrap();
    std::fs::create_dir_all(&fx.paths.db).unwrap();
    std::fs::write(fx.paths.db.join("graph.kz"), b"x").unwrap();
    std::fs::create_dir_all(&fx.paths.claude_cache).unwrap();
    std::fs::write(fx.paths.claude_cache.join("stale"), b"x").unwrap();
    std::fs::write(
        fx.stubdir.join("marketplaces.json"),
        format!(
            r#"[{{"name":"br8n","source":"directory","path":"{}"}}]"#,
            fx.paths.plugin.display()
        ),
    )
    .unwrap();
    std::fs::write(
        fx.stubdir.join("plugins.json"),
        r#"[{"id":"br8n@br8n","version":"1.2.3"}]"#,
    )
    .unwrap();
    std::fs::remove_file(fx.stubdir.join("calls")).unwrap();
    fx
}

fn unopts(fx: &Fx, purge: bool) -> UninstallOpts {
    UninstallOpts {
        purge,
        yes: true,
        claude: fx.opts.claude.clone(),
        config_path: fx.opts.config_path.clone(),
        confirm: |_| false,
    }
}

#[test]
fn uninstall_removes_what_install_made_and_keeps_the_data() {
    let fx = installed_fixture();
    let report = uninstall(&fx.paths, &unopts(&fx, false)).unwrap();
    assert!(!fx.paths.bin.exists() && !fx.paths.plugin.exists() && !fx.paths.tmp.exists());
    assert!(
        !fx.paths.link_dirs[0]
            .join("br8n")
            .symlink_metadata()
            .is_ok(),
        "link removed"
    );
    assert!(!fx.paths.claude_cache.exists(), "claude cache removed");
    assert!(fx.paths.db.join("graph.kz").is_file(), "index kept");
    assert!(fx.opts.config_path.is_file(), "config kept");
    assert!(report.kept.contains(&fx.opts.config_path) && report.kept.contains(&fx.paths.db));
    assert_eq!(
        calls(&fx.stubdir),
        vec![
            "--version",
            "plugin list --json",
            "plugin uninstall br8n@br8n",
            "plugin marketplace list --json",
            "plugin marketplace remove br8n"
        ]
    );
}

#[test]
fn purge_removes_the_root_and_the_config() {
    let fx = installed_fixture();
    uninstall(&fx.paths, &unopts(&fx, true)).unwrap();
    assert!(!fx.root.exists());
    assert!(!fx.opts.config_path.exists());
}

#[test]
fn purge_without_consent_is_refused_and_removes_nothing() {
    let fx = installed_fixture();
    let opts = UninstallOpts {
        yes: false,
        ..unopts(&fx, true)
    };
    assert!(uninstall(&fx.paths, &opts).is_err());
    assert!(fx.paths.bin.is_file() && fx.opts.config_path.is_file());
}

#[test]
fn uninstall_refuses_while_an_index_holds_the_lock() {
    let fx = installed_fixture();
    std::fs::write(
        fx.paths.db.with_extension("lock"),
        std::process::id().to_string(),
    )
    .unwrap();
    let err = uninstall(&fx.paths, &unopts(&fx, false))
        .unwrap_err()
        .to_string();
    assert!(err.contains("index"), "{err}");
    assert!(fx.paths.bin.is_file());
}

#[test]
fn a_foreign_link_and_a_foreign_marketplace_are_left_alone() {
    let fx = installed_fixture();
    let link = fx.paths.link_dirs[0].join("br8n");
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink("/elsewhere/br8n", &link).unwrap();
    std::fs::write(
        fx.stubdir.join("marketplaces.json"),
        r#"[{"name":"br8n","source":"directory","path":"/somebody/elses"}]"#,
    )
    .unwrap();
    let report = uninstall(&fx.paths, &unopts(&fx, false)).unwrap();
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        std::path::PathBuf::from("/elsewhere/br8n")
    );
    assert!(!calls(&fx.stubdir)
        .iter()
        .any(|c| c.starts_with("plugin marketplace remove")));
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("/somebody/elses")),
        "{:?}",
        report.warnings
    );
}

#[test]
fn with_no_model_left_on_ollama_install_does_not_ask_for_ollama() {
    let fx = fixture("1.2.3");
    let opts = InstallOpts {
        models: vec![],
        ..clone_opts(&fx.opts)
    };
    let report = install(&fx.paths, &opts).unwrap();
    let said = [report.lines.clone(), report.warnings.clone()].concat();
    assert!(
        !said.iter().any(|l| l.to_lowercase().contains("ollama")),
        "{said:?}"
    );
}

#[test]
fn each_failed_install_check_names_the_fix_the_dashboard_offers() {
    use br8n::setup::install::{fix_for, Fix, InstallCheck};
    let exe = Path::new("/opt/build/br8n");
    let failed = |name: &'static str| InstallCheck {
        name,
        ok: false,
        detail: String::new(),
    };
    let install = Some(Fix::Command {
        command: "\"/opt/build/br8n\" install".to_string(),
    });
    let connect = Some(Fix::Connect {
        agent: "claude-code",
    });
    for name in ["plugin", "marketplace", "registration"] {
        assert_eq!(fix_for(&failed(name), exe, true), connect, "{name}");
    }
    assert_eq!(
        fix_for(&failed("plugin"), exe, false),
        install,
        "without `claude` the plugin is only written by an install"
    );
    for name in ["binary", "path"] {
        assert_eq!(fix_for(&failed(name), exe, true), install, "{name}");
    }
    assert!(matches!(
        fix_for(&failed("claude"), exe, false),
        Some(Fix::Manual { .. })
    ));
    let passing = InstallCheck {
        ok: true,
        ..failed("plugin")
    };
    assert_eq!(fix_for(&passing, exe, true), None);
}
