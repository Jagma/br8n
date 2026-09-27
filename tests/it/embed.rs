use br8n::config::EmbedConfig;
use br8n::embed::{Embedder, OllamaEmbedder};

fn cfg() -> EmbedConfig {
    toml::from_str("model = \"qwen3-embedding:0.6b\"\ndimensions = 512").unwrap()
}

#[test]
fn model_id_includes_dimensions_so_a_dim_change_invalidates_the_index() {
    let e = OllamaEmbedder::new(&cfg());
    // `{model}@{dims}+{scheme}` — dimensions catch incompatible truncations,
    // the scheme catches a change in what text actually gets embedded.
    assert_eq!(e.model_id(), "qwen3-embedding:0.6b@512+qwen3");
}

#[test]
fn model_id_excludes_the_host_so_cosmetic_url_changes_dont_force_reindexing() {
    let cfg: EmbedConfig = toml::from_str(
        "model = \"qwen3-embedding:0.6b\"\ndimensions = 512\nollama_url = \"http://otherhost:11434\"",
    )
    .unwrap();
    let e = OllamaEmbedder::new(&cfg);
    // Different host, same model_id: editing the URL doesn't invalidate the index.
    assert_eq!(e.model_id(), "qwen3-embedding:0.6b@512+qwen3");
}

#[test]
fn model_id_is_independent_of_url_normalization() {
    // Since model_id excludes the host, any config differences in the URL
    // (trailing slash, hostname, port) produce the same model_id.
    let with_slash: EmbedConfig = toml::from_str(
        "model = \"qwen3-embedding:0.6b\"\ndimensions = 512\nollama_url = \"http://localhost:11434/\"",
    )
    .unwrap();
    let without_slash: EmbedConfig = toml::from_str(
        "model = \"qwen3-embedding:0.6b\"\ndimensions = 512\nollama_url = \"http://localhost:11434\"",
    )
    .unwrap();
    assert_eq!(
        OllamaEmbedder::new(&with_slash).model_id(),
        OllamaEmbedder::new(&without_slash).model_id(),
    );
}

#[test]
fn each_model_family_gets_the_prefix_scheme_it_was_trained_on() {
    use br8n::embed::ollama::PrefixScheme;

    // The default model is Qwen3, which was NOT trained on Nomic's
    // `search_document:` / `search_query:` markers. Applying them anyway put
    // off-distribution text in front of every vector in the index — and the
    // 0.70 threshold was then calibrated on top of that.
    let qwen = PrefixScheme::for_model("qwen3-embedding:0.6b");
    assert_eq!(qwen, PrefixScheme::Qwen3Instruct);
    assert_eq!(qwen.doc("hi"), "hi", "Qwen3 takes the passage bare");
    assert!(
        qwen.query("hi").starts_with("Instruct:"),
        "Qwen3 wraps the query in an instruction, got {:?}",
        qwen.query("hi")
    );

    let nomic = PrefixScheme::for_model("nomic-embed-text");
    assert_eq!(nomic.doc("hi"), "search_document: hi");
    assert_eq!(nomic.query("hi"), "search_query: hi");

    let e5 = PrefixScheme::for_model("multilingual-e5-large");
    assert_eq!(e5.doc("hi"), "passage: hi");
    assert_eq!(e5.query("hi"), "query: hi");

    // An unknown model gets no prefixes. Guessing wrong is worse than not
    // guessing: a wrong scheme silently degrades every vector.
    let unknown = PrefixScheme::for_model("some-new-embedder:v9");
    assert_eq!(unknown, PrefixScheme::None);
    assert_eq!(unknown.doc("hi"), "hi");
    assert_eq!(unknown.query("hi"), "hi");
}

#[test]
fn changing_the_prefix_scheme_invalidates_the_index() {
    // Vectors written under one scheme are not comparable with vectors written
    // under another, so the scheme has to reach `model_id` — that is the value
    // stamped into the index and checked on open. Without it, switching models
    // between two families that happen to share a width would silently mix two
    // embedding spaces.
    let base: EmbedConfig =
        toml::from_str("model = \"qwen3-embedding:0.6b\"\ndimensions = 512").unwrap();
    let other: EmbedConfig =
        toml::from_str("model = \"nomic-embed-text\"\ndimensions = 512").unwrap();

    let a = OllamaEmbedder::new(&base).model_id();
    let b = OllamaEmbedder::new(&other).model_id();
    assert_ne!(a, b);
    assert!(a.contains("qwen3"), "scheme must appear in the id, got {a}");
    assert!(b.contains("nomic"), "scheme must appear in the id, got {b}");
}

#[test]
fn vectors_are_l2_normalized_for_cosine_similarity() {
    let v = br8n::embed::normalize(vec![3.0, 4.0]);
    assert!((v[0] - 0.6).abs() < 1e-6);
    assert!((v[1] - 0.8).abs() < 1e-6);
}

#[test]
fn normalize_leaves_a_zero_vector_alone_rather_than_producing_nan() {
    let v = br8n::embed::normalize(vec![0.0, 0.0]);
    assert_eq!(v, vec![0.0, 0.0]);
}

#[test]
fn truncation_to_matryoshka_dimensions_then_renormalizes() {
    let v = br8n::embed::fit_dimensions(vec![1.0, 1.0, 1.0, 1.0], 2);
    assert_eq!(v.len(), 2);
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-5);
}

#[test]
fn a_zero_vector_stays_zero_and_never_becomes_nan() {
    // This is the shape a zero-padded placeholder would take. `normalize` must
    // pass it through rather than divide by zero — which is exactly why an empty
    // embedding has to be rejected upstream instead of padded.
    let z = br8n::embed::normalize(vec![0.0; 4]);
    assert!(z.iter().all(|x| *x == 0.0));
    assert!(z.iter().all(|x| !x.is_nan()));
}

#[test]
#[ignore = "requires a running ollama with the model pulled"]
fn live_embedding_returns_configured_dimensions() {
    let e = OllamaEmbedder::new(&cfg());
    let v = e.embed_query("hybrid search").unwrap();
    assert_eq!(v.len(), 512);
}

#[test]
fn a_busy_ollama_is_reported_as_slow_not_as_stopped() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let held: Vec<_> = listener.incoming().collect();
        drop(held);
    });
    let mut c = cfg();
    c.ollama_url = url;
    let err = format!(
        "{:#}",
        OllamaEmbedder::new(&c).embed_query("q").unwrap_err()
    );
    assert!(err.contains("did not answer in time"), "{err}");
    assert!(!err.contains("ollama serve"), "{err}");
}

#[test]
fn a_stopped_ollama_still_says_to_start_it() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let mut c = cfg();
    c.ollama_url = url;
    let err = format!(
        "{:#}",
        OllamaEmbedder::new(&c).embed_query("q").unwrap_err()
    );
    assert!(err.contains("ollama serve"), "{err}");
}

#[test]
fn an_embedding_outage_names_its_cause_once() {
    let inner = anyhow::Error::new(std::io::Error::other("operation timed out"))
        .context("http://host:1234 did not answer in time");
    let err = format!(
        "{:#}",
        anyhow::Error::new(br8n::retrieve::EmbedUnavailable(inner))
    );
    assert_eq!(err.matches("did not answer in time").count(), 1, "{err}");
    assert_eq!(err.matches("operation timed out").count(), 1, "{err}");
}
