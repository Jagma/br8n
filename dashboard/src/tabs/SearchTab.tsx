import { useEffect, useState } from "react";
import { search, describeError, Explain, HitSummary, PALETTE } from "../api";
import { Divider, ErrorBanner } from "../components";
import { DocumentFacts } from "../DocumentFacts";

const PLAIN_STAGES = ["vector", "bm25", "graph"];

const TIERS = ["instant", "fast", "balanced", "thorough", "exhaustive"];

// `relevance` is captured at three genuinely different points in the
// pipeline, and a card must say which one it is looking at:
//
//   "real"       — the fused/injected columns. `weight_trace_and_gate`
//                  multiplies in the ranking weight and records the trace
//                  BEFORE applying the gate (src/retrieve/mod.rs), so this
//                  is the actual number `min_relevance` was compared
//                  against — the one real gate decision.
//   "pre-weight" — the vector column. Its `relevance` is a real cosine, but
//                  captured before `weight_and_order` multiplies in the
//                  ranking weight, so comparing it to the threshold is only
//                  a preview of the real decision above.
//   "unmeasured" — the bm25 and graph columns. Both stages set
//                  `relevance: 0.0` at capture time (src/retrieve/mod.rs:630
//                  for bm25, src/store/query.rs for graph expansion) because
//                  neither retriever measures cosine similarity — "never
//                  measured", not "dissimilar". The post-fusion measure
//                  stage backfills a real cosine later, but that happens
//                  AFTER these `HitSummary` snapshots are already taken, so
//                  the trace can never see it. Rendering a gate verdict off
//                  a 0.0 that means "not measured yet" is exactly the
//                  score/relevance conflation CLAUDE.md calls out as this
//                  project's whole defect class — so these columns get no
//                  above/below claim at all, regardless of what the number
//                  in a given hit happens to be today.
//
// This is a required prop with no default on purpose: the previous version
// used an optional boolean (`preWeight`) that defaulted to `false`, so a
// call site that forgot to pass it silently rendered the unqualified "real"
// verdict — the most dangerous of the three to get by accident.
type Gate = "real" | "pre-weight" | "unmeasured";

const SUBTITLE_CHARS = 120;

function subtitleOf(h: HitSummary): string {
  if (h.heading) return h.heading;
  const title = h.title.trim().toLowerCase();
  const line = h.excerpt
    .split("\n")
    .map((l) => l.replace(/^\s*#+\s*/, "").replace(/\s+/g, " ").trim())
    .find((l) => /[\p{L}\p{N}]/u.test(l) && l.toLowerCase() !== title) ?? "";
  return line.length > SUBTITLE_CHARS ? `${line.slice(0, SUBTITLE_CHARS - 1).trimEnd()}…` : line;
}

function RelevanceBar({ relevance }: { relevance: number }) {
  return (
    <div className="flex items-center gap-1.5">
      <div className="grow h-[5px] bg-sunken rounded overflow-hidden">
        <div className="h-full bg-accent rounded" style={{ width: `${Math.min(100, relevance * 100)}%` }} />
      </div>
      <span className="font-mono text-[11px]">{relevance.toFixed(3)}</span>
    </div>
  );
}

function NativeScore({ column, score }: { column: string; score: number }) {
  return (
    <div className="flex items-center gap-1.5 text-muted">
      {column === "bm25"
        ? <span className="font-mono text-[11px]">bm25 {score.toFixed(2)}</span>
        : <span className="text-[11px]">graph neighbour</span>}
      <span className="grow" />
      <span className="text-[9px] uppercase tracking-wider">{column === "bm25" ? "raw score" : "no score of its own"}</span>
    </div>
  );
}

function Card({ h, column, dim, injected, threshold, gate }: { h: HitSummary; column: string; dim?: boolean; injected?: boolean; threshold?: number; gate: Gate }) {
  const [open, setOpen] = useState(false);
  const dot = h.source_type === "transcript" ? PALETTE.session : PALETTE.accent;
  const subtitle = subtitleOf(h);
  return (
    <div className={`bg-surface border rounded-md px-2.5 py-2 flex flex-col gap-1.5 ${injected ? "border-accent" : "border-line"} ${dim ? "opacity-45" : ""}`}>
      <button className="flex items-center gap-1.5 min-w-0 text-left" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
        <span className="w-[7px] h-[7px] rounded-full shrink-0" style={{ background: dot }} />
        {/* No `title=` tooltip any more: the excerpt is the evidence for why
            this hit matched, and hiding it behind a hover was the reason a
            person could not tell a good hit from a bad one at a glance. */}
        <span className="text-xs font-medium truncate">{h.title}</span>
        {h.memory_kind && (
          <span className="text-[9px] font-semibold uppercase tracking-wider text-accent bg-sunken rounded px-1 py-px shrink-0">{h.memory_kind}</span>
        )}
        <span className="text-muted text-[10px] shrink-0">{open ? "▾" : "▸"}</span>
      </button>
      {subtitle && <div className="text-[11px] text-muted truncate -mt-1 pl-[13px]">{subtitle}</div>}
      {gate === "unmeasured" ? <NativeScore column={column} score={h.score} /> : <RelevanceBar relevance={h.relevance} />}
      {open && (
        <div className="flex flex-col gap-2 pt-1 border-t border-sunken">
          <div className="text-[11px] leading-relaxed whitespace-pre-wrap text-muted max-h-56 overflow-auto">
            {h.excerpt || "no excerpt"}
          </div>
          <div className="font-mono text-[10px] text-muted">
            {h.chunk_id}
            {gate === "unmeasured" && <> · not measured at this stage</>}
            {gate !== "unmeasured" && threshold !== undefined && (
              // Ranking weights multiply `relevance` in `weight_and_order`,
              // which runs AFTER the per-stage (vector/bm25/graph) hit lists
              // are captured but BEFORE the fused/injected lists are. So the
              // same chunk_id legitimately carries a different `relevance` in
              // a plain-stage column than in the fused column — comparing
              // either one to `threshold` is arithmetically correct, but only
              // the fused/injected comparison is the actual gate the hook
              // applies. The "pre-weight" gate labels the other one as the
              // hypothetical it is, so the two never read as the same claim.
              <> · {h.relevance >= threshold ? "above" : "below"} the {threshold.toFixed(2)} gate{gate === "pre-weight" ? " (pre-weight)" : ""}</>
            )}
          </div>
          <DocumentFacts id={h.doc_id} />
        </div>
      )}
    </div>
  );
}

function Column({ name, count, children }: { name: string; count: number; children: React.ReactNode }) {
  return (
    <div className="flex-1 min-w-0 flex flex-col gap-2">
      <div className="flex items-baseline gap-1.5 px-0.5">
        <span className="text-[11px] font-semibold uppercase tracking-wider text-muted">{name}</span>
        <span className="font-mono text-[11px] text-muted/70">{count}</span>
      </div>
      {children}
    </div>
  );
}

const CAP = 6;
function capped(hits: HitSummary[], render: (h: HitSummary) => React.ReactNode) {
  return (
    <>
      {hits.slice(0, CAP).map(render)}
      {hits.length > CAP && <div className="text-center text-[11px] text-muted/80">+ {hits.length - CAP} more</div>}
    </>
  );
}

export type SearchSuggestion = { q: string; n: number };

export default function SearchTab({ onHits, suggestion }: { onHits: (docIds: string[]) => void; suggestion?: SearchSuggestion | null }) {
  const [q, setQ] = useState("");
  const [tier, setTier] = useState(1);
  const [ex, setEx] = useState<Explain | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [suggested, setSuggested] = useState<string | null>(null);
  useEffect(() => {
    if (!suggestion) return;
    setQ(suggestion.q);
    setSuggested(suggestion.q);
    run(suggestion.q);
  }, [suggestion?.n]);
  const go = () => run(q);
  const run = async (q: string) => {
    if (!q.trim()) return;
    setBusy(true);
    setErr(null);
    try {
      const r = await search(q, tier);
      setEx(r);
      if (!r.error) onHits(r.fused.map((h) => h.doc_id));
    } catch (e) {
      // A 503 that survived `api.ts`'s one retry (store busy — e.g. a
      // reindex mid-swap) used to reach here as an unhandled rejection:
      // `ex` kept its stale value, the spinner reset, and the user saw
      // nothing happen at all. Surface it instead, distinct from the
      // embedding-outage banner below, which comes from a 200 the server
      // sends on purpose.
      setErr(describeError(e));
    } finally {
      setBusy(false);
    }
  };
  const stage = (n: string) => ex?.stages?.find((s) => s.name === n)?.hits ?? [];
  const injectedSet = new Set(ex?.injected ?? []);
  // Three states exist, and a card's appearance has to distinguish all three
  // or this tab is lying about the pipeline:
  //   1. cleared the gate — in the "fused" stage above the dashed line;
  //   2. survived MMR — also in `ex.fused` (the post-gate, post-MMR list);
  //   3. fit the token budget — also in `ex.injected`, which is what the hook
  //      REALLY puts in the prompt (see `Retriever::search_explained`).
  // The fused column shows the whole pre-gate pool, so its badge and its cards
  // can never disagree; the accent border on a card there means "this one was
  // injected", so an above-gate card without one was dropped by MMR or by the
  // budget — the contrast the previous version's comment described but never
  // rendered, because it passed no `injected` prop at all. The injected column
  // then shows all of `ex.fused` split by the budget line, so a chunk that
  // cleared the gate and lost only to `max_tokens` is visible as such instead
  // of vanishing.
  const threshold = ex?.threshold ?? 1;
  const fusedPool = stage("fused");
  const above = fusedPool.filter((h) => h.relevance >= threshold);
  const below = fusedPool.filter((h) => h.relevance < threshold);
  const admitted = (ex?.fused ?? []).filter((h) => injectedSet.has(h.chunk_id));
  const overBudget = (ex?.fused ?? []).filter((h) => !injectedSet.has(h.chunk_id));
  return (
    <div className="flex flex-col gap-4 h-full">
      <div className="flex items-center gap-3">
        <input value={q} onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => e.key === "Enter" && go()}
          placeholder="Search your knowledge base…"
          className="grow border border-line rounded-md px-3 py-2 text-sm bg-surface" />
        <div className="flex gap-0.5 bg-sunken rounded-md p-0.5">
          {TIERS.map((t, i) => (
            <button key={t} onClick={() => setTier(i)}
              className={`px-2.5 py-1 text-xs rounded ${i === tier ? "font-semibold text-accent bg-surface" : "text-muted"}`}>{t}</button>
          ))}
        </div>
        <button onClick={go} disabled={busy}
          className="px-4 py-2 bg-accent text-white rounded-md text-sm font-semibold disabled:opacity-50">Search</button>
        {ex && !ex.error && <span className="font-mono text-xs text-muted">{ex.elapsed_ms} ms{ex.degraded ? " · degraded" : ""}</span>}
      </div>
      {suggested !== null && suggested === q && (
        <div className="rounded-md px-4 py-2.5 text-sm bg-accent-soft text-accent flex items-center gap-3" role="status" data-testid="suggested-query">
          <span className="grow">
            Setup is done. This query was picked from your own notes to show what br8n finds; type your own question above.
          </span>
          <button onClick={() => setSuggested(null)} className="text-xs shrink-0">dismiss</button>
        </div>
      )}
      {err && <ErrorBanner>{err}</ErrorBanner>}
      {!err && ex?.error && (
        <ErrorBanner>
          {ex.error === "embedding unavailable" ? "Ollama is not reachable — search needs the embedding model. Graph and Health still work." : ex.error}
        </ErrorBanner>
      )}
      {!err && ex && !ex.error && (
        <div className="flex gap-2.5 grow min-h-0 overflow-auto">
          {PLAIN_STAGES.map((name) => (
            <Column key={name} name={name} count={stage(name).length}>
              {/* vector's relevance is a real, measured cosine — just not yet
                  weighted; bm25 and graph never measure one at all. The call
                  site decides "unmeasured" from which COLUMN this is, never
                  from the hit's own value, so a bm25 hit that happens to
                  arrive with a non-zero relevance still reads as unmeasured. */}
              {capped(stage(name), (h) => <Card key={h.chunk_id} h={h} column={name} threshold={threshold} gate={name === "vector" ? "pre-weight" : "unmeasured"} />)}
            </Column>
          ))}
          <Column name="fused · weighted" count={fusedPool.length}>
            {capped(above, (h) => <Card key={h.chunk_id} h={h} column="fused" injected={injectedSet.has(h.chunk_id)} threshold={threshold} gate="real" />)}
            <Divider label={`gate ${ex.threshold.toFixed(2)}`} />
            {capped(below, (h) => <Card key={h.chunk_id} h={h} column="fused" dim threshold={threshold} gate="real" />)}
          </Column>
          <Column name="injected" count={admitted.length}>
            {capped(admitted, (h) => <Card key={h.chunk_id} h={h} column="injected" injected threshold={threshold} gate="real" />)}
            {overBudget.length > 0 && (
              <>
                <Divider label="token budget" />
                {capped(overBudget, (h) => <Card key={h.chunk_id} h={h} column="injected" dim threshold={threshold} gate="real" />)}
              </>
            )}
          </Column>
        </div>
      )}
      {!err && !ex && <p className="text-muted text-sm p-2">Type a query to watch the pipeline: what each retriever found, how fusion ordered it, and what cleared the gate.</p>}
    </div>
  );
}
