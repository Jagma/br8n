use anyhow::Result;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub first_seen: i64,
    pub last_used: Option<i64>,
}

pub type Map = HashMap<String, Usage>;

fn log_path(db: &Path) -> PathBuf {
    db.with_extension(format!("usage.{}", std::process::id()))
}

fn map_path(db: &Path) -> PathBuf {
    db.with_extension("usage.json")
}

pub fn record(db: &Path, doc_ids: &[String]) {
    let distinct: BTreeSet<&str> = doc_ids
        .iter()
        .map(String::as_str)
        .filter(|id| !id.is_empty() && !id.contains('\n'))
        .collect();
    if distinct.is_empty() {
        return;
    }
    let ts = crate::memory::now_secs();
    let lines: String = distinct.iter().map(|id| format!("{ts} {id}\n")).collect();
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path(db))
        .and_then(|mut f| f.write_all(lines.as_bytes()));
}

pub fn record_hits(db: &Path, hits: &[crate::store::Hit]) {
    let ids: Vec<String> = hits.iter().map(|h| h.doc_id.clone()).collect();
    record(db, &ids);
}

pub fn load(db: &Path) -> Result<Map> {
    match std::fs::read(map_path(db)) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(e.into()),
    }
}

fn pending_logs(db: &Path) -> Result<Vec<PathBuf>> {
    let dir = db.parent().unwrap_or(Path::new("."));
    let stem = db.file_name().and_then(|s| s.to_str()).unwrap_or("db");
    let prefix = format!("{stem}.usage.");
    let mut logs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.strip_prefix(&prefix)) else {
            continue;
        };
        if !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()) {
            logs.push(entry.path());
        }
    }
    Ok(logs)
}

fn save(db: &Path, map: &Map) -> Result<()> {
    let tmp = db.with_extension(format!("usage.json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(map)?)?;
    std::fs::rename(&tmp, map_path(db))?;
    Ok(())
}

pub fn fold(db: &Path) -> Result<usize> {
    let logs = pending_logs(db)?;
    if logs.is_empty() {
        return Ok(0);
    }
    let mut map = load(db)?;
    let mut folded = 0usize;
    for log in &logs {
        for line in std::fs::read_to_string(log)?.lines() {
            let Some((ts, id)) = line.split_once(' ') else {
                continue;
            };
            let Ok(ts) = ts.parse::<i64>() else {
                continue;
            };
            let usage = map.entry(id.to_string()).or_insert(Usage {
                first_seen: ts,
                last_used: None,
            });
            usage.first_seen = usage.first_seen.min(ts);
            usage.last_used = Some(usage.last_used.map_or(ts, |prev| prev.max(ts)));
            folded += 1;
        }
    }
    if folded > 0 {
        save(db, &map)?;
    }
    for log in logs {
        let _ = std::fs::remove_file(log);
    }
    Ok(folded)
}

pub fn retain_indexed(db: &Path, indexed: &HashSet<String>) -> Result<usize> {
    let mut map = load(db)?;
    let before = map.len();
    map.retain(|doc_id, _| indexed.contains(doc_id));
    let dropped = before - map.len();
    if dropped > 0 {
        save(db, &map)?;
    }
    Ok(dropped)
}

pub fn pending(db: &Path) -> usize {
    pending_logs(db)
        .unwrap_or_default()
        .iter()
        .filter_map(|log| std::fs::read_to_string(log).ok())
        .map(|text| text.lines().count())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_documents_the_index_still_holds_keep_their_usage() {
        let t = tempfile::tempdir().unwrap();
        let db = t.path().join("db");
        let long_unused = Usage {
            first_seen: 1,
            last_used: Some(1),
        };
        let map: Map = [
            ("still-indexed".to_string(), long_unused),
            ("gone".to_string(), long_unused),
        ]
        .into_iter()
        .collect();
        save(&db, &map).unwrap();

        let indexed: HashSet<String> = ["still-indexed".to_string()].into_iter().collect();
        assert_eq!(retain_indexed(&db, &indexed).unwrap(), 1);

        let reloaded = load(&db).unwrap();
        assert_eq!(reloaded.get("still-indexed"), Some(&long_unused));
        assert!(!reloaded.contains_key("gone"));
    }
}
