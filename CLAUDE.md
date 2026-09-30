# CLAUDE.md

Local RAG plugin for Claude Code. Rust. One binary, no daemon. Embeds through
local Ollama by default, or any OpenAI-compatible `/v1/embeddings` endpoint. LadybugDB (`lbug`) is the store — vector index + BM25 + property
graph in one embedded database.

## Commands

```bash
cargo test                  # 200+ tests, no network, no live Ollama
cargo clippy --all-targets  # zero warnings is the bar
cargo fmt --all
cargo build --release && target/release/br8n install   # installs THIS build over the release, hooks and all
cargo run -- dashboard --no-open  # local web dashboard; SPA sources in dashboard/, built dist/ is committed
make dev                    # cargo build --no-default-features: no backup, no OCR, far fewer crates
make test-dev               # cargo test --no-default-features
cargo test --test it        # the one merged integration binary; see "Build and test layout"
make ci                     # scripts/ci-local.sh: every ci.yml job on this machine, plus the dashboard e2e suite
```

MSRV is 1.95, and it is the OCR tree that sets it: `oar-ocr` and its two
sibling crates declare 1.95, reached through `pdf-inspector`'s `ocr`
feature. `nalgebra`, `safe_arch` and `wide` want 1.89 behind the same
feature. The crates this line used to name — `htmd`/`ignore` for edition
2024's 1.85 floor, `cxx`/`time` at 1.88 — are all still there and are all
now slack. The floor therefore moves whenever `oar-ocr` does, and the CI
`msrv` job is what catches it: it builds on exactly that toolchain rather
than on `stable`, so a declaration the tree cannot meet fails loudly
instead of passing on whatever the developer happens to have installed.

## Build and test layout

**Two cargo features, both on by default.** `backup` pulls in the S3, Google
Drive and encryption crates behind `br8n backup`; `ocr` is
`pdf-inspector/ocr`, the PDFium/ONNX tree that also sets the MSRV above.
`make dev` and `make test-dev` build and test with both off, which is the fast
inner loop; CI runs `cargo clippy --all-targets --no-default-features` so the
feature-off build cannot rot. A test that needs a feature is gated on it —
`tests/it/main.rs` declares every `backup_*` module under
`#[cfg(feature = "backup")]`.

**The release profile is thin LTO, `codegen-units = 1`, `strip = true`.** It
makes a smaller, faster binary, and nothing a developer runs builds it.
Stripping and LTO are exactly what can drop the symbols lbug's dlopen'd
`vector` extension resolves against the executable, and the ONLY check that
this profile still exports them is `scripts/release-smoke.sh` assertion 5 (a
`--quality 2` search, which opens the store). It runs on every packaged
release target, but for Linux, where the export-dynamic flag spelling differs,
it is the only check anywhere: the perf job and the runtime-budget gate both
stay on tier 1 and never load the extension.

**Integration tests are ONE binary, `it`, plus thirteen that must stay
separate.** `Cargo.toml` sets `autotests = false`; `tests/it/main.rs` declares
`common` by `#[path]` and one `mod <name>;` per file in `tests/it/`. Linking one
binary instead of 59 is most of what made `cargo test` quick and is what keeps
the Linux runner's link step off the disk limit. The separate `[[test]]`
targets are the twelve files that mutate process environment in-process
(`std::env::set_var`/`remove_var`) — `bench`, `dashboard`, `golden`, `index`,
`loaders_pdf`, `memory_embed_backend`, `memory_self_yield`,
`retrieve_store_wiring`, `session_start_trigger`, `status_lines`,
`transcript_deferral_cli`, `transcript_settle` — because their env locks only
serialise tests within one file, and inside `it` another file's mutation would
interleave mid-test; and `update_notice`, whose
`a_stale_check_spawns_a_detached_check_that_records_its_failure` races a
detached child by construction and flaked once in four runs under `it`'s
parallelism. A new test file goes in `tests/it/` with a `mod` line unless it
touches process-global state.

The timing probes `probe_real_sizes` and `probe_insert_scaling` are ignored by
default. Run them on demand with:

    cargo test --test it -- --ignored probe

With `autotests = false` a file cargo is not told about is never compiled, and
its tests "pass" by not existing. The CI step "Enforce the test target count
and that every test file is compiled" therefore fails on any `tests/*.rs` that
is not a declared `[[test]]`, on any `tests/it/*.rs` with no `mod` line, and on
more than 14 test targets. That 14 is deliberate friction: raise it in the
same commit that adds a target, with the reason, never to make CI green.

**CI runs locally too.** `scripts/ci-local.sh`
(`make ci`) runs every `ci.yml` job on this machine and prints a pass/fail
table; `--only`/`--skip` pick jobs, `--perf <base>` adds the perf comparison.
The `e2e` job is `dashboard/e2e/`: a Puppeteer suite that indexes a fixture
corpus against a Node stub of Ollama's `/api/embed` (a deterministic
bag-of-words vector, so search results are meaningful without a model), serves
the dashboard under a throwaway `HOME`, and runs every `specs/*.mjs`. Specs
share that one indexed sandbox and run in file order, so a spec that changes
it (connecting an agent, placing `bin/br8n`) affects every later one; a spec
that exports `fresh = true` gets its own unindexed sandbox with no `sources`
and its own dashboard instead. A browser
console error fails the test that caused it. Screenshots land in
`target/e2e-shots/`. It has its own `package.json` so the dashboard job's
`npm ci` never downloads Chrome. The stub must run in the same event loop as
the runner, so the runner never uses `spawnSync` for `br8n`: a blocked loop
cannot answer the embed and `br8n index` times out.

**Performance guards, and how to recalibrate them.** Two exist and neither
runs on a developer's machine by default.

- `scripts/perf-compare.sh <base> <head>` (the CI `perf` job, PRs only) builds
  a 35,000-chunk synthetic pack with each binary, then runs seven interleaved
  `--reuse` pairs. It fails on tier-1 p50 more than 15% slower in at least 5 of
  7 pairs, median reuse peak RSS or the single build peak RSS more than 10%
  higher, `pack_bytes_per_chunk` more than 10% higher, or `pack_build_ms` more
  than 25% slower. Those margins were set by the plan, not derived from a
  measured noise floor; what is verified is only that identical binaries pass
  locally and a head with tier-1 `efs` raised to 800 fails. To recalibrate, run the script with the SAME binary as both
  arguments several times on the runner class that runs it (`PERF_PAIRS`,
  `PERF_CHUNKS`, `PERF_SEED` override the defaults) and set each threshold
  above the largest self-versus-self difference seen; a threshold inside that
  noise fails PRs at random. The job skips itself green when the merge base
  has no `bench --synthetic --reuse`.
- `scripts/runtime-budget.sh <binary> 35000 --budget scripts/runtime-budget.json`
  (the release workflow, x86_64 Linux only, in a container clamped to at most
  8 GB and 4 CPUs) measures the hook's p50 and peak RSS, startup time and
  binary size against a stub embedder, and fails on any key over its ceiling.
  The ceilings are about 2x the first Linux run (a dry run on a
  2-CPU runner, so the container got `--cpus=2`): hook p50
  93 ms, hook peak RSS 89 MB, start 4.1 ms, binary 67.6 MB. To recalibrate,
  trigger the release workflow by `workflow_dispatch` (a dry run is the default
  and publishes nothing), read the measured JSON the step prints before its
  verdict, and reset each ceiling to about 2x that value. Only keys present in
  both the budget file and the measurement are compared, so adding a key to
  the JSON is how a new metric becomes gated.

**`br8n index` runs at nice 10** (`index::lower_priority`), so a prompt's
hook is not starved by an index on a 4-core machine. The cost: under CPU
contention the indexer is descheduled more, and it holds lbug's file lock —
which blocks every store reader — for longer.

## Conventions

**One commit per pull request.** Squash the branch before you open the PR,
and again after review fixes land. Interactive rebase is not available in
this environment, so squash with `git reset --soft <base>`, one
`git commit`, and `git push --force-with-lease`.

**No prefix on a commit subject.** Not `docs:`, not `release:`, not
`fix:`. The subject is a plain sentence that says what changed, for
example `the README carries the install guide`.

**No comments in code unless explicitly asked.** Write code that explains
itself: names that say what a thing is, functions that do one thing, no
comment standing in for a clearer name. This applies to code you write or
change. Comments already in the tree stay unless you are asked to remove
them.

