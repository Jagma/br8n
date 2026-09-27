use crate::config::{Config, EmbedConfig, KeepAlive};
use crate::model::Chunk;
use crate::pack::{records, vectors};
use anyhow::Result;

const PROMPT_EMBED_BUDGET: std::time::Duration = std::time::Duration::from_secs(4);

fn line(ok: bool, what: &str, detail: &str) -> bool {
    println!("{} {what}: {detail}", if ok { "ok  " } else { "FAIL" });
    ok
}

fn skipped(what: &str, detail: &str) {
    println!("--   {what}: {detail}");
}

fn warn(what: &str, detail: &str) {
    println!("WARN {what}: {detail}");
}

const EIGHT_GIB: u64 = 8 * 1024 * 1024 * 1024;

#[cfg(target_os = "macos")]
fn total_memory_bytes() -> Option<u64> {
    let name = c"hw.memsize";
    let mut value: u64 = 0;
    let mut size = std::mem::size_of::<u64>();
    let ret = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            &mut value as *mut u64 as *mut std::ffi::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (ret == 0).then_some(value)
}

#[cfg(target_os = "linux")]
fn total_memory_bytes() -> Option<u64> {
    let content = std::fs::read_to_string("/proc/meminfo").ok()?;
    content.lines().find_map(|line| {
        let rest = line.strip_prefix("MemTotal:")?;
        let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
        Some(kb * 1024)
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn total_memory_bytes() -> Option<u64> {
    None
}

fn local_keep_alive(embed: &EmbedConfig) -> Option<&KeepAlive> {
    (embed.remote.is_none() && embed.remote_error.is_none()).then_some(&embed.keep_alive)
}

fn memory_summary(total: u64, keep_alive: Option<&KeepAlive>) -> String {
    let gib = total as f64 / (1024.0 * 1024.0 * 1024.0);
    match keep_alive {
        Some(keep_alive) => {
            format!("{gib:.1} GiB total; embedding model keep_alive is `{keep_alive}`")
        }
        None => format!("{gib:.1} GiB total; embedding runs on the remote endpoint"),
    }
}

fn low_memory_warning(total: u64, keep_alive: Option<&KeepAlive>) -> Option<String> {
    let keep_alive = keep_alive?;
    (total < EIGHT_GIB).then(|| {
        format!(
            "under 8 GiB — the embedding model stays resident for `keep_alive` \
             (currently `{keep_alive}`); set a shorter value in `[embed] keep_alive`, cold \
             loads then cost ~2 s on the first prompt after it expires"
        )
    })
}

fn report_memory_and_keep_alive(cfg: &Config) {
    let Some(total) = total_memory_bytes() else {
        return;
    };
    let keep_alive = local_keep_alive(&cfg.embed);
    line(true, "memory", &memory_summary(total, keep_alive));
    if let Some(detail) = low_memory_warning(total, keep_alive) {
        warn("memory", &detail);
    }
}

pub fn run(cfg: &Config) -> Result<bool> {
    report_memory_and_keep_alive(cfg);
    let mut all = true;
    let env = crate::env_file::env_path(&Config::config_path());
    let env_state = if env.exists() { "present" } else { "absent" };

    if let Some(e) = &cfg.embed.remote_error {
        return Ok(line(false, "env file", e));
    }
    let Some(remote) = cfg.embed.remote.clone() else {
        return Ok(line(
            true,
            "env file",
            &format!(
                "{} {env_state}, BR8N_EMBED_URL unset — embedding on local ollama",
                env.display()
            ),
        ));
    };
    all &= line(
        true,
        "env file",
        &format!(
            "{} {env_state}; BR8N_EMBED_URL, MODEL and TOKEN resolved (token not shown)",
            env.display()
        ),
    );

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let listed = client
        .get(format!("{}/v1/models", remote.url))
        .bearer_auth(&remote.token)
        .send();
    match listed {
        Err(e) => {
            all &= line(
                false,
                "endpoint",
                &format!("{} unreachable: {e}", remote.url),
            )
        }
        Ok(r)
            if r.status() == reqwest::StatusCode::UNAUTHORIZED
                || r.status() == reqwest::StatusCode::FORBIDDEN =>
        {
            all &= line(
                false,
                "endpoint",
                &format!("token rejected ({})", r.status()),
            )
        }
        Ok(r) if !r.status().is_success() => {
            all &= line(
                false,
                "endpoint",
                &format!("{} answered {} to /v1/models", remote.url, r.status()),
            )
        }
        Ok(r) => match r.json::<serde_json::Value>() {
            Err(e) => {
                all &= line(
                    false,
                    "endpoint",
                    &format!("{} sent a model list that is not JSON: {e}", remote.url),
                )
            }
            Ok(body) => {
                let names: Vec<String> = body["data"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|m| m["id"].as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                all &= line(true, "endpoint", &format!("{} reachable", remote.url));
                all &= line(
                    names.contains(&remote.model),
                    "model",
                    &format!("{} ({} served)", remote.model, names.len()),
                );
            }
        },
    }

    let embedder = crate::embed::for_config(&cfg.embed)?;
    let started = std::time::Instant::now();
    match embedder.embed_documents(&["dimension probe".to_string()]) {
        Err(e) => all &= line(false, "dimensions", &format!("{e:#}")),
        Ok(_) => {
            all &= line(
                true,
                "dimensions",
                &format!(
                    "the endpoint returns at least the {} configured",
                    cfg.embed.dimensions
                ),
            )
        }
    }
    let waited = started.elapsed();
    if waited > PROMPT_EMBED_BUDGET {
        skipped(
            "latency",
            &format!(
                "the first embed took {:.1}s, past the {}s a prompt waits — the endpoint was \
                 probably loading its model; the hook loads it in the background after a timeout",
                waited.as_secs_f64(),
                PROMPT_EMBED_BUDGET.as_secs()
            ),
        );
    }

    if cfg.embed.contextual {
        skipped(
            "agreement",
            "not checked — contextual enrichment prepends a generated summary the \
             pack does not keep, so the embedded text cannot be rebuilt",
        );
        return Ok(all);
    }

    let db = Config::db_path();
    let (Ok(recs), Ok(vecs)) = (
        records::Reader::open(&db),
        vectors::Reader::open(&db, cfg.embed.dimensions),
    ) else {
        return Ok(all
            & line(
                false,
                "agreement",
                "no readable pack with vectors beside the database",
            ));
    };
    let Some((row, stored)) = (0..recs.len()).find_map(|r| vecs.vector(r).ok().map(|v| (r, v)))
    else {
        return Ok(all & line(false, "agreement", "the pack has no row with a vector"));
    };
    let rec = recs.get(row)?;
    let embedded = Chunk::plain_embed_text(&rec.title, &rec.heading_path, &rec.text);
    let fresh = match embedder.embed_documents(&[embedded]) {
        Ok(v) => v,
        Err(e) => return Ok(all & line(false, "agreement", &format!("{e:#}"))),
    };
    let cos: f32 = stored.iter().zip(&fresh[0]).map(|(a, b)| a * b).sum();
    all &= line(
        cos > 0.98,
        "agreement",
        &format!("cosine {cos:.4} against the vector stored for row {row}"),
    );

    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn below_eight_gib_warns_and_names_the_configured_value() {
        let w = low_memory_warning(4 * GIB, Some(&KeepAlive::from("5m")))
            .expect("must warn under 8 GiB");
        assert!(w.contains("5m"), "{w}");
        assert!(w.contains("[embed] keep_alive"), "{w}");
        assert!(w.contains("~2 s"), "{w}");
    }

    #[test]
    fn eight_gib_or_more_does_not_warn() {
        let default = KeepAlive::from("30m");
        assert!(low_memory_warning(8 * GIB, Some(&default)).is_none());
        assert!(low_memory_warning(16 * GIB, Some(&default)).is_none());
    }

    #[test]
    fn the_summary_line_names_total_memory_and_keep_alive() {
        let s = memory_summary(16 * GIB, Some(&KeepAlive::from("30m")));
        assert!(s.contains("16.0 GiB"), "{s}");
        assert!(s.contains("30m"), "{s}");
    }

    #[test]
    fn a_remote_embedding_backend_is_never_told_about_keep_alive() {
        let mut embed: EmbedConfig = toml::from_str("").unwrap();
        assert!(local_keep_alive(&embed).is_some());
        embed.remote = Some(crate::env_file::RemoteEmbed {
            url: "http://127.0.0.1:1".into(),
            model: "m".into(),
            token: "t".into(),
        });
        let keep_alive = local_keep_alive(&embed);
        assert!(keep_alive.is_none());
        let s = memory_summary(4 * GIB, keep_alive);
        assert!(!s.contains("keep_alive"), "{s}");
        assert!(low_memory_warning(4 * GIB, keep_alive).is_none());
    }

    #[test]
    fn an_integer_keep_alive_is_shown_as_written() {
        let embed: EmbedConfig = toml::from_str("keep_alive = 300").unwrap();
        let s = memory_summary(16 * GIB, local_keep_alive(&embed));
        assert!(s.contains("`300`"), "{s}");
    }
}
