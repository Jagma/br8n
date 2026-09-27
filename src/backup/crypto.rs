use anyhow::{anyhow, Context, Result};
use chacha20poly1305::aead::generic_array::GenericArray;
use chacha20poly1305::aead::stream::{DecryptorBE32, EncryptorBE32};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::io::{Read, Write};
use std::path::Path;

type HmacSha256 = Hmac<Sha256>;

const CHUNK: usize = 1024 * 1024;
const MAGIC: &[u8; 4] = b"BRB1";
const DECRYPT_FAILURE_MESSAGE: &str = "could not decrypt: wrong key, or the object is corrupt";

/// 32-byte symmetric key, stored on disk as hex at mode 0600.
#[derive(Clone)]
pub struct Key([u8; 32]);

impl Key {
    pub fn generate() -> Key {
        use chacha20poly1305::aead::rand_core::RngCore;
        use chacha20poly1305::aead::OsRng;
        let mut k = [0u8; 32];
        OsRng.fill_bytes(&mut k);
        Key(k)
    }

    pub fn from_hex(s: &str) -> Result<Key> {
        let raw = hex::decode(s.trim()).context("backup key is not valid hex")?;
        let k: [u8; 32] = raw
            .try_into()
            .map_err(|_| anyhow!("backup key must be 32 bytes (64 hex characters)"))?;
        Ok(Key(k))
    }

    pub fn load(path: &Path) -> Result<Key> {
        let s = std::fs::read_to_string(path)
            .with_context(|| format!("could not read backup key at `{}`", path.display()))?;
        Key::from_hex(&s)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create directory for `{}`", path.display()))?;
        }
        let mut file = create_new_key_file(path)?;
        file.write_all(self.to_hex().as_bytes())
            .with_context(|| format!("could not write backup key to `{}`", path.display()))?;
        Ok(())
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Non-secret fingerprint of this key, safe to record in a manifest.
    pub fn id(&self) -> String {
        let mut mac: HmacSha256 =
            Mac::new_from_slice(&self.0).expect("hmac accepts any key length");
        mac.update(b"br8n-backup-key-id");
        hex::encode(&mac.finalize().into_bytes()[..4])
    }

    fn nonce_prefix(&self, content_hash: &str) -> [u8; 19] {
        let mut mac: HmacSha256 =
            Mac::new_from_slice(&self.0).expect("hmac accepts any key length");
        mac.update(b"br8n-backup-nonce");
        mac.update(content_hash.as_bytes());
        let out = mac.finalize().into_bytes();
        let mut prefix = [0u8; 19];
        prefix.copy_from_slice(&out[..19]);
        prefix
    }
}

#[cfg(unix)]
fn create_new_key_file(path: &Path) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| key_creation_error(path, e))
}

#[cfg(not(unix))]
fn create_new_key_file(path: &Path) -> Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| key_creation_error(path, e))
}

fn key_creation_error(path: &Path, e: std::io::Error) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::AlreadyExists {
        anyhow!(
            "backup key already exists at `{}`; overwriting it would orphan every backup encrypted with the existing key",
            path.display()
        )
    } else {
        anyhow::Error::new(e).context(format!(
            "could not create backup key at `{}`",
            path.display()
        ))
    }
}

pub fn seal_stream(
    key: &Key,
    content_hash: &str,
    src: &mut dyn Read,
    dst: &mut dyn Write,
) -> Result<()> {
    let cipher = XChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(&key.0));
    let prefix = key.nonce_prefix(content_hash);
    let mut enc = EncryptorBE32::from_aead(cipher, GenericArray::from_slice(&prefix));

    dst.write_all(MAGIC)?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = read_until_eof_or_full(src, &mut buf)?;
        if n < CHUNK {
            let frame = enc
                .encrypt_last(&buf[..n])
                .map_err(|_| anyhow!("could not encrypt final frame"))?;
            dst.write_all(&frame)?;
            break;
        }
        let frame = enc
            .encrypt_next(&buf[..n])
            .map_err(|_| anyhow!("could not encrypt frame"))?;
        dst.write_all(&frame)?;
    }
    dst.flush()?;
    Ok(())
}

pub fn open_stream(
    key: &Key,
    content_hash: &str,
    src: &mut dyn Read,
    dst: &mut dyn Write,
) -> Result<()> {
    let mut magic = [0u8; 4];
    src.read_exact(&mut magic)
        .context("could not decrypt: object is too short to be a br8n backup")?;
    if &magic != MAGIC {
        return Err(anyhow!(
            "could not decrypt: object is not a br8n backup (bad magic)"
        ));
    }

    let cipher = XChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(&key.0));
    let prefix = key.nonce_prefix(content_hash);
    let mut dec = DecryptorBE32::from_aead(cipher, GenericArray::from_slice(&prefix));

    const SEALED: usize = CHUNK + 16;
    let mut buf = vec![0u8; SEALED];
    loop {
        let n = read_until_eof_or_full(src, &mut buf)?;
        if n < SEALED {
            let frame = dec
                .decrypt_last(&buf[..n])
                .map_err(|_| anyhow!(DECRYPT_FAILURE_MESSAGE))?;
            dst.write_all(&frame)?;
            break;
        }
        let frame = dec
            .decrypt_next(&buf[..n])
            .map_err(|_| anyhow!(DECRYPT_FAILURE_MESSAGE))?;
        dst.write_all(&frame)?;
    }
    dst.flush()?;
    Ok(())
}

pub fn seal_bytes(key: &Key, content_hash: &str, plain: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(plain.len() + 32);
    seal_stream(
        key,
        content_hash,
        &mut std::io::Cursor::new(plain),
        &mut out,
    )?;
    Ok(out)
}

pub fn open_bytes(key: &Key, content_hash: &str, sealed: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(sealed.len());
    open_stream(
        key,
        content_hash,
        &mut std::io::Cursor::new(sealed),
        &mut out,
    )?;
    Ok(out)
}

fn read_until_eof_or_full(src: &mut dyn Read, buf: &mut [u8]) -> Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match src.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(filled)
}
