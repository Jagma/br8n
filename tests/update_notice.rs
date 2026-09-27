use br8n::hook::update_notice;
use br8n::update::UpdateCheck;
use std::path::PathBuf;
use std::time::Duration;

fn chk(installed: &str, latest: &str) -> UpdateCheck {
    UpdateCheck {
        installed: installed.into(),
        latest: Some(latest.into()),
        url: None,
        checked_at: br8n::update::now(),
        error: None,
    }
}

#[test]
fn the_notice_names_both_versions_and_both_ways_to_update() {
    let n = update_notice(Some(&chk("0.2.0", "0.2.1")), false, "0.2.0").unwrap();
    assert!(n.contains("0.2.1") && n.contains("0.2.0"), "{n}");
    assert!(
        n.contains("br8n update") && n.contains("br8n dashboard"),
        "{n}"
    );
}

#[test]
fn no_notice_when_current_updating_or_already_moved_past_the_cached_check() {
    assert!(update_notice(None, false, "0.2.0").is_none());
    assert!(update_notice(Some(&chk("0.2.0", "0.2.0")), false, "0.2.0").is_none());
    assert!(update_notice(Some(&chk("0.2.0", "0.2.1")), true, "0.2.0").is_none());
    assert!(update_notice(Some(&chk("0.2.0", "0.2.1")), false, "0.2.1").is_none());
}

struct Scratch {
    _t: tempfile::TempDir,
    root: PathBuf,
    db: PathBuf,
    cfg: PathBuf,
    home: PathBuf,
}

fn scratch(config: &str) -> Scratch {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("root");
    let home = t.path().join("home");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let cfg = t.path().join("config.toml");
    std::fs::write(&cfg, config).unwrap();
    Scratch {
        db: root.join("db"),
        root,
        cfg,
        home,
        _t: t,
    }
}

fn session_start(s: &Scratch, extra_env: &[(&str, &str)]) -> std::process::Output {
    let mut c = assert_cmd::Command::cargo_bin("br8n").unwrap();
    c.env("BR8N_DB", &s.db)
        .env("BR8N_CONFIG", &s.cfg)
        .env("HOME", &s.home)
        .env("BR8N_RELEASE_API", "http://127.0.0.1:1")
        .args(["hook", "session-start"])
        .write_stdin(r#"{"source":"startup"}"#.to_string())
        .timeout(Duration::from_secs(30));
    for (k, v) in extra_env {
        c.env(k, v);
    }
    let out = c.output().unwrap();
    assert!(out.status.success(), "SessionStart must always exit 0");
    out
}

fn log(s: &Scratch) -> String {
    std::fs::read_to_string(s.db.with_extension("log")).unwrap_or_default()
}

fn installed(s: &Scratch) {
    std::fs::create_dir_all(s.root.join("plugin")).unwrap();
}

#[test]
fn an_installed_binary_with_a_newer_cached_release_prints_the_notice_as_hook_json() {
    let s = scratch("index_transcripts = false\n");
    installed(&s);
    chk(env!("CARGO_PKG_VERSION"), "99.0.0")
        .write(&s.root.join("update.json"))
        .unwrap();
    let out = session_start(&s, &[]);
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is the hook JSON");
    assert!(v["systemMessage"].as_str().unwrap().contains("99.0.0"));
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    assert!(v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("br8n update"));
}

#[test]
fn an_uninstalled_binary_neither_checks_nor_notifies() {
    let s = scratch("index_transcripts = false\n");
    let out = session_start(&s, &[]);
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(log(&s).contains("not installed"), "{}", log(&s));
    assert!(!s.root.join("update.json").exists());
}

#[test]
fn a_stale_check_spawns_a_detached_check_that_records_its_failure() {
    let s = scratch("index_transcripts = false\n");
    installed(&s);
    let mut old = chk(env!("CARGO_PKG_VERSION"), env!("CARGO_PKG_VERSION"));
    old.checked_at = 1;
    old.write(&s.root.join("update.json")).unwrap();
    let out = session_start(&s, &[]);
    let just_after_return = UpdateCheck::read(&s.root.join("update.json")).unwrap();
    assert_eq!(
        just_after_return.checked_at, 1,
        "the parent must return before the detached child finishes: {just_after_return:?}"
    );
    assert!(out.stdout.is_empty(), "nothing newer is cached yet");
    assert!(
        log(&s).contains("checking for a newer release"),
        "{}",
        log(&s)
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(c) = UpdateCheck::read(&s.root.join("update.json")) {
            if c.checked_at > 1 {
                assert!(
                    c.error.is_some(),
                    "a closed port must record an error: {c:?}"
                );
                break;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the detached check never wrote update.json; db.log says:\n{}",
            log(&s)
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn check_false_in_the_config_skips_the_check() {
    let s = scratch("index_transcripts = false\n\n[update]\ncheck = false\n");
    installed(&s);
    session_start(&s, &[]);
    let log = log(&s);
    assert!(log.contains("update.check = false"), "{log}");
    assert!(
        !log.contains("checking for a newer release"),
        "the parent must not have decided to spawn a check: {log}"
    );
    assert!(!s.root.join("update.json").exists());
}

#[test]
fn a_running_update_suppresses_the_notice() {
    let s = scratch("index_transcripts = false\n");
    installed(&s);
    chk(env!("CARGO_PKG_VERSION"), "99.0.0")
        .write(&s.root.join("update.json"))
        .unwrap();
    let live = serde_json::json!({ "pid": std::process::id(), "started_at": 0, "from": "x", "to": "99.0.0", "phase": "downloading", "message": "", "done": false, "ok": null });
    std::fs::write(s.root.join("update.status"), live.to_string()).unwrap();
    let out = session_start(&s, &[]);
    assert!(out.stdout.is_empty());
}
