use super::files::{self, unparseable, Json};
use super::{Change, Status, MCP_ARGS, SERVER_NAME};
use anyhow::Result;
use std::path::Path;

pub const SERVERS_KEY: &str = "mcpServers";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryState {
    Missing,
    Matches,
    Differs(String),
}

pub struct JsonFile {
    pub doc: Json,
    pub indent: String,
}

pub fn load(file: &Path) -> Result<JsonFile> {
    match files::read_optional(file)? {
        None => Ok(JsonFile {
            doc: Json::object(),
            indent: "  ".to_string(),
        }),
        Some(text) => {
            let doc = Json::parse(&text).map_err(|e| unparseable(file, "JSON", e))?;
            if !doc.is_object() {
                return Err(unparseable(file, "JSON", "the top level is not an object"));
            }
            Ok(JsonFile {
                doc,
                indent: files::detect_indent(&text),
            })
        }
    }
}

pub fn save(file: &Path, loaded: &JsonFile, change: &mut Change) -> Result<()> {
    let written = files::write_atomic(file, &loaded.doc.render(&loaded.indent))?;
    change.wrote(file, written);
    Ok(())
}

fn shape_error(file: &Path, what: &str) -> anyhow::Error {
    unparseable(file, "an agent configuration", what)
}

pub fn server_state(doc: &Json, bin: &str) -> EntryState {
    let Some(entry) = doc.get(SERVERS_KEY).and_then(|s| s.get(SERVER_NAME)) else {
        return EntryState::Missing;
    };
    let command = entry.get("command").and_then(Json::as_str);
    let args = entry.get("args").and_then(Json::strings);
    if command != Some(bin) {
        return EntryState::Differs(format!(
            "the `{SERVER_NAME}` server runs {}, not {bin}",
            command.unwrap_or("nothing")
        ));
    }
    if args.as_deref() != Some(&MCP_ARGS[..]) {
        return EntryState::Differs(format!(
            "the `{SERVER_NAME}` server's args are {}, not [\"mcp\"]",
            args.map(|a| format!("{a:?}"))
                .unwrap_or_else(|| "missing".to_string())
        ));
    }
    EntryState::Matches
}

pub fn upsert_server(doc: &mut Json, bin: &str, extra: &[(&str, &str)]) -> Result<(), String> {
    if doc.get(SERVERS_KEY).is_none() {
        doc.set(SERVERS_KEY, Json::object());
    }
    let servers = doc
        .get_mut(SERVERS_KEY)
        .filter(|s| s.is_object())
        .ok_or_else(|| format!("`{SERVERS_KEY}` is not an object"))?;
    match servers.get_mut(SERVER_NAME) {
        Some(entry) if entry.is_object() => {
            entry.set("command", Json::String(bin.to_string()));
            entry.set("args", Json::string_array(&MCP_ARGS));
        }
        Some(_) => return Err(format!("`{SERVERS_KEY}.{SERVER_NAME}` is not an object")),
        None => {
            let mut entry = Json::object();
            for (k, v) in extra {
                entry.set(k, Json::String(v.to_string()));
            }
            entry.set("command", Json::String(bin.to_string()));
            entry.set("args", Json::string_array(&MCP_ARGS));
            servers.set(SERVER_NAME, entry);
        }
    }
    Ok(())
}

pub fn remove_server(doc: &mut Json) -> bool {
    let Some(servers) = doc.get_mut(SERVERS_KEY) else {
        return false;
    };
    if servers.remove(SERVER_NAME).is_none() {
        return false;
    }
    if servers.is_empty_object() {
        doc.remove(SERVERS_KEY);
    }
    true
}

pub struct PromptHook<'a> {
    pub event: &'a str,
    pub command: String,
    pub marker: String,
    pub timeout: u64,
    pub name: Option<&'a str>,
}

fn hook_commands(doc: &Json, event: &str) -> Vec<String> {
    let Some(Json::Array(groups)) = doc.get("hooks").and_then(|h| h.get(event)) else {
        return Vec::new();
    };
    groups
        .iter()
        .filter_map(|g| match g.get("hooks") {
            Some(Json::Array(hooks)) => Some(hooks),
            _ => None,
        })
        .flatten()
        .filter_map(|h| h.get("command").and_then(Json::as_str).map(str::to_string))
        .collect()
}

pub fn hook_state(doc: &Json, hook: &PromptHook) -> EntryState {
    let ours: Vec<String> = hook_commands(doc, hook.event)
        .into_iter()
        .filter(|c| c.contains(&hook.marker))
        .collect();
    if ours.is_empty() {
        EntryState::Missing
    } else if ours.contains(&hook.command) {
        EntryState::Matches
    } else {
        EntryState::Differs(format!(
            "the {} hook runs `{}`, not `{}`",
            hook.event, ours[0], hook.command
        ))
    }
}

pub fn upsert_hook(doc: &mut Json, hook: &PromptHook) -> Result<(), String> {
    if hook_state(doc, hook) == EntryState::Matches {
        return Ok(());
    }
    if doc.get("hooks").is_none() {
        doc.set("hooks", Json::object());
    }
    let hooks = doc
        .get_mut("hooks")
        .filter(|h| h.is_object())
        .ok_or("`hooks` is not an object")?;
    if hooks.get(hook.event).is_none() {
        hooks.set(hook.event, Json::Array(Vec::new()));
    }
    let Some(Json::Array(groups)) = hooks.get_mut(hook.event) else {
        return Err(format!("`hooks.{}` is not an array", hook.event));
    };
    for group in groups.iter_mut() {
        if let Some(Json::Array(entries)) = group.get_mut("hooks") {
            for entry in entries.iter_mut() {
                let ours = entry
                    .get("command")
                    .and_then(Json::as_str)
                    .is_some_and(|c| c.contains(&hook.marker));
                if ours {
                    entry.set("command", Json::String(hook.command.clone()));
                    return Ok(());
                }
            }
        }
    }
    let mut entry = Json::object();
    if let Some(name) = hook.name {
        entry.set("name", Json::String(name.to_string()));
    }
    entry.set("type", Json::String("command".to_string()));
    entry.set("command", Json::String(hook.command.clone()));
    entry.set("timeout", Json::Number(hook.timeout.into()));
    let mut group = Json::object();
    group.set("hooks", Json::Array(vec![entry]));
    groups.push(group);
    Ok(())
}

pub fn remove_hook(doc: &mut Json, hook: &PromptHook) -> bool {
    let Some(hooks) = doc.get_mut("hooks") else {
        return false;
    };
    let Some(Json::Array(groups)) = hooks.get_mut(hook.event) else {
        return false;
    };
    let mut removed = false;
    for group in groups.iter_mut() {
        if let Some(Json::Array(entries)) = group.get_mut("hooks") {
            let before = entries.len();
            entries.retain(|e| {
                !e.get("command")
                    .and_then(Json::as_str)
                    .is_some_and(|c| c.contains(&hook.marker))
            });
            removed |= entries.len() != before;
        }
    }
    if !removed {
        return false;
    }
    groups.retain(|g| !matches!(g.get("hooks"), Some(Json::Array(e)) if e.is_empty()));
    if groups.is_empty() {
        hooks.remove(hook.event);
    }
    if hooks.is_empty_object() {
        doc.remove("hooks");
    }
    true
}

pub fn combine(states: &[EntryState], what: &[&str]) -> Status {
    if states.iter().all(|s| *s == EntryState::Matches) {
        return Status::Connected;
    }
    if states.iter().all(|s| *s == EntryState::Missing) {
        return Status::NotConnected;
    }
    let reasons: Vec<String> = states
        .iter()
        .zip(what)
        .filter_map(|(s, w)| match s {
            EntryState::Matches => None,
            EntryState::Missing => Some(format!("the {w} is missing")),
            EntryState::Differs(r) => Some(r.clone()),
        })
        .collect();
    Status::Stale(reasons.join("; "))
}

pub struct McpJsonAgent<'a> {
    pub file: &'a Path,
    pub extra: &'a [(&'a str, &'a str)],
    pub hook: Option<PromptHook<'a>>,
}

impl McpJsonAgent<'_> {
    fn states(&self, doc: &Json, bin: &str) -> (Vec<EntryState>, Vec<&'static str>) {
        let mut states = vec![server_state(doc, bin)];
        let mut what = vec!["MCP server entry"];
        if let Some(h) = &self.hook {
            states.push(hook_state(doc, h));
            what.push("prompt hook");
        }
        (states, what)
    }

    pub fn status(&self, env: &super::AgentEnv) -> Status {
        match load(self.file) {
            Err(e) => Status::Broken(format!("{e:#}")),
            Ok(loaded) => {
                let (states, what) = self.states(&loaded.doc, &env.bin_str());
                super::with_binary_check(combine(&states, &what), env)
            }
        }
    }

    pub fn connect(&self, env: &super::AgentEnv) -> Result<Change> {
        let mut change = Change::default();
        let mut loaded = load(self.file)?;
        let bin = env.bin_str();
        let (states, _) = self.states(&loaded.doc, &bin);
        if states.iter().all(|s| *s == EntryState::Matches) {
            return Ok(change);
        }
        upsert_server(&mut loaded.doc, &bin, self.extra).map_err(|e| shape_error(self.file, &e))?;
        if let Some(h) = &self.hook {
            upsert_hook(&mut loaded.doc, h).map_err(|e| shape_error(self.file, &e))?;
        }
        save(self.file, &loaded, &mut change)?;
        Ok(change)
    }

    pub fn disconnect(&self) -> Result<Change> {
        let mut change = Change::default();
        if !self.file.exists() {
            return Ok(change);
        }
        let mut loaded = load(self.file)?;
        let mut touched = remove_server(&mut loaded.doc);
        if let Some(h) = &self.hook {
            touched |= remove_hook(&mut loaded.doc, h);
        }
        if touched {
            save(self.file, &loaded, &mut change)?;
        }
        Ok(change)
    }
}
