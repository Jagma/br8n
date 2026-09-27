#!/bin/sh
# Build, package and smoke-test a release tarball on this machine, the same
# way .github/workflows/release.yml does, for when CI cannot run.
#
#   scripts/build-release.sh                # the host's release target
#   scripts/build-release.sh <target>       # e.g. aarch64-apple-darwin
#
# Writes dist/br8n-<target>.tar.gz and dist/br8n-<target>.sha256.
#
# RUSTFLAGS replaces .cargo/config.toml's per-target flags wholesale, so the
# export-dynamic flag is repeated here.
#
# Compiled-in paths (Rust panic locations, including the standard library's,
# C++ __FILE__ strings from lbug, usearch and cxx, and pdf-inspector's
# CARGO_MANIFEST_DIR) would otherwise carry this machine's home directory. The build therefore runs through
# neutral symlinks under /tmp/br8n-build, and Rust paths are remapped too.
set -eu

cd "$(dirname "$0")/.."
repo="$(pwd -P)"

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64)  host=aarch64-apple-darwin ;;
    Linux-x86_64)  host=x86_64-unknown-linux-gnu ;;
    Linux-aarch64) host=aarch64-unknown-linux-gnu ;;
    *) echo "build-release: unsupported host $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac
target="${1:-$host}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
real_target="$(cargo metadata --no-deps --format-version 1 | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
real_cargo="${CARGO_HOME:-$HOME/.cargo}"

build=/tmp/br8n-build
mkdir -p "$build" "$real_target"
ln -sfn "$real_cargo" "$build/cargo"
ln -sfn "$real_target" "$build/target"
export CARGO_HOME="$build/cargo"
export CARGO_TARGET_DIR="$build/target"
target_dir="$build/target"

# With rustup's rust-src component installed, panic locations inside the
# standard library point at the local toolchain instead of /rustc/<hash>.
sysroot="$(rustc --print sysroot)"

remap="--remap-path-prefix=$repo=/br8n --remap-path-prefix=$build=/build --remap-path-prefix=$sysroot=/rustc-sysroot"

case "$target" in
    *-apple-darwin)
        # Link OpenSSL statically without touching Homebrew: the linker takes
        # libssl.a only from a directory that has no libssl.dylib, so point it
        # at one holding just the archives.
        keg="$(brew --prefix openssl@3)/lib"
        static="$(mktemp -d)"
        trap 'rm -rf "$static"' EXIT
        ln -s "$keg/libssl.a" "$keg/libcrypto.a" "$static/"
        export RUSTFLAGS="-L $static -C link-args=-Wl,-export_dynamic $remap"
        ;;
    *-linux-gnu)
        export RUSTFLAGS="-C link-args=-Wl,--export-dynamic $remap"
        ;;
    *) echo "build-release: unsupported target $target" >&2; exit 1 ;;
esac

cargo clean -p lbug --target "$target"
cargo build --release --locked --target "$target"

# Archive headers store the builder's user and group NAMES unless told not
# to, and bsdtar also stores extended attributes such as
# com.apple.provenance. Owner 0/0, no names, no xattrs.
if tar --version 2>/dev/null | grep -q 'GNU tar'; then
    owner="--owner=0 --group=0 --numeric-owner"
else
    owner="--uid 0 --gid 0 --numeric-owner --no-xattrs --no-mac-metadata"
fi
mkdir -p dist
archive="dist/br8n-$target.tar.gz"
# shellcheck disable=SC2086
COPYFILE_DISABLE=1 tar $owner -czf "$archive" -C "$target_dir/$target/release" br8n
(cd dist && shasum -a 256 "br8n-$target.tar.gz" > "br8n-$target.sha256")

user="$(id -un)"
if strings "$target_dir/$target/release/br8n" | grep -q -e "$HOME" -e "/$user/"; then
    echo "build-release: the binary still contains $HOME" >&2
    exit 1
fi
if gzip -dc "$archive" | head -c 4096 | grep -aq "$user"; then
    echo "build-release: the archive headers still contain the user name $user" >&2
    exit 1
fi

sh scripts/release-smoke.sh "dist/br8n-$target.tar.gz" "$version"
echo "build-release: dist/br8n-$target.tar.gz is ready"
