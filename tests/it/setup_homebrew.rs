use br8n::setup::claude::ClaudeCli;
use br8n::setup::install::{install, uninstall, InstallOpts, UninstallOpts};
use br8n::setup::{homebrew_prefix, Paths};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn executable(p: &Path, body: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
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
            "#!/bin/sh\ncase \"$*\" in\n\
             'plugin marketplace list --json') cat \"{d}/marketplaces.json\" ;;\n\
             'plugin list --json') cat \"{d}/plugins.json\" ;;\n\
             *) exit 0 ;;\nesac\n",
            d = dir.display()
        ),
    );
    ClaudeCli::at(&p)
}

struct Fx {
    _t: tempfile::TempDir,
    prefix: PathBuf,
    keg_bin: PathBuf,
    paths: Paths,
    opts: InstallOpts,
}

fn homebrew_fixture() -> Fx {
    let t = tempfile::tempdir().unwrap();
    let prefix = t.path().join("brew");
    let keg_bin = prefix.join("Cellar/br8n/1.2.3/bin/br8n");
    executable(&keg_bin, "#!/bin/sh\necho 'br8n 1.2.3'\n");
    std::fs::create_dir_all(prefix.join("opt")).unwrap();
    std::os::unix::fs::symlink("../Cellar/br8n/1.2.3", prefix.join("opt/br8n")).unwrap();
    let stub = t.path().join("stub");
    std::fs::create_dir_all(&stub).unwrap();
    let linkdir = t.path().join("linkdir");
    std::fs::create_dir_all(&linkdir).unwrap();
    let paths = Paths::at(
        &t.path().join("root"),
        vec![linkdir],
        t.path().join("cache"),
    )
    .installed_by_homebrew(&prefix);
    let opts = InstallOpts {
        exe: keg_bin.clone(),
        version: "1.2.3".to_string(),
        config_path: t.path().join("cfg/config.toml"),
        claude: stub_claude(&stub),
        yes: true,
        quiet: true,
        ollama_url: "http://127.0.0.1:1".to_string(),
        models: vec![],
        confirm: |_| false,
        pull: |_| Ok(()),
    };
    Fx {
        _t: t,
        prefix,
        keg_bin,
        paths,
        opts,
    }
}

#[test]
fn a_binary_inside_a_homebrew_keg_names_the_prefix() {
    for (exe, prefix) in [
        (
            "/opt/homebrew/Cellar/br8n/0.1.1/bin/br8n",
            Some("/opt/homebrew"),
        ),
        (
            "/home/linuxbrew/.linuxbrew/Cellar/br8n/0.1.1_1/bin/br8n",
            Some("/home/linuxbrew/.linuxbrew"),
        ),
        ("/Users/x/Library/Application Support/br8n/bin/br8n", None),
        ("/opt/homebrew/Cellar/other/1.0/bin/br8n", None),
        ("/opt/homebrew/Cellar/br8n/0.1.1/libexec/br8n", None),
    ] {
        assert_eq!(
            homebrew_prefix(Path::new(exe)),
            prefix.map(PathBuf::from),
            "{exe}"
        );
    }
}

#[test]
fn installed_by_homebrew_runs_the_opt_link_and_keeps_its_data_in_the_root() {
    let fx = homebrew_fixture();
    assert_eq!(fx.paths.bin, fx.prefix.join("opt/br8n/bin/br8n"));
    assert_eq!(fx.paths.homebrew.as_deref(), Some(fx.prefix.as_path()));
    assert!(fx.paths.db.starts_with(&fx.paths.root));
    assert!(fx.paths.plugin.starts_with(&fx.paths.root));
}

#[test]
fn a_homebrew_install_neither_copies_nor_links_the_binary() {
    let fx = homebrew_fixture();
    let report = install(&fx.paths, &fx.opts).unwrap();
    assert!(!fx.paths.root.join("bin").exists(), "nothing copied");
    assert!(
        std::fs::read_dir(&fx.paths.link_dirs[0])
            .unwrap()
            .next()
            .is_none(),
        "nothing linked"
    );
    let hooks = std::fs::read_to_string(fx.paths.plugin.join("hooks/hooks.json")).unwrap();
    assert!(
        hooks.contains(&fx.prefix.join("opt/br8n/bin/br8n").display().to_string()),
        "the plugin must run the opt link, which survives brew upgrade: {hooks}"
    );
    assert!(
        report
            .lines
            .iter()
            .any(|l| l.contains("Homebrew puts br8n on PATH")),
        "{:?}",
        report.lines
    );
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(fx.keg_bin.is_file());
}

#[test]
fn a_homebrew_install_retires_the_copy_an_earlier_install_placed() {
    let fx = homebrew_fixture();
    let own = fx.paths.root.join("bin/br8n");
    executable(&own, "#!/bin/sh\necho 'br8n 1.0.0'\n");
    let linkdir = &fx.paths.link_dirs[0];
    std::os::unix::fs::symlink(&own, linkdir.join("br8n")).unwrap();
    std::os::unix::fs::symlink("/elsewhere/tool", linkdir.join("tool")).unwrap();

    let report = install(&fx.paths, &fx.opts).unwrap();
    assert!(!own.exists() && !fx.paths.root.join("bin").exists());
    assert!(linkdir.join("br8n").symlink_metadata().is_err());
    assert!(
        linkdir.join("tool").symlink_metadata().is_ok(),
        "a link that is not br8n's stays"
    );
    assert!(
        report
            .lines
            .iter()
            .any(|l| l.contains("the copy from before Homebrew installed br8n")),
        "{:?}",
        report.lines
    );
}

#[test]
fn uninstall_leaves_homebrews_binary_and_says_how_to_remove_it() {
    let fx = homebrew_fixture();
    install(&fx.paths, &fx.opts).unwrap();
    let report = uninstall(
        &fx.paths,
        &UninstallOpts {
            purge: false,
            yes: true,
            claude: fx.opts.claude.clone(),
            config_path: fx.opts.config_path.clone(),
            confirm: |_| false,
        },
    )
    .unwrap();
    assert!(
        fx.keg_bin.is_file(),
        "Homebrew's keg is Homebrew's to remove"
    );
    assert!(!fx.paths.plugin.exists());
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("brew uninstall br8n")),
        "{:?}",
        report.warnings
    );
}
