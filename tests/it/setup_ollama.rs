use br8n::setup::ollama::{preflight, probe, OllamaState};
use std::io::{Read, Write};

fn serve(body: &'static str) -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { return };
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
        }
    });
    format!("http://{addr}")
}

const TAGS: &str = r#"{"models":[{"name":"qwen3-embedding:0.6b","model":"qwen3-embedding:0.6b"}]}"#;

#[test]
fn a_closed_port_is_unreachable_with_the_reason() {
    match probe("http://127.0.0.1:1", &["x".to_string()]) {
        OllamaState::Unreachable(why) => assert!(!why.is_empty()),
        other => panic!("expected Unreachable, got {other:?}"),
    }
}

#[test]
fn present_and_missing_models_are_told_apart() {
    let url = serve(TAGS);
    match probe(
        &url,
        &["qwen3-embedding:0.6b".to_string(), "qwen3:0.6b".to_string()],
    ) {
        OllamaState::Reachable { missing } => assert_eq!(missing, vec!["qwen3:0.6b".to_string()]),
        other => panic!("{other:?}"),
    }
}

static PULLED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
static PULLED_TESTS_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn record_pull(m: &str) -> anyhow::Result<()> {
    PULLED.lock().unwrap().push(m.to_string());
    Ok(())
}
fn refuse(_: &str) -> bool {
    false
}
fn accept(_: &str) -> bool {
    true
}

#[test]
fn a_missing_model_is_pulled_only_with_consent() {
    let _serial = PULLED_TESTS_SERIAL.lock().unwrap();
    PULLED.lock().unwrap().clear();
    let url = serve(TAGS);
    let models = vec!["qwen3:0.6b".to_string()];
    let (_, warnings) = preflight(&url, &models, false, refuse, record_pull);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("ollama pull qwen3:0.6b")),
        "{warnings:?}"
    );
    assert!(PULLED.lock().unwrap().is_empty());
    let (lines, warnings) = preflight(&url, &models, false, accept, record_pull);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(
        lines.iter().any(|l| l.contains("pulled qwen3:0.6b")),
        "{lines:?}"
    );
    assert_eq!(*PULLED.lock().unwrap(), vec!["qwen3:0.6b".to_string()]);
}

#[test]
fn yes_skips_the_prompt_and_unreachable_names_ollama_serve() {
    let _serial = PULLED_TESTS_SERIAL.lock().unwrap();
    let url = serve(TAGS);
    PULLED.lock().unwrap().clear();
    let (_, warnings) = preflight(&url, &["m".to_string()], true, refuse, record_pull);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(*PULLED.lock().unwrap(), vec!["m".to_string()]);
    let (_, warnings) = preflight(
        "http://127.0.0.1:1",
        &["m".to_string()],
        true,
        accept,
        record_pull,
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("ollama serve") && w.contains("ollama.com")),
        "{warnings:?}"
    );
}
