use super::check::{check, FieldError};
use super::schema::{config_schema, Leaf, Node};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, TableLike, Value};

#[derive(Debug, Clone, Default)]
pub struct Patch {
    pub set: Vec<(String, Item)>,
    pub unset: Vec<String>,
}

#[derive(Debug)]
pub enum Outcome {
    Written { etag: String },
    Conflict { etag: String },
    Invalid(Vec<FieldError>),
}

pub fn read_document(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn etag_of(text: Option<&str>) -> String {
    text.map(|t| hex::encode(Sha256::digest(t.as_bytes())))
        .unwrap_or_default()
}

pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    path.with_file_name(name)
}

impl Patch {
    pub fn from_json(
        set: &serde_json::Map<String, serde_json::Value>,
        unset: &[String],
    ) -> std::result::Result<Patch, Vec<FieldError>> {
        let mut patch = Patch {
            set: Vec::new(),
            unset: unset.to_vec(),
        };
        let mut errors = Vec::new();
        for (path, json) in set {
            if json.is_null() {
                patch.unset.push(path.clone());
                continue;
            }
            match item_from_json(json) {
                Ok(item) => patch.set.push((path.clone(), item)),
                Err(message) => errors.push(FieldError::new(path, message)),
            }
        }
        if errors.is_empty() {
            Ok(patch)
        } else {
            Err(errors)
        }
    }
}

pub fn value_from_cli(raw: &str) -> Item {
    match raw.parse::<Value>() {
        Ok(value) => Item::Value(value),
        Err(_) => toml_edit::value(raw),
    }
}

fn item_from_json(json: &serde_json::Value) -> std::result::Result<Item, String> {
    match json {
        serde_json::Value::Object(map) => {
            let mut table = Table::new();
            for (key, child) in map {
                if !child.is_null() {
                    table.insert(key, item_from_json(child)?);
                }
            }
            Ok(Item::Table(table))
        }
        other => value_from_json(other).map(Item::Value),
    }
}

fn value_from_json(json: &serde_json::Value) -> std::result::Result<Value, String> {
    Ok(match json {
        serde_json::Value::Null => return Err("null is only allowed as a whole value".into()),
        serde_json::Value::Bool(b) => Value::from(*b),
        serde_json::Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => Value::from(i),
            (None, Some(f)) if n.is_f64() => Value::from(f),
            _ => return Err(format!("{n} is too large for a TOML integer")),
        },
        serde_json::Value::String(s) => Value::from(s.as_str()),
        serde_json::Value::Array(items) => {
            let mut array = Array::new();
            for item in items {
                array.push_formatted(value_from_json(item)?);
            }
            Value::Array(array)
        }
        serde_json::Value::Object(map) => {
            let mut table = InlineTable::new();
            for (key, child) in map {
                if !child.is_null() {
                    table.insert(key, value_from_json(child)?);
                }
            }
            Value::InlineTable(table)
        }
    })
}

fn segments(path: &str) -> Vec<&str> {
    path.split('.').map(str::trim).collect()
}

fn coerce_to_schema(path: &[&str], item: Item) -> Item {
    let expects_float = matches!(config_schema().at(path), Some(Node::Leaf(Leaf::Float)));
    match item {
        Item::Value(Value::Integer(i)) if expects_float => {
            let mut float = Value::from(*i.value() as f64);
            *float.decor_mut() = i.decor().clone();
            Item::Value(float)
        }
        other => other,
    }
}

pub fn apply_patch(
    doc: &mut DocumentMut,
    patch: &Patch,
) -> std::result::Result<(), Vec<FieldError>> {
    let mut errors = Vec::new();
    for path in &patch.unset {
        if let Err(message) = unset_path(doc, &segments(path)) {
            errors.push(FieldError::new(path, message));
        }
    }
    for (path, item) in &patch.set {
        let parts = segments(path);
        if parts.iter().any(|p| p.is_empty()) {
            errors.push(FieldError::new(path, "empty key in dotted path"));
            continue;
        }
        let item = coerce_to_schema(&parts, item.clone());
        if let Err(message) = set_path(doc, &parts, item) {
            errors.push(FieldError::new(path, message));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn set_path(doc: &mut DocumentMut, parts: &[&str], item: Item) -> std::result::Result<(), String> {
    let (leaf, parents) = parts.split_last().ok_or("empty path")?;
    let mut current: &mut dyn TableLike = doc.as_table_mut();
    for (depth, segment) in parents.iter().enumerate() {
        if current.get(segment).is_none() {
            let mut table = Table::new();
            table.set_implicit(true);
            current.insert(segment, Item::Table(table));
        }
        current = current
            .get_mut(segment)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| format!("`{}` is not a table", parents[..=depth].join(".")))?;
    }
    match (current.get_mut(leaf), item) {
        (Some(Item::Value(old)), Item::Value(mut new)) => {
            *new.decor_mut() = old.decor().clone();
            *old = new;
        }
        (Some(existing), item) if !existing.is_none() => *existing = item,
        (_, item) => {
            current.insert(leaf, item);
        }
    }
    Ok(())
}

fn unset_path(doc: &mut DocumentMut, parts: &[&str]) -> std::result::Result<(), String> {
    let (leaf, parents) = parts.split_last().ok_or("empty path")?;
    let mut current: &mut dyn TableLike = doc.as_table_mut();
    for segment in parents {
        match current.get_mut(segment).and_then(Item::as_table_like_mut) {
            Some(table) => current = table,
            None => return Ok(()),
        }
    }
    current.remove(leaf);
    Ok(())
}

pub fn patched_text(
    current: Option<&str>,
    patch: &Patch,
) -> std::result::Result<String, Vec<FieldError>> {
    let mut doc: DocumentMut =
        current
            .unwrap_or("")
            .parse()
            .map_err(|e: toml_edit::TomlError| {
                vec![super::check::syntax_error(current.unwrap_or(""), &e)]
            })?;
    apply_patch(&mut doc, patch)?;
    Ok(doc.to_string())
}

pub fn new_errors(before: &[FieldError], after: Vec<FieldError>) -> Vec<FieldError> {
    after
        .into_iter()
        .filter(|e| !before.iter().any(|b| b.same_problem(e)))
        .collect()
}

pub fn update(path: &Path, expected_etag: Option<&str>, patch: &Patch) -> Result<Outcome> {
    let current = read_document(path)?;
    let etag = etag_of(current.as_deref());
    if expected_etag.is_some_and(|expected| expected != etag) {
        return Ok(Outcome::Conflict { etag });
    }
    let next = match patched_text(current.as_deref(), patch) {
        Ok(text) => text,
        Err(errors) => return Ok(Outcome::Invalid(errors)),
    };
    let before = current.as_deref().map(check).unwrap_or_default();
    let introduced = new_errors(&before, check(&next));
    if !introduced.is_empty() {
        return Ok(Outcome::Invalid(introduced));
    }
    write_atomic(path, &next)?;
    Ok(Outcome::Written {
        etag: etag_of(Some(&next)),
    })
}

static TEMP_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn staging_path(path: &Path) -> PathBuf {
    let n = TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()))
}

pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    if let Ok(previous) = std::fs::read(path) {
        let backup = backup_path(path);
        std::fs::write(&backup, previous)
            .with_context(|| format!("writing {}", backup.display()))?;
    }
    replace_file(path, text.as_bytes(), None)
}

pub fn replace_file(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let staged = staging_path(path);
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode.unwrap_or(0o644))
            .open(&staged)
            .with_context(|| format!("creating {}", staged.display()))?;
        let permissions = match (mode, std::fs::metadata(path)) {
            (Some(mode), _) => Some(std::fs::Permissions::from_mode(mode)),
            (None, Ok(meta)) => Some(meta.permissions()),
            (None, Err(_)) => None,
        };
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&staged, path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result?;
    if let Ok(handle) = std::fs::File::open(dir) {
        let _ = handle.sync_all();
    }
    Ok(())
}
