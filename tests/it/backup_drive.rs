use br8n::backup::remote::drive::{
    explain_auth_error, keys_under, quoted_query_literal, DriveRemote,
};
use br8n::backup::remote::Remote;
use br8n::config::DriveConfig;

const CLIENT_SECRET: &str = r#"{"installed":{"client_id":"x","client_secret":"y","auth_uri":"https://accounts.google.com/o/oauth2/auth","token_uri":"https://oauth2.googleapis.com/token","redirect_uris":["http://localhost"]}}"#;

fn configured(dir: &std::path::Path, folder_id: &str) -> DriveConfig {
    std::fs::write(dir.join("client.json"), CLIENT_SECRET).unwrap();
    DriveConfig {
        folder_id: folder_id.into(),
        client_secret_file: dir.join("client.json"),
        token_file: None,
    }
}

#[test]
fn a_missing_token_file_fails_with_an_actionable_message() {
    let dir = tempfile::tempdir().unwrap();
    let err = DriveRemote::new(
        &configured(dir.path(), "1AbC"),
        &dir.path().join("token.json"),
    )
    .err()
    .expect("no token must refuse")
    .to_string();
    assert!(
        err.contains("br8n backup auth drive"),
        "must name the fix: {err}"
    );
}

#[test]
fn an_empty_folder_id_names_the_command_that_creates_one() {
    let dir = tempfile::tempdir().unwrap();
    let token = dir.path().join("token.json");
    std::fs::write(&token, "[]").unwrap();
    let err = DriveRemote::new(&configured(dir.path(), ""), &token)
        .err()
        .expect("an empty folder id must refuse")
        .to_string();
    assert!(err.contains("folder_id"), "got: {err}");
    assert!(err.contains("br8n backup auth drive"), "got: {err}");
}

#[test]
fn a_missing_client_secret_says_where_to_download_one() {
    let dir = tempfile::tempdir().unwrap();
    let token = dir.path().join("token.json");
    std::fs::write(&token, "[]").unwrap();
    let cfg = DriveConfig {
        folder_id: "1AbC".into(),
        client_secret_file: dir.path().join("absent.json"),
        token_file: None,
    };
    let err = DriveRemote::new(&cfg, &token)
        .err()
        .expect("no secret must refuse")
        .to_string();
    assert!(err.contains("Google Cloud console"), "got: {err}");
}

#[test]
fn a_configured_remote_builds_offline_alongside_the_s3_client() {
    let dir = tempfile::tempdir().unwrap();
    let token = dir.path().join("token.json");
    std::fs::write(&token, "[]").unwrap();
    let s3 = br8n::backup::remote::s3::S3Remote::new(&br8n::config::S3Config {
        bucket: "b".into(),
        region: "eu-west-1".into(),
        prefix: "br8n/".into(),
        profile: "default".into(),
        storage_class: "STANDARD".into(),
    })
    .unwrap();
    let drive = DriveRemote::new(&configured(dir.path(), "1AbC"), &token).unwrap();
    assert_eq!(s3.name(), "s3");
    assert_eq!(drive.name(), "drive");
}

#[test]
fn an_expired_refresh_token_names_the_testing_mode_trap() {
    let msg = explain_auth_error("invalid_grant: Token expired");
    assert!(msg.contains("Testing"), "got: {msg}");
    assert!(msg.contains("In production"), "got: {msg}");
    assert!(msg.contains("br8n backup auth drive"), "got: {msg}");
}

#[test]
fn an_unrelated_auth_error_is_passed_through_unchanged() {
    assert_eq!(
        explain_auth_error("connection refused"),
        "connection refused"
    );
}

#[test]
fn query_literals_escape_quotes_and_backslashes() {
    assert_eq!(quoted_query_literal("blobs/abc"), "'blobs/abc'");
    assert_eq!(quoted_query_literal("it's"), r"'it\'s'");
    assert_eq!(quoted_query_literal(r"a\'b"), r"'a\\\'b'");
}

#[test]
fn listing_keeps_only_names_that_start_with_the_prefix() {
    let names = vec![
        ("blobs/a".to_string(), 1),
        ("index/blobs/b.tar.gz".to_string(), 2),
        ("manifest/latest.json".to_string(), 3),
        ("blobs/c".to_string(), 4),
    ];
    let keys: Vec<String> = keys_under(names, "blobs/")
        .into_iter()
        .map(|o| o.key)
        .collect();
    assert_eq!(keys, vec!["blobs/a", "blobs/c"]);
}

#[test]
#[ignore]
fn a_real_folder_round_trips() {
    let folder = std::env::var("BR8N_TEST_DRIVE_FOLDER").expect("set BR8N_TEST_DRIVE_FOLDER");
    let secret = std::env::var("BR8N_TEST_DRIVE_SECRET").expect("set BR8N_TEST_DRIVE_SECRET");
    let token = std::env::var("BR8N_TEST_DRIVE_TOKEN").expect("set BR8N_TEST_DRIVE_TOKEN");
    let cfg = DriveConfig {
        folder_id: folder,
        client_secret_file: secret.into(),
        token_file: None,
    };
    let r = DriveRemote::new(&cfg, std::path::Path::new(&token)).unwrap();
    r.check().unwrap();
    r.put_bytes("blobs/roundtrip", b"hello").unwrap();
    assert_eq!(r.get_bytes("blobs/roundtrip").unwrap(), b"hello");
    r.put_bytes("blobs/roundtrip", b"goodbye").unwrap();
    assert_eq!(r.get_bytes("blobs/roundtrip").unwrap(), b"goodbye");
    assert_eq!(
        r.list("blobs/")
            .unwrap()
            .iter()
            .filter(|o| o.key == "blobs/roundtrip")
            .count(),
        1,
        "re-putting a key must update, not duplicate"
    );
    r.delete("blobs/roundtrip").unwrap();
    assert!(r.get_bytes("blobs/roundtrip").is_err());
}

#[test]
fn an_unattended_run_that_needs_consent_fails_instead_of_waiting_for_a_browser() {
    let dir = tempfile::tempdir().unwrap();
    let token = dir.path().join("token.json");
    std::fs::write(&token, "[]").unwrap();
    let cfg = configured(dir.path(), "1AbC");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let remote = DriveRemote::new(&cfg, &token).unwrap();
        let _ = tx.send(remote.list("blobs/").map(|_| ()).map_err(|e| e.to_string()));
    });
    let outcome = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the run blocked waiting for interactive consent");
    let err = outcome.expect_err("a run with no usable token must fail");
    assert!(err.contains("br8n backup auth drive"), "got: {err}");
}
