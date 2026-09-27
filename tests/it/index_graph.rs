use crate::common;
use br8n::model::{Document, SourceType};
use common::setup;

#[test]
fn wikilinks_resolve_to_links_to_edges_between_documents() {
    let (_d, idx, _calls) = setup();
    let a = Document::new(
        SourceType::Markdown,
        "file:///notes/postgres.md",
        "Postgres",
        "See [[pooling]].",
    );
    let mut a = a;
    a.links = vec!["pooling".into()];
    let b = Document::new(
        SourceType::Markdown,
        "file:///notes/pooling.md",
        "pooling",
        "PgBouncer.",
    );

    idx.index_documents(&[a.clone(), b.clone()]).unwrap();
    let edges = idx.resolve_links(&[a.clone(), b.clone()]).unwrap();

    assert_eq!(edges, 1);
    assert_eq!(
        idx.store().linked_docs(&a.id, "wikilink").unwrap(),
        vec![b.id.clone()]
    );
}

#[test]
fn an_ambiguous_title_does_not_resolve_to_an_arbitrary_document() {
    // Two notes titled "README" is the norm in a real corpus. Picking one by scan
    // order links the reader to the wrong project's note, and the winner can
    // change between runs.
    let (_d, idx, _c) = setup();
    let r1 = Document::new(
        SourceType::Markdown,
        "file:///proj-a/README.md",
        "README",
        "Project A.",
    );
    let r2 = Document::new(
        SourceType::Markdown,
        "file:///proj-b/README.md",
        "README",
        "Project B.",
    );
    let mut linker = Document::new(
        SourceType::Markdown,
        "file:///n.md",
        "Note",
        "See [[README]].",
    );
    linker.links = vec!["README".into()];

    let docs = vec![r1, r2, linker.clone()];
    idx.index_documents(&docs).unwrap();
    idx.resolve_links(&docs).unwrap();

    assert!(
        idx.store()
            .linked_docs(&linker.id, "wikilink")
            .unwrap()
            .is_empty(),
        "an ambiguous wikilink must fail closed rather than pick one arbitrarily"
    );
}

#[test]
fn an_unambiguous_title_still_resolves_when_others_exist() {
    // Failing closed on ambiguity must not break the ordinary case.
    let (_d, idx, _c) = setup();
    let a = Document::new(
        SourceType::Markdown,
        "file:///proj-a/README.md",
        "README",
        "A.",
    );
    let b = Document::new(
        SourceType::Markdown,
        "file:///notes/pooling.md",
        "pooling",
        "PgBouncer.",
    );
    let mut linker = Document::new(
        SourceType::Markdown,
        "file:///n.md",
        "Note",
        "See [[pooling]].",
    );
    linker.links = vec!["pooling".into()];

    let docs = vec![a, b.clone(), linker.clone()];
    idx.index_documents(&docs).unwrap();
    idx.resolve_links(&docs).unwrap();

    assert_eq!(
        idx.store().linked_docs(&linker.id, "wikilink").unwrap(),
        vec![b.id]
    );
}

#[test]
fn a_single_document_can_link_to_one_indexed_earlier() {
    // `br8n add <file>` calls resolve_links with exactly one document. Building
    // the lookup from that slice would make every wikilink unresolvable.
    let (_d, idx, _c) = setup();

    let target = Document::new(SourceType::Markdown, "file:///beta.md", "beta", "Body.");
    idx.index_documents(std::slice::from_ref(&target)).unwrap();
    idx.resolve_links(std::slice::from_ref(&target)).unwrap();

    // A separate, later invocation — as `br8n add` would do.
    let mut a = Document::new(
        SourceType::Markdown,
        "file:///a.md",
        "Alpha",
        "See [[beta]].",
    );
    a.links = vec!["beta".into()];
    idx.index_documents(std::slice::from_ref(&a)).unwrap();
    let edges = idx.resolve_links(std::slice::from_ref(&a)).unwrap();

    assert_eq!(
        edges, 1,
        "a wikilink to an already-indexed note must resolve"
    );
    assert_eq!(idx.store().linked_docs(&a.id, "wikilink").unwrap().len(), 1);
}

#[test]
fn resolve_links_is_idempotent_across_repeated_index_runs() {
    // SessionStart runs `br8n index` every session, and resolve_links processes
    // every document including hash-skipped ones. With CREATE instead of MERGE,
    // edges accumulate one per run forever.
    let (_d, idx, _c) = setup();
    let mut a = Document::new(
        SourceType::Markdown,
        "file:///a.md",
        "Alpha",
        "See [[beta]].",
    );
    a.links = vec!["beta".into()];
    let b = Document::new(SourceType::Markdown, "file:///beta.md", "beta", "Body.");
    let docs = vec![a.clone(), b];

    for _ in 0..4 {
        idx.index_documents(&docs).unwrap();
        idx.resolve_links(&docs).unwrap();
    }

    assert_eq!(
        idx.store().linked_docs(&a.id, "wikilink").unwrap().len(),
        1,
        "repeated indexing must not accumulate duplicate LINKS_TO edges"
    );
}

#[test]
fn unresolvable_wikilinks_are_dropped_not_errors() {
    let (_d, idx, _calls) = setup();
    let mut a = Document::new(
        SourceType::Markdown,
        "file:///notes/a.md",
        "A",
        "See [[ghost]].",
    );
    a.links = vec!["ghost".into()];
    idx.index_documents(&[a.clone()]).unwrap();
    assert_eq!(idx.resolve_links(&[a]).unwrap(), 0);
}

#[test]
fn web_documents_link_to_their_source_domain() {
    let (_d, idx, _calls) = setup();
    let mut d = Document::new(SourceType::Web, "https://example.com/x", "X", "body");
    d.meta = serde_json::json!({ "domain": "example.com" });
    idx.index_documents(&[d.clone()]).unwrap();
    idx.resolve_links(&[d.clone()]).unwrap();
    assert_eq!(
        idx.store().source_domain(&d.id).unwrap().as_deref(),
        Some("example.com")
    );
}

/// An inbound wikilink must survive its TARGET being re-indexed.
///
/// `upsert_document` opens with `MATCH (d:Document {id: $id}) DETACH DELETE d`
/// (`src/store/mod.rs`), and DETACH DELETE removes edges in BOTH directions —
/// so every `A -> B` edge dies when B is re-indexed. The incremental path then
/// calls `resolve_links(&found.docs)` with only the CHANGED documents
/// (`src/index.rs`, the `idx.resolve_links(&found.docs)` call), and
/// `resolve_links`' write loop is `for d in docs`, so it rebuilds edges FROM
/// those documents only. Nothing recreates `A -> B`, because A did not change.
///
/// Measured on the live index before the fix: the vault held 67 resolvable
/// inbound wikilinks and the store held 44 `links_to` edges — 23 gone, and the
/// loss is monotonic, since every re-index of a linked-to document drops more.
///
/// `br8n add` already carries this fix and its own measurement
/// (`src/main.rs`: "Measured: A->B was 1, dropped to 0 after `br8n add` of B").
/// The incremental path never got it.
///
/// WHAT THIS TEST DOES AND DOES NOT PROVE. It reproduces the incremental
/// path's CALL SHAPE — index only the changed document, then resolve links for
/// only that document — rather than driving `reindex_swap_with`, which builds
/// its own Ollama embedder and so cannot run offline. The fidelity that matters
/// is passing B alone to both calls, exactly as the incremental path does.
///
/// Three documents, not two: A and C both link to B, so a fix that rebuilt one
/// arbitrary edge rather than all of them still fails. The assertions name the
/// specific edges rather than counting, so an unrelated edge appearing cannot
/// mask a real loss, and `inbound_link_counts` is checked too because that is
/// what `authority` weighting actually reads.
#[test]
fn an_inbound_wikilink_survives_its_target_being_reindexed() {
    let (_d, idx, _calls) = setup();

    let mut a = Document::new(
        SourceType::Markdown,
        "file:///notes/alpha.md",
        "Alpha",
        "See [[beta]].",
    );
    a.links = vec!["beta".into()];
    let mut c = Document::new(
        SourceType::Markdown,
        "file:///notes/gamma.md",
        "Gamma",
        "Also see [[beta]].",
    );
    c.links = vec!["beta".into()];
    let b = Document::new(
        SourceType::Markdown,
        "file:///notes/beta.md",
        "beta",
        "The target everyone links to.",
    );

    idx.index_documents(&[a.clone(), b.clone(), c.clone()])
        .unwrap();
    idx.resolve_links(&[a.clone(), b.clone(), c.clone()])
        .unwrap();

    assert_eq!(
        idx.store().linked_docs(&a.id, "wikilink").unwrap(),
        vec![b.id.clone()],
        "fixture precondition: A must link to B before the re-index"
    );
    assert_eq!(
        idx.store().linked_docs(&c.id, "wikilink").unwrap(),
        vec![b.id.clone()],
        "fixture precondition: C must link to B before the re-index"
    );

    // B changes and is re-indexed ALONE — precisely what the incremental path
    // does with `found.docs`. A and C are untouched on disk, so they are not in
    // that slice and `resolve_links` never sees them.
    let b2 = Document::new(
        SourceType::Markdown,
        "file:///notes/beta.md",
        "beta",
        "The target everyone links to, now with an extra sentence.",
    );
    assert_eq!(b2.id, b.id, "same uri must yield the same doc id");
    idx.index_documents(std::slice::from_ref(&b2)).unwrap();
    idx.resolve_links(std::slice::from_ref(&b2)).unwrap();

    assert_eq!(
        idx.store().linked_docs(&a.id, "wikilink").unwrap(),
        vec![b.id.clone()],
        "A -> B was destroyed by re-indexing B and never rebuilt"
    );
    assert_eq!(
        idx.store().linked_docs(&c.id, "wikilink").unwrap(),
        vec![b.id.clone()],
        "C -> B was destroyed by re-indexing B and never rebuilt"
    );
    assert_eq!(
        idx.store()
            .inbound_link_counts()
            .unwrap()
            .get(&b.id)
            .copied(),
        Some(2),
        "authority weighting reads inbound_link_counts, and it must still see both links"
    );
}

/// Restoring inbound links must not resurrect a link the author DELETED.
///
/// `upsert_document` now saves inbound `LINKS_TO` across its own delete. The
/// obvious way to get that wrong is to restore edges that should have gone:
/// when A stops linking to B, A's re-index must drop `A -> B` and B's later
/// re-index must not bring it back from a stale snapshot.
///
/// The fixture keeps a THIRD document linking to B throughout, so the
/// assertions distinguish "the removed edge is gone" from "all edges are gone"
/// — a fix that simply stopped restoring anything would pass the first
/// assertion and fail the second.
#[test]
fn re_indexing_does_not_resurrect_a_link_its_author_removed() {
    let (_d, idx, _calls) = setup();

    let mut a = Document::new(
        SourceType::Markdown,
        "file:///notes/alpha.md",
        "Alpha",
        "See [[beta]].",
    );
    a.links = vec!["beta".into()];
    let mut keeper = Document::new(
        SourceType::Markdown,
        "file:///notes/keeper.md",
        "Keeper",
        "Still see [[beta]].",
    );
    keeper.links = vec!["beta".into()];
    let b = Document::new(
        SourceType::Markdown,
        "file:///notes/beta.md",
        "beta",
        "The target.",
    );

    idx.index_documents(&[a.clone(), b.clone(), keeper.clone()])
        .unwrap();
    idx.resolve_links(&[a.clone(), b.clone(), keeper.clone()])
        .unwrap();
    assert_eq!(
        idx.store()
            .inbound_link_counts()
            .unwrap()
            .get(&b.id)
            .copied(),
        Some(2),
        "fixture precondition: B starts with two inbound links"
    );

    // The author edits A and removes the wikilink.
    let a2 = Document::new(
        SourceType::Markdown,
        "file:///notes/alpha.md",
        "Alpha",
        "No link here any more.",
    );
    assert!(a2.links.is_empty(), "the edited A must carry no links");
    idx.index_documents(std::slice::from_ref(&a2)).unwrap();
    idx.resolve_links(std::slice::from_ref(&a2)).unwrap();

    assert!(
        idx.store()
            .linked_docs(&a2.id, "wikilink")
            .unwrap()
            .is_empty(),
        "A no longer links to B, so the edge must be gone"
    );

    // Now B is re-indexed. The restore must not bring `A -> B` back.
    let b2 = Document::new(
        SourceType::Markdown,
        "file:///notes/beta.md",
        "beta",
        "The target, edited.",
    );
    idx.index_documents(std::slice::from_ref(&b2)).unwrap();
    idx.resolve_links(std::slice::from_ref(&b2)).unwrap();

    assert!(
        idx.store()
            .linked_docs(&a2.id, "wikilink")
            .unwrap()
            .is_empty(),
        "re-indexing B resurrected a link A had deleted"
    );
    assert_eq!(
        idx.store().inbound_link_counts().unwrap().get(&b.id).copied(),
        Some(1),
        "the keeper's link must survive — a fix that restores nothing also passes the assertion above"
    );
}

/// SETTLES: does lbug 0.19.1 `MERGE` match on a relationship PROPERTY?
///
/// `link_documents` puts `kind` inside the MERGE pattern:
///     MERGE (a)-[:LINKS_TO {kind: $kind}]->(b)
/// If lbug matches on the property, a wikilink and a typed relation between the
/// same pair collapse to ONE edge and the second call silently overwrites the
/// first's kind. If it does not, they are TWO edges — and `inbound_link_counts`,
/// which is kind-blind, double-counts the pair. That number feeds `pack.links`
/// and the `authority` weight, so the answer is load-bearing for any typed
/// relation, not a curiosity.
///
/// This test ASSERTS THE ANSWER, whatever it is, so a future lbug upgrade that
/// changes the semantics fails here rather than silently moving authority.
#[test]
fn merge_on_a_relationship_property_decides_whether_two_kinds_are_one_edge() {
    let (_d, idx, _calls) = setup();
    let a = Document::new(SourceType::Markdown, "file:///notes/a.md", "A", "body");
    let b = Document::new(SourceType::Markdown, "file:///notes/b.md", "B", "body");
    idx.index_documents(&[a.clone(), b.clone()]).unwrap();

    let store = idx.store();
    store.link_documents(&a.id, &b.id, "wikilink").unwrap();
    store.link_documents(&a.id, &b.id, "superseded-by").unwrap();

    let edges: Vec<_> = store
        .all_links_to_edges()
        .unwrap()
        .into_iter()
        .filter(|(from, to, _)| from == &a.id && to == &b.id)
        .collect();
    let inbound = store.inbound_link_counts().unwrap().get(&b.id).copied();

    eprintln!("MERGE-on-property: edges = {edges:?}, inbound_link_counts[B] = {inbound:?}");

    // MEASURED against real lbug 0.19.1: TWO edges. The property IS part of the
    // match pattern, so the two kinds coexist rather than the second overwriting
    // the first.
    assert_eq!(
        edges.len(),
        2,
        "lbug MERGEs on the relationship property, so two kinds are two edges"
    );
    let mut kinds: Vec<&str> = edges.iter().map(|(_, _, k)| k.as_str()).collect();
    kinds.sort_unstable();
    assert_eq!(
        kinds,
        vec!["superseded-by", "wikilink"],
        "both kinds survive; neither overwrites the other"
    );

    // The consequence that made the answer above matter, and the fix for it.
    //
    // Two edges between one pair USED TO mean that pair contributed TWO to B's
    // authority the moment a typed relation joined an existing wikilink —
    // feeding `pack.links` and the `authority` weight with a number that said
    // "more linked" about a pair that was no more linked than before, and that
    // pointed the wrong way: a `superseded-by` edge would LIFT the dead record.
    //
    // `inbound_link_counts` now takes a `kind` allowlist, so it reads 1 here:
    // the storage-layer answer is still two edges (asserted above, and that is
    // lbug's semantics, not ours to change), while the RETRIEVAL answer is one
    // link. The two assertions in this test are deliberately different numbers
    // for that reason. `authority_counts_wikilinks_only_not_every_edge_kind`
    // owns the allowlist itself; this one pins that the two layers disagree on
    // purpose, so a future change collapsing them fails here.
    assert_eq!(
        inbound,
        Some(1),
        "two edges, but only one WIKILINK — authority must count the human's link, \
         not the lifecycle relation riding alongside it"
    );

    // Repeating the SAME kind must stay idempotent — that is already pinned by
    // `resolve_links_is_idempotent_across_repeated_index_runs`; assert it here
    // too so this test distinguishes "MERGE is broken" from "MERGE keys on kind".
    store.link_documents(&a.id, &b.id, "wikilink").unwrap();
    let after: Vec<_> = store
        .all_links_to_edges()
        .unwrap()
        .into_iter()
        .filter(|(from, to, _)| from == &a.id && to == &b.id)
        .collect();
    eprintln!("MERGE-on-property: after repeating 'wikilink', edges = {after:?}");
    assert_eq!(
        after.len(),
        edges.len(),
        "repeating an EXISTING kind must not add an edge; if this fires, MERGE \
         is not idempotent at all and the rest of this test means nothing"
    );
}

/// Authority counts WIKILINKS, not every edge kind.
///
/// `inbound_link_counts` feeds `authority_lift` and `pack.links`. lbug MERGEs on
/// the relationship property (see the test above), so a pair that is already
/// wikilinked and then gains a typed relation has TWO edges while being no more
/// linked than before. Counting both would hand a superseded record an authority
/// LIFT from the very edge that marks it dead.
///
/// The fixture deliberately contains a non-wikilink edge from a THIRD document
/// AND a second kind on an existing pair, because those are two different ways
/// to over-count and a fixture with only wikilinks passes with the predicate
/// deleted.
#[test]
fn authority_counts_wikilinks_only_not_every_edge_kind() {
    let (_d, idx, _calls) = setup();
    let a = Document::new(SourceType::Markdown, "file:///notes/a.md", "A", "body");
    let b = Document::new(SourceType::Markdown, "file:///notes/b.md", "B", "body");
    let c = Document::new(SourceType::Markdown, "file:///notes/c.md", "C", "body");
    idx.index_documents(&[a.clone(), b.clone(), c.clone()])
        .unwrap();
    let store = idx.store();

    // One genuine inbound wikilink.
    store.link_documents(&a.id, &b.id, "wikilink").unwrap();
    // The live ADR shape: the SAME pair also declares a lifecycle relation.
    store.link_documents(&a.id, &b.id, "superseded-by").unwrap();
    // And a lifecycle relation from a document that does NOT wikilink to B.
    store.link_documents(&c.id, &b.id, "superseded-by").unwrap();
    // A kind that is NEITHER wikilink nor superseded-by. This is what makes the
    // predicate an ALLOWLIST rather than a denylist, and the assertion below
    // cannot tell the two apart without it: with `WHERE r.kind <> 'superseded-by'`
    // this edge is counted and B reads 2. Stands in for whatever kind ships next.
    store.link_documents(&c.id, &b.id, "see-also").unwrap();

    let counts = store.inbound_link_counts().unwrap();

    // Four edges point at B; exactly one of them is a link a human wrote.
    assert_eq!(
        counts.get(&b.id).copied(),
        Some(1),
        "B has 4 inbound edges but only 1 wikilink; authority must count the wikilink. \
         A `None` here means the kind predicate failed to execute and `count_map` \
         swallowed the error into an empty map — which would silently zero authority \
         for the whole corpus, not just this document."
    );
    // A and C are link TARGETS of nothing, so they must be absent rather than 0 —
    // `inbound_link_counts` returns only documents that have at least one.
    assert_eq!(counts.get(&a.id).copied(), None);
    assert_eq!(counts.get(&c.id).copied(), None);
}

/// Frontmatter relations become TYPED edges, and the wikilink between the same
/// pair is untouched. lbug MERGEs on the relationship property, so these are
/// genuinely two edges — see
/// `merge_on_a_relationship_property_decides_whether_two_kinds_are_one_edge`.
#[test]
fn frontmatter_supersession_becomes_a_typed_edge_beside_the_wikilink() {
    let (_d, idx, _calls) = setup();

    let mut old = Document::new(
        SourceType::Markdown,
        "file:///d/0004-bazel.md",
        "Bazel and NixOS",
        "See [[0005-nix]].",
    );
    old.links = vec!["0005-nix".into()];
    old.meta =
        serde_json::json!({"id": "ADR-0004", "status": "superseded", "superseded_by": "ADR-0005"});

    let mut new = Document::new(
        SourceType::Markdown,
        "file:///d/0005-nix.md",
        "Nix for build and delivery",
        "Supersedes the old one.",
    );
    new.meta =
        serde_json::json!({"id": "ADR-0005", "status": "accepted", "supersedes": "ADR-0004"});

    idx.index_documents(&[old.clone(), new.clone()]).unwrap();
    idx.resolve_links(&[old.clone(), new.clone()]).unwrap();

    let kinds = |from: &str, to: &str| -> Vec<String> {
        let mut k: Vec<String> = idx
            .store()
            .all_links_to_edges()
            .unwrap()
            .into_iter()
            .filter(|(a, b, _)| a == from && b == to)
            .map(|(_, _, kind)| kind)
            .collect();
        k.sort_unstable();
        k
    };

    // 0004 -> 0005 carries BOTH: the prose wikilink and the declared relation.
    assert_eq!(
        kinds(&old.id, &new.id),
        vec!["superseded-by".to_string(), "wikilink".to_string()],
        "the typed edge must sit BESIDE the wikilink, not replace it"
    );
    // 0005 -> 0004 carries only the declared relation; there is no wikilink in
    // its body. This is what proves `supersedes` is resolved by `id`, since
    // "ADR-0004" is neither a title nor a filename stem.
    assert_eq!(
        kinds(&new.id, &old.id),
        vec!["supersedes".to_string()],
        "`supersedes: ADR-0004` must resolve through the id key"
    );
}

/// Task 3 must not move authority. `inbound_link_counts` filters to wikilinks,
/// so the second edge between an already-linked pair contributes nothing.
#[test]
fn typed_edges_do_not_move_the_authority_count() {
    let (_d, idx, _calls) = setup();

    let mut a = Document::new(SourceType::Markdown, "file:///d/a.md", "A", "See [[b]].");
    a.links = vec!["b".into()];
    a.meta = serde_json::json!({"id": "ADR-0001", "superseded_by": "ADR-0002"});
    let b = {
        let mut b = Document::new(SourceType::Markdown, "file:///d/b.md", "B", "Body.");
        b.meta = serde_json::json!({"id": "ADR-0002"});
        b
    };

    idx.index_documents(&[a.clone(), b.clone()]).unwrap();
    idx.resolve_links(&[a.clone(), b.clone()]).unwrap();

    assert_eq!(
        idx.store()
            .inbound_link_counts()
            .unwrap()
            .get(&b.id)
            .copied(),
        Some(1),
        "B has two inbound edges but one wikilink; authority counts the wikilink"
    );
}

/// `linked_docs` answers for ONE kind, and the caller must say which.
///
/// It matched `[:LINKS_TO]` with no predicate. That was unambiguous while
/// `wikilink` was the only kind ever written, and became a silent question the
/// moment lifecycle relations shipped: eleven assertions in this file read "A
/// links to B" while the answer had quietly started including tombstone
/// pointers. The parameter is required rather than defaulted so the question
/// cannot be left unasked.
#[test]
fn linked_docs_answers_for_one_kind_and_the_caller_must_say_which() {
    let (_d, idx, _calls) = setup();
    let a = Document::new(SourceType::Markdown, "file:///a.md", "A", "body");
    let b = Document::new(SourceType::Markdown, "file:///b.md", "B", "body");
    let c = Document::new(SourceType::Markdown, "file:///c.md", "C", "body");
    idx.index_documents(&[a.clone(), b.clone(), c.clone()])
        .unwrap();

    let store = idx.store();
    store.link_documents(&a.id, &b.id, "wikilink").unwrap();
    store.link_documents(&a.id, &c.id, "superseded-by").unwrap();

    assert_eq!(
        store.linked_docs(&a.id, "wikilink").unwrap(),
        vec![b.id.clone()],
        "asking for wikilinks must not return the lifecycle pointer"
    );
    assert_eq!(
        store.linked_docs(&a.id, "superseded-by").unwrap(),
        vec![c.id.clone()],
        "and asking for the lifecycle pointer must not return the wikilink"
    );
    assert!(
        store.linked_docs(&a.id, "no-such-kind").unwrap().is_empty(),
        "an unknown kind is empty, not everything — failing open here would \
         make a typo look like a rich answer"
    );
}
