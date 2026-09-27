use br8n::backup::crypto::Key;
use br8n::backup::manifest::{
    blob_key, index_key, Encryption, FileEntry, IndexEntry, Manifest, MANIFEST_VERSION,
};
use br8n::config::Config;

fn index_entry(model: &str, dims: usize, chunk_tokens: usize) -> IndexEntry {
    IndexEntry {
        hash: "deadbeef".into(),
        size: 42,
        embed_model: model.into(),
        dimensions: dims,
        chunk_tokens,
        documents: 10,
        chunks: 100,
    }
}

#[test]
fn a_manifest_round_trips_through_json() {
    let key = Key::generate();
    let mut m = Manifest::new(Some(&key));
    m.files.push(FileEntry {
        path: "config/config.toml".into(),
        hash: "abc123".into(),
        size: 992,
        mode: 0o600,
    });
    m.index = Some(index_entry("qwen3-embedding:0.6b", 512, 512));

    let json = serde_json::to_vec(&m).unwrap();
    let back: Manifest = serde_json::from_slice(&json).unwrap();

    assert_eq!(back.version, MANIFEST_VERSION);
    assert_eq!(back.files.len(), 1);
    assert_eq!(back.files[0].path, "config/config.toml");
    assert_eq!(back.index.as_ref().unwrap().dimensions, 512);
    let encryption: &Encryption = back.encryption.as_ref().unwrap();
    assert_eq!(encryption.key_id, key.id());
    assert_eq!(encryption.algo, "xchacha20poly1305");
}

#[test]
fn an_unencrypted_manifest_records_no_encryption_block() {
    let m = Manifest::new(None);
    assert!(m.encryption.is_none());
}

#[test]
fn the_generation_key_sorts_lexically_by_time() {
    let mut a = Manifest::new(None);
    a.created_at = "2026-08-25T03:00:00Z".into();
    let mut b = Manifest::new(None);
    b.created_at = "2026-09-01T03:00:00Z".into();
    assert!(a.generation_key() < b.generation_key());
    assert!(a.generation_key().starts_with("manifest/"));
    assert!(a.generation_key().ends_with(".json"));
    assert!(
        !a.generation_key().contains(':'),
        "a colon is illegal in some object stores and awkward everywhere else"
    );
}

#[test]
fn a_new_generation_is_stamped_to_the_millisecond() {
    let created_at = Manifest::new(None).created_at;
    let (seconds, fraction) = created_at
        .split_once('.')
        .unwrap_or_else(|| panic!("no sub-second part in {created_at}"));
    assert_eq!(seconds.len(), "2026-09-23T10:00:00".len(), "{created_at}");
    assert_eq!(fraction.len(), "123Z".len(), "{created_at}");
    assert!(fraction.ends_with('Z'), "{created_at}");
    assert!(
        fraction[..3].bytes().all(|b| b.is_ascii_digit()),
        "{created_at}"
    );
}

#[test]
fn generations_within_one_second_get_distinct_keys_that_sort_by_time() {
    let at = |t: &str| {
        let mut m = Manifest::new(None);
        m.created_at = t.into();
        m.generation_key()
    };
    let early = at("2026-09-23T10:00:00.001Z");
    let late = at("2026-09-23T10:00:00.999Z");
    let next_second = at("2026-09-23T10:00:01.000Z");
    assert_ne!(early, late);
    assert!(early < late);
    assert!(late < next_second);
}

#[test]
fn object_keys_are_namespaced() {
    assert_eq!(blob_key(None, "abc"), "blobs/abc");
    assert_eq!(index_key(None, "abc"), "index/abc.tar.gz");
    assert_eq!(Manifest::latest_key(), "manifest/latest.json");
}

#[test]
fn object_keys_are_namespaced_by_key_id_when_encrypted() {
    assert_eq!(blob_key(Some("k1"), "abc"), "blobs/k1/abc");
    assert_eq!(index_key(Some("k1"), "abc"), "index/k1/abc.tar.gz");
}

#[test]
fn a_matching_index_passes_the_compatibility_check() {
    let cfg = Config::default();
    let mut m = Manifest::new(None);
    m.index = Some(index_entry(
        &cfg.embed.model,
        cfg.embed.dimensions,
        cfg.embed.chunk_tokens,
    ));
    assert!(m.check_index_compat(&cfg).is_ok());
}

#[test]
fn a_different_embedding_model_is_refused() {
    let cfg = Config::default();
    let mut m = Manifest::new(None);
    m.index = Some(index_entry("nomic-embed-text", cfg.embed.dimensions, 512));
    let err = m.check_index_compat(&cfg).unwrap_err().to_string();
    assert!(
        err.contains("nomic-embed-text"),
        "must name the mismatch: {err}"
    );
    assert!(err.contains("--reindex"), "must say how to recover: {err}");
}

#[test]
fn a_different_dimension_count_is_refused() {
    let cfg = Config::default();
    let mut m = Manifest::new(None);
    m.index = Some(index_entry(&cfg.embed.model, 768, cfg.embed.chunk_tokens));
    let err = m.check_index_compat(&cfg).unwrap_err().to_string();
    assert!(err.contains("768"), "must name the mismatch: {err}");
}

#[test]
fn a_different_chunk_size_is_refused() {
    let cfg = Config::default();
    let mut m = Manifest::new(None);
    m.index = Some(index_entry(&cfg.embed.model, cfg.embed.dimensions, 1024));
    assert!(m.check_index_compat(&cfg).is_err());
}

#[test]
fn a_manifest_with_no_index_is_trivially_compatible() {
    assert!(Manifest::new(None)
        .check_index_compat(&Config::default())
        .is_ok());
}

#[test]
fn a_manifest_sealed_with_another_key_is_refused_before_decryption() {
    let mine = Key::generate();
    let theirs = Key::generate();
    let m = Manifest::new(Some(&theirs));
    let err = m.check_key(Some(&mine)).unwrap_err().to_string();
    assert!(err.contains("different key"), "got: {err}");
    assert!(m.check_key(Some(&theirs)).is_ok());
}

#[test]
fn an_encrypted_manifest_cannot_be_restored_without_a_key() {
    let key = Key::generate();
    let m = Manifest::new(Some(&key));
    assert!(m.check_key(None).is_err());
}

#[test]
fn a_future_manifest_version_is_refused() {
    let mut m = Manifest::new(None);
    m.version = MANIFEST_VERSION + 1;
    let err = m.check_key(None).unwrap_err().to_string();
    assert!(err.contains("newer"), "got: {err}");
}
