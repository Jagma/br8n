mod common;

use br8n::config::Config;
use br8n::memory::{edit_at, remember_at, MemoryKind, Origin, Outcome, Remember};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const THE_YIELD_CAP: Duration = Duration::from_secs(5);
const ATTEMPTS: usize = 3;

fn fake() -> Box<dyn br8n::embed::Embedder> {
    Box::new(common::FakeEmbedder::default())
}

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

fn cfg4() -> Config {
    let mut c = Config::default();
    c.embed.dimensions = 4;
    c
}

fn with_query_marker_announced<T>(write: impl FnOnce() -> T) -> (T, Duration) {
    let marker = br8n::index::query_marker_path(&Config::db_path());
    let priority = br8n::index::QueryPriority::announce(&Config::db_path());
    let stop = Arc::new(AtomicBool::new(false));
    let refresher = {
        let stop = stop.clone();
        let marker = marker.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let _ = std::fs::write(&marker, b"q");
                std::thread::sleep(Duration::from_millis(100));
            }
        })
    };
    let started = Instant::now();
    let out = write();
    let elapsed = started.elapsed();
    stop.store(true, Ordering::Relaxed);
    refresher.join().unwrap();
    drop(priority);
    (out, elapsed)
}

fn finishes_under_the_yield_cap(write: &str, mut attempt: impl FnMut(usize) -> Duration) {
    let mut took = Vec::new();
    for n in 0..ATTEMPTS {
        let elapsed = attempt(n);
        if elapsed < THE_YIELD_CAP {
            return;
        }
        took.push(elapsed);
    }
    panic!("self-yield: {write} took {took:?} while its own process held a fresh query marker");
}

#[test]
fn a_memory_write_does_not_yield_to_the_query_marker_its_own_process_announced() {
    let data = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("BR8N_DB", data.path().join("db")) };

    finishes_under_the_yield_cap("remember_at", |n| {
        let root = data.path().join(format!("save-{n}"));
        let (saved, elapsed) = with_query_marker_announced(|| {
            remember_at(
                &root,
                &cfg4(),
                fake(),
                lesson("Never comment code unless asked."),
            )
        });
        let saved = saved.unwrap();
        assert!(matches!(saved, Outcome::Saved { .. }), "{saved:?}");
        elapsed
    });

    finishes_under_the_yield_cap("edit_at", |n| {
        let root = data.path().join(format!("edit-{n}"));
        let Outcome::Saved { id } = remember_at(
            &root,
            &cfg4(),
            fake(),
            lesson("Never comment code unless asked."),
        )
        .unwrap() else {
            panic!("the save must store a new memory")
        };
        let (edited, elapsed) = with_query_marker_announced(|| {
            edit_at(
                &root,
                &cfg4(),
                fake(),
                &id,
                lesson("Never comment code unless the user asks for it."),
            )
        });
        let edited = edited.unwrap();
        assert!(matches!(edited, Outcome::Replaced { .. }), "{edited:?}");
        elapsed
    });
}
