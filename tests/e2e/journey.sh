#!/bin/bash
# End-to-end: a user's journey from an empty directory to injected context.
#
# This exists because the unit suite cannot see what it covers. It runs the real
# binary against a real corpus and checks the things that only fail in
# composition: that the pack is published with the database, that tier 1 answers
# with the database file unreadable, that a corrupt pack REFUSES instead of
# returning an honest-looking empty result, and that `--reindex` does not empty
# the index.
#
# Two of those were live defects found this way. `br8n index --reindex` silently
# emptied an index while being the exact remedy every pack refusal message told
# users to run. And `br8n search` swallowed a refused pack with
# `unwrap_or_default()`, so the one command a user reaches for to ask "why didn't
# the hook inject X" answered "nothing found".
#
# A third was a live outage this file could NOT have caught, because every
# section built its own pack with the binary under test and so never read one
# written by an older binary. Section 10 closes that by simulating the upgrade.
#
# Needs a running Ollama, so it is not part of `cargo test`. Run it before a
# release, and after anything that touches the pack, the swap, or the read path.
#
#   ./tests/e2e/journey.sh
#
set -u
BIN="${BR8N_BIN:-${CARGO_TARGET_DIR:-$(cd "$(dirname "$0")/../.." && pwd)/target}/release/br8n}"
T=$(mktemp -d); mkdir -p "$T/notes"
pass=0; fail=0
ok () { if [ "$2" = "$3" ]; then echo "  PASS  $1"; pass=$((pass+1)); else echo "  FAIL  $1 (got '$2', want '$3')"; fail=$((fail+1)); fi; }
okge () { if [ "$2" -ge "$3" ] 2>/dev/null; then echo "  PASS  $1 ($2)"; pass=$((pass+1)); else echo "  FAIL  $1 (got '$2', want >= $3)"; fail=$((fail+1)); fi; }

cat > "$T/notes/pooling.md" <<'EOF'
# Connection Pooling

We use PgBouncer in transaction pooling mode to reduce Postgres backend
connections. Applications must retry with backoff when the pooler drops idle
sessions under load.
EOF
cat > "$T/notes/baking.md" <<'EOF'
# Sourdough

A long cold ferment develops flavour. The starter needs feeding twice daily at
room temperature before a bake.
EOF
cat > "$T/notes/rust.md" <<'EOF'
# Error Handling in Rust

Prefer anyhow::Result at boundaries and thiserror for library errors. Never
unwrap in a code path a user can reach.
EOF
printf 'index_transcripts = false\nsources = ["%s/notes"]\n\n[hook]\nquality = 1\nthreshold = 0.70\nmax_tokens = 1500\n\n[embed]\nollama_url = "http://localhost:11434"\n\n[weights]\nauthority = 0.0\n' "$T" > "$T/config.toml"
export BR8N_DB="$T/db" BR8N_CONFIG="$T/config.toml"

echo "=== 1. index a fresh corpus ==="
$BIN index >/dev/null 2>&1
ok "documents indexed" "$($BIN status 2>/dev/null | awk '/^documents/{print $2}')" "3"
ok "pack files published" "$(ls "$T/db" | grep -c '^pack\.')" "8"
ok "manifest format" "$(python3 -c "import json;print(json.load(open('$T/db/pack.manifest'))['format'])")" "4"
ok "manifest rows == chunks" "$(python3 -c "import json;print(json.load(open('$T/db/pack.manifest'))['rows'])")" "$($BIN status 2>/dev/null | awk '/^chunks/{print $2}')"

echo "=== 2. search finds the right document ==="
TOP=$($BIN search --quality 1 --json "how do we handle database connections" 2>/dev/null | python3 -c "import sys,json;r=json.load(sys.stdin)['results'];print(r[0]['uri'].split('/')[-1] if r else 'NONE')")
ok "top hit for a pooling question" "$TOP" "pooling.md"
TOP2=$($BIN search --quality 1 --json "how long should dough rest" 2>/dev/null | python3 -c "import sys,json;r=json.load(sys.stdin)['results'];print(r[0]['uri'].split('/')[-1] if r else 'NONE')")
ok "top hit for a baking question" "$TOP2" "baking.md"

echo "=== 3. BM25 works: an exact rare term ==="
okge "PgBouncer found by keyword" "$($BIN search --quality 1 --json "PgBouncer" 2>/dev/null | python3 -c "import sys,json;print(len(json.load(sys.stdin)['results']))")" 1

echo "=== 4. the hook injects ==="
HOOK=$(printf '{"prompt":"what do we do about database connection pooling"}' | $BIN hook prompt 2>/dev/null)
# Take the status IMMEDIATELY, before anything else runs. This assertion used to
# read `$?` on the line below the `okge`, so it reported that shell function's
# status — always 0, because its last command is an arithmetic assignment. It
# passed against a process that exited 42. An assertion that cannot fail is
# worse than no assertion: it reads as coverage of the one thing the hook must
# never get wrong.
HOOKCODE=$?
okge "hook returned context" "$(printf '%s' "$HOOK" | grep -c 'br8n-context')" 1
ok "hook exit code" "$HOOKCODE" "0"

echo "=== 5. tier 1 opens NO database ==="
chmod 000 "$T/db/graph.kz"
HOOK2=$(printf '{"prompt":"what do we do about database connection pooling"}' | $BIN hook prompt 2>/dev/null)
okge "hook injects with graph.kz unreadable" "$(printf '%s' "$HOOK2" | grep -c 'br8n-context')" 1
chmod 644 "$T/db/graph.kz"

echo "=== 6. a corrupt pack REFUSES rather than degrading ==="
cp "$T/db/pack.manifest" "$T/mf.bak"
python3 - "$T/db/pack.manifest" <<'PY'
import json,sys
p=sys.argv[1]; m=json.load(open(p)); m["analyzer"]="wrong/9"; json.dump(m,open(p,"w"))
PY
ERR=$($BIN search --quality 1 "pooling" 2>&1 >/dev/null | grep -ci "analyzer")
okge "analyzer mismatch surfaces an error" "$ERR" 1
cp "$T/mf.bak" "$T/db/pack.manifest"

echo "=== 7. incremental re-index is a no-op ==="
OUT=$($BIN index 2>&1 | tail -1)
okge "no-op run skips everything" "$(echo "$OUT" | grep -c 'skipped')" 1

echo "=== 8. --reindex preserves the corpus ==="
$BIN index --reindex >/dev/null 2>&1
ok "documents survive --reindex" "$($BIN status 2>/dev/null | awk '/^documents/{print $2}')" "3"
ok "pack rebuilt after --reindex" "$(ls "$T/db" | grep -c '^pack\.')" "8"

echo "=== 9. publish-first, embed-after does not destroy the index ==="
# `--no-embed` writes chunks with a NULL embedding; `--backfill` then SETs each
# one. While the `chunk_vec` HNSW index was allowed to exist over NULL rows,
# that SET made lbug compute cosine against a null pointer and SIGSEGV inside a
# write transaction, leaving `documents: 0, chunks: 0` while `br8n search` kept
# answering from the surviving pack, so nothing told the user.
#
# HONESTY NOTE, so nobody mistakes this for crash coverage: three notes are far
# too few to reproduce that. The crash needs a dense enough HNSW graph for
# `shrinkForNode` to reach an unembedded neighbour — 1,500 chunks died every
# time, 450 chunks did not die at all. This section pins the WORKFLOW (a backlog
# appears, keyword search works without any embedding, the backlog drains,
# `pack.vec` appears) and would have passed against the broken binary. The
# crash itself is pinned by `tests/it/store.rs`'s
# `store_open_drops_a_legacy_chunk_vec_index`, which builds a database
# carrying `chunk_vec` directly and asserts `Store::open` removes it — the
# guards named above are gone from this binary, and it is that removal, run
# once per open rather than per write, that keeps a NULL embedding from ever
# sharing the table with the index again.
T2=$(mktemp -d); mkdir -p "$T2/notes"
cp "$T/notes"/*.md "$T2/notes/" 2>/dev/null
printf 'index_transcripts = false\nsources = ["%s/notes"]\n\n[embed]\nollama_url = "http://localhost:11434"\n\n[weights]\nauthority = 0.0\n' "$T2" > "$T2/config.toml"
BR8N_DB="$T2/db" BR8N_CONFIG="$T2/config.toml" $BIN index --no-embed >/dev/null 2>&1
PEND=$(BR8N_DB="$T2/db" BR8N_CONFIG="$T2/config.toml" $BIN status 2>/dev/null | grep -c 'vectors pending')
okge "phase 1 publishes a backlog" "$PEND" 1
ok "phase 1 keyword search works" \
   "$(BR8N_DB="$T2/db" BR8N_CONFIG="$T2/config.toml" $BIN search --quality 1 --json PgBouncer 2>/dev/null | python3 -c "import sys,json;print('yes' if json.load(sys.stdin)['results'] else 'no')")" \
   "yes"
BR8N_DB="$T2/db" BR8N_CONFIG="$T2/config.toml" $BIN index --backfill >/dev/null 2>&1
ok "the corpus SURVIVES the backfill" \
   "$(BR8N_DB="$T2/db" BR8N_CONFIG="$T2/config.toml" $BIN status 2>/dev/null | awk '/^documents/{print $2}')" "3"
ok "the backlog is drained" \
   "$(BR8N_DB="$T2/db" BR8N_CONFIG="$T2/config.toml" $BIN status 2>/dev/null | grep -c 'vectors pending')" "0"
ok "pack.vec appears once embedded" "$(ls "$T2/db" | grep -c '^pack\.vec$')" "1"
rm -rf "$T2"

echo "=== 10. a pack written by an OLDER binary refuses, and --compact repairs it ==="
# Every section above builds its pack with the binary under test, so nothing in
# this file has ever read a pack written by a DIFFERENT one. That gap cost an
# outage: the manifest FORMAT went 3 -> 4, a binary expecting 4 correctly
# refused a format-3 pack, and installing it before migrating the index left
# the prompt hook refusing EVERY prompt until a compaction finished.
#
# Patching the manifest's `format` is the only way to produce such a pack
# without keeping a second, older binary around to write one. The number is
# READ from the manifest and decremented, never written as a literal 3, so
# this section does not rot at the next format bump.
#
# Three halves to the contract, and all three matter. It REFUSES (a pack this
# binary cannot read would otherwise attach plausible relevance to the wrong
# document), it says so LOUDLY on stderr, and it still EXITS 0. Those last two
# are not in tension: the hook must never block a prompt, so exiting 0 is the
# contract and stderr is the only channel left to admit the refusal — silence
# plus exit 0 is precisely the house failure mode. Then the CHEAP remedy the
# message names, `--compact`, must actually repair it: it rebuilds the pack
# from rows that are already embedded, so it costs no Ollama time.
#
# Reuses the index the sections above built rather than indexing a fresh
# corpus: the refusal is decided by the manifest before a single pack file is
# opened, so a second corpus would pin nothing extra and would cost a full
# embed pass, and `--compact` rebuilds from the DATABASE — which is exactly
# what that index already is.
#
# HONESTY NOTE: this patches a manifest, it does not run an older binary. It
# pins the REFUSE-AND-REPAIR contract, not the on-disk layout of a genuine
# format-3 pack — every other file beside this manifest is a current one, so
# nothing here shows what happens to a reader that gets past the manifest and
# meets older bytes. (`validate` runs before any file is opened, which is why
# it does not have to.) It says nothing about the other `validate` branches
# (`model_id`, `dims`, `rows_with_vectors`; `analyzer` is section 6's), nothing
# about a pack NEWER than the binary, and nothing about how long the repair
# takes: three notes compact instantly, while the live index took ~10 minutes,
# and it was that duration, not the refusal, that made the outage an outage.
cp "$T/db/pack.manifest" "$T/fmt.bak"
FMT=$(python3 -c "import json;print(json.load(open('$T/db/pack.manifest'))['format'])")
python3 - "$T/db/pack.manifest" <<'PY'
import json,sys
p=sys.argv[1]; m=json.load(open(p)); m["format"]=m["format"]-1; json.dump(m,open(p,"w"))
PY
ok "the pack now claims the previous format" \
   "$(python3 -c "import json;print(json.load(open('$T/db/pack.manifest'))['format'])")" "$((FMT-1))"
# One invocation, three channels: stdout and stderr each to their own file and
# the status read straight off the command. `$?` taken after a `$(...)` has run
# reports the substitution, not the thing under test.
printf '{"prompt":"what do we do about database connection pooling"}' > "$T/hook.in"
$BIN hook prompt < "$T/hook.in" > "$T/hook.out" 2> "$T/hook.err"
HOOKCODE=$?
ok "hook exits 0 on a pack it refuses" "$HOOKCODE" "0"
okge "hook stderr names the format it refused" "$(grep -c "pack format $((FMT-1))" "$T/hook.err")" 1
okge "the refusal names the cheap remedy" "$(grep -c -- "--compact" "$T/hook.err")" 1
ok "hook injects nothing rather than guessing" "$(grep -c 'br8n-context' "$T/hook.out")" "0"
# `br8n search` reaches the pack through its own copy of that decision, and it
# is the command a user runs to ask why the hook injected nothing — it once
# swallowed a refused pack entirely (see the header).
$BIN search --quality 1 --json "PgBouncer" > "$T/refused.out" 2> "$T/refused.err"
SEARCHCODE=$?
ok "search exits 0 on a pack it refuses" "$SEARCHCODE" "0"
okge "search stderr names the format it refused" "$(grep -c "pack format $((FMT-1))" "$T/refused.err")" 1
$BIN index --compact > "$T/compact.out" 2>&1
COMPACTCODE=$?
ok "--compact exits 0" "$COMPACTCODE" "0"
# Both lines, because either alone is weak: a no-op incremental run also exits
# 0 and also reports `0 written`, and only the before/after size line is proof
# a compaction happened at all.
okge "--compact really compacted" "$(grep -c 'compacted:' "$T/compact.out")" 1
okge "--compact re-embeds nothing" "$(grep -c '0 written' "$T/compact.out")" 1
ok "the pack is readable again" \
   "$(python3 -c "import json;print(json.load(open('$T/db/pack.manifest'))['format'])")" "$FMT"
ok "search works again after the repair" \
   "$($BIN search --quality 1 --json "how do we handle database connections" 2>/dev/null | python3 -c "import sys,json;r=json.load(sys.stdin)['results'];print(r[0]['uri'].split('/')[-1] if r else 'NONE')")" \
   "pooling.md"
$BIN hook prompt < "$T/hook.in" > "$T/hook2.out" 2>/dev/null
okge "the hook injects again after the repair" "$(grep -c 'br8n-context' "$T/hook2.out")" 1
# If the repair did NOT happen, put the original manifest back, so a section
# added after this one is not run against wreckage this one made. Unlike
# section 6's restore this is conditional: on the passing path `--compact` has
# already rewritten the manifest, and the backup describes the pack files that
# compaction replaced.
[ "$(python3 -c "import json;print(json.load(open('$T/db/pack.manifest'))['format'])")" = "$FMT" ] \
  || cp "$T/fmt.bak" "$T/db/pack.manifest"

# ---------------------------------------------------------------------------
# 11. A text-only corpus never starts the OCR pass.
#
# The second pass costs another seed copy, another stat walk and another full
# pack build, so the guard that keeps it off an ordinary corpus is the part
# worth pinning. Recognition itself is not asserted here: it needs PDFium and
# ONNX Runtime, which this script must run without. The observable is the
# stderr line `src/index.rs` prints BEFORE OCR is attempted — its ABSENCE is
# the assertion, so this section needs neither library installed and does not
# care whether recognition would succeed.
#
# Uses its own scratch corpus, config and database rather than the ones the
# sections above built and exported: `paper.pdf` is a clean text PDF and owes
# no OCR, which is exactly what makes it the right fixture for proving the
# second pass does NOT start, and it must not be reached through the repo
# path from inside a temp-dir test.
# ---------------------------------------------------------------------------
echo "=== 11. the OCR pass stays off a corpus that owes nothing ==="

mkdir -p "$T/pdfsrc"
cp "$(cd "$(dirname "$0")/../.." && pwd)/tests/fixtures/corpus/paper.pdf" "$T/pdfsrc/paper.pdf"
printf '# A note\n\nSome body text about connection pooling.\n' > "$T/pdfsrc/note.md"
cat > "$T/pdfcfg.toml" <<CFG
sources = ["$T/pdfsrc"]
index_transcripts = false
CFG

BR8N_CONFIG="$T/pdfcfg.toml" BR8N_DB="$T/pdfdb" \
  $BIN index > "$T/phase1.out" 2> "$T/phase1.err"
IDXCODE=$?
ok "index exits 0 on a text-only corpus" "$IDXCODE" "0"
ok "no OCR pass is started" \
  "$(grep -c 'running the OCR pass' "$T/phase1.err")" "0"
ok "both files were indexed" \
  "$(BR8N_CONFIG="$T/pdfcfg.toml" BR8N_DB="$T/pdfdb" $BIN status 2>/dev/null | awk '/^documents/{print $2}')" "2"

# A clean text PDF must not be re-read on the next run: if phase 1 wrongly
# queued it, its stamp was withheld and the corpus would never settle — this
# run matters as much as the first.
BR8N_CONFIG="$T/pdfcfg.toml" BR8N_DB="$T/pdfdb" \
  $BIN index > "$T/phase2.out" 2> "$T/phase2.err"
RERUNCODE=$?
ok "a second run exits 0" "$RERUNCODE" "0"
ok "the second run still starts no OCR pass" \
  "$(grep -c 'running the OCR pass' "$T/phase2.err")" "0"

# ---------------------------------------------------------------------------
# 12. `br8n update` against a local release mirror, and the dashboard's
#     Update button behind it.
#
# No network: a python http.server plays GitHub. The mirror first serves the
# binary's OWN version (update says up to date), then claims v99.0.0 over the
# SAME bytes. The updater must download, verify, extract, and then REFUSE at
# the version proof, leaving the installed binary byte-identical — that guard
# is what stops a mislabelled asset from being installed. Finally the
# dashboard's POST spawns the same updater detached and its status file ends
# `failed` for the same reason: that is the spawn path no cargo test can run.
# ---------------------------------------------------------------------------
echo "=== 12. br8n update against a local mirror, and the dashboard button ==="
U=$(mktemp -d); mkdir -p "$U/root/bin" "$U/m/dl" "$U/m/releases" "$U/stub" "$U/link"
cp "$BIN" "$U/root/bin/br8n"
V=$("$BIN" --version | awk '{print $2}')
# THIS SANDBOX IS LOAD-BEARING, and it is here because its absence did real
# damage. `br8n update` hands over to `br8n install`, and `install` registers
# the plugin by shelling out to the REAL `claude` CLI and symlinking `br8n`
# onto the REAL PATH. With the version guard intact `update` refuses before it
# gets there — but a mutation check deletes that guard on purpose, which is a
# documented practice in this repo. When that happened, this section re-pointed
# the developer's own `br8n` marketplace at $U, left ~/.local/bin/br8n
# dangling into a deleted temp directory, and rewrote the live plugin's
# hooks.json to a binary that no longer existed. The prompt hook exits 0 by
# design, so it went silently dead in every session until someone noticed.
#
# A stub `claude` first on PATH and BR8N_LINK_DIR pointed inside $U keep the
# blast radius inside the temp directory whatever the binary under test does.
cat > "$U/stub/claude" <<'STUB'
#!/bin/sh
echo "$*" >> "$(dirname "$0")/calls"
case "$*" in
    *--json) echo '[]' ;;
esac
exit 0
STUB
chmod +x "$U/stub/claude"
PATH="$U/stub:$PATH"
export BR8N_LINK_DIR="$U/link"
# Derived from uname, the way scripts/install.sh derives it, NOT by grepping
# the binary for a triple-shaped string. That grep returned EMPTY here, so the
# fixture published `br8n-.tar.gz`, `update` failed at ASSET LOOKUP, and every
# assertion below passed without the download, the checksum, the extraction or
# the version proof ever running. The guard on the next line is what stops that
# returning silently.
case "$(uname -s)" in
    Darwin) OS_PART=apple-darwin ;;
    Linux)  OS_PART=unknown-linux-gnu ;;
    *)      OS_PART=unsupported ;;
esac
case "$(uname -m)" in
    arm64|aarch64) ARCH_PART=aarch64 ;;
    x86_64|amd64)  ARCH_PART=x86_64 ;;
    *)             ARCH_PART=unsupported ;;
esac
TARGET="$ARCH_PART-$OS_PART"
ok "the fixture knows this platform's target triple" "$(echo "$TARGET" | grep -c unsupported)" "0"
# COPYFILE_DISABLE=1, exactly as release.yml packages the real assets: without
# it macOS tar stores the binary's extended attributes as a second `._br8n`
# entry, extract_single correctly refuses the two-entry archive, and the run
# never reaches the version proof this section exists to exercise.
(cd "$U/m/dl" && COPYFILE_DISABLE=1 tar -czf "br8n-$TARGET.tar.gz" -C "$U/root/bin" br8n && shasum -a 256 "br8n-$TARGET.tar.gz" > "br8n-$TARGET.sha256")
PORT=$((20000 + $$ % 10000))
mirror () { # version
python3 - "$U/m" "$1" "$PORT" <<'PY'
import json,sys,os
m,v,port=sys.argv[1:]
assets=[{"name":n,"url":f"http://127.0.0.1:{port}/dl/{n}"} for n in os.listdir(f"{m}/dl")]
json.dump({"tag_name":f"v{v}","html_url":"http://127.0.0.1/rel","assets":assets},open(f"{m}/releases/latest","w"))
PY
}
mirror "$V"
(cd "$U/m" && python3 -m http.server "$PORT" >/dev/null 2>&1 &)
sleep 1
export BR8N_DB="$U/root/db" BR8N_RELEASE_API="http://127.0.0.1:$PORT"
"$U/root/bin/br8n" update --check > "$U/check.out" 2>&1
ok "update --check exits 0 when current" "$?" "0"
okge "update --check says up to date" "$(grep -c 'up to date' "$U/check.out")" 1
mirror "99.0.0"
# The tarball holds a byte-identical copy of the installed binary, so a sha256
# comparison cannot tell "refused" from "installed" — it reads the same either
# way, and an earlier draft of this section asserted exactly that and passed
# with the version guard deleted. The INODE is the discriminator: place_binary
# unlinks and renames, so any install at all gives the file a new one.
#
# The version number is not a discriminator either. `99.0.0` appears in the
# success message (`updated X -> 99.0.0`) as well as the refusal, and so does
# the installed version, since the success line names both. Only the phrase
# `refusing to install` is unique to the path that declines.
BEFORE=$(ls -i "$U/root/bin/br8n" | awk '{print $1}')
"$U/root/bin/br8n" update > "$U/update.out" 2>&1
UPDCODE=$?
okge "a mislabelled asset makes update exit non-zero" "$UPDCODE" 1
okge "the refusal says it is refusing" "$(grep -c 'refusing to install' "$U/update.out")" 1
okge "the refusal names the version it expected" "$(grep -c '99.0.0' "$U/update.out")" 1
ok "the installed binary is never replaced" "$(ls -i "$U/root/bin/br8n" | awk '{print $1}')" "$BEFORE"
ok "update.status ends failed" "$(python3 -c "import json;print(json.load(open('$U/root/update.status'))['phase'])")" "failed"
ok "staging is cleaned up" "$(ls "$U/root/tmp" 2>/dev/null | wc -l | tr -d ' ')" "0"
"$U/root/bin/br8n" dashboard --no-open > "$U/dash.out" 2>&1 &
DASH=$!
sleep 1
DURL=$(sed -n 's/^br8n dashboard: //p' "$U/dash.out" | head -1)
ok "dashboard POST /api/update is accepted" "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$DURL/api/update")" "202"
sleep 3
ok "the spawned update also ends failed" "$(python3 -c "import json;print(json.load(open('$U/root/update.status'))['phase'])")" "failed"
ok "GET /api/version reports the failed update" "$(curl -s "$DURL/api/version" | python3 -c "import sys,json;print(json.load(sys.stdin)['update']['ok'])")" "False"
ok "a foreign origin is refused" "$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'Origin: http://evil.example' "$DURL/api/update")" "409"
kill $DASH 2>/dev/null
pkill -f "http.server $PORT" 2>/dev/null
unset BR8N_RELEASE_API BR8N_LINK_DIR
export BR8N_DB="$T/db"
rm -rf "$U"

echo "=== 13. memory: saved now, injected on the next prompt, gone after forget ==="
"$BIN" memory add --kind fact --global "The pooling note says PgBouncer runs in transaction mode." > "$T/mem-add.out" 2> "$T/mem-add.err"
ok "memory add exits 0" "$?" "0"
ok "memory add reports saved" "$(grep -c '^Saved fact' "$T/mem-add.out")" "1"
MEMID=$(sed -n 's/^Saved fact \([0-9a-f]*\).*/\1/p' "$T/mem-add.out")
echo '{"prompt":"what mode does pgbouncer run in according to my notes"}' | "$BIN" hook prompt > "$T/mem-hook.out" 2> "$T/mem-hook.err"
HOOKCODE=$?
ok "hook exits 0 with a memory pack" "$HOOKCODE" "0"
ok "hook injects the memory without an index run" "$(grep -c 'memory: fact' "$T/mem-hook.out")" "1"
"$BIN" memory add --kind lesson --global "Never comment code unless asked." > /dev/null 2>&1
echo '{"source":"startup","cwd":"'"$PWD"'"}' | "$BIN" hook session-start > "$T/mem-ss.out" 2> "$T/mem-ss.err"
SSCODE=$?
ok "session-start exits 0" "$SSCODE" "0"
ok "session-start prints the lessons block" "$(grep -c '^<br8n-lessons>' "$T/mem-ss.out")" "1"
ok "session-start block is not json" "$(grep -c '^{' "$T/mem-ss.out")" "0"
"$BIN" memory forget "$MEMID" > /dev/null 2>&1
ok "memory forget exits 0" "$?" "0"
echo '{"prompt":"what mode does pgbouncer run in according to my notes"}' | "$BIN" hook prompt > "$T/mem-hook2.out" 2>/dev/null
ok "forgotten memory is no longer injected" "$(grep -c 'memory: fact' "$T/mem-hook2.out")" "0"

echo "=== 14. dashboard: a memory saved over HTTP is listed, drawn on the graph, and deleted ==="
"$BIN" dashboard --no-open > "$T/dash14.out" 2>&1 &
DASH=$!
sleep 1
DURL=$(sed -n 's/^br8n dashboard: //p' "$T/dash14.out" | head -1)
curl -s -o "$T/d14-list0.json" -w '%{http_code}' "$DURL/api/memories" > "$T/d14-list0.code" 2> "$T/d14-list0.err"
CODE=$?
ok "GET /api/memories: curl exits 0" "$CODE" "0"
ok "GET /api/memories answers 200" "$(cat "$T/d14-list0.code")" "200"
ok "GET /api/memories reports no failure" "$(python3 -c "import json;print(json.load(open('$T/d14-list0.json'))['unavailable'] is None)")" "True"
MEMS0=$(python3 -c "import json;print(len(json.load(open('$T/d14-list0.json'))['memories']))")
BODY14='{"kind":"fact","text":"The sourdough starter is fed twice daily at room temperature.","scope":"global"}'
curl -s -o "$T/d14-save.json" -w '%{http_code}' -X POST -H 'Content-Type: application/json' --data "$BODY14" "$DURL/api/memory/save" > "$T/d14-save.code" 2> "$T/d14-save.err"
CODE=$?
ok "POST /api/memory/save: curl exits 0" "$CODE" "0"
ok "POST /api/memory/save answers 200" "$(cat "$T/d14-save.code")" "200"
ID14=$(python3 -c "import json;print(json.load(open('$T/d14-save.json'))['id'] or '')")
okge "the save returns an id" "${#ID14}" 1
curl -s -o "$T/d14-list1.json" -w '%{http_code}' "$DURL/api/memories" > "$T/d14-list1.code" 2> "$T/d14-list1.err"
CODE=$?
ok "GET /api/memories after the save: curl exits 0" "$CODE" "0"
ok "the save added exactly one memory" "$(python3 -c "import json;print(len(json.load(open('$T/d14-list1.json'))['memories']))")" "$((MEMS0 + 1))"
curl -s -o "$T/d14-graph.json" -w '%{http_code}' "$DURL/api/graph" > "$T/d14-graph.code" 2> "$T/d14-graph.err"
CODE=$?
ok "GET /api/graph: curl exits 0" "$CODE" "0"
ok "the graph draws the saved memory as a memory node" "$(python3 -c "import json;print(sum(1 for n in json.load(open('$T/d14-graph.json'))['nodes'] if n.get('source_type')=='memory' and n.get('memory_id')=='$ID14'))")" "1"
curl -s -o "$T/d14-foreign.json" -w '%{http_code}' -X POST -H 'Content-Type: application/json' -H 'Origin: http://evil.example' --data "$BODY14" "$DURL/api/memory/save" > "$T/d14-foreign.code" 2> "$T/d14-foreign.err"
CODE=$?
ok "a foreign-origin save: curl exits 0" "$CODE" "0"
ok "a foreign-origin save is refused" "$(cat "$T/d14-foreign.code")" "409"
ok "the foreign-origin refusal names the origin, not a busy store" "$(grep -c 'refused: request origin' "$T/d14-foreign.json")" "1"
curl -s -o "$T/d14-delete.json" -w '%{http_code}' -X POST -H 'Content-Type: application/json' --data "{\"id\":\"$ID14\"}" "$DURL/api/memory/delete" > "$T/d14-delete.code" 2> "$T/d14-delete.err"
CODE=$?
ok "POST /api/memory/delete: curl exits 0" "$CODE" "0"
ok "POST /api/memory/delete answers 200" "$(cat "$T/d14-delete.code")" "200"
ok "the delete names the memory the section saved" "$(python3 -c "import json;print(json.load(open('$T/d14-delete.json')).get('id'))")" "$ID14"
curl -s -o "$T/d14-list2.json" -w '%{http_code}' "$DURL/api/memories" > "$T/d14-list2.code" 2> "$T/d14-list2.err"
CODE=$?
ok "GET /api/memories after the delete: curl exits 0" "$CODE" "0"
ok "the delete returns the count to where the section found it" "$(python3 -c "import json;print(len(json.load(open('$T/d14-list2.json'))['memories']))")" "${MEMS0:-unreadable}"
kill $DASH 2>/dev/null

echo
echo "=== $pass passed, $fail failed ==="
rm -rf "$T"
exit $fail
