use crate::common;

use br8n::backup::crypto::Key;
use br8n::backup::manifest::{blob_key, index_key, IndexEntry, Manifest};
use br8n::backup::remote::{FakeRemote, Remote};
use br8n::backup::{back_up, resolve_generation, restore, RestoreOptions, RestoreStats};
use br8n::config::{BackupConfig, Config};
use std::path::PathBuf;

const CONFIG: &str = "sources = []\n";
const GOLDEN: &str = "[[case]]\nquery = \"x\"\n";

struct Fixture {
    _db: common::EnvVarGuard,
    _cfg: common::EnvVarGuard,
    data: PathBuf,
    _home: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Fixture {
    fn new() -> Fixture {
        let lock = common::lock_env_vars();
        let home = tempfile::tempdir().unwrap();
        let data = home.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("config.toml"), CONFIG).unwrap();
        std::fs::write(data.join("golden.toml"), GOLDEN).unwrap();
        Fixture {
            _cfg: common::EnvVarGuard::set("BR8N_CONFIG", data.join("config.toml")),
            _db: common::EnvVarGuard::set("BR8N_DB", data.join("db")),
            data,
            _home: home,
            _lock: lock,
        }
    }

    fn config(&self) -> PathBuf {
        self.data.join("config.toml")
    }

    fn golden(&self) -> PathBuf {
        self.data.join("golden.toml")
    }

    fn db(&self) -> PathBuf {
        self.data.join("db")
    }
}

fn files_only() -> Config {
    Config {
        index_transcripts: false,
        backup: BackupConfig {
            include_index: false,
            ..Default::default()
        },
        ..Config::default()
    }
}

fn with_index() -> Config {
    Config {
        index_transcripts: false,
        backup: BackupConfig {
            include_index: true,
            ..Default::default()
        },
        ..Config::default()
    }
}

fn files() -> RestoreOptions {
    RestoreOptions::default()
}

fn manifest_of(remote: &FakeRemote, generation: &str) -> Manifest {
    serde_json::from_slice(&remote.get_bytes(generation).unwrap()).unwrap()
}

#[test]
fn a_deleted_file_is_restored_and_an_existing_one_is_left_alone() {
    let fx = Fixture::new();
    let key = Key::generate();
    let remote = FakeRemote::new();
    back_up(&files_only(), &remote, Some(&key)).unwrap();

    std::fs::remove_file(fx.golden()).unwrap();
    std::fs::write(fx.config(), "sources = [\"edited since the backup\"]\n").unwrap();

    let stats = restore(&files_only(), &remote, Some(&key), &files()).unwrap();
    assert_eq!(
        stats,
        RestoreStats {
            files: 1,
            index_restored: false,
            skipped_existing: 1
        }
    );
    assert_eq!(std::fs::read_to_string(fx.golden()).unwrap(), GOLDEN);
    assert_eq!(
        std::fs::read_to_string(fx.config()).unwrap(),
        "sources = [\"edited since the backup\"]\n",
        "an existing config must not be clobbered without --force"
    );
}

#[test]
fn force_overwrites_existing_files() {
    let fx = Fixture::new();
    let key = Key::generate();
    let remote = FakeRemote::new();
    back_up(&files_only(), &remote, Some(&key)).unwrap();
    std::fs::write(fx.config(), "edited\n").unwrap();

    let opts = RestoreOptions {
        force: true,
        ..files()
    };
    let stats = restore(&files_only(), &remote, Some(&key), &opts).unwrap();
    assert_eq!(stats.files, 2);
    assert_eq!(stats.skipped_existing, 0);
    assert_eq!(std::fs::read_to_string(fx.config()).unwrap(), CONFIG);
}

#[cfg(unix)]
#[test]
fn a_restored_file_gets_back_its_recorded_mode() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new();
    std::fs::set_permissions(fx.golden(), std::fs::Permissions::from_mode(0o600)).unwrap();
    let remote = FakeRemote::new();
    back_up(&files_only(), &remote, None).unwrap();
    std::fs::remove_file(fx.golden()).unwrap();

    restore(&files_only(), &remote, None, &files()).unwrap();
    let mode = std::fs::metadata(fx.golden()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn a_dry_run_writes_nothing_but_still_proves_every_blob_decrypts() {
    let fx = Fixture::new();
    let key = Key::generate();
    let remote = FakeRemote::new();
    let generation = back_up(&files_only(), &remote, Some(&key))
        .unwrap()
        .generation;
    std::fs::remove_file(fx.golden()).unwrap();

    let dry = RestoreOptions {
        dry_run: true,
        ..files()
    };
    let stats = restore(&files_only(), &remote, Some(&key), &dry).unwrap();
    assert_eq!(stats.files, 1);
    assert!(
        !fx.golden().exists(),
        "--dry-run must not touch the filesystem"
    );

    let golden = manifest_of(&remote, &generation)
        .files
        .into_iter()
        .find(|f| f.path.ends_with("golden.toml"))
        .unwrap();
    let object = blob_key(Some(key.id().as_str()), &golden.hash);
    let mut sealed = remote.get_bytes(&object).unwrap();
    let last = sealed.len() - 1;
    sealed[last] ^= 1;
    remote.put_bytes(&object, &sealed).unwrap();
    assert!(
        restore(&files_only(), &remote, Some(&key), &dry).is_err(),
        "a dry run must fail on a blob that would not decrypt"
    );
}

#[test]
fn the_wrong_key_is_refused_before_anything_is_written() {
    let fx = Fixture::new();
    let remote = FakeRemote::new();
    back_up(&files_only(), &remote, Some(&Key::generate())).unwrap();
    std::fs::remove_file(fx.golden()).unwrap();

    let err = restore(&files_only(), &remote, Some(&Key::generate()), &files())
        .unwrap_err()
        .to_string();
    assert!(err.contains("different key"), "got: {err}");
    assert!(!fx.golden().exists());
}

#[test]
fn an_unencrypted_backup_restores_even_when_a_key_is_configured() {
    let fx = Fixture::new();
    let remote = FakeRemote::new();
    back_up(&files_only(), &remote, None).unwrap();
    std::fs::remove_file(fx.golden()).unwrap();

    restore(&files_only(), &remote, Some(&Key::generate()), &files()).unwrap();
    assert_eq!(std::fs::read_to_string(fx.golden()).unwrap(), GOLDEN);
}

#[test]
fn one_bad_blob_means_no_file_is_written_at_all() {
    let fx = Fixture::new();
    let key = Key::generate();
    let remote = FakeRemote::new();
    let generation = back_up(&files_only(), &remote, Some(&key))
        .unwrap()
        .generation;
    std::fs::remove_file(fx.config()).unwrap();
    std::fs::remove_file(fx.golden()).unwrap();

    let golden = manifest_of(&remote, &generation)
        .files
        .into_iter()
        .find(|f| f.path.ends_with("golden.toml"))
        .unwrap();
    remote
        .delete(&blob_key(Some(key.id().as_str()), &golden.hash))
        .unwrap();

    assert!(restore(&files_only(), &remote, Some(&key), &files()).is_err());
    assert!(
        !fx.config().exists(),
        "config.toml sorts before the missing golden blob and must still not be written"
    );
}

#[test]
fn a_plain_blob_that_does_not_match_its_hash_is_refused() {
    let fx = Fixture::new();
    let remote = FakeRemote::new();
    let generation = back_up(&files_only(), &remote, None).unwrap().generation;
    std::fs::remove_file(fx.golden()).unwrap();

    let golden = manifest_of(&remote, &generation)
        .files
        .into_iter()
        .find(|f| f.path.ends_with("golden.toml"))
        .unwrap();
    remote
        .put_bytes(&blob_key(None, &golden.hash), b"not what was backed up")
        .unwrap();

    let err = restore(&files_only(), &remote, None, &files())
        .unwrap_err()
        .to_string();
    assert!(err.contains("do not match"), "got: {err}");
    assert!(!fx.golden().exists());
}

#[test]
fn a_manifest_path_outside_the_config_directory_is_refused() {
    let fx = Fixture::new();
    let remote = FakeRemote::new();
    let generation = back_up(&files_only(), &remote, None).unwrap().generation;
    let mut m = manifest_of(&remote, &generation);

    for hostile in [
        "config/../escaped.toml",
        "config/nested/golden.toml",
        "transcripts/a.jsonl",
    ] {
        m.files[0].path = hostile.into();
        remote
            .put_bytes(&generation, &serde_json::to_vec(&m).unwrap())
            .unwrap();
        let opts = RestoreOptions {
            generation: Some(generation.clone()),
            force: true,
            ..files()
        };
        let err = restore(&files_only(), &remote, None, &opts)
            .unwrap_err()
            .to_string();
        assert!(err.contains("refusing"), "{hostile}: {err}");
    }
    assert!(!fx.data.parent().unwrap().join("escaped.toml").exists());
}

#[test]
fn an_explicit_generation_is_honoured_over_the_latest() {
    let fx = Fixture::new();
    let remote = FakeRemote::new();
    let old = back_up(&files_only(), &remote, None).unwrap().generation;
    std::fs::write(fx.golden(), "[[case]]\nquery = \"newer\"\n").unwrap();
    back_up(&files_only(), &remote, None).unwrap();
    std::fs::remove_file(fx.golden()).unwrap();

    let pinned = RestoreOptions {
        generation: Some(old),
        ..files()
    };
    restore(&files_only(), &remote, None, &pinned).unwrap();
    assert_eq!(std::fs::read_to_string(fx.golden()).unwrap(), GOLDEN);
}

#[test]
fn a_generation_that_does_not_exist_fails_by_name() {
    let _fx = Fixture::new();
    let remote = FakeRemote::new();
    back_up(&files_only(), &remote, None).unwrap();
    let missing = RestoreOptions {
        generation: Some("manifest/1999-01-01T00-00-00.000Z.json".into()),
        ..files()
    };
    let err = restore(&files_only(), &remote, None, &missing)
        .unwrap_err()
        .to_string();
    assert!(err.contains("1999-01-01"), "got: {err}");
}

#[test]
fn without_a_latest_pointer_the_newest_generation_is_found_by_listing() {
    let fx = Fixture::new();
    let remote = FakeRemote::new();
    back_up(&files_only(), &remote, None).unwrap();
    std::fs::write(fx.golden(), "[[case]]\nquery = \"newest\"\n").unwrap();
    let newest = back_up(&files_only(), &remote, None).unwrap().generation;
    remote.delete(Manifest::latest_key()).unwrap();

    assert_eq!(resolve_generation(&remote, None).unwrap(), newest);
    std::fs::remove_file(fx.golden()).unwrap();
    restore(&files_only(), &remote, None, &files()).unwrap();
    assert_eq!(
        std::fs::read_to_string(fx.golden()).unwrap(),
        "[[case]]\nquery = \"newest\"\n"
    );
}

#[test]
fn an_empty_remote_says_there_is_nothing_to_restore() {
    let _fx = Fixture::new();
    let err = restore(&files_only(), &FakeRemote::new(), None, &files())
        .unwrap_err()
        .to_string();
    assert!(err.contains("no backups"), "got: {err}");
}

fn seed_index(db: &std::path::Path, contents: &[u8]) {
    std::fs::create_dir_all(db).unwrap();
    std::fs::write(db.join("graph.kz"), contents).unwrap();
    std::fs::write(db.join("graph.kz.wal"), b"wal").unwrap();
}

#[test]
fn the_index_round_trips_and_drops_the_stale_stamps() {
    for encrypted in [true, false] {
        let fx = Fixture::new();
        let key = Key::generate();
        let used = encrypted.then_some(&key);
        seed_index(&fx.db(), b"the index as it was backed up");
        let remote = FakeRemote::new();
        back_up(&with_index(), &remote, used).unwrap();

        std::fs::remove_dir_all(fx.db()).unwrap();
        seed_index(&fx.db(), b"an index built after the backup");
        std::fs::write(fx.db().join("only-in-the-newer-index"), b"x").unwrap();
        std::fs::write(fx.db().with_extension("stamps"), "{}").unwrap();

        let opts = RestoreOptions {
            index: true,
            ..files()
        };
        let stats = restore(&with_index(), &remote, used, &opts).unwrap();
        assert!(stats.index_restored, "encrypted: {encrypted}");
        assert_eq!(
            std::fs::read(fx.db().join("graph.kz")).unwrap(),
            b"the index as it was backed up".to_vec(),
            "encrypted: {encrypted}"
        );
        assert!(!fx.db().join("only-in-the-newer-index").exists());
        assert!(
            !fx.db().with_extension("stamps").exists(),
            "stamps describe the replaced index and would make the next run skip files"
        );
        assert!(!fx.db().with_extension("restore.staging").exists());
        assert!(!fx.db().with_extension("old").exists());
    }
}

#[test]
fn without_the_index_flag_the_live_index_is_untouched() {
    let fx = Fixture::new();
    seed_index(&fx.db(), b"backed up");
    let remote = FakeRemote::new();
    back_up(&with_index(), &remote, None).unwrap();
    seed_index(&fx.db(), b"live");

    let stats = restore(&with_index(), &remote, None, &files()).unwrap();
    assert!(!stats.index_restored);
    assert_eq!(
        std::fs::read(fx.db().join("graph.kz")).unwrap(),
        b"live".to_vec()
    );
}

#[test]
fn a_dry_run_leaves_the_index_alone() {
    let fx = Fixture::new();
    seed_index(&fx.db(), b"backed up");
    let remote = FakeRemote::new();
    back_up(&with_index(), &remote, None).unwrap();
    seed_index(&fx.db(), b"live");

    let opts = RestoreOptions {
        index: true,
        dry_run: true,
        ..files()
    };
    let stats = restore(&with_index(), &remote, None, &opts).unwrap();
    assert!(!stats.index_restored);
    assert_eq!(
        std::fs::read(fx.db().join("graph.kz")).unwrap(),
        b"live".to_vec()
    );
}

#[test]
fn restoring_the_index_while_indexing_runs_is_refused_and_changes_nothing() {
    let fx = Fixture::new();
    seed_index(&fx.db(), b"backed up");
    let remote = FakeRemote::new();
    back_up(&with_index(), &remote, None).unwrap();
    seed_index(&fx.db(), b"live");

    let _indexing = br8n::index::IndexLock::acquire(&fx.db()).unwrap();
    let opts = RestoreOptions {
        index: true,
        ..files()
    };
    let err = restore(&with_index(), &remote, None, &opts)
        .unwrap_err()
        .to_string();
    assert!(err.contains("br8n index"), "got: {err}");
    assert_eq!(
        std::fs::read(fx.db().join("graph.kz")).unwrap(),
        b"live".to_vec()
    );
    assert!(!fx.db().with_extension("restore.staging").exists());
}

#[test]
fn a_substituted_index_snapshot_is_refused_and_the_live_index_survives() {
    let fx = Fixture::new();
    seed_index(&fx.db(), b"backed up");
    let remote = FakeRemote::new();
    let generation = back_up(&with_index(), &remote, None).unwrap().generation;
    seed_index(&fx.db(), b"live");

    let ix = manifest_of(&remote, &generation).index.unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let other_db = elsewhere.path().join("db");
    seed_index(&other_db, b"a different, perfectly valid index");
    let substitute = br8n::backup::snapshot::archive_index(&other_db, elsewhere.path())
        .unwrap()
        .unwrap();
    remote
        .put_file(&index_key(None, &ix.hash), &substitute.path)
        .unwrap();
    let opts = RestoreOptions {
        index: true,
        ..files()
    };
    assert!(restore(&with_index(), &remote, None, &opts).is_err());
    assert_eq!(
        std::fs::read(fx.db().join("graph.kz")).unwrap(),
        b"live".to_vec()
    );
}

#[test]
fn an_incompatible_index_is_refused_before_any_file_is_written() {
    let fx = Fixture::new();
    let remote = FakeRemote::new();
    let generation = back_up(&files_only(), &remote, None).unwrap().generation;
    let mut m = manifest_of(&remote, &generation);
    m.index = Some(IndexEntry {
        hash: "abc".into(),
        size: 1,
        embed_model: "nomic-embed-text".into(),
        dimensions: 768,
        chunk_tokens: 512,
        documents: 1,
        chunks: 1,
    });
    remote
        .put_bytes(&generation, &serde_json::to_vec(&m).unwrap())
        .unwrap();
    std::fs::remove_file(fx.golden()).unwrap();

    let opts = RestoreOptions {
        index: true,
        ..files()
    };
    let err = restore(&Config::default(), &remote, None, &opts)
        .unwrap_err()
        .to_string();
    assert!(err.contains("nomic-embed-text"), "got: {err}");
    assert!(err.contains("--reindex"), "must say how to recover: {err}");
    assert!(!fx.golden().exists());
}
