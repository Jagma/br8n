use anyhow::{Context, Result};
use memmap2::Mmap;
use std::path::Path;

pub const USED_FILE: &str = "pack.used";

const MAGIC: &[u8; 8] = b"BRNUSED1";
const HEADER: usize = 12;

pub fn write(dir: &Path, last_used: &[i64]) -> Result<()> {
    let mut buf: Vec<u8> = Vec::with_capacity(HEADER + last_used.len() * 8);
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&(last_used.len() as u32).to_le_bytes());
    for v in last_used {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    let p = dir.join(USED_FILE);
    std::fs::write(&p, &buf).with_context(|| format!("write {}", p.display()))
}

pub struct Reader {
    map: Mmap,
    num_rows: usize,
}

impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("used::Reader")
            .field("rows", &self.num_rows)
            .finish()
    }
}

impl Reader {
    pub fn open(dir: &Path, rows: usize) -> Result<Option<Reader>> {
        let p = dir.join(USED_FILE);
        let f = match std::fs::File::open(&p) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("open {}", p.display())),
        };
        let map = unsafe { Mmap::map(&f) }.context("mmap pack.used")?;
        anyhow::ensure!(
            map.len() >= HEADER && &map[0..8] == MAGIC,
            "{} is not a pack.used file",
            p.display()
        );
        let declared = u32::from_le_bytes(map[8..12].try_into().unwrap()) as usize;
        anyhow::ensure!(
            declared == rows && map.len() == HEADER + rows * 8,
            "{} declares {declared} rows in {} bytes against the records' {rows} rows — \
             a stale per-row array would attach one document's usage to another",
            p.display(),
            map.len()
        );
        Ok(Some(Reader {
            map,
            num_rows: rows,
        }))
    }

    pub fn rows(&self) -> usize {
        self.num_rows
    }

    pub fn get(&self, row: usize) -> Option<i64> {
        if row >= self.num_rows {
            return None;
        }
        let o = HEADER + row * 8;
        let v = i64::from_le_bytes(self.map[o..o + 8].try_into().unwrap());
        (v > 0).then_some(v)
    }
}
