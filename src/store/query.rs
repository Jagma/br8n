use super::{as_f32_vec, as_f64, as_i64, as_string, string_pairs, Store};
use anyhow::Result;
use lbug::{LogicalType, Value};

#[derive(Debug, Clone)]
pub struct Hit {
    pub chunk_id: String,
    pub doc_id: String,
    pub text: String,
    pub heading_path: String,
    pub uri: String,
    pub title: String,
    pub page_no: Option<i64>,
    /// Fused rank value. Drives ORDER. Not comparable across pipelines: RRF sums
    /// land in ~0.015-0.049, a reranked hit is exactly 1.0 or 0.0.
    pub score: f32,
    /// Semantic similarity in [0,1]. Drives the injection GATE.
    ///
    /// `score` cannot serve this purpose: RRF's maximum is about 0.049, so any
    /// human-meaningful threshold (0.55, 0.4) rejects every hit and the prompt
    /// hook silently injects nothing, forever, with no error. Vector search sets
    /// this from the cosine similarity it already computes; BM25 and graph
    /// expansion have no similarity of their own and set 0.0; fusion keeps the
    /// MAX across merged duplicates; the reranker overwrites it with its verdict.
    pub relevance: f32,
    /// Which loader produced the document: `markdown`, `pdf`, `web`,
    /// `transcript`. Retrieval treats these differently — see `source_weight`.
    pub source_type: String,
    /// How many documents link to this hit's document, carried from the pack
    /// row this hit was hydrated from (`pack.links`, joined by row ordinal).
    ///
    /// It is 0 on every hit the STORE produced, and deliberately so: the store
    /// path already holds the whole `inbound_link_counts` map, which also
    /// covers graph-expanded hits that have no pack row at all. `rank_weight`
    /// reads exactly one of the two sources, never a mixture — see
    /// `retrieve::Authority`.
    ///
    /// Not a ranking signal on its own. It is `authority_lift`'s numerator,
    /// and `authority_lift` returns exactly 1.0 whenever authority is off, so
    /// a populated `inbound` changes nothing until `[weights] authority` is.
    pub inbound: u32,
    /// This row's document lifecycle. Filled by `Pack::hydrate` from
    /// `pack.status` using the row ordinal, exactly as `inbound` is filled from
    /// `pack.links`. `Current` on any `Hit` that did not come through hydrate —
    /// including every write-path `Hit` — which is the no-op value.
    pub lifecycle: crate::pack::status::Lifecycle,
    pub last_used: Option<i64>,
    pub memory: Option<crate::memory::MemoryFacts>,
}

const HIT_RETURN: &str =
    "c.id, c.doc_id, c.text, c.heading_path, d.uri, d.title, c.page_no, d.source_type";

#[derive(Debug, Clone, serde::Serialize)]
pub struct GraphNode {
    pub id: String,
    pub title: String,
    pub source_type: String,
    pub chunks: u32,
    pub inbound: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<&'static str>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GraphEntity {
    pub id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    /// For `LINKS_TO` this is the edge's stored `kind` — `"wikilink"`,
    /// `"supersedes"`, `"superseded-by"`. For the other two edge tables it is
    /// the table itself: `"mentions"`, `"tagged"`.
    ///
    /// The dashboard does NOT colour by this — `GraphTab`'s `linkColor` is a
    /// constant. It filters by it: the node panel lists a document's
    /// neighbours by excluding `mentions` and `tagged`, so a new edge KIND
    /// appears there automatically while a new edge TABLE would have to be
    /// added to that exclusion. An earlier version of this comment claimed the
    /// colouring, which sent a reader looking for a palette that does not
    /// exist.
    pub kind: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GraphSnapshot {
    pub nodes: Vec<GraphNode>,
    pub entities: Vec<GraphEntity>,
    pub tags: Vec<String>,
    pub edges: Vec<GraphEdge>,
}

/// One document's facts for the dashboard's detail panels — everything
/// `/api/graph` deliberately leaves off because that payload is every
/// document in the vault and a detail panel shows exactly one.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DocumentDetail {
    pub id: String,
    pub uri: String,
    pub title: String,
    pub source_type: String,
    pub indexed_at: i64,
    pub chunks: u32,
    pub inbound: u32,
    /// The vault's own free-text `status:`, or `None`. Kept RAW and beside
    /// the derived rung: a panel that shows only `Investigating` cannot tell
    /// a note that said `investigating` from a `proposed` note promoted by an
    /// inbound link, and those are different facts about the document.
    pub status: Option<String>,
    pub lifecycle: crate::pack::status::Lifecycle,
}

/// What both `br8n status` and `/api/stats` report about an index.
///
/// `model` is `Option` rather than a placeholder string so each surface can
/// render absence its own way — the CLI prints "-", the dashboard sends null —
/// without either having to un-pick the other's stand-in.
#[derive(Debug, Clone)]
pub struct StatusSnapshot {
    pub documents: i64,
    pub chunks: i64,
    pub model: Option<String>,
    pub skipped: Vec<String>,
    /// Chunks with no stored embedding — the backlog `backfill_vectors`
    /// drains. A half-embedded index that presents as complete is worse than
    /// the slow synchronous index this exists to replace, so this is always
    /// computed, never left implicit in `chunks` alone.
    pub vectors_pending: i64,
}

/// One `Document`'s row, exactly as compaction must reproduce it.
///
/// Distinct from `crate::model::Document`: that struct drives normal
/// indexing and carries `source_type` as the `SourceType` enum and `meta` as
/// parsed JSON. A compacted document is copied verbatim from the live store
/// instead, so this carries both as the raw stored strings, and it carries
/// `indexed_at` — which `Store::upsert_document` stamps with the current
/// time — because compaction is not a new indexing event and must not
/// silently reset it.
#[derive(Debug, Clone)]
pub struct DocumentRow {
    pub id: String,
    pub uri: String,
    pub title: String,
    pub source_type: String,
    pub content_hash: String,
    pub indexed_at: i64,
    pub meta: String,
    pub tags: Vec<String>,
}

/// One `Chunk`'s row, exactly as compaction must reproduce it — including
/// the stored `embed_hash`, which a normal `insert_chunks` computes from
/// `embed_text`. That field is not stored anywhere as of schema version 3
/// (see `schema.rs`), so a rebuild from the store's own rows has no text to
/// recompute the hash from and must carry it across unchanged instead.
#[derive(Debug, Clone)]
pub struct ChunkRow {
    pub id: String,
    pub doc_id: String,
    pub ord: i64,
    pub text: String,
    pub embed_hash: String,
    pub heading_path: String,
    pub page_no: Option<i64>,
    pub embedding: Vec<f32>,
}

impl Store {
    /// Cosine similarity between `query` and each named chunk's stored embedding.
    ///
    /// BM25 and graph expansion find chunks by words and by edges, so their hits
    /// have no similarity of their own. They carried `relevance: 0.0`, which does
    /// not mean "dissimilar" — it means "never measured". The hook gates on
    /// relevance, so every keyword-only and neighbour-only hit was rejected
    /// outright no matter how good it was, and at the fast tier (no reranker)
    /// nothing could ever restore it. An exact-phrase match was structurally
    /// uninjectable at the one surface that fires automatically.
    ///
    /// Embeddings are stored normalized, so the dot product IS the cosine.
    pub fn cosine_for(
        &self,
        query: &[f32],
        chunk_ids: &[String],
    ) -> Result<std::collections::HashMap<String, f32>> {
        let mut out = std::collections::HashMap::new();
        if query.is_empty() || chunk_ids.is_empty() {
            return Ok(out);
        }
        let ids = Value::List(
            LogicalType::String,
            chunk_ids.iter().map(|i| Value::String(i.clone())).collect(),
        );
        let rows = match self.exec(
            "MATCH (c:Chunk) WHERE list_contains($ids, c.id) RETURN c.id, c.embedding",
            vec![("ids", ids)],
        ) {
            Ok(r) => r,
            Err(_) => return Ok(out),
        };
        for row in rows {
            let (Some(id), Some(emb)) = (
                row.first().and_then(as_string),
                row.get(1).and_then(as_f32_vec),
            ) else {
                continue;
            };
            if emb.len() != query.len() {
                continue;
            }
            let dot: f32 = emb.iter().zip(query).map(|(a, b)| a * b).sum();
            // (1 + s) / 2, NOT the raw cosine — the same scale the pack's
            // vector search (`vectors::Reader::search`) produces from
            // usearch's cosine distance (`1.0 - dist / 2.0`), and the scale
            // the 0.70 gate and every bench number are calibrated against.
            //
            // This returned the raw cosine, so the two writers of `relevance`
            // disagreed: at s = 0.4 vector search reported 0.70 and this
            // reported 0.40 for the same similarity. `relevance` drives BOTH
            // the injection gate and the final ordering, so a backfilled hit
            // was systematically under-ranked against a vector hit and cut by
            // a threshold it had actually cleared.
            //
            // It stayed hidden because it only bites when the measure stage is
            // busy, and the FTS index was stale enough that few BM25-only hits
            // ever reached it. Repairing that index made this the common path.
            // Clamped for drift in a stored vector that is not quite unit length.
            out.insert(id, ((1.0 + dot) / 2.0).clamp(0.0, 1.0));
        }
        Ok(out)
    }

    /// Inbound `LINKS_TO` count for every document that has at least one,
    /// counting WIKILINKS ONLY.
    ///
    /// Computed at index time and cached, because running it per query would
    /// scan the whole edge table on every search.
    ///
    /// The `kind` predicate is an ALLOWLIST, and it is load-bearing rather than
    /// cosmetic. `MERGE` in lbug 0.19.1 matches on a relationship PROPERTY —
    /// verified by `merge_on_a_relationship_property_decides_whether_two_kinds_
    /// are_one_edge` — so `link_documents(a, b, "wikilink")` and
    /// `link_documents(a, b, "superseded-by")` produce TWO edges between the
    /// same pair, not one. Without this predicate a document pair that is
    /// already wikilinked and then gains a typed relation contributes 2 to the
    /// target's inbound count while being no more linked than before.
    ///
    /// That is not a rounding error, it points the wrong way: a `superseded-by`
    /// edge is a tombstone, and counting it would hand the DEAD record an
    /// `authority_lift` — the same edge that marks a record superseded would
    /// make it rank higher. Measured on a real vault, the two records of a
    /// supersedes pair each go 2 -> 3 inbound, +2.4% lift at `authority = 0.3`, against a
    /// demotion of -5% at a multiplier of 0.95.
    ///
    /// Allowlist, not denylist (`kind <> 'superseded-by'`), so a future edge
    /// kind is excluded until someone decides it confers authority. Adding one
    /// is then a deliberate edit here, not a silent consequence of writing an
    /// edge somewhere else.
    ///
    /// This changes no number today, and that is an induction over the three
    /// call sites rather than a measurement of the live index: `index.rs`
    /// writes the literal `"wikilink"` and is the ONLY literal; compaction and
    /// `upsert_document`'s inbound-restore both propagate a `kind` they read
    /// back out of the store. So no edge can carry another kind unless some
    /// literal wrote one, and there is exactly one literal. Any existing index
    /// is therefore all-wikilink and the predicate excludes nothing. It takes
    /// effect the day a second literal ships.
    ///
    /// `pack.links` needs no separate fix — it is built FROM this map
    /// (`index.rs`), so both backends move together, which they must:
    /// `Authority::Store` and `Authority::Pack` are two paths to one number,
    /// and a divergence between them would be invisible (the pack path is
    /// taken at tiers 0-1, the store path at 2-4, so one query would weight a
    /// document differently from the next).
    pub fn inbound_link_counts(&self) -> Result<std::collections::HashMap<String, u32>> {
        self.count_map(
            "MATCH (:Document)-[r:LINKS_TO]->(d:Document) \
             WHERE r.kind = 'wikilink' RETURN d.id, count(*)",
        )
    }

    /// `doc_id -> Lifecycle` for every document that is not `Current`, per the
    /// ladder in `pack::status`'s module docs — where a document with NO
    /// `status:` key lands on `Proposed` (or `Investigating`, with an inbound
    /// wikilink) exactly like `proposed`/`draft`/`shaping`/anything
    /// unrecognised, because `Lifecycle::from_status(None, _)` already says
    /// so.
    ///
    /// `Current` documents are ABSENT from the map, not present as `Current`
    /// — the caller's `unwrap_or_default()` supplies exactly that value, so
    /// keeping them out only shrinks the map (7 rows on the reference vault,
    /// not 1,089). A missing `status:` key must NOT be folded into that same
    /// `continue`, or a status-less document — the overwhelming majority of a
    /// real vault — silently reads back as `Current` instead of `Proposed`.
    ///
    /// `inbound` is supplied by the caller rather than read here. Both
    /// `index.rs` call sites already compute `inbound_link_counts()` for
    /// `pack.links` and would otherwise call it again for this map — two
    /// reads of the same table that could in principle disagree, with
    /// nothing to say which one the published pack's authority weighting and
    /// lifecycle ladder each saw. Passing the map in makes both derive from
    /// the same single read.
    pub fn all_lifecycles(
        &self,
        inbound: &std::collections::HashMap<String, u32>,
    ) -> Result<std::collections::HashMap<String, crate::pack::status::Lifecycle>> {
        use crate::pack::status::Lifecycle;
        let mut out = std::collections::HashMap::new();
        for row in self.exec("MATCH (d:Document) RETURN d.id, d.meta", vec![])? {
            let (Some(id), Some(meta)) = (
                row.first().and_then(as_string),
                row.get(1).and_then(as_string),
            ) else {
                continue;
            };
            // `meta` is the literal four bytes `null` for most documents, which
            // is not valid JSON to index into — `status` is `None` for those,
            // same as a document whose `meta` parses but carries no `status`
            // key. Either way, `from_status` maps the absence onto the ladder
            // itself rather than this loop deciding it by skipping the row.
            let status = serde_json::from_str::<serde_json::Value>(&meta)
                .ok()
                .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(String::from));
            let lc = Lifecycle::from_status(
                status.as_deref(),
                inbound.get(&id).copied().unwrap_or(0) > 0,
            );
            if lc != Lifecycle::Current {
                out.insert(id, lc);
            }
        }
        Ok(out)
    }

    pub fn all_documents_meta(&self) -> Result<Vec<(String, String, String)>> {
        let mut out = Vec::new();
        for row in self.exec("MATCH (d:Document) RETURN d.uri, d.title, d.meta", vec![])? {
            let (Some(uri), title, Some(meta)) = (
                row.first().and_then(as_string),
                row.get(1).and_then(as_string).unwrap_or_default(),
                row.get(2).and_then(as_string),
            ) else {
                continue;
            };
            out.push((uri, title, meta));
        }
        Ok(out)
    }

    /// One document's facts for the dashboard's detail panels.
    ///
    /// `Ok(None)` is a document id that is not in the store — a client holding
    /// a graph payload from before a re-index, which is ordinary, not an
    /// outage. It must not be an `Err`: `json_result` turns every `Err` into a
    /// 503 and the SPA retries a 503, so returning one here would make a
    /// deleted document retry forever.
    pub fn document_detail(&self, id: &str) -> Result<Option<DocumentDetail>> {
        let rows = self.exec(
            "MATCH (d:Document) WHERE d.id = $id \
             RETURN d.uri, d.title, d.source_type, d.indexed_at, d.meta",
            vec![("id", Value::String(id.to_string()))],
        )?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        let (Some(uri), Some(title), Some(source_type)) = (
            row.first().and_then(as_string),
            row.get(1).and_then(as_string),
            row.get(2).and_then(as_string),
        ) else {
            return Ok(None);
        };
        let indexed_at = row.get(3).and_then(as_i64).unwrap_or(0);
        let meta = row.get(4).and_then(as_string).unwrap_or_default();
        let status = serde_json::from_str::<serde_json::Value>(&meta)
            .ok()
            .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(String::from));

        // Inbound count for THIS document only — `inbound_link_counts` walks
        // every LINKS_TO edge in the vault, which is the graph tab's job, not
        // a one-document panel's.
        let inbound = self
            .exec(
                "MATCH (:Document)-[r:LINKS_TO]->(d:Document) \
                 WHERE d.id = $id AND r.kind = 'wikilink' RETURN count(*)",
                vec![("id", Value::String(id.to_string()))],
            )?
            .next()
            .and_then(|r| r.first().and_then(as_i64))
            .unwrap_or(0);
        let chunks = self
            .exec(
                "MATCH (d:Document)-[:HAS_CHUNK]->(:Chunk) WHERE d.id = $id RETURN count(*)",
                vec![("id", Value::String(id.to_string()))],
            )?
            .next()
            .and_then(|r| r.first().and_then(as_i64))
            .unwrap_or(0);

        Ok(Some(DocumentDetail {
            id: id.to_string(),
            uri,
            title,
            source_type,
            indexed_at,
            chunks: chunks.max(0) as u32,
            inbound: inbound.max(0) as u32,
            lifecycle: crate::pack::status::Lifecycle::from_status(status.as_deref(), inbound > 0),
            status,
        }))
    }

    pub fn counts_sessions_by_agent(&self) -> Result<std::collections::BTreeMap<String, u32>> {
        use crate::loaders::transcript::SessionAgent;
        let mut out: std::collections::BTreeMap<String, u32> = SessionAgent::ALL
            .iter()
            .map(|a| (a.id().to_string(), 0))
            .collect();
        let rows = self.exec(
            "MATCH (d:Document) WHERE d.source_type = 'transcript' RETURN d.uri",
            vec![],
        )?;
        for uri in rows.filter_map(|r| r.first().and_then(as_string)) {
            if let Some(agent) = SessionAgent::from_uri(&uri) {
                *out.entry(agent.id().to_string()).or_default() += 1;
            }
        }
        Ok(out)
    }

    /// Documents per source type, one aggregate query.
    pub fn counts_by_source(&self) -> Result<std::collections::HashMap<String, u32>> {
        self.count_map("MATCH (d:Document) RETURN d.source_type, count(*)")
    }

    /// What an index reports about itself: how much is in it, what embedded it,
    /// and what it refused.
    ///
    /// `br8n status` and the dashboard's `/api/stats` answer the same four
    /// questions, and each used to read them independently — including its own
    /// copy of the `skipped` JSON decode. They are one read now, so a fifth
    /// field cannot reach one surface and miss the other.
    ///
    /// Every field degrades on its own: a missing counter reads 0 and an
    /// unstamped model reads `None`, because a half-answered status is more
    /// useful than none. Deciding what absence LOOKS like stays with the
    /// caller — the CLI prints "-", the dashboard sends null.
    pub fn status_snapshot(&self) -> StatusSnapshot {
        StatusSnapshot {
            documents: self.count_documents().unwrap_or(0),
            chunks: self.count_chunks().unwrap_or(0),
            model: self.get_meta("embed_model").ok().flatten(),
            skipped: self
                .get_meta("skipped")
                .ok()
                .flatten()
                .and_then(|j| serde_json::from_str::<Vec<String>>(&j).ok())
                .unwrap_or_default(),
            vectors_pending: self.count_chunks_without_vectors().unwrap_or(0),
        }
    }

    /// Run a `RETURN key, count(*)` aggregate and decode it into a map.
    ///
    /// An unavailable index answers "nothing counted" rather than failing: both
    /// callers are display paths that must still render against a database
    /// mid-swap. Written once so that choice cannot be made two different ways.
    fn count_map(&self, cypher: &str) -> Result<std::collections::HashMap<String, u32>> {
        let mut out = std::collections::HashMap::new();
        let rows = match self.exec(cypher, vec![]) {
            Ok(r) => r,
            Err(_) => return Ok(out),
        };
        for row in rows {
            if let (Some(key), Some(n)) =
                (row.first().and_then(as_string), row.get(1).and_then(as_i64))
            {
                out.insert(key, n.max(0) as u32);
            }
        }
        Ok(out)
    }

    /// The whole knowledge graph at document granularity, for the dashboard.
    ///
    /// Chunk-level structure (HAS_CHUNK, NEXT_CHUNK) is deliberately absent:
    /// 19k chunk nodes would drown a canvas that exists to show ~400
    /// documents. Chunk detail arrives through search, not the global graph.
    /// MENTIONS edges are lifted from chunk to document and deduplicated here,
    /// in Rust, because the pair (document, entity) is what the graph draws.
    pub fn graph_snapshot(&self) -> Result<GraphSnapshot> {
        let mut nodes = Vec::new();
        let chunk_counts: std::collections::HashMap<String, u32> = {
            let mut m = std::collections::HashMap::new();
            if let Ok(rows) = self.exec(
                "MATCH (d:Document)-[:HAS_CHUNK]->(:Chunk) RETURN d.id, count(*)",
                vec![],
            ) {
                for row in rows {
                    if let (Some(id), Some(n)) =
                        (row.first().and_then(as_string), row.get(1).and_then(as_i64))
                    {
                        m.insert(id, n.max(0) as u32);
                    }
                }
            }
            m
        };
        let inbound = self.inbound_link_counts()?;
        if let Ok(rows) = self.exec(
            "MATCH (d:Document) RETURN d.id, d.title, d.source_type, d.uri",
            vec![],
        ) {
            for row in rows {
                let (Some(id), Some(title), Some(st)) = (
                    row.first().and_then(as_string),
                    row.get(1).and_then(as_string),
                    row.get(2).and_then(as_string),
                ) else {
                    continue;
                };
                let agent = row
                    .get(3)
                    .and_then(as_string)
                    .and_then(|uri| crate::loaders::transcript::SessionAgent::from_uri(&uri))
                    .map(|a| a.id());
                nodes.push(GraphNode {
                    chunks: chunk_counts.get(&id).copied().unwrap_or(0),
                    inbound: inbound.get(&id).copied().unwrap_or(0),
                    id,
                    title,
                    source_type: st,
                    agent,
                });
            }
        }

        let mut edges = Vec::new();
        // `r.kind`, not a literal. Every LINKS_TO edge used to be reported as
        // "links_to" — the TABLE name — which was harmless while `"wikilink"`
        // was the only kind ever written and became a lie the moment lifecycle
        // relations shipped. The dashboard is the only inspection surface for
        // the graph, so a mislabelled edge there makes typed edges
        // unverifiable by eye.
        if let Ok(rows) = self.exec(
            "MATCH (a:Document)-[r:LINKS_TO]->(b:Document) RETURN a.id, b.id, r.kind",
            vec![],
        ) {
            for row in rows {
                if let (Some(f), Some(t), Some(k)) = (
                    row.first().and_then(as_string),
                    row.get(1).and_then(as_string),
                    row.get(2).and_then(as_string),
                ) {
                    edges.push(GraphEdge {
                        from: f,
                        to: t,
                        kind: k,
                    });
                }
            }
        }
        let mut seen_mentions = std::collections::HashSet::new();
        if let Ok(rows) = self.exec(
            "MATCH (d:Document)-[:HAS_CHUNK]->(:Chunk)-[:MENTIONS]->(e:Entity) RETURN d.id, e.id",
            vec![],
        ) {
            for row in rows {
                if let (Some(f), Some(t)) = (
                    row.first().and_then(as_string),
                    row.get(1).and_then(as_string),
                ) {
                    if seen_mentions.insert((f.clone(), t.clone())) {
                        edges.push(GraphEdge {
                            from: f,
                            to: t,
                            kind: "mentions".into(),
                        });
                    }
                }
            }
        }
        if let Ok(rows) = self.exec(
            "MATCH (d:Document)-[:TAGGED]->(t:Tag) RETURN d.id, t.name",
            vec![],
        ) {
            for row in rows {
                if let (Some(f), Some(t)) = (
                    row.first().and_then(as_string),
                    row.get(1).and_then(as_string),
                ) {
                    edges.push(GraphEdge {
                        from: f,
                        to: t,
                        kind: "tagged".into(),
                    });
                }
            }
        }

        let mut entities = Vec::new();
        if let Ok(rows) = self.exec("MATCH (e:Entity) RETURN e.id, e.name, e.kind", vec![]) {
            for row in rows {
                if let (Some(id), Some(name), Some(kind)) = (
                    row.first().and_then(as_string),
                    row.get(1).and_then(as_string),
                    row.get(2).and_then(as_string),
                ) {
                    entities.push(GraphEntity { id, name, kind });
                }
            }
        }
        let mut tags = Vec::new();
        if let Ok(rows) = self.exec("MATCH (t:Tag) RETURN t.name", vec![]) {
            for row in rows {
                if let Some(n) = row.first().and_then(as_string) {
                    tags.push(n);
                }
            }
        }
        Ok(GraphSnapshot {
            nodes,
            entities,
            tags,
            edges,
        })
    }

    /// Existing embeddings for a document, keyed by a hash of the exact text
    /// that was embedded.
    ///
    /// Re-indexing a changed document threw away every chunk it already had
    /// and re-embedded all of them. For an append-only source that is almost
    /// entirely wasted work: a session transcript grows by a few messages and
    /// costs a full re-embed of thousands of chunks. Measured, one such
    /// document took 405 seconds on a run where nothing else had changed.
    ///
    /// Keyed by a hash of `embed_text` rather than by chunk id, because ids
    /// are positional (`doc:ord`) and inserting content in the middle of a
    /// document shifts every id after it while the text itself is untouched.
    /// The store no longer keeps the text to key on directly — it keeps only
    /// `embed_hash` — so a caller must hash its own candidate text with
    /// `Document::content_hash` before looking it up here.
    pub fn embeddings_by_hash(
        &self,
        doc_id: &str,
    ) -> Result<std::collections::HashMap<String, Vec<f32>>> {
        let mut out = std::collections::HashMap::new();
        let rows = match self.exec(
            "MATCH (c:Chunk {doc_id: $d}) RETURN c.embed_hash, c.embedding",
            vec![("d", Value::String(doc_id.to_string()))],
        ) {
            Ok(r) => r,
            Err(_) => return Ok(out),
        };
        for row in rows {
            if let (Some(h), Some(v)) = (
                row.first().and_then(as_string),
                row.get(1).and_then(as_f32_vec),
            ) {
                out.insert(h, v);
            }
        }
        Ok(out)
    }

    /// Every chunk id of a document with the hash of the text that was embedded.
    ///
    /// This is what makes re-indexing write only what changed. The store keeps
    /// only this hash, not the text it was derived from — storing the text a
    /// second time (`embed_text`, alongside `text`) to answer "has this
    /// changed" doubled the corpus's dominant field for one yes/no question a
    /// 64-byte hash answers just as well.
    pub fn chunk_hashes(&self, doc_id: &str) -> Result<std::collections::HashMap<String, String>> {
        let mut out = std::collections::HashMap::new();
        let rows = self.exec(
            "MATCH (c:Chunk {doc_id: $d}) RETURN c.id, c.embed_hash",
            vec![("d", Value::String(doc_id.to_string()))],
        )?;
        for row in rows {
            let (Some(id), Some(h)) = (
                row.first().and_then(as_string),
                row.get(1).and_then(as_string),
            ) else {
                continue;
            };
            out.insert(id, h);
        }
        Ok(out)
    }

    /// BM25 moved to the pack (`src/pack/`) as of schema version 3 — this
    /// store no longer builds lbug's FTS index (see `schema.rs`'s comment on
    /// the retired `FTS_INDEX`/`FTS_DROP` constants), so there is no
    /// `chunk_fts` index left to query.
    ///
    /// This errors rather than returning `Ok(Vec::new())` deliberately: an
    /// empty keyword result is indistinguishable from an honest no-match,
    /// which is this project's house failure mode. A caller reaching this —
    /// the store-fallback path taken when no pack is available — must
    /// degrade to vector-only LOUDLY, not silently drop BM25's contribution.
    /// See `retrieve::Retriever::run`'s BM25 stage, which catches this error
    /// and reports it on stderr instead of failing the whole query.
    pub fn fts_search(&self, _query: &str, _k: usize) -> Result<Vec<Hit>> {
        anyhow::bail!(
            "keyword search moved to the retrieval pack and this store no longer builds an \
             FTS index; open the pack instead of falling back to the store, or run \
             `br8n index` to publish one"
        )
    }

    pub fn hydrate(&self, chunk_ids: &[String]) -> Result<Vec<Hit>> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        let cypher = format!(
            "MATCH (d:Document)-[:HAS_CHUNK]->(c:Chunk) WHERE c.id IN $ids \
             RETURN {HIT_RETURN}, 0.0"
        );
        self.rows_to_hits(
            &cypher,
            vec![("ids", string_list(chunk_ids))],
            |raw| raw,
            |_| 0.0,
        )
    }

    /// Graph expansion from seed chunks. Neighbours arrive with score 0.0 and are
    /// ranked by fusion alongside the direct hits.
    pub fn expand(&self, seeds: &[String], hops: u8, max_neighbors: usize) -> Result<Vec<Hit>> {
        if seeds.is_empty() || max_neighbors == 0 {
            return Ok(Vec::new());
        }
        // `hops` and `max_neighbors` come from Profile, not user input; Cypher takes
        // no parameter for a path-length bound or a LIMIT.
        let depth = hops.max(1);

        // Adjacent chunks, either direction — a fact split across a boundary can
        // live on either side of the seed. ORDER BY c.doc_id, c.ord keeps the result
        // reproducible and numerically ordered; chunk_id is "{doc_id}:{ord}", so
        // ordering by the whole id string would sort the ordinal lexicographically
        // ("…:10" before "…:8"). Expanded hits all score 0.0, so without an explicit
        // order fusion's tie-break would see arbitrary storage order. `c.ord` must
        // also be projected in RETURN DISTINCT — otherwise `c` is out of scope for
        // ORDER BY and lbug 0.19.1 raises "Binder exception: Variable c is not in
        // scope"; it's appended after the score literal so rows_to_hits' fixed
        // column indices (0..=7) are unaffected.
        let adjacent = format!(
            "MATCH (s:Chunk)-[:NEXT_CHUNK*1..{depth}]-(c:Chunk) \
             WHERE s.id IN $seeds AND NOT c.id IN $seeds \
             MATCH (d:Document)-[:HAS_CHUNK]->(c) \
             RETURN DISTINCT {HIT_RETURN}, 0.0, c.ord ORDER BY c.doc_id, c.ord LIMIT {max_neighbors}"
        );

        // First chunk of each linked document — the note you would have clicked to.
        //
        // An ALLOWLIST, and the two kinds in it are not symmetric:
        //
        //   wikilink       a link a human wrote. Always follow.
        //   superseded-by  sits on the RETIRED record and points at its
        //                  replacement. Follow it: landing on a dead decision
        //                  and being shown the live one is the whole point.
        //   supersedes     sits on the LIVE record and points back at what it
        //                  replaced. Do NOT follow. `expand` has a
        //                  `LIMIT max_neighbors`, and this pointer spends one
        //                  of those slots on a document the vault retired.
        //
        // The first cut of this filter admitted only `wikilink`, which read as
        // symmetric and is not: it also dropped `superseded-by`, removing a
        // traversal that existed before any of this work and is useful. An ADR
        // may declare `superseded-by:` in frontmatter without also writing the
        // prose link — the loader treats frontmatter relations as first-class
        // and reports unresolvable ones on stderr — and for those, dropping the
        // edge makes the replacement UNREACHABLE from the record it replaces.
        // `LifecycleSource` cannot compensate: it demotes what is in the pool,
        // and an unreachable document is absent rather than demoted.
        //
        // Allowlist rather than `<> 'supersedes'` so a future kind is excluded
        // until someone decides it is worth a neighbour slot — the same posture
        // `inbound_link_counts` takes.
        //
        // The comprehension form is what expresses a SET; the inline property
        // map (`{{kind: '...'}}`) holds one value only. Both were run against
        // real lbug 0.19.1 in a spike rather than taken from
        // documentation, which the spec had
        // recorded as unverified.
        let linked = format!(
            "MATCH (s:Chunk)<-[:HAS_CHUNK]-(:Document)-[:LINKS_TO*1..{depth} \
              (r, _ | WHERE r.kind IN ['wikilink', 'superseded-by'])]->(d:Document) \
             WHERE s.id IN $seeds \
             MATCH (d)-[:HAS_CHUNK]->(c:Chunk) WHERE c.ord = 0 AND NOT c.id IN $seeds \
             RETURN DISTINCT {HIT_RETURN}, 0.0, c.ord ORDER BY c.doc_id, c.ord LIMIT {max_neighbors}"
        );

        let adjacent_hits = self.rows_to_hits(
            &adjacent,
            vec![("seeds", string_list(seeds))],
            |s| s,
            |_| 0.0,
        )?;
        let linked_hits =
            self.rows_to_hits(&linked, vec![("seeds", string_list(seeds))], |s| s, |_| 0.0)?;

        // Interleave rather than concatenate-then-truncate. Each query has its own
        // LIMIT, so appending meant adjacency could fill the budget by itself and
        // silently drop every LINKS_TO neighbour — and a linked note ("the one you
        // would have clicked to") is usually the more interesting result, while an
        // adjacent chunk is a continuation of something already matched.
        let mut seen = std::collections::HashSet::new();
        let mut out: Vec<Hit> = Vec::with_capacity(max_neighbors);
        let mut a = adjacent_hits.into_iter();
        let mut l = linked_hits.into_iter();
        loop {
            let (na, nl) = (a.next(), l.next());
            if na.is_none() && nl.is_none() {
                break;
            }
            for hit in [na, nl].into_iter().flatten() {
                if out.len() < max_neighbors && seen.insert(hit.chunk_id.clone()) {
                    out.push(hit);
                }
            }
            if out.len() >= max_neighbors {
                break;
            }
        }
        Ok(out)
    }

    /// `score` maps the raw row value to the fused-ordering score; `relevance`
    /// maps it to a [0,1] similarity for the injection gate. They differ because
    /// BM25 has no bounded similarity to offer.
    pub(crate) fn rows_to_hits(
        &self,
        cypher: &str,
        params: Vec<(&str, Value)>,
        score: impl Fn(f32) -> f32,
        relevance: impl Fn(f32) -> f32,
    ) -> Result<Vec<Hit>> {
        let r = match self.exec(cypher, params) {
            Ok(r) => r,
            // A missing index means "nothing indexed yet", which is empty, not broken.
            Err(_) => return Ok(Vec::new()),
        };
        Ok(r.filter_map(|row| {
            let page = row.get(6).and_then(as_i64).unwrap_or(-1);
            // HIT_RETURN projects 8 columns (0..=7); the caller's score is the
            // 9th. Adding a column to HIT_RETURN must move this index too — the
            // score silently reads 0.0 otherwise, which ranks every hit equally.
            let raw = row.get(8).and_then(as_f64).unwrap_or(0.0) as f32;
            Some(Hit {
                chunk_id: as_string(row.first()?)?,
                doc_id: as_string(row.get(1)?)?,
                text: as_string(row.get(2)?)?,
                heading_path: row.get(3).and_then(as_string).unwrap_or_default(),
                uri: as_string(row.get(4)?)?,
                title: row.get(5).and_then(as_string).unwrap_or_default(),
                page_no: if page < 0 { None } else { Some(page) },
                score: score(raw),
                relevance: relevance(raw),
                source_type: row.get(7).and_then(as_string).unwrap_or_default(),
                // The store path weights by its own `inbound_link_counts`
                // map, which covers graph-expanded hits too. See `Hit.inbound`.
                inbound: 0,
                // The pack is what carries per-row lifecycle today — see
                // `Hit.lifecycle`. A store-path hit has no row to join
                // against, so it stays `Current`, the no-op value.
                lifecycle: Default::default(),
                last_used: None,
                memory: None,
            })
        })
        .collect())
    }

    /// Every chunk, joined to its document, ordered so two builds of the same
    /// data produce the same row ordinals.
    ///
    /// Ordering by `c.id` (not by insertion) is what makes a pack reproducible:
    /// the ordinal is the join key between the vector index and the records, and
    /// a nondeterministic order would silently reshuffle it between builds.
    ///
    /// A chunk with no stored embedding is INCLUDED here, with an empty
    /// (`Vec::new()`) vector — not skipped. It must still appear in
    /// `pack.rec`/`pack.fts`: phase 1 of an asynchronous index (`br8n index
    /// --no-embed`) publishes exactly this, and BM25 must be able to find
    /// every chunk immediately even though none of them has a vector yet.
    /// `Pack::build` is what decides, from the empty vectors this produces,
    /// whether the resulting pack has NO vector index at all (every vector
    /// empty — phase 1) or a real one (every vector present) — a chunk with
    /// no vector must still end up ABSENT from vector search, never present
    /// at zero: `relevance` gates injection, and a fabricated zero is a claim
    /// the pipeline cannot distinguish from a measured one. `Pack::build`
    /// refuses a MIX of the two rather than guessing which chunks belong in
    /// `pack.vec` — see its doc comment.
    pub fn all_rows_for_pack(&self) -> Result<Vec<(crate::pack::records::Record, Vec<f32>)>> {
        let r = self.exec(
            "MATCH (d:Document)-[:HAS_CHUNK]->(c:Chunk) \
             RETURN c.id, c.doc_id, c.text, c.heading_path, d.uri, d.title, \
                    c.page_no, d.source_type, c.embedding, d.meta \
             ORDER BY c.id",
            vec![],
        )?;
        let mut out = Vec::new();
        for row in r {
            let emb = row.get(8).and_then(as_f32_vec).unwrap_or_default();
            let page = row.get(6).and_then(as_i64).unwrap_or(-1);
            let (Some(chunk_id), Some(doc_id), Some(text), Some(uri)) = (
                row.first().and_then(as_string),
                row.get(1).and_then(as_string),
                row.get(2).and_then(as_string),
                row.get(4).and_then(as_string),
            ) else {
                // Unlike a missing embedding (deliberately skipped above), this
                // is a row the schema should never produce: `chunk_id`, `doc_id`,
                // `text`, and `uri` are all non-null columns. If it happens
                // anyway, the pack is an immutable published artifact with no
                // audit trail — silently dropping the row would vanish a
                // document from search with no trace anywhere. Fail loudly
                // instead, naming the offending row.
                anyhow::bail!(
                    "pack build: chunk row missing a required field (chunk_id={:?}, \
                     doc_id={:?}, text present={}, uri={:?}); this indicates a \
                     schema or query bug, not a normal skip",
                    row.first().and_then(as_string),
                    row.get(1).and_then(as_string),
                    row.get(2).and_then(as_string).is_some(),
                    row.get(4).and_then(as_string),
                );
            };
            let source_type = row.get(7).and_then(as_string).unwrap_or_default();
            let memory = if source_type == "memory" {
                row.get(9)
                    .and_then(as_string)
                    .and_then(|m| serde_json::from_str::<serde_json::Value>(&m).ok())
                    .and_then(|v| {
                        serde_json::from_value::<crate::memory::MemoryFacts>(v["memory"].clone())
                            .ok()
                    })
            } else {
                None
            };
            out.push((
                crate::pack::records::Record {
                    chunk_id,
                    doc_id,
                    text,
                    heading_path: row.get(3).and_then(as_string).unwrap_or_default(),
                    uri,
                    title: row.get(5).and_then(as_string).unwrap_or_default(),
                    page_no: if page < 0 { None } else { Some(page) },
                    source_type,
                    // Never read on the write path: `Pack::build` takes the
                    // counts as its own argument (`inbound_link_counts`) and
                    // writes them to `pack.links`, which is their single
                    // source. See `records::Record::inbound`.
                    inbound: 0,
                    // Same story: `Pack::build` takes lifecycles as its own
                    // argument (`all_lifecycles`) and writes them to
                    // `pack.status`, keyed by `doc_id`, not from this field.
                    lifecycle: Default::default(),
                    last_used: None,
                    memory,
                },
                emb,
            ));
        }
        Ok(out)
    }

    /// The backlog phase 2 must drain: chunks published with no embedding at
    /// all, up to `limit` of them, oldest-by-id first. Either `--no-embed`
    /// left them (phase 1 of an asynchronous index) or a previous
    /// `--backfill` was killed or ran out of budget before finishing them.
    ///
    /// This is a fresh query against the live store on every call — never
    /// cached, never derived from anything an in-memory struct remembers —
    /// which is what makes `backfill_vectors` resumable: a killed process
    /// leaves no state to lose, because none of its progress ever lived
    /// anywhere but the rows it had already written back.
    ///
    /// Returns `(chunk_id, embed_text)`. `embed_text` is RECONSTRUCTED from
    /// stored fields (`d.title`, `c.heading_path`, `c.text`) via
    /// `Chunk::plain_embed_text` — the exact formula `Chunker::chunk` uses —
    /// because schema version 3 does not store `embed_text` itself, only its
    /// hash (see `schema.rs`). See `Chunk::plain_embed_text`'s doc comment
    /// for the one case (contextual enrichment) where this is not exact.
    ///
    /// `limit` is not user input — same reasoning as `Store::expand`'s
    /// `max_neighbors` — so it is interpolated into the query rather than
    /// bound as a parameter; lbug's Cypher takes no parameter for `LIMIT`.
    pub fn chunks_without_vectors(&self, limit: usize) -> Result<Vec<(String, String)>> {
        let cypher = format!(
            "MATCH (d:Document)-[:HAS_CHUNK]->(c:Chunk) WHERE c.embedding IS NULL \
             RETURN c.id, d.title, c.heading_path, c.text ORDER BY c.id LIMIT {limit}"
        );
        let rows = self.exec(&cypher, vec![])?;
        let mut out = Vec::new();
        for row in rows {
            let (Some(id), Some(title), Some(text)) = (
                row.first().and_then(as_string),
                row.get(1).and_then(as_string),
                row.get(3).and_then(as_string),
            ) else {
                continue;
            };
            let heading_path = row.get(2).and_then(as_string).unwrap_or_default();
            out.push((
                id,
                crate::model::Chunk::plain_embed_text(&title, &heading_path, &text),
            ));
        }
        Ok(out)
    }

    /// Count of the same backlog `chunks_without_vectors` drains, for `br8n
    /// status` — same predicate, no `LIMIT`, so status reports the real
    /// total rather than the size of one backfill batch.
    pub fn count_chunks_without_vectors(&self) -> Result<i64> {
        let mut r = self.exec(
            "MATCH (:Document)-[:HAS_CHUNK]->(c:Chunk) WHERE c.embedding IS NULL RETURN count(c)",
            vec![],
        )?;
        Ok(r.next()
            .and_then(|row| row.first().and_then(as_i64))
            .unwrap_or(0))
    }

    /// Every document, verbatim, for compaction's rebuild-from-store path.
    /// Ordered by `d.id` for the same reproducibility reason
    /// `all_rows_for_pack` orders by `c.id` — not load-bearing here (nothing
    /// downstream binary-searches this), but it makes two compactions of an
    /// unchanged store byte-for-byte comparable.
    pub fn all_document_rows(&self) -> Result<Vec<DocumentRow>> {
        let mut tags_by_doc: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        let tag_rows = self.exec(
            "MATCH (d:Document)-[:TAGGED]->(t:Tag) RETURN d.id, t.name",
            vec![],
        )?;
        for (doc_id, tag) in string_pairs(tag_rows) {
            tags_by_doc.entry(doc_id).or_default().push(tag);
        }

        let r = self.exec(
            "MATCH (d:Document) RETURN d.id, d.uri, d.title, d.source_type, \
             d.content_hash, d.indexed_at, d.meta ORDER BY d.id",
            vec![],
        )?;
        let mut out = Vec::new();
        for row in r {
            let (
                Some(id),
                Some(uri),
                Some(title),
                Some(source_type),
                Some(content_hash),
                Some(meta),
            ) = (
                row.first().and_then(as_string),
                row.get(1).and_then(as_string),
                row.get(2).and_then(as_string),
                row.get(3).and_then(as_string),
                row.get(4).and_then(as_string),
                row.get(6).and_then(as_string),
            )
            else {
                // `id`, `uri`, `title`, `source_type`, `content_hash` and `meta`
                // are all non-null columns in `schema::ddl`. A document row
                // failing to decode one is a schema or query bug, not a normal
                // skip — compaction must not silently drop a document, which
                // is exactly the risk this task exists to avoid.
                anyhow::bail!(
                    "compaction: document row missing a required field (id={:?})",
                    row.first().and_then(as_string),
                );
            };
            let indexed_at = row.get(5).and_then(as_i64).unwrap_or(0);
            let tags = tags_by_doc.get(&id).cloned().unwrap_or_default();
            out.push(DocumentRow {
                id,
                uri,
                title,
                source_type,
                content_hash,
                indexed_at,
                meta,
                tags,
            });
        }
        Ok(out)
    }

    /// Every chunk, verbatim (including its stored `embed_hash` and
    /// `embedding`), for compaction's rebuild-from-store path. Ordered by
    /// `c.id`, matching `all_rows_for_pack`.
    pub fn all_chunk_rows(&self) -> Result<Vec<ChunkRow>> {
        let r = self.exec(
            "MATCH (c:Chunk) RETURN c.id, c.doc_id, c.ord, c.text, c.embed_hash, \
             c.heading_path, c.page_no, c.embedding ORDER BY c.id",
            vec![],
        )?;
        let mut out = Vec::new();
        for row in r {
            let (Some(id), Some(doc_id), Some(text), Some(embed_hash), Some(embedding)) = (
                row.first().and_then(as_string),
                row.get(1).and_then(as_string),
                row.get(3).and_then(as_string),
                row.get(4).and_then(as_string),
                row.get(7).and_then(as_f32_vec),
            ) else {
                // Unlike `all_rows_for_pack`, which treats a missing embedding
                // as an expected skip (a chunk that must stay absent from
                // vector search), a fixed-width `embedding FLOAT[dims]` column
                // failing to decode at all — for THIS chunk id, doc_id, text or
                // embed_hash — indicates corruption compaction must not paper
                // over by silently dropping the row.
                anyhow::bail!(
                    "compaction: chunk row missing a required field (id={:?})",
                    row.first().and_then(as_string),
                );
            };
            let ord = row.get(2).and_then(as_i64).unwrap_or(0);
            let heading_path = row.get(5).and_then(as_string).unwrap_or_default();
            let page = row.get(6).and_then(as_i64).unwrap_or(-1);
            out.push(ChunkRow {
                id,
                doc_id,
                ord,
                text,
                embed_hash,
                heading_path,
                page_no: if page < 0 { None } else { Some(page) },
                embedding,
            });
        }
        Ok(out)
    }

    /// `(from_chunk_id, to_chunk_id)` for every `NEXT_CHUNK` edge.
    pub fn all_next_chunk_edges(&self) -> Result<Vec<(String, String)>> {
        Ok(string_pairs(self.exec(
            "MATCH (a:Chunk)-[:NEXT_CHUNK]->(b:Chunk) RETURN a.id, b.id",
            vec![],
        )?))
    }

    /// `(from_doc_id, to_doc_id, kind)` for every `LINKS_TO` edge.
    pub fn all_links_to_edges(&self) -> Result<Vec<(String, String, String)>> {
        let r = self.exec(
            "MATCH (a:Document)-[r:LINKS_TO]->(b:Document) RETURN a.id, b.id, r.kind",
            vec![],
        )?;
        Ok(r.filter_map(|row| {
            Some((
                as_string(row.first()?)?,
                as_string(row.get(1)?)?,
                as_string(row.get(2)?)?,
            ))
        })
        .collect())
    }

    /// `(chunk_id, entity_name, entity_kind)` for every `MENTIONS` edge.
    pub fn all_mentions_edges(&self) -> Result<Vec<(String, String, String)>> {
        let r = self.exec(
            "MATCH (c:Chunk)-[:MENTIONS]->(e:Entity) RETURN c.id, e.name, e.kind",
            vec![],
        )?;
        Ok(r.filter_map(|row| {
            Some((
                as_string(row.first()?)?,
                as_string(row.get(1)?)?,
                as_string(row.get(2)?)?,
            ))
        })
        .collect())
    }

    /// `(doc_id, domain)` for every `DERIVED_FROM` edge.
    pub fn all_derived_from_edges(&self) -> Result<Vec<(String, String)>> {
        Ok(string_pairs(self.exec(
            "MATCH (d:Document)-[:DERIVED_FROM]->(s:Source) RETURN d.id, s.domain",
            vec![],
        )?))
    }

    /// Every `IndexMeta` key/value pair. Compaction carries `embed_model` and
    /// `schema_version` (and anything future) into the shadow verbatim —
    /// compaction never touches the embedder or the schema, so there is
    /// nothing to re-derive them from.
    pub fn all_meta(&self) -> Result<Vec<(String, String)>> {
        Ok(string_pairs(self.exec(
            "MATCH (m:IndexMeta) RETURN m.key, m.value",
            vec![],
        )?))
    }

    /// A rough estimate of what the live rows should occupy on disk: the sum
    /// of every chunk's text, heading path, embed hash and embedding, plus
    /// every document's own string fields.
    ///
    /// This is not a byte-for-byte prediction of `graph.kz`'s size — that also
    /// carries lbug's own storage overhead, any indexes, and (before a
    /// compaction) the dead rows a rebuild is about to reclaim. It exists so
    /// `br8n index --compact` can report how much of the file was actually
    /// content versus overhead, not to predict the compacted file's exact size.
    pub fn live_row_bytes(&self) -> Result<u64> {
        let mut total: u64 = 0;
        for row in self.exec(
            "MATCH (c:Chunk) RETURN c.text, c.heading_path, c.embed_hash, c.embedding",
            vec![],
        )? {
            total += row.first().and_then(as_string).map_or(0, |s| s.len()) as u64;
            total += row.get(1).and_then(as_string).map_or(0, |s| s.len()) as u64;
            total += row.get(2).and_then(as_string).map_or(0, |s| s.len()) as u64;
            total += row.get(3).and_then(as_f32_vec).map_or(0, |v| v.len() * 4) as u64;
        }
        for row in self.exec(
            "MATCH (d:Document) RETURN d.uri, d.title, d.content_hash, d.meta",
            vec![],
        )? {
            for i in 0..4 {
                total += row.get(i).and_then(as_string).map_or(0, |s| s.len()) as u64;
            }
        }
        Ok(total)
    }
}

pub(crate) fn string_list(v: &[String]) -> Value {
    // `Value::List` carries an explicit `LogicalType` discriminant for its
    // element type, verified against real lbug 0.19.1 (`Value::List(LogicalType, Vec<Value>)`),
    // the same pattern `float_array`'s `Value::Array` already relies on.
    Value::List(
        LogicalType::String,
        v.iter().map(|s| Value::String(s.clone())).collect(),
    )
}
