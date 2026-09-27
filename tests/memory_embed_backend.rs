use br8n::config::Config;
use br8n::memory::{MemoryKind, Origin, Remember};

fn lesson(text: &str) -> Remember {
    Remember {
        kind: MemoryKind::Lesson,
        text: text.into(),
        title: None,
        project: None,
        confidence: 100,
        origin: Origin::User,
        session: None,
        source_hash: None,
        source_stamp: None,
        created: None,
    }
}

#[test]
fn saving_and_editing_embed_through_the_configured_backend() {
    let data = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("BR8N_DB", data.path().join("db")) };

    let mut cfg = Config::default();
    cfg.embed.dimensions = 4;
    let broken = "the remote embedding endpoint is misconfigured";
    cfg.embed.remote_error = Some(broken.into());

    let saved = br8n::memory::remember(&cfg, lesson("Never comment code unless asked."));
    let edited = br8n::memory::edit(&cfg, "0", lesson("Never comment code unless asked to."));

    for (write, result) in [
        ("remember", saved.map(|_| ())),
        ("edit", edited.map(|_| ())),
    ] {
        let err = result.expect_err(write);
        assert!(
            format!("{err:#}").contains(broken),
            "{write} must build its embedder from the configured backend, which refuses here; got: {err:#}"
        );
    }
    assert!(
        !data.path().join("memory").exists(),
        "a refused write must not create a memory store"
    );
}
