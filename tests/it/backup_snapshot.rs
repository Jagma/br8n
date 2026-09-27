use crate::common;

use br8n::backup::snapshot::{archive_index, collect, copy_index_under_lock};
use br8n::config::Config;
use br8n::index::IndexLock;
use sha2::{Digest, Sha256};

fn seed(dir: &std::path::Path, golden: bool) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("config.toml"), "sources = []\n").unwrap();
    if golden {
        std::fs::write(dir.join("golden.toml"), "[[case]]\n").unwrap();
    }
}

#[test]
fn collection_is_the_config_and_the_golden_set_and_nothing_else() {
    let _lock = common::lock_env_vars();
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    seed(&data, true);
    std::fs::write(data.join("backup.key"), "aa").unwrap();
    std::fs::write(data.join("drive-token.json"), "{}").unwrap();
    std::fs::write(data.join("db.lock"), "123").unwrap();
    std::fs::create_dir_all(data.join("db.new")).unwrap();
    std::fs::write(data.join("db.new/graph.kz"), "partial").unwrap();

    let _cfg_guard = common::EnvVarGuard::set("BR8N_CONFIG", data.join("config.toml"));
    let _db_guard = common::EnvVarGuard::set("BR8N_DB", data.join("db"));

    let items = collect(&Config::default()).unwrap();
    let paths: Vec<&str> = items.iter().map(|i| i.rel.as_str()).collect();
    assert_eq!(paths, vec!["config/config.toml", "config/golden.toml"]);
}

#[test]
fn the_hash_is_the_sha256_of_the_file_contents() {
    let _lock = common::lock_env_vars();
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    seed(&data, false);
    let _cfg_guard = common::EnvVarGuard::set("BR8N_CONFIG", data.join("config.toml"));
    let _db_guard = common::EnvVarGuard::set("BR8N_DB", data.join("db"));

    let items = collect(&Config::default()).unwrap();
    let want = hex::encode(Sha256::digest(b"sources = []\n"));
    assert_eq!(items[0].hash, want);
    assert_eq!(items[0].size, 13);
}

#[test]
fn an_absent_golden_set_is_skipped_rather_than_failing() {
    let _lock = common::lock_env_vars();
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    seed(&data, false);
    let _cfg_guard = common::EnvVarGuard::set("BR8N_CONFIG", data.join("config.toml"));
    let _db_guard = common::EnvVarGuard::set("BR8N_DB", data.join("db"));

    let items = collect(&Config::default()).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].rel, "config/config.toml");
}

#[test]
fn a_missing_config_is_an_error_not_a_silently_short_backup() {
    let _lock = common::lock_env_vars();
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let _cfg_guard = common::EnvVarGuard::set("BR8N_CONFIG", data.join("config.toml"));
    let _db_guard = common::EnvVarGuard::set("BR8N_DB", data.join("db"));

    let err = collect(&Config::default()).unwrap_err().to_string();
    assert!(
        err.contains("config.toml"),
        "must name the path, got: {err}"
    );
}

#[test]
fn collection_is_deterministic_across_runs() {
    let _lock = common::lock_env_vars();
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    seed(&data, true);
    let _cfg_guard = common::EnvVarGuard::set("BR8N_CONFIG", data.join("config.toml"));
    let _db_guard = common::EnvVarGuard::set("BR8N_DB", data.join("db"));

    let a: Vec<String> = collect(&Config::default())
        .unwrap()
        .into_iter()
        .map(|i| i.rel)
        .collect();
    let b: Vec<String> = collect(&Config::default())
        .unwrap()
        .into_iter()
        .map(|i| i.rel)
        .collect();
    assert_eq!(a, b);
}

#[test]
fn the_index_is_archived_and_hashed() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    std::fs::create_dir_all(&db).unwrap();
    std::fs::write(db.join("graph.kz"), vec![7u8; 200_000]).unwrap();
    std::fs::write(db.join("graph.kz.wal"), b"wal").unwrap();

    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let a = archive_index(&db, &work).unwrap().unwrap();

    assert!(a.path.exists());
    assert_eq!(a.hash.len(), 64);
    assert!(a.size > 0);
    assert!(
        a.size < 200_000,
        "gzip must actually compress a repetitive 200 KB file, got {}",
        a.size
    );

    let b = archive_index(&db, &work).unwrap().unwrap();
    assert_eq!(a.hash, b.hash);

    let leftovers: Vec<String> = std::fs::read_dir(&work)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| !n.ends_with(".tar.gz"))
        .collect();
    assert!(leftovers.is_empty(), "staging left behind: {leftovers:?}");
}

#[test]
fn a_missing_database_archives_to_nothing_rather_than_failing() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    assert!(archive_index(&dir.path().join("db"), &work)
        .unwrap()
        .is_none());
}

#[test]
fn a_held_index_lock_stops_the_archive() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    std::fs::create_dir_all(&db).unwrap();
    std::fs::write(db.join("graph.kz"), b"live").unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();

    let held = br8n::index::IndexLock::acquire(&db).expect("lock is free");
    let err = archive_index(&db, &work).unwrap_err().to_string();
    assert!(err.contains("index"), "must name the cause: {err}");
    drop(held);

    assert!(
        archive_index(&db, &work).unwrap().is_some(),
        "lock released"
    );
}

#[test]
fn copying_the_index_releases_the_lock_before_returning() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    std::fs::create_dir_all(&db).unwrap();
    std::fs::write(db.join("graph.kz"), b"live").unwrap();
    let staging = dir.path().join("staging");

    copy_index_under_lock(&db, &staging).unwrap();

    assert!(!IndexLock::is_held(&db));
    assert!(staging.join("graph.kz").exists());
}
