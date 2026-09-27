#!/usr/bin/env bash
# Usage: runtime-budget.sh <br8n-binary> [chunks=35000] [--budget <file>]
set -euo pipefail

usage() {
  echo "Usage: runtime-budget.sh <br8n-binary> [chunks=35000] [--budget <file>]" >&2
  exit 1
}

if [ "$#" -lt 1 ]; then
  usage
fi

BIN_INPUT="$1"
shift

CHUNKS=35000
BUDGET_FILE=""

if [ "$#" -gt 0 ] && [ "$1" != "--budget" ]; then
  CHUNKS="$1"
  shift
fi

if [ "$#" -gt 0 ]; then
  if [ "$1" = "--budget" ]; then
    shift
    if [ "$#" -lt 1 ]; then
      usage
    fi
    BUDGET_FILE="$1"
    shift
  fi
fi

if [ "$#" -gt 0 ]; then
  usage
fi

if [ -n "$BUDGET_FILE" ] && [ ! -f "$BUDGET_FILE" ]; then
  echo "runtime-budget: $BUDGET_FILE is not a file" >&2
  exit 1
fi

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
STUB="$SCRIPT_DIR/stub-embedder.py"
BIN_DIR=$(cd "$(dirname "$BIN_INPUT")" && pwd)
BIN="$BIN_DIR/$(basename "$BIN_INPUT")"

if [ ! -x "$BIN" ]; then
  echo "runtime-budget: $BIN is not an executable file" >&2
  exit 1
fi

WORKDIR=$(mktemp -d "${TMPDIR:-/tmp}/br8n-runtime-budget.XXXXXX")
STUB_PID=""

cleanup() {
  if [ -n "$STUB_PID" ]; then
    kill "$STUB_PID" >/dev/null 2>&1 || true
    wait "$STUB_PID" 2>/dev/null || true
  fi
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

DIMS=512
MODEL="qwen3-embedding:0.6b"
PORT=$(python3 -c 'import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()')

CFG="$WORKDIR/config.toml"
cat > "$CFG" <<CFGEOF
index_transcripts = false
sources = []

[embed]
model = "$MODEL"
dimensions = $DIMS
ollama_url = "http://127.0.0.1:$PORT"

[hook]
threshold = 0.0
CFGEOF

PACKDIR="$WORKDIR/pack"
export BR8N_CONFIG="$CFG"
export BR8N_DB="$PACKDIR"

echo "runtime-budget: building a synthetic pack of $CHUNKS chunks" >&2
"$BIN" bench --synthetic "$CHUNKS" --seed 42 --out "$PACKDIR" --json > "$WORKDIR/build.json"

python3 "$STUB" --port "$PORT" --dims "$DIMS" &
STUB_PID=$!

echo "runtime-budget: waiting for the stub embedder on port $PORT" >&2
READY=0
for _ in $(seq 1 100); do
  if python3 -c "
import socket
try:
  s = socket.create_connection(('127.0.0.1', $PORT), timeout=0.5)
  s.close()
  exit(0)
except Exception:
  exit(1)
" 2>/dev/null; then
    READY=1
    break
  fi
  sleep 0.1
done
if [ "$READY" -ne 1 ]; then
  echo "runtime-budget: stub embedder never came up on port $PORT" >&2
  exit 1
fi

PROMPT_JSON="$WORKDIR/prompt.json"
python3 - "$PACKDIR/pack.rec" "$PACKDIR/pack.recidx" "$PROMPT_JSON" <<'PY'
import json
import struct
import sys

rec_path, idx_path, out_path = sys.argv[1:4]
with open(idx_path, "rb") as f:
    idx = f.read()
offset_0, offset_1 = struct.unpack_from("<QQ", idx, 0)
with open(rec_path, "rb") as f:
    f.seek(offset_0)
    blob = f.read(offset_1 - offset_0)
record = json.loads(blob)
words = record["text"].split()[:6]
prompt = "please tell me about " + " ".join(words)
with open(out_path, "w") as f:
    json.dump({"prompt": prompt}, f)
PY

QUERY_TEXT=$(python3 -c 'import json, sys
print(json.load(open(sys.argv[1]))["prompt"])' "$PROMPT_JSON")

DRIVER="$WORKDIR/driver.py"
cat > "$DRIVER" <<'PYEOF'
import json
import resource
import subprocess
import sys
import time


def percentile(values, p):
    if not values:
        return 0.0
    ordered = sorted(values)
    idx = min(len(ordered) - 1, int(round(p * (len(ordered) - 1))))
    return ordered[idx]


def main():
    argv = sys.argv[1:]
    mode = argv.pop(0)
    count = int(argv.pop(0))
    stdin_file = None
    require_substring = None
    while argv and argv[0] != "--":
        flag = argv.pop(0)
        if flag == "--stdin-file":
            stdin_file = argv.pop(0)
        elif flag == "--require-substring":
            require_substring = argv.pop(0)
        else:
            sys.exit(f"driver: unknown flag {flag}")
    if argv and argv[0] == "--":
        argv.pop(0)
    cmd = argv
    stdin_data = None
    if stdin_file is not None:
        with open(stdin_file, "rb") as f:
            stdin_data = f.read()
    elapsed_ms = []
    failures = 0
    last_exit_code = None
    last_stdout = ""
    last_stderr = ""
    for _ in range(count):
        started = time.perf_counter()
        proc = subprocess.run(cmd, input=stdin_data, capture_output=True)
        elapsed_ms.append((time.perf_counter() - started) * 1000.0)
        last_exit_code = proc.returncode
        last_stdout = proc.stdout.decode("utf-8", "replace")
        last_stderr = proc.stderr.decode("utf-8", "replace")
        ok = proc.returncode == 0
        if ok and require_substring is not None:
            ok = require_substring in last_stdout
        if not ok:
            failures += 1
    if mode == "warmup":
        print(json.dumps({"failures": failures}))
        return
    ru = resource.getrusage(resource.RUSAGE_CHILDREN)
    peak_rss_bytes = ru.ru_maxrss * (1024 if sys.platform.startswith("linux") else 1)
    print(
        json.dumps(
            {
                "p50_ms": round(percentile(elapsed_ms, 0.50), 3),
                "p95_ms": round(percentile(elapsed_ms, 0.95), 3),
                "peak_rss_bytes": peak_rss_bytes,
                "failures": failures,
                "last_exit_code": last_exit_code,
                "last_stdout": last_stdout[:2000],
                "last_stderr": last_stderr[:2000],
            }
        )
    )


main()
PYEOF

jget() {
  python3 -c 'import json, sys
print(json.loads(sys.argv[1])[sys.argv[2]])' "$1" "$2"
}

echo "runtime-budget: measuring the hook (UserPromptSubmit)" >&2
python3 "$DRIVER" warmup 3 --stdin-file "$PROMPT_JSON" -- "$BIN" hook prompt > /dev/null
HOOK_RESULT=$(python3 "$DRIVER" measure 30 --stdin-file "$PROMPT_JSON" --require-substring additionalContext -- "$BIN" hook prompt)
HOOK_FAILURES=$(jget "$HOOK_RESULT" failures)
if [ "$HOOK_FAILURES" -ne 0 ]; then
  echo "runtime-budget: the hook failed or injected nothing on $HOOK_FAILURES/30 runs" >&2
  echo "runtime-budget: last exit code $(jget "$HOOK_RESULT" last_exit_code)" >&2
  echo "runtime-budget: last stderr: $(jget "$HOOK_RESULT" last_stderr)" >&2
  exit 1
fi

echo "runtime-budget: measuring br8n search at tier 1" >&2
python3 "$DRIVER" warmup 3 -- "$BIN" search "$QUERY_TEXT" --quality 1 --json > /dev/null
SEARCH_RESULT=$(python3 "$DRIVER" measure 30 -- "$BIN" search "$QUERY_TEXT" --quality 1 --json)
SEARCH_FAILURES=$(jget "$SEARCH_RESULT" failures)
if [ "$SEARCH_FAILURES" -ne 0 ]; then
  echo "runtime-budget: br8n search exited non-zero on $SEARCH_FAILURES/30 runs" >&2
  echo "runtime-budget: last stderr: $(jget "$SEARCH_RESULT" last_stderr)" >&2
  exit 1
fi

echo "runtime-budget: measuring br8n --version" >&2
python3 "$DRIVER" warmup 3 -- "$BIN" --version > /dev/null
VERSION_RESULT=$(python3 "$DRIVER" measure 30 -- "$BIN" --version)
VERSION_FAILURES=$(jget "$VERSION_RESULT" failures)
if [ "$VERSION_FAILURES" -ne 0 ]; then
  echo "runtime-budget: br8n --version exited non-zero on $VERSION_FAILURES/30 runs" >&2
  exit 1
fi

BINARY_BYTES=$(python3 -c 'import os, sys
print(os.path.getsize(sys.argv[1]))' "$BIN")

RESULT=$(python3 -c 'import json, sys

hook_result, search_result, version_result, binary_bytes, chunks = sys.argv[1:6]
hook = json.loads(hook_result)
search = json.loads(search_result)
version = json.loads(version_result)

print(
    json.dumps(
        {
            "chunks": int(chunks),
            "hook_p50_ms": hook["p50_ms"],
            "hook_p95_ms": hook["p95_ms"],
            "hook_peak_rss_bytes": hook["peak_rss_bytes"],
            "search_peak_rss_bytes": search["peak_rss_bytes"],
            "start_ms": version["p50_ms"],
            "binary_bytes": int(binary_bytes),
        }
    )
)
' "$HOOK_RESULT" "$SEARCH_RESULT" "$VERSION_RESULT" "$BINARY_BYTES" "$CHUNKS")

echo "$RESULT"

if [ -n "$BUDGET_FILE" ]; then
  python3 -c 'import json, sys

measured_json, budget_path = sys.argv[1:3]
measured = json.loads(measured_json)
with open(budget_path) as f:
    budget = json.load(f)

exceeded = []
for key, ceiling in budget.items():
    if key not in measured:
        continue
    value = measured[key]
    if value > ceiling:
        exceeded.append(f"{key}: {value} exceeds ceiling {ceiling}")

if exceeded:
    for line in exceeded:
        print(f"runtime-budget: over budget: {line}", file=sys.stderr)
    sys.exit(1)
' "$RESULT" "$BUDGET_FILE"
fi
