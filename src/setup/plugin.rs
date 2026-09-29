use anyhow::{Context, Result};
use std::path::Path;

#[derive(rust_embed::RustEmbed)]
#[folder = "plugin/"]
struct Templates;

const LAUNCHER: &str = "${CLAUDE_PLUGIN_ROOT}/scripts/br8n.sh";
const SHIPPED_VERSION: &str = concat!("\"version\": \"", env!("CARGO_PKG_VERSION"), "\"");

pub fn render(version: &str, bin: &Path) -> Vec<(String, Vec<u8>)> {
    let bin = bin.to_string_lossy();
    let stamped = format!("\"version\": \"{version}\"");
    Templates::iter()
        .map(|name| {
            let data = Templates::get(&name).expect("iterated name exists").data;
            let text = String::from_utf8_lossy(&data)
                .replace(SHIPPED_VERSION, &stamped)
                .replace(LAUNCHER, &bin);
            (name.to_string(), text.into_bytes())
        })
        .collect()
}

pub fn write(dir: &Path, version: &str, bin: &Path) -> Result<()> {
    for (rel, bytes) in render(version, bin) {
        let path = dir.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}
