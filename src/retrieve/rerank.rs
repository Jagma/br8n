use crate::store::Hit;

pub struct Reranker {
    url: String,
    model: String,
    client: reqwest::blocking::Client,
    deadline: Option<std::time::Instant>,
}

impl Reranker {
    /// Construct with a hard ceiling on how long reranking may take.
    ///
    /// The stage deadline only gates stage ENTRY: a rerank that begins one
    /// millisecond inside the budget still runs to its own timeout. Measured
    /// with a 700ms tier budget, `br8n bench` reported a p50 of 7288ms —
    /// ten times over. Passing the remaining budget in makes the ceiling real.
    pub fn with_budget(url: &str, model: &str, budget_ms: u64) -> Self {
        let budget = std::time::Duration::from_millis(budget_ms.max(200));
        let mut r = Self::new(url, model);
        // Per-REQUEST timeout, and a whole-stage deadline below. The per-request
        // one alone is not enough: scoring is one request per candidate, so a
        // 20-candidate rerank could take twenty times the budget and did —
        // p95 5495ms against a 700ms tier.
        r.client = reqwest::blocking::Client::builder()
            .timeout(budget)
            .build()
            .expect("build http client");
        r.deadline = Some(std::time::Instant::now() + budget);
        r
    }

    pub fn new(url: &str, model: &str) -> Self {
        Self {
            url: url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            deadline: None,
            client: reqwest::blocking::Client::builder()
                // A cold model load measured 2043 ms against the previous 2000 ms
                // timeout — so the first rerank after Ollama unloaded the model
                // always timed out, returned None, and silently fell back to input
                // order. Warm calls are ~36 ms; this headroom only ever costs
                // anything on a genuine cold start.
                .timeout(std::time::Duration::from_millis(6000))
                .build()
                .expect("build http client"),
        }
    }

    /// Never fails. A reranker that errors must cost quality, not results.
    pub fn rerank(&self, query: &str, hits: Vec<Hit>, top_n: usize) -> Vec<Hit> {
        if hits.is_empty() {
            return hits;
        }
        let n = top_n.min(hits.len());
        let (head, tail) = hits.split_at(n);
        let mut head = head.to_vec();

        // Score into a buffer first. Mutating in place meant a mid-list failure
        // returned a MIXTURE: candidates scored before the failure kept their new
        // 1.0/0.0 while the rest kept RRF values. Measured on a stub that dies
        // after two answers: [0.99, 0.75, 0.42, 0.10] came back [1.0, 1.0, 0.42,
        // 0.10]. The gate would then compare two incomparable scales.
        let mut scored = Vec::with_capacity(head.len());
        for h in head.iter() {
            // Out of time is handled exactly like a failure: return everything
            // UNRERANKED. A partial rerank would mix a binary verdict with RRF
            // values in one list, which is the incomparable-scales bug this
            // function was already written to avoid.
            if self
                .deadline
                .is_some_and(|d| std::time::Instant::now() >= d)
            {
                return [head, tail.to_vec()].concat();
            }
            match self.score(query, &h.text) {
                Some(s) => scored.push(s),
                None => return [head, tail.to_vec()].concat(),
            }
        }
        for (h, s) in head.iter_mut().zip(scored) {
            // ORDER only. `relevance` stays the bi-encoder cosine.
            //
            // Overwriting it with this verdict replaced a calibrated [0,1]
            // similarity with a binary 1.0/0.0, which made `threshold` mean
            // nothing at tiers 3 and 4: every value in (0,1] gated identically,
            // and the numbers `br8n bench` reported came from a scale no other
            // tier produces. A reranked hit that the model rejects now sorts
            // last and is dropped by truncation, rather than being laundered
            // into a relevance score.
            h.score = s;
        }
        // `sort_by` is a stable sort, so hits that tie on score (every
        // reranked hit scores exactly 1.0 or 0.0) keep their pre-rerank
        // relative order rather than being shuffled. That pre-rerank order
        // is itself deterministic (fusion carries a chunk_id tie-break), so
        // reranking never reintroduces non-determinism.
        head.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        [head, tail.to_vec()].concat()
    }

    /// Greedy-decodes a single token for a yes/no relevance judgement and
    /// string-matches the response against `starts_with("yes")` — it does not
    /// use the model's logprob. This works with any instruct model served by
    /// Ollama's `/api/generate` and needs no dedicated rerank endpoint.
    fn score(&self, query: &str, doc: &str) -> Option<f32> {
        let prompt = format!(
            "Query: {query}\n\nDocument: {}\n\n\
             Is this document relevant to the query? Answer yes or no.",
            doc.chars().take(2000).collect::<String>()
        );
        let body = serde_json::json!({
            "model": self.model,
            "prompt": prompt,
            "stream": false,
            // qwen3 is a thinking model: without this the first generated token opens a
            // <think> block, so `num_predict: 1` returns nothing and every document
            // scores 0.0 — reranking silently inverted into a no-op.
            // Pin the model, exactly as the embedder does. Ollama unloads after
            // ~5 minutes idle, and reloading costs 2043 ms versus 36 ms warm.
            "keep_alive": "30m",
            "think": false,
            "options": { "num_predict": 1, "temperature": 0.0 },
        });
        let resp: serde_json::Value = self
            .client
            .post(format!("{}/api/generate", self.url))
            .json(&body)
            .send()
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .ok()?;
        let answer = resp["response"].as_str()?.trim().to_lowercase();
        Some(if answer.starts_with("yes") { 1.0 } else { 0.0 })
    }
}
