use br8n::env_file::{self, RemoteEmbed};
use std::io::Write;

fn write(dir: &std::path::Path, name: &str, body: &str, mode: u32) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(body.as_bytes()).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    p
}

#[test]
fn parse_keeps_values_verbatim_and_ignores_noise() {
    let m = env_file::parse(
        "# a comment\n\nBR8N_EMBED_URL=http://h:1234\nBR8N_EMBED_TOKEN=a=b=c\nnot a pair\n  SPACED = x \n",
    );
    assert_eq!(m.get("BR8N_EMBED_URL").unwrap(), "http://h:1234");
    assert_eq!(m.get("BR8N_EMBED_TOKEN").unwrap(), "a=b=c");
    assert_eq!(m.get("SPACED").unwrap(), "x");
    assert_eq!(m.len(), 3);
}

#[test]
fn quotes_are_part_of_the_value_not_stripped() {
    let m = env_file::parse("BR8N_EMBED_TOKEN=\"abc\"\n");
    assert_eq!(m.get("BR8N_EMBED_TOKEN").unwrap(), "\"abc\"");
}

#[test]
fn a_world_readable_env_file_is_refused_by_name() {
    let t = tempfile::tempdir().unwrap();
    let p = write(t.path(), "env", "BR8N_EMBED_URL=http://h:1234\n", 0o644);
    let err = env_file::read_map(&p).unwrap_err().to_string();
    assert!(err.contains("0600"), "error must say what to do: {err}");
    assert!(
        err.contains(p.to_str().unwrap()),
        "error must name the file: {err}"
    );
}

#[test]
fn a_0600_env_file_is_read() {
    let t = tempfile::tempdir().unwrap();
    let p = write(t.path(), "env", "BR8N_EMBED_URL=http://h:1234\n", 0o600);
    assert_eq!(env_file::read_map(&p).unwrap().len(), 1);
}

#[test]
fn resolve_is_none_when_no_file_and_no_env() {
    let t = tempfile::tempdir().unwrap();
    assert_eq!(
        env_file::resolve(&t.path().join("config.toml")).unwrap(),
        None
    );
}

#[test]
fn resolve_reads_the_file_beside_the_config() {
    let t = tempfile::tempdir().unwrap();
    write(
        t.path(),
        "env",
        "BR8N_EMBED_URL=http://h:1234\nBR8N_EMBED_MODEL=wire-name\nBR8N_EMBED_TOKEN=secret\n",
        0o600,
    );
    let got = env_file::resolve(&t.path().join("config.toml"))
        .unwrap()
        .unwrap();
    assert_eq!(
        got,
        RemoteEmbed {
            url: "http://h:1234".into(),
            model: "wire-name".into(),
            token: "secret".into()
        }
    );
}

#[test]
fn a_url_without_a_token_is_refused() {
    let t = tempfile::tempdir().unwrap();
    write(
        t.path(),
        "env",
        "BR8N_EMBED_URL=http://h:1234\nBR8N_EMBED_MODEL=w\n",
        0o600,
    );
    let err = env_file::resolve(&t.path().join("config.toml"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("BR8N_EMBED_TOKEN"), "{err}");
}

#[test]
fn a_url_without_a_model_is_refused() {
    let t = tempfile::tempdir().unwrap();
    write(
        t.path(),
        "env",
        "BR8N_EMBED_URL=http://h:1234\nBR8N_EMBED_TOKEN=s\n",
        0o600,
    );
    let err = env_file::resolve(&t.path().join("config.toml"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("BR8N_EMBED_MODEL"), "{err}");
}

#[test]
fn debug_output_never_carries_the_token() {
    let remote = RemoteEmbed {
        url: "http://h:1".into(),
        model: "m".into(),
        token: "tok-must-not-leak".into(),
    };
    let shown = format!("{remote:?} {remote:#?}");
    assert!(!shown.contains("tok-must-not-leak"), "{shown}");
    assert!(
        shown.contains("<redacted>") && shown.contains("http://h:1"),
        "{shown}"
    );
}
