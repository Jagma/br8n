use super::json_config::McpJsonAgent;
use super::{Agent, AgentEnv, Capabilities, Change, ConnectOptions, Detected, Status};
use anyhow::Result;
use std::path::{Path, PathBuf};

pub struct Cursor;

pub const EXTRA: [(&str, &str); 1] = [("type", "stdio")];

impl Cursor {
    pub fn config_file(env: &AgentEnv) -> PathBuf {
        env.home.join(".cursor/mcp.json")
    }

    fn file_agent(file: &Path) -> McpJsonAgent<'_> {
        McpJsonAgent {
            file,
            extra: &EXTRA,
            hook: None,
        }
    }
}

impl Agent for Cursor {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn display_name(&self) -> &'static str {
        "Cursor"
    }

    fn detect(&self, env: &AgentEnv) -> Detected {
        super::detect_by(
            env,
            &["cursor", "cursor-agent"],
            &[env.home.join(".cursor")],
            Self::config_file(env),
        )
    }

    fn status(&self, env: &AgentEnv) -> Status {
        Self::file_agent(&Self::config_file(env)).status(env)
    }

    fn connect(&self, env: &AgentEnv, _opts: &ConnectOptions) -> Result<Change> {
        Self::file_agent(&Self::config_file(env)).connect(env)
    }

    fn disconnect(&self, env: &AgentEnv) -> Result<Change> {
        Self::file_agent(&Self::config_file(env)).disconnect()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            mcp: true,
            ..Capabilities::default()
        }
    }

    fn snippet(&self, env: &AgentEnv) -> Option<String> {
        Some(super::mcp_json_snippet(env))
    }
}
