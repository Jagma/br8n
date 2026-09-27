use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;

fn br8n(dir: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("br8n").unwrap();
    c.env("BR8N_CONFIG", dir.join("config.toml"))
        .env("BR8N_DB", dir.join("db"));
    c
}

fn write_config(dir: &std::path::Path, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("config.toml"), body).unwrap();
}

#[test]
fn no_backup_table_is_a_skip_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), "sources = []\n");
    br8n(dir.path())
        .arg("backup")
        .assert()
        .code(2)
        .stderr(contains("no backups configured"));
}

#[test]
fn enabled_with_no_targets_is_still_a_skip() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), "sources = []\n[backup]\nenabled = true\n");
    br8n(dir.path())
        .arg("backup")
        .assert()
        .code(2)
        .stderr(contains("no backups configured"));
}

#[test]
fn an_enabled_backup_with_no_key_names_the_command_that_fixes_it() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        "sources = []\n[backup]\nenabled = true\ntargets = [\"s3\"]\n\
         [backup.s3]\nbucket = \"b\"\nregion = \"eu-west-1\"\n",
    );
    br8n(dir.path())
        .arg("backup")
        .assert()
        .code(1)
        .stderr(contains("br8n backup init"));
}

#[test]
fn every_run_appends_one_line_to_the_backup_log() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), "sources = []\n");
    br8n(dir.path()).arg("backup").assert().code(2);
    br8n(dir.path()).arg("backup").assert().code(2);
    let log = std::fs::read_to_string(dir.path().join("db.backup.log")).unwrap();
    assert_eq!(log.lines().count(), 2, "got: {log}");
    assert!(log
        .lines()
        .all(|l| l.contains("skip  no backups configured")));
}

#[test]
fn a_multi_line_failure_is_logged_as_one_line() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        "sources = []\n[backup]\nenabled = true\nencrypt = false\ntargets = [\"dropbox\", \"gdrive\"]\n",
    );
    br8n(dir.path()).arg("backup").assert().code(1);
    let log = std::fs::read_to_string(dir.path().join("db.backup.log")).unwrap();
    assert_eq!(log.lines().count(), 1, "got: {log}");
    assert!(
        log.contains("dropbox") && log.contains("gdrive"),
        "got: {log}"
    );
}

#[test]
fn init_writes_a_key_at_mode_600_and_prints_it_once() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), "sources = []\n[backup]\nenabled = true\n");
    let out = br8n(dir.path())
        .args(["backup", "init", "--yes"])
        .assert()
        .success()
        .stdout(contains("store this somewhere other than this machine"));

    let key = dir.path().join("backup.key");
    let hex = std::fs::read_to_string(&key).unwrap();
    assert_eq!(hex.trim().len(), 64);
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert_eq!(
        stdout.matches(hex.trim()).count(),
        1,
        "the key is printed exactly once"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn init_without_yes_and_without_confirmation_exits_non_zero() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), "sources = []\n[backup]\nenabled = true\n");
    br8n(dir.path())
        .args(["backup", "init"])
        .write_stdin("no\n")
        .assert()
        .code(1)
        .stderr(contains("not confirmed"));
}

#[test]
fn init_refuses_to_overwrite_an_existing_key() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), "sources = []\n[backup]\nenabled = true\n");
    br8n(dir.path())
        .args(["backup", "init", "--yes"])
        .assert()
        .success();
    let first = std::fs::read_to_string(dir.path().join("backup.key")).unwrap();
    br8n(dir.path())
        .args(["backup", "init", "--yes"])
        .assert()
        .code(1)
        .stderr(contains("already exists").and(contains("orphan every backup")));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("backup.key")).unwrap(),
        first
    );
}

#[test]
fn a_target_with_no_matching_table_is_a_configuration_error() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        "sources = []\n[backup]\nenabled = true\nencrypt = false\ntargets = [\"s3\"]\n",
    );
    br8n(dir.path())
        .arg("backup")
        .assert()
        .code(1)
        .stderr(contains("[backup.s3]"));
}

#[test]
fn an_unknown_target_is_named() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        "sources = []\n[backup]\nenabled = true\nencrypt = false\ntargets = [\"dropbox\"]\n",
    );
    br8n(dir.path())
        .arg("backup")
        .assert()
        .code(1)
        .stderr(contains("dropbox"));
}

#[test]
fn check_reports_an_unauthorized_drive_target_without_opening_a_browser() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        "sources = []\n[backup]\nenabled = true\nencrypt = false\ntargets = [\"drive\"]\n\
         [backup.drive]\nfolder_id = \"f\"\nclient_secret_file = \"/nonexistent.json\"\n",
    );
    br8n(dir.path())
        .args(["backup", "check"])
        .timeout(std::time::Duration::from_secs(60))
        .assert()
        .code(1)
        .stderr(contains("drive:").and(contains("br8n backup auth drive")));
}

#[test]
fn status_reports_backup_age_including_never() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        "sources = []\n[backup]\nenabled = true\ntargets = [\"s3\"]\n",
    );
    br8n(dir.path())
        .arg("status")
        .assert()
        .success()
        .stdout(contains("backup:").and(contains("never")));
}

#[test]
fn status_reports_the_age_of_the_last_stamp() {
    let dir = tempfile::tempdir().unwrap();
    write_config(
        dir.path(),
        "sources = []\n[backup]\nenabled = true\ntargets = [\"s3\"]\n",
    );
    br8n::backup::write_stamp(&dir.path().join("db")).unwrap();
    br8n(dir.path())
        .arg("status")
        .assert()
        .success()
        .stdout(contains("backup:     under an hour ago (s3)"));
}

#[test]
fn status_says_nothing_about_backups_when_none_are_configured() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), "sources = []\n");
    let out = br8n(dir.path()).arg("status").assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        !stdout.contains("backup:"),
        "no noise when unconfigured: {stdout}"
    );
}

#[test]
fn restore_against_an_unconfigured_remote_fails_clearly() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), "sources = []\n");
    br8n(dir.path())
        .args(["restore", "--dry-run"])
        .assert()
        .code(1)
        .stderr(contains("no backups configured"));
}

#[test]
fn the_cron_line_is_absolute_and_carries_the_config_path() {
    let line = br8n::backup::cron_line(
        "0 13 * * *",
        std::path::Path::new("/usr/local/bin/br8n"),
        Some(std::path::Path::new("/home/u/config.toml")),
    );
    assert_eq!(
        line,
        "0 13 * * * BR8N_CONFIG=/home/u/config.toml /usr/local/bin/br8n backup >/dev/null 2>&1 # br8n-backup"
    );
}

#[test]
fn the_cron_line_omits_the_config_var_when_the_default_path_is_used() {
    let line = br8n::backup::cron_line(
        "0 13 * * *",
        std::path::Path::new("/usr/local/bin/br8n"),
        None,
    );
    assert_eq!(
        line,
        "0 13 * * * /usr/local/bin/br8n backup >/dev/null 2>&1 # br8n-backup"
    );
}

#[test]
fn scheduling_twice_leaves_exactly_one_entry_and_keeps_the_rest() {
    let existing = "MAILTO=me\n0 1 * * * other-job\n";
    let once = br8n::backup::crontab_with(existing, Some("0 13 * * * br8n backup # br8n-backup"));
    let twice = br8n::backup::crontab_with(&once, Some("0 14 * * * br8n backup # br8n-backup"));
    assert_eq!(
        twice,
        "MAILTO=me\n0 1 * * * other-job\n0 14 * * * br8n backup # br8n-backup\n"
    );
}

#[test]
fn uninstalling_removes_only_the_br8n_entry() {
    let existing = "0 1 * * * other-job\n0 13 * * * br8n backup # br8n-backup\n";
    assert_eq!(
        br8n::backup::crontab_with(existing, None),
        "0 1 * * * other-job\n"
    );
}

#[test]
fn backup_age_reads_in_hours_then_days() {
    use br8n::backup::describe_age_at;
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-24T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let ago = |h: i64| Some(now - chrono::Duration::hours(h));
    assert_eq!(describe_age_at(None, now), "never");
    assert_eq!(describe_age_at(Some(now), now), "under an hour ago");
    assert_eq!(describe_age_at(ago(1), now), "1 hour ago");
    assert_eq!(describe_age_at(ago(47), now), "47 hours ago");
    assert_eq!(describe_age_at(ago(48), now), "2 days ago");
    assert_eq!(describe_age_at(ago(24 * 6 + 5), now), "6 days ago");
}

#[test]
fn the_exit_codes_separate_done_failed_and_skipped() {
    use br8n::backup::RunOutcome;
    assert_eq!(RunOutcome::Done(vec![]).exit_code(), 0);
    assert_eq!(RunOutcome::Failed(String::new()).exit_code(), 1);
    assert_eq!(RunOutcome::Skipped(String::new()).exit_code(), 2);
}

#[test]
fn every_documented_backup_subcommand_exists() {
    let readme = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/backups.md"),
    )
    .unwrap();
    for cmd in [
        "br8n backup init",
        "br8n backup auth drive",
        "br8n backup --help",
        "br8n backup check",
        "br8n backup status",
        "br8n backup schedule",
        "br8n backup schedule --uninstall",
        "br8n restore --dry-run",
        "br8n restore --index",
    ] {
        assert!(
            readme.contains(cmd),
            "docs/backups.md must document `{cmd}`"
        );
    }

    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), "sources = []\n");
    for args in [
        vec!["backup", "--help"],
        vec!["backup", "init", "--help"],
        vec!["backup", "check", "--help"],
        vec!["backup", "auth", "--help"],
        vec!["backup", "status", "--help"],
        vec!["backup", "schedule", "--help"],
        vec!["backup", "schedule", "--uninstall", "--help"],
        vec!["restore", "--dry-run", "--index", "--help"],
    ] {
        br8n(dir.path()).args(&args).assert().success();
    }
}
