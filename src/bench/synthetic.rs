use crate::config::{Config, Profile, Surface};
use crate::embed::Embedder;
use crate::model::{Chunk, Document};
use crate::pack::records::Record;
use crate::pack::Pack;
use crate::retrieve::Retriever;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const QUERY_COUNT: usize = 200;
pub const TIERS: [u8; 2] = [0, 1];

const VOCABULARY_SIZE: usize = 20_000;
const ZIPF_EXPONENT: f64 = 1.07;
const CHUNKS_PER_DOCUMENT: (usize, usize) = (5, 60);
const WORDS_PER_CHUNK: (usize, usize) = (150, 300);
const WORDS_PER_TITLE: usize = 3;
const WORDS_PER_QUERY: (usize, usize) = (3, 8);
const QUERY_NOISE_RATIO: f32 = 0.75;
const QUERY_STREAM_SALT: u64 = 0x5EED_0F9E_7715_0001;
const SYLLABLES_CONSONANTS: &[u8] = b"bdfgklmnprstvz";
const SYLLABLES_VOWELS: &[u8] = b"aeiou";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spec {
    pub chunks: usize,
    pub seed: u64,
    pub dims: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Query {
    pub text: String,
    pub vector: Vec<f32>,
}

pub struct Corpus {
    pub rows: Vec<(Record, Vec<f32>)>,
    pub queries: Vec<Query>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TierLatency {
    pub tier: u8,
    pub name: &'static str,
    pub queries: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub degraded_queries: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyntheticReport {
    pub chunks: usize,
    pub seed: u64,
    pub dims: usize,
    pub pack_build_ms: Option<u64>,
    pub pack_bytes: u64,
    pub pack_bytes_per_chunk: u64,
    pub tiers: Vec<TierLatency>,
    pub peak_rss_bytes: u64,
}

struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: usize) -> usize {
        ((self.unit() * n as f64) as usize).min(n.saturating_sub(1))
    }

    fn between(&mut self, (lo, hi): (usize, usize)) -> usize {
        lo + self.below(hi - lo + 1)
    }

    fn gaussian(&mut self) -> f32 {
        let u1 = 1.0 - self.unit();
        let u2 = self.unit();
        ((-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()) as f32
    }

    fn unit_vector(&mut self, dims: usize) -> Vec<f32> {
        crate::embed::normalize((0..dims).map(|_| self.gaussian()).collect())
    }
}

struct Vocabulary {
    words: Vec<String>,
    cumulative_weight: Vec<f64>,
}

impl Vocabulary {
    fn fixed() -> Vocabulary {
        let syllable_count = SYLLABLES_CONSONANTS.len() * SYLLABLES_VOWELS.len();
        let words = (0..VOCABULARY_SIZE)
            .map(|rank| word_for(rank + syllable_count, syllable_count))
            .collect();
        let mut total = 0.0;
        let cumulative_weight = (0..VOCABULARY_SIZE)
            .map(|rank| {
                total += 1.0 / ((rank + 1) as f64).powf(ZIPF_EXPONENT);
                total
            })
            .collect();
        Vocabulary {
            words,
            cumulative_weight,
        }
    }

    fn draw(&self, rng: &mut SplitMix64) -> &str {
        let total = *self.cumulative_weight.last().unwrap_or(&0.0);
        let target = rng.unit() * total;
        let rank = self
            .cumulative_weight
            .partition_point(|&w| w <= target)
            .min(self.words.len() - 1);
        &self.words[rank]
    }

    fn sentence(&self, rng: &mut SplitMix64, words: usize) -> String {
        let mut out = String::with_capacity(words * 8);
        for i in 0..words {
            if i > 0 {
                out.push(' ');
            }
            out.push_str(self.draw(rng));
        }
        out
    }
}

fn word_for(mut n: usize, syllable_count: usize) -> String {
    let mut syllables = Vec::new();
    while n > 0 {
        let s = n % syllable_count;
        syllables.push(s);
        n /= syllable_count;
    }
    let mut word = String::with_capacity(syllables.len() * 2);
    for s in syllables.into_iter().rev() {
        word.push(SYLLABLES_CONSONANTS[s / SYLLABLES_VOWELS.len()] as char);
        word.push(SYLLABLES_VOWELS[s % SYLLABLES_VOWELS.len()] as char);
    }
    word
}

pub fn generate(spec: &Spec) -> Corpus {
    let vocabulary = Vocabulary::fixed();
    let mut rng = SplitMix64(spec.seed);
    let mut rows: Vec<(Record, Vec<f32>)> = Vec::with_capacity(spec.chunks);
    let mut document = 0usize;
    while rows.len() < spec.chunks {
        let size = rng
            .between(CHUNKS_PER_DOCUMENT)
            .min(spec.chunks - rows.len());
        let uri = format!("synthetic:///doc-{document:07}.md");
        let doc_id = Document::new_id(&uri);
        let title = vocabulary.sentence(&mut rng, WORDS_PER_TITLE);
        for ordinal in 0..size {
            let words = rng.between(WORDS_PER_CHUNK);
            let text = vocabulary.sentence(&mut rng, words);
            let record = Record {
                chunk_id: Chunk::id(&doc_id, ordinal as i64),
                doc_id: doc_id.clone(),
                text,
                heading_path: String::new(),
                uri: uri.clone(),
                title: title.clone(),
                page_no: None,
                source_type: "markdown".to_string(),
                inbound: 0,
                lifecycle: Default::default(),
                last_used: None,
                memory: None,
            };
            rows.push((record, rng.unit_vector(spec.dims)));
        }
        document += 1;
    }
    rows.sort_by(|a, b| a.0.chunk_id.cmp(&b.0.chunk_id));
    let queries = generate_queries(spec, &rows);
    Corpus { rows, queries }
}

fn generate_queries(spec: &Spec, rows: &[(Record, Vec<f32>)]) -> Vec<Query> {
    if rows.is_empty() {
        return Vec::new();
    }
    let mut rng = SplitMix64(spec.seed ^ QUERY_STREAM_SALT);
    let noise_scale = QUERY_NOISE_RATIO / (spec.dims as f32).sqrt();
    (0..QUERY_COUNT)
        .map(|_| {
            let (record, vector) = &rows[rng.below(rows.len())];
            let source_words: Vec<&str> = record.text.split_whitespace().collect();
            let text = (0..rng.between(WORDS_PER_QUERY))
                .map(|_| source_words[rng.below(source_words.len())])
                .collect::<Vec<_>>()
                .join(" ");
            let noisy = vector
                .iter()
                .map(|x| x + noise_scale * rng.gaussian())
                .collect();
            Query {
                text,
                vector: crate::embed::normalize(noisy),
            }
        })
        .collect()
}

pub struct QueryEmbedder {
    model_id: String,
    dims: usize,
    vectors: HashMap<String, Vec<f32>>,
}

impl QueryEmbedder {
    pub fn new(model_id: String, dims: usize, queries: &[Query]) -> QueryEmbedder {
        let mut vectors = HashMap::new();
        for q in queries {
            vectors
                .entry(q.text.clone())
                .or_insert_with(|| q.vector.clone());
        }
        QueryEmbedder {
            model_id,
            dims,
            vectors,
        }
    }
}

impl Embedder for QueryEmbedder {
    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        texts.iter().map(|t| self.embed_query(t)).collect()
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.vectors
            .get(text)
            .cloned()
            .with_context(|| format!("the synthetic embedder has no vector for {text:?}"))
    }

    fn warm(&self) -> Result<()> {
        Ok(())
    }

    fn model_id(&self) -> String {
        self.model_id.clone()
    }

    fn dimensions(&self) -> usize {
        self.dims
    }
}

pub fn configured_model_id(cfg: &Config) -> Result<String> {
    Ok(crate::embed::for_config(&cfg.embed)?.model_id())
}

pub fn build_pack(
    dir: &Path,
    model_id: &str,
    dims: usize,
    rows: Vec<(Record, Vec<f32>)>,
) -> Result<u64> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let started = Instant::now();
    Pack::build(dir, model_id, dims, rows, &HashMap::new(), &HashMap::new())?;
    Ok(started.elapsed().as_millis() as u64)
}

pub fn hook_retriever(
    cfg: &Config,
    dir: &Path,
    model_id: &str,
    queries: &[Query],
) -> Result<Retriever> {
    let dims = cfg.embed.dimensions;
    let pack = Pack::open(dir, model_id, dims)?;
    let embedder = QueryEmbedder::new(model_id.to_string(), dims, queries);
    Ok(
        Retriever::packed(pack, Box::new(embedder), cfg.embed.ollama_url.clone())
            .with_memory(Ok(None::<crate::pack::Pack>), cfg.memory.clone())
            .with_weights(cfg.weights_for(Surface::Hook)),
    )
}

pub fn time_tier(retriever: &Retriever, queries: &[Query], tier: u8) -> Result<TierLatency> {
    let profile = Profile::tier(tier);
    let mut elapsed_ms: Vec<f32> = Vec::with_capacity(queries.len());
    let mut degraded_queries = 0;
    for q in queries {
        let started = Instant::now();
        let (_, report) = retriever.search_with_report(&q.text, &profile)?;
        elapsed_ms.push(started.elapsed().as_secs_f64() as f32 * 1000.0);
        if report.degraded {
            degraded_queries += 1;
        }
    }
    elapsed_ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let at = |q: f64| super::percentile(&elapsed_ms, q).map_or(0.0, |v| round_ms(v as f64));
    Ok(TierLatency {
        tier,
        name: profile.name,
        queries: queries.len(),
        p50_ms: at(0.50),
        p95_ms: at(0.95),
        degraded_queries,
    })
}

fn round_ms(ms: f64) -> f64 {
    (ms * 1000.0).round() / 1000.0
}

pub fn directory_bytes(dir: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        if entry.file_name() == QUERIES_FILE {
            continue;
        }
        let meta = entry.metadata()?;
        if meta.is_file() {
            total += meta.len();
        }
    }
    Ok(total)
}

pub fn peak_rss_bytes() -> u64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if status != 0 {
        return 0;
    }
    let max_rss = unsafe { usage.assume_init() }.ru_maxrss.max(0) as u64;
    if cfg!(target_os = "macos") {
        max_rss
    } else {
        max_rss * 1024
    }
}

struct TemporaryDirectory(PathBuf);

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fresh_temporary_directory() -> TemporaryDirectory {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    TemporaryDirectory(
        std::env::temp_dir().join(format!("br8n-synthetic-{}-{nanos}", std::process::id())),
    )
}

const SPEC_FILE: &str = "bench-synthetic.spec.json";
pub const QUERIES_FILE: &str = "bench-synthetic.queries.json";

fn write_queries(dir: &Path, queries: &[Query]) -> Result<()> {
    let path = dir.join(QUERIES_FILE);
    std::fs::write(&path, serde_json::to_vec(queries)?)
        .with_context(|| format!("write {}", path.display()))
}

fn read_queries(dir: &Path) -> Result<Vec<Query>> {
    let path = dir.join(QUERIES_FILE);
    let bytes = std::fs::read(&path).with_context(|| {
        format!(
            "{} has no {QUERIES_FILE} — it was built by an older binary; rebuild it with \
             `br8n bench --synthetic --out`",
            dir.display()
        )
    })?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct BuildSpec {
    chunks: usize,
    seed: u64,
}

impl BuildSpec {
    fn write(dir: &Path, chunks: usize, seed: u64) -> Result<()> {
        let path = dir.join(SPEC_FILE);
        let json = serde_json::to_string_pretty(&BuildSpec { chunks, seed })?;
        std::fs::write(&path, json).with_context(|| format!("write {}", path.display()))
    }

    fn read(dir: &Path) -> Result<BuildSpec> {
        let path = dir.join(SPEC_FILE);
        let s = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "{} has no {SPEC_FILE} — --reuse only replays a pack `br8n bench \
                 --synthetic --out` wrote itself",
                dir.display()
            )
        })?;
        serde_json::from_str(&s).with_context(|| format!("parse {}", path.display()))
    }
}

fn refuse_unless_empty(out: &Path) -> Result<()> {
    let occupied = match std::fs::read_dir(out) {
        Ok(mut entries) => entries.next().is_some(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e).with_context(|| format!("read {}", out.display())),
    };
    anyhow::ensure!(
        !occupied,
        "{} is not empty; --out writes a fresh index directory and will not overwrite one",
        out.display()
    );
    Ok(())
}

pub fn run(cfg: &Config, chunks: usize, seed: u64, out: Option<&Path>) -> Result<SyntheticReport> {
    anyhow::ensure!(chunks > 0, "--synthetic needs at least one chunk");
    let dims = cfg.embed.dimensions;
    anyhow::ensure!(dims > 0, "embed.dimensions must be positive");
    let model_id = configured_model_id(cfg)?;
    let temporary;
    let dir: &Path = match out {
        Some(dir) => {
            refuse_unless_empty(dir)?;
            dir
        }
        None => {
            temporary = fresh_temporary_directory();
            &temporary.0
        }
    };
    let Corpus { rows, queries } = generate(&Spec { chunks, seed, dims });
    let pack_build_ms = build_pack(dir, &model_id, dims, rows)?;
    BuildSpec::write(dir, chunks, seed)?;
    write_queries(dir, &queries)?;
    let pack_bytes = directory_bytes(dir)?;
    let retriever = hook_retriever(cfg, dir, &model_id, &queries)?;
    let tiers = TIERS
        .iter()
        .map(|&tier| time_tier(&retriever, &queries, tier))
        .collect::<Result<Vec<_>>>()?;
    drop(retriever);
    Ok(SyntheticReport {
        chunks,
        seed,
        dims,
        pack_build_ms: Some(pack_build_ms),
        pack_bytes,
        pack_bytes_per_chunk: pack_bytes / chunks as u64,
        tiers,
        peak_rss_bytes: peak_rss_bytes(),
    })
}

pub fn run_reuse(cfg: &Config, chunks: usize, seed: u64, dir: &Path) -> Result<SyntheticReport> {
    anyhow::ensure!(chunks > 0, "--synthetic needs at least one chunk");
    let dims = cfg.embed.dimensions;
    anyhow::ensure!(dims > 0, "embed.dimensions must be positive");
    let model_id = configured_model_id(cfg)?;
    let recorded = BuildSpec::read(dir)?;
    anyhow::ensure!(
        recorded.chunks == chunks && recorded.seed == seed,
        "{} was built with {} chunks and seed {}, not {chunks} chunks and seed {seed} — \
         --reuse only replays the exact pack a matching --out build produced",
        dir.display(),
        recorded.chunks,
        recorded.seed
    );
    let queries = read_queries(dir)?;
    let retriever = hook_retriever(cfg, dir, &model_id, &queries)?;
    let pack_bytes = directory_bytes(dir)?;
    let tiers = TIERS
        .iter()
        .map(|&tier| time_tier(&retriever, &queries, tier))
        .collect::<Result<Vec<_>>>()?;
    drop(retriever);
    Ok(SyntheticReport {
        chunks,
        seed,
        dims,
        pack_build_ms: None,
        pack_bytes,
        pack_bytes_per_chunk: pack_bytes / chunks as u64,
        tiers,
        peak_rss_bytes: peak_rss_bytes(),
    })
}

pub fn render(report: &SyntheticReport) -> String {
    let pack_build_ms = report
        .pack_build_ms
        .map_or_else(|| "n/a (reused)".to_string(), |ms| format!("{ms} ms"));
    let mut out = format!(
        "synthetic pack: {} chunks, seed {}, {} dims\n\
         pack build   {}\n\
         pack size    {} bytes ({} bytes/chunk)\n\
         peak rss     {} bytes\n\n\
         {:<12} {:>9} {:>9} {:>9}\n",
        report.chunks,
        report.seed,
        report.dims,
        pack_build_ms,
        report.pack_bytes,
        report.pack_bytes_per_chunk,
        report.peak_rss_bytes,
        "tier",
        "p50 ms",
        "p95 ms",
        "degraded"
    );
    for t in &report.tiers {
        out.push_str(&format!(
            "{:<12} {:>9.2} {:>9.2} {:>5}/{}\n",
            format!("{} {}", t.tier, t.name),
            t.p50_ms,
            t.p95_ms,
            t.degraded_queries,
            t.queries
        ));
    }
    out
}
