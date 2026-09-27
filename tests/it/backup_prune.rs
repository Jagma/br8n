use br8n::backup::crypto::Key;
use br8n::backup::manifest::{blob_key, index_key, FileEntry, IndexEntry, LatestPointer, Manifest};
use br8n::backup::prune;
use br8n::backup::remote::{FakeRemote, Remote};

fn at(day: u32) -> String {
    format!("2026-08-{day:02}T03:00:00.000Z")
}

fn seed_with(
    remote: &FakeRemote,
    key: Option<&Key>,
    when: &str,
    blob: &str,
    index: Option<&str>,
) -> String {
    let mut m = Manifest::new(key);
    m.created_at = when.into();
    let key_id = key.map(|k| k.id());
    m.files.push(FileEntry {
        path: "config/config.toml".into(),
        hash: blob.into(),
        size: 1,
        mode: 0o600,
    });
    remote
        .put_bytes(&blob_key(key_id.as_deref(), blob), b"x")
        .unwrap();
    if let Some(ih) = index {
        m.index = Some(IndexEntry {
            hash: ih.into(),
            size: 1,
            embed_model: "m".into(),
            dimensions: 512,
            chunk_tokens: 512,
            documents: 1,
            chunks: 1,
        });
        remote
            .put_bytes(&index_key(key_id.as_deref(), ih), b"i")
            .unwrap();
    }
    let generation = m.generation_key();
    remote
        .put_bytes(&generation, &serde_json::to_vec(&m).unwrap())
        .unwrap();
    generation
}

fn seed(remote: &FakeRemote, day: u32, blob: &str, index: Option<&str>) -> String {
    seed_with(remote, None, &at(day), blob, index)
}

fn point_latest_at(remote: &FakeRemote, generation: &str) {
    let pointer = LatestPointer {
        generation: generation.into(),
    };
    remote
        .put_bytes(
            Manifest::latest_key(),
            &serde_json::to_vec(&pointer).unwrap(),
        )
        .unwrap();
}

#[test]
fn old_generations_are_deleted_and_their_orphan_blobs_collected() {
    let r = FakeRemote::new();
    for day in 1..=5 {
        seed(&r, day, &format!("blob{day}"), None);
    }
    let stats = prune(&r, 2, 2).unwrap();

    assert_eq!(stats.manifests_deleted, 3);
    assert_eq!(stats.blobs_deleted, 3);
    let keys = r.keys();
    assert!(keys.iter().any(|k| k.contains("2026-08-05")), "newest kept");
    assert!(
        keys.iter().any(|k| k.contains("2026-08-04")),
        "second newest kept"
    );
    assert!(!keys.iter().any(|k| k.contains("2026-08-03")));
    assert!(keys.contains(&blob_key(None, "blob5")));
    assert!(keys.contains(&blob_key(None, "blob4")));
    assert!(!keys.contains(&blob_key(None, "blob3")));
}

#[test]
fn a_blob_still_referenced_by_a_surviving_generation_is_never_collected() {
    let r = FakeRemote::new();
    for day in 1..=3 {
        seed(&r, day, "shared", None);
    }
    let stats = prune(&r, 1, 2).unwrap();
    assert_eq!(stats.manifests_deleted, 2);
    assert_eq!(stats.blobs_deleted, 0, "still referenced by the survivor");
    assert!(r.keys().contains(&blob_key(None, "shared")));
}

#[test]
fn the_newest_index_snapshots_are_the_ones_kept() {
    let r = FakeRemote::new();
    for day in 1..=4 {
        seed(&r, day, &format!("blob{day}"), Some(&format!("ix{day}")));
    }
    let stats = prune(&r, 4, 2).unwrap();

    assert_eq!(stats.manifests_deleted, 0, "generations are within budget");
    assert_eq!(stats.index_deleted, 2);
    let keys = r.keys();
    assert!(keys.contains(&index_key(None, "ix4")));
    assert!(keys.contains(&index_key(None, "ix3")));
    assert!(!keys.contains(&index_key(None, "ix2")));
    assert!(!keys.contains(&index_key(None, "ix1")));
}

#[test]
fn an_index_shared_by_several_generations_counts_once_against_its_budget() {
    let r = FakeRemote::new();
    seed(&r, 1, "b1", Some("old"));
    seed(&r, 2, "b2", Some("second"));
    seed(&r, 3, "b3", Some("same"));
    seed(&r, 4, "b4", Some("same"));
    seed(&r, 5, "b5", Some("same"));

    prune(&r, 5, 2).unwrap();
    let keys = r.keys();
    assert!(keys.contains(&index_key(None, "same")));
    assert!(
        keys.contains(&index_key(None, "second")),
        "three generations sharing one snapshot must not use up the whole budget"
    );
    assert!(!keys.contains(&index_key(None, "old")));
}

#[test]
fn zero_budgets_still_keep_the_newest_generation_and_index() {
    let r = FakeRemote::new();
    seed(&r, 1, "b1", Some("ix1"));
    let newest = seed(&r, 2, "b2", Some("ix2"));

    prune(&r, 0, 0).unwrap();
    let keys = r.keys();
    assert!(keys.contains(&newest));
    assert!(keys.contains(&blob_key(None, "b2")));
    assert!(keys.contains(&index_key(None, "ix2")));
}

#[test]
fn the_generation_latest_points_at_survives_even_outside_the_budget() {
    let r = FakeRemote::new();
    let pointed = seed(&r, 1, "pointed", None);
    seed(&r, 2, "b2", None);
    seed(&r, 3, "b3", None);
    point_latest_at(&r, &pointed);

    prune(&r, 1, 1).unwrap();
    let keys = r.keys();
    assert!(keys.contains(&pointed));
    assert!(keys.contains(&blob_key(None, "pointed")));
    assert!(keys.contains(&Manifest::latest_key().to_string()));
    assert!(!keys.iter().any(|k| k.contains("2026-08-02")));
}

#[test]
fn blobs_under_a_rotated_key_go_only_once_no_generation_uses_that_key() {
    let r = FakeRemote::new();
    let old_key = Key::generate();
    let new_key = Key::generate();
    seed_with(&r, Some(&old_key), &at(1), "cfg", None);
    seed_with(&r, Some(&new_key), &at(2), "cfg", None);
    let old_blob = blob_key(Some(old_key.id().as_str()), "cfg");
    let new_blob = blob_key(Some(new_key.id().as_str()), "cfg");

    prune(&r, 2, 1).unwrap();
    assert!(
        r.keys().contains(&old_blob),
        "the old generation still needs it"
    );

    prune(&r, 1, 1).unwrap();
    assert!(!r.keys().contains(&old_blob));
    assert!(r.keys().contains(&new_blob));
}

#[test]
fn nothing_is_deleted_when_any_listing_fails() {
    for failing_listing in 0..3 {
        let r = FakeRemote::new();
        for day in 1..=5 {
            seed(&r, day, &format!("blob{day}"), Some(&format!("ix{day}")));
        }
        let before = r.keys();
        r.fail_list_after(failing_listing);

        assert!(
            prune(&r, 1, 1).is_err(),
            "listing #{failing_listing} failed but prune succeeded"
        );
        assert_eq!(
            r.keys(),
            before,
            "listing #{failing_listing} failed and something was deleted"
        );
    }
}

#[test]
fn nothing_is_deleted_when_any_single_listing_fails_alone() {
    for failing_listing in 0..3 {
        let r = FakeRemote::new();
        for day in 1..=5 {
            seed(&r, day, &format!("blob{day}"), Some(&format!("ix{day}")));
        }
        let before = r.keys();
        r.fail_only_list_number(failing_listing);

        assert!(
            prune(&r, 1, 1).is_err(),
            "only listing #{failing_listing} failed but prune succeeded"
        );
        assert_eq!(
            r.keys(),
            before,
            "only listing #{failing_listing} failed and something was deleted"
        );
    }
}

#[test]
fn nothing_is_deleted_when_a_surviving_manifest_cannot_be_read() {
    let r = FakeRemote::new();
    seed(&r, 1, "blob1", None);
    seed(&r, 2, "blob2", None);
    let keep = seed(&r, 3, "blob3", None);
    r.put_bytes(&keep, b"not json at all").unwrap();
    let before = r.keys();

    assert!(prune(&r, 2, 1).is_err());
    assert_eq!(r.keys(), before);
}

#[test]
fn pruning_an_empty_remote_is_a_no_op() {
    let r = FakeRemote::new();
    assert_eq!(prune(&r, 30, 2).unwrap(), Default::default());
}

#[test]
fn generations_are_ranked_by_key_whatever_order_the_listing_returns() {
    let r = FakeRemote::new();
    for day in 1..=4 {
        seed(&r, day, &format!("blob{day}"), Some(&format!("ix{day}")));
    }
    r.list_in_reverse_key_order();

    prune(&r, 2, 1).unwrap();
    let keys = r.keys();
    assert!(keys.iter().any(|k| k.contains("2026-08-04")));
    assert!(keys.iter().any(|k| k.contains("2026-08-03")));
    assert!(!keys.iter().any(|k| k.contains("2026-08-02")));
    assert!(keys.contains(&index_key(None, "ix4")));
    assert!(!keys.contains(&index_key(None, "ix3")));
}

#[test]
fn a_prune_interrupted_mid_delete_leaves_every_remaining_generation_restorable() {
    for deletes_allowed in 0..8 {
        let r = FakeRemote::new();
        for day in 1..=4 {
            seed(&r, day, &format!("blob{day}"), Some(&format!("ix{day}")));
        }
        r.fail_delete_after(deletes_allowed);
        assert!(
            prune(&r, 1, 1).is_err(),
            "a failed delete after {deletes_allowed} must be reported, not swallowed"
        );

        let keys = r.keys();
        for generation in keys.iter().filter(|k| k.starts_with("manifest/")) {
            let m: Manifest = serde_json::from_slice(&r.get_bytes(generation).unwrap()).unwrap();
            for f in &m.files {
                assert!(
                    keys.contains(&blob_key(None, &f.hash)),
                    "after {deletes_allowed} deletes, {generation} lost its blob {}",
                    f.hash
                );
            }
        }
    }
}

#[test]
fn a_single_failed_delete_is_reported_and_never_orphans_a_surviving_generation() {
    for failing_delete in 0..9 {
        let r = FakeRemote::new();
        for day in 1..=4 {
            seed(&r, day, &format!("blob{day}"), Some(&format!("ix{day}")));
        }
        r.fail_only_delete_number(failing_delete);
        assert!(
            prune(&r, 1, 1).is_err(),
            "delete #{failing_delete} failed and prune reported success"
        );

        let keys = r.keys();
        for generation in keys.iter().filter(|k| k.starts_with("manifest/")) {
            let m: Manifest = serde_json::from_slice(&r.get_bytes(generation).unwrap()).unwrap();
            for f in &m.files {
                assert!(
                    keys.contains(&blob_key(None, &f.hash)),
                    "delete #{failing_delete} failed, and {generation} lost its blob {}",
                    f.hash
                );
            }
        }
    }
}

#[test]
fn nothing_is_deleted_when_latest_exists_but_cannot_be_parsed() {
    let r = FakeRemote::new();
    for day in 1..=3 {
        seed(&r, day, &format!("blob{day}"), None);
    }
    r.put_bytes(Manifest::latest_key(), b"not json at all")
        .unwrap();
    let before = r.keys();

    assert!(prune(&r, 1, 1).is_err());
    assert_eq!(r.keys(), before);
}
