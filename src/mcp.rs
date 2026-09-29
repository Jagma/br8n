use crate::config::{Config, Profile};
use crate::embed::Embedder;
use crate::pack::Pack;
use crate::store::Hit;
use anyhow::Result;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{
    handler::server::tool::ToolRouter, model::*, schemars, tool, tool_handler, tool_router,
    ServerHandler, ServiceExt,
};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SearchArgs {
    /// What to look for, in natural language.
    pub query: String,
    /// Retrieval depth 0-4. Higher is slower and more thorough. Omit for the default.
    pub quality: Option<u8>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RelatedArgs {
    /// The `uri` field of a hit from a previous br8n_search call.
    pub uri: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RememberArgs {
    /// "lesson" (a rule the user taught you), "fact" (something durable about the user or their setup) or "episode" (what happened, with the decision and why).
    pub kind: String,
    /// One to three sentences, 10 to 2000 characters, in the user's own intent.
    pub text: String,
    /// Optional short title. Defaults to the first sentence.
    pub title: Option<String>,
    /// "global" for a rule that applies everywhere, or an absolute project path. Omit to scope to the current project.
    pub scope: Option<String>,
    /// 0-100. Use 100 only when the user said it explicitly; the default is 80.
    pub confidence: Option<u8>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ForgetArgs {
    /// The id shown in <br8n-lessons>, in a (memory: …) line, or by br8n_remember.
    pub id: String,
}

pub fn resolve_remember(
    args: RememberArgs,
    cwd: &std::path::Path,
) -> Result<crate::memory::Remember> {
    let kind = crate::memory::MemoryKind::parse(&args.kind).ok_or_else(|| {
        anyhow::anyhow!("kind must be lesson, fact or episode, got `{}`", args.kind)
    })?;
    let project = match args.scope.as_deref().map(str::trim) {
        Some("global") => None,
        Some(path) if !path.is_empty() => Some(std::path::PathBuf::from(path)),
        _ => Some(cwd.to_path_buf()),
    };
    Ok(crate::memory::Remember {
        kind,
        text: args.text,
        title: args.title,
        project,
        confidence: args.confidence.unwrap_or(80).min(100),
        origin: crate::memory::Origin::Claude,
        session: None,
        source_hash: None,
        source_stamp: None,
        created: None,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ManifestStamp {
    dev: u64,
    ino: u64,
    mtime: i64,
    mtime_nsec: i64,
    len: u64,
}

impl ManifestStamp {
    fn of(dir: &Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(dir.join(crate::pack::manifest::MANIFEST_FILE)).ok()?;
        Some(Self {
            dev: m.dev(),
            ino: m.ino(),
            mtime: m.mtime(),
            mtime_nsec: m.mtime_nsec(),
            len: m.len(),
        })
    }
}

struct OpenedPack {
    stamp: Option<ManifestStamp>,
    pack: Option<Arc<Pack>>,
}

impl OpenedPack {
    fn reuse_or_open(
        slot: &mut Option<OpenedPack>,
        dir: &Path,
        open: impl FnOnce() -> Result<Option<Pack>>,
    ) -> Result<Option<Arc<Pack>>> {
        let stamp = ManifestStamp::of(dir);
        if let Some(held) = slot.as_ref().filter(|held| held.stamp == stamp) {
            return Ok(held.pack.clone());
        }
        *slot = None;
        let pack = open()?.map(Arc::new);
        *slot = Some(OpenedPack {
            stamp,
            pack: pack.clone(),
        });
        Ok(pack)
    }
}

#[derive(Default)]
struct SearchCache {
    embedder: Option<Arc<dyn Embedder>>,
    main: Option<OpenedPack>,
    memory: Option<OpenedPack>,
}

#[derive(Clone)]
pub struct Br8nTools {
    config: Config,
    cache: Arc<Mutex<SearchCache>>,
    pack_opens: Arc<AtomicUsize>,
    tool_router: ToolRouter<Self>,
}

impl Br8nTools {
    pub fn clamp_quality(q: Option<u8>) -> u8 {
        q.map(|v| v.min(4)).unwrap_or(3)
    }

    pub fn tools() -> Vec<String> {
        Self::tool_router()
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect()
    }

    pub fn search(&self, query: &str, quality: Option<u8>) -> String {
        let profile = Profile::tier(Self::clamp_quality(quality));
        // `mcp.threshold` existed in the config, was documented, and was
        // read by nothing — an explicit search returned every candidate
        // however weak. It is deliberately lower than the hook's (0.55 vs
        // 0.70): Claude asked for this list on purpose and can judge a
        // marginal result, where the hook fires unasked and must stay quiet.
        let min = self.config.surface(crate::config::Surface::Mcp).threshold;
        // The MCP surface's weights go with the MCP surface's threshold:
        // weighting happens before the gate, so `[mcp.weights]` is how a
        // strong transcript is allowed to clear 0.55 at all. Under the
        // global 0.45 it would need a 1.22 cosine, which cannot happen.
        // `profile` above is `quality`, which the caller picks per
        // call and can differ from `[mcp] quality`'s configured default.
        // `retrieve_from_opened` decides store-vs-pack against the tier
        // actually about to run, not the surface's default — see the same
        // reasoning on the dashboard's tier override in `dashboard.rs`.
        self.retriever_for(&profile)
            .and_then(|r| r.search_gated(query, &profile, min))
            .map(|hits| {
                crate::usage::record_hits(&crate::config::Config::db_path(), &hits);
                Self::format_hits(&hits)
            })
            .unwrap_or_else(Self::format_search_failure)
    }

    pub fn pack_opens(&self) -> usize {
        self.pack_opens.load(Ordering::SeqCst)
    }

    fn retriever_for(&self, profile: &Profile) -> Result<crate::retrieve::Retriever> {
        let db = Config::db_path();
        let dims = self.config.embed.dimensions;
        let (embedder, pack, memory) = {
            let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            let embedder = match &cache.embedder {
                Some(held) => Arc::clone(held),
                None => {
                    let fresh: Arc<dyn Embedder> =
                        Arc::from(crate::embed::for_config(&self.config.embed)?);
                    cache.embedder = Some(Arc::clone(&fresh));
                    fresh
                }
            };
            let model_id = embedder.model_id();
            let pack = OpenedPack::reuse_or_open(&mut cache.main, &db, || {
                self.pack_opens.fetch_add(1, Ordering::SeqCst);
                crate::pack::open_pack_beside(&db, &model_id, dims)
            })?;
            let memory = OpenedPack::reuse_or_open(
                &mut cache.memory,
                &crate::memory::pack_dir(&crate::memory::default_root()),
                || crate::memory::open_pack(&self.config, &model_id),
            );
            (embedder, pack, memory)
        };
        crate::retrieve_from_opened(
            &self.config,
            crate::config::Surface::Mcp,
            profile,
            Box::new(embedder),
            pack,
            memory,
        )
    }

    pub fn format_search_failure(e: anyhow::Error) -> String {
        if let Some(refused) = e.downcast_ref::<crate::retrieve::PackRefused>() {
            return refused.to_string();
        }
        eprintln!("br8n: br8n_search retrieval unavailable — {e:#}");
        Self::format_hits(&[])
    }

    pub fn format_hits(hits: &[Hit]) -> String {
        if hits.is_empty() {
            return "No matching notes found in the knowledge base.".to_string();
        }
        hits.iter()
            .map(|h| {
                let page = h.page_no.map(|p| format!(" (p. {p})")).unwrap_or_default();
                let heading = if h.heading_path.is_empty() {
                    String::new()
                } else {
                    format!("\n{}", h.heading_path)
                };
                // `relevance`, never `score`. `score` is an RRF rank value
                // topping out near 0.049, so an excellent match was reported to
                // the model as `score: 0.033` — reading as near-worthless and
                // exporting the ordinal/cardinal confusion the rest of the
                // system was built to avoid. `relevance` is a [0,1] cosine that
                // means the same thing here as it does at the gate.
                format!(
                    "## {}{}{}\nsource: {}\nrelevance: {:.3}\n\n{}",
                    h.title, page, heading, h.uri, h.relevance, h.text
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n---\n\n")
    }
}

#[tool_router]
impl Br8nTools {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            cache: Arc::default(),
            pack_opens: Arc::default(),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        title = "Search the knowledge base",
        annotations(
            title = "Search the knowledge base",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        description = "Search the user's personal knowledge base (notes, papers, saved \
                       articles, past Claude Code sessions) by meaning. Use this when the \
                       user refers to something they have written down, read, or worked on \
                       before, rather than asking them to repeat it."
    )]
    async fn br8n_search(
        &self,
        Parameters(args): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        // The store and embedder both go through a blocking `reqwest` client;
        // calling that directly on a Tokio worker thread panics ("cannot drop
        // a runtime in a context where blocking is not allowed") because
        // `serve` already runs inside a multi-thread runtime. `spawn_blocking`
        // moves the work to Tokio's blocking pool, where nested blocking calls
        // are expected and safe. Confirmed by reproducing the panic against a
        // live empty index during self-review.
        let tools = self.clone();
        let text = tokio::task::spawn_blocking(move || tools.search(&args.query, args.quality))
            .await
            .unwrap_or_else(|e| {
                eprintln!("br8n: br8n_search worker panicked — {e:#}");
                Self::format_hits(&[])
            });
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    #[tool(
        title = "Find connected notes",
        annotations(
            title = "Find connected notes",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        description = "Given the uri of a note already found, return notes connected to it \
                       in the knowledge graph — wikilinks and adjacent sections. Use this to \
                       follow a thread after br8n_search finds a starting point."
    )]
    async fn br8n_related(
        &self,
        Parameters(args): Parameters<RelatedArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        // See br8n_search: blocking store/HTTP calls must run off the async
        // worker threads.
        let config = self.config.clone();
        let hits = tokio::task::spawn_blocking(move || {
            crate::related_for(&config, &args.uri).unwrap_or_default()
        })
        .await
        .unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(
            Self::format_hits(&hits),
        )]))
    }

    #[tool(
        title = "Re-index the knowledge base",
        annotations(
            title = "Re-index the knowledge base",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        description = "Re-index the knowledge base, picking up new and changed files."
    )]
    async fn br8n_index(&self) -> Result<CallToolResult, ErrorData> {
        // See br8n_search: blocking store/HTTP calls must run off the async
        // worker threads.
        let config = self.config.clone();
        let msg = tokio::task::spawn_blocking(move || match crate::index_now(&config) {
            Ok(s) => format!(
                "indexed: {} added, {} updated, {} chunks",
                s.added, s.updated, s.chunks
            ),
            Err(e) => format!("index failed: {e}"),
        })
        .await
        .unwrap_or_else(|e| format!("index failed: task panicked: {e}"));
        Ok(CallToolResult::success(vec![ContentBlock::text(msg)]))
    }

    #[tool(
        title = "Save a memory",
        annotations(
            title = "Save a memory",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        ),
        description = "Save a memory for future sessions: a lesson the user taught you (a correction or rule), \
                       a durable fact about the user or their setup, or an episode (what happened and what was \
                       decided). Lessons are shown to you at the start of every session in the matching project; \
                       everything is also retrieved by relevance on later prompts. Returns the memory's id."
    )]
    async fn br8n_remember(
        &self,
        Parameters(args): Parameters<RememberArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let config = self.config.clone();
        let msg = tokio::task::spawn_blocking(move || {
            let cwd = std::env::current_dir().unwrap_or_default();
            match resolve_remember(args, &cwd) {
                Ok(r) => {
                    let kind = r.kind;
                    match crate::memory::remember(&config, r) {
                        Ok(outcome) => outcome.describe(kind),
                        Err(e) => {
                            eprintln!("br8n: br8n_remember failed — {e:#}");
                            format!("Not saved: {e}")
                        }
                    }
                }
                Err(e) => format!("Not saved: {e}"),
            }
        })
        .await
        .unwrap_or_else(|e| format!("Not saved: worker panicked: {e}"));
        Ok(CallToolResult::success(vec![ContentBlock::text(msg)]))
    }

    #[tool(
        title = "Forget a memory",
        annotations(
            title = "Forget a memory",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        ),
        description = "Delete a memory by id, when the user says a lesson or fact no longer applies."
    )]
    async fn br8n_forget(
        &self,
        Parameters(args): Parameters<ForgetArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let config = self.config.clone();
        let msg =
            tokio::task::spawn_blocking(move || match crate::memory::forget(&config, &args.id) {
                Ok(m) => format!("Forgot {} {}: {}", m.facts.kind.as_str(), m.id, m.title),
                Err(e) => format!("Nothing forgotten: {e}"),
            })
            .await
            .unwrap_or_else(|e| format!("Nothing forgotten: worker panicked: {e}"));
        Ok(CallToolResult::success(vec![ContentBlock::text(msg)]))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Br8nTools {
    fn get_info(&self) -> ServerInfo {
        // ServerInfo (= InitializeResult) is #[non_exhaustive]: a struct literal
        // will not compile. Build with `new`, then set the optional fields.
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("br8n", env!("CARGO_PKG_VERSION"))
                    .with_website_url("https://github.com/Jagma/br8n"),
            );
        info.instructions = Some(
            "Semantic search over the user's personal knowledge base, plus durable memory: \
             br8n_remember saves lessons, facts and episodes; br8n_forget retracts one."
                .into(),
        );
        info
    }
}

pub fn serve(config: Config) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let service = Br8nTools::new(config)
                .serve(rmcp::transport::stdio())
                .await?;
            service.waiting().await?;
            Ok::<_, anyhow::Error>(())
        })
}
