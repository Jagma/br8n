//! `usearch` 2.26 emits ARM SME code whose runtime symbol `___arm_sme_state`
//! lives in clang's compiler-rt, and Rust links with `-nodefaultlibs`. Without
//! this, the build fails on aarch64-apple-darwin with:
//!
//!     Undefined symbols for architecture arm64: "___arm_sme_state"
//!
//! This is deliberately NOT a hardcoded path in `.cargo/config.toml`: the
//! clang version directory (`21`, `21.1`, ...) changes with the installed
//! Xcode, and a pinned path breaks on any other machine or CI image. Instead
//! we locate the toolchain at build time and emit the link arg only when
//! found, only on this one target. `.cargo/config.toml` itself is untouched —
//! its `-Wl,-export_dynamic` flags are load-bearing for lbug's dlopen'd
//! `vector`/`fts` extensions (see the comment at the top of that file) and
//! must not be disturbed by this workaround.
//!
//! Finding the runtime lib is a two-step affair:
//!   1. Ask clang directly (`xcrun clang -print-runtime-dir`) — this is the
//!      toolchain's own answer and needs no version-directory guessing at
//!      all. Confirmed on this machine (Xcode 26, clang 21) to print
//!      `.../usr/lib/clang/21/lib/darwin`, which contains `libclang_rt.osx.a`
//!      directly.
//!   2. If that fails, or the directory it names doesn't actually have the
//!      file (older/newer clang layouts have moved this before), fall back to
//!      globbing `<clang_lib_dir>/*/lib/darwin/libclang_rt.osx.a` ourselves.
//!      The glob's ambiguity — multiple version directories, e.g. a real `21`
//!      next to a `21.0.0` alias, or across major versions like `9` vs `21` —
//!      is resolved by parsing the leading numeric components of the
//!      directory name and comparing them numerically, never lexicographically
//!      (a plain string sort would rank `"9"` above `"21"`).
//!
//! A standalone spike is how this was discovered.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=DEVELOPER_DIR");

    let target = env::var("TARGET").unwrap_or_default();
    println!("cargo:rustc-env=BR8N_TARGET={target}");

    if target != "aarch64-apple-darwin" {
        return;
    }

    if let Some(path) = runtime_dir_via_clang() {
        println!("cargo:rustc-link-arg={}", path.display());
        return;
    }

    let toolchain_root = match developer_dir() {
        Some(dir) => dir,
        None => {
            warn_missing(
                "could not locate an Xcode toolchain via DEVELOPER_DIR or `xcode-select -p`",
            );
            return;
        }
    };

    let clang_lib_dir =
        PathBuf::from(&toolchain_root).join("Toolchains/XcodeDefault.xctoolchain/usr/lib/clang");

    match find_runtime_lib(&clang_lib_dir) {
        Some(path) => {
            println!("cargo:rustc-link-arg={}", path.display());
        }
        None => {
            warn_missing(&format!(
                "could not find libclang_rt.osx.a under {} (tried `xcrun clang \
                 -print-runtime-dir` and */lib/darwin/ globbing)",
                clang_lib_dir.display()
            ));
        }
    }
}

fn warn_missing(detail: &str) {
    println!(
        "cargo:warning=usearch build.rs: {detail}; the usearch ARM SME runtime symbol \
         (___arm_sme_state) may fail to link on aarch64-apple-darwin"
    );
}

/// Ask clang for its own runtime directory rather than guessing a version
/// path. Preferred over globbing because it removes the version-directory
/// ambiguity entirely instead of making the tie-break smarter.
fn runtime_dir_via_clang() -> Option<PathBuf> {
    let output = Command::new("xcrun")
        .args(["clang", "-print-runtime-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?;
    let dir = PathBuf::from(s.trim());
    if dir.as_os_str().is_empty() {
        return None;
    }
    let candidate = dir.join("libclang_rt.osx.a");
    candidate.is_file().then_some(candidate)
}

/// `DEVELOPER_DIR` overrides the active toolchain (Xcode's own convention);
/// fall back to `xcode-select -p` when it is unset.
fn developer_dir() -> Option<String> {
    if let Ok(dir) = env::var("DEVELOPER_DIR") {
        if !dir.trim().is_empty() {
            return Some(dir);
        }
    }
    let output = Command::new("xcode-select").arg("-p").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Fallback used only when `xcrun clang -print-runtime-dir` is unavailable or
/// wrong. Searches `<clang_lib_dir>/*/lib/darwin/libclang_rt.osx.a` — the `*`
/// is the clang version directory and MUST NOT be hardcoded, since it changes
/// with the installed Xcode. Candidates are ordered by the leading numeric
/// components of their version-directory name (major, minor, patch, ...),
/// compared numerically so that e.g. `21` sorts above `9`; a directory name
/// that fails to parse as a version sorts below every one that does, rather
/// than panicking. The greatest version wins; ties (e.g. a `21` directory and
/// a `21.0.0` symlink alias to the same file) resolve arbitrarily but
/// harmlessly, since both name the same file.
fn find_runtime_lib(clang_lib_dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(clang_lib_dir).ok()?;
    let mut candidates: Vec<(Vec<u64>, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let version = parse_version(&entry.file_name().to_string_lossy());
            let lib_path = entry.path().join("lib/darwin/libclang_rt.osx.a");
            lib_path.is_file().then_some((version, lib_path))
        })
        .collect();
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    candidates.pop().map(|(_, path)| path)
}

/// Parse the leading dot-separated numeric components of a version-directory
/// name (e.g. `"21.0.0"` -> `[21, 0, 0]`, `"9"` -> `[9]`). A name with no
/// parseable leading integer yields an empty vector, which sorts below every
/// non-empty one — so unparseable directories never win the comparison and
/// never panic.
fn parse_version(name: &str) -> Vec<u64> {
    name.split('.')
        .map_while(|part| part.parse::<u64>().ok())
        .collect()
}
