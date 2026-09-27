use anyhow::{Context, Result};
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const BACKUP_SUFFIX: &str = ".br8n-bak";

#[derive(Debug, thiserror::Error)]
#[error("{} cannot be read as {format}, so br8n left it untouched: {detail}", file.display())]
pub struct Unparseable {
    pub file: PathBuf,
    pub format: &'static str,
    pub detail: String,
}

pub fn unparseable(file: &Path, format: &'static str, detail: impl ToString) -> anyhow::Error {
    Unparseable {
        file: file.to_path_buf(),
        format,
        detail: detail.to_string(),
    }
    .into()
}

pub fn backup_path(file: &Path) -> PathBuf {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(BACKUP_SUFFIX);
    file.with_file_name(name)
}

pub fn read_optional(file: &Path) -> Result<Option<String>> {
    match std::fs::read(file) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|e| unparseable(file, "UTF-8 text", e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", file.display())),
    }
}

#[derive(Debug, Default)]
pub struct Written {
    pub backup: Option<PathBuf>,
}

pub fn write_atomic(file: &Path, contents: &str) -> Result<Written> {
    let target = if file.symlink_metadata().is_ok() {
        std::fs::canonicalize(file).with_context(|| format!("resolve {}", file.display()))?
    } else {
        file.to_path_buf()
    };
    let parent = target
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&parent).with_context(|| format!("create {}", parent.display()))?;
    let mut written = Written::default();
    let permissions = match std::fs::metadata(&target) {
        Ok(meta) => {
            let backup = backup_path(file);
            if backup.symlink_metadata().is_err() {
                std::fs::copy(&target, &backup).with_context(|| {
                    format!("back up {} to {}", target.display(), backup.display())
                })?;
                written.backup = Some(backup);
            }
            Some(meta.permissions())
        }
        Err(_) => None,
    };
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let staged = parent.join(format!(".{name}.br8n-tmp-{}", std::process::id()));
    let result = (|| -> Result<()> {
        let mut f = std::fs::File::create(&staged)
            .with_context(|| format!("create {}", staged.display()))?;
        f.write_all(contents.as_bytes())
            .with_context(|| format!("write {}", staged.display()))?;
        if let Some(p) = &permissions {
            f.set_permissions(p.clone())
                .with_context(|| format!("set permissions on {}", staged.display()))?;
        }
        f.sync_all()
            .with_context(|| format!("sync {}", staged.display()))?;
        std::fs::rename(&staged, &target)
            .with_context(|| format!("rename {} to {}", staged.display(), target.display()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result.map(|_| written)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn object() -> Json {
        Json::Object(Vec::new())
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Json> {
        match self {
            Json::Object(entries) => entries.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn set(&mut self, key: &str, value: Json) {
        if let Json::Object(entries) = self {
            match entries.iter_mut().find(|(k, _)| k == key) {
                Some((_, v)) => *v = value,
                None => entries.push((key.to_string(), value)),
            }
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<Json> {
        match self {
            Json::Object(entries) => {
                let at = entries.iter().position(|(k, _)| k == key)?;
                Some(entries.remove(at).1)
            }
            _ => None,
        }
    }

    pub fn is_object(&self) -> bool {
        matches!(self, Json::Object(_))
    }

    pub fn is_empty_object(&self) -> bool {
        matches!(self, Json::Object(e) if e.is_empty())
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn strings(&self) -> Option<Vec<&str>> {
        match self {
            Json::Array(items) => items.iter().map(Json::as_str).collect(),
            _ => None,
        }
    }

    pub fn string_array(items: &[&str]) -> Json {
        Json::Array(items.iter().map(|s| Json::String(s.to_string())).collect())
    }

    pub fn parse(text: &str) -> std::result::Result<Json, serde_json::Error> {
        if text.trim().is_empty() {
            return Ok(Json::object());
        }
        serde_json::from_str(text)
    }

    pub fn render(&self, indent: &str) -> String {
        let mut out = Vec::new();
        let formatter = serde_json::ser::PrettyFormatter::with_indent(indent.as_bytes());
        let mut ser = serde_json::Serializer::with_formatter(&mut out, formatter);
        self.serialize(&mut ser).expect("serializing to memory");
        let mut text = String::from_utf8(out).expect("serde_json writes UTF-8");
        text.push('\n');
        text
    }
}

pub fn detect_indent(text: &str) -> String {
    text.lines()
        .map(|l| {
            let trimmed = l.trim_start_matches([' ', '\t']);
            &l[..l.len() - trimmed.len()]
        })
        .find(|lead| !lead.is_empty())
        .unwrap_or("  ")
        .to_string()
}

impl Serialize for Json {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Json::Null => s.serialize_unit(),
            Json::Bool(b) => s.serialize_bool(*b),
            Json::Number(n) => n.serialize(s),
            Json::String(v) => s.serialize_str(v),
            Json::Array(items) => {
                let mut seq = s.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Json::Object(entries) => {
                let mut map = s.serialize_map(Some(entries.len()))?;
                for (k, v) in entries {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}

struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Json;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_unit<E>(self) -> std::result::Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_none<E>(self) -> std::result::Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_bool<E>(self, v: bool) -> std::result::Result<Json, E> {
        Ok(Json::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> std::result::Result<Json, E> {
        Ok(Json::Number(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> std::result::Result<Json, E> {
        Ok(Json::Number(v.into()))
    }

    fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Json, E> {
        serde_json::Number::from_f64(v)
            .map(Json::Number)
            .ok_or_else(|| E::custom("a number JSON cannot represent"))
    }

    fn visit_str<E>(self, v: &str) -> std::result::Result<Json, E> {
        Ok(Json::String(v.to_string()))
    }

    fn visit_string<E>(self, v: String) -> std::result::Result<Json, E> {
        Ok(Json::String(v))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Json, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Json::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Json, A::Error> {
        let mut entries: Vec<(String, Json)> = Vec::new();
        while let Some((k, v)) = map.next_entry::<String, Json>()? {
            match entries.iter_mut().find(|(key, _)| *key == k) {
                Some((_, existing)) => *existing = v,
                None => entries.push((k, v)),
            }
        }
        Ok(Json::Object(entries))
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Json, D::Error> {
        d.deserialize_any(JsonVisitor)
    }
}
