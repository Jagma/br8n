use br8n::backup::remote::s3::{normalized_prefix, S3Remote};
use br8n::backup::remote::Remote;
use br8n::config::S3Config;

fn cfg(prefix: &str) -> S3Config {
    S3Config {
        bucket: std::env::var("BR8N_TEST_S3_BUCKET").unwrap_or_else(|_| "example".into()),
        region: std::env::var("BR8N_TEST_S3_REGION").unwrap_or_else(|_| "eu-west-1".into()),
        prefix: prefix.into(),
        profile: std::env::var("BR8N_TEST_AWS_PROFILE").unwrap_or_else(|_| "default".into()),
        storage_class: "STANDARD".into(),
    }
}

#[test]
fn constructing_a_client_needs_no_network_and_no_credentials() {
    let r = S3Remote::new(&S3Config {
        profile: "a-profile-that-does-not-exist".into(),
        ..cfg("br8n/")
    });
    assert!(
        r.is_ok(),
        "credentials must resolve lazily, not at construction"
    );
    assert_eq!(r.unwrap().name(), "s3");
}

#[test]
fn keys_are_prefixed_with_the_configured_prefix() {
    let r = S3Remote::new(&cfg("br8n/")).unwrap();
    assert_eq!(r.full_key("blobs/abc"), "br8n/blobs/abc");
}

#[test]
fn a_prefix_without_a_trailing_slash_still_produces_one_separator() {
    assert_eq!(normalized_prefix("br8n"), "br8n/");
    assert_eq!(normalized_prefix("br8n/"), "br8n/");
    assert_eq!(normalized_prefix(""), "");
    let r = S3Remote::new(&cfg("br8n")).unwrap();
    assert_eq!(r.full_key("blobs/abc"), "br8n/blobs/abc");
}

#[test]
fn an_empty_prefix_writes_at_the_bucket_root() {
    let r = S3Remote::new(&cfg("")).unwrap();
    assert_eq!(r.full_key("blobs/abc"), "blobs/abc");
}

#[test]
fn listed_keys_come_back_relative_to_the_prefix() {
    let r = S3Remote::new(&cfg("br8n/")).unwrap();
    assert_eq!(r.relative_key("br8n/blobs/abc"), Some("blobs/abc"));
    assert_eq!(
        r.relative_key("br8nx/blobs/abc"),
        None,
        "a sibling prefix must not leak into the listing"
    );
}

#[test]
#[ignore]
fn a_real_bucket_round_trips() {
    let c = cfg("br8n-test/");
    assert_ne!(c.bucket, "example", "set BR8N_TEST_S3_BUCKET");
    let r = S3Remote::new(&c).unwrap();
    r.check().unwrap();
    r.put_bytes("blobs/roundtrip", b"hello").unwrap();
    assert_eq!(r.get_bytes("blobs/roundtrip").unwrap(), b"hello");
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out");
    r.get_file("blobs/roundtrip", &dest).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
    assert!(r
        .list("blobs/")
        .unwrap()
        .iter()
        .any(|o| o.key == "blobs/roundtrip"));
    r.delete("blobs/roundtrip").unwrap();
    assert!(r.get_bytes("blobs/roundtrip").is_err());
}

#[derive(Debug)]
struct Layer(&'static str, Option<Box<Layer>>);

impl std::fmt::Display for Layer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Layer {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.1.as_deref().map(|l| l as _)
    }
}

#[test]
fn an_sdk_error_reads_as_its_causes_without_a_debug_dump() {
    let e = Layer(
        "dispatch failure",
        Some(Box::new(Layer(
            "no credentials found",
            Some(Box::new(Layer("profile `nope` is not defined", None))),
        ))),
    );
    assert_eq!(
        br8n::backup::remote::s3::error_chain(&e),
        "dispatch failure: no credentials found: profile `nope` is not defined"
    );
}
