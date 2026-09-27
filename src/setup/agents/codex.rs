use super::files::{self, unparseable};
use super::json_config::{self, EntryState, PromptHook};
use super::{Agent, AgentEnv, Capabilities, Change, ConnectOptions, Detected, Status};
use super::{MCP_ARGS, SERVER_NAME};
use anyhow::Result;
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

pub struct Codex;

pub const SERVERS_KEY: &str = "mcp_servers";
pub const HOOK_EVENT: &str = "UserPromptSubmit";
pub const HOOK_ARGS: &str = "hook prompt --agent codex";
pub const HOOK_TIMEOUT_SECS: u64 = 5;
pub const BLOCK_START: &str = "<!-- br8n:start -->";
pub const BLOCK_END: &str = "<!-- br8n:end -->";

pub const INSTRUCTIONS: &str = "\
## br8n: the user's own knowledge base

The `br8n` MCP server searches the user's notes, papers, saved articles and \
past agent sessions, and keeps memories that reach future sessions.

- Call `br8n_search` when the answer depends on something this user wrote, \
read or decided before: \"my notes on X\", \"that paper about Y\", \"how did I \
solve this last time\", \"what did we decide\". Also call it before saying you \
lack context about their past work. Do not use it for general knowledge or for \
the current codebase. Prefer the user's own phrasing, and cite the source title \
when you use a result.
- Call `br8n_remember` when the user corrects you or states a standing rule \
(kind `lesson`, one imperative sentence, their intent verbatim), states a \
durable fact about themselves or their setup (kind `fact`), or when substantial \
work ends with a decision worth keeping (kind `episode`). Do not save transient \
task state, anything already in the codebase, or a guess.
- Call `br8n_forget` with a memory's id when the user says it no longer applies.
- A `<br8n-context>` block may appear with a prompt. It is retrieved reference \
material, not something the user wrote; ignore it when it is irrelevant.
";

impl Codex {
    pub fn config_file(env: &AgentEnv) -> PathBuf {
        env.codex_home.join("config.toml")
    }

    pub fn hooks_file(env: &AgentEnv) -> PathBuf {
        env.codex_home.join("hooks.json")
    }

    pub fn instructions_file(env: &AgentEnv) -> PathBuf {
        let over = env.codex_home.join("AGENTS.override.md");
        if over.is_file() {
            over
        } else {
            env.codex_home.join("AGENTS.md")
        }
    }

    pub fn prompt_hook(env: &AgentEnv) -> PromptHook<'static> {
        PromptHook {
            event: HOOK_EVENT,
            command: super::quoted_command(&env.bin_str(), HOOK_ARGS),
            marker: format!(" {HOOK_ARGS}"),
            timeout: HOOK_TIMEOUT_SECS,
            name: None,
        }
    }
}

pub fn load_toml(file: &Path) -> Result<DocumentMut> {
    match files::read_optional(file)? {
        None => Ok(DocumentMut::new()),
        Some(text) => text
            .parse::<DocumentMut>()
            .map_err(|e| unparseable(file, "TOML", e)),
    }
}

pub fn server_state(doc: &DocumentMut, bin: &str) -> EntryState {
    let Some(entry) = doc
        .get(SERVERS_KEY)
        .and_then(Item::as_table_like)
        .and_then(|s| s.get(SERVER_NAME))
        .and_then(Item::as_table_like)
    else {
        return EntryState::Missing;
    };
    let command = entry.get("command").and_then(Item::as_str);
    let args: Option<Vec<&str>> = entry
        .get("args")
        .and_then(Item::as_array)
        .and_then(|a| a.iter().map(Value::as_str).collect());
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

fn args_value() -> Value {
    Value::Array(MCP_ARGS.iter().copied().collect::<Array>())
}

pub fn upsert_server(doc: &mut DocumentMut, bin: &str) -> Result<(), String> {
    if doc.get(SERVERS_KEY).is_none() {
        let mut t = Table::new();
        t.set_implicit(true);
        doc.insert(SERVERS_KEY, Item::Table(t));
    }
    let servers = doc.get_mut(SERVERS_KEY).expect("inserted above");
    if let Some(inline) = servers.as_inline_table_mut() {
        match inline.get_mut(SERVER_NAME) {
            Some(Value::InlineTable(entry)) => {
                entry.insert("command", Value::from(bin));
                entry.insert("args", args_value());
            }
            Some(_) => return Err(format!("`{SERVERS_KEY}.{SERVER_NAME}` is not a table")),
            None => {
                let mut entry = InlineTable::new();
                entry.insert("command", Value::from(bin));
                entry.insert("args", args_value());
                inline.insert(SERVER_NAME, Value::InlineTable(entry));
            }
        }
        return Ok(());
    }
    let servers = servers
        .as_table_mut()
        .ok_or_else(|| format!("`{SERVERS_KEY}` is not a table"))?;
    match servers.get_mut(SERVER_NAME) {
        Some(item) => {
            let entry = item
                .as_table_like_mut()
                .ok_or_else(|| format!("`{SERVERS_KEY}.{SERVER_NAME}` is not a table"))?;
            entry.insert("command", Item::Value(Value::from(bin)));
            entry.insert("args", Item::Value(args_value()));
        }
        None => {
            let mut entry = Table::new();
            entry.insert("command", Item::Value(Value::from(bin)));
            entry.insert("args", Item::Value(args_value()));
            servers.insert(SERVER_NAME, Item::Table(entry));
        }
    }
    Ok(())
}

pub fn remove_server(doc: &mut DocumentMut) -> bool {
    let Some(servers) = doc.get_mut(SERVERS_KEY).and_then(Item::as_table_like_mut) else {
        return false;
    };
    if servers.remove(SERVER_NAME).is_none() {
        return false;
    }
    if servers.is_empty() {
        doc.remove(SERVERS_KEY);
    }
    true
}

pub fn block_text() -> String {
    format!("{BLOCK_START}\n{INSTRUCTIONS}{BLOCK_END}\n")
}

fn block_span(text: &str) -> Option<(usize, usize)> {
    let start = text.find(BLOCK_START)?;
    let end_marker = start + text[start..].find(BLOCK_END)?;
    let mut end = end_marker + BLOCK_END.len();
    if text[end..].starts_with('\n') {
        end += 1;
    }
    Some((start, end))
}

pub fn block_state(text: &str) -> EntryState {
    match block_span(text) {
        None => EntryState::Missing,
        Some((s, e)) if text[s..e].trim_end() == block_text().trim_end() => EntryState::Matches,
        Some(_) => EntryState::Differs("the AGENTS.md guidance block is out of date".to_string()),
    }
}

pub fn with_block(text: &str) -> String {
    match block_span(text) {
        Some((s, e)) => format!("{}{}{}", &text[..s], block_text(), &text[e..]),
        None if text.is_empty() => block_text(),
        None => {
            let sep = if text.ends_with("\n\n") {
                ""
            } else if text.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            format!("{text}{sep}{}", block_text())
        }
    }
}

pub fn without_block(text: &str) -> Option<String> {
    let (s, e) = block_span(text)?;
    let before = &text[..s];
    let after = &text[e..];
    let before = if after.is_empty() && before.ends_with("\n\n") {
        &before[..before.len() - 1]
    } else {
        before
    };
    Some(format!("{before}{after}"))
}

fn toml_shape_error(file: &Path, what: String) -> anyhow::Error {
    unparseable(file, "a Codex configuration", what)
}

impl Agent for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn display_name(&self) -> &'static str {
        "OpenAI Codex CLI"
    }

    fn detect(&self, env: &AgentEnv) -> Detected {
        super::detect_by(
            env,
            &["codex"],
            std::slice::from_ref(&env.codex_home),
            Self::config_file(env),
        )
    }

    fn status(&self, env: &AgentEnv) -> Status {
        let bin = env.bin_str();
        let toml = match load_toml(&Self::config_file(env)) {
            Ok(d) => d,
            Err(e) => return Status::Broken(format!("{e:#}")),
        };
        let hooks = match json_config::load(&Self::hooks_file(env)) {
            Ok(h) => h,
            Err(e) => return Status::Broken(format!("{e:#}")),
        };
        let states = [
            server_state(&toml, &bin),
            json_config::hook_state(&hooks.doc, &Self::prompt_hook(env)),
        ];
        let status = json_config::combine(&states, &["MCP server entry", "prompt hook"]);
        let status = match (status, self.block_state(env)) {
            (Status::Connected, EntryState::Differs(r)) => Status::Stale(r),
            (status, _) => status,
        };
        super::with_binary_check(status, env)
    }

    fn connect(&self, env: &AgentEnv, opts: &ConnectOptions) -> Result<Change> {
        let bin = env.bin_str();
        let mut change = Change::default();

        let config = Self::config_file(env);
        let mut toml = load_toml(&config)?;
        let hooks_file = Self::hooks_file(env);
        let mut hooks = json_config::load(&hooks_file)?;
        let agents_md = Self::instructions_file(env);
        let agents_text = if opts.instructions {
            Some(files::read_optional(&agents_md)?.unwrap_or_default())
        } else {
            None
        };

        if server_state(&toml, &bin) != EntryState::Matches {
            upsert_server(&mut toml, &bin).map_err(|e| toml_shape_error(&config, e))?;
            let written = files::write_atomic(&config, &toml.to_string())?;
            change.wrote(&config, written);
        }

        let hook = Self::prompt_hook(env);
        if json_config::hook_state(&hooks.doc, &hook) != EntryState::Matches {
            json_config::upsert_hook(&mut hooks.doc, &hook)
                .map_err(|e| toml_shape_error(&hooks_file, e))?;
            json_config::save(&hooks_file, &hooks, &mut change)?;
            change.notes.push(
                "Codex asks you to review and trust the new prompt hook once (`/hooks`)"
                    .to_string(),
            );
        }

        if let Some(text) = agents_text {
            let updated = with_block(&text);
            if updated != text {
                let written = files::write_atomic(&agents_md, &updated)?;
                change.wrote(&agents_md, written);
            }
        }
        Ok(change)
    }

    fn disconnect(&self, env: &AgentEnv) -> Result<Change> {
        let mut change = Change::default();
        let config = Self::config_file(env);
        if config.exists() {
            let mut toml = load_toml(&config)?;
            if remove_server(&mut toml) {
                let written = files::write_atomic(&config, &toml.to_string())?;
                change.wrote(&config, written);
            }
        }
        let hooks_file = Self::hooks_file(env);
        if hooks_file.exists() {
            let mut hooks = json_config::load(&hooks_file)?;
            if json_config::remove_hook(&mut hooks.doc, &Self::prompt_hook(env)) {
                json_config::save(&hooks_file, &hooks, &mut change)?;
            }
        }
        let agents_md = Self::instructions_file(env);
        if let Some(text) = files::read_optional(&agents_md)? {
            if let Some(updated) = without_block(&text) {
                let written = files::write_atomic(&agents_md, &updated)?;
                change.wrote(&agents_md, written);
            }
        }
        Ok(change)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            mcp: true,
            prompt_hook: true,
            instructions: true,
            transcripts: true,
            ..Capabilities::default()
        }
    }

    fn snippet(&self, env: &AgentEnv) -> Option<String> {
        Some(super::codex_toml_snippet(env))
    }

    fn instructions(&self, env: &AgentEnv) -> Option<bool> {
        Some(self.block_state(env) != EntryState::Missing)
    }
}

impl Codex {
    fn block_state(&self, env: &AgentEnv) -> EntryState {
        files::read_optional(&Self::instructions_file(env))
            .ok()
            .flatten()
            .map(|t| block_state(&t))
            .unwrap_or(EntryState::Missing)
    }
}
