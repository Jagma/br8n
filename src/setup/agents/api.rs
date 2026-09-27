use super::files::Unparseable;
use super::{Agent, AgentEnv, AgentError, Change, ConnectOptions};
use serde_json::{json, Value};

pub fn agent_json(agent: &dyn Agent, env: &AgentEnv) -> Value {
    let status = agent.status(env);
    let mut v = json!({
        "id": agent.id(),
        "name": agent.display_name(),
        "detected": agent.detect(env),
        "status": { "state": status.state() },
        "capabilities": agent.capabilities(),
        "snippet": agent.snippet(env),
    });
    if let Some(reason) = status.reason() {
        v["status"]["reason"] = json!(reason);
    }
    if let Some(on) = agent.instructions(env) {
        v["instructions"] = json!(on);
    }
    v
}

pub fn snippets(env: &AgentEnv) -> Value {
    json!({
        "mcp_json": super::mcp_json_snippet(env),
        "codex_toml": super::codex_toml_snippet(env),
    })
}

pub fn list(env: &AgentEnv) -> Value {
    let agents = super::all();
    let rows: Vec<Value> = std::thread::scope(|s| {
        let handles: Vec<_> = agents
            .iter()
            .map(|a| s.spawn(move || agent_json(a.as_ref(), env)))
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| json!({ "error": "agent probe panicked" }))
            })
            .collect()
    });
    json!({ "agents": rows, "snippets": snippets(env) })
}

#[derive(serde::Deserialize)]
struct ActionBody {
    id: String,
    #[serde(default)]
    instructions: bool,
}

fn parse_body(body: &[u8]) -> Result<ActionBody, (u16, Value)> {
    let parsed: ActionBody = serde_json::from_slice(body)
        .map_err(|e| (400, json!({ "error": format!("bad request body: {e}") })))?;
    if parsed.id.trim().is_empty() {
        return Err((400, json!({ "error": "no agent id given" })));
    }
    Ok(parsed)
}

fn error_status(e: &anyhow::Error) -> u16 {
    if e.downcast_ref::<Unparseable>().is_some() {
        return 409;
    }
    match e.downcast_ref::<AgentError>() {
        Some(AgentError::Unknown(_)) => 404,
        Some(AgentError::BadOption(_)) => 400,
        Some(AgentError::Unavailable(_)) => 409,
        None => 500,
    }
}

fn outcome(
    env: &AgentEnv,
    id: &str,
    act: impl FnOnce(&dyn Agent) -> anyhow::Result<Change>,
) -> (u16, Value) {
    let agent = match super::find(id) {
        Ok(a) => a,
        Err(e) => return (error_status(&e), json!({ "error": format!("{e:#}") })),
    };
    match act(agent.as_ref()) {
        Ok(change) => (
            200,
            json!({ "agent": agent_json(agent.as_ref(), env), "change": change }),
        ),
        Err(e) => (error_status(&e), json!({ "error": format!("{e:#}") })),
    }
}

pub fn connect(env: &AgentEnv, body: &[u8]) -> (u16, Value) {
    let req = match parse_body(body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let opts = ConnectOptions {
        instructions: req.instructions,
    };
    outcome(env, &req.id, |a| super::connect(a, env, &opts))
}

pub fn disconnect(env: &AgentEnv, body: &[u8]) -> (u16, Value) {
    let req = match parse_body(body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    outcome(env, &req.id, |a| a.disconnect(env))
}
