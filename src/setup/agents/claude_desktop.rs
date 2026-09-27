use super::json_config::McpJsonAgent;
use super::{Agent, AgentEnv, Capabilities, Change, ConnectOptions, Detected, Status};
use anyhow::Result;
use std::path::{Path, PathBuf};

pub struct ClaudeDesktop;

impl ClaudeDesktop {
    pub fn config_dir(env: &AgentEnv) -> PathBuf {
        env.app_config.join("Claude")
    }

    pub fn config_file(env: &AgentEnv) -> PathBuf {
        Self::config_dir(env).join("claude_desktop_config.json")
    }

    fn file_agent(file: &Path) -> McpJsonAgent<'_> {
        McpJsonAgent {
            file,
            extra: &[],
            hook: None,
        }
    }
}

impl Agent for ClaudeDesktop {
    fn id(&self) -> &'static str {
        "claude-desktop"
    }

    fn display_name(&self) -> &'static str {
        "Claude Desktop"
    }

    fn detect(&self, env: &AgentEnv) -> Detected {
        let mut dirs = vec![Self::config_dir(env)];
        if cfg!(target_os = "macos") {
            dirs.push(PathBuf::from("/Applications/Claude.app"));
        }
        super::detect_by(env, &[], &dirs, Self::config_file(env))
    }

    fn status(&self, env: &AgentEnv) -> Status {
        Self::file_agent(&Self::config_file(env)).status(env)
    }

    fn connect(&self, env: &AgentEnv, _opts: &ConnectOptions) -> Result<Change> {
        let mut change = Self::file_agent(&Self::config_file(env)).connect(env)?;
        if !change.is_empty() {
            change
                .notes
                .push("quit and reopen Claude Desktop to load it".to_string());
        }
        Ok(change)
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
