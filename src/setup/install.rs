use super::claude::ClaudeCli;
use super::{plugin, Paths};
use anyhow::{anyhow, bail, Context, Result};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub struct InstallOpts {
    pub exe: PathBuf,
    pub version: String,
    pub config_path: PathBuf,
    pub claude: ClaudeCli,
    pub yes: bool,
    pub quiet: bool,
    pub ollama_url: String,
    pub models: Vec<String>,
    pub confirm: fn(&str) -> bool,
    pub pull: fn(&str) -> Result<()>,
}

#[derive(Debug, Default)]
pub struct InstallReport {
    pub lines: Vec<String>,
    pub warnings: Vec<String>,
}

pub const STARTER_CONFIG: &str = "\
# Directories and files to index. Without at least one entry there is nothing
# to search but your Claude Code session transcripts.
sources = [
    # \"~/notes\",
    # \"~/Documents/papers\",
]

# Session history from ~/.claude/projects is indexed by default, and so is
# Codex's from $CODEX_HOME/sessions (~/.codex/sessions). The first switch
# turns off both; the second only Codex's.
# index_transcripts = false
# index_codex_sessions = false
";

pub const PLUGIN_ID: &str = "br8n@br8n";
pub const MARKETPLACE: &str = "br8n";

pub fn install(paths: &Paths, opts: &InstallOpts) -> Result<InstallReport> {
    let mut r = InstallReport::default();

    if place_binary(paths, &opts.exe)? {
        r.lines
            .push(format!("binary:   placed at {}", paths.bin.display()));
    } else {
        r.lines
            .push(format!("binary:   already at {}", paths.bin.display()));
    }

    if paths.plugin.exists() {
        std::fs::remove_dir_all(&paths.plugin)
            .with_context(|| format!("remove {}", paths.plugin.display()))?;
    }
    plugin::write(&paths.plugin, &opts.version, &paths.bin)
        .with_context(|| format!("write the plugin to {}", paths.plugin.display()))?;
    r.lines.push(format!(
        "plugin:   written ({}) at {}",
        opts.version,
        paths.plugin.display()
    ));

    match link_binary(paths) {
        Ok(Some(link)) => {
            r.lines.push(format!("link:     {}", link.display()));
            if let Some(hint) = path_hint(&link) {
                r.warnings.push(hint);
            }
        }
        Ok(None) => r.warnings.push(format!(
            "no directory to link `br8n` into ({}); add {} to PATH yourself",
            paths
                .link_dirs
                .iter()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
            paths.bin.parent().unwrap_or(&paths.root).display()
        )),
        Err(e) => r
            .warnings
            .push(format!("could not link `br8n` onto PATH: {e:#}")),
    }

    if opts.config_path.is_file() {
        r.lines
            .push(format!("config:   kept {}", opts.config_path.display()));
    } else {
        if let Some(parent) = opts.config_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        match std::fs::write(&opts.config_path, STARTER_CONFIG) {
            Ok(()) => r.lines.push(format!(
                "config:   wrote a starter at {}",
                opts.config_path.display()
            )),
            Err(e) => r.warnings.push(format!(
                "could not write {}: {e}",
                opts.config_path.display()
            )),
        }
    }

    register(paths, &opts.claude, &mut r);

    if !opts.models.is_empty() {
        let (lines, warnings) = super::ollama::preflight(
            &opts.ollama_url,
            &opts.models,
            opts.yes,
            opts.confirm,
            opts.pull,
        );
        r.lines.extend(lines);
        r.warnings.extend(warnings);
    }

    prove_placed_binary(paths, &opts.version)?;
    r.lines.push(format!(
        "check:    {} --version reports {}",
        paths.bin.display(),
        opts.version
    ));
    Ok(r)
}

pub fn place_binary(paths: &Paths, exe: &Path) -> Result<bool> {
    if let (Ok(a), Ok(b)) = (exe.canonicalize(), paths.bin.canonicalize()) {
        if a == b {
            return Ok(false);
        }
    }
    std::fs::create_dir_all(&paths.tmp)
        .with_context(|| format!("create {}", paths.tmp.display()))?;
    if let Some(bin_dir) = paths.bin.parent() {
        std::fs::create_dir_all(bin_dir)
            .with_context(|| format!("create {}", bin_dir.display()))?;
    }
    let staged = paths.tmp.join(format!("br8n-{}", std::process::id()));
    std::fs::copy(exe, &staged)
        .with_context(|| format!("copy {} to {}", exe.display(), staged.display()))?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("set permissions on {}", staged.display()))?;
    match std::fs::remove_file(&paths.bin) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(anyhow!("unlink {}: {e}", paths.bin.display())),
    }
    std::fs::rename(&staged, &paths.bin)
        .with_context(|| format!("rename {} to {}", staged.display(), paths.bin.display()))?;
    Ok(true)
}

pub fn link_binary(paths: &Paths) -> Result<Option<PathBuf>> {
    for dir in &paths.link_dirs {
        let is_usr_local = dir == Path::new("/usr/local/bin");
        if !dir.is_dir() {
            if is_usr_local {
                continue;
            }
            if std::fs::create_dir_all(dir).is_err() {
                continue;
            }
        }
        let link = dir.join("br8n");
        if let Ok(target) = std::fs::read_link(&link) {
            if target == paths.bin {
                return Ok(Some(link));
            }
        }
        if link.symlink_metadata().is_ok() && std::fs::remove_file(&link).is_err() {
            continue;
        }
        match std::os::unix::fs::symlink(&paths.bin, &link) {
            Ok(()) => return Ok(Some(link)),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => continue,
            Err(e) => {
                return Err(anyhow!(
                    "symlink {} -> {}: {e}",
                    link.display(),
                    paths.bin.display()
                ))
            }
        }
    }
    Ok(None)
}

pub fn path_hint(link: &Path) -> Option<String> {
    let dir = link.parent()?;
    let on_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d == dir))
        .unwrap_or(false);
    (!on_path).then(|| {
        format!(
            "{} is not on PATH; add this to your shell profile:\n    export PATH=\"{}:$PATH\"",
            dir.display(),
            dir.display()
        )
    })
}

pub fn connect_claude_code(paths: &Paths, cli: &ClaudeCli) -> Result<Vec<String>> {
    let mut r = InstallReport::default();
    register(paths, cli, &mut r);
    if r.warnings.is_empty() {
        Ok(r.lines)
    } else {
        Err(anyhow!(r.warnings.join("; ")))
    }
}

pub fn disconnect_claude_code(paths: &Paths, cli: &ClaudeCli) -> Result<Vec<String>> {
    let mut r = UninstallReport::default();
    unregister(paths, cli, &mut r);
    if r.warnings.is_empty() {
        Ok(r.lines)
    } else {
        Err(anyhow!(r.warnings.join("; ")))
    }
}

fn register(paths: &Paths, cli: &ClaudeCli, r: &mut InstallReport) {
    if !cli.available() {
        r.warnings.push(
            "the `claude` command was not found, so the plugin is not registered with Claude Code. \
             Install Claude Code (https://claude.com/claude-code), then run `br8n install` again."
                .to_string(),
        );
        return;
    }
    let markets = match cli.marketplaces() {
        Ok(m) => m,
        Err(e) => {
            r.warnings.push(format!("{e:#}"));
            return;
        }
    };
    let wanted = paths.plugin.clone();
    let add_cmd = format!("claude plugin marketplace add \"{}\"", wanted.display());
    match markets.iter().find(|m| m.name == MARKETPLACE) {
        Some(m) if m.path.as_deref() == Some(wanted.as_path()) => {
            r.lines
                .push(format!("market:   br8n -> {}", wanted.display()));
        }
        Some(m) => {
            let old = m
                .path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| m.source.clone());
            if let Err(e) = cli
                .marketplace_remove(MARKETPLACE)
                .and_then(|_| cli.marketplace_add(&wanted))
            {
                r.warnings.push(format!(
                    "{e:#}; run by hand: claude plugin marketplace remove br8n && {add_cmd}"
                ));
                return;
            }
            r.lines.push(format!(
                "market:   br8n re-pointed from {old} to {}",
                wanted.display()
            ));
        }
        None => {
            if let Err(e) = cli.marketplace_add(&wanted) {
                r.warnings.push(format!("{e:#}; run by hand: {add_cmd}"));
                return;
            }
            r.lines
                .push(format!("market:   br8n -> {}", wanted.display()));
        }
    }
    let plugins = match cli.plugins() {
        Ok(p) => p,
        Err(e) => {
            r.warnings.push(format!("{e:#}"));
            return;
        }
    };
    let installed = plugins.iter().any(|p| p.id == PLUGIN_ID);
    let result = if installed {
        cli.plugin_update(PLUGIN_ID)
    } else {
        cli.plugin_install(PLUGIN_ID)
    };
    match result {
        Ok(()) => r.lines.push(format!(
            "plugin:   {PLUGIN_ID} {}",
            if installed { "updated" } else { "installed" }
        )),
        Err(e) => r.warnings.push(format!(
            "{e:#}; run by hand: claude plugin {} {PLUGIN_ID}",
            if installed { "update" } else { "install" }
        )),
    }
}

pub struct UninstallOpts {
    pub purge: bool,
    pub yes: bool,
    pub claude: ClaudeCli,
    pub config_path: PathBuf,
    pub confirm: fn(&str) -> bool,
}

#[derive(Debug, Default)]
pub struct UninstallReport {
    pub lines: Vec<String>,
    pub warnings: Vec<String>,
    pub kept: Vec<PathBuf>,
}

pub fn uninstall(paths: &Paths, opts: &UninstallOpts) -> Result<UninstallReport> {
    if crate::index::IndexLock::is_held(&paths.db) {
        bail!("an index is running; wait for it to finish (`br8n status` shows progress), then run `br8n uninstall` again");
    }
    if opts.purge && !opts.yes && !(opts.confirm)("remove the index and the config as well?") {
        bail!("purge declined; nothing was removed");
    }
    let mut r = UninstallReport::default();

    unregister(paths, &opts.claude, &mut r);

    if paths.claude_cache.exists() {
        match std::fs::remove_dir_all(&paths.claude_cache) {
            Ok(()) => r
                .lines
                .push(format!("removed {}", paths.claude_cache.display())),
            Err(e) => r.warnings.push(format!(
                "could not remove {}: {e}",
                paths.claude_cache.display()
            )),
        }
    }

    for dir in &paths.link_dirs {
        let link = dir.join("br8n");
        if std::fs::read_link(&link)
            .map(|t| t == paths.bin)
            .unwrap_or(false)
        {
            match std::fs::remove_file(&link) {
                Ok(()) => r.lines.push(format!("removed {}", link.display())),
                Err(e) => r
                    .warnings
                    .push(format!("could not remove {}: {e}", link.display())),
            }
        }
    }

    if opts.purge {
        std::fs::remove_dir_all(&paths.root)
            .with_context(|| format!("remove {}", paths.root.display()))?;
        r.lines.push(format!("removed {}", paths.root.display()));
        if opts.config_path.exists() && !opts.config_path.starts_with(&paths.root) {
            let victim = match opts.config_path.parent() {
                Some(p) if p.file_name().map(|n| n == "br8n").unwrap_or(false) => p.to_path_buf(),
                _ => opts.config_path.clone(),
            };
            let res = if victim.is_dir() {
                std::fs::remove_dir_all(&victim)
            } else {
                std::fs::remove_file(&victim)
            };
            match res {
                Ok(()) => r.lines.push(format!("removed {}", victim.display())),
                Err(e) => r
                    .warnings
                    .push(format!("could not remove {}: {e}", victim.display())),
            }
        }
        return Ok(r);
    }

    for p in [
        paths.bin.parent().unwrap_or(&paths.root).to_path_buf(),
        paths.plugin.clone(),
        paths.tmp.clone(),
    ] {
        if p.exists() {
            match std::fs::remove_dir_all(&p) {
                Ok(()) => r.lines.push(format!("removed {}", p.display())),
                Err(e) => r
                    .warnings
                    .push(format!("could not remove {}: {e}", p.display())),
            }
        }
    }
    for p in [&paths.update_json, &paths.update_status] {
        let _ = std::fs::remove_file(p);
    }
    for kept in [opts.config_path.clone(), paths.db.clone()] {
        if kept.exists() {
            r.kept.push(kept);
        }
    }
    Ok(r)
}

fn unregister(paths: &Paths, cli: &ClaudeCli, r: &mut UninstallReport) {
    if !cli.available() {
        r.warnings.push(
            "the `claude` command was not found; the plugin registration was left as it is"
                .to_string(),
        );
        return;
    }
    match cli.plugins() {
        Ok(p) if p.iter().any(|p| p.id == PLUGIN_ID) => match cli.plugin_uninstall(PLUGIN_ID) {
            Ok(()) => r.lines.push(format!("unregistered {PLUGIN_ID}")),
            Err(e) => r.warnings.push(format!(
                "{e:#}; run by hand: claude plugin uninstall {PLUGIN_ID}"
            )),
        },
        Ok(_) => {}
        Err(e) => r.warnings.push(format!("{e:#}")),
    }
    match cli.marketplaces() {
        Ok(m) => {
            if let Some(m) = m.iter().find(|m| m.name == MARKETPLACE) {
                let ours = match &m.path {
                    Some(p) => (p == &paths.plugin) || p.join("scripts/bootstrap.sh").is_file(),
                    None => false,
                };
                if ours {
                    match cli.marketplace_remove(MARKETPLACE) {
                        Ok(()) => r.lines.push(format!("removed marketplace {MARKETPLACE}")),
                        Err(e) => r.warnings.push(format!(
                            "{e:#}; run by hand: claude plugin marketplace remove {MARKETPLACE}"
                        )),
                    }
                } else {
                    r.warnings.push(format!(
                        "a marketplace named {MARKETPLACE} points at {}, which this install did not create; left alone",
                        m.path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| m.source.clone())
                    ));
                }
            }
        }
        Err(e) => r.warnings.push(format!("{e:#}")),
    }
}

#[derive(Debug, serde::Serialize)]
pub struct InstallCheck {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Fix {
    Connect { agent: &'static str },
    Command { command: String },
    Manual { text: String },
}

pub fn fix_for(check: &InstallCheck, exe: &Path, claude_available: bool) -> Option<Fix> {
    if check.ok {
        return None;
    }
    Some(match check.name {
        "plugin" | "marketplace" | "registration" if claude_available => Fix::Connect {
            agent: super::agents::claude_code::ID,
        },
        "claude" => Fix::Manual {
            text: "install Claude Code (https://claude.com/claude-code), then connect it here"
                .to_string(),
        },
        _ => Fix::Command {
            command: format!("\"{}\" install", exe.display()),
        },
    })
}

pub fn install_checks(
    paths: &Paths,
    cli: &ClaudeCli,
    installed: &str,
    exe: &Path,
) -> Vec<InstallCheck> {
    let mut out = Vec::new();
    let same = match (exe.canonicalize(), paths.bin.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    out.push(InstallCheck {
        name: "binary",
        ok: same,
        detail: if same {
            paths.bin.display().to_string()
        } else {
            format!(
                "this binary is {}, not {}",
                exe.display(),
                paths.bin.display()
            )
        },
    });

    let manifest = paths.plugin.join(".claude-plugin/plugin.json");
    let plugin_version = std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["version"].as_str().map(str::to_string));
    out.push(match plugin_version.as_deref() {
        Some(v) if v == installed => InstallCheck {
            name: "plugin",
            ok: true,
            detail: format!("{} ({v})", paths.plugin.display()),
        },
        Some(v) => InstallCheck {
            name: "plugin",
            ok: false,
            detail: format!("plugin files are {v}, binary is {installed}"),
        },
        None => InstallCheck {
            name: "plugin",
            ok: false,
            detail: format!("no plugin files at {}", paths.plugin.display()),
        },
    });

    if cli.available() {
        let market_ok = cli
            .marketplaces()
            .map(|m| {
                m.iter().any(|m| {
                    m.name == MARKETPLACE && m.path.as_deref() == Some(paths.plugin.as_path())
                })
            })
            .unwrap_or(false);
        out.push(InstallCheck {
            name: "marketplace",
            ok: market_ok,
            detail: if market_ok {
                "br8n -> plugin directory".into()
            } else {
                "no marketplace `br8n` pointing at the plugin directory".into()
            },
        });
        let plugin_ok = cli
            .plugins()
            .map(|p| p.iter().any(|p| p.id == PLUGIN_ID))
            .unwrap_or(false);
        out.push(InstallCheck {
            name: "registration",
            ok: plugin_ok,
            detail: if plugin_ok {
                PLUGIN_ID.into()
            } else {
                format!("{PLUGIN_ID} is not installed in Claude Code")
            },
        });
    } else {
        out.push(InstallCheck {
            name: "claude",
            ok: false,
            detail: "the `claude` command is not on PATH".into(),
        });
    }

    let on_path = std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("br8n"))
                .find(|c| c.exists())
        })
        .unwrap_or(None);
    let path_ok = on_path
        .as_ref()
        .and_then(|c| c.canonicalize().ok())
        .zip(paths.bin.canonicalize().ok())
        .map(|(a, b)| a == b)
        .unwrap_or(false);
    out.push(InstallCheck {
        name: "path",
        ok: path_ok,
        detail: match on_path {
            Some(c) if path_ok => c.display().to_string(),
            Some(c) => format!(
                "`br8n` on PATH is {}, not {}",
                c.display(),
                paths.bin.display()
            ),
            None => "`br8n` is not on PATH".into(),
        },
    });
    out
}

pub fn prove_placed_binary(paths: &Paths, version: &str) -> Result<()> {
    let out = std::process::Command::new(&paths.bin)
        .arg("--version")
        .output()
        .with_context(|| format!("{} will not execute on this machine", paths.bin.display()))?;
    let got = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let want = format!("br8n {version}");
    if !out.status.success() || got != want {
        bail!(
            "{} reports `{}`, expected `{}`{}",
            paths.bin.display(),
            got,
            want,
            if out.status.success() {
                ""
            } else {
                " (and exited non-zero)"
            }
        );
    }
    Ok(())
}
