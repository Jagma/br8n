use br8n::retrieve::rerank::Reranker;
use br8n::store::Hit;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

fn hit(id: &str, text: &str, score: f32) -> Hit {
    Hit {
        chunk_id: id.into(),
        doc_id: "d".into(),
        text: text.into(),
        heading_path: String::new(),
        uri: "file:///d.md".into(),
        // Deliberately distinct from `score` (rather than e.g. equal to it) so
        // tests that assert relevance is left untouched, or overwritten, can't
        // pass by accident just because the two fields happened to coincide.
        title: "d".into(),
        page_no: None,
        score,
        relevance: score / 2.0,
        source_type: "markdown".into(),
        inbound: 0,
        lifecycle: Default::default(),
        last_used: None,
        memory: None,
    }
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Binds a stub HTTP server on an OS-assigned loopback port. `answer` maps a
/// request index (0-based, in arrival order) to the "yes"/"no" verdict the
/// stub replies with, letting tests exercise mixed relevance judgements
/// across several candidates. Follows the same pattern as `tests/it/enrich.rs`.
fn spawn_stub_server(answers: Vec<&'static str>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub listener");
    let addr = listener.local_addr().expect("stub listener local_addr");

    thread::spawn(move || {
        for answer in answers {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };

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

            let body = format!(r#"{{"response":"{answer}"}}"#);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body,
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    format!("http://{}", addr)
}

/// A stub server that accepts exactly `answers_before_death` connections and
/// then stops accepting entirely, simulating a connection that dies mid-list
/// (e.g. the model process crashes after scoring the first few candidates).
fn spawn_dying_stub_server(answers_before_death: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub listener");
    let addr = listener.local_addr().expect("stub listener local_addr");

    thread::spawn(move || {
        for _ in 0..answers_before_death {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };

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

            let body = r#"{"response":"yes"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body,
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
        // Drop the listener: any further connection attempts are refused,
        // simulating the backend dying after scoring the first few hits.
    });

    format!("http://{}", addr)
}

#[test]
fn unreachable_backend_returns_input_order_rather_than_failing() {
    let r = Reranker::new("http://127.0.0.1:1", "nope");
    let input = vec![hit("1", "a", 0.9), hit("2", "b", 0.8)];
    let out = r.rerank("query", input.clone(), 10);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].chunk_id, "1", "must degrade to the pre-rerank order");
}

#[test]
fn empty_input_is_handled() {
    let r = Reranker::new("http://127.0.0.1:1", "nope");
    assert!(r.rerank("query", vec![], 10).is_empty());
}

#[test]
fn only_top_n_candidates_are_scored_the_tail_is_appended_unchanged() {
    let r = Reranker::new("http://127.0.0.1:1", "nope");
    let input: Vec<Hit> = (0..30)
        .map(|i| hit(&i.to_string(), "t", 1.0 - i as f32 / 100.0))
        .collect();
    let out = r.rerank("query", input, 5);
    assert_eq!(out.len(), 30, "tail must be preserved, not dropped");
}

#[test]
fn tail_relative_order_is_untouched_by_reranking() {
    let r = Reranker::new("http://127.0.0.1:1", "nope");
    let input: Vec<Hit> = (0..30)
        .map(|i| hit(&i.to_string(), "t", 1.0 - i as f32 / 100.0))
        .collect();
    let out = r.rerank("query", input, 5);
    let tail_ids: Vec<&str> = out[5..].iter().map(|h| h.chunk_id.as_str()).collect();
    let expected: Vec<String> = (5..30).map(|i| i.to_string()).collect();
    assert_eq!(
        tail_ids, expected,
        "hits 6-30 must come back in original relative order"
    );
}

#[test]
fn top_n_larger_than_input_scores_everything_without_panicking() {
    let url = spawn_stub_server(vec!["yes", "no", "yes"]);
    let r = Reranker::new(&url, "model");
    let input = vec![hit("1", "a", 0.5), hit("2", "b", 0.4), hit("3", "c", 0.3)];
    let out = r.rerank("query", input, 100);
    assert_eq!(out.len(), 3);
}

#[test]
fn success_path_against_stub_server_reorders_by_relevance() {
    // Candidate "2" is scored "no" while "1" and "3" score "yes"; the "yes"
    // hits must sort ahead of "2", and since "1" and "3" tie at 1.0 the
    // stable sort must keep them in their original relative order (1 before 3).
    let url = spawn_stub_server(vec!["yes", "no", "yes"]);
    let r = Reranker::new(&url, "model");
    let input = vec![hit("1", "a", 0.1), hit("2", "b", 0.2), hit("3", "c", 0.3)];
    let out = r.rerank("query", input, 10);
    assert_eq!(
        out.iter().map(|h| h.chunk_id.as_str()).collect::<Vec<_>>(),
        vec!["1", "3", "2"]
    );
}

#[test]
fn reranked_order_is_deterministic_across_many_runs() {
    // All hits tie at 1.0 ("yes" for every one of 10 candidates). If the
    // sort weren't stable (or the input order weren't already deterministic)
    // repeated runs would scatter into different orderings, exactly the bug
    // Task 16 fixed for fusion. Run many times and count distinct orderings.
    let input: Vec<Hit> = (0..10)
        .map(|i| hit(&i.to_string(), "t", 1.0 - i as f32 / 100.0))
        .collect();
    let mut orderings = std::collections::HashSet::new();
    for _ in 0..50 {
        let url = spawn_stub_server(vec!["yes"; 10]);
        let r = Reranker::new(&url, "model");
        let out = r.rerank("query", input.clone(), 10);
        let order: Vec<String> = out.iter().map(|h| h.chunk_id.clone()).collect();
        orderings.insert(order);
    }
    assert_eq!(
        orderings.len(),
        1,
        "expected exactly 1 distinct ordering across 50 runs, got {}",
        orderings.len()
    );
}

#[test]
fn mid_list_connection_death_returns_original_pre_rerank_order() {
    // The stub answers the first 2 requests then stops accepting connections,
    // simulating the backend dying partway through scoring 5 candidates.
    // Per the infallible-by-signature contract, ANY failure — even midway
    // through the head — must discard partial scores and fall back to the
    // original, pre-rerank order rather than returning a partially-reordered
    // (and therefore inconsistent) list.
    let url = spawn_dying_stub_server(2);
    let r = Reranker::new(&url, "model");
    let input: Vec<Hit> = (0..5)
        .map(|i| hit(&i.to_string(), "t", 1.0 - i as f32 / 100.0))
        .collect();
    let original_order: Vec<String> = input.iter().map(|h| h.chunk_id.clone()).collect();
    let out = r.rerank("query", input, 5);
    let out_order: Vec<String> = out.iter().map(|h| h.chunk_id.clone()).collect();
    assert_eq!(
        out_order, original_order,
        "a mid-list failure must fall back to pre-rerank order, not a partial reorder"
    );
}

#[test]
fn mid_list_connection_death_leaves_every_score_and_relevance_pristine() {
    // The order-only assertion above is not enough: mutating `head` in place
    // meant a mid-list failure left the candidates scored BEFORE the failure
    // holding their fresh 1.0/0.0 verdicts while the rest kept RRF-scale
    // values, all while happening to preserve the id order. That mixture
    // silently corrupts the gate downstream. Assert the exact floats.
    let url = spawn_dying_stub_server(2);
    let r = Reranker::new(&url, "model");
    let input: Vec<Hit> = (0..5)
        .map(|i| hit(&i.to_string(), "t", 1.0 - i as f32 / 100.0))
        .collect();
    let original: Vec<(f32, f32)> = input.iter().map(|h| (h.score, h.relevance)).collect();

    let out = r.rerank("query", input, 5);

    let after: Vec<(f32, f32)> = out.iter().map(|h| (h.score, h.relevance)).collect();
    assert_eq!(
        after, original,
        "a mid-list failure must leave every score AND relevance exactly as it was, not a mixture of scales"
    );
}

#[test]
fn success_path_reorders_without_redefining_relevance() {
    // The reranker orders; it does not get to redefine what relevance MEANS.
    //
    // It used to write its binary verdict into `relevance`, replacing a
    // calibrated [0,1] cosine with 1.0 or 0.0. That made `threshold` inert at
    // tiers 3 and 4 — every value in (0,1] gated identically — and fed
    // `br8n bench` a scale no other tier produces, so the number it
    // recommended was derived from a mixture of two incomparable signals.
    let url = spawn_stub_server(vec!["yes", "no", "yes"]);
    let r = Reranker::new(&url, "model");
    let input = vec![hit("1", "a", 0.1), hit("2", "b", 0.2), hit("3", "c", 0.3)];
    let before: std::collections::HashMap<String, f32> = input
        .iter()
        .map(|h| (h.chunk_id.clone(), h.relevance))
        .collect();

    let out = r.rerank("query", input, 10);

    let by_id: std::collections::HashMap<&str, &br8n::store::Hit> =
        out.iter().map(|h| (h.chunk_id.as_str(), h)).collect();

    // Verdicts land on `score`, which is what ordering reads.
    assert_eq!(by_id["1"].score, 1.0, "\"yes\" must score 1.0");
    assert_eq!(by_id["2"].score, 0.0, "\"no\" must score 0.0");
    assert_eq!(by_id["3"].score, 1.0, "\"yes\" must score 1.0");

    // Relevance is untouched — still the bi-encoder cosine it arrived with.
    for h in &out {
        assert_eq!(
            h.relevance, before[&h.chunk_id],
            "rerank must not overwrite the cosine relevance the gate is calibrated against"
        );
    }

    // And the rejected hit sorts last rather than being laundered into a score.
    assert_eq!(
        out.last().unwrap().chunk_id,
        "2",
        "the rejected hit must sort last"
    );
}
