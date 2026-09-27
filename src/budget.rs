//! The injection budget: how much retrieved text a surface may spend, and
//! which hits fit inside it.
//!
//! This lives on its own because two very different callers need the same
//! answer. `hook` renders the block it injects; `retrieve` reports which hits
//! the budget admitted so the dashboard can show what the prompt really
//! carried. The arithmetic used to live in `hook`, which meant the retrieval
//! core imported from a surface to ask a question that belongs to neither.

use crate::store::Hit;

/// One rendered excerpt, exactly as it appears inside the injected block.
fn entry_for(h: &Hit) -> String {
    let page = h.page_no.map(|p| format!(" p.{p}")).unwrap_or_default();
    let memory_line = match &h.memory {
        Some(f) => format!(
            "\n(memory: {}, {}, {}, id {})",
            f.kind.as_str(),
            crate::memory::ymd(f.created),
            f.project
                .as_deref()
                .map(|p| format!("project {p}"))
                .unwrap_or_else(|| "global".into()),
            crate::memory::id_from_uri(&h.uri).unwrap_or("?")
        ),
        None => String::new(),
    };
    format!(
        "\n[{}{}{}]({})\n{}{}\n",
        h.title,
        if h.heading_path.is_empty() {
            String::new()
        } else {
            format!(" > {}", h.heading_path)
        },
        page,
        h.uri,
        h.text.trim(),
        memory_line
    )
}

/// Walk `hits` in order under a `max_tokens` budget (4 chars to the token),
/// handing every admitted entry to `emit`, and return how many were admitted.
///
/// THE one definition of the injection budget, written once so the caller that
/// RENDERS the body and the caller that only COUNTS cannot drift apart. They
/// did drift: the dashboard's "injected" column listed every chunk that cleared
/// the gate — ten cards, where chunks of 1024-2048 chars meant the hook really
/// injected three or four.
///
/// A hit the budget can only take PART of still counts as admitted: its text is
/// in the prompt, truncated, which is a thing the reader was shown rather than
/// a thing that was dropped.
fn walk_budget(hits: &[Hit], max_tokens: usize, mut emit: impl FnMut(&str)) -> usize {
    let budget_chars = max_tokens * 4;
    let mut used = 0usize;
    let mut admitted = 0usize;

    for h in hits {
        let entry = entry_for(h);
        if used + entry.len() > budget_chars {
            let room = budget_chars.saturating_sub(used);
            if room > 200 {
                emit(&entry.chars().take(room).collect::<String>());
                admitted += 1;
            }
            break;
        }
        used += entry.len();
        emit(&entry);
        admitted += 1;
    }
    admitted
}

/// The rendered body, and how many hits the budget admitted.
pub fn fit_to_budget(hits: &[Hit], max_tokens: usize) -> (String, usize) {
    let mut body = String::new();
    let admitted = walk_budget(hits, max_tokens, |e| body.push_str(e));
    (body, admitted)
}

/// How many of `hits` the budget admits — the same walk as `fit_to_budget`,
/// without building the body a counter would only discard.
pub fn admitted_by_budget(hits: &[Hit], max_tokens: usize) -> usize {
    walk_budget(hits, max_tokens, |_| {})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{ymd, MemoryFacts, MemoryKind, Origin};

    fn base_hit(uri: &str) -> Hit {
        Hit {
            chunk_id: "c1".into(),
            doc_id: "d1".into(),
            text: "Never comment code unless asked.".into(),
            heading_path: String::new(),
            uri: uri.into(),
            title: "Never comment code unless asked".into(),
            page_no: None,
            score: 0.9,
            relevance: 0.9,
            source_type: "memory".into(),
            inbound: 0,
            lifecycle: Default::default(),
            last_used: None,
            memory: None,
        }
    }

    #[test]
    fn a_project_scoped_memory_hit_renders_kind_date_scope_and_id() {
        let mut h = base_hit("memory://lesson/abc123def456");
        h.memory = Some(MemoryFacts {
            kind: MemoryKind::Lesson,
            created: 1_700_000_000,
            project: Some("/Users/x/repo".into()),
            origin: Origin::User,
            confidence: 100,
            session: None,
            source_hash: None,
            source_stamp: None,
        });

        let (body, admitted) = fit_to_budget(&[h], 10_000);
        assert_eq!(admitted, 1, "the one hit must be admitted whole");
        assert_eq!(
            body,
            format!(
                "\n[Never comment code unless asked](memory://lesson/abc123def456)\n\
                 Never comment code unless asked.\n\
                 (memory: lesson, {}, project /Users/x/repo, id abc123def456)\n",
                ymd(1_700_000_000)
            ),
            "the whole rendered line, exactly as `br8n-memory/SKILL.md` parses it"
        );
    }

    #[test]
    fn a_global_memory_hit_renders_scope_as_global() {
        let mut h = base_hit("memory://fact/deadbeefcafe");
        h.memory = Some(MemoryFacts {
            kind: MemoryKind::Fact,
            created: 1_700_000_000,
            project: None,
            origin: Origin::Claude,
            confidence: 90,
            session: None,
            source_hash: None,
            source_stamp: None,
        });

        let (body, admitted) = fit_to_budget(&[h], 10_000);
        assert_eq!(admitted, 1);
        assert_eq!(
            body,
            format!(
                "\n[Never comment code unless asked](memory://fact/deadbeefcafe)\n\
                 Never comment code unless asked.\n\
                 (memory: fact, {}, global, id deadbeefcafe)\n",
                ymd(1_700_000_000)
            )
        );
    }

    #[test]
    fn a_non_memory_hit_renders_no_memory_line() {
        let h = base_hit("file:///n/pool.md");
        let (body, admitted) = fit_to_budget(&[h], 10_000);
        assert_eq!(admitted, 1);
        assert_eq!(
            body,
            "\n[Never comment code unless asked](file:///n/pool.md)\n\
             Never comment code unless asked.\n",
            "a non-memory hit's line must be unchanged"
        );
    }
}
