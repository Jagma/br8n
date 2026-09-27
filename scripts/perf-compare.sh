#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "Usage: perf-compare.sh <base-binary> <head-binary>" >&2
  exit 1
fi

resolve() {
  cd "$(dirname "$1")" && pwd -P
}

BASE_BIN="$(resolve "$1")/$(basename "$1")"
HEAD_BIN="$(resolve "$2")/$(basename "$2")"

for bin in "$BASE_BIN" "$HEAD_BIN"; do
  if [ ! -x "$bin" ]; then
    echo "perf-compare: $bin is not an executable file" >&2
    exit 1
  fi
done

CHUNKS="${PERF_CHUNKS:-35000}"
SEED="${PERF_SEED:-42}"
PAIRS="${PERF_PAIRS:-7}"
LATENCY_SLOWDOWN_PCT=15
RSS_PCT=10
BYTES_PER_CHUNK_PCT=10
BUILD_MS_PCT=25

WORKDIR=$(mktemp -d "${TMPDIR:-/tmp}/br8n-perf-compare.XXXXXX")
trap 'rm -rf "$WORKDIR"' EXIT

export BR8N_CONFIG="$WORKDIR/no-such-config.toml"

set +e
BASE_HELP_OUTPUT=$("$BASE_BIN" bench --help 2>&1)
BASE_HELP_STATUS=$?
set -e

if [ "$BASE_HELP_STATUS" -ne 0 ]; then
  echo "perf-compare: $BASE_BIN bench --help exited $BASE_HELP_STATUS instead of running — cannot treat that as an old CLI without --reuse" >&2
  echo "$BASE_HELP_OUTPUT" >&2
  exit 1
fi

if ! echo "$BASE_HELP_OUTPUT" | grep -q -- '--reuse'; then
  echo "perf-compare: $BASE_BIN does not support bench --synthetic --reuse; cannot compare" >&2
  exit 0
fi

echo "perf-compare: load average $(python3 -c 'import os; print(", ".join("%.2f" % v for v in os.getloadavg()))')" >&2

echo "perf-compare: building the base pack ($CHUNKS chunks, seed $SEED, build-vs-build)" >&2
BASE_PACK="$WORKDIR/base-pack"
"$BASE_BIN" bench --synthetic "$CHUNKS" --seed "$SEED" --out "$BASE_PACK" --json > "$WORKDIR/base-build.json"

echo "perf-compare: building the head pack ($CHUNKS chunks, seed $SEED, build-vs-build)" >&2
HEAD_PACK="$WORKDIR/head-pack"
"$HEAD_BIN" bench --synthetic "$CHUNKS" --seed "$SEED" --out "$HEAD_PACK" --json > "$WORKDIR/head-build.json"

: > "$WORKDIR/base-queries.list"
: > "$WORKDIR/head-queries.list"
for i in $(seq 1 "$PAIRS"); do
  echo "perf-compare: reuse-vs-reuse pair $i/$PAIRS" >&2
  BASE_QUERY_JSON="$WORKDIR/base-query-$i.json"
  HEAD_QUERY_JSON="$WORKDIR/head-query-$i.json"
  "$BASE_BIN" bench --synthetic "$CHUNKS" --seed "$SEED" --reuse "$BASE_PACK" --json > "$BASE_QUERY_JSON"
  "$HEAD_BIN" bench --synthetic "$CHUNKS" --seed "$SEED" --reuse "$HEAD_PACK" --json > "$HEAD_QUERY_JSON"
  echo "$BASE_QUERY_JSON" >> "$WORKDIR/base-queries.list"
  echo "$HEAD_QUERY_JSON" >> "$WORKDIR/head-queries.list"
done

python3 - "$WORKDIR/base-build.json" "$WORKDIR/head-build.json" \
  "$WORKDIR/base-queries.list" "$WORKDIR/head-queries.list" \
  "$LATENCY_SLOWDOWN_PCT" "$RSS_PCT" "$BYTES_PER_CHUNK_PCT" "$BUILD_MS_PCT" <<'PY'
import json
import math
import statistics
import sys

(base_build_path, head_build_path, base_list_path, head_list_path,
 latency_pct, rss_pct, bytes_pct, build_pct) = sys.argv[1:9]
latency_pct = float(latency_pct)
rss_pct = float(rss_pct)
bytes_pct = float(bytes_pct)
build_pct = float(build_pct)

base_build = json.load(open(base_build_path))
head_build = json.load(open(head_build_path))

base_query_paths = open(base_list_path).read().split()
head_query_paths = open(head_list_path).read().split()
base_queries = [json.load(open(p)) for p in base_query_paths]
head_queries = [json.load(open(p)) for p in head_query_paths]


def tier1_p50(report):
    return next(t["p50_ms"] for t in report["tiers"] if t["tier"] == 1)


base_p50s = [tier1_p50(r) for r in base_queries]
head_p50s = [tier1_p50(r) for r in head_queries]
base_rss = [r["peak_rss_bytes"] for r in base_queries]
head_rss = [r["peak_rss_bytes"] for r in head_queries]

print(f"{'pair':<6}{'base p50 ms':<14}{'head p50 ms':<14}{'change':<10}{'slow?'}")
slow_pairs = 0
for i, (b, h) in enumerate(zip(base_p50s, head_p50s), start=1):
    change_pct = ((h - b) / b * 100.0) if b > 0 else 0.0
    is_slow = h > b * (1.0 + latency_pct / 100.0)
    slow_pairs += is_slow
    print(f"{i:<6}{b:<14.3f}{h:<14.3f}{change_pct:<10.1f}{is_slow}")

required_slow_pairs = math.ceil(len(base_p50s) * 5 / 7)
latency_failed = slow_pairs >= required_slow_pairs
print(
    f"tier-1 p50 (reuse-vs-reuse): {slow_pairs}/{len(base_p50s)} pairs over "
    f"{latency_pct:.0f}% slower (fails at {required_slow_pairs}+): "
    f"{'FAIL' if latency_failed else 'ok'}"
)

base_rss_median = statistics.median(base_rss)
head_rss_median = statistics.median(head_rss)
rss_failed = head_rss_median > base_rss_median * (1.0 + rss_pct / 100.0)
print(
    f"peak_rss_bytes median (reuse-vs-reuse): base {base_rss_median:.0f} "
    f"head {head_rss_median:.0f}: {'FAIL' if rss_failed else 'ok'}"
)

base_bytes_per_chunk = base_build["pack_bytes_per_chunk"]
head_bytes_per_chunk = head_build["pack_bytes_per_chunk"]
bytes_failed = head_bytes_per_chunk > base_bytes_per_chunk * (1.0 + bytes_pct / 100.0)
print(
    f"pack_bytes_per_chunk (build-vs-build): base {base_bytes_per_chunk} "
    f"head {head_bytes_per_chunk}: {'FAIL' if bytes_failed else 'ok'}"
)

base_build_rss = base_build["peak_rss_bytes"]
head_build_rss = head_build["peak_rss_bytes"]
build_rss_failed = head_build_rss > base_build_rss * (1.0 + rss_pct / 100.0)
print(
    f"peak_rss_bytes (build-vs-build, one build each): base {base_build_rss} "
    f"head {head_build_rss}: {'FAIL' if build_rss_failed else 'ok'}"
)

base_build_ms = base_build["pack_build_ms"]
head_build_ms = head_build["pack_build_ms"]
build_failed = head_build_ms > base_build_ms * (1.0 + build_pct / 100.0)
print(
    f"pack_build_ms (build-vs-build): base {base_build_ms} head {head_build_ms}: "
    f"{'FAIL' if build_failed else 'ok'}"
)

if latency_failed or rss_failed or build_rss_failed or bytes_failed or build_failed:
    print("perf-compare: FAIL")
    sys.exit(1)
print("perf-compare: PASS")
PY
