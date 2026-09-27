pub mod agents;
pub mod claude;
pub mod install;
pub mod ollama;
pub mod plugin;

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Paths {
    pub root: PathBuf,
    pub db: PathBuf,
    pub bin: PathBuf,
    pub plugin: PathBuf,
    pub tmp: PathBuf,
    pub update_json: PathBuf,
    pub update_status: PathBuf,
    pub link_dirs: Vec<PathBuf>,
    pub claude_cache: PathBuf,
}

impl Paths {
    pub fn from_env() -> Paths {
        let db = crate::config::Config::db_path();
        let root = db
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let home = directories::BaseDirs::new()
            .map(|b| b.home_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let link_dirs = match std::env::var_os("BR8N_LINK_DIR") {
            Some(d) => vec![PathBuf::from(d)],
            None => vec![PathBuf::from("/usr/local/bin"), home.join(".local/bin")],
        };
        let mut paths = Paths::at(&root, link_dirs, home.join(".claude/plugins/cache/br8n"));
        paths.db = db;
        paths
    }

    pub fn at(root: &Path, link_dirs: Vec<PathBuf>, claude_cache: PathBuf) -> Paths {
        Paths {
            root: root.to_path_buf(),
            db: root.join("db"),
            bin: root.join("bin/br8n"),
            plugin: root.join("plugin"),
            tmp: root.join("tmp"),
            update_json: root.join("update.json"),
            update_status: root.join("update.status"),
            link_dirs,
            claude_cache,
        }
    }
}
