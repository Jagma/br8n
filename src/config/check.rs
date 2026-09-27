use super::schema::{config_schema, Node};
use super::{Config, SurfaceConfig, WeightOverrides, Weights};
use serde::Serialize;
use std::collections::BTreeMap;
use toml_edit::{ImDocument, Item, TableLike};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldError {
    pub path: String,
    pub message: String,
    pub line: Option<usize>,
}

impl FieldError {
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> FieldError {
        FieldError {
            path: path.into(),
            message: message.into(),
            line: None,
        }
    }

    fn at_line(mut self, line: Option<usize>) -> FieldError {
        self.line = line;
        self
    }

    pub fn same_problem(&self, other: &FieldError) -> bool {
        self.path == other.path && self.message == other.message
    }
}

impl std::fmt::Display for FieldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(line) = self.line {
            write!(f, "line {line}: ")?;
        }
        if self.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

const MAX_TYPE_ERRORS: usize = 64;

pub fn check(text: &str) -> Vec<FieldError> {
    let doc = match ImDocument::parse(text.to_string()) {
        Ok(doc) => doc,
        Err(e) => return vec![syntax_error(text, &e)],
    };
    let mut errors = Vec::new();
    if let Some(root) = config_schema().children() {
        unknown_keys(doc.as_table(), root, "", text, &mut errors);
    }
    let (parsed, type_errors) = strict_parse(text);
    let unreadable: Vec<String> = type_errors.iter().map(|e| e.path.clone()).collect();
    errors.extend(type_errors.into_iter().map(|e| {
        let line = line_of_path(&doc, text, &e.path).or(e.line);
        e.at_line(line)
    }));
    if let Some(cfg) = parsed {
        errors.extend(range_errors(&cfg, &unreadable).into_iter().map(|e| {
            let line = line_of_path(&doc, text, &e.path);
            e.at_line(line)
        }));
    }
    errors
}

pub fn syntax_error(text: &str, error: &toml_edit::TomlError) -> FieldError {
    let line = error.span().map(|span| line_at(text, span.start));
    FieldError::new("", format!("TOML syntax error: {}", error.message().trim())).at_line(line)
}

fn line_at(text: &str, offset: usize) -> usize {
    text.as_bytes()[..offset.min(text.len())]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
        + 1
}

fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

fn unknown_keys(
    table: &dyn TableLike,
    schema: &BTreeMap<String, Node>,
    prefix: &str,
    text: &str,
    out: &mut Vec<FieldError>,
) {
    for (key, item) in table.iter() {
        let path = join(prefix, key);
        match schema.get(key) {
            None => {
                let line = table
                    .key(key)
                    .and_then(|k| k.span())
                    .map(|span| line_at(text, span.start));
                out.push(FieldError::new(&path, unknown_key_message(key, schema)).at_line(line));
            }
            Some(Node::Table(children)) => {
                if let Some(inner) = item.as_table_like() {
                    unknown_keys(inner, children, &path, text, out);
                }
            }
            Some(Node::Leaf(_)) => {}
        }
    }
}

fn unknown_key_message(key: &str, siblings: &BTreeMap<String, Node>) -> String {
    let suggestion = closest_sibling(key, siblings)
        .map(|s| format!("`{s}`"))
        .or_else(|| same_name_elsewhere(key));
    match suggestion {
        Some(s) => format!("unknown key `{key}`; did you mean {s}?"),
        None => format!("unknown key `{key}`"),
    }
}

fn closest_sibling<'a>(key: &str, siblings: &'a BTreeMap<String, Node>) -> Option<&'a str> {
    let budget = (key.chars().count() / 3).max(1);
    siblings
        .keys()
        .map(|candidate| (strsim::damerau_levenshtein(key, candidate), candidate))
        .filter(|(distance, _)| *distance <= budget)
        .min()
        .map(|(_, candidate)| candidate.as_str())
}

fn same_name_elsewhere(key: &str) -> Option<String> {
    let budget = (key.chars().count() / 3).max(1);
    let scored: Vec<(usize, String)> = config_schema()
        .leaf_paths()
        .into_iter()
        .filter(|path| path.contains('.'))
        .filter_map(|path| {
            let last = path.rsplit('.').next()?;
            let distance = strsim::damerau_levenshtein(key, last);
            (distance <= budget).then_some((distance, path))
        })
        .collect();
    let best = scored.iter().map(|(distance, _)| *distance).min()?;
    let matches: Vec<String> = scored
        .into_iter()
        .filter(|(distance, _)| *distance == best)
        .map(|(_, path)| format!("`{path}`"))
        .collect();
    Some(matches.join(" or "))
}

fn strict_parse(text: &str) -> (Option<Config>, Vec<FieldError>) {
    let mut remaining = text.to_string();
    let mut errors = Vec::new();
    for _ in 0..MAX_TYPE_ERRORS {
        let error = match toml::from_str::<Config>(&remaining) {
            Ok(cfg) => return (Some(cfg), errors),
            Err(e) => e,
        };
        let Ok(doc) = ImDocument::parse(remaining.clone()) else {
            break;
        };
        let line = error.span().map(|span| line_at(&remaining, span.start));
        let located = error
            .span()
            .and_then(|span| path_at(doc.as_table(), span.start))
            .filter(|path| !path.is_empty());
        let message = error.message().trim().to_string();
        let Some(path) = located else {
            errors.push(FieldError::new("", message).at_line(line));
            break;
        };
        errors.push(FieldError::new(path.join("."), message).at_line(line));
        let mut editable = doc.into_mut();
        remove_path(editable.as_table_mut(), &path);
        remaining = editable.to_string();
    }
    (None, errors)
}

fn path_at(table: &dyn TableLike, offset: usize) -> Option<Vec<String>> {
    for (key, item) in table.iter() {
        let key_start = table.key(key).and_then(|k| k.span()).map(|s| s.start);
        match item {
            Item::Table(inner) => {
                if let Some(mut path) = path_at(inner, offset) {
                    path.insert(0, key.to_string());
                    return Some(path);
                }
                if inner.span().is_some_and(|s| s.contains(&offset)) {
                    return Some(vec![key.to_string()]);
                }
            }
            Item::Value(value) => {
                let end = value.span().map(|s| s.end);
                let within = matches!((key_start, end), (Some(start), Some(end)) if (start..end).contains(&offset));
                if !within {
                    continue;
                }
                if let Some(inline) = value.as_inline_table() {
                    if let Some(mut path) = path_at(inline, offset) {
                        path.insert(0, key.to_string());
                        return Some(path);
                    }
                }
                return Some(vec![key.to_string()]);
            }
            Item::ArrayOfTables(array) => {
                if array.span().is_some_and(|s| s.contains(&offset)) {
                    return Some(vec![key.to_string()]);
                }
            }
            Item::None => {}
        }
    }
    None
}

fn remove_path(table: &mut dyn TableLike, path: &[String]) {
    match path {
        [] => {}
        [leaf] => {
            table.remove(leaf);
        }
        [head, rest @ ..] => {
            if let Some(inner) = table.get_mut(head).and_then(Item::as_table_like_mut) {
                remove_path(inner, rest);
            }
        }
    }
}

fn line_of_path(doc: &ImDocument<String>, text: &str, path: &str) -> Option<usize> {
    let mut table: &dyn TableLike = doc.as_table();
    let mut line = None;
    for segment in path.split('.') {
        let (key, item) = table.get_key_value(segment)?;
        line = key.span().map(|span| line_at(text, span.start)).or(line);
        match item.as_table_like() {
            Some(inner) => table = inner,
            None => break,
        }
    }
    line
}

struct Ranges<'a> {
    errors: Vec<FieldError>,
    unreadable: &'a [String],
}

impl Ranges<'_> {
    fn written_but_unreadable(&self, table: &str) -> bool {
        let nested = format!("{table}.");
        self.unreadable
            .iter()
            .any(|path| path == table || path.starts_with(&nested))
    }

    fn fail(&mut self, path: &str, message: String) {
        self.errors.push(FieldError::new(path, message));
    }

    fn between(&mut self, path: &str, value: f64, low: f64, high: f64) {
        if !(low..=high).contains(&value) {
            self.fail(
                path,
                format!("must be between {low} and {high}, got {value}"),
            );
        }
    }

    fn at_least(&mut self, path: &str, value: f64, low: f64) {
        if value.is_nan() || value < low {
            self.fail(path, format!("must be at least {low}, got {value}"));
        }
    }

    fn above(&mut self, path: &str, value: f64, low: f64) {
        if value.is_nan() || value <= low {
            self.fail(path, format!("must be greater than {low}, got {value}"));
        }
    }

    fn not_blank(&mut self, path: &str, value: &str) {
        if value.trim().is_empty() {
            self.fail(path, "must not be empty".to_string());
        }
    }

    fn surface(&mut self, name: &str, surface: &SurfaceConfig) {
        if let Some(quality) = surface.quality {
            self.between(&format!("{name}.quality"), quality.into(), 0.0, 4.0);
        }
        self.between(
            &format!("{name}.threshold"),
            surface.threshold.into(),
            0.0,
            1.0,
        );
        self.at_least(
            &format!("{name}.max_tokens"),
            surface.max_tokens as f64,
            1.0,
        );
        self.weight_overrides(&format!("{name}.weights"), &surface.weights);
    }

    fn weight_overrides(&mut self, prefix: &str, overrides: &WeightOverrides) {
        let named = [
            ("authority", overrides.authority),
            ("markdown", overrides.markdown),
            ("pdf", overrides.pdf),
            ("web", overrides.web),
            ("transcript", overrides.transcript),
            ("memory", overrides.memory),
            ("current", overrides.current),
            ("investigating", overrides.investigating),
            ("proposed", overrides.proposed),
            ("superseded", overrides.superseded),
        ];
        for (key, value) in named {
            if let Some(value) = value {
                self.at_least(&join(prefix, key), value.into(), 0.0);
            }
        }
    }

    fn weights(&mut self, weights: &Weights) {
        let named = [
            ("authority", weights.authority),
            ("markdown", weights.markdown),
            ("pdf", weights.pdf),
            ("web", weights.web),
            ("transcript", weights.transcript),
            ("memory", weights.memory),
            ("current", weights.current),
            ("investigating", weights.investigating),
            ("proposed", weights.proposed),
            ("superseded", weights.superseded),
        ];
        for (key, value) in named {
            self.at_least(&join("weights", key), value.into(), 0.0);
        }
        let decay = &weights.decay;
        self.at_least("weights.decay.grace_days", decay.grace_days.into(), 0.0);
        self.above(
            "weights.decay.half_life_days",
            decay.half_life_days.into(),
            0.0,
        );
        self.between("weights.decay.floor", decay.floor.into(), 0.0, 1.0);
    }

    fn embed(&mut self, cfg: &Config) {
        let embed = &cfg.embed;
        self.not_blank("embed.model", &embed.model);
        self.not_blank("embed.enrich_model", &embed.enrich_model);
        self.at_least("embed.dimensions", embed.dimensions as f64, 1.0);
        self.at_least("embed.concurrency", embed.concurrency as f64, 1.0);
        self.at_least("embed.batch", embed.batch as f64, 1.0);
        self.at_least("embed.chunk_tokens", embed.chunk_tokens as f64, 1.0);
        if let Err(why) = http_url(&embed.ollama_url) {
            self.fail("embed.ollama_url", why);
        }
        if let Some(scheme) = &embed.prefix_scheme {
            if crate::embed::ollama::PrefixScheme::parse(scheme).is_none() {
                self.fail(
                    "embed.prefix_scheme",
                    format!("`{scheme}` is not one of qwen3, nomic, e5, plain"),
                );
            }
        }
    }

    fn pdf(&mut self, cfg: &Config) {
        self.between(
            "pdf.ocr_min_confidence",
            cfg.pdf.ocr_min_confidence.into(),
            0.0,
            1.0,
        );
        self.above("pdf.dpi", cfg.pdf.dpi.into(), 0.0);
    }

    fn memory(&mut self, cfg: &Config) {
        let memory = &cfg.memory;
        self.at_least(
            "memory.lessons_max_tokens",
            memory.lessons_max_tokens as f64,
            1.0,
        );
        self.between(
            "memory.min_confidence",
            memory.min_confidence.into(),
            0.0,
            100.0,
        );
        self.between(
            "memory.duplicate_similarity",
            memory.duplicate_similarity.into(),
            0.0,
            1.0,
        );
        self.at_least(
            "memory.episode_half_life_days",
            memory.episode_half_life_days.into(),
            0.0,
        );
        self.between(
            "memory.episode_decay_floor",
            memory.episode_decay_floor.into(),
            0.0,
            1.0,
        );
        self.at_least(
            "memory.distill_after_hours",
            memory.distill_after_hours.into(),
            0.0,
        );
        self.not_blank("memory.distill_model", &memory.distill_model);
        self.at_least("memory.max_memories", memory.max_memories as f64, 1.0);
    }

    fn backup(&mut self, cfg: &Config) {
        let backup = &cfg.backup;
        self.at_least(
            "backup.keep_generations",
            backup.keep_generations as f64,
            1.0,
        );
        self.at_least("backup.keep_index", backup.keep_index as f64, 1.0);
        if backup.enabled && backup.targets.is_empty() {
            self.fail(
                "backup.targets",
                "backup is enabled but lists no targets, so nothing is backed up".to_string(),
            );
        }
        for target in &backup.targets {
            match target.as_str() {
                "s3" if backup.s3.is_none() && !self.written_but_unreadable("backup.s3") => self
                    .fail(
                        "backup.targets",
                        "lists \"s3\" but there is no [backup.s3] table".to_string(),
                    ),
                "drive"
                    if backup.drive.is_none() && !self.written_but_unreadable("backup.drive") =>
                {
                    self.fail(
                        "backup.targets",
                        "lists \"drive\" but there is no [backup.drive] table".to_string(),
                    )
                }
                "s3" | "drive" => {}
                other => self.fail(
                    "backup.targets",
                    format!("unknown target `{other}`; supported targets are \"s3\" and \"drive\""),
                ),
            }
        }
    }

    fn sources(&mut self, cfg: &Config) {
        for source in &cfg.sources {
            let expanded = Config::expand_tilde_path(source);
            if !expanded.exists() {
                self.fail(
                    "sources",
                    format!(
                        "`{}` does not exist, and `br8n index` refuses to run until it does",
                        expanded.display()
                    ),
                );
            } else if !expanded.is_dir() && !expanded.is_file() {
                self.fail(
                    "sources",
                    format!("`{}` is not a file or a directory", expanded.display()),
                );
            }
        }
    }
}

fn http_url(raw: &str) -> Result<(), String> {
    match url::Url::parse(raw) {
        Ok(u) if matches!(u.scheme(), "http" | "https") && u.host().is_some() => Ok(()),
        Ok(u) => Err(format!(
            "`{raw}` must be an http:// or https:// URL, not {}://",
            u.scheme()
        )),
        Err(e) => Err(format!("`{raw}` is not a URL: {e}")),
    }
}

pub fn endpoint_url_problem(raw: &str) -> Option<String> {
    http_url(raw).err()
}

fn range_errors(cfg: &Config, unreadable: &[String]) -> Vec<FieldError> {
    let mut ranges = Ranges {
        errors: Vec::new(),
        unreadable,
    };
    ranges.surface("hook", &cfg.hook);
    ranges.surface("mcp", &cfg.mcp);
    ranges.weights(&cfg.weights);
    ranges.embed(cfg);
    ranges.pdf(cfg);
    ranges.memory(cfg);
    ranges.backup(cfg);
    ranges.sources(cfg);
    ranges.errors
}
