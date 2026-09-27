use assert_cmd::Command;
use br8n::config::check::{check, FieldError};
use br8n::config::edit::{self, Outcome, Patch};
use predicates::str::contains;

const COMMENTED: &str = "\
# my br8n config
index_transcripts = false

[hook]
# the hook gate, tuned by hand
threshold = 0.6  # measured on my vault
quality = 2
";

fn set(path: &str, value: serde_json::Value) -> Patch {
    let mut map = serde_json::Map::new();
    map.insert(path.to_string(), value);
    Patch::from_json(&map, &[]).unwrap()
}

fn paths(errors: &[FieldError]) -> Vec<&str> {
    errors.iter().map(|e| e.path.as_str()).collect()
}

#[test]
fn a_patch_keeps_every_comment_and_the_order_of_the_file() {
    let mut patch = set("hook.threshold", serde_json::json!(0.7));
    patch.set.push((
        "embed.model".into(),
        edit::value_from_cli("nomic-embed-text"),
    ));
    patch.unset.push("hook.quality".into());
    let out = edit::patched_text(Some(COMMENTED), &patch).unwrap();
    assert!(
        out.starts_with("# my br8n config\nindex_transcripts = false\n"),
        "{out}"
    );
    assert!(
        out.contains("# the hook gate, tuned by hand\nthreshold = 0.7  # measured on my vault\n"),
        "{out}"
    );
    assert!(!out.contains("quality"), "{out}");
    assert!(
        out.contains("[embed]\nmodel = \"nomic-embed-text\"\n"),
        "{out}"
    );
    assert!(out.find("[hook]") < out.find("[embed]"), "{out}");
    assert!(check(&out).is_empty(), "{:?}", check(&out));
}

#[test]
fn an_integer_written_to_a_float_field_is_stored_as_a_float() {
    let out = edit::patched_text(None, &set("mcp.threshold", serde_json::json!(1))).unwrap();
    assert_eq!(out, "[mcp]\nthreshold = 1.0\n");
}

#[test]
fn a_misspelt_key_is_reported_with_the_key_it_was_meant_to_be() {
    let errors = check("[hook]\nthresold = 0.5\n");
    assert_eq!(paths(&errors), ["hook.thresold"]);
    assert!(
        errors[0].message.contains("did you mean `threshold`"),
        "{errors:?}"
    );
    assert_eq!(errors[0].line, Some(2));

    let errors = check("thresold = 0.5\n");
    assert!(
        errors[0]
            .message
            .contains("`hook.threshold` or `mcp.threshold`"),
        "{errors:?}"
    );
}

#[test]
fn a_key_nothing_resembles_is_reported_without_a_guess() {
    let errors = check("[embed]\nzzzzzzzz = 1\n");
    assert_eq!(paths(&errors), ["embed.zzzzzzzz"]);
    assert!(!errors[0].message.contains("did you mean"), "{errors:?}");
}

#[test]
fn every_type_error_is_reported_at_its_own_key_not_only_the_first() {
    let errors = check("[hook]\nmax_tokens = \"lots\"\n\n[embed]\nbatch = true\n");
    assert_eq!(paths(&errors), ["hook.max_tokens", "embed.batch"]);
    assert_eq!(errors[0].line, Some(2));
    assert_eq!(errors[1].line, Some(5));
}

#[test]
fn values_outside_the_range_the_code_accepts_are_errors() {
    let errors = check(
        "[hook]\nquality = 7\nthreshold = 1.5\nmax_tokens = 0\n\n[embed]\nprefix_scheme = \"weird\"\nollama_url = \"localhost:11434\"\n\n[memory]\nmin_confidence = 101\n",
    );
    assert_eq!(
        paths(&errors),
        [
            "hook.quality",
            "hook.threshold",
            "hook.max_tokens",
            "embed.ollama_url",
            "embed.prefix_scheme",
            "memory.min_confidence"
        ]
    );
    assert_eq!(errors[1].line, Some(3));
    assert!(check("[hook]\nquality = 4\nthreshold = 1.0\n").is_empty());
}

#[test]
fn sources_must_exist_as_a_file_or_a_directory() {
    let t = tempfile::tempdir().unwrap();
    let file = t.path().join("note.md");
    std::fs::write(&file, "x").unwrap();
    let missing = t.path().join("gone");
    let text = format!(
        "sources = [{:?}, {:?}, {:?}, \"/dev/null\"]\n",
        t.path().display().to_string(),
        file.display().to_string(),
        missing.display().to_string()
    );
    let errors = check(&text);
    assert_eq!(paths(&errors), ["sources", "sources"], "{errors:?}");
    assert!(
        errors[0].message.contains("gone` does not exist"),
        "{errors:?}"
    );
    assert!(
        errors[1]
            .message
            .contains("/dev/null` is not a file or a directory"),
        "{errors:?}"
    );
}

#[test]
fn a_syntax_error_names_its_line() {
    let errors = check("[hook]\nthreshold = = 1\n");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].path, "");
    assert_eq!(errors[0].line, Some(2));
}

#[test]
fn an_update_writes_atomically_and_keeps_the_previous_file_as_bak() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("config.toml");
    std::fs::write(&path, COMMENTED).unwrap();
    let etag = edit::etag_of(Some(COMMENTED));
    let outcome = edit::update(
        &path,
        Some(&etag),
        &set("hook.threshold", serde_json::json!(0.7)),
    )
    .unwrap();
    let Outcome::Written { etag: new_etag } = outcome else {
        panic!("{outcome:?}");
    };
    let written = std::fs::read_to_string(&path).unwrap();
    assert_eq!(new_etag, edit::etag_of(Some(&written)));
    assert!(written.contains("threshold = 0.7  # measured on my vault"));
    assert_eq!(
        std::fs::read_to_string(edit::backup_path(&path)).unwrap(),
        COMMENTED
    );
    let leftovers: Vec<_> = std::fs::read_dir(t.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn a_stale_etag_is_a_conflict_and_writes_nothing() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("config.toml");
    std::fs::write(&path, COMMENTED).unwrap();
    let stale = edit::etag_of(Some("index_transcripts = true\n"));
    let outcome = edit::update(
        &path,
        Some(&stale),
        &set("hook.threshold", serde_json::json!(0.7)),
    )
    .unwrap();
    let Outcome::Conflict { etag } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(etag, edit::etag_of(Some(COMMENTED)));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), COMMENTED);
    assert!(!edit::backup_path(&path).exists());
}

#[test]
fn a_change_that_introduces_an_error_writes_nothing() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("config.toml");
    std::fs::write(&path, COMMENTED).unwrap();
    let outcome =
        edit::update(&path, None, &set("hook.threshold", serde_json::json!(1.5))).unwrap();
    let Outcome::Invalid(errors) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(paths(&errors), ["hook.threshold"]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), COMMENTED);
    assert!(!edit::backup_path(&path).exists());
}

#[test]
fn an_error_already_in_the_file_does_not_block_an_unrelated_change() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("config.toml");
    std::fs::write(&path, "[hook]\nthresold = 0.5\n").unwrap();
    let outcome =
        edit::update(&path, None, &set("mcp.max_tokens", serde_json::json!(3000))).unwrap();
    assert!(matches!(outcome, Outcome::Written { .. }), "{outcome:?}");
}

#[test]
fn an_update_creates_a_missing_file_and_its_directory() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("fresh/br8n/config.toml");
    let outcome = edit::update(
        &path,
        Some(""),
        &set("index_transcripts", serde_json::json!(false)),
    )
    .unwrap();
    assert!(matches!(outcome, Outcome::Written { .. }), "{outcome:?}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "index_transcripts = false\n"
    );
    assert!(!edit::backup_path(&path).exists());
}

#[test]
fn no_config_key_is_mistaken_for_a_secret() {
    let leaves = br8n::config::schema::config_schema().leaf_paths();
    assert!(leaves.len() > 40, "{leaves:?}");
    for leaf in leaves {
        for segment in leaf.split('.') {
            assert!(!br8n::config::view::looks_secret(segment), "{leaf}");
        }
    }
    assert!(br8n::config::view::looks_secret("token"));
    assert!(br8n::config::view::looks_secret("aws_secret_access_key"));
}

#[test]
fn env_file_entries_are_written_at_0600_and_other_lines_survive() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let env = t.path().join("env");
    std::fs::write(&env, "# mine\nOTHER=1\nBR8N_EMBED_TOKEN=old\n").unwrap();
    br8n::env_file::write_entries(
        &env,
        &[
            ("BR8N_EMBED_URL", Some("http://h:1".into())),
            ("BR8N_EMBED_TOKEN", Some("new".into())),
        ],
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&env).unwrap(),
        "# mine\nOTHER=1\nBR8N_EMBED_TOKEN=new\nBR8N_EMBED_URL=http://h:1\n"
    );
    assert_eq!(
        std::fs::metadata(&env).unwrap().permissions().mode() & 0o777,
        0o600
    );
    br8n::env_file::write_entries(&env, &[("BR8N_EMBED_TOKEN", None)]).unwrap();
    assert_eq!(
        std::fs::read_to_string(&env).unwrap(),
        "# mine\nOTHER=1\nBR8N_EMBED_URL=http://h:1\n"
    );
    assert!(
        br8n::env_file::write_entries(&env, &[("BR8N_EMBED_TOKEN", Some("a\nb".into()))]).is_err()
    );
}

fn br8n(tmp: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("br8n").unwrap();
    c.env("BR8N_DB", tmp.join("db"));
    c.env("BR8N_CONFIG", tmp.join("config.toml"));
    c.env("PATH", "/usr/bin:/bin");
    c
}

#[test]
fn config_set_then_get_round_trips_through_the_file() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("config.toml");
    std::fs::write(&path, COMMENTED).unwrap();
    br8n(t.path())
        .args(["config", "set", "hook.threshold", "0.72"])
        .assert()
        .success();
    br8n(t.path())
        .args(["config", "set", "embed.model", "nomic-embed-text"])
        .assert()
        .success();
    br8n(t.path())
        .args(["config", "get", "hook.threshold"])
        .assert()
        .success()
        .stdout("0.72\n");
    br8n(t.path())
        .args(["config", "get", "embed.model"])
        .assert()
        .success()
        .stdout("nomic-embed-text\n");
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(
        written
            .contains("# the hook gate, tuned by hand\nthreshold = 0.72  # measured on my vault"),
        "{written}"
    );
    br8n(t.path())
        .args(["config", "path"])
        .assert()
        .success()
        .stdout(format!("{}\n", path.display()));
}

#[test]
fn config_set_refuses_a_bad_value_and_config_check_reports_it() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("config.toml");
    std::fs::write(&path, COMMENTED).unwrap();
    br8n(t.path())
        .args(["config", "set", "hook.quality", "9"])
        .assert()
        .failure()
        .stderr(contains("hook.quality: must be between 0 and 4"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), COMMENTED);
    br8n(t.path()).args(["config", "check"]).assert().success();
    std::fs::write(&path, "[hook]\nthresold = 0.5\n").unwrap();
    br8n(t.path())
        .args(["config", "check"])
        .assert()
        .failure()
        .stdout(contains("did you mean `threshold`"));
    br8n(t.path())
        .arg("status")
        .assert()
        .success()
        .stdout(contains("1 problem(s)"))
        .stdout(contains("hook.thresold"));
}
