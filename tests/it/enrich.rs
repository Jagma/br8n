use br8n::config::EmbedConfig;
use br8n::enrich::{request_body, Enricher, GenerateOpts};
use br8n::model::{Chunk, Document, SourceType};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

/// Binds a stub HTTP server on an OS-assigned loopback port, accepts exactly
/// one connection, drains the request (using Content-Length so we don't race
/// reqwest's write), and replies with a canned 200 response carrying `body`
/// as the JSON payload. Returns the `http://127.0.0.1:<port>` base URL.
///
/// The accept thread is detached: on success it replies and exits; if the
/// client never connects it blocks forever on `accept`, but that's harmless
/// since nothing joins it — the real backstop against a hung *test* is the
/// `Enricher`'s own 60s request timeout.
fn spawn_stub_server(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub listener");
    let addr = listener.local_addr().expect("stub listener local_addr");

    thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };

        // Read until we have the full header block, then keep reading until
        // we have at least as many body bytes as Content-Length declares.
        let mut received: Vec<u8> = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    received.extend_from_slice(&buf[..n]);
                    if let Some(header_end) = find_double_crlf(&received) {
                        let headers = String::from_utf8_lossy(&received[..header_end]);
                        let content_length: usize = headers
                            .lines()
                            .find_map(|l| {
                                l.strip_prefix("Content-Length:")
                                    .or_else(|| l.strip_prefix("content-length:"))
                            })
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(0);
                        let body_so_far = received.len() - (header_end + 4);
                        if body_so_far >= content_length {
                            break;
                        }
                    }
                }
                Err(_) => break,
            }
        }

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body,
        );
        // Deliberately swallow write errors: a failed reply must not panic
        // this detached thread and take the test process down with it.
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    });

    format!("http://{}", addr)
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn chunks() -> Vec<Chunk> {
    vec![Chunk {
        id: "d:0".into(),
        doc_id: "d".into(),
        ord: 0,
        text: "It failed for three reasons.".into(),
        embed_text: "Doc > Section\n\nIt failed for three reasons.".into(),
        heading_path: "Section".into(),
        page_no: None,
    }]
}

#[test]
fn disabled_by_default_and_leaves_chunks_untouched() {
    let cfg: EmbedConfig = toml::from_str("").unwrap();
    assert!(!cfg.contextual);

    let doc = Document::new(SourceType::Markdown, "file:///d.md", "Doc", "body");
    let mut cs = chunks();
    let before = cs[0].embed_text.clone();
    Enricher::new(&cfg).enrich(&doc, &mut cs).unwrap();
    assert_eq!(cs[0].embed_text, before);
}

#[test]
fn enrichment_never_alters_display_text() {
    let cfg: EmbedConfig = toml::from_str("contextual = true").unwrap();
    let doc = Document::new(SourceType::Markdown, "file:///d.md", "Doc", "body");
    let mut cs = chunks();
    let display_before = cs[0].text.clone();
    // Ollama may be absent in CI; enrich must degrade rather than fail the index.
    let _ = Enricher::new(&cfg).enrich(&doc, &mut cs);
    assert_eq!(cs[0].text, display_before);
}

#[test]
fn prompt_includes_document_context_and_the_chunk() {
    let p = br8n::enrich::build_prompt("Postgres migration", "body text here", "It failed.");
    assert!(p.contains("Postgres migration"));
    assert!(p.contains("It failed."));
    assert!(p.to_lowercase().contains("situate"));
}

#[test]
fn enrichment_prepends_blurb_from_stub_and_leaves_display_text_untouched() {
    let url =
        spawn_stub_server(r#"{"response":"This chunk explains why the pooler was rejected."}"#);
    let cfg: EmbedConfig =
        toml::from_str(&format!("contextual = true\nollama_url = \"{url}\"")).unwrap();

    let doc = Document::new(SourceType::Markdown, "file:///d.md", "Doc", "body");
    let mut cs = chunks();
    let text_before = cs[0].text.clone();
    let embed_text_before = cs[0].embed_text.clone();

    let result = Enricher::new(&cfg).enrich(&doc, &mut cs);

    assert!(result.is_ok());
    assert!(cs[0]
        .embed_text
        .starts_with("This chunk explains why the pooler was rejected."));
    assert!(cs[0].embed_text.contains(&embed_text_before));
    // The regression this test exists to catch: assigning into c.text
    // instead of c.embed_text would alter the chunk's display text.
    assert_eq!(cs[0].text, text_before);
}

#[test]
fn enrichment_ignores_whitespace_only_blurb() {
    let url = spawn_stub_server(r#"{"response":"   "}"#);
    let cfg: EmbedConfig =
        toml::from_str(&format!("contextual = true\nollama_url = \"{url}\"")).unwrap();

    let doc = Document::new(SourceType::Markdown, "file:///d.md", "Doc", "body");
    let mut cs = chunks();
    let embed_text_before = cs[0].embed_text.clone();

    let result = Enricher::new(&cfg).enrich(&doc, &mut cs);

    assert!(result.is_ok());
    assert_eq!(cs[0].embed_text, embed_text_before);
}

#[test]
fn the_enricher_request_body_is_unchanged_by_the_shared_helper() {
    let opts = GenerateOpts {
        num_predict: 80,
        num_ctx: None,
        keep_alive: "30m".into(),
        temperature: 0.0,
    };
    let got = request_body("qwen3:4b", "hello", &opts);
    let want = serde_json::json!({
        "model": "qwen3:4b",
        "prompt": "hello",
        "stream": false,
        "keep_alive": "30m",
        "think": false,
        "options": { "num_predict": 80, "temperature": 0.0 },
    });
    assert_eq!(got, want);
}

#[test]
fn distillation_options_add_a_context_window_and_unload_immediately() {
    let opts = GenerateOpts {
        num_predict: 400,
        num_ctx: Some(8192),
        keep_alive: "0".into(),
        temperature: 0.0,
    };
    let got = request_body("qwen3:4b", "p", &opts);
    assert_eq!(got["options"]["num_ctx"], 8192);
    assert_eq!(got["keep_alive"], "0");
}
