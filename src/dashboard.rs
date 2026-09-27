//! `br8n dashboard` — a local web view of the index, and the only place
//! that writes a memory or the config over HTTP.
//!
//! Hand-rolled thread-per-connection HTTP, GETs plus a handful of POSTs,
//! bound to 127.0.0.1. No framework: the surface is a few JSON endpoints and
//! a static bundle, and the test stubs already established this exact pattern.
//!
//! Every POST passes `origin_allowed` before it is routed, config writes are
//! serialised on `CONFIG_WRITE`, and the two that
//! write a memory are serialised on `MEMORY_WRITE` — `IndexLock` is a
//! read-then-write pidfile, so two threads entering it together would both
//! pass, and lbug's own file lock does not conflict with a lock the same
//! process already holds.
//!
//! THE rule of this module: the store is opened PER REQUEST and dropped
//! before the response is written. LadybugDB holds an exclusive OS file
//! lock — a held-open store would silence the prompt hook for as long as
//! the dashboard runs.

use crate::config::Config;
use crate::setup::agents::AgentEnv;
use anyhow::Result;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

#[derive(rust_embed::RustEmbed)]
#[folder = "dashboard/dist/"]
struct Assets;

#[derive(Clone)]
pub enum ConfigSource {
    Fixed(Box<Config>),
    File(std::path::PathBuf),
}

impl ConfigSource {
    fn path(&self) -> std::path::PathBuf {
        match self {
            ConfigSource::Fixed(_) => Config::config_path(),
            ConfigSource::File(path) => path.clone(),
        }
    }

    fn current(&self) -> Config {
        match self {
            ConfigSource::Fixed(cfg) => cfg.as_ref().clone(),
            ConfigSource::File(path) => Config::load_from(path),
        }
    }
}

impl From<Config> for ConfigSource {
    fn from(cfg: Config) -> ConfigSource {
        ConfigSource::Fixed(Box::new(cfg))
    }
}

pub fn serve(port: Option<u16>, open_browser: bool) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port.unwrap_or(0)))?;
    let addr = listener.local_addr()?;
    println!("br8n dashboard: http://{addr}");
    if open_browser {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let _ = std::process::Command::new(opener)
            .arg(format!("http://{addr}"))
            .spawn();
    }
    serve_on(ConfigSource::File(Config::config_path()), listener)
}

/// Test seam: serve on an already-bound listener, forever.
pub fn serve_on(source: impl Into<ConfigSource>, listener: TcpListener) -> Result<()> {
    serve_on_with_agents(source, listener, None)
}

pub fn serve_on_with_agents(
    source: impl Into<ConfigSource>,
    listener: TcpListener,
    agents: Option<AgentEnv>,
) -> Result<()> {
    let source = source.into();
    let agents = agents.map(std::sync::Arc::new);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let source = source.clone();
        let agents = agents.clone();
        std::thread::spawn(move || handle(stream, &source, agents.as_deref()));
    }
    Ok(())
}

fn agent_env(fixed: Option<&AgentEnv>) -> AgentEnv {
    fixed.cloned().unwrap_or_else(AgentEnv::from_env)
}

fn status_line(code: u16) -> &'static str {
    match code {
        200 => "200 OK",
        400 => "400 Bad Request",
        404 => "404 Not Found",
        409 => "409 Conflict",
        _ => "500 Internal Server Error",
    }
}

const MAX_BODY: usize = 64 * 1024;
const DRAIN_LIMIT: usize = MAX_BODY * 2;
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
static MEMORY_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());
static CONFIG_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());
static AGENT_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn handle(mut stream: TcpStream, source: &ConfigSource, agents: Option<&AgentEnv>) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let mut buf = [0u8; 8192];
    let mut req = Vec::new();
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                req.extend_from_slice(&buf[..n]);
                if req.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if req.len() > 64 * 1024 {
                    return;
                }
            }
            Err(_) => return,
        }
    }
    let line = String::from_utf8_lossy(&req);
    let head = line.split("\r\n\r\n").next().unwrap_or("").to_string();
    let mut parts = head.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return;
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if method == "POST" {
        let header_end = req
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|i| i + 4)
            .unwrap_or(req.len());
        let content_length: usize = head
            .lines()
            .find_map(|l| {
                l.split_once(':')
                    .filter(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, v)| v.trim().parse().ok())
            })
            .unwrap_or(0);
        if content_length > MAX_BODY {
            let mut seen = req.len() - header_end;
            while seen < content_length && seen < DRAIN_LIMIT {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => seen += n,
                    Err(_) => break,
                }
            }
            return json_status(
                &mut stream,
                "413 Payload Too Large",
                serde_json::json!({ "error": "request body too large" }),
            );
        }
        let mut body = req[header_end..].to_vec();
        while body.len() < content_length {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => body.extend_from_slice(&buf[..n]),
                Err(_) => return,
            }
        }
        body.truncate(content_length);

        let local = stream.local_addr().ok();
        if !local.map(|l| origin_allowed(&head, l)).unwrap_or(false) {
            return json_status(
                &mut stream,
                "409 Conflict",
                serde_json::json!({ "error": "refused: request origin is not this dashboard" }),
            );
        }
        match path {
            "/api/update" => {
                let paths = crate::setup::Paths::from_env();
                if let Some(why) = update_refusal(&paths) {
                    return json_status(
                        &mut stream,
                        "409 Conflict",
                        serde_json::json!({ "error": why }),
                    );
                }
                return match start_update(&paths) {
                    Ok(v) => json_status(&mut stream, "202 Accepted", v),
                    Err(e) => json_status(
                        &mut stream,
                        "500 Internal Server Error",
                        serde_json::json!({ "error": e.to_string() }),
                    ),
                };
            }
            "/api/memory/save" => {
                let _writing = MEMORY_WRITE.lock().unwrap_or_else(|e| e.into_inner());
                let _priority = crate::index::QueryPriority::announce(&Config::db_path());
                let (status, v) = memory_save(&source.current(), &body);
                return json_status(&mut stream, status, v);
            }
            "/api/agents/connect" => {
                let _writing = AGENT_WRITE.lock().unwrap_or_else(|e| e.into_inner());
                let (code, v) = crate::setup::agents::api::connect(&agent_env(agents), &body);
                return json_status(&mut stream, status_line(code), v);
            }
            "/api/agents/disconnect" => {
                let _writing = AGENT_WRITE.lock().unwrap_or_else(|e| e.into_inner());
                let (code, v) = crate::setup::agents::api::disconnect(&agent_env(agents), &body);
                return json_status(&mut stream, status_line(code), v);
            }
            "/api/memory/delete" => {
                let _writing = MEMORY_WRITE.lock().unwrap_or_else(|e| e.into_inner());
                let (status, v) = memory_delete(&source.current(), &body);
                return json_status(&mut stream, status, v);
            }
            "/api/config" => {
                let _writing = CONFIG_WRITE.lock().unwrap_or_else(|e| e.into_inner());
                let (status, v) = config_save(&source.path(), &body);
                return json_status(&mut stream, status, v);
            }
            "/api/config/check" => {
                let (status, v) = config_check(&source.path(), &body);
                return json_status(&mut stream, status, v);
            }
            "/api/config/embed" => {
                let _writing = CONFIG_WRITE.lock().unwrap_or_else(|e| e.into_inner());
                let (status, v) = config_embed(&source.path(), &body);
                return json_status(&mut stream, status, v);
            }
            "/api/index" => {
                let (status, v) = index_start(&Config::db_path(), &body);
                return json_status(&mut stream, status, v);
            }
            "/api/embed/test" => {
                return json_status(&mut stream, "200 OK", embed_test(&source.path()));
            }
            _ => {
                return respond(
                    &mut stream,
                    "405 Method Not Allowed",
                    "application/json",
                    br#"{"error":"GET only"}"#,
                );
            }
        }
    }
    if method != "GET" {
        respond(
            &mut stream,
            "405 Method Not Allowed",
            "application/json",
            br#"{"error":"GET only"}"#,
        );
        return;
    }
    match path {
        "/api/stats" => json_result(&mut stream, stats(&source.current())),
        "/api/progress" => json_result(&mut stream, progress()),
        "/api/version" => json_result(&mut stream, version(query)),
        "/api/graph" => json_result(&mut stream, graph(&source.current())),
        "/api/document" => json_result(&mut stream, document(&source.current(), query)),
        "/api/search" => json_result(&mut stream, search(&source.current(), query)),
        "/api/memories" => json_result(&mut stream, memories(&source.current())),
        "/api/memory/reach" => json_result(&mut stream, memory_reach(&source.current(), query)),
        "/api/config" => json_result(&mut stream, crate::config::view::view(&source.path())),
        "/api/index" => json_status(&mut stream, "200 OK", index_run()),
        "/api/install" => json_status(
            &mut stream,
            "200 OK",
            install_view(&source.path(), &agent_env(agents)),
        ),
        "/api/agents" => json_status(
            &mut stream,
            "200 OK",
            crate::setup::agents::api::list(&agent_env(agents)),
        ),
        p if p.starts_with("/api/") => respond(
            &mut stream,
            "404 Not Found",
            "application/json",
            br#"{"error":"unknown endpoint"}"#,
        ),
        _ => {
            let _ = query;
            // The SPA owns routing: any non-API path gets the shell.
            let asset = path.trim_start_matches('/');
            let file = Assets::get(asset)
                .or_else(|| Assets::get("index.html"))
                .expect("index.html is embedded");
            let ctype = match asset.rsplit('.').next() {
                Some("js") => "text/javascript",
                Some("css") => "text/css",
                Some("svg") => "image/svg+xml",
                _ => "text/html",
            };
            respond(&mut stream, "200 OK", ctype, &file.data);
        }
    }
}

fn json_result(stream: &mut TcpStream, r: Result<serde_json::Value>) {
    match r {
        Ok(v) => respond(
            stream,
            "200 OK",
            "application/json",
            v.to_string().as_bytes(),
        ),
        // Mid-swap rename window: the store is briefly unopenable. 503 tells
        // the client to retry; anything else is still a JSON error, not HTML.
        Err(e) => respond(
            stream,
            "503 Service Unavailable",
            "application/json",
            serde_json::json!({ "error": e.to_string() })
                .to_string()
                .as_bytes(),
        ),
    }
}

fn json_status(stream: &mut TcpStream, status: &str, v: serde_json::Value) {
    respond(stream, status, "application/json", v.to_string().as_bytes());
}

fn origin_allowed(head: &str, local: std::net::SocketAddr) -> bool {
    let origin = head.lines().find_map(|l| {
        l.split_once(':')
            .filter(|(k, _)| k.trim().eq_ignore_ascii_case("origin"))
            .map(|(_, v)| v.trim().to_string())
    });
    match origin {
        None => true,
        Some(o) => {
            let port = local.port();
            o == format!("http://127.0.0.1:{port}") || o == format!("http://localhost:{port}")
        }
    }
}

fn update_refusal(paths: &crate::setup::Paths) -> Option<String> {
    if crate::index::IndexLock::is_held(&paths.db) {
        return Some("an index is running; wait for it to finish".to_string());
    }
    if let Some(s) = crate::update::Status::running(&paths.update_status) {
        return Some(format!("an update is already running ({})", s.phase));
    }
    None
}

fn start_update(paths: &crate::setup::Paths) -> Result<serde_json::Value> {
    let exe = std::env::current_exe()?;
    let log = paths.db.with_extension("log");
    let sink = || {
        crate::hook::open_log_for_append(&log)
            .map(std::process::Stdio::from)
            .unwrap_or_else(|_| std::process::Stdio::null())
    };
    let child = std::process::Command::new(exe)
        .args(["update", "--yes", "--quiet"])
        .stdout(sink())
        .stderr(sink())
        .spawn()?;
    Ok(serde_json::json!({ "started": true, "pid": child.id() }))
}

#[derive(serde::Deserialize)]
struct ConfigSaveBody {
    etag: String,
    #[serde(default)]
    set: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    unset: Vec<String>,
}

#[derive(serde::Deserialize)]
struct ConfigCheckBody {
    #[serde(default)]
    set: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    unset: Vec<String>,
}

fn present<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Option<String>>, D::Error> {
    <Option<String> as serde::Deserialize>::deserialize(deserializer).map(Some)
}

#[derive(serde::Deserialize)]
struct EmbedEndpointBody {
    #[serde(default, deserialize_with = "present")]
    url: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    model: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    token: Option<Option<String>>,
}

const UNPROCESSABLE: &str = "422 Unprocessable Entity";
const BAD_BODY: &str = "body is not the expected JSON";

fn refused(errors: Vec<crate::config::check::FieldError>) -> (&'static str, serde_json::Value) {
    let summary = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    (
        UNPROCESSABLE,
        serde_json::json!({ "error": format!("not saved: {summary}"), "errors": errors }),
    )
}

fn server_error(e: anyhow::Error) -> (&'static str, serde_json::Value) {
    (
        "500 Internal Server Error",
        serde_json::json!({ "error": format!("{e:#}") }),
    )
}

fn config_view(config_path: &std::path::Path) -> (&'static str, serde_json::Value) {
    match crate::config::view::view(config_path) {
        Ok(v) => ("200 OK", v),
        Err(e) => server_error(e),
    }
}

fn config_save(config_path: &std::path::Path, body: &[u8]) -> (&'static str, serde_json::Value) {
    use crate::config::edit::{update, Outcome, Patch};
    let Ok(b) = serde_json::from_slice::<ConfigSaveBody>(body) else {
        return ("400 Bad Request", serde_json::json!({ "error": BAD_BODY }));
    };
    let patch = match Patch::from_json(&b.set, &b.unset) {
        Ok(patch) => patch,
        Err(errors) => return refused(errors),
    };
    match update(config_path, Some(&b.etag), &patch) {
        Ok(Outcome::Written { .. }) => config_view(config_path),
        Ok(Outcome::Conflict { etag }) => (
            "409 Conflict",
            serde_json::json!({
                "error": "the config file changed since it was read; reload it and make the change again",
                "etag": etag,
            }),
        ),
        Ok(Outcome::Invalid(errors)) => refused(errors),
        Err(e) => server_error(e),
    }
}

fn config_check(config_path: &std::path::Path, body: &[u8]) -> (&'static str, serde_json::Value) {
    use crate::config::check::check;
    use crate::config::edit::{new_errors, patched_text, read_document, Patch};
    let Ok(b) = serde_json::from_slice::<ConfigCheckBody>(body) else {
        return ("400 Bad Request", serde_json::json!({ "error": BAD_BODY }));
    };
    let patch = match Patch::from_json(&b.set, &b.unset) {
        Ok(patch) => patch,
        Err(errors) => {
            return (
                "200 OK",
                serde_json::json!({ "errors": errors, "blocking": errors }),
            )
        }
    };
    let current = match read_document(config_path) {
        Ok(current) => current,
        Err(e) => return server_error(e),
    };
    let (errors, blocking) = match patched_text(current.as_deref(), &patch) {
        Ok(next) => {
            let errors = check(&next);
            let before = current.as_deref().map(check).unwrap_or_default();
            let blocking = new_errors(&before, errors.clone());
            (errors, blocking)
        }
        Err(errors) => (errors.clone(), errors),
    };
    (
        "200 OK",
        serde_json::json!({ "errors": errors, "blocking": blocking }),
    )
}

fn config_embed(config_path: &std::path::Path, body: &[u8]) -> (&'static str, serde_json::Value) {
    use crate::config::check::FieldError;
    use crate::env_file::{self, MODEL_VAR, TOKEN_VAR, URL_VAR};
    let Ok(b) = serde_json::from_slice::<EmbedEndpointBody>(body) else {
        return ("400 Bad Request", serde_json::json!({ "error": BAD_BODY }));
    };
    let changes: Vec<(&str, Option<String>)> =
        [(URL_VAR, b.url), (MODEL_VAR, b.model), (TOKEN_VAR, b.token)]
            .into_iter()
            .filter_map(|(var, field)| {
                field.map(|value| {
                    let value = value
                        .map(|v| v.trim().to_string())
                        .filter(|v| !v.is_empty());
                    (var, value)
                })
            })
            .collect();
    if changes.is_empty() {
        return config_view(config_path);
    }
    let env_path = env_file::env_path(config_path);
    let mut merged = env_file::read_lenient(&env_path);
    for (var, value) in &changes {
        match value {
            Some(v) => merged.insert(var.to_string(), v.clone()),
            None => merged.remove(*var),
        };
    }
    let effective = |var: &str| {
        std::env::var(var)
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| merged.get(var).cloned())
    };
    let mut errors = Vec::new();
    for (var, value) in &changes {
        if value.as_deref().is_some_and(|v| v.contains(['\n', '\r'])) {
            errors.push(FieldError::new(
                endpoint_setting(var),
                "must be a single line",
            ));
        }
    }
    if let Some(url) = effective(URL_VAR) {
        if let Some(why) = crate::config::check::endpoint_url_problem(&url) {
            errors.push(FieldError::new("endpoint.url", why));
        }
        for var in [MODEL_VAR, TOKEN_VAR] {
            if effective(var).is_none() {
                errors.push(FieldError::new(
                    endpoint_setting(var),
                    "is required while an endpoint URL is set",
                ));
            }
        }
    }
    if !errors.is_empty() {
        return refused(errors);
    }
    match env_file::write_entries(&env_path, &changes) {
        Ok(()) => config_view(config_path),
        Err(e) => server_error(e),
    }
}

fn endpoint_setting(var: &str) -> &'static str {
    match var {
        crate::env_file::URL_VAR => "endpoint.url",
        crate::env_file::MODEL_VAR => "endpoint.model",
        _ => "embed.token",
    }
}

fn an_index_is_running(db: &std::path::Path) -> bool {
    crate::index::IndexLock::is_held(db) || progress().is_ok_and(|p| p.get("idle").is_none())
}

#[derive(serde::Deserialize, Default)]
struct IndexBody {
    #[serde(default)]
    reindex: bool,
}

#[derive(Clone, serde::Serialize)]
struct IndexRun {
    pid: u32,
    reindex: bool,
    finished: bool,
    ok: Option<bool>,
    code: Option<i32>,
    log: String,
}

static INDEX_RUN: std::sync::Mutex<Option<IndexRun>> = std::sync::Mutex::new(None);

fn index_run() -> serde_json::Value {
    let run = INDEX_RUN.lock().unwrap_or_else(|e| e.into_inner()).clone();
    serde_json::json!({ "run": run })
}

fn record_index_exit(pid: u32, status: std::io::Result<std::process::ExitStatus>) {
    let mut run = INDEX_RUN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(run) = run.as_mut().filter(|r| r.pid == pid) {
        run.finished = true;
        run.ok = Some(status.as_ref().is_ok_and(|s| s.success()));
        run.code = status.ok().and_then(|s| s.code());
    }
}

fn index_start(db: &std::path::Path, body: &[u8]) -> (&'static str, serde_json::Value) {
    let request = if body.iter().all(u8::is_ascii_whitespace) {
        IndexBody::default()
    } else {
        match serde_json::from_slice::<IndexBody>(body) {
            Ok(request) => request,
            Err(_) => return ("400 Bad Request", serde_json::json!({ "error": BAD_BODY })),
        }
    };
    if an_index_is_running(db) {
        return (
            "409 Conflict",
            serde_json::json!({ "error": "an index is already running" }),
        );
    }
    let log = db.with_extension("log");
    let sink = || {
        crate::hook::open_log_for_append(&log)
            .map(std::process::Stdio::from)
            .unwrap_or_else(|_| std::process::Stdio::null())
    };
    let mut args = vec!["index"];
    if request.reindex {
        args.push("--reindex");
    }
    let spawned = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .args(&args)
            .stdout(sink())
            .stderr(sink())
            .spawn()
    });
    match spawned {
        Ok(mut child) => {
            let pid = child.id();
            *INDEX_RUN.lock().unwrap_or_else(|e| e.into_inner()) = Some(IndexRun {
                pid,
                reindex: request.reindex,
                finished: false,
                ok: None,
                code: None,
                log: log.display().to_string(),
            });
            std::thread::spawn(move || record_index_exit(pid, child.wait()));
            (
                "202 Accepted",
                serde_json::json!({ "started": true, "pid": pid, "reindex": request.reindex }),
            )
        }
        Err(e) => server_error(e.into()),
    }
}

fn install_view(config_path: &std::path::Path, env: &AgentEnv) -> serde_json::Value {
    use crate::setup::install::{fix_for, install_checks};
    let exe = std::env::current_exe().unwrap_or_default();
    let claude = env.claude.available();
    let checks: Vec<serde_json::Value> =
        install_checks(&env.paths, &env.claude, &env.version, &exe)
            .into_iter()
            .map(|c| {
                let fix = fix_for(&c, &exe, claude);
                serde_json::json!({ "name": c.name, "ok": c.ok, "detail": c.detail, "fix": fix })
            })
            .collect();
    let config_errors = crate::config::edit::read_document(config_path)
        .ok()
        .flatten()
        .map(|text| crate::config::check::check(&text))
        .unwrap_or_default();
    let cfg = Config::load_from(config_path);
    let (remote, url, model) = embed_target(&cfg);
    serde_json::json!({
        "root": env.paths.root,
        "binary": env.paths.bin,
        "exe": exe,
        "checks": checks,
        "config": { "path": config_path, "errors": config_errors },
        "embed": {
            "backend": if remote { "remote" } else { "ollama" },
            "url": url,
            "model": model,
        },
    })
}

fn embed_target(cfg: &Config) -> (bool, Option<String>, Option<String>) {
    let remote = cfg.embed.remote.is_some() || cfg.embed.remote_error.is_some();
    let (url, model) = match &cfg.embed.remote {
        Some(r) => (Some(r.url.clone()), Some(r.model.clone())),
        None if remote => (None, None),
        None => (
            Some(cfg.embed.ollama_url.clone()),
            Some(cfg.embed.model.clone()),
        ),
    };
    (remote, url, model)
}

const EMBED_TEST_TEXT: &str = "br8n connection test";

fn embed_test(config_path: &std::path::Path) -> serde_json::Value {
    let cfg = Config::load_from(config_path);
    let (remote, url, model) = embed_target(&cfg);
    let started = std::time::Instant::now();
    let result = crate::embed::for_config(&cfg.embed).and_then(|embedder| {
        let _priority = crate::index::QueryPriority::announce(&Config::db_path());
        embedder.embed_query(EMBED_TEST_TEXT)
    });
    let latency_ms = started.elapsed().as_millis() as u64;
    let backend = if remote { "remote" } else { "ollama" };
    match result {
        Ok(vector) => serde_json::json!({
            "ok": true,
            "latency_ms": latency_ms,
            "dimensions": vector.len(),
            "backend": backend,
            "url": url,
            "model": model,
        }),
        Err(e) => serde_json::json!({
            "ok": false,
            "latency_ms": latency_ms,
            "dimensions": null,
            "backend": backend,
            "url": url,
            "model": model,
            "error": format!("{e:#}"),
        }),
    }
}

fn version(raw_query: &str) -> Result<serde_json::Value> {
    let paths = crate::setup::Paths::from_env();
    let installed = env!("CARGO_PKG_VERSION");
    let refresh = raw_query.split('&').any(|p| p == "refresh=1");
    if refresh {
        let opts = crate::update::UpdateOpts {
            paths: paths.clone(),
            installed: installed.to_string(),
            api: crate::update::release::api_base(),
            token: crate::update::release::token(),
            target: env!("BR8N_TARGET").to_string(),
            check_only: true,
        };
        let _ = crate::update::check(&opts);
    }
    let check = crate::update::UpdateCheck::read(&paths.update_json);
    let running = crate::update::Status::read(&paths.update_status)
        .filter(|s| s.done || crate::index::pid_is_alive(s.pid));
    Ok(serde_json::json!({
        "installed": installed,
        "latest": check.as_ref().and_then(|c| c.latest.clone()),
        "url": check.as_ref().and_then(|c| c.url.clone()),
        "checked_at": check.as_ref().map(|c| c.checked_at),
        "error": check.as_ref().and_then(|c| c.error.clone()),
        "update_available": check.as_ref().and_then(|c| c.available_against(installed)),
        "update": running,
    }))
}

fn respond(stream: &mut TcpStream, status: &str, ctype: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

fn stats(cfg: &Config) -> Result<serde_json::Value> {
    // Open, read, DROP — before serialization even begins.
    //
    // The four shared fields come from the same `status_snapshot` the CLI
    // prints; `by_source` is the dashboard's alone, so it stays out of the
    // snapshot rather than making `br8n status` pay for an aggregate it
    // never shows.
    let db = Config::db_path();
    let (snap, by_source, sessions_by_agent) = if no_index_yet(&db) {
        (
            crate::store::StatusSnapshot {
                documents: 0,
                chunks: 0,
                model: None,
                skipped: Vec::new(),
                vectors_pending: 0,
            },
            Default::default(),
            Default::default(),
        )
    } else {
        let store = crate::store::Store::open_existing(&db, cfg.embed.dimensions)?;
        (
            store.status_snapshot(),
            store.counts_by_source().unwrap_or_default(),
            store.counts_sessions_by_agent().unwrap_or_default(),
        )
    };
    Ok(serde_json::json!({
        "documents": snap.documents,
        "chunks": snap.chunks,
        "by_source": by_source,
        "sessions_by_agent": sessions_by_agent,
        "model": snap.model,
        "skipped": snap.skipped,
        "bench": crate::bench::read_report(),
        "memory": crate::memory::counts_at(&crate::memory::default_root())
            .unwrap_or_default()
            .into_iter()
            .map(|(k, n)| (k.as_str().to_string(), n))
            .collect::<std::collections::BTreeMap<String, usize>>(),
    }))
}

fn no_index_yet(db: &std::path::Path) -> bool {
    !db.exists() && !db.with_extension("old").exists()
}

fn readable_memories(cfg: &Config) -> std::result::Result<Vec<crate::memory::Memory>, String> {
    if !cfg.memory.enabled {
        return Err(crate::memory::DISABLED.to_string());
    }
    crate::memory::list_at(
        &crate::memory::default_root(),
        &crate::memory::Filter::default(),
    )
    .map_err(|e| format!("{e:#}"))
}

fn memories(cfg: &Config) -> Result<serde_json::Value> {
    match readable_memories(cfg) {
        Ok(list) => Ok(serde_json::json!({ "memories": list, "unavailable": null })),
        Err(reason) => Ok(serde_json::json!({ "memories": [], "unavailable": reason })),
    }
}

fn memory_reach(cfg: &Config, raw_query: &str) -> Result<serde_json::Value> {
    let mut id = String::new();
    for pair in raw_query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == "id" {
            id = url_decode(v);
        }
    }
    if id.trim().is_empty() {
        return Ok(serde_json::json!({ "error": "no id given" }));
    }
    let all = match readable_memories(cfg) {
        Ok(all) => all,
        Err(reason) => return Ok(serde_json::json!({ "error": reason })),
    };
    let Some(m) = all.iter().find(|m| m.id == id) else {
        return Ok(serde_json::json!({ "error": format!("no memory with id `{id}`") }));
    };
    let surface = crate::config::Surface::Hook;
    let profile = cfg.profile_for(surface);
    let threshold = cfg.surface(surface).threshold;
    let _priority = crate::index::QueryPriority::announce(&Config::db_path());
    let retriever = crate::retrieve_for_profile(cfg, surface, &profile)?;
    let hits = match retriever.search_gated(&m.text, &profile, threshold) {
        Ok(h) => h,
        Err(e) => return search_failure(e),
    };
    let reach: Vec<serde_json::Value> = hits
        .iter()
        .filter(|h| h.source_type != "memory")
        .map(|h| {
            serde_json::json!({
                "doc_id": h.doc_id,
                "title": h.title,
                "uri": h.uri,
                "relevance": h.relevance,
            })
        })
        .collect();
    Ok(serde_json::json!({ "reach": reach }))
}

#[derive(serde::Deserialize)]
struct SaveBody {
    id: Option<String>,
    kind: String,
    text: String,
    scope: String,
    confidence: Option<u8>,
}

#[derive(serde::Deserialize)]
struct DeleteBody {
    id: String,
}

fn memory_busy() -> Option<String> {
    let db = crate::memory::store_dir(&crate::memory::default_root());
    crate::index::IndexLock::is_held(&db).then(|| crate::memory::MemoryBusy.to_string())
}

fn memory_save(cfg: &Config, body: &[u8]) -> (&'static str, serde_json::Value) {
    let Ok(b) = serde_json::from_slice::<SaveBody>(body) else {
        return (
            "400 Bad Request",
            serde_json::json!({ "error": "body is not the expected JSON" }),
        );
    };
    let Some(kind) = crate::memory::MemoryKind::parse(&b.kind) else {
        return (
            "400 Bad Request",
            serde_json::json!({ "error": format!("kind `{}` is not lesson, fact or episode", b.kind) }),
        );
    };
    if let Some(why) = memory_busy() {
        return ("409 Conflict", serde_json::json!({ "error": why }));
    }
    if b.confidence.is_some_and(|c| c > 100) {
        return (
            "400 Bad Request",
            serde_json::json!({ "error": "confidence is a percent, so it must be 0 to 100" }),
        );
    }
    let project = match b.scope.trim() {
        "global" | "" => None,
        p => Some(std::path::PathBuf::from(p)),
    };
    let r = crate::memory::Remember {
        kind,
        text: b.text,
        title: None,
        project,
        confidence: b.confidence.unwrap_or(100),
        origin: crate::memory::Origin::User,
        session: None,
        source_hash: None,
        source_stamp: None,
        created: None,
    };
    let done = match &b.id {
        Some(id) if id.trim().is_empty() => {
            return (
                "400 Bad Request",
                serde_json::json!({ "error": "an edit must name the id it is editing" }),
            );
        }
        Some(id) => crate::memory::edit(cfg, id, r),
        None => crate::memory::remember(cfg, r),
    };
    match done {
        Ok(crate::memory::Outcome::Rejected(why)) => {
            ("400 Bad Request", serde_json::json!({ "error": why }))
        }
        Ok(crate::memory::Outcome::Saved { id }) => (
            "200 OK",
            serde_json::json!({ "id": id, "outcome": "saved" }),
        ),
        Ok(crate::memory::Outcome::Replaced {
            id, previous_title, ..
        }) => (
            "200 OK",
            serde_json::json!({ "id": id, "outcome": "replaced", "previous_title": previous_title }),
        ),
        Ok(crate::memory::Outcome::Duplicate { of }) => (
            "200 OK",
            serde_json::json!({ "id": of, "outcome": "duplicate" }),
        ),
        Err(e) => memory_error(e),
    }
}

fn memory_delete(cfg: &Config, body: &[u8]) -> (&'static str, serde_json::Value) {
    let Ok(b) = serde_json::from_slice::<DeleteBody>(body) else {
        return (
            "400 Bad Request",
            serde_json::json!({ "error": "body is not the expected JSON" }),
        );
    };
    if b.id.trim().is_empty() {
        return (
            "400 Bad Request",
            serde_json::json!({ "error": "a delete must name the id it is deleting" }),
        );
    }
    if let Some(why) = memory_busy() {
        return ("409 Conflict", serde_json::json!({ "error": why }));
    }
    match crate::memory::forget(cfg, &b.id) {
        Ok(m) => (
            "200 OK",
            serde_json::json!({ "id": m.id, "title": m.title }),
        ),
        Err(e) => memory_error(e),
    }
}

fn memory_error(e: anyhow::Error) -> (&'static str, serde_json::Value) {
    if e.downcast_ref::<crate::memory::MemoryNotFound>().is_some()
        || e.downcast_ref::<crate::memory::MemoryAmbiguous>().is_some()
    {
        return (
            "404 Not Found",
            serde_json::json!({ "error": format!("{e}") }),
        );
    }
    if e.downcast_ref::<crate::memory::MemoryBusy>().is_some() {
        return (
            "409 Conflict",
            serde_json::json!({ "error": format!("{e}") }),
        );
    }
    (
        "503 Service Unavailable",
        serde_json::json!({ "error": format!("{e:#}") }),
    )
}

fn progress() -> Result<serde_json::Value> {
    let p = Config::db_path().with_extension("progress");
    let idle = serde_json::json!({ "idle": true });
    let Ok(raw) = std::fs::read_to_string(&p) else {
        return Ok(idle);
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Ok(idle);
    };
    // A killed indexer cannot clean up after itself; a dead pid means stale.
    match v["pid"].as_u64() {
        Some(pid) if crate::index::pid_is_alive(pid as u32) => Ok(v),
        _ => Ok(idle),
    }
}

fn graph(cfg: &Config) -> Result<serde_json::Value> {
    // Open, read, DROP — same rule as `stats`, before serialization begins.
    let db = Config::db_path();
    let mut v = if no_index_yet(&db) {
        serde_json::json!({ "nodes": [], "entities": [], "tags": [], "edges": [] })
    } else {
        let snap = {
            let store = crate::store::Store::open_existing(&db, cfg.embed.dimensions)?;
            store.graph_snapshot()?
        };
        serde_json::to_value(snap)?
    };
    if cfg.memory.enabled {
        graph_memories(&mut v);
    }
    Ok(v)
}

fn graph_memories(v: &mut serde_json::Value) {
    let root = crate::memory::default_root();
    let Ok(mems) = crate::memory::list_at(&root, &crate::memory::Filter::default()) else {
        return;
    };
    let mut projects: std::collections::BTreeSet<String> = Default::default();
    let mut nodes: Vec<serde_json::Value> = Vec::new();
    let mut edges: Vec<serde_json::Value> = Vec::new();
    for m in &mems {
        nodes.push(serde_json::json!({
            "id": m.doc_id,
            "title": m.title,
            "source_type": "memory",
            "memory_kind": m.facts.kind.as_str(),
            "memory_id": m.id,
            "chunks": 1,
            "inbound": 0,
        }));
        if let Some(s) = &m.facts.session {
            edges.push(serde_json::json!({
                "from": m.doc_id,
                "to": crate::model::Document::new_id(s),
                "kind": "distilled-from",
            }));
        }
        if let Some(p) = &m.facts.project {
            projects.insert(p.clone());
            edges.push(serde_json::json!({
                "from": m.doc_id,
                "to": format!("project:{p}"),
                "kind": "scoped-to",
            }));
        }
    }
    for p in &projects {
        nodes.push(serde_json::json!({
            "id": format!("project:{p}"),
            "title": p,
            "source_type": "project",
            "chunks": 0,
            "inbound": 0,
        }));
    }
    if let Some(a) = v["nodes"].as_array_mut() {
        a.extend(nodes);
    }
    if let Some(a) = v["edges"].as_array_mut() {
        a.extend(edges);
    }
}

fn document(cfg: &Config, raw_query: &str) -> Result<serde_json::Value> {
    let mut id = String::new();
    for pair in raw_query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == "id" {
            id = url_decode(v);
        }
    }
    if id.trim().is_empty() {
        return Ok(serde_json::json!({ "error": "missing id" }));
    }
    // Open, read, DROP — same shape as `stats` and `graph`. The store is the
    // single-writer resource; a handler that holds it across serialization
    // holds it across a client's slow socket.
    let detail = {
        let store = crate::store::Store::open_existing(&Config::db_path(), cfg.embed.dimensions)?;
        store.document_detail(&id)?
    };
    match detail {
        Some(d) => Ok(serde_json::to_value(d)?),
        // 200 with an error payload, NOT a 503: `j` in api.ts retries a 503
        // once and then shows "the index is busy", which would be a lie about
        // a document that is simply gone.
        None => Ok(serde_json::json!({ "error": "unknown document" })),
    }
}

fn search(cfg: &Config, raw_query: &str) -> Result<serde_json::Value> {
    let mut q = String::new();
    let mut tier: Option<u8> = None;
    for pair in raw_query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "q" => q = url_decode(v),
            "tier" => tier = v.parse().ok(),
            _ => {}
        }
    }
    if q.trim().is_empty() {
        return Ok(serde_json::json!({ "error": "empty query" }));
    }
    let surface = crate::config::Surface::Hook;
    let profile = tier
        .map(crate::config::Profile::tier)
        .unwrap_or_else(|| cfg.profile_for(surface));
    let threshold = cfg.surface(surface).threshold;
    // The hook's own injection budget: `search_explained` needs it to report
    // what would really be injected rather than everything that cleared the
    // gate. Same surface as the threshold and the weights, or the answer
    // describes a hook nobody configured.
    let max_tokens = cfg.surface(surface).max_tokens;
    // Ollama serves embedding on ONE slot: without this marker, a dashboard
    // query queues behind bulk-indexing batches and was measured at 7.8s,
    // 15.3s and 7.9s against a 1500ms budget — the prompt hook went silent
    // for the entire duration of any real index. Prompts preempt indexing;
    // a dashboard query deserves the same priority.
    let _priority = crate::index::QueryPriority::announce(&Config::db_path());
    // The store and the embedding pipeline fail for different reasons, and
    // the client has to react differently to each:
    //   - store failure (e.g. the shadow-swap rename window): a transient
    //     condition, so `?` propagates it to `json_result`'s 503 path and the
    //     client silently retries once.
    //   - embedding failure (Ollama down): an expected, stable runtime state,
    //     so it's reported as 200 with an error payload and the client shows a
    //     banner while Graph and Health keep working.
    // The plan's example snippet folded both into one `.and_then` chain and
    // reported them identically as "embedding unavailable" — which lied about
    // a store hiccup by blaming Ollama. Splitting `retrieve_for`'s `?` off
    // from the match only covered store-OPEN failure: an `expand` or
    // `fts_search` that failed INSIDE the query (the swap landing after the
    // store opened) still came back as "embedding unavailable", the same lie
    // one step further in. So the split is now on the error itself:
    // `EmbedUnavailable` tags the one call that talks to Ollama, and
    // everything else — every store error, wherever in the pipeline it was
    // raised — takes the 503 path.
    // `retrieve_for` decides store-vs-pack against `surface`'s own configured
    // profile, but `profile` here can be a `?tier=` override that asks for
    // graph expansion the surface's default does not. Deciding against the
    // surface default would build a storeless retriever whenever the hook's
    // own tier needs no graph, and then any `?tier=2` (or higher) request
    // would fail every time — not a rare race, a guaranteed break of the
    // tier picker. `retrieve_for_profile` decides against the profile that is
    // actually about to run.
    let retriever = match crate::retrieve_for_profile(cfg, surface, &profile) {
        Ok(r) => r,
        Err(e) => return search_failure(e),
    };
    match retriever.search_explained(&q, &profile, threshold, max_tokens) {
        Ok(ex) => Ok(serde_json::to_value(ex)?),
        Err(e) => search_failure(e),
    }
}

/// Which of the three answers a failed query gets: a 200 with a banner
/// payload, a 200 with `PackRefused`'s own message, or a 503 the client
/// retries.
///
/// Decided by the KIND of error, not by where in `search` it was raised.
/// `EmbedUnavailable` — the tag on the one call that talks to Ollama — earns
/// the "embedding unavailable" banner; `PackRefused` earns its own message,
/// verbatim. Every other error is the store's, and the store's failures are
/// transient. Deciding by position is what produced the original lie: the
/// whole pipeline sat behind one `Err(_)` arm that said "embedding
/// unavailable" whatever had actually gone wrong.
fn search_failure(e: anyhow::Error) -> Result<serde_json::Value> {
    if e.downcast_ref::<crate::retrieve::EmbedUnavailable>()
        .is_some()
    {
        return Ok(serde_json::json!({ "error": "embedding unavailable" }));
    }
    if let Some(refused) = e.downcast_ref::<crate::retrieve::PackRefused>() {
        return Ok(serde_json::json!({ "error": refused.to_string() }));
    }
    Err(e)
}

/// Percent-decoding for the one query parameter we accept. `+` is a space.
fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            // `i` is always a char boundary here — `b[i] == b'%'` is an ASCII
            // byte value, which cannot occur as a continuation byte of a
            // multi-byte UTF-8 sequence, so this position always starts a
            // character. `i + 1` is therefore also always a boundary (a
            // one-byte char's only boundary is right after it). `i + 3` is
            // NOT guaranteed: the request is read with
            // `String::from_utf8_lossy`, so a literal (non-percent-encoded)
            // multi-byte character can sit immediately after the `%`, e.g.
            // `%€` — 0x25 then a 3-byte UTF-8 sequence — and `i + 3` then
            // lands one byte inside that character. Slicing `s[i+1..i+3]`
            // across a non-boundary panics the connection thread; skipping
            // the escape and treating it as a literal `%`, exactly like the
            // existing not-valid-hex arm below, keeps this a dropped escape
            // rather than a dropped connection.
            b'%' if i + 3 <= b.len() && s.is_char_boundary(i + 3) => {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 3;
                } else {
                    out.push(b[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_write_failure_is_classified_by_its_type_not_by_its_words() {
        use crate::memory::{MemoryAmbiguous, MemoryBusy, MemoryNotFound};
        let status = |e: anyhow::Error| super::memory_error(e).0;
        assert_eq!(status(MemoryBusy.into()), "409 Conflict");
        assert_eq!(
            status(anyhow::Error::from(MemoryBusy).context("saving a memory")),
            "409 Conflict",
            "a busy store is still a 409 once a caller has added context"
        );
        assert_eq!(status(MemoryNotFound("abc".into()).into()), "404 Not Found");
        assert_eq!(
            status(
                MemoryAmbiguous {
                    id: "a".into(),
                    matches: 2
                }
                .into()
            ),
            "404 Not Found"
        );
        assert_eq!(
            status(anyhow::anyhow!("{MemoryBusy}")),
            "503 Service Unavailable",
            "an error that only says it is busy is not a busy store"
        );
        assert_eq!(
            status(anyhow::anyhow!("the id matches 2 memories")),
            "503 Service Unavailable"
        );
    }

    #[test]
    fn a_disabled_memory_store_is_named_rather_than_read_back_as_empty() {
        let mut cfg = crate::config::Config::default();
        cfg.memory.enabled = false;
        let v = super::memories(&cfg).unwrap();
        assert!(
            v["memories"].as_array().is_some_and(|a| a.is_empty()),
            "a disabled store lists nothing: {v}"
        );
        assert_eq!(
            v["unavailable"].as_str(),
            Some(crate::memory::DISABLED),
            "disabled must not look like an empty store, which invites a create the server refuses: {v}"
        );
    }

    #[test]
    fn a_disabled_memory_store_refuses_a_reach_by_name() {
        let mut cfg = crate::config::Config::default();
        cfg.memory.enabled = false;
        let v = super::memory_reach(&cfg, "id=abc").unwrap();
        assert_eq!(
            v["error"].as_str(),
            Some(crate::memory::DISABLED),
            "a disabled store must refuse a reach the way it refuses a listing: {v}"
        );
    }

    /// Both arms of `search_failure`, with errors of the kinds the pipeline
    /// really raises rather than hand-written strings.
    ///
    /// This is a unit test and not another `/api/search` case in
    /// `tests/dashboard.rs` because a store failure raised INSIDE a query
    /// cannot currently be reached through the endpoint: `expand` returns
    /// through `Store::rows_to_hits`, which deliberately turns a failed
    /// `exec` into an empty result set ("a missing index means nothing
    /// indexed yet"); `fts_search` never reaches `rows_to_hits` at
    /// all any more — it always errors outright now that lbug's FTS index is
    /// gone (see its doc comment) — but that error is just as unreachable
    /// through the endpoint, since `retrieve::Retriever::run` catches it and
    /// reports `bm25_unavailable` instead of letting it propagate. The
    /// misclassification this test guards against was real all the same —
    /// every one of these calls is declared `Result` and propagates with `?`
    /// wherever it isn't caught deliberately, so the day one of them does
    /// fail somewhere new, the answer must not be a banner blaming Ollama.
    #[test]
    fn only_an_embedding_failure_gets_the_banner_a_store_failure_gets_a_503() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(dir.path(), 4).unwrap();
        // A genuine store error object, straight out of the query path.
        let store_err = store
            .exec("MATCH (x:NoSuchLabel) RETURN x.nope", vec![])
            .expect_err("querying a label that does not exist must fail");

        let out = super::search_failure(store_err);
        let err = out.expect_err("a store failure must propagate to the 503 path");
        assert!(
            !err.to_string().contains("embedding"),
            "a store failure must not be described as an embedding one: {err}"
        );

        let embed_err = crate::retrieve::EmbedUnavailable(anyhow::anyhow!(
            "error sending request for url (http://127.0.0.1:11434/api/embed)"
        ));
        let out = super::search_failure(anyhow::Error::new(embed_err))
            .expect("an embedding outage is a 200 with a payload, not an HTTP error");
        assert_eq!(out["error"], "embedding unavailable");

        let refused = crate::retrieve::PackRefused(anyhow::anyhow!(
            "this index predates the retrieval pack and can no longer be searched \
             directly — run `br8n index --compact` to rebuild the pack from the rows \
             you already have (no re-embedding), or `br8n index --reindex` to rebuild \
             from source"
        ));
        let message = refused.to_string();
        let out = super::search_failure(anyhow::Error::new(refused)).expect(
            "a pack refusal is a stable state, not a hiccup — it must be a 200 with a payload, \
             not the 503 a client retries and reports as busy",
        );
        assert_eq!(
            out["error"], message,
            "the dashboard must show the refusal's OWN message, not a generic banner — it \
             names its own fix (`br8n index --compact`) and a generic banner would discard that"
        );
    }

    /// A `%` immediately followed by a literal (not percent-encoded)
    /// multi-byte UTF-8 character must not panic.
    ///
    /// The request is read with `String::from_utf8_lossy`, so a query target
    /// like `/api/document?id=%€` reaches `url_decode` as `%€` — `%` (one
    /// byte) then `€` (three bytes: 0xE2 0x82 0xAC). `i + 3` then lands one
    /// byte inside `€`, which is not a char boundary, and slicing across it
    /// panicked the connection thread before the char-boundary guard was
    /// added. A bad escape is treated as a literal `%`, exactly like the
    /// existing not-valid-hex case, so the euro sign itself must survive
    /// intact in the output.
    #[test]
    fn url_decode_does_not_panic_on_a_percent_before_a_multibyte_char() {
        assert_eq!(super::url_decode("%€"), "%€");
        // The panicking byte offset sits one further in when there is a
        // normal percent-escape ahead of it too.
        assert_eq!(super::url_decode("a%20%€b"), "a %€b");
    }
}
