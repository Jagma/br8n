use crate::common;

use br8n::backup::crypto::Key;
use br8n::backup::manifest::{blob_key, index_key, LatestPointer, Manifest};
use br8n::backup::remote::{FakeRemote, Remote};
use br8n::backup::snapshot::unpack_index;
use br8n::backup::{back_up, BackupLock};
use br8n::config::Config;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

struct RemoteThatRewritesAFileOnFirstPut {
    inner: FakeRemote,
    file: PathBuf,
    new_contents: &'static [u8],
    rewritten: AtomicBool,
}

impl Remote for RemoteThatRewritesAFileOnFirstPut {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn put_file(&self, key: &str, path: &Path) -> anyhow::Result<()> {
        self.inner.put_file(key, path)
    }
    fn put_bytes(&self, key: &str, bytes: &[u8]) -> anyhow::Result<()> {
        if !self.rewritten.swap(true, Ordering::SeqCst) {
            std::fs::write(&self.file, self.new_contents).unwrap();
        }
        self.inner.put_bytes(key, bytes)
    }
    fn get_file(&self, key: &str, dest: &Path) -> anyhow::Result<()> {
        self.inner.get_file(key, dest)
    }
    fn get_bytes(&self, key: &str) -> anyhow::Result<Vec<u8>> {
        self.inner.get_bytes(key)
    }
    fn list(&self, prefix: &str) -> anyhow::Result<Vec<br8n::backup::remote::RemoteObject>> {
        self.inner.list(prefix)
    }
    fn delete(&self, key: &str) -> anyhow::Result<()> {
        self.inner.delete(key)
    }
}

#[test]
fn a_backup_uploads_once_dedupes_thereafter_and_records_a_generation() {
    let _lock = common::lock_env_vars();
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("config.toml"), "sources = []\n").unwrap();
    std::fs::write(data.join("golden.toml"), "[[case]]\n").unwrap();

    let _cfg_guard = common::EnvVarGuard::set("BR8N_CONFIG", data.join("config.toml"));
    let _db_guard = common::EnvVarGuard::set("BR8N_DB", data.join("db"));

    let cfg = Config {
        index_transcripts: false,
        backup: br8n::config::BackupConfig {
            include_index: false,
            ..Default::default()
        },
        ..Config::default()
    };
    let key = Key::generate();
    let remote = FakeRemote::new();

    let first = back_up(&cfg, &remote, Some(&key)).unwrap();
    assert_eq!(first.uploaded, 2, "config and golden");
    assert_eq!(first.deduped, 0);
    assert!(first.bytes > 0);

    let keys = remote.keys();
    assert!(keys.iter().any(|k| k.starts_with("blobs/")));
    assert!(keys.iter().any(|k| k == &first.generation));
    assert!(keys.iter().any(|k| k == Manifest::latest_key()));

    let ptr: LatestPointer =
        serde_json::from_slice(&remote.get_bytes(Manifest::latest_key()).unwrap()).unwrap();
    assert_eq!(ptr.generation, first.generation);

    let manifest: Manifest =
        serde_json::from_slice(&remote.get_bytes(&first.generation).unwrap()).unwrap();
    let golden = manifest
        .files
        .iter()
        .find(|f| f.path.ends_with("golden.toml"))
        .unwrap();
    let key_id = key.id();
    let stored = remote
        .get_bytes(&blob_key(Some(key_id.as_str()), &golden.hash))
        .unwrap();
    assert_ne!(stored, b"[[case]]\n".to_vec(), "must not be plaintext");
    assert_eq!(
        br8n::backup::crypto::open_bytes(&key, &golden.hash, &stored).unwrap(),
        b"[[case]]\n".to_vec()
    );

    let second = back_up(&cfg, &remote, Some(&key)).unwrap();
    assert_eq!(
        second.uploaded, 0,
        "an unchanged corpus must re-upload nothing"
    );
    assert_eq!(second.deduped, 2);
    assert_ne!(
        second.generation, first.generation,
        "still a new generation"
    );

    let second_manifest: Manifest =
        serde_json::from_slice(&remote.get_bytes(&second.generation).unwrap()).unwrap();
    assert_eq!(
        second_manifest.files.len(),
        2,
        "a deduped run must still record every file, not an empty manifest"
    );
    let mut first_hashes: Vec<&str> = manifest.files.iter().map(|f| f.hash.as_str()).collect();
    let mut second_hashes: Vec<&str> = second_manifest
        .files
        .iter()
        .map(|f| f.hash.as_str())
        .collect();
    first_hashes.sort();
    second_hashes.sort();
    assert_eq!(
        first_hashes, second_hashes,
        "the same hashes as the first run"
    );

    std::fs::write(data.join("golden.toml"), "[[case]]\nname = \"x\"\n").unwrap();
    let third = back_up(&cfg, &remote, Some(&key)).unwrap();
    assert_eq!(third.uploaded, 1);
    assert_eq!(third.deduped, 1);

    let plain_remote = FakeRemote::new();
    let plain = back_up(&cfg, &plain_remote, None).unwrap();
    let m: Manifest =
        serde_json::from_slice(&plain_remote.get_bytes(&plain.generation).unwrap()).unwrap();
    assert!(m.encryption.is_none());
    let plain_golden = m
        .files
        .iter()
        .find(|f| f.path.ends_with("golden.toml"))
        .unwrap();
    let plain_stored = plain_remote
        .get_bytes(&blob_key(None, &plain_golden.hash))
        .unwrap();
    assert_eq!(
        plain_stored,
        std::fs::read(data.join("golden.toml")).unwrap(),
        "unencrypted backups must store the plaintext, byte for byte"
    );
}

#[test]
fn rotating_the_key_reuploads_instead_of_wrongly_deduping() {
    let _lock = common::lock_env_vars();
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("config.toml"), "sources = []\n").unwrap();

    let _cfg_guard = common::EnvVarGuard::set("BR8N_CONFIG", data.join("config.toml"));
    let _db_guard = common::EnvVarGuard::set("BR8N_DB", data.join("db"));

    let cfg = Config {
        index_transcripts: false,
        backup: br8n::config::BackupConfig {
            include_index: false,
            ..Default::default()
        },
        ..Config::default()
    };
    let remote = FakeRemote::new();
    let key_a = Key::generate();
    let key_b = Key::generate();

    let first = back_up(&cfg, &remote, Some(&key_a)).unwrap();
    assert_eq!(first.uploaded, 1);
    assert_eq!(first.deduped, 0);

    let second = back_up(&cfg, &remote, Some(&key_b)).unwrap();
    assert_eq!(
        second.uploaded, 1,
        "a new key seals different bytes and must not dedupe against a blob sealed under the old key"
    );
    assert_eq!(second.deduped, 0);

    let manifest_a: Manifest =
        serde_json::from_slice(&remote.get_bytes(&first.generation).unwrap()).unwrap();
    let manifest_b: Manifest =
        serde_json::from_slice(&remote.get_bytes(&second.generation).unwrap()).unwrap();
    let hash_a = manifest_a.files[0].hash.clone();
    let hash_b = manifest_b.files[0].hash.clone();
    assert_eq!(
        hash_a, hash_b,
        "the plaintext content, and so its hash, is unchanged"
    );

    let id_a = key_a.id();
    let id_b = key_b.id();
    let object_a = blob_key(Some(id_a.as_str()), &hash_a);
    let object_b = blob_key(Some(id_b.as_str()), &hash_b);
    assert_ne!(
        object_a, object_b,
        "blobs sealed under different keys must live under different prefixes"
    );
    let keys = remote.keys();
    assert!(
        keys.contains(&object_a),
        "the blob sealed under key A must still exist"
    );
    assert!(
        keys.contains(&object_b),
        "the blob sealed under key B must have been uploaded"
    );
}

#[test]
fn exactly_two_list_calls_happen_per_run() {
    let _lock = common::lock_env_vars();
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("config.toml"), "sources = []\n").unwrap();

    let _cfg_guard = common::EnvVarGuard::set("BR8N_CONFIG", data.join("config.toml"));
    let _db_guard = common::EnvVarGuard::set("BR8N_DB", data.join("db"));

    let cfg = Config {
        index_transcripts: false,
        backup: br8n::config::BackupConfig {
            include_index: false,
            ..Default::default()
        },
        ..Config::default()
    };

    let ok_remote = FakeRemote::new();
    ok_remote.fail_list_after(2);
    assert!(
        back_up(&cfg, &ok_remote, None).is_ok(),
        "exactly two list calls happen, for blobs/ and index/, and both must be allowed"
    );

    let fail_remote = FakeRemote::new();
    fail_remote.fail_list_after(1);
    assert!(
        back_up(&cfg, &fail_remote, None).is_err(),
        "a run that needs a third list call was never going to happen; this pins the count at two"
    );
}

#[test]
fn an_index_is_uploaded_and_recorded_in_the_manifest() {
    let _lock = common::lock_env_vars();
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("config.toml"), "sources = []\n").unwrap();

    let db = data.join("db");
    std::fs::create_dir_all(&db).unwrap();
    std::fs::write(db.join("shard.dat"), b"a small fake index shard").unwrap();
    std::fs::write(db.join("shard.dat.wal"), b"wal").unwrap();

    let _cfg_guard = common::EnvVarGuard::set("BR8N_CONFIG", data.join("config.toml"));
    let _db_guard = common::EnvVarGuard::set("BR8N_DB", &db);

    let cfg = Config {
        index_transcripts: false,
        backup: br8n::config::BackupConfig {
            include_index: true,
            ..Default::default()
        },
        ..Config::default()
    };
    let key = Key::generate();
    let remote = FakeRemote::new();

    let stats = back_up(&cfg, &remote, Some(&key)).unwrap();
    assert_eq!(stats.uploaded, 2, "the config file and the index archive");
    assert_eq!(stats.deduped, 0);

    let manifest: Manifest =
        serde_json::from_slice(&remote.get_bytes(&stats.generation).unwrap()).unwrap();
    let index = manifest.index.expect("an index entry must be recorded");
    let object = index_key(Some(key.id().as_str()), &index.hash);
    assert!(
        remote.keys().contains(&object),
        "the index blob must be uploaded under the key-namespaced object key"
    );

    assert!(
        !db.with_extension("backup.staging").exists(),
        "staging must be cleaned up after a successful run"
    );
}

#[test]
fn an_unchanged_index_dedupes_on_the_second_run() {
    let _lock = common::lock_env_vars();
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("config.toml"), "sources = []\n").unwrap();

    let db = data.join("db");
    std::fs::create_dir_all(&db).unwrap();
    std::fs::write(db.join("shard.dat"), b"a small fake index shard").unwrap();

    let _cfg_guard = common::EnvVarGuard::set("BR8N_CONFIG", data.join("config.toml"));
    let _db_guard = common::EnvVarGuard::set("BR8N_DB", &db);

    let cfg = Config {
        index_transcripts: false,
        backup: br8n::config::BackupConfig {
            include_index: true,
            ..Default::default()
        },
        ..Config::default()
    };
    let key = Key::generate();
    let remote = FakeRemote::new();

    let first = back_up(&cfg, &remote, Some(&key)).unwrap();
    assert_eq!(first.uploaded, 2);
    assert_eq!(first.deduped, 0);

    let second = back_up(&cfg, &remote, Some(&key)).unwrap();
    assert_eq!(
        second.uploaded, 0,
        "an unchanged config file and index must both dedupe"
    );
    assert_eq!(second.deduped, 2);

    let manifest: Manifest =
        serde_json::from_slice(&remote.get_bytes(&second.generation).unwrap()).unwrap();
    assert!(
        manifest.index.is_some(),
        "a deduped index must still be recorded in the manifest, not silently dropped"
    );
}

#[test]
fn the_uploaded_index_blob_decrypts_to_the_original_archive_contents() {
    let _lock = common::lock_env_vars();
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("config.toml"), "sources = []\n").unwrap();

    let db = data.join("db");
    std::fs::create_dir_all(&db).unwrap();
    let shard_contents = b"a rather particular sequence of index bytes";
    std::fs::write(db.join("shard.dat"), shard_contents).unwrap();

    let _cfg_guard = common::EnvVarGuard::set("BR8N_CONFIG", data.join("config.toml"));
    let _db_guard = common::EnvVarGuard::set("BR8N_DB", &db);

    let cfg = Config {
        index_transcripts: false,
        backup: br8n::config::BackupConfig {
            include_index: true,
            ..Default::default()
        },
        ..Config::default()
    };
    let key = Key::generate();
    let remote = FakeRemote::new();

    let stats = back_up(&cfg, &remote, Some(&key)).unwrap();
    let manifest: Manifest =
        serde_json::from_slice(&remote.get_bytes(&stats.generation).unwrap()).unwrap();
    let index = manifest.index.unwrap();
    let object = index_key(Some(key.id().as_str()), &index.hash);
    let sealed = remote.get_bytes(&object).unwrap();

    let plain = br8n::backup::crypto::open_bytes(&key, &index.hash, &sealed).unwrap();

    let workdir = tempfile::tempdir().unwrap();
    let archive_path = workdir.path().join("index.tar.gz");
    std::fs::write(&archive_path, &plain).unwrap();
    let unpacked = workdir.path().join("unpacked");
    unpack_index(&archive_path, &unpacked).unwrap();

    assert_eq!(
        std::fs::read(unpacked.join("shard.dat")).unwrap(),
        shard_contents.to_vec(),
        "the decrypted, ungzipped, untarred archive must match the original index shard"
    );
}

#[test]
fn a_put_failure_between_the_manifest_and_the_latest_pointer_leaves_no_dangling_pointer() {
    let _lock = common::lock_env_vars();
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("config.toml"), "sources = []\n").unwrap();

    let db = data.join("db");
    std::fs::create_dir_all(&db).unwrap();
    std::fs::write(db.join("shard.dat"), b"index-bytes").unwrap();

    let _cfg_guard = common::EnvVarGuard::set("BR8N_CONFIG", data.join("config.toml"));
    let _db_guard = common::EnvVarGuard::set("BR8N_DB", &db);

    let cfg = Config {
        index_transcripts: false,
        backup: br8n::config::BackupConfig {
            include_index: true,
            ..Default::default()
        },
        ..Config::default()
    };
    let key = Key::generate();
    let remote = FakeRemote::new();
    remote.fail_put_after(3);

    assert!(
        back_up(&cfg, &remote, Some(&key)).is_err(),
        "the write of the latest pointer must fail"
    );

    let keys = remote.keys();
    assert!(
        keys.iter()
            .any(|k| k.starts_with("manifest/") && k != Manifest::latest_key()),
        "the manifest itself must have been written before the failure: {keys:?}"
    );
    assert!(
        !keys.iter().any(|k| k == Manifest::latest_key()),
        "latest.json must not point at a generation whose blobs might be missing"
    );
    assert!(
        !db.with_extension("backup.staging").exists(),
        "the staging directory must not survive a failed run"
    );
}

#[test]
fn the_backup_lock_excludes_a_second_run() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    std::fs::create_dir_all(&db).unwrap();

    let held = BackupLock::acquire(&db).expect("first acquire succeeds");
    assert!(BackupLock::is_held(&db));
    assert!(BackupLock::acquire(&db).is_none(), "second must be refused");
    drop(held);
    assert!(!BackupLock::is_held(&db));
    assert!(BackupLock::acquire(&db).is_some(), "released");
}

#[test]
fn a_stale_backup_lock_from_a_dead_process_is_reclaimed() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    std::fs::create_dir_all(&db).unwrap();
    std::fs::write(db.with_extension("backup.lock"), "999999").unwrap();
    assert!(
        BackupLock::acquire(&db).is_some(),
        "a crashed run must not block backups forever"
    );
}

#[test]
fn the_backup_lock_is_separate_from_the_index_lock() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    std::fs::create_dir_all(&db).unwrap();
    let _backup = BackupLock::acquire(&db).unwrap();
    assert!(
        br8n::index::IndexLock::acquire(&db).is_some(),
        "holding the backup lock must not hold the index lock"
    );
}

#[test]
fn a_file_rewritten_mid_run_is_sealed_as_the_bytes_that_were_hashed() {
    let _lock = common::lock_env_vars();
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("config.toml"), "sources = []\n").unwrap();
    let golden_path = data.join("golden.toml");

    let _cfg_guard = common::EnvVarGuard::set("BR8N_CONFIG", data.join("config.toml"));
    let _db_guard = common::EnvVarGuard::set("BR8N_DB", data.join("db"));

    let cfg = Config {
        index_transcripts: false,
        backup: br8n::config::BackupConfig {
            include_index: false,
            ..Default::default()
        },
        ..Config::default()
    };
    let key = Key::generate();

    for encrypted in [true, false] {
        std::fs::write(&golden_path, "[[case]]\n").unwrap();
        let remote = RemoteThatRewritesAFileOnFirstPut {
            inner: FakeRemote::new(),
            file: golden_path.clone(),
            new_contents: b"[[case]]\nname = \"written by another process\"\n",
            rewritten: AtomicBool::new(false),
        };
        let used_key = encrypted.then_some(&key);

        let stats = back_up(&cfg, &remote, used_key).unwrap();
        assert!(
            remote.rewritten.load(Ordering::SeqCst),
            "the golden set must be rewritten after collection and before it is sealed"
        );

        let manifest: Manifest =
            serde_json::from_slice(&remote.get_bytes(&stats.generation).unwrap()).unwrap();
        let golden = manifest
            .files
            .iter()
            .find(|f| f.path.ends_with("golden.toml"))
            .unwrap();
        let key_id = used_key.map(|k| k.id());
        let stored = remote
            .get_bytes(&blob_key(key_id.as_deref(), &golden.hash))
            .unwrap();
        let plain = match used_key {
            Some(k) => br8n::backup::crypto::open_bytes(k, &golden.hash, &stored).unwrap(),
            None => stored,
        };
        assert_eq!(
            hex::encode(Sha256::digest(&plain)),
            golden.hash,
            "the blob must hold the bytes its hash names (encrypted: {encrypted})"
        );
        assert_eq!(plain, b"[[case]]\n".to_vec());
        assert_eq!(golden.size, plain.len() as u64);
    }
}

#[test]
fn rotating_the_key_reuploads_the_index_instead_of_wrongly_deduping() {
    let _lock = common::lock_env_vars();
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("config.toml"), "sources = []\n").unwrap();

    let db = data.join("db");
    std::fs::create_dir_all(&db).unwrap();
    std::fs::write(db.join("shard.dat"), b"index bytes that do not change").unwrap();

    let _cfg_guard = common::EnvVarGuard::set("BR8N_CONFIG", data.join("config.toml"));
    let _db_guard = common::EnvVarGuard::set("BR8N_DB", &db);

    let cfg = Config {
        index_transcripts: false,
        backup: br8n::config::BackupConfig {
            include_index: true,
            ..Default::default()
        },
        ..Config::default()
    };
    let remote = FakeRemote::new();
    let key_a = Key::generate();
    let key_b = Key::generate();

    back_up(&cfg, &remote, Some(&key_a)).unwrap();
    let second = back_up(&cfg, &remote, Some(&key_b)).unwrap();
    assert_eq!(
        second.uploaded, 2,
        "the config file and the index must both be resealed under the new key"
    );
    assert_eq!(second.deduped, 0);

    let manifest: Manifest =
        serde_json::from_slice(&remote.get_bytes(&second.generation).unwrap()).unwrap();
    let index = manifest.index.unwrap();
    let sealed = remote
        .get_bytes(&index_key(Some(key_b.id().as_str()), &index.hash))
        .unwrap();
    assert!(
        br8n::backup::crypto::open_bytes(&key_b, &index.hash, &sealed).is_ok(),
        "the index the new manifest names must open under the new key"
    );
}

fn one_target_setup() -> (
    tempfile::TempDir,
    common::EnvVarGuard,
    common::EnvVarGuard,
    Config,
) {
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("config.toml"), "sources = []\n").unwrap();
    let cfg_guard = common::EnvVarGuard::set("BR8N_CONFIG", data.join("config.toml"));
    let db_guard = common::EnvVarGuard::set("BR8N_DB", data.join("db"));
    let cfg = Config {
        index_transcripts: false,
        backup: br8n::config::BackupConfig {
            include_index: false,
            ..Default::default()
        },
        ..Config::default()
    };
    (home, cfg_guard, db_guard, cfg)
}

#[test]
fn a_target_that_cannot_be_built_does_not_cost_the_other_its_backup() {
    let _lock = common::lock_env_vars();
    let (_home, _c, _d, cfg) = one_target_setup();
    let fake = std::sync::Arc::new(FakeRemote::new());
    let remotes: Vec<(String, anyhow::Result<Box<dyn Remote>>)> = vec![
        ("drive".into(), Err(anyhow::anyhow!("not yet authorized"))),
        ("s3".into(), Ok(Box::new(SharedRemote(fake.clone())))),
    ];

    let outcome = br8n::backup::run_targets(&cfg, None, remotes);
    let br8n::backup::RunOutcome::Failed(why) = outcome else {
        panic!("a failed target must fail the run, got {outcome:?}");
    };
    assert!(why.contains("1 of 2"), "got: {why}");
    assert!(why.contains("drive: not yet authorized"), "got: {why}");
    assert!(
        fake.keys().iter().any(|k| k.starts_with("manifest/2")),
        "the healthy target must still receive a generation"
    );
    assert!(
        br8n::backup::read_stamp(&Config::db_path()).is_none(),
        "a run that left a target behind must not look fresh in `br8n status`"
    );
}

#[test]
fn a_run_where_every_target_succeeds_writes_the_stamp() {
    let _lock = common::lock_env_vars();
    let (_home, _c, _d, cfg) = one_target_setup();
    let remotes: Vec<(String, anyhow::Result<Box<dyn Remote>>)> =
        vec![("s3".into(), Ok(Box::new(FakeRemote::new())))];

    let outcome = br8n::backup::run_targets(&cfg, None, remotes);
    assert!(
        matches!(outcome, br8n::backup::RunOutcome::Done(ref t) if t.len() == 1),
        "got {outcome:?}"
    );
    assert!(br8n::backup::read_stamp(&Config::db_path()).is_some());
}

#[test]
fn a_prune_failure_is_reported_but_the_backup_still_counts() {
    let _lock = common::lock_env_vars();
    let (_home, _c, _d, cfg) = one_target_setup();
    let fake = std::sync::Arc::new(FakeRemote::new());
    fake.fail_list_after(2);
    let remotes: Vec<(String, anyhow::Result<Box<dyn Remote>>)> =
        vec![("s3".into(), Ok(Box::new(SharedRemote(fake.clone()))))];

    let outcome = br8n::backup::run_targets(&cfg, None, remotes);
    let br8n::backup::RunOutcome::Failed(why) = outcome else {
        panic!("a prune failure must be visible, got {outcome:?}");
    };
    assert!(why.contains("pruning failed"), "got: {why}");
    assert!(fake.keys().iter().any(|k| k.starts_with("manifest/2")));
    assert!(br8n::backup::read_stamp(&Config::db_path()).is_some());
}

#[test]
fn run_all_prunes_to_the_configured_budget() {
    let _lock = common::lock_env_vars();
    let (_home, _c, _d, mut cfg) = one_target_setup();
    cfg.backup.keep_generations = 2;
    let fake = std::sync::Arc::new(FakeRemote::new());
    for _ in 0..4 {
        let remotes: Vec<(String, anyhow::Result<Box<dyn Remote>>)> =
            vec![("s3".into(), Ok(Box::new(SharedRemote(fake.clone()))))];
        br8n::backup::run_targets(&cfg, None, remotes);
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
    let generations = fake
        .keys()
        .into_iter()
        .filter(|k| k.starts_with("manifest/") && k != Manifest::latest_key())
        .count();
    assert_eq!(generations, 2);
}

#[test]
fn scrubbing_removes_every_shell_only_credential() {
    let _lock = common::lock_env_vars();
    let guards: Vec<common::EnvVarGuard> = br8n::backup::SHELL_ONLY_CREDENTIAL_VARS
        .iter()
        .map(|v| common::EnvVarGuard::set(v, "from-an-interactive-shell"))
        .collect();
    br8n::backup::scrub_credential_env();
    for var in br8n::backup::SHELL_ONLY_CREDENTIAL_VARS {
        assert!(std::env::var_os(var).is_none(), "{var} survived the scrub");
    }
    drop(guards);
}

struct SharedRemote(std::sync::Arc<FakeRemote>);

impl Remote for SharedRemote {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn put_file(&self, key: &str, path: &Path) -> anyhow::Result<()> {
        self.0.put_file(key, path)
    }
    fn put_bytes(&self, key: &str, bytes: &[u8]) -> anyhow::Result<()> {
        self.0.put_bytes(key, bytes)
    }
    fn get_file(&self, key: &str, dest: &Path) -> anyhow::Result<()> {
        self.0.get_file(key, dest)
    }
    fn get_bytes(&self, key: &str) -> anyhow::Result<Vec<u8>> {
        self.0.get_bytes(key)
    }
    fn list(&self, prefix: &str) -> anyhow::Result<Vec<br8n::backup::remote::RemoteObject>> {
        self.0.list(prefix)
    }
    fn delete(&self, key: &str) -> anyhow::Result<()> {
        self.0.delete(key)
    }
}
