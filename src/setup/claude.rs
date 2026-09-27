use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct ClaudeCli {
    program: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marketplace {
    pub name: String,
    pub source: String,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPlugin {
    pub id: String,
    pub version: String,
    pub install_path: Option<PathBuf>,
}

impl ClaudeCli {
    pub fn from_path() -> ClaudeCli {
        ClaudeCli::at(Path::new("claude"))
    }

    pub fn at(program: &Path) -> ClaudeCli {
        ClaudeCli {
            program: program.to_path_buf(),
        }
    }

    pub fn available(&self) -> bool {
        Command::new(&self.program)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let shown = format!("claude {}", args.join(" "));
        let out = Command::new(&self.program)
            .args(args)
            .output()
            .with_context(|| format!("cannot run `{shown}`"))?;
        if !out.status.success() {
            return Err(anyhow!(
                "`{shown}` failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn json(&self, args: &[&str]) -> Result<serde_json::Value> {
        let raw = self.run(args)?;
        let start = raw.find('[').unwrap_or(0);
        serde_json::from_str(&raw[start..])
            .with_context(|| format!("`claude {}` did not print JSON: {raw}", args.join(" ")))
    }

    pub fn marketplaces(&self) -> Result<Vec<Marketplace>> {
        let v = self.json(&["plugin", "marketplace", "list", "--json"])?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .map(|m| Marketplace {
                        name: m["name"].as_str().unwrap_or_default().to_string(),
                        source: m["source"].as_str().unwrap_or_default().to_string(),
                        path: m["path"].as_str().map(PathBuf::from),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn plugins(&self) -> Result<Vec<InstalledPlugin>> {
        let v = self.json(&["plugin", "list", "--json"])?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .map(|p| InstalledPlugin {
                        id: p["id"].as_str().unwrap_or_default().to_string(),
                        version: p["version"].as_str().unwrap_or_default().to_string(),
                        install_path: p["installPath"].as_str().map(PathBuf::from),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn marketplace_add(&self, path: &Path) -> Result<()> {
        let p = path.to_string_lossy();
        self.run(&["plugin", "marketplace", "add", &p]).map(|_| ())
    }

    pub fn marketplace_remove(&self, name: &str) -> Result<()> {
        self.run(&["plugin", "marketplace", "remove", name])
            .map(|_| ())
    }

    pub fn plugin_install(&self, id: &str) -> Result<()> {
        self.run(&["plugin", "install", id]).map(|_| ())
    }

    pub fn plugin_update(&self, id: &str) -> Result<()> {
        self.run(&["plugin", "update", id]).map(|_| ())
    }

    pub fn plugin_uninstall(&self, id: &str) -> Result<()> {
        self.run(&["plugin", "uninstall", id]).map(|_| ())
    }
}
