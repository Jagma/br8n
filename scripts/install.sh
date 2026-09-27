#!/bin/sh
# Installs the latest br8n release: download, verify the checksum, then let
# the binary install itself.
#
#   curl -fsSL https://github.com/Jagma/br8n/releases/latest/download/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --yes      # unattended: pulls the model without asking
#
# A person runs this, so it exits non-zero with the reason on every failure.
# BR8N_RELEASE_BASE overrides the download prefix; file:// works.
set -eu

BASE="${BR8N_RELEASE_BASE:-https://github.com/Jagma/br8n/releases/latest/download}"

os="$(uname -s)"; arch="$(uname -m)"
case "$os" in
    Darwin) os_part=apple-darwin ;;
    Linux)  os_part=unknown-linux-gnu ;;
    *) echo "br8n: unsupported operating system: $os" >&2; exit 1 ;;
esac
case "$arch" in
    arm64|aarch64) arch_part=aarch64 ;;
    x86_64|amd64)  arch_part=x86_64 ;;
    *) echo "br8n: unsupported architecture: $arch" >&2; exit 1 ;;
esac
target="$arch_part-$os_part"
if [ "$target" = x86_64-apple-darwin ]; then
    echo "br8n: there is no release binary for Intel Macs; build from source (see docs/install.md)" >&2
    exit 1
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
tgz="br8n-$target.tar.gz"
sha="br8n-$target.sha256"

curl -fsSL "$BASE/$tgz" -o "$tmp/$tgz" || { echo "br8n: could not download $BASE/$tgz" >&2; exit 1; }
curl -fsSL "$BASE/$sha" -o "$tmp/$sha" || { echo "br8n: could not download $BASE/$sha" >&2; exit 1; }

if command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$tmp/$tgz" | awk '{print $1}')"
elif command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$tmp/$tgz" | awk '{print $1}')"
else
    echo "br8n: no sha256 tool (shasum or sha256sum) to verify the download" >&2; exit 1
fi
expected="$(awk '{print $1}' "$tmp/$sha")"
if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
    echo "br8n: checksum mismatch for $tgz: expected $expected, got $actual" >&2; exit 1
fi

tar -xzf "$tmp/$tgz" -C "$tmp"
[ -x "$tmp/br8n" ] || { echo "br8n: $tgz does not contain an executable named br8n" >&2; exit 1; }

# `curl | sh` leaves stdin holding the script, so the model-pull prompt
# would read the script. Hand the install a terminal when there is one.
if (exec 3</dev/tty) 2>/dev/null; then
    "$tmp/br8n" install "$@" </dev/tty
else
    "$tmp/br8n" install "$@"
fi
