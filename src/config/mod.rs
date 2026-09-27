pub mod check;
pub mod edit;
pub mod schema;
pub mod view;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Hook,
    Mcp,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GraphExpansion {
    pub hops: u8,
    pub max_neighbors: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rerank {
    pub model: String,
    pub top_n: usize,
}

/// One detent on the speed slider. Adding a tier is data, not code.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub name: &'static str,
    pub candidates_k: usize,
    /// HNSW search effort, passed to `usearch`'s `change_expansion_search`
    /// by the pack's own vector search (`src/pack/vectors.rs`).
    ///
    /// Left unset, lbug's `QUERY_VECTOR_INDEX` (since deleted) defaulted to
    /// 200. Measured on the live index (27k chunks): the call's cost was
    /// FLAT in `k` — 57ms at k=1, 58ms at k=100 — so this value, not the
    /// number of results, is what a vector query actually pays for. Top-20
    /// agreement against efs=800:
    ///
    /// ```text
    /// efs=10  81.4%    efs=50   97.1%    efs=200  99.3%
    /// efs=32  93.6%    efs=100  98.6%
    /// ```
    ///
    /// Must be >= `candidates_k`; below that the index cannot return the
    /// neighbours the tier asked for.
    pub efs: usize,
    pub bm25: bool,
    pub graph: Option<GraphExpansion>,
    /// LLM reranking. `None` in every shipped profile.
    ///
    /// Ollama has no rerank endpoint (`/api/rerank` returns 404 as of 0.32), so
    /// a true cross-encoder cannot be served, and the generative substitutes
    /// were measured as noise rather than signal: qwen3:0.6b answers "yes" to
    /// every query/passage pair, including a sourdough note against "how do we
    /// handle code review", while qwen3:4b rejected five of six chunks that
    /// scored 0.69-0.73 cosine. One promotes everything, the other rejects
    /// almost everything; neither ranks.
    ///
    /// Ordering by the cosine instead took thorough from 0.80 to 1.00 recall
    /// and 706ms to 137ms. The field stays so a real cross-encoder can be
    /// wired in when one can be served.
    pub rerank: Option<Rerank>,
    pub mmr_lambda: f32,
    /// Hard deadline. Stages check elapsed time and return early rather than overrun.
    /// Wall-clock ceiling for the whole search, embedding included.
    ///
    /// Every budget here is at least ~2x the measured warm floor of ~65ms
    /// (40ms fixed + 24ms embed). They used to sit much closer: tier 0 allowed
    /// 90ms against that 65ms floor, and the stage-gating tests recorded
    /// themselves failing under parallel load because of it. A ceiling that
    /// trips on a healthy machine is not a deadline, it is a source of silent
    /// degradation — the tier quietly stops doing what it advertises, and the
    /// only symptom is worse results.
    ///
    /// These are ceilings, not targets. A warm query at tier 1 still returns in
    /// roughly 65-90ms; the budget only binds when something is genuinely slow.
    pub budget_ms: u32,
}

impl Profile {
    pub fn tier(t: u8) -> Profile {
        // `qwen3-reranker:0.6b` does not exist in Ollama's registry — `ollama pull`
        // returns "file does not exist". The reranker degrades to input order on
        // failure by design, so tiers 3 and 4 silently did no reranking at all
        // while still paying the latency of trying.
        //
        // The scoring prompt is a yes/no relevance question, which any instruct
        // model answers. Measured on the relevant/irrelevant pair:
        //   qwen3:0.6b   -> "Yes" / "No."   (discriminates)
        //   llama3.2:1b  -> "No"  / "No."   (says no to everything)
        match t.min(4) {
            0 => Profile {
                name: "instant",
                candidates_k: 5,
                efs: 32,
                // Was `false`, on the reasoning that the latency tier should
                // pay for one signal only. That reasoning predates BM25 moving
                // onto the retrieval pack, and the measurement that followed
                // reads the other way: serving BM25 from the pack was worth
                // +0.05/+0.04/+0.03/+0.03 recall@5 at tiers 1-4, while tier 0
                // — the one tier where it stayed switched off — sat 0.10 below
                // the store (0.42 against 0.52) with no way to close it, since
                // BM25 cannot compensate where it is not run.
                //
                // Turning it on makes tier 0 run bm25 + fusion + measure + mmr
                // instead of stopping after `vector` (see `wants_more` in
                // `retrieve::run`). At `candidates_k: 5` those stages work over
                // a handful of hits, so the expected cost is small against this
                // tier's 150ms budget — but `budget_ms` is a BETWEEN-STAGE
                // checkpoint, not a cap on elapsed time, so total elapsed can
                // land past 150ms without anything degrading.
                //
                // UNVERIFIED as of this edit: no bench has been run against it.
                // Revert to `false` if recall@5 at tier 0 does not improve, or
                // if tier 0 latency regresses materially.
                bm25: true,
                graph: None,
                rerank: None,
                mmr_lambda: 1.0,
                budget_ms: 150,
            },
            1 => Profile {
                name: "fast",
                candidates_k: 20,
                efs: 50,
                bm25: true,
                graph: None,
                rerank: None,
                mmr_lambda: 0.7,
                budget_ms: 220,
            },
            2 => Profile {
                name: "balanced",
                candidates_k: 30,
                efs: 64,
                bm25: true,
                graph: Some(GraphExpansion {
                    hops: 1,
                    max_neighbors: 5,
                }),
                rerank: None,
                mmr_lambda: 0.7,
                budget_ms: 320,
            },
            3 => Profile {
                name: "thorough",
                candidates_k: 40,
                efs: 100,
                bm25: true,
                graph: Some(GraphExpansion {
                    hops: 1,
                    max_neighbors: 8,
                }),
                rerank: None,
                mmr_lambda: 0.6,
                budget_ms: 700,
            },
            _ => Profile {
                name: "exhaustive",
                candidates_k: 80,
                efs: 200,
                bm25: true,
                graph: Some(GraphExpansion {
                    hops: 2,
                    max_neighbors: 12,
                }),
                rerank: None,
                mmr_lambda: 0.5,
                budget_ms: 1600,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurfaceConfig {
    /// `None` means "not configured" — resolved per-surface by `Config::quality_for`.
    ///
    /// This MUST NOT be a bare `u8`. `#[serde(default)]` on a `u8` yields 0
    /// (`instant`), and the `default_hook`/`default_mcp` functions only apply when
    /// the WHOLE table is absent. So a partial table — the most natural way to
    /// write an override:
    ///     [hook]
    ///     threshold = 0.65
    /// would silently drop retrieval to tier 0 with no indication why.
    #[serde(default)]
    pub quality: Option<u8>,
    /// Minimum `relevance` (cosine, [0,1]) for a chunk to be injected — never
    /// `score`, which is an RRF rank value. Calibrated by `br8n bench`.
    #[serde(default = "default_threshold")]
    pub threshold: f32,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: usize,
    /// This surface's overrides of the global `[weights]` table, written as
    /// `[hook.weights]` / `[mcp.weights]`. Resolved by `Config::weights_for`;
    /// an empty one means "whatever the global table says".
    #[serde(default)]
    pub weights: WeightOverrides,
}

fn expand_tilde(p: &std::path::Path) -> PathBuf {
    if p == std::path::Path::new("~") {
        return directories::BaseDirs::new()
            .map(|b| b.home_dir().to_path_buf())
            .unwrap_or_else(|| p.to_path_buf());
    }
    match p.strip_prefix("~") {
        Ok(rest) => directories::BaseDirs::new()
            .map(|b| b.home_dir().join(rest))
            .unwrap_or_else(|| p.to_path_buf()),
        Err(_) => p.to_path_buf(),
    }
}

fn default_true() -> bool {
    true
}

/// Measured, not guessed. Modern embedding models have a high cosine baseline —
/// unrelated text does not score near zero. Against a three-note corpus with
/// `qwen3-embedding:0.6b`:
///
/// ```text
/// query                                 correct match   irrelevant floor
/// "why did the pooler drop sessions"        0.864            0.606
/// "how do I bake bread"                     0.727            0.581
/// "explain borrow checker rules"            0.789            0.592
/// "what is the capital of France"           (none)           0.600
/// ```
///
/// At 0.55 every hit cleared the gate, so the hook injected two unrelated notes
/// on a question about France. That was four hand-checked queries, and 0.70 was
/// chosen on them.
///
/// 0.70 was then MEASURED and found expensive. `br8n bench`'s trade-off curve,
/// over 222 golden cases at 650 documents, prices it at 31 of 172 answer cases
/// cut — 18% — taking gated recall@5 from 0.87 to 0.71 at the hook's tier. The
/// negative side that argued for a high gate did not survive inspection: with
/// the queries' own authoring transcripts excluded, the entire non-leaked
/// negative class is ONE document (0.708), and that one clears
/// 0.70 anyway. Lowering to 0.66 therefore admits no false positive that 0.70
/// was already admitting, and recovers 27 of the 31 lost cases.
///
/// The gate is COUPLED to `[weights] transcript`, which is why this number
/// cannot be tuned alone. Both decide how readily a transcript is injected, and
/// lowering the gate has the same direction as raising that weight. Measured on
/// the live index, injected chunks per query (notes / transcripts): 0.70 gives
/// 2.52 / 0.22, 0.66 gives 2.72 / 0.85, 0.64 gives 2.73 / 1.32. So lowering
/// mostly buys TRANSCRIPTS, roughly three per extra note. 0.66 is chosen
/// because 0.85 per query still sits well under the ~2.1 at which transcripts
/// crowd out notes, while coverage improves where it matters: of 60 positive
/// queries, 8 injected nothing at all at 0.70 and 3 do at 0.66.
///
/// Model-dependent. `br8n bench` calibrates it against the user's own corpus.
fn default_threshold() -> f32 {
    0.66
}
fn default_max_tokens() -> usize {
    1500
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedConfig {
    #[serde(default = "default_embed_model")]
    pub model: String,
    /// No upper bound is enforced here. A value above the model's native
    /// width (1024 for the default `qwen3-embedding:0.6b`) is not caught at
    /// config-parse time — the embedder rejects it at call time instead, with
    /// an actionable error naming the model's actual dimensionality (see
    /// `OllamaEmbedder::call_with`'s `floats.len() >= self.dims` check).
    #[serde(default = "default_dimensions")]
    pub dimensions: usize,
    #[serde(default = "default_ollama")]
    pub ollama_url: String,
    #[serde(default = "default_keep_alive")]
    pub keep_alive: KeepAlive,
    /// How many documents to chunk and embed at once.
    ///
    /// Embedding is a network round-trip to Ollama and was strictly sequential,
    /// so the indexer spent nearly all of its time waiting. Writes stay on one
    /// thread — the store connection is not shareable — but the waiting does
    /// not have to. Beyond about 4 the limit becomes Ollama's own parallelism
    /// (`OLLAMA_NUM_PARALLEL`), not ours.
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    /// Chunks per embedding request. Fewer, larger requests mean less
    /// per-request overhead, bounded by Ollama's batch size (`-ub`).
    #[serde(default = "default_batch")]
    pub batch: usize,
    /// Target chunk size, in approximate tokens (4 chars each — see `Chunker`).
    ///
    /// This is the single biggest lever on indexing time, because it decides
    /// how many embedding round-trips the corpus needs: doubling it roughly
    /// halves them. It trades against retrieval precision — a bigger chunk
    /// matches more queries but pins the answer down less exactly — and
    /// changing it invalidates the index.
    #[serde(default = "default_chunk_tokens")]
    pub chunk_tokens: usize,
    /// Override the document/query prefix scheme picked from the model name.
    /// One of `qwen3`, `nomic`, `e5`, `plain`. Changing it invalidates the index.
    #[serde(default)]
    pub prefix_scheme: Option<String>,
    /// Opt-in LLM context blurb per chunk (Task 12).
    #[serde(default)]
    pub contextual: bool,
    #[serde(default = "default_enrich_model")]
    pub enrich_model: String,
    #[serde(skip)]
    pub remote: Option<crate::env_file::RemoteEmbed>,
    #[serde(skip)]
    pub remote_error: Option<String>,
}

fn default_embed_model() -> String {
    "qwen3-embedding:0.6b".into()
}
fn default_chunk_tokens() -> usize {
    512
}
fn default_concurrency() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    concurrency_for_cores(cores)
}

fn concurrency_for_cores(cores: usize) -> usize {
    (cores / 2).clamp(1, 4)
}
fn default_batch() -> usize {
    32
}
fn default_dimensions() -> usize {
    512
}
fn default_ollama() -> String {
    "http://localhost:11434".into()
}
fn default_keep_alive() -> KeepAlive {
    KeepAlive::from("30m")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum KeepAlive {
    Seconds(i64),
    Duration(String),
}

impl From<&str> for KeepAlive {
    fn from(duration: &str) -> Self {
        KeepAlive::Duration(duration.to_string())
    }
}

impl std::fmt::Display for KeepAlive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeepAlive::Seconds(seconds) => write!(f, "{seconds}"),
            KeepAlive::Duration(duration) => f.write_str(duration),
        }
    }
}
fn default_enrich_model() -> String {
    "qwen3:4b".into()
}
fn default_ignore() -> Vec<String> {
    vec!["_templates".to_string()]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S3Config {
    pub bucket: String,
    pub region: String,
    #[serde(default = "default_s3_prefix")]
    pub prefix: String,
    /// A profile in `~/.aws/credentials`, not an environment variable.
    #[serde(default = "default_aws_profile")]
    pub profile: String,
    #[serde(default = "default_storage_class")]
    pub storage_class: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriveConfig {
    #[serde(default)]
    pub folder_id: String,
    pub client_secret_file: PathBuf,
    /// `None` resolves lazily to `Config::drive_token_path()`.
    #[serde(default)]
    pub token_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default = "default_true")]
    pub include_index: bool,
    #[serde(default = "default_true")]
    pub encrypt: bool,
    #[serde(default)]
    pub key_file: Option<PathBuf>,
    #[serde(default = "default_keep_generations")]
    pub keep_generations: usize,
    #[serde(default = "default_keep_index")]
    pub keep_index: usize,
    #[serde(default)]
    pub s3: Option<S3Config>,
    #[serde(default)]
    pub drive: Option<DriveConfig>,
}

fn default_s3_prefix() -> String {
    "br8n/".into()
}
fn default_aws_profile() -> String {
    "default".into()
}
fn default_storage_class() -> String {
    "STANDARD_IA".into()
}
fn default_keep_generations() -> usize {
    30
}
fn default_keep_index() -> usize {
    2
}

impl Default for BackupConfig {
    fn default() -> Self {
        BackupConfig {
            enabled: false,
            targets: Vec::new(),
            include_index: true,
            encrypt: true,
            key_file: None,
            keep_generations: default_keep_generations(),
            keep_index: default_keep_index(),
            s3: None,
            drive: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Index past Claude Code sessions from `~/.claude/projects`.
    ///
    /// On by default because session history is one of the four sources this
    /// tool exists to search. But it is not listed in `sources`, so without a
    /// flag the user has no way to decline it — and it is easily the largest
    /// source. Measured on one developer machine: 277 session files, 72 MB.
    #[serde(default = "default_true")]
    pub index_transcripts: bool,
    #[serde(default)]
    pub index_transcripts_max_age_days: Option<u32>,
    #[serde(default = "default_true")]
    pub index_codex_sessions: bool,
    /// Path components never indexed, matched exactly against every component.
    ///
    /// Defaults to Obsidian's template folder. Those files are placeholder
    /// skeletons whose text retrieves — measured at 0.802 on the live index
    /// against a 0.66 hook gate — and injecting an empty ADR template into a
    /// prompt is worse than injecting nothing.
    ///
    /// A LIST rather than a hardcoded skip, for two reasons: a user who keeps
    /// real notes under that name can empty it, and the exclusion appears in
    /// the config file where it can be found. Only `_templates` is defaulted,
    /// not `templates`/`Templates` — a broader default would silently drop
    /// content in vaults that use those names for notes, and widening it is one
    /// edit.
    #[serde(default = "default_ignore")]
    pub ignore: Vec<String>,
    #[serde(default = "default_hook")]
    pub hook: SurfaceConfig,
    #[serde(default = "default_mcp")]
    pub mcp: SurfaceConfig,
    #[serde(default = "default_embed")]
    pub embed: EmbedConfig,
    #[serde(default)]
    pub sources: Vec<PathBuf>,
    #[serde(default = "default_weights")]
    pub weights: Weights,
    #[serde(default)]
    pub pdf: PdfConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
    #[serde(default)]
    pub update: UpdateConfig,
    #[serde(default)]
    pub backup: BackupConfig,
}

/// When a scanned page may be recognized rather than skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OcrMode {
    /// Recognize only pages the detector already flags as unreadable. This is
    /// the default: clean pages are never re-recognized, so a text PDF costs
    /// nothing — measured at 10ms with the renderer never loaded.
    #[default]
    Auto,
    /// Never recognize. Restores the pre-OCR behaviour exactly.
    Off,
    /// Recognize every page, including pages that already have good text.
    /// Slow and rarely what you want; kept for diagnosing a bad text layer.
    Force,
}

/// Whether a missing OCR model may be fetched over the network.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelDownloads {
    /// Fetch the pinned model set on first use. Today's behaviour, and the
    /// default, because the alternative makes a fresh install fail on its
    /// first scanned PDF.
    #[default]
    IfMissing,
    /// Never touch the network. A missing model becomes a loud failure and
    /// OCR degrades to text-layer extraction, which is what a machine with no
    /// model cache should do on a tool that promises to stay local.
    Offline,
}

/// Scanned-PDF recognition.
///
/// OCR needs two native libraries that are NOT bundled and, on macOS, are not
/// discoverable by default — `/opt/homebrew/lib` is not on the dyld search
/// path, so both a PDFium and an ONNX Runtime path must be resolved before the
/// first call. `pdfium_lib_path` and `ort_dylib_path` are the explicit escape
/// hatch when probing the usual prefixes fails.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PdfConfig {
    #[serde(default)]
    pub ocr: OcrMode,
    /// Recognition spans below this confidence are dropped.
    ///
    /// The crate defaults to 0.0, which keeps everything. That is the wrong
    /// default for a knowledge base: the character soup a scanner produces on
    /// noise would be embedded and would then compete with real text for the
    /// hook's attention. A floor costs a little recall on faint scans and buys
    /// chunks that are worth retrieving.
    #[serde(default = "default_ocr_min_confidence")]
    pub ocr_min_confidence: f32,
    /// Rasterization resolution for pages routed to OCR.
    ///
    /// 150 is the crate default and it sits just under a cost cliff, which is
    /// the real reason this is exposed. `detect_boxes` abandons its 960px
    /// standard detection pass once a page's longest side passes 1920px and
    /// runs the 2560px escalated detector instead. A4 crosses at 164 DPI and
    /// US Letter at 174, so raising this is not a smooth trade.
    ///
    /// Lowering it buys less than it looks: detection resizes every page to a
    /// 960px longest side before inference, and recognition runs one
    /// fixed-height crop per text line, so neither scales with DPI between
    /// roughly 96 and 164. Only rasterization does.
    #[serde(default = "default_pdf_dpi")]
    pub dpi: f32,
    /// Directory holding a pre-downloaded model set, for offline installs.
    #[serde(default)]
    pub model_dir: Option<PathBuf>,
    /// Whether a missing model may be downloaded. See `ModelDownloads`.
    #[serde(default)]
    pub model_downloads: ModelDownloads,
    #[serde(default)]
    pub pdfium_lib_path: Option<PathBuf>,
    #[serde(default)]
    pub ort_dylib_path: Option<PathBuf>,
}

fn default_ocr_min_confidence() -> f32 {
    0.5
}

fn default_pdf_dpi() -> f32 {
    150.0
}

impl Default for PdfConfig {
    fn default() -> Self {
        PdfConfig {
            ocr: OcrMode::default(),
            ocr_min_confidence: default_ocr_min_confidence(),
            dpi: default_pdf_dpi(),
            model_dir: None,
            model_downloads: ModelDownloads::default(),
            pdfium_lib_path: None,
            ort_dylib_path: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_lessons_max_tokens")]
    pub lessons_max_tokens: usize,
    #[serde(default = "default_min_confidence")]
    pub min_confidence: u8,
    #[serde(default = "default_duplicate_similarity")]
    pub duplicate_similarity: f32,
    #[serde(default = "default_episode_half_life_days")]
    pub episode_half_life_days: f32,
    #[serde(default = "default_episode_decay_floor")]
    pub episode_decay_floor: f32,
    #[serde(default = "default_true")]
    pub distill_episodes: bool,
    #[serde(default = "default_distill_after_hours")]
    pub distill_after_hours: f32,
    #[serde(default = "default_distill_idle_secs")]
    pub distill_idle_secs: u64,
    #[serde(default = "default_distill_model")]
    pub distill_model: String,
    #[serde(default = "default_max_memories")]
    pub max_memories: usize,
}

fn default_lessons_max_tokens() -> usize {
    600
}
fn default_min_confidence() -> u8 {
    80
}
fn default_duplicate_similarity() -> f32 {
    0.94
}
fn default_episode_half_life_days() -> f32 {
    30.0
}
fn default_episode_decay_floor() -> f32 {
    0.85
}
fn default_distill_after_hours() -> f32 {
    3.0
}
fn default_distill_idle_secs() -> u64 {
    60
}
fn default_distill_model() -> String {
    "qwen3:4b".into()
}
fn default_max_memories() -> usize {
    10_000
}

impl Default for MemoryConfig {
    fn default() -> Self {
        MemoryConfig {
            enabled: true,
            lessons_max_tokens: default_lessons_max_tokens(),
            min_confidence: default_min_confidence(),
            duplicate_similarity: default_duplicate_similarity(),
            episode_half_life_days: default_episode_half_life_days(),
            episode_decay_floor: default_episode_decay_floor(),
            distill_episodes: true,
            distill_after_hours: default_distill_after_hours(),
            distill_idle_secs: default_distill_idle_secs(),
            distill_model: default_distill_model(),
            max_memories: default_max_memories(),
        }
    }
}

impl MemoryConfig {
    pub fn episode_decay(&self, age_days: f32) -> f32 {
        let floor = self.episode_decay_floor.clamp(0.0, 1.0);
        if self.episode_half_life_days <= 0.0 || age_days <= 0.0 {
            return 1.0;
        }
        floor + (1.0 - floor) * 0.5f32.powf(age_days / self.episode_half_life_days)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateConfig {
    #[serde(default = "default_true")]
    pub check: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        UpdateConfig { check: true }
    }
}

/// How much each kind of source counts when ORDERING results.
///
/// Not all relevance is equal authority. A session transcript that discusses a
/// note is genuinely similar to a query about that note — and is still the
/// wrong answer, because the note is the source of truth and the transcript is
/// a conversation about it. Measured on a 35-case golden set: adding 351
/// transcripts to a 38-note vault took recall from 0.94 to 0.71, and the hits
/// displacing the notes were sessions where those very notes were discussed.
///
/// This multiplies `relevance` ITSELF, which is the gate's input as well as the
/// sort key — see the long note in `retrieve::Retriever::run` for why the two
/// must not disagree. A weight therefore decides both who goes first AND who
/// qualifies at all, which is why the number a surface uses is a per-surface
/// decision: `Config::weights_for`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Weights {
    #[serde(default = "one")]
    pub markdown: f32,
    #[serde(default = "one")]
    pub pdf: f32,
    #[serde(default = "one")]
    pub web: f32,
    /// Below 1.0 by default: see the note above on authority.
    #[serde(default = "default_transcript_weight")]
    pub transcript: f32,
    #[serde(default = "one")]
    pub memory: f32,
    /// How much inbound-link count can lift a document. OFF by default.
    ///
    /// Opt-in, because link structure is only a signal where linking is an
    /// actual practice. Measured on one vault: 46% of notes had no inbound
    /// wikilink at all, and transcripts have none by construction — so applied
    /// blindly this scores most of the corpus on a property it cannot have.
    ///
    /// It is a BOUNDED LIFT, never a penalty: an unlinked document keeps a
    /// multiplier of exactly 1.0, and the most-linked one reaches
    /// `1.0 + authority`. Being unlinked is not the same as being unimportant,
    /// and a formulation that drove unlinked notes toward zero would bury the
    /// leaves of the graph, which are usually where the answers are.
    #[serde(default)]
    pub authority: f32,
    /// Lifecycle multipliers. THREE ship at 1.0; `superseded` ships at 0.88 and
    /// is ON by default.
    ///
    /// This sentence used to say all four were 1.0. That stopped being true
    /// long ago and the correction was six lines further down, where a
    /// reader who had already believed the first line would not look. In a
    /// codebase where comments are the primary spec, a false leading sentence
    /// is worse than no comment.
    ///
    /// Three of the four are 1.0 for a reason that is not obvious: most notes
    /// in a vault carry no `status:` at all and therefore land on `Proposed`.
    /// Anything below 1.0 there demotes the ENTIRE corpus relative to a handful
    /// of decision records, moving every recall figure to fix one document.
    /// `superseded` is the only real dial, and it ships at 0.88 — ON by default.
    ///
    /// THE DESIGN'S WINDOW IS CLOSED. The design measured `0.90 < m < 0.99` against
    /// the ungated, pre-MMR ordering and said in the same paragraph that it
    /// must be re-measured before a number shipped. It was, on the live index
    /// on 2026-09-05 with `pack.status` published for the first time, and the
    /// two constraints no longer overlap:
    ///
    ///   demote ADR-0004 below the 0.66 hook gate   0.7420 -> needs m < 0.890
    ///   keep it top-5 for a history question
    ///     about the retired record                 0.7811 vs 5th at 0.7032
    ///                                              -> needs m > 0.900
    ///
    /// The second bound was measured ONCE and is not trustworthy: that case
    /// returns ADR-0004 in only 8 of 12 identical runs AT m = 1.0, before any
    /// demotion. Its score is deterministic (0.7811 every time it appears) but
    /// its PRESENCE is not, and no degradation is reported on stderr. It is a
    /// pre-existing instability, not something this multiplier causes.
    ///
    /// Re-derived from the stable cases only:
    ///
    ///   probe 1, 12/12 identical    0.7420 -> needs m < 0.8895
    ///   golden "we used to run two
    ///     build tools", 11/12       0.7777 vs 5th at 0.6840
    ///                               -> needs m > 0.8795
    ///
    /// 0.880 < m < 0.889, and 0.88 sits inside it. Verified by running the
    /// binary rather than by arithmetic: at 0.88 the superseded record reads
    /// 0.6530 on 8 of 8 runs (below the gate, deterministic) and the golden
    /// case keeps it at distinct rank 3-4 on 8 of 8. The defect is fixed and
    /// that case is NOT lost.
    ///
    /// The window is nine thousandths wide. Do not nudge this number.
    ///
    /// Set it to 1.0 to switch the feature off. That is a supported state, not
    /// a broken one: every other multiplier here is 1.0 permanently.
    #[serde(default = "one")]
    pub current: f32,
    #[serde(default = "one")]
    pub investigating: f32,
    #[serde(default = "one")]
    pub proposed: f32,
    #[serde(default = "default_superseded")]
    pub superseded: f32,
    #[serde(default)]
    pub decay: Decay,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Decay {
    pub enabled: bool,
    pub grace_days: f32,
    pub half_life_days: f32,
    pub floor: f32,
}

impl Default for Decay {
    fn default() -> Self {
        Self {
            enabled: false,
            grace_days: 90.0,
            half_life_days: 100.0,
            floor: 0.1,
        }
    }
}

impl Weights {
    /// This table with `o`'s named fields replacing theirs, field by field.
    fn overridden_by(&self, o: &WeightOverrides) -> Weights {
        Weights {
            markdown: o.markdown.unwrap_or(self.markdown),
            pdf: o.pdf.unwrap_or(self.pdf),
            web: o.web.unwrap_or(self.web),
            transcript: o.transcript.unwrap_or(self.transcript),
            memory: o.memory.unwrap_or(self.memory),
            authority: o.authority.unwrap_or(self.authority),
            current: o.current.unwrap_or(self.current),
            investigating: o.investigating.unwrap_or(self.investigating),
            proposed: o.proposed.unwrap_or(self.proposed),
            superseded: o.superseded.unwrap_or(self.superseded),
            decay: self.decay,
        }
    }

    /// Multiplier for a document's lifecycle position. Exactly 1.0 for
    /// `Current`, so a pack with no status file — every row `Current` — is
    /// bit-identical in ranking to one that never had the feature.
    pub fn lifecycle_weight(&self, lc: crate::pack::status::Lifecycle) -> f32 {
        use crate::pack::status::Lifecycle;
        match lc {
            Lifecycle::Current => self.current,
            Lifecycle::Investigating => self.investigating,
            Lifecycle::Proposed => self.proposed,
            Lifecycle::Superseded => self.superseded,
        }
    }

    /// Multiplier for a document with `inbound` links, where `max_inbound` is
    /// the most any document in the corpus has.
    ///
    /// Returns exactly 1.0 when authority is off, when nothing is linked, or
    /// when this document has no inbound links — so enabling it can only ever
    /// lift the well-linked, never demote the rest.
    pub fn authority_lift(&self, inbound: u32, max_inbound: u32) -> f32 {
        if self.authority <= 0.0 || max_inbound == 0 || inbound == 0 {
            return 1.0;
        }
        1.0 + self.authority * (inbound as f32 / max_inbound as f32)
    }

    pub fn decay_weight(&self, source_type: &str, last_used: Option<i64>, now: i64) -> f32 {
        if !self.decay.enabled || source_type != "transcript" {
            return 1.0;
        }
        let Some(last) = last_used else {
            return 1.0;
        };
        let past_grace = (now - last) as f32 / 86_400.0 - self.decay.grace_days;
        if past_grace <= 0.0 {
            return 1.0;
        }
        0.5f32
            .powf(past_grace / self.decay.half_life_days)
            .max(self.decay.floor)
    }

    pub fn for_source(&self, source_type: &str) -> f32 {
        match source_type {
            "pdf" => self.pdf,
            "web" => self.web,
            "transcript" => self.transcript,
            "memory" => self.memory,
            _ => self.markdown,
        }
    }
}

/// A surface's overrides of [`Weights`]. Only the fields that surface's table
/// actually names are applied; every other source type keeps whatever the
/// global `[weights]` table resolved to.
///
/// This MUST NOT be a plain `Weights`, for the same reason `quality` above is
/// not a bare `u8`. `Weights` fills its unset fields from `serde(default)`, so
/// the natural override —
///
/// ```toml
/// [mcp.weights]
/// transcript = 0.85
/// ```
///
/// — would also silently reset markdown, pdf and web to 1.0, discarding a
/// global `web = 0.8` set two lines earlier. A partial table must mean "these
/// fields", never "these fields, and defaults for everything else".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WeightOverrides {
    #[serde(default)]
    pub authority: Option<f32>,
    #[serde(default)]
    pub markdown: Option<f32>,
    #[serde(default)]
    pub pdf: Option<f32>,
    #[serde(default)]
    pub web: Option<f32>,
    #[serde(default)]
    pub transcript: Option<f32>,
    #[serde(default)]
    pub memory: Option<f32>,
    #[serde(default)]
    pub current: Option<f32>,
    #[serde(default)]
    pub investigating: Option<f32>,
    #[serde(default)]
    pub proposed: Option<f32>,
    #[serde(default)]
    pub superseded: Option<f32>,
}

/// The shipped demotion for a superseded record. See `Weights::superseded`
/// for the measurement that produced it and the golden case it costs.
fn default_superseded() -> f32 {
    0.88
}
fn one() -> f32 {
    1.0
}
fn default_transcript_weight() -> f32 {
    // Swept against a 35-case golden set over a 389-document index (38 notes,
    // 351 session transcripts). Unweighted recall was 0.71 at the thorough
    // tier; 0.75 gave 0.83; 0.45 and 0.30 both gave 0.86, and 0.94 at
    // exhaustive against a notes-only baseline of 0.97. 0.45 is chosen over
    // 0.30 because they measure the same and 0.45 keeps transcripts more
    // findable when they ARE the best answer.
    0.45
}
fn default_weights() -> Weights {
    Weights {
        markdown: one(),
        pdf: one(),
        web: one(),
        transcript: default_transcript_weight(),
        memory: one(),
        authority: 0.0,
        current: one(),
        investigating: one(),
        proposed: one(),
        superseded: default_superseded(),
        decay: Decay::default(),
    }
}

// `quality: None` here deliberately: the per-surface default lives in exactly one
// place, `Config::quality_for`, so a partial table and an absent table behave
// identically.
fn default_hook() -> SurfaceConfig {
    SurfaceConfig {
        quality: None,
        threshold: default_threshold(),
        max_tokens: default_max_tokens(),
        weights: WeightOverrides::default(),
    }
}
fn default_mcp() -> SurfaceConfig {
    // Deliberately more permissive than the hook: Claude called this on purpose
    // and wants a ranked list, while the hook fires unasked on every prompt and
    // must stay quiet unless something genuinely matches. 0.55 sits just under
    // the measured irrelevant floor (~0.565-0.67), so an explicit search still
    // returns ranked results rather than an empty response.
    SurfaceConfig {
        quality: None,
        threshold: 0.55,
        max_tokens: 4000,
        weights: WeightOverrides::default(),
    }
}
fn default_embed() -> EmbedConfig {
    EmbedConfig {
        model: default_embed_model(),
        dimensions: default_dimensions(),
        concurrency: default_concurrency(),
        batch: default_batch(),
        chunk_tokens: default_chunk_tokens(),
        prefix_scheme: None,
        ollama_url: default_ollama(),
        keep_alive: default_keep_alive(),
        contextual: false,
        enrich_model: default_enrich_model(),
        remote: None,
        remote_error: None,
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            index_transcripts: true,
            index_transcripts_max_age_days: None,
            index_codex_sessions: true,
            ignore: default_ignore(),
            hook: default_hook(),
            mcp: default_mcp(),
            embed: default_embed(),
            sources: Vec::new(),
            weights: default_weights(),
            pdf: PdfConfig::default(),
            memory: MemoryConfig::default(),
            update: UpdateConfig::default(),
            backup: BackupConfig::default(),
        }
    }
}

impl Config {
    /// Expand a leading `~` against the user's home directory.
    ///
    /// Only a leading `~` or `~/...`; a path like `foo/~/bar` is left alone,
    /// and so is `~user`, which we cannot resolve.
    pub fn expand_tilde_path(p: &std::path::Path) -> PathBuf {
        expand_tilde(p)
    }

    pub fn config_path() -> PathBuf {
        if let Ok(p) = std::env::var("BR8N_CONFIG") {
            return PathBuf::from(p);
        }
        directories::BaseDirs::new()
            .map(|b| b.config_dir().join("br8n/config.toml"))
            .unwrap_or_else(|| PathBuf::from("br8n.toml"))
    }

    pub fn db_path() -> PathBuf {
        if let Ok(p) = std::env::var("BR8N_DB") {
            return PathBuf::from(p);
        }
        directories::BaseDirs::new()
            .map(|b| b.data_dir().join("br8n/db"))
            .unwrap_or_else(|| PathBuf::from("br8n-db"))
    }

    pub fn backup_key_path(&self) -> PathBuf {
        self.backup
            .key_file
            .clone()
            .map(|p| expand_tilde(&p))
            .unwrap_or_else(|| Self::db_path().with_file_name("backup.key"))
    }

    pub fn drive_token_path(&self) -> PathBuf {
        self.backup
            .drive
            .as_ref()
            .and_then(|d| d.token_file.clone())
            .map(|p| expand_tilde(&p))
            .unwrap_or_else(|| Self::db_path().with_file_name("drive-token.json"))
    }

    pub fn backup_log_path() -> PathBuf {
        Self::db_path().with_extension("backup.log")
    }

    /// Never fails: a missing or malformed config yields defaults, because the
    /// hook must degrade to silence rather than error.
    pub fn load() -> Config {
        Self::load_from(&Self::config_path())
    }

    pub fn load_from(config_path: &std::path::Path) -> Config {
        let mut cfg: Config = std::fs::read_to_string(config_path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();
        // Nothing expands `~` on the way out of a TOML string, and
        // `Path::new("~/notes").exists()` is false — so the documented
        // `sources = ["~/notes"]` made `discover` hard-fail with "configured
        // source does not exist". Expand once, here, so every consumer
        // downstream (discovery, and prune's reachability test) compares real
        // absolute paths.
        cfg.sources = cfg.sources.iter().map(|p| expand_tilde(p)).collect();
        match crate::env_file::resolve(config_path) {
            Ok(remote) => cfg.embed.remote = remote,
            Err(e) => cfg.embed.remote_error = Some(format!("{e:#}")),
        }
        cfg
    }

    pub fn surface(&self, s: Surface) -> &SurfaceConfig {
        match s {
            Surface::Hook => &self.hook,
            Surface::Mcp => &self.mcp,
        }
    }

    /// The single source of the asymmetric defaults: hook is `fast`, the MCP tool
    /// is `thorough`. Applies whether the surface's table is absent or partial.
    pub fn quality_for(&self, s: Surface) -> u8 {
        self.surface(s).quality.unwrap_or(match s {
            Surface::Hook => 1,
            Surface::Mcp => 3,
        })
    }

    pub fn profile_for(&self, s: Surface) -> Profile {
        Profile::tier(self.quality_for(s))
    }

    /// The weights a surface retrieves with: the global `[weights]` table with
    /// that surface's `[hook.weights]`/`[mcp.weights]` entries layered on top.
    ///
    /// One number cannot do both jobs. `relevance` is EFFECTIVE relevance and
    /// drives the gate as well as the ordering, so at the global transcript
    /// weight of 0.45 a transcript needs a 1.56 cosine to clear the hook's 0.70
    /// gate and a 1.22 cosine to clear the MCP's 0.55 — both arithmetically
    /// impossible. Transcripts could therefore never be injected or returned by
    /// an explicit search however plainly they were the answer; only the ungated
    /// CLI reached them. Ordering wants 0.45 (swept on a 35-case golden set:
    /// 0.86 recall against 0.83 at 0.75), injection wants something above 0.82,
    /// and those are different numbers. Splitting them per surface lets the hook
    /// keep the strict one — it fires unasked on every prompt — while the MCP
    /// tool, which Claude calls deliberately and can judge the answer to, may be
    /// configured to let a strong transcript through.
    pub fn weights_for(&self, s: Surface) -> Weights {
        self.weights.overridden_by(&self.surface(s).weights)
    }
}

#[cfg(test)]
mod tests {
    use super::{concurrency_for_cores, Config, OcrMode, Profile, Surface};

    /// Measured warm floor for any search at all: ~40ms fixed + ~24ms embed.
    const WARM_FLOOR_MS: u32 = 65;

    #[test]
    fn concurrency_for_cores_is_half_the_cores_clamped_one_to_four() {
        let cases = [(1, 1), (2, 1), (4, 2), (8, 4), (16, 4)];
        for (cores, expected) in cases {
            assert_eq!(
                concurrency_for_cores(cores),
                expected,
                "cores={cores} should give concurrency={expected}"
            );
        }
    }

    #[test]
    fn explicit_embed_concurrency_overrides_the_core_derived_default() {
        let c: Config = toml::from_str("[embed]\nconcurrency = 7\n").unwrap();
        assert_eq!(c.embed.concurrency, 7);
    }

    #[test]
    fn tiers_increase_in_cost_monotonically() {
        let budgets: Vec<u32> = (0..=4).map(|t| Profile::tier(t).budget_ms).collect();
        for w in budgets.windows(2) {
            assert!(
                w[1] > w[0],
                "budgets must strictly increase, got {budgets:?}"
            );
        }
    }

    #[test]
    fn every_tier_budget_clears_the_measured_floor_with_room() {
        // This asserts the RULE, not a snapshot of the numbers. The previous
        // version pinned `[90, 130, 200, 450, 1200]` exactly, which meant it
        // could not notice that tier 0's 90ms sat only 25ms above the 65ms warm
        // floor — the margin the stage-gating tests recorded themselves failing
        // on under parallel load. A ceiling that trips on a healthy machine is
        // not a deadline; it is silent degradation with no symptom but worse
        // results.
        for tier in 0..=4u8 {
            let p = Profile::tier(tier);
            assert!(
                p.budget_ms >= WARM_FLOOR_MS * 2,
                "tier {tier} ({}) allows {}ms against a {WARM_FLOOR_MS}ms floor — \
                 under 2x, it will degrade on a busy machine rather than a slow query",
                p.name,
                p.budget_ms
            );
        }
    }

    #[test]
    fn tier_0_runs_bm25_but_nothing_heavier() {
        // This test used to be `tier_0_is_vector_only` and asserted `!p.bm25`.
        // Tier 0 is the LATENCY tier, not the one-signal tier: what it must not
        // pay for is graph expansion (which forces a store open) and LLM
        // reranking (which is a network round trip per candidate). BM25 comes
        // off the mmapped pack over `candidates_k: 5` postings, and it is the
        // only thing that can close the measured 0.42-against-0.52 recall gap
        // that this tier carried for exactly as long as the flag was false.
        let p = Profile::tier(0);
        assert!(
            p.bm25,
            "tier 0 must run BM25: the only signal that can lift this tier's recall"
        );
        assert!(
            p.graph.is_none(),
            "graph expansion would force a store open"
        );
        assert!(p.rerank.is_none());
    }

    #[test]
    fn no_shipped_profile_enables_llm_reranking() {
        // Previously tiers 3 and 4 turned it on. It was measured as noise:
        // qwen3:0.6b answers "yes" to every query/passage pair, qwen3:4b
        // rejects most of them, and Ollama cannot serve a true cross-encoder
        // (no /api/rerank endpoint). Ordering by the cosine took thorough from
        // 0.80 to 1.00 recall at a fifth of the latency.
        //
        // The field remains, so a real cross-encoder can be wired in later —
        // this asserts that nothing ships with the LLM stand-in enabled.
        for tier in 0..=4u8 {
            assert!(
                Profile::tier(tier).rerank.is_none(),
                "tier {tier} must not enable LLM reranking"
            );
        }
    }

    #[test]
    fn tiers_clamp_out_of_range_input() {
        assert_eq!(Profile::tier(9).budget_ms, Profile::tier(4).budget_ms);
    }

    #[test]
    fn default_surfaces_are_asymmetric() {
        let c = Config::default();
        assert_eq!(c.quality_for(Surface::Hook), 1, "hook defaults to `fast`");
        assert_eq!(
            c.quality_for(Surface::Mcp),
            3,
            "mcp tool defaults to `thorough`"
        );
        assert!(c.profile_for(Surface::Mcp).budget_ms > c.profile_for(Surface::Hook).budget_ms);
    }

    #[test]
    fn the_hook_gate_sits_above_the_measured_irrelevant_floor() {
        // The bounds are measured, and the measurement has been redone once.
        //
        // The first was four hand-checked queries: unrelated content at
        // 0.565-0.669, correct matches at 0.727-0.864, so the gate had to sit
        // between them and the guard read `> 0.67`. That floor was an artifact
        // of n=4. `br8n bench`'s trade-off curve, over 222 golden cases at 650
        // documents, puts the tier-1 answer floor at 0.623 and shows a gate at
        // 0.70 cutting 18% of answer cases.
        //
        // The lower bound is now 0.63, which is what the curve supports: the
        // non-leaked negative ceiling is 0.708 (one document), transcript noise
        // clusters at or below 0.686, and below ~0.62 the negative class opens
        // up fast — 11 of 26 cases at 0.60 against 2 at 0.64. The guard's job is
        // unchanged: stop the gate sinking into the noise, where the hook
        // injects on every prompt.
        let c = Config::default();
        assert!(
            c.hook.threshold > 0.63,
            "hook gate must clear the noise floor"
        );
        assert!(
            c.hook.threshold < 0.727,
            "hook gate must not reject genuine matches"
        );
        assert!(
            c.mcp.threshold < c.hook.threshold,
            "an explicit search should be more permissive than an unasked injection"
        );
    }

    #[test]
    fn transcripts_are_indexed_by_default_but_can_be_declined() {
        assert!(Config::default().index_transcripts);
        let off: Config = toml::from_str("index_transcripts = false").unwrap();
        assert!(!off.index_transcripts);
        // A config that says nothing about it must still opt in.
        let quiet: Config = toml::from_str("[hook]\nthreshold = 0.6\n").unwrap();
        assert!(quiet.index_transcripts);
    }

    #[test]
    fn codex_sessions_are_indexed_by_default_but_can_be_declined() {
        assert!(Config::default().index_codex_sessions);
        let off: Config = toml::from_str("index_codex_sessions = false").unwrap();
        assert!(!off.index_codex_sessions);
        assert!(off.index_transcripts);
        let quiet: Config = toml::from_str("[hook]\nthreshold = 0.6\n").unwrap();
        assert!(quiet.index_codex_sessions);
    }

    #[test]
    fn transcript_age_limit_defaults_to_unset_and_round_trips() {
        assert_eq!(Config::default().index_transcripts_max_age_days, None);
        let limited: Config = toml::from_str("index_transcripts_max_age_days = 30").unwrap();
        assert_eq!(limited.index_transcripts_max_age_days, Some(30));
        let quiet: Config = toml::from_str("[hook]\nthreshold = 0.6\n").unwrap();
        assert_eq!(quiet.index_transcripts_max_age_days, None);
    }

    #[test]
    fn a_partial_surface_table_keeps_that_surface_default() {
        // The most natural override a user writes. A bare `u8` + serde(default)
        // would silently make this tier 0 (`instant`) instead of tier 1 (`fast`).
        let c: Config = toml::from_str("[hook]\nthreshold = 0.65\n").unwrap();
        assert_eq!(
            c.quality_for(Surface::Hook),
            1,
            "partial table must not collapse to tier 0"
        );
        assert_eq!(c.hook.threshold, 0.65);
        assert_eq!(
            c.quality_for(Surface::Mcp),
            3,
            "untouched surface keeps its default"
        );
    }

    #[test]
    fn a_partial_mcp_table_keeps_the_mcp_default_too() {
        // Same code path as `a_partial_surface_table_keeps_that_surface_default`,
        // exercised from the other surface — only `[hook]` was covered before,
        // and the two surfaces have different defaults (1 vs 3), so this is not
        // redundant with the hook-side test.
        let c: Config = toml::from_str("[mcp]\nthreshold = 0.30\n").unwrap();
        assert_eq!(
            c.quality_for(Surface::Mcp),
            3,
            "partial table must not collapse to tier 0"
        );
        assert_eq!(c.mcp.threshold, 0.30);
        assert_eq!(
            c.quality_for(Surface::Hook),
            1,
            "untouched surface keeps its default"
        );
    }

    #[test]
    fn an_explicit_quality_of_zero_is_honoured_not_treated_as_unset() {
        let c: Config = toml::from_str("[hook]\nquality = 0\n").unwrap();
        assert_eq!(c.quality_for(Surface::Hook), 0);
    }

    #[test]
    fn the_shipped_defaults_put_every_surface_on_the_global_table() {
        // The new field must be invisible until someone writes it. `Weights`
        // is the only thing `Retriever` ever sees, so if this drifts, every
        // calibrated threshold in the codebase is being compared against a
        // scale that moved under it.
        let empty: Config = toml::from_str("").unwrap();
        for c in [Config::default(), empty] {
            for s in [Surface::Hook, Surface::Mcp] {
                let w = c.weights_for(s);
                assert_eq!(w.transcript, 0.45);
                assert_eq!(w.markdown, 1.0);
                assert_eq!(w.pdf, 1.0);
                assert_eq!(w.web, 1.0);
            }
        }
    }

    /// The four lifecycle multipliers must ship INERT — 1.0 — down BOTH default
    /// paths, and the two paths are not the same code.
    ///
    /// `Config::default()` and `toml::from_str("")` both reach `default_weights()`,
    /// the hand-written literal, because `weights` itself carries
    /// `#[serde(default = "default_weights")]`. The FIELD-level
    /// `#[serde(default = "one")]` attributes only fire when a `[weights]` table is
    /// PRESENT and the field is absent — which is what a real user's config looks
    /// like the moment they set any one weight.
    ///
    /// Those two defaults live two dozen lines apart, one in a field attribute and
    /// one in a freestanding function, and nothing but this test makes them agree.
    /// Measured cost of them disagreeing: changing the attribute to a bare
    /// `#[serde(default)]` yields `0.0` for an `f32`, so a user who set only
    /// `transcript` would silently get `superseded = 0.0` — every superseded record
    /// crushed to zero relevance rather than left alone — with the whole suite green.
    #[test]
    fn a_partial_weights_table_still_ships_every_lifecycle_multiplier_inert() {
        use crate::pack::status::Lifecycle;

        // A `[weights]` table that names ONE key. This is the path the literal
        // never covers.
        let partial: Config = toml::from_str("[weights]\ntranscript = 0.5\n").unwrap();
        assert_eq!(partial.weights.transcript, 0.5, "the key the user set");

        for (label, w) in [
            ("partial [weights] table", &partial.weights),
            ("hand-written literal", &Config::default().weights),
        ] {
            assert_eq!(
                w.lifecycle_weight(Lifecycle::Superseded),
                0.88,
                "{label}: superseded ships ON at 0.88. It is the ONLY multiplier \
                 that is not 1.0, and the value is a measured trade, not a \
                 preference — see `Weights::superseded`. If you are changing this \
                 number, re-measure both bounds on the live index first; the \
                 design's original window closed once the corpus moved."
            );
            assert_eq!(
                w.lifecycle_weight(Lifecycle::Current),
                1.0,
                "{label}: current"
            );
            assert_eq!(
                w.lifecycle_weight(Lifecycle::Proposed),
                1.0,
                "{label}: proposed — most notes carry no `status:` at all and land here"
            );
            assert_eq!(
                w.lifecycle_weight(Lifecycle::Investigating),
                1.0,
                "{label}: investigating"
            );
        }
    }

    #[test]
    fn pdf_defaults_are_ocr_auto_with_a_confidence_floor() {
        // OCR is on by default, so a config that never mentions `[pdf]` still
        // reads scanned documents. The floor matters as much as the mode: the
        // crate's own default of 0.0 would index recognition noise.
        let empty: Config = toml::from_str("").unwrap();
        for c in [Config::default(), empty] {
            assert_eq!(c.pdf.ocr, OcrMode::Auto);
            assert_eq!(c.pdf.ocr_min_confidence, 0.5);
            assert_eq!(c.pdf.model_dir, None);
            assert_eq!(c.pdf.pdfium_lib_path, None);
            assert_eq!(c.pdf.ort_dylib_path, None);
        }
    }

    #[test]
    fn the_default_query_path_never_loads_the_enrichment_model() {
        let empty: Config = toml::from_str("").unwrap();
        for c in [Config::default(), empty] {
            assert!(!c.embed.contextual);
            assert_eq!(c.embed.keep_alive, crate::config::KeepAlive::from("30m"));
        }
    }

    #[test]
    fn a_partial_pdf_table_keeps_the_defaults_of_its_siblings() {
        // The hazard `[weights]` already has: naming one key must not silently
        // zero the rest. Someone turning OCR off should not also lose the
        // confidence floor that protects them when they turn it back on.
        let c: Config = toml::from_str(
            r#"
            [pdf]
            ocr = "off"
            "#,
        )
        .unwrap();
        assert_eq!(c.pdf.ocr, OcrMode::Off);
        assert_eq!(
            c.pdf.ocr_min_confidence, 0.5,
            "an unmentioned sibling must keep its default, not reset to zero"
        );
    }

    #[test]
    fn pdf_library_paths_round_trip_from_config() {
        let c: Config = toml::from_str(
            r#"
            [pdf]
            ocr = "force"
            ocr_min_confidence = 0.8
            pdfium_lib_path = "/somewhere/libpdfium.dylib"
            ort_dylib_path = "/somewhere/libonnxruntime.dylib"
            "#,
        )
        .unwrap();
        assert_eq!(c.pdf.ocr, OcrMode::Force);
        assert_eq!(c.pdf.ocr_min_confidence, 0.8);
        assert_eq!(
            c.pdf.pdfium_lib_path.as_deref(),
            Some(std::path::Path::new("/somewhere/libpdfium.dylib"))
        );
        assert_eq!(
            c.pdf.ort_dylib_path.as_deref(),
            Some(std::path::Path::new("/somewhere/libonnxruntime.dylib"))
        );
    }

    #[test]
    fn a_surface_weight_table_overrides_the_global_one() {
        let c: Config = toml::from_str(
            r#"
            [weights]
            transcript = 0.45

            [mcp.weights]
            transcript = 0.85
            "#,
        )
        .unwrap();
        assert_eq!(c.weights_for(Surface::Mcp).transcript, 0.85);
        assert_eq!(
            c.weights_for(Surface::Hook).transcript,
            0.45,
            "the surface that asked for nothing must not inherit the other's override"
        );
        assert_eq!(
            c.weights.transcript, 0.45,
            "and the global table itself is untouched — it still orders the CLI"
        );
    }

    #[test]
    fn a_surface_without_a_weight_table_falls_back_to_the_global_one() {
        let c: Config = toml::from_str(
            r#"
            [weights]
            transcript = 0.30
            web = 0.80
            "#,
        )
        .unwrap();
        for s in [Surface::Hook, Surface::Mcp] {
            assert_eq!(c.weights_for(s).transcript, 0.30);
            assert_eq!(c.weights_for(s).web, 0.80);
        }
    }

    #[test]
    fn a_partial_surface_weight_table_leaves_every_other_source_alone() {
        // The bug this shape exists to prevent, and one this codebase has
        // already shipped once in another table: a partial TOML table whose
        // unnamed fields collapse to a default — 0.0 for a bare `f32`, or 1.0
        // via `Weights`'s own serde defaults — silently undoing the global
        // table written two lines above it. Overriding the transcript weight
        // must override the transcript weight and nothing else.
        let c: Config = toml::from_str(
            r#"
            [weights]
            web = 0.80
            transcript = 0.30

            [mcp.weights]
            transcript = 0.85
            "#,
        )
        .unwrap();
        let w = c.weights_for(Surface::Mcp);
        assert_eq!(w.transcript, 0.85, "the named field is overridden");
        assert_eq!(
            w.web, 0.80,
            "an unnamed field keeps the GLOBAL value, not a default"
        );
        assert_eq!(w.markdown, 1.0);
        assert_eq!(w.pdf, 1.0);
        for (name, v) in [("markdown", w.markdown), ("pdf", w.pdf), ("web", w.web)] {
            assert!(v > 0.0, "{name} was zeroed by a partial table");
        }
    }

    #[test]
    fn the_mcp_weight_is_what_lets_a_strong_transcript_clear_a_gate_at_all() {
        // The arithmetic the per-surface split exists for. `relevance` is
        // weighted BEFORE the gate reads it, so the effective score is
        // cosine * weight. At the global 0.45 a transcript needs 1.56 to clear
        // the hook's 0.70 and 1.22 to clear the MCP's 0.55 — impossible, so a
        // transcript could never be injected or returned by an explicit search
        // no matter how plainly it was the answer.
        let c: Config = toml::from_str(
            r#"
            [weights]
            transcript = 0.45

            [mcp.weights]
            transcript = 0.85
            "#,
        )
        .unwrap();
        // A strong but entirely plausible cosine for this embedding model:
        // correct matches were measured at 0.727-0.864.
        let cosine = 0.85f32;

        let mcp = cosine * c.weights_for(Surface::Mcp).for_source("transcript");
        assert!(
            mcp >= c.surface(Surface::Mcp).threshold,
            "a 0.85 transcript must be able to clear the MCP gate: {mcp} < {}",
            c.surface(Surface::Mcp).threshold
        );

        let hook = cosine * c.weights_for(Surface::Hook).for_source("transcript");
        assert!(
            hook < c.surface(Surface::Hook).threshold,
            "the hook fires unasked and must stay strict: {hook} >= {}",
            c.surface(Surface::Hook).threshold
        );
    }

    #[test]
    fn config_parses_from_toml() {
        let c: Config = toml::from_str(
            r#"
            [hook]
            quality = 2
            [mcp]
            quality = 4
            [embed]
            model = "nomic-embed-text"
            dimensions = 768
            "#,
        )
        .unwrap();
        assert_eq!(c.quality_for(Surface::Hook), 2);
        assert_eq!(c.quality_for(Surface::Mcp), 4);
        assert_eq!(c.embed.dimensions, 768);
    }
}

#[cfg(test)]
mod load_tests {
    //! `Config::load()` reads `BR8N_CONFIG`/`BR8N_DB` from the process environment,
    //! and `std::env::set_var` mutates that environment globally. Rust runs tests in
    //! parallel threads by default, so any two tests that each set these vars and then
    //! assert on them race with each other. Every env-var-dependent assertion below
    //! therefore lives in this single `#[test]` function, which the test harness runs
    //! on one thread — sequencing is achieved by construction, not by `#[serial]` or
    //! similar.
    use super::{Config, Surface};
    use std::ffi::OsStr;
    use std::ffi::OsString;
    use std::io::Write;

    /// RAII guard for a single process env var. Captures whatever was there
    /// before (possibly nothing), sets the new value immediately, and restores
    /// the prior state on `Drop` — including when the drop happens because a
    /// test panicked partway through. Without this, an `assert!` failure
    /// between `set_var` calls leaves `BR8N_CONFIG`/`BR8N_DB` set for the
    /// rest of the process, corrupting whatever test runs next.
    struct EnvGuard {
        key: &'static str,
        prior: Option<OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
            let prior = std::env::var_os(key);
            std::env::set_var(key, value);
            EnvGuard { key, prior }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // Drop must not panic: a `Drop` that panics during unwinding
            // aborts the process instead of letting the original panic
            // propagate, which would defeat the whole point of this guard.
            match self.prior.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn load_and_db_path_honour_and_survive_the_env() {
        // 1. BR8N_CONFIG pointing at a path that does not exist -> defaults, no panic.
        let missing = tempfile::tempdir()
            .unwrap()
            .path()
            .join("does-not-exist.toml");
        let _config_guard = EnvGuard::set("BR8N_CONFIG", &missing);
        let c = Config::load();
        assert_eq!(c.quality_for(Surface::Hook), 1);
        assert_eq!(c.quality_for(Surface::Mcp), 3);

        // 2. BR8N_CONFIG pointing at malformed TOML -> defaults, no panic.
        let mut bad = tempfile::NamedTempFile::new().unwrap();
        write!(bad, "[hook\nthis is not toml").unwrap();
        let _config_guard = EnvGuard::set("BR8N_CONFIG", bad.path());
        let c = Config::load();
        assert_eq!(c.quality_for(Surface::Hook), 1);
        assert_eq!(c.quality_for(Surface::Mcp), 3);

        // 3. BR8N_CONFIG pointing at a valid partial config -> values honoured,
        //    quality_for still correct through the asymmetric-default path.
        let mut partial = tempfile::NamedTempFile::new().unwrap();
        write!(partial, "[hook]\nthreshold = 0.65\n").unwrap();
        let _config_guard = EnvGuard::set("BR8N_CONFIG", partial.path());
        let c = Config::load();
        assert_eq!(c.hook.threshold, 0.65);
        assert_eq!(
            c.quality_for(Surface::Hook),
            1,
            "partial table must not collapse to tier 0"
        );
        assert_eq!(c.quality_for(Surface::Mcp), 3);

        // 4. BR8N_DB override is reflected by Config::db_path().
        let db_dir = tempfile::tempdir().unwrap();
        let db_path = db_dir.path().join("br8n-db");
        let _db_guard = EnvGuard::set("BR8N_DB", &db_path);
        assert_eq!(Config::db_path(), db_path);
    }

    #[test]
    fn source_paths_expand_a_leading_tilde() {
        // `sources = ["~/notes"]` is what the README and the plugin docs tell
        // people to write, and nothing expands `~` on the way out of a TOML
        // string. `Path::new("~/notes").exists()` is false, so `discover`
        // aborted the whole run with "configured source does not exist" —
        // the documented configuration could not work at all.
        let home = directories::BaseDirs::new()
            .unwrap()
            .home_dir()
            .to_path_buf();

        let expanded = Config::expand_tilde_path(std::path::Path::new("~/notes"));
        assert_eq!(expanded, home.join("notes"));

        assert_eq!(Config::expand_tilde_path(std::path::Path::new("~")), home);

        // Only a LEADING tilde. An absolute path is untouched, and a `~` in the
        // middle of a path is a real directory name, not a home reference.
        let abs = std::path::Path::new("/srv/notes");
        assert_eq!(Config::expand_tilde_path(abs), abs);
        let mid = std::path::Path::new("/srv/~/notes");
        assert_eq!(Config::expand_tilde_path(mid), mid);
    }

    #[test]
    fn a_broken_env_file_is_carried_not_panicked() {
        let t = tempfile::tempdir().unwrap();
        let cfg_path = t.path().join("config.toml");
        std::fs::write(&cfg_path, "").unwrap();
        std::fs::write(t.path().join("env"), "BR8N_EMBED_URL=http://h:1\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(t.path().join("env"), std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        let cfg = Config::load_from(&cfg_path);
        assert!(cfg.embed.remote.is_none());
        let err = cfg.embed.remote_error.expect("the failure must be carried");
        assert!(err.contains("BR8N_EMBED_MODEL"), "{err}");
    }

    #[test]
    fn a_complete_env_file_carries_the_remote_endpoint() {
        let t = tempfile::tempdir().unwrap();
        let cfg_path = t.path().join("config.toml");
        std::fs::write(&cfg_path, "").unwrap();
        std::fs::write(
            t.path().join("env"),
            "BR8N_EMBED_URL=http://h:1/\nBR8N_EMBED_MODEL=m\nBR8N_EMBED_TOKEN=tok\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(t.path().join("env"), std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        let cfg = Config::load_from(&cfg_path);
        assert!(
            cfg.embed.remote_error.is_none(),
            "{:?}",
            cfg.embed.remote_error
        );
        assert_eq!(
            cfg.embed.remote,
            Some(crate::env_file::RemoteEmbed {
                url: "http://h:1".into(),
                model: "m".into(),
                token: "tok".into(),
            })
        );
    }

    #[test]
    fn a_config_with_no_env_file_beside_it_carries_no_remote() {
        let t = tempfile::tempdir().unwrap();
        let cfg_path = t.path().join("config.toml");
        std::fs::write(&cfg_path, "").unwrap();
        let cfg = Config::load_from(&cfg_path);
        assert!(cfg.embed.remote.is_none());
        assert!(cfg.embed.remote_error.is_none());
    }

    #[test]
    fn decay_is_flat_through_the_grace_period_then_falls_to_the_floor() {
        let mut w = Config::default().weights;
        w.decay.enabled = true;
        let now = 1_800_000_000i64;
        let days = |n: f32| Some(now - (n * 86_400.0) as i64);

        assert_eq!(w.decay_weight("transcript", days(0.0), now), 1.0);
        assert_eq!(w.decay_weight("transcript", days(89.0), now), 1.0);
        let at_six_months = w.decay_weight("transcript", days(180.0), now);
        assert!(
            at_six_months < 0.65 && at_six_months > 0.45,
            "six months in, a transcript should be clearly demoted but not gone: {at_six_months}"
        );
        let at_a_year = w.decay_weight("transcript", days(365.0), now);
        assert!(
            (at_a_year - 0.5f32.powf(2.75)).abs() < 1e-4,
            "a year is 275 days past grace, 2.75 half-lives: {at_a_year}"
        );
        assert_eq!(
            w.decay_weight("transcript", days(3650.0), now),
            w.decay.floor,
            "a decade on, the floor holds"
        );
        assert_eq!(
            w.decay_weight("markdown", days(3650.0), now),
            1.0,
            "a vault note is not less true for being old"
        );
        assert_eq!(
            w.decay_weight("transcript", None, now),
            1.0,
            "never retrieved keeps full weight"
        );
        let mut off = w.clone();
        off.decay.enabled = false;
        assert_eq!(off.decay_weight("transcript", days(3650.0), now), 1.0);
    }

    #[test]
    fn decay_ships_off_and_is_switched_on_from_its_own_table() {
        assert!(!Config::default().weights.decay.enabled);
        let c: Config = toml::from_str("[weights.decay]\nenabled = true\n").unwrap();
        assert!(c.weights.decay.enabled);
        assert_eq!(c.weights.decay.grace_days, 90.0);
        assert_eq!(c.weights_for(Surface::Hook).decay, c.weights.decay);
    }
}
