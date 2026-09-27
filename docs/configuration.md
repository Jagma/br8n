# Configuration and tuning

## Configure

Run `br8n status` once before writing a config — it prints the exact paths
this build of `br8n` resolves for its config file and database. They come
from the `directories` crate and are platform-specific: on macOS the config
lives at `~/Library/Application Support/br8n/config.toml`, not
`~/.config/br8n/`. Don't guess the path; ask `br8n status`.

The hook never fails over a bad config: a file it cannot parse means every
setting falls back to its default, silently. `br8n config check` is the
strict reader. It reports syntax errors, unknown keys (with the key you
probably meant), values of the wrong type, values outside the range br8n
accepts, and `sources` that do not exist, and exits 1 if it finds any.
`br8n status` prints the same problems.

```bash
br8n config path                      # where config.toml is
br8n config check                     # every problem, with its line
br8n config get hook.threshold        # the value in effect, default or not
br8n config set hook.threshold 0.7    # value is read as TOML, else as a string
br8n config set sources '["~/notes"]'
br8n config unset hook.threshold      # back to the default
```

`set` and `unset` edit the file in place and keep your comments and ordering.
They refuse a change that would introduce a problem, write through a temporary
file and a rename, and leave the previous version beside it as
`config.toml.bak`. The dashboard's settings use the same code.

A minimal config:

```toml
sources = [
    "~/notes",
    "~/Documents/papers",
]
```

`sources` is the one setting with no default — without it there is nothing to
index but your Claude Code session transcripts (see below). Paths can be
files or directories; directories are walked recursively, honoring
`.gitignore`.

Session history from `~/.claude/projects` is indexed automatically —
`index_transcripts` defaults to `true` — because it's one of the four sources
this tool exists to search, and typically the largest one: 283 files and 75 MB
of transcripts on one developer machine. Add this to decline it:

```toml
index_transcripts = false
```

Transcripts otherwise stay in the index until Claude Code's own 30-day sweep
deletes the file, so a long-lived history grows the index without limit. Set
`index_transcripts_max_age_days` to leave out anything older than that many
days, judged by the transcript file's own modification time; it defaults to
unset, which keeps every transcript regardless of age. Setting it to `0`
excludes every transcript, no matter how recent, since none can be zero days
old at the moment it is discovered. A transcript that crosses the limit is
left out of the next index run rather than deleted — turning the limit back
off, or raising it, brings it back.

```toml
index_transcripts_max_age_days = 60
```

Codex CLI sessions are indexed the same way, from `$CODEX_HOME/sessions`
(`~/.codex/sessions` when `CODEX_HOME` is unset). They get the same
ten-minute settle rule, the same `index_transcripts_max_age_days` limit, the
same ranking weight and decay as Claude Code transcripts, and finished ones are
distilled into episodes too. Each one keeps what was said by you and by Codex,
plus a one-line marker for each tool call and the first 600 characters of its
output; the model's reasoning, and the instructions and environment context
Codex injects into every session, are left out. The graph and the Health tab
say which agent a session came from. `index_transcripts = false` turns off
both agents' sessions; to decline only Codex's:

```toml
index_codex_sessions = false
```

Codex's session file format is internal to Codex and undocumented, so br8n
reads it defensively: a line it does not recognise is skipped rather than
failing the file. Two Codex features take sessions out of that directory, and
br8n then drops them from the index at the next run: archiving a session
moves it to `archived_sessions`, and the experimental
`local_thread_store_compression` feature rewrites sessions older than seven
days as `.jsonl.zst`, which br8n does not read.

Some directories are skipped by default. `_templates` is the only one, and it
exists because an empty Obsidian template scores like a real note: a blank ADR
form measured 0.802 on one live vault, above the 0.66 hook gate, so a blank
form could be injected into a prompt as though it were a decision. The index
run prints how many files it skipped, so the exclusion is visible rather than
inferred from an absence.

```toml
ignore = ["_templates"]   # default; set to [] to index everything
```

It is matched against every path component, exactly and case-sensitively —
`_templates2` is a different directory, and `Templates` is not excluded, since
a vault may keep real notes under that name. **If you keep notes you want to
search inside a matching directory, empty this list**, and note that a
previously indexed document under one of these names is removed from the index
on the next run.

## Quality tiers

Retrieval runs at one of five tiers, each trading latency for accuracy. The
prompt hook defaults to `fast` (it fires on every turn and must stay quick);
the MCP tool defaults to `thorough` (Claude called it on purpose and can
afford to wait).

| Tier | Name | Candidates | BM25 | Graph expansion | Budget |
|---|---|---|---|---|---|
| 0 | instant | 5 | no | no | 150ms |
| 1 | fast | 20 | yes | no | 220ms |
| 2 | balanced | 30 | yes | 1 hop | 320ms |
| 3 | thorough | 40 | yes | 1 hop | 700ms |
| 4 | exhaustive | 80 | yes | 2 hops | 1600ms |

Higher tiers widen the candidate pool. They do not change how candidates are
ordered: every tier ranks by the same cosine similarity.

There is no LLM reranking stage. Ollama has no rerank endpoint, so a true
cross-encoder cannot be served, and the generative substitutes measured as
noise — `qwen3:0.6b` answers "yes" to every query/passage pair, `qwen3:4b`
rejects most of them. Ordering by the cosine took `thorough` from 0.80 to
1.00 recall and 706ms to 137ms.

Budgets are ceilings, not targets — every tier now returns in under 160ms. Each
sits at least twice the measured warm floor, because a ceiling that trips on a
healthy machine degrades silently instead of catching a genuinely slow query.

Set a surface's tier explicitly in config:

```toml
[hook]
quality = 2

[mcp]
quality = 4
```

### Ranking weights

Not all relevance is equal authority. A session transcript that discusses a
note is genuinely similar to a query about that note — and still the wrong
answer, because the note is the source of truth. Retrieval therefore
multiplies each hit's relevance by a per-source-type weight before gating and
ordering:

```toml
[weights]
transcript = 0.45   # default; measured: recall 0.71 -> 0.86 on a mixed corpus
authority  = 0.0    # opt-in link authority, see below

[mcp.weights]       # per-surface override, field by field
transcript = 0.85   # let Claude's own searches use a strong transcript
```

At the default weight, transcripts cannot clear the hook's 0.66 gate at all —
deliberate, since the hook fires unasked. Give `[mcp.weights]` a higher value
if Claude's deliberate searches should see them.

`authority` (default off) lifts well-linked notes by their inbound wikilink
count. It is a bounded lift, never a penalty: an unlinked note keeps exactly
1.0, the most-linked reaches `1.0 + authority`. Measured on a 100-case golden
set, `authority = 0.3` gains 0.02-0.03 recall at the middle tiers.

### Superseded decision records are demoted — ON by default

If a note's frontmatter says `status: superseded`, its chunks are multiplied by
`superseded` before the gate. This is the one weight here that does NOT ship at
1.0:

```toml
[weights]
superseded    = 0.88   # ON by default. Set to 1.0 to switch it off.
current       = 1.0
proposed      = 1.0    # keep at 1.0 — see below
investigating = 1.0
```

The problem it solves: a decision record you replaced still matches a
present-tense question about as well as its replacement does, so both were
being injected. Measured on one live vault, asking *"what do we use to build
and deliver software"* returned the superseded record at 0.7420 — above the
0.66 hook gate — directly beneath the record that replaced it. At 0.88 it
reads 0.6530 and is cut.

**The useful range is narrow: roughly 0.880 to 0.889 on that vault.** Below it,
questions that are genuinely about history stop finding the old record. Above
it, the old record stays above the gate. Both bounds were measured by running
the binary 8-12 times per case, and they are corpus-specific — if you change
this number, measure your own with `br8n bench` and `expect_absent` rather
than trusting these.

**Leave `proposed` at 1.0.** Most notes carry no `status:` key at all and land
there — 99.9% of chunks on the vault this was measured against. Anything below
1.0 demotes your whole corpus to adjust a handful of decision records.

Only `status: superseded` demotes. Every other value, and no value at all, is
treated as current.

### Embedding options

```toml
[embed]
model         = "qwen3-embedding:0.6b"
dimensions    = 512      # truncated from the model's native width, renormalized
concurrency   = 2        # parallel chunk/embed workers during indexing; default half the cores, 1 to 4
batch         = 32       # chunks per embedding request
chunk_tokens  = 512      # target chunk size (approximate: 4 chars/token)
prefix_scheme = "qwen3"  # qwen3 | nomic | e5 | plain; usually auto-detected
keep_alive    = "30m"    # how long Ollama keeps the model resident after a request
```

Changing `model`, `dimensions`, `chunk_tokens`, or the prefix scheme
invalidates the index; `br8n` detects the mismatch and refuses to mix
embedding spaces — rebuild with `br8n index --reindex`.

### Low-memory machines

The embedding model is the biggest resident cost in the whole pipeline —
bigger than `br8n` itself, which peaks around 69 MB. Ollama keeps it loaded
for `keep_alive` after every request (default `30m`; ~2.4 GB for the default
model), whether or not another prompt arrives. `br8n doctor` warns when the
machine has under 8 GiB of total memory.

Two settings bring the footprint down:

```toml
[embed]
keep_alive = "5m"    # the model unloads sooner; a cold reload back in costs
                      # about 2s on the first prompt after it expires
contextual = false    # default — leave off, since `true` loads a second
                       # model (`[embed] enrich_model`) during indexing
```

Do not lower `[hook] quality` to save memory. It saves none that has been
measured, and the `instant` tier costs recall: 0.70 against 0.90 for the
default tier on the project's own benchmark.

`br8n doctor` reports total memory and the configured `keep_alive` on every
run, so it is the way to check the setting took effect.

### Embedding somewhere other than local Ollama

br8n embeds against local Ollama by default. To use an OpenAI-compatible
endpoint instead — LM Studio, vLLM, anything serving `/v1/embeddings` — create
a file named `env` beside your `config.toml` (`br8n status` prints where that
is), readable only by you:

```bash
install -m 600 /dev/null "<the directory br8n status shows>/env"
```

```
BR8N_EMBED_URL=http://gpu-box:1234
BR8N_EMBED_MODEL=text-embedding-qwen3-embedding-0.6b
BR8N_EMBED_TOKEN=your-token
```

The file holds a token, so br8n refuses to read it unless it is mode `0600` or
tighter. br8n reads the file itself rather than relying on your shell, because
the hook runs in a process that never sees your shell profile. A variable set
in the process environment overrides the same key in the file.

Setting `BR8N_EMBED_URL` switches embedding **entirely** to that endpoint:
`ollama_url` is ignored for embedding and nothing falls back to localhost, so a
failure is reported rather than silently served by a different model. Features
that run an Ollama *chat* model — episode distillation for memory, contextual
enrichment — still use `ollama_url`, so leave Ollama running if
you use them.

A remote server may unload an idle model and take longer to reload it than a
prompt can wait. br8n loads it in the background at session start, and again
when a prompt times out; that prompt reports `loading it in the background` on
stderr and injects nothing, and the next one finds the model ready.

Run `br8n doctor` after any change. It checks the file, the endpoint, the
token, the model, the dimensions, and whether the endpoint still agrees with
the vectors already in your index, and exits non-zero if anything fails.

`BR8N_EMBED_MODEL` is the name the server uses. The identity stamped into the
index stays `[embed] model` from `config.toml`, so pointing br8n at another
server running the same model does not force a re-index. `br8n doctor`'s
agreement check is what tells you whether it really is the same model.

### Scanned PDFs

PDFs with a text layer are read directly. Pages without one are recognized
with OCR, which is on by default and runs ONLY on pages the detector flags —
a text PDF costs nothing and never loads the recognition machinery.

Recognition does not hold up your index. `br8n index` runs two passes: the
first reads every PDF from its text layer alone and publishes, so search works
immediately; the second runs afterwards, recognizes the pages the first pass
could not read, and republishes. A PDF that owes pages deliberately keeps its
previous fingerprint until it has been recognized, which is how the second
pass knows what to re-read. A corpus with no scanned pages never starts the
second pass at all.

```toml
[pdf]
ocr                = "auto"      # auto | off | force
ocr_min_confidence = 0.5         # drop weaker recognition rather than index noise
dpi                = 150         # rasterization resolution for pages routed to OCR
model_downloads    = "if-missing"  # if-missing | offline
```

`dpi` sits just under a cost cliff and that is why it is exposed. Detection
abandons its 960px pass once a page's longest side passes 1920px and runs a
2560px detector instead, which costs substantially more per page — an A4 page
crosses at 164 DPI and US Letter at 174. Lowering it buys less than it looks,
because only rasterization scales with DPI; `br8n` warns once, on the run
that actually recognizes something, if your setting crosses the threshold.

`model_downloads = "offline"` refuses to fetch a missing OCR model rather than
reaching for the network. Use it with `model_dir` pointing at a model set you
placed yourself:

```toml
[pdf]
model_dir       = "/path/to/ocr-models"
model_downloads = "offline"
```

`ocr = "off"` restores the pre-OCR behaviour exactly: scanned pages are
reported and skipped, nothing is queued, and there is no second pass.

**Migrating a corpus indexed before two-pass ingestion:** run
`br8n index --reindex` once. An ordinary run stats the corpus against the
previous run's fingerprints, and a PDF indexed by an older binary already has
a current one — so it takes the unchanged fast path, is never re-read, and the
second pass never learns it owes recognition. `--reindex` discards those
fingerprints and reads everything again.

Recognition needs two native libraries that `br8n` does not bundle: PDFium
and ONNX Runtime. On macOS they are not discoverable by default even once
installed, because `/opt/homebrew/lib` is not on the dynamic loader's search
path, so `br8n` probes the usual prefixes itself. If yours live elsewhere:

```toml
[pdf]
pdfium_lib_path = "/path/to/libpdfium.dylib"
ort_dylib_path  = "/path/to/libonnxruntime.dylib"
```

`br8n status` reports which of the two it can find, so you can see OCR is
unavailable before a scan goes missing from search rather than after.

Without them, `br8n` says so once and falls back to text-layer extraction for
the rest of the run — scanned pages are skipped and reported, exactly as they
were before OCR existed. The first recognized document also downloads a model,
which takes around twenty seconds.

### Calibrating the injection threshold

The prompt hook only injects a match when its relevance clears a
threshold — 0.66 by default, calibrated against a golden set of a few
hundred cases. The right number depends on your embedding
model and your own documents, not on this README. Run:

```
br8n bench
```

against your actual corpus (or `/br8n-bench` from Claude Code) and set
`threshold` from what it reports. When relevant and irrelevant results overlap
it says so and keeps the default rather than inventing a number:

```toml
[hook]
threshold = 0.80
```

### The golden set

`br8n bench` measures against a golden set — queries, and the document each one
should find — at `golden.toml` beside your config, or wherever `BR8N_GOLDEN`
points. You do not have to write it by hand:

```
br8n golden init     # a starter set, seeded with real documents from YOUR index
br8n golden check    # every problem in it, reported at once
```

`init` is worth using even if you would rather write the file yourself, because
the expensive part is not the schema: every case must name a uri that resolves
in your own index, and one that does not scores 0 forever while looking exactly
like a retrieval miss. `init` takes up to 25 real uris and titles from the
index, spread across it, and writes them as commented-out stubs. The file
therefore parses to zero cases and `br8n bench` refuses it until you fill some
in — deliberately, because a case with an empty query is a valid case that gets
scored, and on a small corpus an empty query scores as a HIT.

`check` reports the faults that make a measurement mean nothing rather than mean
less: a uri that is no longer in the index, two cases asking the same question,
a case that names no document, a query that repeats its own target's title (the
title was indexed, so such a query is scored on string matching and keeps
passing after a change that broke everything else), and too few `expect_none`
cases to calibrate the threshold above. Read its last line: when a check did not
run — an index it could not open, or a file holding no cases — it says the
result is NOT a clean bill of health.

Keep adding cases as retrieval misses things. One caution: a recall figure
belongs to the golden set AND the corpus it was measured against, so editing
either makes the previous number incomparable. `br8n bench` records a hash of
the file with every run and tells you when it moved.
