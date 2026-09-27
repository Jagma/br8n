pub mod crypto;
pub mod manifest;
pub mod remote;
pub mod snapshot;

use crate::config::Config;
use anyhow::{anyhow, bail, Context, Result};
use crypto::Key;
use manifest::{blob_key, index_key, FileEntry, IndexEntry, LatestPointer, Manifest};
use remote::Remote;
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

pub struct BackupLock(std::path::PathBuf);

impl BackupLock {
    pub fn acquire(db: &Path) -> Option<BackupLock> {
        let path = db.with_extension("backup.lock");
        if let Ok(existing) = std::fs::read_to_string(&path) {
            if let Ok(pid) = existing.trim().parse::<u32>() {
                if crate::index::pid_is_alive(pid) {
                    return None;
                }
            }
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, std::process::id().to_string()).ok()?;
        Some(BackupLock(path))
    }

    pub fn is_held(db: &Path) -> bool {
        std::fs::read_to_string(db.with_extension("backup.lock"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .is_some_and(crate::index::pid_is_alive)
    }
}

impl Drop for BackupLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[derive(Debug, Clone, Default)]
pub struct BackupStats {
    pub uploaded: usize,
    pub deduped: usize,
    pub bytes: u64,
    pub generation: String,
}

pub fn back_up(cfg: &Config, remote: &dyn Remote, key: Option<&Key>) -> Result<BackupStats> {
    let present: std::collections::HashSet<String> = remote
        .list("blobs/")
        .context("could not list existing blobs")?
        .into_iter()
        .map(|o| o.key)
        .collect();
    let present_index: std::collections::HashSet<String> = remote
        .list("index/")
        .context("could not list existing index snapshots")?
        .into_iter()
        .map(|o| o.key)
        .collect();

    let key_id = key.map(|k| k.id());
    let staging = Config::db_path().with_extension("backup.staging");
    let _ = std::fs::remove_dir_all(&staging);

    let result = (|| -> Result<BackupStats> {
        let mut stats = BackupStats::default();
        let mut m = Manifest::new(key);

        for item in snapshot::collect(cfg)? {
            let object = blob_key(key_id.as_deref(), &item.hash);
            m.files.push(FileEntry {
                path: item.rel.clone(),
                hash: item.hash.clone(),
                size: item.size,
                mode: item.mode,
            });
            if present.contains(&object) {
                stats.deduped += 1;
                continue;
            }
            let payload = match key {
                Some(k) => crypto::seal_bytes(k, &item.hash, &item.bytes)?,
                None => item.bytes.clone(),
            };
            stats.bytes += payload.len() as u64;
            remote
                .put_bytes(&object, &payload)
                .with_context(|| format!("could not upload {}", item.rel))?;
            stats.uploaded += 1;
        }

        if cfg.backup.include_index {
            std::fs::create_dir_all(&staging).context("could not create a staging directory")?;
            if let Some(archive) = snapshot::archive_index(&Config::db_path(), &staging)? {
                let (documents, chunks) = index_counts(cfg);
                let object = index_key(key_id.as_deref(), &archive.hash);
                m.index = Some(IndexEntry {
                    hash: archive.hash.clone(),
                    size: archive.size,
                    embed_model: cfg.embed.model.clone(),
                    dimensions: cfg.embed.dimensions,
                    chunk_tokens: cfg.embed.chunk_tokens,
                    documents,
                    chunks,
                });
                if present_index.contains(&object) {
                    stats.deduped += 1;
                } else {
                    let upload = staging.join("index.sealed");
                    match key {
                        Some(k) => {
                            let mut src = std::fs::File::open(&archive.path)?;
                            let mut dst = std::fs::File::create(&upload)?;
                            crypto::seal_stream(k, &archive.hash, &mut src, &mut dst)?;
                        }
                        None => {
                            std::fs::rename(&archive.path, &upload)?;
                        }
                    }
                    stats.bytes += std::fs::metadata(&upload)?.len();
                    remote
                        .put_file(&object, &upload)
                        .context("could not upload the index snapshot")?;
                    stats.uploaded += 1;
                }
            }
        }

        let generation = m.generation_key();
        remote
            .put_bytes(&generation, &serde_json::to_vec_pretty(&m)?)
            .context("could not upload the manifest")?;
        remote
            .put_bytes(
                Manifest::latest_key(),
                &serde_json::to_vec_pretty(&LatestPointer {
                    generation: generation.clone(),
                })?,
            )
            .context("could not update the latest pointer")?;

        stats.generation = generation;
        Ok(stats)
    })();

    let _ = std::fs::remove_dir_all(&staging);
    result
}

fn index_counts(cfg: &Config) -> (usize, usize) {
    match crate::store::Store::open_existing(&Config::db_path(), cfg.embed.dimensions) {
        Ok(s) => (
            s.count_documents().unwrap_or(0) as usize,
            s.count_chunks().unwrap_or(0) as usize,
        ),
        Err(_) => (0, 0),
    }
}

#[derive(Debug, Clone, Default)]
pub struct RestoreOptions {
    pub generation: Option<String>,
    pub index: bool,
    pub dry_run: bool,
    pub force: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestoreStats {
    pub files: usize,
    pub index_restored: bool,
    pub skipped_existing: usize,
}

pub fn resolve_generation(remote: &dyn Remote, want: Option<&str>) -> Result<String> {
    if let Some(generation) = want {
        return Ok(generation.to_string());
    }
    let pointer = remote
        .get_bytes(Manifest::latest_key())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<LatestPointer>(&bytes).ok());
    if let Some(pointer) = pointer {
        return Ok(pointer.generation);
    }
    let mut generations: Vec<String> = remote
        .list("manifest/")
        .context("could not list backup generations")?
        .into_iter()
        .map(|o| o.key)
        .filter(|k| k != Manifest::latest_key())
        .collect();
    generations.sort();
    generations
        .pop()
        .context("this remote holds no backups yet; run `br8n backup` first")
}

struct RestoredFile {
    dest: PathBuf,
    bytes: Vec<u8>,
    mode: u32,
}

pub fn restore(
    cfg: &Config,
    remote: &dyn Remote,
    key: Option<&Key>,
    opts: &RestoreOptions,
) -> Result<RestoreStats> {
    let generation = resolve_generation(remote, opts.generation.as_deref())?;
    let raw = remote
        .get_bytes(&generation)
        .with_context(|| format!("no such generation: {generation}"))?;
    let m: Manifest = serde_json::from_slice(&raw)
        .with_context(|| format!("could not parse manifest {generation}"))?;

    m.check_key(key)?;
    if opts.index {
        m.check_index_compat(cfg)?;
    }
    let sealing_key = m.encryption.as_ref().and(key);
    let key_id = m.encryption.as_ref().map(|e| e.key_id.as_str());

    let mut stats = RestoreStats::default();
    let mut pending = Vec::new();
    for entry in &m.files {
        let dest = config_destination(&entry.path)?;
        if dest.exists() && !opts.force {
            stats.skipped_existing += 1;
            continue;
        }
        let stored = remote
            .get_bytes(&blob_key(key_id, &entry.hash))
            .with_context(|| format!("the backup is missing the blob for {}", entry.path))?;
        let bytes = match sealing_key {
            Some(k) => crypto::open_bytes(k, &entry.hash, &stored)
                .with_context(|| format!("could not decrypt {}", entry.path))?,
            None => stored,
        };
        if hex::encode(Sha256::digest(&bytes)) != entry.hash {
            bail!(
                "the restored bytes of {} do not match the hash its manifest records",
                entry.path
            );
        }
        pending.push(RestoredFile {
            dest,
            bytes,
            mode: entry.mode,
        });
    }

    stats.files = pending.len();
    if !opts.dry_run {
        for file in &pending {
            write_atomically(&file.dest, &file.bytes, file.mode)?;
        }
    }

    if opts.index && !opts.dry_run {
        if let Some(ix) = &m.index {
            restore_index(remote, sealing_key, key_id, ix)?;
            stats.index_restored = true;
        }
    }

    Ok(stats)
}

fn config_destination(rel: &str) -> Result<PathBuf> {
    let name = rel
        .strip_prefix("config/")
        .filter(|name| {
            let mut parts = Path::new(name).components();
            matches!(parts.next(), Some(Component::Normal(_))) && parts.next().is_none()
        })
        .ok_or_else(|| anyhow!("refusing to restore {rel:?}: not a file this backup writes"))?;
    Ok(Config::config_path().with_file_name(name))
}

fn write_atomically(dest: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dest.with_file_name(format!(".{name}.br8n-restore-tmp"));
    let written = (|| -> Result<()> {
        std::fs::write(&tmp, bytes)?;
        set_mode(&tmp, mode)?;
        std::fs::rename(&tmp, dest)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.with_context(|| format!("could not restore {}", dest.display()))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

fn restore_index(
    remote: &dyn Remote,
    key: Option<&Key>,
    key_id: Option<&str>,
    ix: &IndexEntry,
) -> Result<()> {
    let db = Config::db_path();
    let work = db.with_extension("restore.staging");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).context("could not create a restore staging directory")?;

    let result = (|| -> Result<()> {
        let downloaded = work.join("index.download");
        remote
            .get_file(&index_key(key_id, &ix.hash), &downloaded)
            .context(
                "this generation's index snapshot is no longer on the remote. Restore without `--index` and rebuild with `br8n index --reindex`.",
            )?;
        let archive = work.join("index.tar.gz");
        match key {
            Some(k) => {
                let mut src = std::fs::File::open(&downloaded)?;
                let mut dst = std::fs::File::create(&archive)?;
                crypto::open_stream(k, &ix.hash, &mut src, &mut dst)
                    .context("could not decrypt the index snapshot")?;
            }
            None => std::fs::rename(&downloaded, &archive)?,
        }
        if sha256_of_file(&archive)? != ix.hash {
            bail!("the downloaded index snapshot does not match the hash its manifest records");
        }
        let unpacked = work.join("db");
        snapshot::unpack_index(&archive, &unpacked)?;

        let _lock = crate::index::IndexLock::acquire(&db).ok_or_else(|| {
            anyhow!("`br8n index` is running; wait for it to finish before restoring the index")
        })?;
        crate::index::publish_shadow(&db, &unpacked)?;
        let _ = std::fs::remove_file(crate::index::stamp_path(&db));
        Ok(())
    })();

    let _ = std::fs::remove_dir_all(&work);
    result
}

fn sha256_of_file(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    std::io::copy(&mut std::fs::File::open(path)?, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneStats {
    pub manifests_deleted: usize,
    pub blobs_deleted: usize,
    pub index_deleted: usize,
}

pub fn prune(
    remote: &dyn Remote,
    keep_generations: usize,
    keep_index: usize,
) -> Result<PruneStats> {
    let (latest, mut generations): (Vec<String>, Vec<String>) = remote
        .list("manifest/")
        .context("could not list generations; nothing was deleted")?
        .into_iter()
        .map(|o| o.key)
        .partition(|k| k == Manifest::latest_key());
    generations.sort();
    generations.reverse();

    let pointed_at = match latest.first() {
        None => None,
        Some(key) => {
            let raw = remote
                .get_bytes(key)
                .with_context(|| format!("could not read {key}; nothing was deleted"))?;
            let pointer: LatestPointer = serde_json::from_slice(&raw)
                .with_context(|| format!("could not parse {key}; nothing was deleted"))?;
            Some(pointer.generation)
        }
    };
    let mut survivors = Vec::new();
    let mut doomed = Vec::new();
    for (rank, generation) in generations.into_iter().enumerate() {
        if rank < keep_generations.max(1) || pointed_at.as_ref() == Some(&generation) {
            survivors.push(generation);
        } else {
            doomed.push(generation);
        }
    }

    let mut live_blobs = std::collections::HashSet::new();
    let mut live_index = Vec::new();
    for generation in &survivors {
        let raw = remote
            .get_bytes(generation)
            .with_context(|| format!("could not read {generation}; nothing was deleted"))?;
        let m: Manifest = serde_json::from_slice(&raw)
            .with_context(|| format!("could not parse {generation}; nothing was deleted"))?;
        let key_id = m.encryption.as_ref().map(|e| e.key_id.as_str());
        for f in &m.files {
            live_blobs.insert(blob_key(key_id, &f.hash));
        }
        if let Some(ix) = &m.index {
            let object = index_key(key_id, &ix.hash);
            if !live_index.contains(&object) {
                live_index.push(object);
            }
        }
    }
    live_index.truncate(keep_index.max(1));

    let all_blobs = remote
        .list("blobs/")
        .context("could not list blobs; nothing was deleted")?;
    let all_index = remote
        .list("index/")
        .context("could not list index snapshots; nothing was deleted")?;

    let mut stats = PruneStats::default();
    for generation in &doomed {
        remote.delete(generation)?;
        stats.manifests_deleted += 1;
    }
    for object in all_blobs
        .into_iter()
        .filter(|o| !live_blobs.contains(&o.key))
    {
        remote.delete(&object.key)?;
        stats.blobs_deleted += 1;
    }
    for object in all_index
        .into_iter()
        .filter(|o| !live_index.contains(&o.key))
    {
        remote.delete(&object.key)?;
        stats.index_deleted += 1;
    }
    Ok(stats)
}

pub const CRON_SENTINEL: &str = "# br8n-backup";

#[derive(Debug)]
pub enum RunOutcome {
    Done(Vec<(String, BackupStats)>),
    Skipped(String),
    Failed(String),
}

impl RunOutcome {
    pub fn exit_code(&self) -> i32 {
        match self {
            RunOutcome::Done(_) => 0,
            RunOutcome::Failed(_) => 1,
            RunOutcome::Skipped(_) => 2,
        }
    }
}

pub fn is_configured(cfg: &Config) -> bool {
    cfg.backup.enabled && !cfg.backup.targets.is_empty()
}

pub fn load_key(cfg: &Config) -> Result<Option<Key>> {
    if !cfg.backup.encrypt {
        return Ok(None);
    }
    let path = cfg.backup_key_path();
    if !path.exists() {
        return Err(anyhow::anyhow!(
            "encryption is on but there is no key at `{}`.\n\
             Run `br8n backup init` once: it generates the key, prints it, and asks you to \
             store a copy somewhere other than this machine.",
            path.display()
        ));
    }
    Ok(Some(Key::load(&path)?))
}

fn s3_table(cfg: &Config) -> Result<&crate::config::S3Config> {
    cfg.backup
        .s3
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("`targets` lists \"s3\" but there is no [backup.s3] table"))
}

fn drive_table(cfg: &Config) -> Result<&crate::config::DriveConfig> {
    cfg.backup.drive.as_ref().ok_or_else(|| {
        anyhow::anyhow!("`targets` lists \"drive\" but there is no [backup.drive] table")
    })
}

fn unknown_target(other: &str) -> anyhow::Error {
    anyhow::anyhow!("unknown backup target `{other}`; supported targets are \"s3\" and \"drive\"")
}

pub fn remote_for(cfg: &Config, target: &str) -> Result<Box<dyn Remote>> {
    match target {
        "s3" => Ok(Box::new(remote::s3::S3Remote::new(s3_table(cfg)?)?)),
        "drive" => Ok(Box::new(remote::drive::DriveRemote::new(
            drive_table(cfg)?,
            &cfg.drive_token_path(),
        )?)),
        other => Err(unknown_target(other)),
    }
}

pub fn check_target(cfg: &Config, target: &str) -> Result<String> {
    match target {
        "s3" => {
            let table = s3_table(cfg)?;
            remote::s3::S3Remote::new(table)?.check()?;
            Ok(format!("s3://{}/{}", table.bucket, table.prefix))
        }
        "drive" => {
            let table = drive_table(cfg)?;
            remote::drive::DriveRemote::new(table, &cfg.drive_token_path())?.check()?;
            Ok(format!("folder {}", table.folder_id))
        }
        other => Err(unknown_target(other)),
    }
}

pub fn run_all(cfg: &Config) -> RunOutcome {
    if !is_configured(cfg) {
        return RunOutcome::Skipped(
            "no backups configured (set `enabled = true` and `targets` under [backup])".into(),
        );
    }
    let db = Config::db_path();
    let Some(_lock) = BackupLock::acquire(&db) else {
        return RunOutcome::Skipped("another `br8n backup` is already running".into());
    };
    let key = match load_key(cfg) {
        Ok(k) => k,
        Err(e) => return RunOutcome::Failed(format!("{e:#}")),
    };
    let remotes = cfg
        .backup
        .targets
        .iter()
        .map(|t| (t.clone(), remote_for(cfg, t)))
        .collect();
    run_targets(cfg, key.as_ref(), remotes)
}

pub fn run_targets(
    cfg: &Config,
    key: Option<&Key>,
    remotes: Vec<(String, Result<Box<dyn Remote>>)>,
) -> RunOutcome {
    let total = remotes.len();
    let mut done = Vec::new();
    let mut backup_failures = Vec::new();
    let mut prune_failures = Vec::new();
    for (target, remote) in remotes {
        let remote = match remote {
            Ok(r) => r,
            Err(e) => {
                backup_failures.push(format!("{target}: {e:#}"));
                continue;
            }
        };
        match back_up(cfg, remote.as_ref(), key) {
            Ok(stats) => {
                if let Err(e) = prune(
                    remote.as_ref(),
                    cfg.backup.keep_generations,
                    cfg.backup.keep_index,
                ) {
                    prune_failures.push(format!(
                        "{target}: the backup succeeded but pruning failed: {e:#}"
                    ));
                }
                done.push((target, stats));
            }
            Err(e) => backup_failures.push(format!("{target}: {e:#}")),
        }
    }
    if backup_failures.is_empty() {
        if let Err(e) = write_stamp(&Config::db_path()) {
            prune_failures.push(format!("could not write the backup stamp: {e:#}"));
        }
    }
    let failures: Vec<String> = backup_failures.into_iter().chain(prune_failures).collect();
    if failures.is_empty() {
        return RunOutcome::Done(done);
    }
    RunOutcome::Failed(format!(
        "{} of {total} targets backed up\n{}",
        done.len(),
        failures.join("\n")
    ))
}

pub fn stamp_path(db: &Path) -> std::path::PathBuf {
    db.with_extension("backup.stamp")
}

pub fn write_stamp(db: &Path) -> Result<()> {
    let path = stamp_path(db);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        path,
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )?;
    Ok(())
}

pub fn read_stamp(db: &Path) -> Option<chrono::DateTime<chrono::Utc>> {
    let raw = std::fs::read_to_string(stamp_path(db)).ok()?;
    chrono::DateTime::parse_from_rfc3339(raw.trim())
        .ok()
        .map(|d| d.with_timezone(&chrono::Utc))
}

pub fn describe_age_at(
    stamp: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let Some(t) = stamp else {
        return "never".into();
    };
    match (now - t).num_hours() {
        h if h < 1 => "under an hour ago".into(),
        1 => "1 hour ago".into(),
        h if h < 48 => format!("{h} hours ago"),
        h => format!("{} days ago", h / 24),
    }
}

pub fn describe_age(stamp: Option<chrono::DateTime<chrono::Utc>>) -> String {
    describe_age_at(stamp, chrono::Utc::now())
}

pub fn cron_line(schedule: &str, exe: &Path, config: Option<&Path>) -> String {
    let env = match config {
        Some(p) => format!("BR8N_CONFIG={} ", p.display()),
        None => String::new(),
    };
    format!(
        "{schedule} {env}{} backup >/dev/null 2>&1 {CRON_SENTINEL}",
        exe.display()
    )
}

pub fn crontab_with(existing: &str, line: Option<&str>) -> String {
    let mut kept: Vec<&str> = existing
        .lines()
        .filter(|l| !l.trim_end().ends_with(CRON_SENTINEL))
        .collect();
    kept.extend(line);
    let mut body = kept.join("\n");
    body.push('\n');
    body
}

pub const SHELL_ONLY_CREDENTIAL_VARS: [&str; 7] = [
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_PROFILE",
    "AWS_REGION",
    "AWS_DEFAULT_REGION",
    "GOOGLE_APPLICATION_CREDENTIALS",
];

pub fn scrub_credential_env() {
    for var in SHELL_ONLY_CREDENTIAL_VARS {
        std::env::remove_var(var);
    }
}
