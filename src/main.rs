use anyhow::Result;
use br8n::config::{Config, Profile, Surface};
use br8n::index::{discover, reindex_swap_with, Indexer, RebuildMode};
use br8n::retrieve::Retriever;
use br8n::store::{StatusSnapshot, Store};
use clap::{Parser, Subcommand};
use sha2::{Digest, Sha256};

#[derive(Parser)]
#[command(name = "br8n", version, about = "Local RAG over your knowledge base")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Index every configured source
    Index {
        /// Delete the existing index and rebuild from scratch
        #[arg(long, conflicts_with = "compact")]
        reindex: bool,
        /// Reclaim space lbug cannot: rebuild the index from its own rows,
        /// reusing every stored embedding. No re-embedding, no Ollama traffic.
        #[arg(long)]
        compact: bool,
        /// Publish immediately with no embeddings at all: BM25 keyword
        /// search works the moment this returns, vector search stays empty
        /// until `--backfill` runs. Phase 1 of an asynchronous index.
        #[arg(long, conflicts_with_all = ["compact", "backfill"])]
        no_embed: bool,
        /// Embed the backlog `--no-embed` (or a previous, interrupted
        /// `--backfill`) left behind, then republish the pack. Runs in short
        /// `IndexLock` slices and yields to prompts between batches — never
        /// holds the store open for the whole run. Phase 2 of an
        /// asynchronous index.
        #[arg(long, conflicts_with_all = ["compact", "reindex", "no_embed"])]
        backfill: bool,
    },
    /// Search the index
    Search {
        query: Vec<String>,
        #[arg(long)]
        quality: Option<u8>,
        #[arg(long)]
        json: bool,
    },
    /// Show index size and configuration
    Status,
    /// Check the embedding endpoint and the index agree
    Doctor,
    /// Add a single URL or file to the index
    Add { target: String },
    /// Measure recall and latency at every quality tier
    Bench {
        #[arg(long)]
        no_graph: bool,
        #[arg(
            long,
            value_name = "CHUNKS",
            conflicts_with = "no_graph",
            help = "Build a synthetic pack of this many chunks and time it, without Ollama"
        )]
        synthetic: Option<usize>,
        #[arg(long, default_value_t = 0, requires = "synthetic")]
        seed: u64,
        #[arg(
            long,
            value_name = "DIR",
            requires = "synthetic",
            conflicts_with = "reuse",
            help = "Leave the synthetic index here, laid out as a live index directory"
        )]
        out: Option<std::path::PathBuf>,
        #[arg(
            long,
            value_name = "DIR",
            requires = "synthetic",
            conflicts_with = "out",
            help = "Skip the build and query an index a matching --out already wrote at the same chunks and seed"
        )]
        reuse: Option<std::path::PathBuf>,
        #[arg(long, requires = "synthetic")]
        json: bool,
    },
    /// Read, check and change config.toml without losing its comments
    Config {
        #[command(subcommand)]
        what: ConfigCmd,
    },
    /// Build and check the golden set `br8n bench` measures against
    Golden {
        #[command(subcommand)]
        what: GoldenCmd,
    },
    /// Remember, list and forget lessons, facts and episodes
    Memory {
        #[command(subcommand)]
        what: MemoryCmd,
    },
    /// Read the hook's own injection ledger: what it really put in prompts.
    ///
    /// `br8n bench` measures retrieval against a golden set of vault notes.
    /// This measures what the hook ACTUALLY injected, which is a different
    /// question and, as of this writing, a very different answer.
    AuditInjections {
        /// Claude Code's transcript root. Defaults to ~/.claude/projects.
        #[arg(long)]
        root: Option<std::path::PathBuf>,
    },
    /// Run the MCP server on stdio
    Mcp,
    /// Internal: hook entrypoints
    Hook {
        #[arg(value_parser = ["prompt", "session-start", "load"])]
        which: String,
        /// Which agent's hook envelope to read and write
        #[arg(long, value_parser = ["claude-code", "codex", "gemini"], default_value = "claude-code")]
        agent: String,
    },
    /// Serve the local web dashboard (graph, search, health)
    Dashboard {
        /// Port to bind (default: OS-assigned)
        #[arg(long)]
        port: Option<u16>,
        /// Do not open the browser
        #[arg(long)]
        no_open: bool,
    },
    /// List the coding agents br8n can connect to, and whether each is connected
    Agents {
        /// Print the listing as JSON
        #[arg(long)]
        json: bool,
    },
    /// Connect br8n to one or more agents: claude-code, codex, claude-desktop, cursor, gemini
    Connect {
        agents: Vec<String>,
        /// Codex only: also add a marked guidance block to Codex's global AGENTS.md
        #[arg(long)]
        instructions: bool,
        /// Connect every agent found on this machine
        #[arg(long, conflicts_with = "agents")]
        all_detected: bool,
        /// Print a config snippet instead of writing anything: an agent id, `mcp-json` or `codex-toml`
        #[arg(long, value_name = "TARGET", conflicts_with_all = ["agents", "all_detected", "instructions"])]
        print: Option<String>,
    },
    /// Remove br8n from one or more agents' configuration
    Disconnect {
        #[arg(required = true)]
        agents: Vec<String>,
    },
    /// Install br8n into its own directory, register it with Claude Code, and offer other agents
    Install {
        /// Answer yes to every prompt (the model pull)
        #[arg(long)]
        yes: bool,
        /// Print failures only
        #[arg(long)]
        quiet: bool,
    },
    /// Remove br8n: the binary, the plugin, the PATH link, and every agent connection
    Uninstall {
        /// Also remove the index and the config
        #[arg(long)]
        purge: bool,
        /// Do not ask before purging
        #[arg(long)]
        yes: bool,
    },
    /// Fetch the latest release, verify it, and let it install itself
    Update {
        /// Only report whether a newer version exists
        #[arg(long)]
        check: bool,
        /// Accepted for scripts; the update never prompts
        #[arg(long)]
        yes: bool,
        /// Print failures only
        #[arg(long)]
        quiet: bool,
    },
    /// Back up the config, the golden set, and the index; no subcommand runs one backup
    #[cfg(feature = "backup")]
    Backup {
        #[command(subcommand)]
        action: Option<BackupAction>,
    },
    /// Restore the config and golden set, and optionally the index, from a backup
    #[cfg(feature = "backup")]
    Restore {
        /// A generation key, e.g. `manifest/2026-08-25T03-00-00.000Z.json`; defaults to the newest
        #[arg(long)]
        generation: Option<String>,
        /// Also restore the index; refused if it was built with a different model, width, or chunk size
        #[arg(long)]
        index: bool,
        /// Print what would be written and write nothing
        #[arg(long)]
        dry_run: bool,
        /// Overwrite files that already exist locally
        #[arg(long)]
        force: bool,
    },
}

#[cfg(feature = "backup")]
#[derive(Subcommand)]
enum BackupAction {
    /// Generate the encryption key
    Init {
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    /// Validate credentials and reachability the way cron will see them
    Check,
    /// Authorize Google Drive once, interactively, and create the backup folder
    Auth {
        #[arg(value_parser = ["drive"])]
        provider: String,
    },
    /// Show the last success, the targets, and how many generations each holds
    Status,
    /// Install or remove the crontab entry
    Schedule {
        /// A cron schedule
        #[arg(long, default_value = "0 13 * * *")]
        at: String,
        /// Remove the entry instead of installing it
        #[arg(long)]
        uninstall: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Report every problem in config.toml; exit non-zero if there are any
    Check,
    /// Print the effective value of a dotted key, e.g. `hook.threshold`
    Get { key: String },
    /// Set a dotted key; the value is read as TOML, or as a string if it is not TOML
    Set { key: String, value: String },
    /// Remove a dotted key from config.toml so its default applies again
    Unset { key: String },
    /// Print the path of config.toml
    Path,
}

#[derive(Subcommand)]
enum GoldenCmd {
    /// Write a starter golden set, seeded with real documents from your index
    Init {
        /// Overwrite an existing golden set. It may be your only copy.
        #[arg(long)]
        force: bool,
    },
    /// Report every problem in the golden set; exit non-zero if any are errors
    Check,
}

#[derive(Subcommand)]
enum MemoryCmd {
    /// Save a memory. Text is the remaining arguments.
    Add {
        /// lesson, fact or episode
        #[arg(long, default_value = "lesson")]
        kind: String,
        /// Scope to this project directory (default: the current directory)
        #[arg(long, conflicts_with = "global")]
        project: Option<std::path::PathBuf>,
        /// Apply everywhere, not only in the current project
        #[arg(long)]
        global: bool,
        #[arg(long)]
        title: Option<String>,
        text: Vec<String>,
    },
    /// List memories, newest first
    List {
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        project: Option<std::path::PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Delete a memory by id (a unique prefix is enough)
    Forget { id: String },
    /// Distill episodes from finished session transcripts
    Distill {
        /// Drain every pending session, not just one
        #[arg(long)]
        all: bool,
        /// Distill exactly this transcript
        #[arg(long)]
        session: Option<std::path::PathBuf>,
    },
    /// Re-embed every memory with the configured model and republish
    Rebuild,
    /// Write every memory as JSON lines
    Export {
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    /// Read JSON lines written by export and save each as a memory
    Import { file: std::path::PathBuf },
}

/// Render the progress file written by a running index, if one is active.
///
/// Returns `None` when no run is in progress, and also when the file is stale:
/// a killed indexer cannot clean up after itself, so a progress file whose
/// process is gone must not be reported as live work.
fn read_progress(path: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let num = |k: &str| -> f64 {
        raw.split(&format!("\"{k}\":"))
            .nth(1)
            .and_then(|s| {
                s.split(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
                    .find(|t| !t.is_empty())
            })
            .and_then(|t| t.parse().ok())
            .unwrap_or(0.0)
    };
    let pid = num("pid") as u32;
    if pid != 0 && !br8n::index::pid_is_alive(pid) {
        let _ = std::fs::remove_file(path);
        return None;
    }
    let mins = |s: f64| format!("{}m{:02}s", (s as u64) / 60, (s as u64) % 60);
    Some(format!(
        "{:.1}%  {}/{} docs  {} chunks  {} elapsed  {} left",
        num("pct"),
        num("docs_done") as u64,
        num("docs_total") as u64,
        num("chunks") as u64,
        mins(num("elapsed_s")),
        mins(num("eta_s")),
    ))
}

fn confirm_on_stdin(question: &str) -> bool {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return false;
    }
    eprint!("? {question} [y/N] ");
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim(), "y" | "Y" | "yes" | "YES")
}

fn run_agents(json: bool) -> Result<()> {
    use br8n::setup::agents;
    let env = agents::AgentEnv::from_env();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&agents::api::list(&env))?
        );
        return Ok(());
    }
    println!(
        "{:<16} {:<22} {:<16} CAPABILITIES",
        "AGENT", "DETECTED", "STATUS"
    );
    let mut reasons = Vec::new();
    for agent in agents::all() {
        let detected = agent.detect(&env);
        let status = agent.status(&env);
        let seen = match (&detected.installed, &detected.version) {
            (true, Some(v)) => format!("yes ({})", v.split_whitespace().next().unwrap_or(v)),
            (true, None) => "yes".to_string(),
            (false, _) => "no".to_string(),
        };
        println!(
            "{:<16} {:<22} {:<16} {}",
            agent.id(),
            seen,
            status.state().replace('_', " "),
            agent.capabilities().names().join(", ")
        );
        if let Some(r) = status.reason() {
            reasons.push(format!("  {}: {r}", agent.id()));
        }
    }
    for r in reasons {
        println!("{r}");
    }
    Ok(())
}

fn run_connect(
    ids: Vec<String>,
    instructions: bool,
    all_detected: bool,
    print: Option<String>,
) -> Result<()> {
    use br8n::setup::agents;
    let env = agents::AgentEnv::from_env();
    if let Some(target) = print {
        let text = match target.as_str() {
            "mcp-json" => agents::mcp_json_snippet(&env),
            "codex-toml" => agents::codex_toml_snippet(&env),
            id => agents::find(id)?.snippet(&env).unwrap_or_default(),
        };
        print!("{text}");
        return Ok(());
    }
    let chosen: Vec<Box<dyn agents::Agent>> = if all_detected {
        agents::all()
            .into_iter()
            .filter(|a| a.detect(&env).installed)
            .collect()
    } else if ids.is_empty() {
        anyhow::bail!(
            "name at least one agent ({}), or pass --all-detected",
            agents::ids().join(", ")
        );
    } else {
        ids.iter()
            .map(|id| agents::find(id))
            .collect::<Result<_>>()?
    };
    let mut failed = 0;
    for agent in &chosen {
        let opts = agents::ConnectOptions {
            instructions: instructions && agent.capabilities().instructions,
        };
        if instructions && !agent.capabilities().instructions && !all_detected {
            eprintln!(
                "! {}: --instructions applies to codex only; ignored",
                agent.id()
            );
        }
        match agents::connect(agent.as_ref(), &env, &opts) {
            Ok(change) => println!("{}", agents::describe(agent.as_ref(), &change)),
            Err(e) => {
                failed += 1;
                eprintln!("! {}: {e:#}", agent.id());
            }
        }
    }
    if chosen.is_empty() {
        println!("no agents detected");
    }
    if failed > 0 {
        anyhow::bail!("{failed} of {} agents could not be connected", chosen.len());
    }
    Ok(())
}

fn run_disconnect(ids: Vec<String>) -> Result<()> {
    use br8n::setup::agents;
    let env = agents::AgentEnv::from_env();
    let chosen = ids
        .iter()
        .map(|id| agents::find(id))
        .collect::<Result<Vec<_>>>()?;
    let mut failed = 0;
    for agent in &chosen {
        match agent.disconnect(&env) {
            Ok(change) if change.is_empty() => {
                println!("{}: not connected, nothing changed", agent.id())
            }
            Ok(change) => {
                let mut parts: Vec<String> = change
                    .files
                    .iter()
                    .map(|f| format!("updated {}", f.display()))
                    .collect();
                parts.extend(change.notes.iter().cloned());
                println!("{}: disconnected ({})", agent.id(), parts.join("; "));
            }
            Err(e) => {
                failed += 1;
                eprintln!("! {}: {e:#}", agent.id());
            }
        }
    }
    if failed > 0 {
        anyhow::bail!(
            "{failed} of {} agents could not be disconnected",
            chosen.len()
        );
    }
    Ok(())
}

fn required_models(cfg: &Config) -> Vec<String> {
    let mut m = Vec::new();
    if cfg.embed.remote.is_none() && cfg.embed.remote_error.is_none() {
        m.push(cfg.embed.model.clone());
    }
    if cfg.embed.contextual {
        m.push(cfg.embed.enrich_model.clone());
    }
    m
}

fn sha256_file(path: &std::path::Path) -> std::io::Result<String> {
    let mut f = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h)?;
    Ok(hex::encode(h.finalize()))
}

pub fn build_indexer(cfg: &Config) -> Result<Indexer> {
    let store = Store::open(&Config::db_path(), cfg.embed.dimensions)?;
    Ok(Indexer::new(
        store,
        br8n::embed::for_config(&cfg.embed)?,
        cfg.clone(),
    ))
}

/// `br8n search` is ungated, so weights only reorder here and the global
/// `[weights]` table — the one whose whole job is ordering — is the right one.
/// A surface's overrides exist to move its GATE; borrowing them here would make
/// the CLI rank differently from the hook for no reason a user asked for.
///
/// `profile` is the tier this call is actually about to search at — `Cmd::Search`
/// computes it (`--quality`, or the MCP surface's default) before calling this —
/// not a fixed surface default, so the store-vs-pack decision below is made
/// against the tier that will really run, the same distinction `retrieve_for_profile`
/// draws against `retrieve_for` in `lib.rs`.
pub fn build_retriever(cfg: &Config, profile: &Profile) -> Result<Retriever> {
    let db = Config::db_path();
    let embedder = br8n::embed::for_config(&cfg.embed)?;
    let model_id = embedder.model_id();
    // Same lookup `retrieve_for` uses for every other surface (the hook, the
    // dashboard, the MCP tool, `br8n bench`): a missing pack degrades to the
    // store, an invalid one refuses. Without this, `br8n search` — the tool
    // reached for to debug "why didn't the hook inject X" — queried a
    // different backend than the hook it is meant to be debugging.
    let pack = br8n::pack::open_pack_beside(&db, &model_id, cfg.embed.dimensions)?;
    // RE-ENABLED, reversing the MEASURED HOLD this comment used to describe.
    //
    // On the 100-case golden set, same corpus, quiet machine, bench repeatable
    // to 0.01, the pack loses recall at every tier where it matters:
    //
    //     tier         store   pack    latency
    //     instant      0.52    0.42    -48%
    //     fast (hook)  0.83    0.76    -17%
    //     balanced     0.85    0.80    -17%
    //     thorough     0.86    0.84    -22%
    //     exhaustive   0.87    0.87    -24%
    //
    // Seven cases in a hundred at the hook's tier. That table has not been
    // re-measured since; task 7 re-benches it and returns the decision.
    // Re-enabling here is necessary for that measurement to be possible, not
    // a verdict that the trade is now good — see `retrieve_for` in `lib.rs`
    // for the fuller account of why stage 1b changes that verdict.
    //
    // `profile` IS the single profile `--quality` is about to search at (the
    // caller computed it before calling this), so the same store-vs-pack
    // decision `retrieve_for_profile` makes for the hook and the dashboard
    // applies here too: a tier that needs no graph expansion reads the pack
    // alone. `br8n search`'s weights are the ungated global table (see the
    // doc comment above), not a surface's override, so authority is read from
    // `cfg.weights` directly rather than through `weights_for`.
    //
    // Authority no longer forces the store open: `pack.links` carries the
    // inbound counts (see `retrieve_for_profile` in `lib.rs` for the measured
    // cost it used to add). With NO pack the counts are still store-only, so
    // the store must still open for it — `&& pack.is_none()`, not a dropped
    // clause.
    let needs_store = profile.graph.is_some() || (cfg.weights.authority > 0.0 && pack.is_none());

    let retriever = if !needs_store {
        match pack {
            // The store-free path this decision exists for: no `Database::new`,
            // no `LOAD EXTENSION`, nothing but the mmap'd pack.
            Some(pk) => Retriever::packed(pk, embedder, cfg.embed.ollama_url.clone()),
            // No pack validated (an index built before packs existed, or one
            // that failed validation above would already have returned `Err`)
            // — vector/bm25/measure still need somewhere to read from.
            None => {
                let store = Store::open_existing(&db, cfg.embed.dimensions)?;
                Retriever::new(store, embedder, cfg.embed.ollama_url.clone())
            }
        }
    } else {
        // Graph expansion, or authority weighting with no pack to read the
        // counts from: the store opens regardless. The pack is still attached
        // when present — vector/bm25/measure read it either way.
        let store = Store::open_existing(&db, cfg.embed.dimensions)?;
        Retriever::new(store, embedder, cfg.embed.ollama_url.clone()).with_pack(pack)
    };

    let memory = br8n::memory::open_pack(cfg, &model_id);
    Ok(retriever
        .with_memory(memory, cfg.memory.clone())
        .with_weights(cfg.weights.clone()))
}

/// One summary line naming titles/filename stems shared by more than one
/// document — those wikilinks resolve to nothing (ambiguous fails closed, same
/// as unresolvable). Printed once after the run, not per document: ambiguous
/// keys are bounded by duplicate titles, not corpus size, so a per-document
/// `eprintln!` would be silent 99% of the time and spammy the other 1%.
fn print_ambiguous_titles(idx: &Indexer) {
    if let Ok(ambiguous) = idx.ambiguous_titles() {
        if !ambiguous.is_empty() {
            println!(
                "ambiguous wikilink targets (unresolved): {}",
                ambiguous.join(", ")
            );
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::load();

    // Before the match, because every arm below can reach a loader and some of
    // them spawn threads: `Cmd::Mcp` builds a multi-thread runtime and
    // `Cmd::Hook` spawns the embedder warm-up. `std::env::set_var` is only
    // sound while this process is single-threaded, and this is the last point
    // at which it is. See `resolve_ocr_libraries`.
    br8n::loaders::pdf::resolve_ocr_libraries(&cfg.pdf);

    match cli.cmd {
        Cmd::Index {
            reindex,
            compact,
            no_embed,
            backfill,
        } => {
            br8n::index::lower_priority();
            if backfill {
                // Phase 2: drain whatever `--no-embed` (or a previous,
                // interrupted `--backfill`) left behind. Runs to completion —
                // an unbounded budget — because this is a direct, foreground
                // request; it still yields `IndexLock` and the store itself
                // between short batches the whole way, so it never blocks a
                // concurrent reader or writer for its own duration. See
                // `index::backfill_vectors`.
                let stats = br8n::index::backfill_vectors(&cfg, std::time::Duration::MAX)?;
                println!(
                    "backfilled: {} embedded, {} still pending{}",
                    stats.embedded,
                    stats.remaining,
                    if stats.republished {
                        " (pack republished)"
                    } else {
                        ""
                    }
                );
                return Ok(());
            }

            // Builds into a shadow directory and swaps it in with a rename, so
            // the hook (a separate process) never sees the database mid-write.
            // LadybugDB takes an exclusive OS file lock, so indexing in place
            // would leave the hook returning nothing, silently, for the whole
            // run. See `index::reindex_swap`.
            //
            // `--reindex` used to `remove_dir_all` the live database HERE,
            // before `reindex_swap` took the writer lock — deleting the
            // directory out from under a concurrent indexer mid-swap. That is
            // the same race the lock exists to close, moved one line earlier.
            // It is now expressed as "do not seed the shadow from the live
            // index", which needs no wipe at all and happens under the lock.
            let mode = if compact {
                RebuildMode::Compact
            } else if reindex {
                RebuildMode::FromScratch
            } else {
                RebuildMode::Incremental
            };
            // `graph.kz` is what actually holds the space lbug cannot
            // reclaim, so the before/after line compaction reports is
            // measured against the whole store directory on disk, not any
            // in-memory estimate.
            let before_bytes = compact.then(|| br8n::index::dir_size(&Config::db_path()));
            let stats = reindex_swap_with(&cfg, mode, !no_embed)?;
            println!(
                "indexed: {} added, {} updated, {} skipped, {} chunks \
                 ({} written, {} reused, {} pruned)",
                stats.added,
                stats.updated,
                stats.skipped,
                stats.chunks,
                stats.chunks_written,
                stats.chunks_reused,
                stats.chunks_pruned
            );
            if no_embed {
                println!(
                    "note: published with no embeddings; run `br8n index --backfill` \
                     to fill them in"
                );
            }
            if let Some(before) = before_bytes {
                let after = br8n::index::dir_size(&Config::db_path());
                println!(
                    "compacted: {:.1} MB -> {:.1} MB",
                    before as f64 / 1_048_576.0,
                    after as f64 / 1_048_576.0
                );
            }
            if cfg.index_transcripts
                && cfg.memory.enabled
                && cfg.memory.distill_episodes
                && !no_embed
            {
                match br8n::memory::distill::distill_pending(&cfg, 1, true) {
                    Ok(r) if r.skipped_idle => eprintln!(
                        "br8n: distillation skipped — a prompt was seen in the last {}s",
                        cfg.memory.distill_idle_secs
                    ),
                    Ok(r) if r.distilled > 0 => eprintln!(
                        "br8n: distilled {} session(s), {} pending",
                        r.distilled,
                        r.candidates - r.distilled
                    ),
                    Ok(_) => {}
                    Err(e) => eprintln!("br8n: distillation failed — {e:#}"),
                }
            }
        }

        Cmd::Search {
            query,
            quality,
            json,
        } => {
            let q = query.join(" ");
            let profile = quality
                .map(Profile::tier)
                .unwrap_or_else(|| cfg.profile_for(Surface::Mcp));
            // A search that cannot reach the index prints nothing on stdout —
            // stdout is the result channel and `--json` must stay parseable —
            // but it SAYS SO on stderr and exits 0.
            //
            // This swallowed every error silently, which made the one command a
            // user reaches for to ask "why didn't the hook inject X" answer
            // "nothing found" when the real answer was a refused pack. An
            // end-to-end test caught it: corrupting the pack's analyzer
            // produced an empty result and no message, which is exactly the
            // shape of an honest no-match.
            //
            // `search_with_report` rather than `search`, for the same reason:
            // a store-only index (no pack) at a profile that asks for BM25
            // hits `Store::fts_search`, which always errors now that lbug's
            // FTS index is gone — the pipeline degrades to vector-only rather
            // than failing, and this is the same command whose whole purpose
            // is answering "why didn't the hook inject X". Silently returning
            // vector-only results here would hide the answer from the one
            // place a user would look for it.
            let hits = match build_retriever(&cfg, &profile)
                .and_then(|r| r.search_with_report(&q, &profile))
            {
                Ok((hits, report)) => {
                    // The DEADLINE degradation, and this command is the one
                    // that most needs it. `hook::run_prompt` has printed this
                    // since it was written; `br8n search` did not, and the
                    // asymmetry was backwards: the hook's user sees a prompt
                    // that quietly lacked a document, while THIS command's
                    // whole purpose (see the comment above) is answering "why
                    // didn't the hook inject X". A silent stage skip here made
                    // the diagnostic tool hide the diagnosis.
                    //
                    // Measured on the live index, tier 1, one query over 14
                    // runs: the single run that took 384ms — past the 220ms
                    // budget — returned a result set missing a document the
                    // other 13 all found, with nothing printed. It reads as
                    // run-to-run nondeterminism in retrieval, which is the
                    // wrong place to go looking.
                    if report.degraded {
                        eprintln!(
                            "br8n: retrieval degraded — budget {}ms exceeded, ran only [{}]",
                            profile.budget_ms,
                            report.stages_run.join("+")
                        );
                    }
                    if let Some(reason) = &report.bm25_unavailable {
                        eprintln!(
                            "br8n: retrieval degraded — bm25 unavailable, ran vector-only ({reason})"
                        );
                    }
                    // Mirror image of the check above — see
                    // `StageReport::vectors_unavailable`.
                    if let Some(reason) = &report.vectors_unavailable {
                        eprintln!(
                            "br8n: retrieval degraded — vectors unavailable, ran bm25-only ({reason})"
                        );
                    }
                    if let Some(reason) = &report.memory_unavailable {
                        eprintln!("br8n: memory unavailable — {reason}; run `br8n memory rebuild`");
                    }
                    hits
                }
                Err(e) => {
                    eprintln!("br8n: search unavailable — {e:#}");
                    Vec::new()
                }
            };

            if json {
                let results: Vec<serde_json::Value> = hits
                    .iter()
                    .map(|h| {
                        serde_json::json!({
                            "uri": h.uri, "title": h.title, "heading": h.heading_path,
                            "page": h.page_no,
                            // Both, explicitly named. `relevance` is the [0,1]
                            // cosine a human or a model can interpret; `score`
                            // only orders, and after reranking it is a binary
                            // verdict. Emitting `score` alone under that name
                            // made every result at tier 3 read as 1.000.
                            "relevance": h.relevance, "score": h.score,
                            "text": h.text,
                        })
                    })
                    .collect();
                println!("{}", serde_json::json!({ "results": results }));
            } else if hits.is_empty() {
                println!("no results");
            } else {
                for h in &hits {
                    // `relevance`, not `score`: CLI search runs at the MCP tier,
                    // which reranks, and a reranked `score` is 1.0 or 0.0 — so
                    // every hit printed as (1.000), including irrelevant ones.
                    println!("\n\x1b[1m{}\x1b[0m  ({:.3})", h.title, h.relevance);
                    if !h.heading_path.is_empty() {
                        println!("  \x1b[2m{}\x1b[0m", h.heading_path);
                    }
                    println!("  \x1b[2m{}\x1b[0m", h.uri);
                    println!("  {}", h.text.chars().take(300).collect::<String>());
                }
            }
        }

        Cmd::Doctor => {
            if !br8n::doctor::run(&cfg)? {
                std::process::exit(1);
            }
        }
        Cmd::Status => {
            let cfg_path = Config::config_path();
            let db = Config::db_path();
            let opened = Store::open_existing(&db, cfg.embed.dimensions);
            // A missing index (fresh machine, `br8n index` never run) is a
            // normal empty state — `open_existing`'s own "no index at ..."
            // error names it. Any other failure to open — most often
            // `LOAD EXTENSION vector` refusing to load — is a broken build,
            // and must say so on both stderr and in the status line itself.
            // Rendering it as "documents: 0" would read as an empty knowledge
            // base instead of a link/toolchain problem the user needs to fix.
            let store_broken = match &opened {
                Ok(_) => false,
                Err(e) if e.to_string().contains("no index at") => false,
                Err(e) => {
                    eprintln!("br8n: status — the store could not be opened: {e:#}");
                    true
                }
            };
            let snap = opened
                .map(|s| s.status_snapshot())
                .unwrap_or(StatusSnapshot {
                    documents: 0,
                    chunks: 0,
                    model: None,
                    skipped: Vec::new(),
                    vectors_pending: 0,
                });
            let skipped = snap.skipped;
            let paths = br8n::setup::Paths::from_env();
            let installed = env!("CARGO_PKG_VERSION");
            println!(
                "version:    {}",
                br8n::update::version_line(
                    installed,
                    br8n::update::UpdateCheck::read(&paths.update_json).as_ref(),
                    br8n::update::Status::running(&paths.update_status).as_ref(),
                    br8n::update::now(),
                )
            );
            let checks = br8n::setup::install::install_checks(
                &paths,
                &br8n::setup::claude::ClaudeCli::from_path(),
                installed,
                &std::env::current_exe().unwrap_or_default(),
            );
            let failed: Vec<_> = checks.iter().filter(|c| !c.ok).collect();
            if failed.is_empty() {
                println!("install:    {}  ok", paths.root.display());
            } else {
                println!(
                    "install:    {}  {} problem(s) — run br8n install",
                    paths.root.display(),
                    failed.len()
                );
                for c in failed {
                    println!("  - {}: {}", c.name, c.detail);
                }
            }
            println!("config:     {}", cfg_path.display());
            let config_errors = br8n::config::edit::read_document(&cfg_path)
                .ok()
                .flatten()
                .map(|text| br8n::config::check::check(&text))
                .unwrap_or_default();
            if !config_errors.is_empty() {
                println!(
                    "            {} problem(s) — run br8n config check; the hook ignores what it cannot read",
                    config_errors.len()
                );
                for e in &config_errors {
                    println!("  - {e}");
                }
            }
            println!("database:   {}", db.display());
            if store_broken {
                println!(
                    "store:      UNAVAILABLE — could not open (see stderr); counts below are not meaningful"
                );
            }
            println!("documents:  {}", snap.documents);
            match br8n::memory::counts_at(&br8n::memory::default_root()) {
                Ok(counts) => {
                    let mut kinds: Vec<String> = counts
                        .iter()
                        .map(|(k, n)| {
                            format!("{n} {}{}", k.as_str(), if *n == 1 { "" } else { "s" })
                        })
                        .collect();
                    kinds.sort();
                    if kinds.is_empty() {
                        println!("memory:     none");
                    } else {
                        println!("memory:     {}", kinds.join(", "));
                    }
                }
                Err(e) => {
                    eprintln!("br8n: status — the memory store could not be read: {e:#}");
                    println!(
                        "memory:     UNAVAILABLE — could not read the memory store (see stderr)"
                    );
                }
            }
            // A half-embedded index that presents as complete is a worse
            // outcome than the slow synchronous index this task replaces, so
            // the backlog is always named explicitly rather than folded
            // silently into the chunk count.
            if snap.vectors_pending > 0 {
                println!(
                    "chunks:     {} ({} vectors pending — run `br8n index --backfill`)",
                    snap.chunks, snap.vectors_pending
                );
            } else {
                println!("chunks:     {}", snap.chunks);
            }
            let db = Config::db_path();
            match br8n::usage::load(&db) {
                Ok(map) => println!(
                    "usage:      {} documents retrieved, {} records waiting for the next index",
                    map.len(),
                    br8n::usage::pending(&db)
                ),
                Err(e) => println!("usage:      UNAVAILABLE — {e:#}"),
            }
            println!(
                "model:      {} (configured: {})",
                snap.model.as_deref().unwrap_or("-"),
                cfg.embed.model
            );
            match (&cfg.embed.remote, &cfg.embed.remote_error) {
                (_, Some(e)) => println!("embed:      MISCONFIGURED — {e}"),
                (Some(r), None) => {
                    println!("embed:      {} (model {}; ollama bypassed)", r.url, r.model)
                }
                (None, None) => println!("embed:      {} (ollama)", cfg.embed.ollama_url),
            }
            let hq = cfg.quality_for(Surface::Hook);
            let mq = cfg.quality_for(Surface::Mcp);
            println!("hook:       tier {hq} ({})", Profile::tier(hq).name);
            println!("mcp:        tier {mq} ({})", Profile::tier(mq).name);
            println!("sources:    {}", cfg.sources.len());
            #[cfg(feature = "backup")]
            if br8n::backup::is_configured(&cfg) {
                println!(
                    "backup:     {} ({})",
                    br8n::backup::describe_age(br8n::backup::read_stamp(&db)),
                    cfg.backup.targets.join(", ")
                );
            }

            let ollama_models = required_models(&cfg);
            if !ollama_models.is_empty() {
                match br8n::setup::ollama::probe(&cfg.embed.ollama_url, &ollama_models) {
                    br8n::setup::ollama::OllamaState::Unreachable(why) => println!(
                        "ollama:     unreachable at {} — start it with `ollama serve` ({why})",
                        cfg.embed.ollama_url
                    ),
                    br8n::setup::ollama::OllamaState::Reachable { missing }
                        if missing.is_empty() =>
                    {
                        println!(
                            "ollama:     reachable at {}, model(s) present",
                            cfg.embed.ollama_url
                        )
                    }
                    br8n::setup::ollama::OllamaState::Reachable { missing } => println!(
                        "ollama:     reachable, missing model(s): {} — run: {}",
                        missing.join(", "),
                        missing
                            .iter()
                            .map(|m| format!("ollama pull {m}"))
                            .collect::<Vec<_>>()
                            .join(" && ")
                    ),
                }
            }

            // Scanned PDFs are read only if two external libraries are present,
            // and neither is discoverable by default on macOS. Reporting them
            // here is the difference between "OCR is on" and OCR actually
            // having a chance to run. Location only — a library that is found
            // can still fail to load, so this never claims OCR works.
            match cfg.pdf.ocr {
                br8n::config::OcrMode::Off => println!("ocr:        off"),
                mode => {
                    let libs = br8n::loaders::pdf::ocr_library_paths(&cfg.pdf);
                    let missing: Vec<&str> = libs
                        .iter()
                        .filter(|(_, path)| path.is_none())
                        .map(|(name, _)| *name)
                        .collect();
                    let mode = if matches!(mode, br8n::config::OcrMode::Force) {
                        "force"
                    } else {
                        "auto"
                    };
                    // Reported beside the library paths for the same reason
                    // they are: an offline install that silently tried the
                    // network, or a locked-down one that silently did not,
                    // both look like "OCR did nothing".
                    //
                    // UNCONDITIONAL, symmetric with the `ocr:` line below.
                    // Printing only the offline case meant a user who had
                    // explicitly set `if-missing` saw exactly what a user who
                    // had set nothing saw, so the setting could not be
                    // confirmed to have taken effect from the tool's own
                    // output — which is the shape of thing this project's
                    // failure culture exists to refuse.
                    println!(
                        "ocr models: {}",
                        match cfg.pdf.model_downloads {
                            br8n::config::ModelDownloads::Offline => "offline (never downloaded)",
                            br8n::config::ModelDownloads::IfMissing =>
                                "if-missing (downloaded on first use)",
                        }
                    );
                    if missing.is_empty() {
                        println!("ocr:        {mode} (libraries found)");
                    } else {
                        println!(
                            "ocr:        {mode} — NOT available, missing {}",
                            missing.join(" and ")
                        );
                    }
                }
            }

            // A run in progress. The SessionStart indexer is detached and its
            // stderr goes to a log file, so without this there is no way to ask
            // how far a background index has got — or whether one is running.
            if let Some(p) = read_progress(&db.with_extension("progress")) {
                println!("indexing:   {p}");
            } else if br8n::index::IndexLock::is_held(&db) {
                println!("indexing:   starting…");
            }
            // The last run's skips. Without this the only report was an
            // `eprintln!` from a process whose stderr goes to a log nobody
            // opens, so a scanned PDF vanished from search with no explanation.
            if skipped.is_empty() {
                println!("skipped:    none");
            } else {
                println!("skipped:    {} (not indexed)", skipped.len());
                for line in skipped.iter().take(10) {
                    println!("  - {line}");
                }
                if skipped.len() > 10 {
                    println!("  … and {} more", skipped.len() - 10);
                }
            }

            match std::env::current_exe().and_then(|p| sha256_file(&p)) {
                Ok(h) => println!("build:      {h}"),
                Err(e) => {
                    println!("build:      unknown (could not read this binary's own bytes: {e})")
                }
            }
            match std::fs::metadata(&paths.bin) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!(
                    "hook binary:{} — not installed yet (run `br8n install`)",
                    paths.bin.display()
                ),
                Err(e) => println!("hook binary:{} — could not read ({e})", paths.bin.display()),
                Ok(_) => {
                    let theirs = match sha256_file(&paths.bin) {
                        Ok(h) => h,
                        Err(e) => {
                            println!("hook binary:{} — could not read ({e})", paths.bin.display());
                            return Ok(());
                        }
                    };
                    match std::env::current_exe() {
                        Err(e) => println!(
                            "hook binary:{} — could not determine this binary's own path to compare ({e})",
                            paths.bin.display()
                        ),
                        Ok(mine_path) => match sha256_file(&mine_path) {
                            Err(e) => println!(
                                "hook binary:{} — could not read this binary's own bytes to compare ({e})",
                                paths.bin.display()
                            ),
                            Ok(mine) => {
                                let verdict = if theirs == mine {
                                    "same as this binary"
                                } else {
                                    "MISMATCH — the hook runs a different build"
                                };
                                println!("hook binary:{} — {verdict}", paths.bin.display());
                                if theirs != mine {
                                    println!(
                                        "            fix: rm -f {} && cp {} {}",
                                        paths.bin.display(),
                                        mine_path.display(),
                                        paths.bin.display()
                                    );
                                }
                            }
                        },
                    }
                }
            }
        }

        Cmd::Add { target } => {
            let doc = if target.starts_with("http") {
                br8n::loaders::web::WebLoader::fetch(&target)?
            } else {
                let p = std::path::Path::new(&target);
                if p.extension().and_then(|e| e.to_str()) == Some("pdf") {
                    br8n::loaders::pdf::PdfLoader::load_file_with(p, &cfg.pdf)?
                } else {
                    br8n::loaders::markdown::MarkdownLoader::load_file(p)?
                }
            };
            // `add` writes straight into the live database, so it must hold
            // the SAME lock `reindex_swap` takes. Without it, an add landing
            // after the swap copied live -> shadow was written to a database
            // about to be renamed away: the document vanished with no error.
            // `SessionStart` spawns an indexer every session, so that race is
            // routine, not theoretical.
            let _lock = br8n::index::IndexLock::acquire(&Config::db_path()).ok_or_else(|| {
                anyhow::anyhow!("`br8n index` is running; try again once it finishes")
            })?;
            let idx = build_indexer(&cfg)?;
            let stats = idx.index_documents(std::slice::from_ref(&doc))?;

            // Resolve against the WHOLE corpus, not just this document.
            // `upsert_document` DETACH DELETEs the node, which drops INBOUND edges
            // too — so re-adding a note that other notes link TO severs their links.
            // Measured: A->B was 1, dropped to 0 after `br8n add` of B, and only
            // returned after the next full index. Passing every document lets those
            // inbound links be rebuilt immediately.
            let all = discover(&cfg).unwrap_or_default();
            let mut corpus = all;
            if !corpus.iter().any(|d| d.id == doc.id) {
                corpus.push(doc.clone());
            }
            idx.resolve_links(&corpus)?;
            print_ambiguous_titles(&idx);

            println!("added `{}` ({} chunks)", doc.title, stats.chunks);
            // `add` writes straight into the live database and never touches
            // the pack, which is only ever built in a shadow directory and
            // published by rename (see `reindex_swap_with`) — writing it into
            // the live directory here would break that atomicity guarantee.
            // Once retrieval reads the pack instead of the database, this
            // document is invisible to search until the next `br8n index`.
            // Silence here would be exactly the silent-degradation failure
            // mode this project exists to avoid, so say so on stderr.
            eprintln!(
                "note: `{}` will not be searchable until the next `br8n index`",
                doc.title
            );
        }

        Cmd::Bench {
            synthetic: Some(chunks),
            seed,
            out,
            reuse,
            json,
            ..
        } => {
            let report = match reuse {
                Some(dir) => br8n::bench::synthetic::run_reuse(&cfg, chunks, seed, &dir)?,
                None => br8n::bench::synthetic::run(&cfg, chunks, seed, out.as_deref())?,
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", br8n::bench::synthetic::render(&report));
            }
        }
        Cmd::Bench { no_graph, .. } => br8n::bench::run(&cfg, no_graph)?,
        // `check` returns `Err` when it finds an error, which is what makes the
        // process exit non-zero — the report itself is already printed by then,
        // so the `Error:` line anyhow adds is a summary and not the finding.
        Cmd::Config { what } => {
            if !run_config(what)? {
                std::process::exit(1);
            }
        }
        Cmd::Golden { what } => match what {
            GoldenCmd::Init { force } => br8n::golden::init(&cfg, force)?,
            GoldenCmd::Check => br8n::golden::check(&cfg)?,
        },
        Cmd::Memory { what } => match what {
            MemoryCmd::Add {
                kind,
                project,
                global,
                title,
                text,
            } => {
                let kind = br8n::memory::MemoryKind::parse(&kind)
                    .ok_or_else(|| anyhow::anyhow!("--kind must be lesson, fact or episode"))?;
                let project = match (global, project) {
                    (true, _) => None,
                    (false, Some(p)) => Some(p),
                    (false, None) => Some(std::env::current_dir()?),
                };
                let outcome = br8n::memory::remember(
                    &cfg,
                    br8n::memory::Remember {
                        kind,
                        text: text.join(" "),
                        title,
                        project,
                        confidence: 100,
                        origin: br8n::memory::Origin::User,
                        session: None,
                        source_hash: None,
                        source_stamp: None,
                        created: None,
                    },
                )?;
                println!("{}", outcome.describe(kind));
                if matches!(outcome, br8n::memory::Outcome::Rejected(_)) {
                    std::process::exit(1);
                }
            }
            MemoryCmd::List {
                kind,
                project,
                json,
            } => {
                let kind = match kind {
                    Some(k) => Some(br8n::memory::MemoryKind::parse(&k).ok_or_else(|| {
                        anyhow::anyhow!("--kind must be lesson, fact or episode")
                    })?),
                    None => None,
                };
                let all = br8n::memory::list(&cfg, &br8n::memory::Filter { kind, project })?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&all)?);
                } else if all.is_empty() {
                    println!("no memories yet");
                } else {
                    for m in &all {
                        let scope = m.facts.project.as_deref().unwrap_or("global");
                        println!(
                            "{}  {:<7} {}  {}  {}",
                            m.id,
                            m.facts.kind.as_str(),
                            br8n::memory::ymd(m.facts.created),
                            scope,
                            m.text.lines().next().unwrap_or("")
                        );
                    }
                }
            }
            MemoryCmd::Forget { id } => {
                let gone = br8n::memory::forget(&cfg, &id)?;
                println!(
                    "forgot {} {}: {}",
                    gone.facts.kind.as_str(),
                    gone.id,
                    gone.title
                );
            }
            MemoryCmd::Distill { all, session } => {
                if let Some(path) = session {
                    let out = br8n::memory::distill::distill_session(&cfg, &path)?;
                    println!("{}", out.describe(br8n::memory::MemoryKind::Episode));
                } else {
                    let limit = if all { usize::MAX } else { 1 };
                    let r = br8n::memory::distill::distill_pending(&cfg, limit, false)?;
                    println!(
                        "distilled {} of {} pending session(s){}",
                        r.distilled,
                        r.candidates,
                        r.latched
                            .as_deref()
                            .map(|l| format!(" — stopped: {l}"))
                            .unwrap_or_default()
                    );
                }
            }
            MemoryCmd::Export { out } => {
                let rows = br8n::memory::export(&cfg)?;
                let mut text = String::new();
                for r in &rows {
                    text.push_str(&serde_json::to_string(r)?);
                    text.push('\n');
                }
                match out {
                    Some(p) => {
                        std::fs::write(&p, text)?;
                        println!("wrote {} memories to {}", rows.len(), p.display());
                    }
                    None => print!("{text}"),
                }
            }
            MemoryCmd::Import { file } => {
                let text = std::fs::read_to_string(&file)?;
                let rows: Vec<br8n::memory::ExportRow> = text
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(serde_json::from_str)
                    .collect::<Result<_, _>>()?;
                let (saved, skipped) = br8n::memory::import(&cfg, &rows)?;
                println!("imported {saved} memories, skipped {skipped} duplicates");
            }
            MemoryCmd::Rebuild => {
                let n = br8n::memory::rebuild(&cfg)?;
                println!("rebuilt {n} memories with {}", cfg.embed.model);
            }
        },
        Cmd::AuditInjections { root } => {
            let root =
                root.unwrap_or_else(br8n::loaders::transcript::TranscriptLoader::default_root);
            let a = br8n::audit::audit(&root)?;
            if a.injections == 0 {
                println!("no injections found under {}", root.display());
                println!("(the hook writes its block into the session transcript; if you have");
                println!(" never run a prompt with the hook installed, this is expected)");
                return Ok(());
            }
            println!("injections:  {}", a.injections);
            println!("entries:     {}", a.entries);
            for (scheme, n) in &a.by_scheme {
                let label = if scheme == "file" {
                    "vault note"
                } else if scheme == "claude-session" {
                    "transcript"
                } else if scheme == "codex-session" {
                    "codex session"
                } else {
                    scheme
                };
                println!(
                    "  {label:<12} {n:>6}  ({:.1}%)",
                    100.0 * *n as f64 / a.entries.max(1) as f64
                );
            }
            println!(
                "injections with no vault note: {} ({:.0}%)",
                a.injections_with_no_note,
                100.0 * a.injections_with_no_note as f64 / a.injections as f64
            );
            let mut b = a.body_bytes.clone();
            b.sort_unstable();
            if !b.is_empty() {
                let q = |p: f64| b[((b.len() - 1) as f64 * p) as usize];
                println!(
                    "body bytes:  p25 {}  p50 {}  p90 {}",
                    q(0.25),
                    q(0.50),
                    q(0.90)
                );
            }
        }
        Cmd::Mcp => br8n::mcp::serve(cfg)?,
        Cmd::Hook { which, agent } => br8n::hook::run_for(
            &which,
            br8n::hook::PromptAgent::parse(&agent).unwrap_or(br8n::hook::PromptAgent::ClaudeCode),
            &cfg,
        ),
        Cmd::Dashboard { port, no_open } => {
            br8n::dashboard::serve(port, !no_open)?;
        }
        Cmd::Install { yes, quiet } => {
            let paths = br8n::setup::Paths::from_env();
            let opts = br8n::setup::install::InstallOpts {
                exe: std::env::current_exe()?,
                version: env!("CARGO_PKG_VERSION").to_string(),
                config_path: Config::config_path(),
                claude: br8n::setup::claude::ClaudeCli::from_path(),
                yes,
                quiet,
                ollama_url: cfg.embed.ollama_url.clone(),
                models: required_models(&cfg),
                confirm: confirm_on_stdin,
                pull: br8n::setup::ollama::pull,
            };
            let report = br8n::setup::install::install(&paths, &opts)?;
            let offered = br8n::setup::agents::offer_after_install(
                &br8n::setup::agents::AgentEnv::from_env(),
                yes,
                quiet,
                confirm_on_stdin,
            );
            if !quiet {
                for l in report.lines.iter().chain(&offered.lines) {
                    println!("{l}");
                }
            }
            for w in report.warnings.iter().chain(&offered.warnings) {
                eprintln!("! {w}");
            }
            if !quiet {
                println!(
                    "\nInstalled. Two things left, and only you can do them:\n\n  1. add your notes to  sources  in {}\n  2. run  br8n index\n\nThen restart Claude Code so the plugin loads.",
                    opts.config_path.display()
                );
            }
        }
        Cmd::Agents { json } => run_agents(json)?,
        Cmd::Connect {
            agents,
            instructions,
            all_detected,
            print,
        } => run_connect(agents, instructions, all_detected, print)?,
        Cmd::Disconnect { agents } => run_disconnect(agents)?,
        Cmd::Uninstall { purge, yes } => {
            let paths = br8n::setup::Paths::from_env();
            let opts = br8n::setup::install::UninstallOpts {
                purge,
                yes,
                claude: br8n::setup::claude::ClaudeCli::from_path(),
                config_path: Config::config_path(),
                confirm: confirm_on_stdin,
            };
            let report = br8n::setup::install::uninstall(&paths, &opts)?;
            let agents = br8n::setup::agents::disconnect_all_but_claude_code(
                &br8n::setup::agents::AgentEnv::from_env(),
            );
            for l in report.lines.iter().chain(&agents.lines) {
                println!("{l}");
            }
            for w in report.warnings.iter().chain(&agents.warnings) {
                eprintln!("! {w}");
            }
            if !report.kept.is_empty() {
                println!("kept (remove with `br8n uninstall --purge`):");
                for k in &report.kept {
                    println!("  {}", k.display());
                }
            }
        }
        Cmd::Update {
            check,
            yes: _,
            quiet,
        } => {
            let opts = br8n::update::UpdateOpts {
                paths: br8n::setup::Paths::from_env(),
                installed: env!("CARGO_PKG_VERSION").to_string(),
                api: br8n::update::release::api_base(),
                token: br8n::update::release::token(),
                target: env!("BR8N_TARGET").to_string(),
                check_only: check,
            };
            match br8n::update::run(&opts)? {
                br8n::update::Outcome::CheckOnly(c) => {
                    if !quiet {
                        match c.available() {
                            Some(v) => println!(
                                "installed {}, latest {v} — run `br8n update`",
                                c.installed
                            ),
                            None => println!(
                                "installed {}, latest {} — up to date",
                                c.installed,
                                c.latest.as_deref().unwrap_or("?")
                            ),
                        }
                    }
                }
                br8n::update::Outcome::UpToDate(v) => {
                    if !quiet {
                        println!("br8n {v} is up to date");
                    }
                }
                br8n::update::Outcome::Updated { from, to } => {
                    println!(
                        "updated {from} -> {to}; restart Claude Code so the new version loads"
                    );
                }
            }
        }
        #[cfg(feature = "backup")]
        Cmd::Backup { action } => run_backup(&cfg, action)?,
        #[cfg(feature = "backup")]
        Cmd::Restore {
            generation,
            index,
            dry_run,
            force,
        } => run_restore(
            &cfg,
            br8n::backup::RestoreOptions {
                generation,
                index,
                dry_run,
                force,
            },
        )?,
    }
    Ok(())
}

fn run_config(what: ConfigCmd) -> Result<bool> {
    use br8n::config::{check, edit};
    let path = Config::config_path();
    match what {
        ConfigCmd::Path => println!("{}", path.display()),
        ConfigCmd::Check => {
            let Some(text) = edit::read_document(&path)? else {
                println!(
                    "no config at {}; every setting is at its default",
                    path.display()
                );
                return Ok(true);
            };
            let errors = check::check(&text);
            if errors.is_empty() {
                println!("{}: ok", path.display());
                return Ok(true);
            }
            for e in &errors {
                println!("{}: {e}", path.display());
            }
            println!("{} problem(s)", errors.len());
            return Ok(false);
        }
        ConfigCmd::Get { key } => {
            let segments: Vec<&str> = key.split('.').collect();
            if br8n::config::schema::config_schema()
                .at(&segments)
                .is_none()
            {
                eprintln!("br8n: `{key}` is not a config key");
                return Ok(false);
            }
            let effective = br8n::config::view::config_json(&Config::load_from(&path))?;
            let value = segments
                .iter()
                .try_fold(&effective, |v, k| v.get(*k))
                .unwrap_or(&serde_json::Value::Null);
            match value {
                serde_json::Value::Null => eprintln!("{key} is not set"),
                serde_json::Value::String(s) => println!("{s}"),
                serde_json::Value::Object(_) => print!(
                    "{}",
                    toml::to_string(&br8n::config::view::without_nulls(value))?
                ),
                other => println!("{other}"),
            }
        }
        ConfigCmd::Set { key, value } => {
            let patch = edit::Patch {
                set: vec![(key.clone(), edit::value_from_cli(&value))],
                unset: Vec::new(),
            };
            return report_config_update(&path, &key, edit::update(&path, None, &patch)?);
        }
        ConfigCmd::Unset { key } => {
            let patch = edit::Patch {
                set: Vec::new(),
                unset: vec![key.clone()],
            };
            return report_config_update(&path, &key, edit::update(&path, None, &patch)?);
        }
    }
    Ok(true)
}

fn report_config_update(
    path: &std::path::Path,
    key: &str,
    outcome: br8n::config::edit::Outcome,
) -> Result<bool> {
    use br8n::config::edit::Outcome;
    match outcome {
        Outcome::Written { .. } => {
            println!("updated {key} in {}", path.display());
            Ok(true)
        }
        Outcome::Invalid(errors) => {
            for e in &errors {
                eprintln!("br8n: {e}");
            }
            eprintln!("br8n: {} was not changed", path.display());
            Ok(false)
        }
        Outcome::Conflict { .. } => {
            eprintln!(
                "br8n: {} changed while it was being edited; try again",
                path.display()
            );
            Ok(false)
        }
    }
}

#[cfg(feature = "backup")]
fn run_backup(cfg: &Config, action: Option<BackupAction>) -> Result<()> {
    match action {
        None => backup_once(cfg),
        Some(BackupAction::Init { yes }) => backup_init(cfg, yes),
        Some(BackupAction::Check) => backup_check(cfg),
        Some(BackupAction::Auth { provider: _ }) => backup_auth_drive(cfg),
        Some(BackupAction::Status) => backup_status(cfg),
        Some(BackupAction::Schedule { at, uninstall }) => backup_schedule(&at, uninstall),
    }
}

#[cfg(feature = "backup")]
fn backup_once(cfg: &Config) -> Result<()> {
    let outcome = br8n::backup::run_all(cfg);
    let line = match &outcome {
        br8n::backup::RunOutcome::Done(targets) => {
            let summary = targets
                .iter()
                .map(|(name, s)| {
                    format!(
                        "{name}: {} uploaded, {} deduped, {} bytes, {}",
                        s.uploaded, s.deduped, s.bytes, s.generation
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            format!("ok  {summary}")
        }
        br8n::backup::RunOutcome::Skipped(why) => format!("skip  {why}"),
        br8n::backup::RunOutcome::Failed(why) => format!("fail  {why}"),
    };
    append_backup_log(&line);
    match outcome.exit_code() {
        0 => println!("{line}"),
        code => {
            eprintln!("{line}");
            std::process::exit(code);
        }
    }
    Ok(())
}

#[cfg(feature = "backup")]
fn backup_init(cfg: &Config, yes: bool) -> Result<()> {
    let path = cfg.backup_key_path();
    let key = br8n::backup::crypto::Key::generate();
    key.save(&path)?;
    println!("backup key: {}", key.to_hex());
    println!();
    println!(
        "Written to {}. Please store this somewhere other than this machine: \
         a password manager, or paper.",
        path.display()
    );
    println!(
        "Without it, every backup made with this key is unrecoverable. `br8n` cannot help you get it back."
    );
    if !yes {
        print!("Type `yes` once you have stored it: ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if answer.trim() != "yes" {
            eprintln!("not confirmed; the key is still at {}", path.display());
            std::process::exit(1);
        }
    }
    Ok(())
}

#[cfg(feature = "backup")]
fn backup_check(cfg: &Config) -> Result<()> {
    br8n::backup::scrub_credential_env();
    if !br8n::backup::is_configured(cfg) {
        eprintln!("no backups configured");
        std::process::exit(2);
    }
    let mut failed = false;
    if let Err(e) = br8n::backup::load_key(cfg) {
        eprintln!("key:    FAILED  {e:#}");
        failed = true;
    }
    for target in &cfg.backup.targets {
        match br8n::backup::check_target(cfg, target) {
            Ok(place) => println!("{:<7} ok  {place}", format!("{target}:")),
            Err(e) => {
                eprintln!("{:<7} FAILED  {e:#}", format!("{target}:"));
                failed = true;
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(feature = "backup")]
fn backup_auth_drive(cfg: &Config) -> Result<()> {
    let table = cfg
        .backup
        .drive
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no [backup.drive] table in your config"))?;
    let token = cfg.drive_token_path();
    let folder_id = br8n::backup::remote::drive::authorize(
        &Config::expand_tilde_path(&table.client_secret_file),
        &token,
        &table.folder_id,
    )?;
    println!("authorized; token written to {}", token.display());
    if table.folder_id.is_empty() {
        println!(
            "created the Drive folder \"{}\". Add this under [backup.drive] in {}:",
            br8n::backup::remote::drive::FOLDER_NAME,
            Config::config_path().display()
        );
        println!();
        println!("folder_id = \"{folder_id}\"");
        println!();
    }
    println!(
        "If backups stop working in about a week, the OAuth app is still in \"Testing\" status; \
         set it to \"In production\" in the Google Cloud console."
    );
    Ok(())
}

#[cfg(feature = "backup")]
fn backup_status(cfg: &Config) -> Result<()> {
    let db = Config::db_path();
    println!(
        "last backup: {}",
        br8n::backup::describe_age(br8n::backup::read_stamp(&db))
    );
    println!("targets:     {}", cfg.backup.targets.join(", "));
    println!("encrypted:   {}", cfg.backup.encrypt);
    println!("key:         {}", cfg.backup_key_path().display());
    if cfg.backup.drive.is_some() {
        match std::fs::metadata(cfg.drive_token_path()).and_then(|m| m.modified()) {
            Ok(t) => println!(
                "drive token: refreshed {}",
                br8n::backup::describe_age(Some(t.into()))
            ),
            Err(_) => println!("drive token: missing; run `br8n backup auth drive`"),
        }
    }
    for target in &cfg.backup.targets {
        let counted = br8n::backup::remote_for(cfg, target).and_then(|r| r.list("manifest/"));
        match counted {
            Ok(objects) => println!(
                "{target}: {} generations",
                objects
                    .iter()
                    .filter(|o| o.key != br8n::backup::manifest::Manifest::latest_key())
                    .count()
            ),
            Err(e) => println!("{target}: unreachable ({e:#})"),
        }
    }
    Ok(())
}

#[cfg(feature = "backup")]
fn backup_schedule(at: &str, uninstall: bool) -> Result<()> {
    let exe = std::env::current_exe()?;
    let explicit = std::env::var_os("BR8N_CONFIG").map(std::path::PathBuf::from);
    let line = br8n::backup::cron_line(at, &exe, explicit.as_deref());
    let existing = std::process::Command::new("crontab")
        .arg("-l")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let body = br8n::backup::crontab_with(&existing, (!uninstall).then_some(line.as_str()));
    let mut child = std::process::Command::new("crontab")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            anyhow::anyhow!("could not run `crontab`; is cron available on this machine? {e}")
        })?;
    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .expect("stdin was piped")
            .write_all(body.as_bytes())?;
    }
    let status = child.wait()?;
    if !status.success() {
        return Err(anyhow::anyhow!("`crontab -` exited with {status}"));
    }
    if uninstall {
        println!("removed the br8n-backup crontab entry");
    } else {
        println!("installed: {line}");
        println!(
            "Cron does not run while the machine is asleep and does not catch up afterwards. \
             Pick an hour this machine is awake; `br8n status` shows how long it has been since \
             the last successful backup."
        );
    }
    Ok(())
}

#[cfg(feature = "backup")]
fn run_restore(cfg: &Config, opts: br8n::backup::RestoreOptions) -> Result<()> {
    if !br8n::backup::is_configured(cfg) {
        eprintln!("no backups configured");
        std::process::exit(1);
    }
    let key = br8n::backup::load_key(cfg)?;
    let target = &cfg.backup.targets[0];
    let remote = br8n::backup::remote_for(cfg, target)?;
    let stats = br8n::backup::restore(cfg, remote.as_ref(), key.as_ref(), &opts)?;
    let verb = if opts.dry_run {
        "would restore"
    } else {
        "restored"
    };
    println!(
        "{verb} {} files{} from {target}{}",
        stats.files,
        if stats.index_restored {
            " and the index"
        } else {
            ""
        },
        if stats.skipped_existing > 0 {
            format!(
                " ({} already present; --force overwrites them)",
                stats.skipped_existing
            )
        } else {
            String::new()
        }
    );
    Ok(())
}

#[cfg(feature = "backup")]
fn append_backup_log(line: &str) {
    use std::io::Write;
    let path = Config::backup_log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(
            f,
            "{}  {}",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            line.lines().collect::<Vec<_>>().join(" | ")
        );
    }
}
