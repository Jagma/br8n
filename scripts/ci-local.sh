#!/usr/bin/env bash
set -uo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/ci-local.sh [--only job[,job]] [--skip job[,job]] [--perf <base-ref>]

Runs the jobs .github/workflows/ci.yml runs, on this machine.
Jobs: fmt clippy test targets msrv dashboard e2e perf
perf runs only with --perf, against the release binary built from <base-ref>.
USAGE
}

ALL_JOBS=(fmt clippy test targets msrv dashboard e2e perf)
ONLY=""
SKIP=""
PERF_BASE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --only) ONLY="$2"; shift 2 ;;
    --skip) SKIP="$2"; shift 2 ;;
    --perf) PERF_BASE="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage; exit 2 ;;
  esac
done

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
LOGS="$ROOT/target/ci-local"
mkdir -p "$LOGS"

if [ -d "$HOME/.local/node/bin" ]; then
  export PATH="$HOME/.local/node/bin:$PATH"
fi
CHROME_LIBS="$HOME/.local/chrome-libs/root/usr/lib/x86_64-linux-gnu"
if [ -d "$CHROME_LIBS" ]; then
  export LD_LIBRARY_PATH="$CHROME_LIBS${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi

selected() {
  local job="$1"
  if [ "$job" = perf ] && [ -z "$PERF_BASE" ]; then return 1; fi
  if [ -n "$ONLY" ] && [[ ",$ONLY," != *",$job,"* ]]; then return 1; fi
  if [ -n "$SKIP" ] && [[ ",$SKIP," == *",$job,"* ]]; then return 1; fi
  return 0
}

need_linux_node() {
  local node
  node="$(command -v node || true)"
  if [ -z "$node" ] || [[ "$node" == /mnt/* ]]; then
    echo "a Linux node 22 is required (found: ${node:-none}); install it to ~/.local/node" >&2
    return 1
  fi
}

job_fmt() { cargo fmt --check; }

job_clippy() {
  cargo clippy --all-targets -- -D warnings &&
    cargo clippy --all-targets --no-default-features -- -D warnings
}

job_test() {
  CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 cargo test --all
}

job_targets() { python3 scripts/check-test-targets.py; }

job_msrv() {
  if ! rustup toolchain list | grep -q '^1\.95'; then
    echo "SKIP: toolchain 1.95 is not installed (rustup toolchain install 1.95)"
    return 99
  fi
  cargo +1.95 check --all-targets --locked
}

job_dashboard() {
  need_linux_node || return 1
  (cd dashboard && npm ci && npx tsc --noEmit && npm run build) &&
    git diff --exit-code dashboard/dist
}

chrome_missing_libs() {
  local chrome
  chrome="$(find "$HOME/.cache/puppeteer" -type f -name chrome-headless-shell 2>/dev/null | sort | tail -1)"
  [ -n "$chrome" ] || return 1
  ldd "$chrome" 2>/dev/null | awk '/not found/ {print $1}'
}

job_e2e() {
  need_linux_node || return 1
  cargo build || return 1
  (cd dashboard/e2e && npm ci --no-audit --no-fund) || return 1
  local missing
  missing="$(chrome_missing_libs)"
  if [ -n "$missing" ]; then
    echo "headless Chrome cannot load: $missing" >&2
    echo "fix: sudo apt-get install -y libnss3 libnspr4 libasound2t64" >&2
    return 1
  fi
  (cd dashboard/e2e && node run.mjs)
}

job_perf() {
  local base_dir="$ROOT/target/ci-local/perf-base"
  rm -rf "$base_dir"
  git worktree add --force "$base_dir" "$PERF_BASE" || return 1
  local target
  target="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys;print(json.load(sys.stdin)["target_directory"])')"
  cargo build --release && cp "$target/release/br8n" "$LOGS/br8n-head" &&
    (cd "$base_dir" && cargo build --release) && cp "$target/release/br8n" "$LOGS/br8n-base" &&
    scripts/perf-compare.sh "$LOGS/br8n-base" "$LOGS/br8n-head"
  local status=$?
  git worktree remove --force "$base_dir"
  return $status
}

declare -a SUMMARY=()
FAILED=0
for job in "${ALL_JOBS[@]}"; do
  selected "$job" || continue
  echo "==> $job (log: target/ci-local/$job.log)"
  start=$(date +%s)
  "job_$job" > >(tee "$LOGS/$job.log") 2>&1
  status=$?
  secs=$(( $(date +%s) - start ))
  if [ $status -eq 0 ]; then
    SUMMARY+=("$(printf '%-10s pass  %4ss' "$job" "$secs")")
  elif [ $status -eq 99 ]; then
    SUMMARY+=("$(printf '%-10s skip  %4ss' "$job" "$secs")")
  else
    SUMMARY+=("$(printf '%-10s FAIL  %4ss  see target/ci-local/%s.log' "$job" "$secs" "$job")")
    FAILED=1
  fi
done

echo
echo "ci-local on $(git rev-parse --short HEAD)$(git diff --quiet || echo ' (dirty tree)')"
printf '%s\n' "${SUMMARY[@]}"
exit $FAILED
