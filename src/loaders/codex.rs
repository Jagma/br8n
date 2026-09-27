use super::transcript::{mentioned_files, result_line, tool_line, SessionAgent, TOOL_TARGET_KEYS};
use crate::model::{Document, SourceType};
use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct CodexSessionLoader;

#[derive(Default)]
struct Session {
    id: String,
    project: String,
    started: String,
    first_timestamp: String,
    sections: Vec<(&'static str, Vec<String>)>,
}

impl Session {
    fn message(&mut self, role: &'static str, text: String) {
        self.sections.push((role, vec![text]));
    }

    fn tool(&mut self, line: String) {
        match self.sections.last_mut() {
            Some(("assistant", lines)) => lines.push(line),
            _ => self.sections.push(("assistant", vec![line])),
        }
    }

    fn markdown(&self) -> String {
        let mut md = String::new();
        for (turn, (role, lines)) in self.sections.iter().enumerate() {
            md.push_str(&format!(
                "## {role} (turn {})\n\n{}\n\n",
                turn + 1,
                lines.join("\n").trim()
            ));
        }
        md
    }
}

impl CodexSessionLoader {
    pub fn default_root() -> PathBuf {
        crate::setup::agents::codex_home().join("sessions")
    }

    pub fn load_session(path: &Path) -> Result<Document> {
        let raw = std::fs::read_to_string(path)?;
        let mut session = Session::default();
        for line in raw.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            read_line(&mut session, &v);
        }
        let md = session.markdown();
        anyhow::ensure!(!md.trim().is_empty(), "empty transcript");

        let id = if session.id.is_empty() {
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(id_from_file_stem)
                .unwrap_or_else(|| "session".to_string())
        } else {
            session.id.clone()
        };
        let short: String = id.chars().take(8).collect();
        let title = if session.project.is_empty() {
            format!("Codex session {short}")
        } else {
            let name = Path::new(&session.project)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(session.project.as_str());
            format!("{name} codex session ({short})")
        };
        let started = if session.started.is_empty() {
            session.first_timestamp.clone()
        } else {
            session.started.clone()
        };
        let uri = SessionAgent::Codex.uri_for(&path.canonicalize()?);
        let mut doc = Document::new(SourceType::Transcript, &uri, &title, &md);
        doc.meta = serde_json::json!({
            "project": session.project,
            "started": started,
            "files": mentioned_files(&md),
            "agent": SessionAgent::Codex.id(),
            "session_id": id,
        });
        Ok(doc)
    }
}

pub(crate) fn id_from_file_stem(stem: &str) -> String {
    const UUID_LEN: usize = 36;
    let chars: Vec<char> = stem.chars().collect();
    if chars.len() > UUID_LEN && stem.starts_with("rollout-") {
        chars[chars.len() - UUID_LEN..].iter().collect()
    } else {
        stem.to_string()
    }
}

fn read_line(session: &mut Session, v: &Value) {
    if session.first_timestamp.is_empty() {
        if let Some(t) = v["timestamp"].as_str() {
            session.first_timestamp = t.to_string();
        }
    }
    match v["type"].as_str() {
        Some("session_meta") => read_meta(session, &v["payload"]),
        Some("turn_context") => {
            if session.project.is_empty() {
                if let Some(cwd) = v["payload"]["cwd"].as_str() {
                    session.project = cwd.to_string();
                }
            }
        }
        Some("response_item") => read_item(session, &v["payload"]),
        Some(_) => read_item(session, v),
        None if v["id"].is_string() && session.id.is_empty() => read_meta(session, v),
        None => {}
    }
}

fn read_meta(session: &mut Session, meta: &Value) {
    if session.id.is_empty() {
        if let Some(id) = meta["id"].as_str().or(meta["session_id"].as_str()) {
            session.id = id.to_string();
        }
    }
    if session.project.is_empty() {
        if let Some(cwd) = meta["cwd"].as_str() {
            session.project = cwd.to_string();
        }
    }
    if session.started.is_empty() {
        if let Some(t) = meta["timestamp"].as_str() {
            session.started = t.to_string();
        }
    }
}

fn read_item(session: &mut Session, item: &Value) {
    match item["type"].as_str() {
        Some("message") => {
            let role = match item["role"].as_str() {
                Some("user") => "user",
                Some("assistant") => "assistant",
                _ => return,
            };
            let text = message_text(role, &item["content"]);
            if !text.trim().is_empty() {
                session.message(role, text);
            }
        }
        Some("function_call") => {
            let name = item["name"].as_str().unwrap_or("tool");
            let args = match &item["arguments"] {
                Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
                other => other.clone(),
            };
            session.tool(tool_line(name, &call_target(&args)));
        }
        Some("local_shell_call") => {
            session.tool(tool_line(
                "shell",
                &command_text(&item["action"]["command"]),
            ));
        }
        Some("custom_tool_call") => {
            let name = item["name"].as_str().unwrap_or("tool");
            let target = item["input"].as_str().map(patched_file).unwrap_or_default();
            session.tool(tool_line(name, target));
        }
        Some("web_search_call") => {
            let query = item["action"]["query"].as_str().unwrap_or_default();
            session.tool(tool_line("web_search", query));
        }
        Some("function_call_output") | Some("custom_tool_call_output") => {
            if let Some(line) = result_line(&output_text(&item["output"])) {
                session.tool(line);
            }
        }
        _ => {}
    }
}

fn message_text(role: &str, content: &Value) -> String {
    let parts: Vec<&str> = match content {
        Value::String(s) => vec![s.as_str()],
        Value::Array(items) => items
            .iter()
            .filter(|c| {
                matches!(
                    c["type"].as_str(),
                    Some("input_text") | Some("output_text") | Some("text") | None
                )
            })
            .filter_map(|c| c["text"].as_str())
            .collect(),
        _ => Vec::new(),
    };
    parts
        .into_iter()
        .filter(|t| !t.trim().is_empty())
        .filter(|t| role != "user" || !is_injected_context(t))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn is_injected_context(text: &str) -> bool {
    let t = text.trim();
    if t.starts_with("# AGENTS.md instructions") {
        return true;
    }
    let Some(rest) = t.strip_prefix('<') else {
        return false;
    };
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if name.is_empty() {
        return false;
    }
    t.to_ascii_lowercase()
        .ends_with(&format!("</{}>", name.to_ascii_lowercase()))
}

fn call_target(args: &Value) -> String {
    for key in TOOL_TARGET_KEYS.iter().chain(["cmd"].iter()) {
        match &args[*key] {
            Value::String(s) if !s.is_empty() => return s.clone(),
            Value::Array(_) => {
                let joined = command_text(&args[*key]);
                if !joined.is_empty() {
                    return joined;
                }
            }
            _ => {}
        }
    }
    String::new()
}

fn command_text(command: &Value) -> String {
    let Some(parts) = command.as_array() else {
        return command.as_str().unwrap_or_default().to_string();
    };
    let words: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
    match words.as_slice() {
        [shell, flag, script]
            if flag.starts_with('-') && flag.ends_with('c') && is_shell(shell) =>
        {
            script.to_string()
        }
        _ => words.join(" "),
    }
}

fn is_shell(program: &str) -> bool {
    matches!(
        Path::new(program).file_name().and_then(|s| s.to_str()),
        Some("bash" | "sh" | "zsh" | "fish" | "pwsh" | "powershell")
    )
}

fn patched_file(input: &str) -> &str {
    input
        .lines()
        .find_map(|l| {
            ["*** Update File: ", "*** Add File: ", "*** Delete File: "]
                .iter()
                .find_map(|p| l.strip_prefix(p))
        })
        .map(str::trim)
        .unwrap_or_default()
}

fn output_text(output: &Value) -> String {
    match output {
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Object(o)) => o
                .get("output")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| s.clone()),
            _ => s.clone(),
        },
        Value::Array(items) => items
            .iter()
            .filter_map(|c| c["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(o) => o
            .get("content")
            .or_else(|| o.get("output"))
            .map(output_text)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

impl super::Loader for CodexSessionLoader {
    fn load(&self, uri: &str) -> Result<Vec<Document>> {
        let path = uri
            .strip_prefix(SessionAgent::Codex.scheme())
            .unwrap_or(uri);
        Ok(vec![Self::load_session(Path::new(path))?])
    }
}
