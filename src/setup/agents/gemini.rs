use super::json_config::{McpJsonAgent, PromptHook};
use super::{Agent, AgentEnv, Capabilities, Change, ConnectOptions, Detected, Status};
use anyhow::Result;
use std::path::{Path, PathBuf};

pub struct Gemini;

pub const HOOK_EVENT: &str = "BeforeAgent";
pub const HOOK_ARGS: &str = "hook prompt --agent gemini";
pub const HOOK_TIMEOUT_MS: u64 = 5000;

impl Gemini {
    pub fn config_file(env: &AgentEnv) -> PathBuf {
        env.home.join(".gemini/settings.json")
    }

    pub fn prompt_hook(env: &AgentEnv) -> PromptHook<'static> {
        PromptHook {
            event: HOOK_EVENT,
            command: super::quoted_command(&env.bin_str(), HOOK_ARGS),
            marker: format!(" {HOOK_ARGS}"),
            timeout: HOOK_TIMEOUT_MS,
            name: Some("br8n"),
        }
    }

    fn file_agent<'a>(file: &'a Path, env: &AgentEnv) -> McpJsonAgent<'a> {
        McpJsonAgent {
            file,
            extra: &[],
            hook: Some(Self::prompt_hook(env)),
        }
    }
}

impl Agent for Gemini {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn display_name(&self) -> &'static str {
        "Gemini CLI"
    }

    fn detect(&self, env: &AgentEnv) -> Detected {
        super::detect_by(
            env,
            &["gemini"],
            &[env.home.join(".gemini")],
            Self::config_file(env),
        )
    }

    fn status(&self, env: &AgentEnv) -> Status {
        Self::file_agent(&Self::config_file(env), env).status(env)
    }

    fn connect(&self, env: &AgentEnv, _opts: &ConnectOptions) -> Result<Change> {
        Self::file_agent(&Self::config_file(env), env).connect(env)
    }

    fn disconnect(&self, env: &AgentEnv) -> Result<Change> {
        Self::file_agent(&Self::config_file(env), env).disconnect()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            mcp: true,
            prompt_hook: true,
            ..Capabilities::default()
        }
    }

    fn snippet(&self, env: &AgentEnv) -> Option<String> {
        Some(super::mcp_json_snippet(env))
    }
}
