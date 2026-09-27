#!/bin/sh
# Smoke-tests a PACKAGED release tarball the way a fresh machine would run it.
#
#   sh scripts/release-smoke.sh br8n-<target>.tar.gz <version>
#
# Every assertion runs; each prints what it checked and what it saw; the exit
# status is 1 at the end if any failed. Needs no Ollama: the corpus is indexed
# with --no-embed and searched by keyword. Needs the network once, for lbug's
# `INSTALL vector` on the first store open (it writes under $HOME/.lbdb).
#
# Assertion 5 is the export-dynamic probe. A tier-2 search opens the store
# through `open_existing`, whose `LOAD EXTENSION vector` is a hard error, and
# without `-Wl,-export_dynamic` that load fails ("symbol not found in flat
# namespace"). Tier 1
# never opens the database, and `br8n index` swallows extension-load errors
# on the write path by design, so neither of those can catch it.
set -u

ARCHIVE="${1:?usage: release-smoke.sh <tarball> <version>}"
VERSION="${2:?usage: release-smoke.sh <tarball> <version>}"

FAILS=0
ok() { # label got want [output to show on failure]
    if [ "$2" = "$3" ]; then
        printf 'ok    %s: %s\n' "$1" "$2"
    else
        printf 'FAIL  %s: got [%s] want [%s]\n' "$1" "$2" "$3"
        [ -n "${4:-}" ] && printf '%s\n' "$4" | sed 's/^/      | /'
        FAILS=$((FAILS + 1))
    fi
}
# "found" or "missing": whether $1 contains the fixed string $2.
has() { printf '%s\n' "$1" | grep -qF -- "$2" && echo found || echo missing; }

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT

tar -xzf "$ARCHIVE" -C "$T" || { echo "FAIL  cannot untar $ARCHIVE"; exit 1; }
BIN="$T/br8n"
[ -x "$BIN" ] || { echo "FAIL  $ARCHIVE has no executable 'br8n' at its root"; exit 1; }

# 1. It runs at all, and it is the version we think it is.
ok "1 --version" "$("$BIN" --version 2>&1)" "br8n $VERSION"

# 2. macOS: nothing from a package manager's prefix. A binary that links
#    /opt/homebrew/opt/openssl@3/lib/libssl.3.dylib dies in dyld on any Mac
#    without Homebrew — before main, before any of our own error reporting.
if [ "$(uname -s)" = Darwin ]; then
    foreign="$(otool -L "$BIN" | awk 'NR > 1 { print $1 }' \
        | grep -E '^/(opt/homebrew|usr/local|opt/local)/' || true)"
    ok "2 otool -L has no package-manager paths" "${foreign:-none}" "none"
else
    echo "skip  2 otool -L (not macOS)"
fi

# A two-note corpus, indexed without embeddings against a closed port.
mkdir -p "$T/notes"
printf '# Kettle descaling\n\nDescale the kettle with citric acid every month.\n' \
    > "$T/notes/kettle.md"
printf '# Bicycle chain\n\nWax the bicycle chain after every wet ride.\n' \
    > "$T/notes/chain.md"
printf 'index_transcripts = false\nsources = ["%s/notes"]\n\n[embed]\nollama_url = "http://127.0.0.1:1"\n\n[weights]\nauthority = 0.0\n' \
    "$T" > "$T/config.toml"
export BR8N_CONFIG="$T/config.toml" BR8N_DB="$T/db"

# 3. Indexing publishes without Ollama.
OUT="$("$BIN" index --no-embed 2>&1)"
ok "3 index --no-embed" "$(has "$OUT" "indexed: 2 added")" "found" "$OUT"

# 4. Tier 1 answers from the pack: BM25 only, no database, no Ollama.
OUT="$("$BIN" search --quality 1 citric acid 2>&1)"
ok "4 search --quality 1" "$(has "$OUT" "Kettle descaling")" "found" "$OUT"

# 5. Tier 2 opens the database: the vector extension must load (see header).
OUT="$("$BIN" search --quality 2 citric acid 2>&1)"
ok "5 search --quality 2" "$(has "$OUT" "Kettle descaling")" "found" "$OUT"

# 6. The packaged binary was built with default features.
OUT="$("$BIN" --help 2>&1)"
ok "6 --help lists backup" "$(has "$OUT" "backup")" "found" "$OUT"

if [ "$FAILS" -eq 0 ]; then
    echo "release-smoke: 6 assertions, all passed"
    exit 0
fi
echo "release-smoke: $FAILS assertion(s) FAILED"
exit 1
