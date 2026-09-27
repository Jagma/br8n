use anyhow::Result;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Entry {
    pub title: String,
    pub uri: String,
}

#[derive(Debug, Clone)]
pub struct Injection {
    pub session: String,
    pub entries: Vec<Entry>,
    pub body_bytes: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Audit {
    pub injections: usize,
    pub entries: usize,
    pub by_scheme: BTreeMap<String, usize>,
    pub injections_with_no_note: usize,
    pub body_bytes: Vec<usize>,
}

const MARKER: &str = "<br8n-context>";

/// Walk Claude Code's transcript root and recover every br8n injection.
///
/// An unreadable or malformed transcript is skipped rather than treated as
/// fatal: these are another program's files, and a partial answer over
/// thousands of them is worth more than an error on the first bad line.
pub fn audit(root: &Path) -> Result<Audit> {
    anyhow::ensure!(
        root.is_dir(),
        "no transcript directory at {}",
        root.display()
    );
    let mut a = Audit::default();
    for path in transcripts(root) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let session = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        for line in text.lines() {
            if !line.contains(MARKER) {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            for body in bodies(&v) {
                if !body.contains(MARKER) {
                    continue;
                }
                let inj = parse_block(&session, &body);
                a.injections += 1;
                a.entries += inj.entries.len();
                a.body_bytes.push(inj.body_bytes);
                let mut has_note = false;
                for e in &inj.entries {
                    let scheme = e.uri.split("://").next().unwrap().to_string();
                    if scheme == "file" {
                        has_note = true;
                    }
                    *a.by_scheme.entry(scheme).or_insert(0) += 1;
                }
                if !has_note {
                    a.injections_with_no_note += 1;
                }
            }
        }
    }
    Ok(a)
}

fn transcripts(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else {
                continue;
            };
            let p = e.path();
            if ft.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "jsonl") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn bodies(v: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(m) => {
                if m.get("type").and_then(|t| t.as_str()) == Some("hook_additional_context") {
                    match m.get("content") {
                        Some(serde_json::Value::Array(xs)) => {
                            let joined: Vec<&str> = xs.iter().filter_map(|x| x.as_str()).collect();
                            out.push(joined.join("\n"));
                        }
                        Some(serde_json::Value::String(s)) => out.push(s.clone()),
                        _ => {}
                    }
                    return;
                }
                for x in m.values() {
                    walk(x, out);
                }
            }
            serde_json::Value::Array(xs) => {
                for x in xs {
                    walk(x, out);
                }
            }
            _ => {}
        }
    }
    walk(v, &mut out);
    out
}

fn parse_block(session: &str, body: &str) -> Injection {
    let mut entries = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if !line.starts_with('[') || !line.ends_with(')') {
            continue;
        }
        let Some(close) = line.find("](") else {
            continue;
        };
        let title = &line[1..close];
        let uri = &line[close + 2..line.len() - 1];
        if uri.contains("://") {
            entries.push(Entry {
                title: title.to_string(),
                uri: uri.to_string(),
            });
        }
    }
    Injection {
        session: session.to_string(),
        entries,
        body_bytes: body.len(),
    }
}
