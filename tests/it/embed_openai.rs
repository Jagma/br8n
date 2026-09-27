use br8n::config::EmbedConfig;
use br8n::embed::{Embedder, OpenAiEmbedder};
use br8n::env_file::RemoteEmbed;
use std::io::{Read, Write};

fn stub(status: &'static str, body: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut buf = [0u8; 65536];
            let n = stream.read(&mut buf).unwrap_or(0);
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    (format!("http://{addr}"), rx)
}

fn cfg() -> EmbedConfig {
    let mut c = br8n::config::Config::default().embed;
    c.model = "qwen3-embedding:0.6b".into();
    c.dimensions = 2;
    c
}

fn remote(url: &str) -> RemoteEmbed {
    RemoteEmbed {
        url: url.into(),
        model: "wire-name".into(),
        token: "tok".into(),
    }
}

#[test]
fn documents_are_embedded_and_the_token_is_sent() {
    let (url, rx) = stub(
        "200 OK",
        r#"{"data":[{"index":0,"embedding":[3.0,4.0]},{"index":1,"embedding":[1.0,0.0]}]}"#,
    );
    let e = OpenAiEmbedder::new(&cfg(), &remote(&url));
    let out = e.embed_documents(&["a".into(), "b".into()]).unwrap();
    assert_eq!(out.len(), 2);
    assert!((out[0][0] - 0.6).abs() < 1e-5, "{:?}", out[0]);
    let req = rx.recv().unwrap();
    assert!(req.contains("POST /v1/embeddings"), "{req}");
    assert!(
        req.lines()
            .any(|l| l.eq_ignore_ascii_case("authorization: Bearer tok")),
        "{req}"
    );
    assert!(req.contains("\"model\":\"wire-name\""), "{req}");
}

#[test]
fn an_out_of_order_response_is_reordered_by_index() {
    let (url, _rx) = stub(
        "200 OK",
        r#"{"data":[{"index":1,"embedding":[0.0,1.0]},{"index":0,"embedding":[1.0,0.0]}]}"#,
    );
    let e = OpenAiEmbedder::new(&cfg(), &remote(&url));
    let out = e
        .embed_documents(&["first".into(), "second".into()])
        .unwrap();
    assert_eq!(
        out[0],
        vec![1.0, 0.0],
        "row 0 must be the item with index 0"
    );
    assert_eq!(out[1], vec![0.0, 1.0]);
}

#[test]
fn a_short_response_is_refused() {
    let (url, _rx) = stub("200 OK", r#"{"data":[{"index":0,"embedding":[1.0,0.0]}]}"#);
    let e = OpenAiEmbedder::new(&cfg(), &remote(&url));
    let err = e
        .embed_documents(&["a".into(), "b".into()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("1 embeddings for 2 inputs"), "{err}");
}

#[test]
fn a_rejected_token_says_so() {
    let (url, _rx) = stub("401 Unauthorized", r#"{"error":{"message":"no token"}}"#);
    let e = OpenAiEmbedder::new(&cfg(), &remote(&url));
    let err = format!("{:#}", e.embed_query("q").unwrap_err());
    assert!(err.contains("token"), "a 401 must name the token: {err}");
}

#[test]
fn model_id_reports_the_configured_identity_not_the_wire_name() {
    let e = OpenAiEmbedder::new(&cfg(), &remote("http://127.0.0.1:1"));
    assert_eq!(e.model_id(), "qwen3-embedding:0.6b@2+qwen3");
}

#[test]
fn a_duplicated_index_is_refused_rather_than_paired_by_guesswork() {
    let (url, _rx) = stub(
        "200 OK",
        r#"{"data":[{"index":0,"embedding":[1.0,0.0]},{"index":0,"embedding":[0.0,1.0]}]}"#,
    );
    let e = OpenAiEmbedder::new(&cfg(), &remote(&url));
    let err = e
        .embed_documents(&["a".into(), "b".into()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("indices [0, 0]"), "{err}");
}

#[test]
fn a_gap_in_the_indices_is_refused() {
    let (url, _rx) = stub(
        "200 OK",
        r#"{"data":[{"index":0,"embedding":[1.0,0.0]},{"index":2,"embedding":[0.0,1.0]}]}"#,
    );
    let e = OpenAiEmbedder::new(&cfg(), &remote(&url));
    let err = e
        .embed_documents(&["a".into(), "b".into()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("indices [0, 2]"), "{err}");
}

#[test]
fn an_item_missing_its_index_cannot_collide_with_another() {
    let (url, _rx) = stub(
        "200 OK",
        r#"{"data":[{"index":1,"embedding":[1.0,0.0]},{"embedding":[0.0,1.0]}]}"#,
    );
    let e = OpenAiEmbedder::new(&cfg(), &remote(&url));
    let err = e
        .embed_documents(&["a".into(), "b".into()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("indices [1, 1]"), "{err}");
}

#[test]
fn warming_a_remote_that_never_answers_gives_up_on_the_query_timeout() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let held: Vec<_> = listener.incoming().collect();
        drop(held);
    });
    let e = OpenAiEmbedder::new(&cfg(), &remote(&url));
    let started = std::time::Instant::now();
    e.warm().unwrap();
    let waited = started.elapsed();
    assert!(
        waited < std::time::Duration::from_secs(10),
        "SessionStart waits on warm; it took {waited:?}"
    );
}

#[test]
fn for_config_picks_ollama_when_no_remote_is_set() {
    let c = cfg();
    let e = br8n::embed::for_config(&c).unwrap();
    assert_eq!(e.model_id(), "qwen3-embedding:0.6b@2+qwen3");
}

#[test]
fn for_config_refuses_when_the_env_file_was_broken() {
    let mut c = cfg();
    c.remote_error = Some("BR8N_EMBED_TOKEN is not set".into());
    let Err(err) = br8n::embed::for_config(&c) else {
        panic!("a broken env file must refuse, not fall back to ollama");
    };
    let err = err.to_string();
    assert!(err.contains("BR8N_EMBED_TOKEN"), "{err}");
}

#[test]
fn for_config_sends_to_the_remote_when_one_is_set() {
    let (url, rx) = stub("200 OK", r#"{"data":[{"index":0,"embedding":[1.0,0.0]}]}"#);
    let mut c = cfg();
    c.remote = Some(remote(&url));
    let e = br8n::embed::for_config(&c).unwrap();
    let _ = e.embed_query("q");
    let req = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the remote stub must receive the query");
    assert!(req.contains("POST /v1/embeddings"), "{req}");
}

#[test]
fn a_query_the_endpoint_never_answers_is_recognised_as_a_timeout() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let held: Vec<_> = listener.incoming().collect();
        drop(held);
    });
    let e = OpenAiEmbedder::new(&cfg(), &remote(&url));
    let err = e.embed_query("q").unwrap_err();
    assert!(br8n::embed::timed_out(&err), "{err:#}");
}

#[test]
fn a_refused_connection_is_not_mistaken_for_a_timeout() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let e = OpenAiEmbedder::new(&cfg(), &remote(&url));
    let err = e.embed_query("q").unwrap_err();
    assert!(!br8n::embed::timed_out(&err), "{err:#}");
}

#[test]
fn an_unserved_model_names_br8n_embed_model() {
    let (url, _rx) = stub("404 Not Found", r#"{"error":"model not found"}"#);
    let e = OpenAiEmbedder::new(&cfg(), &remote(&url));
    let err = format!("{:#}", e.embed_query("q").unwrap_err());
    assert!(err.contains("BR8N_EMBED_MODEL"), "{err}");
}

#[test]
fn a_busy_server_is_not_blamed_on_the_model_name() {
    let (url, _rx) = stub("503 Service Unavailable", r#"{"error":"loading"}"#);
    let e = OpenAiEmbedder::new(&cfg(), &remote(&url));
    let err = format!("{:#}", e.embed_query("q").unwrap_err());
    assert!(err.contains("503"), "{err}");
    assert!(!err.contains("BR8N_EMBED_MODEL"), "{err}");
}

#[test]
fn a_forbidden_token_says_so() {
    let (url, _rx) = stub("403 Forbidden", r#"{"error":"forbidden"}"#);
    let e = OpenAiEmbedder::new(&cfg(), &remote(&url));
    let err = format!("{:#}", e.embed_query("q").unwrap_err());
    assert!(err.contains("rejected the token"), "{err}");
}

#[test]
fn a_token_with_a_newline_is_blamed_on_the_token_not_the_network() {
    let (url, _rx) = stub("200 OK", r#"{"data":[{"index":0,"embedding":[1.0,0.0]}]}"#);
    let mut r = remote(&url);
    r.token = "tok\n".into();
    let e = OpenAiEmbedder::new(&cfg(), &r);
    let err = format!("{:#}", e.embed_query("q").unwrap_err());
    assert!(err.contains("BR8N_EMBED_TOKEN"), "{err}");
    assert!(!err.contains("unreachable"), "{err}");
}

#[test]
fn a_reply_that_is_not_json_names_the_endpoint() {
    let (url, _rx) = stub("200 OK", "<html>proxy login</html>");
    let e = OpenAiEmbedder::new(&cfg(), &remote(&url));
    let err = format!("{:#}", e.embed_query("q").unwrap_err());
    assert!(err.contains(&url) && err.contains("not JSON"), "{err}");
}

#[test]
fn a_timeout_is_reported_as_slow_not_unreachable() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let held: Vec<_> = listener.incoming().collect();
        drop(held);
    });
    let e = OpenAiEmbedder::new(&cfg(), &remote(&url));
    let err = format!("{:#}", e.embed_query("q").unwrap_err());
    assert!(err.contains("did not answer in time"), "{err}");
    assert!(!err.contains("unreachable"), "{err}");
}
