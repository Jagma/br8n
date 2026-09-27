use crate::config::Config;
use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use flate2::{Compression, GzBuilder};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Item {
    pub rel: String,
    pub hash: String,
    pub size: u64,
    pub mode: u32,
    pub bytes: Vec<u8>,
}

pub fn collect(_cfg: &Config) -> Result<Vec<Item>> {
    let config_path = Config::config_path();
    let mut items = vec![read_item(&config_path, "config/config.toml")?];
    let golden = config_path.with_file_name("golden.toml");
    if golden.is_file() {
        items.push(read_item(&golden, "config/golden.toml")?);
    }
    Ok(items)
}

fn read_item(abs: &Path, rel: &str) -> Result<Item> {
    let bytes = std::fs::read(abs).with_context(|| format!("could not read {}", abs.display()))?;
    let meta = std::fs::metadata(abs)?;
    Ok(Item {
        rel: rel.to_string(),
        hash: hex::encode(Sha256::digest(&bytes)),
        size: bytes.len() as u64,
        mode: mode_of(&meta),
        bytes,
    })
}

#[cfg(unix)]
fn mode_of(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn mode_of(_meta: &std::fs::Metadata) -> u32 {
    0o644
}

#[derive(Debug, Clone)]
pub struct IndexArchive {
    pub path: PathBuf,
    pub hash: String,
    pub size: u64,
}

pub fn copy_index_under_lock(db: &Path, staging: &Path) -> Result<()> {
    let _lock = crate::index::IndexLock::acquire(db).ok_or_else(|| {
        anyhow::anyhow!(
            "`br8n index` is running; a database copied mid-index is torn. Try again once it finishes."
        )
    })?;
    crate::index::copy_dir(db, staging)
}

pub fn archive_index(db: &Path, workdir: &Path) -> Result<Option<IndexArchive>> {
    if !db.exists() {
        return Ok(None);
    }
    let staging = workdir.join("db-snapshot");
    let _ = std::fs::remove_dir_all(&staging);

    copy_index_under_lock(db, &staging)?;

    let dest = workdir.join("index.tar.gz");
    let result = (|| -> Result<IndexArchive> {
        let out = std::fs::File::create(&dest)
            .with_context(|| format!("could not create {}", dest.display()))?;
        let enc = GzBuilder::new().mtime(0).write(out, Compression::default());
        let mut tar = tar::Builder::new(enc);

        let mut entries: Vec<PathBuf> = std::fs::read_dir(&staging)?
            .flatten()
            .map(|e| e.path())
            .collect();
        entries.sort();
        for path in entries {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let bytes = std::fs::read(&path)
                .with_context(|| format!("could not read {}", path.display()))?;
            let mut header = tar::Header::new_gnu();
            header.set_path(&name)?;
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(0);
            header.set_cksum();
            tar.append_data(&mut header, &name, bytes.as_slice())?;
        }
        tar.finish()?;
        tar.into_inner()?.finish()?;

        let bytes =
            std::fs::read(&dest).with_context(|| format!("could not read {}", dest.display()))?;
        Ok(IndexArchive {
            hash: hex::encode(Sha256::digest(&bytes)),
            size: bytes.len() as u64,
            path: dest.clone(),
        })
    })();

    let _ = std::fs::remove_dir_all(&staging);
    result.map(Some)
}

pub fn unpack_index(archive: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let f = std::fs::File::open(archive)
        .with_context(|| format!("could not open {}", archive.display()))?;
    let dec = GzDecoder::new(f);
    tar::Archive::new(dec).unpack(dest)?;
    Ok(())
}
