use super::{Agent, AgentEnv, AgentError, Capabilities, Change, ConnectOptions, Detected, Status};
use crate::setup::install::{self, PLUGIN_ID};
use anyhow::{Context, Result};

pub struct ClaudeCode;

pub const ID: &str = "claude-code";
const CONNECTION_CHECKS: [&str; 3] = ["plugin", "marketplace", "registration"];

fn plugin_version(env: &AgentEnv) -> Option<String> {
    let manifest = env.paths.plugin.join(".claude-plugin/plugin.json");
    std::fs::read_to_string(manifest)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["version"].as_str().map(str::to_string))
}

impl Agent for ClaudeCode {
    fn id(&self) -> &'static str {
        ID
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn detect(&self, env: &AgentEnv) -> Detected {
        let version = super::program_version(env.claude.program());
        Detected {
            installed: version.is_some(),
            version,
            config_path: Some(env.paths.plugin.clone()),
        }
    }

    fn status(&self, env: &AgentEnv) -> Status {
        if !env.claude.available() {
            return Status::NotConnected;
        }
        let checks = install::install_checks(&env.paths, &env.claude, &env.version, env.bin());
        let relevant: Vec<_> = checks
            .iter()
            .filter(|c| CONNECTION_CHECKS.contains(&c.name))
            .collect();
        let registered = relevant
            .iter()
            .any(|c| c.ok && (c.name == "registration" || c.name == "marketplace"));
        let status = if relevant.iter().all(|c| c.ok) {
            Status::Connected
        } else if !registered {
            Status::NotConnected
        } else {
            Status::Stale(
                relevant
                    .iter()
                    .filter(|c| !c.ok)
                    .map(|c| c.detail.clone())
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        };
        super::with_binary_check(status, env)
    }

    fn connect(&self, env: &AgentEnv, _opts: &ConnectOptions) -> Result<Change> {
        if !env.claude.available() {
            return Err(AgentError::Unavailable(
                "the `claude` command was not found; install Claude Code (https://claude.com/claude-code) first".to_string(),
            )
            .into());
        }
        let mut change = Change::default();
        if plugin_version(env).as_deref() != Some(env.version.as_str()) {
            if env.paths.plugin.exists() {
                std::fs::remove_dir_all(&env.paths.plugin)
                    .with_context(|| format!("remove {}", env.paths.plugin.display()))?;
            }
            crate::setup::plugin::write(&env.paths.plugin, &env.version, env.bin())
                .with_context(|| format!("write the plugin to {}", env.paths.plugin.display()))?;
            change.files.push(env.paths.plugin.clone());
        }
        change
            .notes
            .extend(install::connect_claude_code(&env.paths, &env.claude)?);
        change
            .notes
            .push("restart Claude Code so the plugin loads".to_string());
        Ok(change)
    }

    fn disconnect(&self, env: &AgentEnv) -> Result<Change> {
        if !env.claude.available() {
            return Err(AgentError::Unavailable(
                "the `claude` command was not found; the plugin registration was left as it is"
                    .to_string(),
            )
            .into());
        }
        let notes = install::disconnect_claude_code(&env.paths, &env.claude)?;
        Ok(Change {
            notes,
            ..Change::default()
        })
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            mcp: true,
            prompt_hook: true,
            session_hook: true,
            transcripts: true,
            instructions: false,
        }
    }

    fn snippet(&self, env: &AgentEnv) -> Option<String> {
        Some(format!(
            "claude plugin marketplace add \"{}\"\nclaude plugin install {PLUGIN_ID}\n",
            env.paths.plugin.display()
        ))
    }
}
