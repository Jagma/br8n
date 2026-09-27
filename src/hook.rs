use crate::budget::fit_to_budget;
use crate::config::{Config, Surface};
use crate::store::Hit;
use std::io::{IsTerminal, Read, Write};
use std::time::Duration;

/// Words below which a prompt carries no retrievable intent. "yes", "continue",
/// and "now fix it" would otherwise inject noise on every turn.
const MIN_WORDS: usize = 4;

/// How long a successful index buys before `SessionStart` will spawn another.
///
/// `SessionStart` fires on startup, on resume, on `/clear`, and on every
/// auto-compaction — in EVERY live Claude Code session, and this hook used to
/// spawn a detached `br8n index` on all of them unconditionally. Measured on
/// this machine with seven sessions open: `db.log` held 247 completed runs and
/// 258 "another `br8n index` is already running; skipping this run" refusals.
/// `IndexLock` was working — the runs were serialised, not concurrent — but
/// the moment one finished the next trigger started another, so the indexer
/// looked like it restarted on completion.
///
/// The signal is `index::last_index_age`, i.e. `db.stamps`' mtime, which is
/// written only after a successful swap. Two properties of that choice matter:
///
///   * It is a GLOBAL bound, not a per-session one. Every session reads the
///     same file beside the same database, so N concurrent sessions get at
///     most one run per interval BETWEEN them. That is what turns 247 runs a
///     day into at most ~96, and it is the half of this fix that scales with
///     the number of open sessions. The other half is
///     `TranscriptLoader::SETTLE`, which is what makes the runs that DO happen
///     cheap; this constant alone would not have been enough.
///   * A no-op run does not advance it, because the unchanged-corpus fast path
///     returns before `save_stamps`. The limit suppresses the runs that cost
///     something and leaves the free ones alone.
///
/// READ THE SECOND PROPERTY AS A LIMIT ON THE LIMIT, not only as a nicety. In
/// the steady state the deferral targets — N live sessions and nothing else
/// changing — every run is a no-op run, so `db.stamps`' mtime never moves and
/// this interval never actually binds. What bounds the cost there is that each
/// such run is a few hundred `stat` calls and returns before opening the store,
/// not that it was refused. Any argument of the form "the interval already
/// bounds X to four runs an hour" is therefore false in exactly the state that
/// matters most, and was made once in review.
///
/// Fifteen minutes. The floor is `TranscriptLoader::SETTLE` (ten): a shorter
/// interval spends whole runs re-discovering that every live session's
/// transcript is still too fresh to read, so the two numbers would fight. The
/// ceiling is patience — a note saved before a coffee break should be
/// searchable when you get back. Nothing here bounds a MANUAL `br8n index`,
/// or the MCP `br8n_index` tool, which is what a user reaches for when they
/// want it now.
const REINDEX_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// How long to wait for Claude Code's `SessionStart` payload on stdin.
///
/// Claude Code writes the JSON and closes the pipe immediately, so this is
/// only ever reached when nothing is coming. It has to exist because
/// `read_to_string` on stdin blocks until EOF, and there are contexts where
/// EOF never arrives — most obviously
/// `$CLAUDE_PLUGIN_ROOT/bin/br8n hook session-start` typed into a shell. A
/// hook that hangs until Claude Code's 60s timeout breaks session startup,
/// which is far worse than the indexing storm this decision exists to stop.
///
/// This is the guarantee, and it is the only one. A terminal is short-circuited
/// before the read even begins (see `read_stdin_bounded`), but that is a
/// latency optimisation layered on top — measured, the timeout alone answers a
/// real pty in 0.6s. What no terminal check could ever see is the case this
/// exists for: a pipe held open by a parent that never writes and never closes.
const STDIN_WAIT: Duration = Duration::from_millis(500);

pub fn should_retrieve(prompt: &str) -> bool {
    let p = prompt.trim();
    if p.is_empty() {
        return false;
    }
    p.split_whitespace().count() >= MIN_WORDS
}

pub fn build_context(hits: &[Hit], max_tokens: usize) -> Option<String> {
    if hits.is_empty() {
        return None;
    }
    let (body, _) = fit_to_budget(hits, max_tokens);

    if body.trim().is_empty() {
        return None;
    }
    Some(format!(
        "<br8n-context>\nRelevant excerpts from the user's knowledge base. \
         Read the linked source if you need more.\n{body}</br8n-context>"
    ))
}

/// Entry point for both hooks. Returns `()` and swallows every error: the
/// UserPromptSubmit hook must never be able to block the user's prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptAgent {
    ClaudeCode,
    Codex,
    Gemini,
}

impl PromptAgent {
    pub fn parse(id: &str) -> Option<PromptAgent> {
        match id {
            "claude-code" => Some(PromptAgent::ClaudeCode),
            "codex" => Some(PromptAgent::Codex),
            "gemini" => Some(PromptAgent::Gemini),
            _ => None,
        }
    }

    pub fn prompt_of(self, input: &str) -> Option<String> {
        serde_json::from_str::<serde_json::Value>(input)
            .ok()
            .map(|v| v["prompt"].as_str().unwrap_or_default().to_string())
    }

    pub fn envelope(self, context: Option<&str>) -> Option<String> {
        match (self, context) {
            (PromptAgent::ClaudeCode | PromptAgent::Codex, Some(ctx)) => Some(
                serde_json::json!({
                    "hookSpecificOutput": {
                        "hookEventName": "UserPromptSubmit",
                        "additionalContext": ctx,
                    }
                })
                .to_string(),
            ),
            (PromptAgent::ClaudeCode | PromptAgent::Codex, None) => None,
            (PromptAgent::Gemini, Some(ctx)) => Some(
                serde_json::json!({ "hookSpecificOutput": { "additionalContext": ctx } })
                    .to_string(),
            ),
            (PromptAgent::Gemini, None) => Some("{}".to_string()),
        }
    }
}

pub fn run(which: &str, cfg: &Config) {
    run_for(which, PromptAgent::ClaudeCode, cfg)
}

pub fn run_for(which: &str, agent: PromptAgent, cfg: &Config) {
    match which {
        "prompt" => run_prompt(cfg, agent),
        "session-start" => run_session_start(cfg),
        "load" => run_load(cfg),
        _ => {}
    }
}

fn run_load(cfg: &Config) {
    let started = std::time::Instant::now();
    let outcome =
        crate::embed::for_config(&cfg.embed).and_then(|e| e.embed_documents(&["warm".to_string()]));
    match outcome {
        Ok(_) => eprintln!(
            "br8n: remote embedding model loaded in {:.1}s",
            started.elapsed().as_secs_f64()
        ),
        Err(e) => eprintln!("br8n: loading the remote embedding model failed — {e:#}"),
    }
}

const REMOTE_LOAD_WINDOW: Duration = Duration::from_secs(120);

fn spawn_remote_load(log: &std::path::Path) -> std::io::Result<Option<u32>> {
    let marker = log.with_extension("rload");
    let in_flight = std::fs::metadata(&marker)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < REMOTE_LOAD_WINDOW);
    if in_flight {
        return Ok(None);
    }
    std::fs::write(&marker, std::process::id().to_string())?;
    let sink = || {
        open_log_for_append(log)
            .map(std::process::Stdio::from)
            .unwrap_or_else(|_| std::process::Stdio::null())
    };
    let exe = std::env::current_exe()?;
    let child = std::process::Command::new(exe)
        .args(["hook", "load"])
        .stdin(std::process::Stdio::null())
        .stdout(sink())
        .stderr(sink())
        .spawn()?;
    Ok(Some(child.id()))
}

fn run_prompt(cfg: &Config, agent: PromptAgent) {
    let context = prompt_context(cfg, agent);
    if let Some(out) = agent.envelope(context.as_deref()) {
        println!("{out}");
    }
}

fn prompt_context(cfg: &Config, agent: PromptAgent) -> Option<String> {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return None;
    }
    let prompt = agent.prompt_of(&input)?;
    if !should_retrieve(&prompt) {
        return None;
    }

    let surface = cfg.surface(Surface::Hook);
    let profile = cfg.profile_for(Surface::Hook);

    // The relevance gate. Below threshold, injecting costs tokens and buys noise.
    // Gate on `relevance`, never on `score`. `score` is an RRF rank value whose
    // arithmetic maximum is about 0.049, so comparing it against a human
    // threshold like 0.55 rejects every hit and the hook silently injects
    // nothing forever. `relevance` is a [0,1] similarity with an absolute
    // meaning, carried by every hit regardless of which retriever found it.
    //
    // `search_gated` applies it inside the pipeline, before rerank and before
    // diversity selection — filtering out here instead let MMR spend its slots
    // on hits that were about to be discarded.
    //
    // Weighted with the HOOK's weights, not the global table. The weights
    // multiply `relevance`, which is what `threshold` is compared against, so
    // the two have to come from the same surface or the gate is calibrated
    // against a scale nobody configured.
    // Take priority over any background index for the duration of this
    // retrieval — without it, the query embed queued behind bulk batches and
    // the hook was mute for every prompt of a multi-minute reindex.
    let _priority = crate::index::QueryPriority::announce(&crate::config::Config::db_path());
    let relevant: Vec<Hit> = match crate::retrieve_for(cfg, Surface::Hook)
        .and_then(|r| r.search_gated_with_report(&prompt, &profile, surface.threshold))
    {
        Ok((hits, report)) => {
            // The hook exits 0 no matter what, so a pipeline that silently
            // stopped running a stage looks exactly like an honest no-match.
            // stdout carries the hook's JSON contract, so stderr — wherever
            // the invoking process (Claude Code, or a shell during manual
            // testing) sends it — is the only place a human can see this
            // happened. Unlike `run_session_start`'s indexer subprocess, this
            // process's own stderr is not redirected to `db.log`.
            if report.degraded {
                eprintln!(
                    "br8n: retrieval degraded — budget {}ms exceeded, ran only [{}]",
                    profile.budget_ms,
                    report.stages_run.join("+")
                );
            }
            // Distinct from the budget line above: this profile had time to
            // run BM25, asked for it, and there was no pack to serve it — the
            // store-fallback path, where `Store::fts_search` always errors
            // now that lbug's FTS index is gone. The query still answers
            // vector-only rather than failing outright, so stderr is the only
            // place this loss is visible.
            if let Some(reason) = &report.bm25_unavailable {
                eprintln!(
                    "br8n: retrieval degraded — bm25 unavailable, ran vector-only ({reason})"
                );
            }
            // The mirror image: a pack published before any embedding has
            // postings but no vectors (`Manifest::rows_with_vectors == 0`,
            // `Pack::has_vectors() == false`). Vector search answers empty
            // rather than erroring, so this is the only way the loss is
            // visible — see `StageReport::vectors_unavailable`.
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
        // A hard error here is almost always `open_existing` refusing to load
        // the `vector` extension — a build/link misconfiguration, not a
        // transient condition. Silently returning made that failure
        // indistinguishable from an honest no-match, forever. The hook still
        // exits 0 and never blocks the prompt; stderr is the only channel
        // left to say this happened, since stdout carries the hook's JSON
        // contract.
        Err(e) if cfg.embed.remote.is_some() && crate::embed::timed_out(&e) => {
            let log = Config::db_path().with_extension("log");
            match spawn_remote_load(&log) {
                Ok(Some(pid)) => eprintln!(
                    "br8n: retrieval unavailable — the embedding endpoint did not answer in \
                     time, probably while loading its model; loading it in the background \
                     (pid {pid}) for the next prompt ({e:#})"
                ),
                Ok(None) => eprintln!(
                    "br8n: retrieval unavailable — the embedding endpoint did not answer in \
                     time; a background load started in the last {}s is still running ({e:#})",
                    REMOTE_LOAD_WINDOW.as_secs()
                ),
                Err(spawn) => eprintln!(
                    "br8n: retrieval unavailable — {e:#}; could not start a background load: \
                     {spawn}"
                ),
            }
            return None;
        }
        Err(e) => {
            eprintln!("br8n: retrieval unavailable — {e:#}");
            return None;
        }
    };

    let injected = crate::budget::admitted_by_budget(&relevant, surface.max_tokens);
    crate::usage::record_hits(&crate::config::Config::db_path(), &relevant[..injected]);

    build_context(&relevant, surface.max_tokens)
}

/// What a `SessionStart` decided to do about indexing, and why.
#[derive(Debug, PartialEq, Eq)]
pub enum SessionStart {
    /// Spawn the detached indexer.
    Index,
    /// Do not. The string is the reason, written verbatim to `db.log`.
    Skip(String),
}

/// Decide whether a `SessionStart` should spawn an indexer.
///
/// Both inputs are arguments rather than read in here, so the decision can be
/// tested with no process, no database and no clock.
///
/// `source` is the payload's `source` field — `None` when stdin carried
/// nothing usable. `since_last_index` is `index::last_index_age`.
///
/// THE INTERVAL IS THE WHOLE DECISION. `source` reaches `db.log` and nothing
/// else: no trigger is refused for being the wrong KIND of trigger. That is
/// deliberate and it is the second version of this function — see the block
/// inside.
pub fn session_start_decision(
    source: Option<&str>,
    since_last_index: Option<Duration>,
) -> SessionStart {
    // NO SOURCE IS DENIED, INCLUDING `compact`, and this is the one place the
    // shipped behaviour changed after review.
    //
    // The first version refused `compact` outright, reasoning that a
    // compaction continues the same session so "nothing has changed on disk
    // that a startup or resume did not already cover". That claim is FALSE and
    // there is a repro: write a note mid-session, backdate `db.stamps` so the
    // interval is out of the picture, fire `source=compact` — the note is
    // never indexed, and no further trigger fires for the rest of that
    // session. The sentence confused the SESSION (a compaction does imply an
    // earlier startup) with the CLOCK (that startup may have been eight hours
    // and forty notes ago).
    //
    // Follow it through and it is this project's own house failure mode: in an
    // all-day single session, compaction is the ONLY trigger that fires, so
    // denying it means the index silently stops growing until the next
    // process. That is exactly the trade the `resume` and `clear` notes below
    // refuse to make, and it is worse than the storm of runs being fixed —
    // a storm is visible in `db.log`, a corpus that quietly stopped is not.
    //
    // What is given up is small and was measured on the wrong axis. Compaction
    // was "the single most frequent needless trigger", but frequency was never
    // the cost — the cost was that every run re-parsed 157MB of transcript.
    // With `TranscriptLoader::SETTLE` deferring live transcripts, a run over an
    // unchanged corpus is a few hundred `stat` calls and returns before it
    // opens the store at all. So a compaction now buys a sub-second no-op
    // process instead of a multi-minute one, and the interval below bounds the
    // runs that actually cost something for every source alike.
    //
    // `resume` cannot be denied by source: a resume can be days after the
    // session was left, and a user who always resumes would then never index
    // at all.
    //
    // `clear` cannot be denied either, for the same shape of reason. `/clear`
    // is user-initiated and marks the point at which the previous conversation
    // is FINISHED, which is exactly when its transcript becomes worth reading;
    // and for someone who works all day in one process it is the only boundary
    // that ever fires. Rate-limiting is what makes it cheap, not refusing it.
    //
    // An unknown or absent source indexes for the same reason: a trigger this
    // build has never heard of must not silently disable indexing.
    if let Some(age) = since_last_index {
        if age < REINDEX_INTERVAL {
            return SessionStart::Skip(format!(
                "source={} — last successful index finished {}s ago, inside the {}s interval",
                source.unwrap_or("unknown"),
                age.as_secs(),
                REINDEX_INTERVAL.as_secs()
            ));
        }
    }
    SessionStart::Index
}

pub fn update_notice(
    check: Option<&crate::update::UpdateCheck>,
    updating: bool,
    installed: &str,
) -> Option<String> {
    if updating {
        return None;
    }
    let newer = check?.available_against(installed)?;
    Some(format!(
        "br8n {newer} is available (you have {installed}). Run `br8n update`, or press Update in `br8n dashboard`."
    ))
}

/// The most stdin this hook will buffer.
///
/// A `SessionStart` payload is four short fields — a session id, a source, a
/// cwd and a PATH to the transcript, never its contents — so a real one is a
/// few hundred bytes. `read_to_string` has no ceiling of its own, and stdin is
/// whatever the invoker attached: `br8n hook session-start < /dev/zero`
/// exits in 0.8s having peaked at 992 MB RSS. A timeout bounds how LONG this
/// waits and nothing bounds how MUCH it reads, which is the other half.
///
/// A megabyte, and truncation is safe rather than merely tolerable. Past the
/// cap the JSON no longer parses, `session_source` answers `None`, and the
/// decision treats it as an unknown trigger — which indexes. Since
/// `session_start_decision` denies no source, losing the label costs the
/// accuracy of one `db.log` line and no behaviour at all.
const STDIN_CAP: u64 = 1 << 20;

/// Read stdin to EOF or `STDIN_CAP`, whichever comes first, or give up after
/// `wait`.
///
/// See `STDIN_WAIT` for why a plain `read_to_string` is not safe here.
fn read_stdin_bounded(wait: Duration) -> Option<String> {
    // A LATENCY OPTIMISATION, NOT A SECOND SAFETY NET, and an earlier draft
    // that called it one was measured and found overstated. A terminal never
    // sends EOF on its own, so the read would block until a human pressed
    // ctrl-D — but the timeout below already covers that: with this
    // short-circuit removed, the hook against a real pty still exits, in 0.6s
    // rather than 0.09s. What this buys is those 0.5s on a hook a developer
    // ran by hand; the guarantee is the timeout's.
    //
    // Nothing pins it. The only observable difference is wall-clock, and a
    // sub-second assertion on a debug binary under a loaded machine is a
    // flake, not a pin — so deleting this line leaves the suite green. It is
    // documented here instead of tested, which is the honest version.
    if std::io::stdin().is_terminal() {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    // Detached deliberately. If the read never completes, this process still
    // exits normally and takes the thread with it; joining it would reintroduce
    // exactly the hang the timeout exists to prevent.
    std::thread::spawn(move || {
        let mut buf = String::new();
        let ok = std::io::stdin()
            .lock()
            .take(STDIN_CAP)
            .read_to_string(&mut buf)
            .is_ok();
        let _ = tx.send(ok.then_some(buf));
    });
    rx.recv_timeout(wait).ok().flatten()
}

struct SessionPayload {
    source: Option<String>,
    cwd: Option<std::path::PathBuf>,
}

fn session_payload(wait: Duration) -> SessionPayload {
    let parsed = read_stdin_bounded(wait)
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok());
    SessionPayload {
        source: parsed
            .as_ref()
            .and_then(|v| v["source"].as_str().map(str::to_string)),
        cwd: parsed
            .as_ref()
            .and_then(|v| v["cwd"].as_str().map(std::path::PathBuf::from)),
    }
}

const LOG_ROTATE_LIMIT: u64 = 10 * 1024 * 1024;

fn rotate_log_if_large(log: &std::path::Path) {
    let Ok(meta) = std::fs::metadata(log) else {
        return;
    };
    if meta.len() < LOG_ROTATE_LIMIT {
        return;
    }
    let _ = std::fs::rename(log, log.with_extension("log.1"));
}

pub(crate) fn open_log_for_append(log: &std::path::Path) -> std::io::Result<std::fs::File> {
    rotate_log_if_large(log);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
}

/// Append one line to `db.log`.
///
/// Not stdout: `SessionStart` stdout is fed to the model as context, so a
/// diagnostic printed there would end up in the conversation. `db.log` is
/// where this hook's indexer subprocess already writes, so a decision line and
/// the run it did or did not produce sit next to each other.
fn log_line(log: &std::path::Path, line: &str) {
    let stamped = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => format!("[{}] {line}\n", d.as_secs()),
        Err(_) => format!("[?] {line}\n"),
    };
    if let Ok(mut f) = open_log_for_append(log) {
        let _ = f.write_all(stamped.as_bytes());
    }
}

fn run_session_start(cfg: &Config) {
    let paths = crate::setup::Paths::from_env();
    let log_path = paths.db.with_extension("log");

    // Warm the embedding model: Spike 2 measured 2000ms cold vs 24ms warm, and a
    // cold model would otherwise stall the first prompt of the session.
    // Unconditional, and deliberately above the indexing decision — warming is
    // what makes the FIRST PROMPT fast, which every session needs whether or
    // not this one is going to index.
    // SYNCHRONOUS, and it used to be a detached thread. That thread was never
    // joined, so `main` returned and the process tore down while the warm was
    // still inside reqwest and OpenSSL — and glibc caught the result on CI:
    //
    //   a_rate_limited_trigger_spawns_no_indexer_at_all        -> SIGSEGV (139)
    //   absent_empty_and_garbage_stdin_all_index_without_hanging -> SIGABRT (134)
    //     malloc_consolidate(): unaligned fastbin chunk detected
    //
    // Which tests failed is the evidence: both are the paths that return
    // FASTEST, so the process exits while the thread is still mid-flight. The
    // slower paths — the 500ms stdin wait, the enormous-stdin read — passed,
    // because by then the thread had finished. It never reproduced on macOS,
    // and no CI run reached these tests between 2026-08-26 and 2026-09-06.
    //
    // Waiting is affordable only because `warm` now uses the 4s `query_client`:
    // a refused connection returns at once, a cold model costs the ~2000ms
    // Spike 2 measured, and nothing can exceed 4s. Joining against the old 120s
    // client would have traded a crash for a two-minute stall in session
    // startup, which `SessionStart` treats as the worse failure of the two.
    if cfg.embed.remote.is_some() {
        match spawn_remote_load(&log_path) {
            Ok(Some(pid)) => log_line(
                &log_path,
                &format!(
                    "br8n: SessionStart loading the remote embedding model in the background \
                     — pid {pid}"
                ),
            ),
            Ok(None) => log_line(
                &log_path,
                &format!(
                    "br8n: SessionStart skipped the remote model load — one started in the \
                     last {}s is still running",
                    REMOTE_LOAD_WINDOW.as_secs()
                ),
            ),
            Err(e) => log_line(
                &log_path,
                &format!("br8n: SessionStart could not start the remote model load — {e}"),
            ),
        }
    } else {
        match crate::embed::for_config(&cfg.embed) {
            Ok(e) => {
                let _ = e.warm();
            }
            Err(e) => log_line(
                &log_path,
                &format!("br8n: SessionStart cannot embed — {e:#}"),
            ),
        }
    }

    session_update_check(cfg, &paths, &log_path);

    let db = Config::db_path();
    // Log, do not discard. This indexer runs on EVERY session and its output is
    // the only place skipped files, prune counts and model mismatches are
    // reported; sending it to /dev/null made every one of those failures
    // invisible. Appending to one file keeps startup silent without losing it.
    let log = db.with_extension("log");

    // Deciding, rather than always indexing, is the fix for the restart loop —
    // see `REINDEX_INTERVAL`. The decision is recorded either way: a run that
    // silently did not happen is indistinguishable from one that happened and
    // found nothing, which is the failure mode this project keeps shipping.
    let payload = session_payload(STDIN_WAIT);
    match crate::memory::lessons_block(cfg, payload.cwd.as_deref()) {
        Ok(Some(block)) => print!("{block}"),
        Ok(None) => {}
        Err(err) => log_line(
            &log,
            &format!("br8n: lessons pack unreadable, lessons not injected — {err}"),
        ),
    }
    let source = payload.source;
    match session_start_decision(source.as_deref(), crate::index::last_index_age(&db)) {
        SessionStart::Skip(why) => {
            log_line(&log, &format!("br8n: SessionStart did not index — {why}"));
            return;
        }
        SessionStart::Index => log_line(
            &log,
            &format!(
                "br8n: SessionStart indexing — source={}",
                source.as_deref().unwrap_or("unknown")
            ),
        ),
    }

    // Detached so session startup is never blocked by indexing.
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(_) => return,
    };
    let sink = || {
        open_log_for_append(&log)
            .map(std::process::Stdio::from)
            .unwrap_or_else(|_| std::process::Stdio::null())
    };
    let _ = std::process::Command::new(exe)
        .arg("index")
        .stdout(sink())
        .stderr(sink())
        .spawn();
}

fn session_update_check(cfg: &Config, paths: &crate::setup::Paths, log: &std::path::Path) {
    use crate::update::{check_decision, CheckDecision, Status, UpdateCheck};
    if !paths.plugin.is_dir() {
        log_line(
            log,
            "br8n: SessionStart update check skipped — not installed (no plugin directory)",
        );
        return;
    }
    let cached = UpdateCheck::read(&paths.update_json);
    match check_decision(cached.as_ref(), crate::update::now(), cfg.update.check) {
        CheckDecision::Skip(why) => log_line(
            log,
            &format!("br8n: SessionStart update check skipped — {why}"),
        ),
        CheckDecision::Check => {
            let sink = || {
                open_log_for_append(log)
                    .map(std::process::Stdio::from)
                    .unwrap_or_else(|_| std::process::Stdio::null())
            };
            let spawned = std::env::current_exe().and_then(|exe| {
                std::process::Command::new(exe)
                    .args(["update", "--check", "--quiet"])
                    .stdout(sink())
                    .stderr(sink())
                    .spawn()
            });
            match spawned {
                Ok(child) => log_line(
                    log,
                    &format!(
                        "br8n: SessionStart checking for a newer release — pid {}",
                        child.id()
                    ),
                ),
                Err(e) => log_line(
                    log,
                    &format!("br8n: SessionStart could not start the update check — {e}"),
                ),
            }
        }
    }
    let updating = Status::running(&paths.update_status).is_some();
    if let Some(msg) = update_notice(cached.as_ref(), updating, env!("CARGO_PKG_VERSION")) {
        let out = serde_json::json!({
            "systemMessage": msg,
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": msg,
            }
        });
        println!("{out}");
    }
}
