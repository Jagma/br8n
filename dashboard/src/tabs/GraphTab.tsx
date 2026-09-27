import { useEffect, useMemo, useRef, useState } from "react";
import ForceGraph2D from "react-force-graph-2d";
import { AGENT_NAME, describeError, getGraph, getMemories, getReach, Graph, GraphEdge, GraphNode, Memories, PALETTE } from "../api";
import { ErrorBanner } from "../components";
import { DocumentFacts } from "../DocumentFacts";

const COLOR: Record<string, string> = {
  markdown: PALETTE.note, pdf: PALETTE.note, web: PALETTE.note,
  transcript: PALETTE.session, entity: PALETTE.entity, tag: PALETTE.tag,
  memory: PALETTE.memory, project: PALETTE.project,
};
const MEMORY_SHADE: Record<string, string> = {
  lesson: "#D96E96", fact: "#C2557A", episode: "#9E3F5F",
};
const NODE_REL_SIZE = 4;
const LABEL_ZOOM = 1.5;
const LABEL_CHARS = 32;
const NOT_DOCUMENTS = new Set(["memory", "project"]);

const nodeValue = (n: any) => 4 + 2 * Math.sqrt(n.inbound ?? 0);
const nodeRadius = (n: any) => Math.sqrt(nodeValue(n)) * NODE_REL_SIZE;
const shortTitle = (t: string) => (t.length > LABEL_CHARS ? `${t.slice(0, LABEL_CHARS - 1).trimEnd()}…` : t);

function NoDocuments({ onAddSource, onSetup }: { onAddSource: () => void; onSetup: () => void }) {
  return (
    <div className="absolute inset-0 flex items-center justify-center pointer-events-none">
      <div className="max-w-md bg-[#16211DD9] border border-[#2C3A35] rounded-lg px-5 py-4 flex flex-col gap-3 text-sm text-[#93A69F] pointer-events-auto">
        <span className="font-semibold text-[#DCE9E4]">No documents indexed yet</span>
        <span>The graph is drawn from the folders br8n indexes. Add one and index it to see your notes here.</span>
        <div className="flex items-center gap-2">
          <button onClick={onSetup} className="px-4 py-2 bg-accent text-white rounded-md text-sm font-semibold">
            Open guided setup
          </button>
          <button onClick={onAddSource} className="px-4 py-2 border border-[#2C3A35] text-[#DCE9E4] rounded-md text-sm font-semibold">
            Add a folder
          </button>
        </div>
        <span className="text-xs">Guided setup picks folders, connects your coding agents and runs the first index.</span>
        <span className="text-xs">
          Opens Settings › Sources. From a terminal: add it to <code className="font-mono text-[#DCE9E4]">sources</code> in
          config.toml, then run <code className="font-mono text-[#DCE9E4]">br8n index</code>.
        </span>
      </div>
    </div>
  );
}

function ProjectDetails({ node, edges }: { node: GraphNode; edges: GraphEdge[] }) {
  const count = edges.filter((e) => e.kind === "scoped-to" && e.to === node.id).length;
  return (
    <dl className="text-[11px] grid grid-cols-[auto_1fr] gap-x-2 gap-y-1">
      <dt className="text-muted">Path</dt>
      <dd className="font-mono truncate" title={node.title}>{node.title}</dd>
      <dt className="text-muted">Memories</dt>
      <dd>{count}</dd>
    </dl>
  );
}

function MemoryDetails({
  node, memories, reach, reachError, reaching, onReach,
}: {
  node: GraphNode;
  memories: Memories | null;
  reach: { title: string; uri: string; relevance: number }[] | null;
  reachError: string | null;
  reaching: boolean;
  onReach: () => void;
}) {
  if (!memories) return <div className="text-[11px] text-muted">Loading…</div>;
  if (memories.unavailable) return <div className="text-[11px] text-muted">{memories.unavailable}</div>;
  const m = memories.memories.find((x) => x.id === node.memory_id);
  if (!m) return <div className="text-[11px] text-muted">This memory could not be found.</div>;
  return (
    <>
      <dl className="text-[11px] grid grid-cols-[auto_1fr] gap-x-2 gap-y-1">
        <dt className="text-muted">Kind</dt>
        <dd>{m.facts.kind}</dd>
        <dt className="text-muted">Created</dt>
        <dd>{new Date(m.facts.created * 1000).toLocaleString()}</dd>
        {m.facts.project && (
          <>
            <dt className="text-muted">Project</dt>
            <dd className="font-mono truncate" title={m.facts.project}>{m.facts.project}</dd>
          </>
        )}
        <dt className="text-muted">Origin</dt>
        <dd>{m.facts.origin}</dd>
        <dt className="text-muted">Confidence</dt>
        <dd>{m.facts.confidence}</dd>
        {m.facts.session && (
          <>
            <dt className="text-muted">Session</dt>
            <dd className="font-mono truncate" title={m.facts.session}>{m.facts.session}</dd>
          </>
        )}
      </dl>
      <div className="text-sm">{m.text}</div>
      <button
        onClick={onReach}
        className="self-start text-xs font-semibold text-accent bg-accent-soft border border-line rounded-md px-2.5 py-1.5"
      >
        {reaching ? "Reaching…" : "What would this pull in?"}
      </button>
      {reachError && (
        <div>
          <div className="text-[11px] font-semibold uppercase tracking-wider text-muted pb-1.5">Reach</div>
          <div className="text-sm text-[#D98E8E]">{reachError}</div>
        </div>
      )}
      {reach && (
        <div>
          <div className="text-[11px] font-semibold uppercase tracking-wider text-muted pb-1.5">Reach</div>
          {reach.length === 0 && <div className="text-sm text-muted">nothing</div>}
          {reach.map((r) => (
            <div key={r.uri} className="flex items-center justify-between gap-2 py-1.5 border-t border-sunken text-sm">
              <span className="truncate">{r.title}</span>
              <span className="text-muted shrink-0">{r.relevance.toFixed(2)}</span>
            </div>
          ))}
        </div>
      )}
    </>
  );
}

export default function GraphTab({
  highlight,
  reloads,
  onAddSource,
  onSetup,
}: {
  highlight: Set<string>;
  reloads: number;
  onAddSource: () => void;
  onSetup: () => void;
}) {
  const [graph, setGraph] = useState<Graph | null>(null);
  const [memories, setMemories] = useState<Memories | null>(null);
  const [showSessions, setShowSessions] = useState(false);
  const [selected, setSelected] = useState<GraphNode | null>(null);
  const selectedRef = useRef<GraphNode | null>(null);
  selectedRef.current = selected;
  const [err, setErr] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    setErr(null);
    getGraph().then(setGraph).catch((e) => setErr(describeError(e)));
    getMemories().then(setMemories).catch((e) => setMemories({ memories: [], unavailable: describeError(e) }));
  }, [attempt, reloads]);

  const [reach, setReach] = useState<{ title: string; uri: string; relevance: number }[] | null>(null);
  const [reachError, setReachError] = useState<string | null>(null);
  const [reaching, setReaching] = useState(false);
  useEffect(() => {
    setReach(null);
    setReachError(null);
    setReaching(false);
  }, [selected?.id]);
  const loadReach = () => {
    const id = selected?.memory_id;
    if (!id) return;
    setReaching(true);
    setReach(null);
    setReachError(null);
    const stale = () => selectedRef.current?.memory_id !== id;
    getReach(id)
      .then((r) => {
        if (stale()) return;
        if (r.error) setReachError(r.error);
        else setReach(r.reach ?? []);
      })
      .catch((e) => {
        if (stale()) return;
        setReachError(describeError(e));
      })
      .finally(() => {
        if (!stale()) setReaching(false);
      });
  };

  // react-force-graph-2d doesn't observe its container — it sizes the canvas
  // from explicit width/height props (falling back to the whole window), so
  // without this the canvas renders at 0x0 inside our flex/rounded panel.
  const viewportRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ width: 0, height: 0 });
  useEffect(() => {
    const el = viewportRef.current;
    if (!el) return;
    const ro = new ResizeObserver(([entry]) => {
      const { width, height } = entry.contentRect;
      // A hidden tab measures 0x0. `App` keeps visited tabs mounted so the
      // graph is not refetched and re-simulated on every visit, which means
      // this fires with zeroes each time the user switches away. Taking that
      // measurement would collapse the canvas and it would come back empty,
      // so the last real size stands until a real one replaces it.
      if (width === 0 || height === 0) return;
      setSize({ width, height });
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const data = useMemo(() => {
    if (!graph) return { nodes: [], links: [] };
    const docs = graph.nodes.filter((n) => showSessions || n.source_type !== "transcript");
    const ids = new Set(docs.map((n) => n.id));
    const nodes = [
      ...docs.map((n) => ({ ...n, kind: n.source_type })),
      ...graph.entities.map((e) => ({ id: e.id, title: e.name, kind: "entity", chunks: 0, inbound: 0, source_type: "entity" })),
      ...graph.tags.map((t) => ({ id: `tag:${t}`, title: `#${t}`, kind: "tag", chunks: 0, inbound: 0, source_type: "tag" })),
    ];
    const nodeIds = new Set(nodes.map((n) => n.id));
    const links = graph.edges
      .map((e) => ({ source: e.from, target: e.kind === "tagged" ? `tag:${e.to}` : e.to, kind: e.kind }))
      .filter((l) => nodeIds.has(l.source) && nodeIds.has(l.target));
    void ids;
    return { nodes, links };
  }, [graph, showSessions]);

  const documentCount = graph ? graph.nodes.filter((n) => !NOT_DOCUMENTS.has(n.source_type)).length : 0;
  const hidden = graph ? graph.nodes.filter((n) => n.source_type === "transcript").length : 0;
  const sessionAgents = graph
    ? [...new Set(graph.nodes.filter((n) => n.agent).map((n) => AGENT_NAME[n.agent!] ?? n.agent!))].sort()
    : [];
  const titleOf = (id: string) => graph?.nodes.find((n) => n.id === id)?.title ?? id;
  // Document-to-document edges only: `mentions` and `tagged` point at entities
  // and tags, which are not documents and have no title to show here. A
  // denylist rather than a list of kinds, so a new KIND (a new frontmatter
  // relation) appears automatically; a new edge TABLE would have to be added.
  //
  // Deduped by (direction, title). One pair of documents can now carry several
  // edges — a prose wikilink AND a declared `superseded-by`, which is exactly
  // what every ADR pair in a real vault looks like — and listing the same
  // neighbour twice with nothing to tell the rows apart reads as a rendering
  // bug. The old filter collapsed them only because every kind was the same
  // constant.
  const neighbors = selected && graph
    ? [
        ...new Map(
          graph.edges
            .filter((e) => e.kind !== "mentions" && e.kind !== "tagged" && (e.from === selected.id || e.to === selected.id))
            .map((e) => (e.from === selected.id ? { dir: "→", t: titleOf(e.to) } : { dir: "←", t: titleOf(e.from) }))
            .map((n) => [`${n.dir}${n.t}`, n] as const),
        ).values(),
      ]
    : [];

  // Tags and mentions for the selected document. Both are already in every
  // /api/graph payload — `tagged` edges point at a tag string, `mentions` at an
  // Entity id — and the neighbour list above filters them out because they have
  // no document title to render. They were never surfaced anywhere else.
  const entityName = (id: string) => graph?.entities.find((e) => e.id === id)?.name ?? id;
  const tagsOf = selected && graph
    ? [...new Set(graph.edges.filter((e) => e.kind === "tagged" && e.from === selected.id).map((e) => e.to))].sort()
    : [];
  // `graph_snapshot` (src/store/query.rs) already dedupes `(doc_id,
  // entity_id)` pairs server-side, via its own `seen_mentions` set, so this
  // document never receives more than one `mentions` edge per entity ID —
  // the per-CHUNK duplication this `new Set` was described as guarding
  // against does not reach the wire. What this `Set` actually collapses is
  // NAMES, after `entityName` resolves each edge's entity id: two distinct
  // `Entity` records that happen to share a display name become one chip
  // here. That's real, silent behaviour worth knowing about if two mentions
  // chips vanish into one and the count looks short.
  const mentionsOf = selected && graph
    ? [...new Set(graph.edges.filter((e) => e.kind === "mentions" && e.from === selected.id).map((e) => entityName(e.to)))].sort()
    : [];

  if (err) {
    return (
      <div className="grow flex flex-col gap-3 items-center justify-center bg-viewport rounded-lg">
        <ErrorBanner>{err}</ErrorBanner>
        <button onClick={() => setAttempt((n) => n + 1)} className="text-xs font-semibold text-[#DCE9E4] underline">
          Try again
        </button>
      </div>
    );
  }

  return (
    <div className="flex gap-4 h-full">
      <div ref={viewportRef} className="grow relative bg-viewport rounded-lg overflow-hidden">
        <button onClick={() => setShowSessions(!showSessions)}
          className="absolute z-10 top-3 left-3 text-xs text-[#93A69F] bg-[#16211DD9] border border-[#2C3A35] rounded-md px-2.5 py-1.5">
          Sessions {showSessions ? "shown" : `(${hidden} hidden)`}
        </button>
        <ForceGraph2D
          width={size.width}
          height={size.height}
          graphData={data}
          backgroundColor={PALETTE.viewport}
          nodeLabel={(n: any) => n.title}
          nodeRelSize={NODE_REL_SIZE}
          nodeVal={nodeValue}
          nodeColor={(n: any) => (n.memory_kind ? MEMORY_SHADE[n.memory_kind] ?? PALETTE.memory : COLOR[n.kind] ?? PALETTE.note)}
          linkColor={() => "#2C3A35"}
          nodeCanvasObjectMode={() => "after"}
          nodeCanvasObject={(n: any, ctx, globalScale) => {
            const r = nodeRadius(n);
            if (highlight.has(n.id) || selected?.id === n.id) {
              ctx.beginPath();
              ctx.arc(n.x, n.y, r + 2, 0, 2 * Math.PI);
              ctx.strokeStyle = "#DCE9E4";
              ctx.lineWidth = 1.5;
              ctx.stroke();
            }
            if (globalScale < LABEL_ZOOM) return;
            ctx.font = `${11 / globalScale}px sans-serif`;
            ctx.textAlign = "center";
            ctx.textBaseline = "top";
            ctx.fillStyle = "#DCE9E4";
            ctx.fillText(shortTitle(n.title ?? ""), n.x, n.y + r + 3 / globalScale);
          }}
          onNodeClick={(n: any) => setSelected(graph?.nodes.find((g) => g.id === n.id) ?? null)}
        />
        {graph && documentCount === 0 && <NoDocuments onAddSource={onAddSource} onSetup={onSetup} />}
        <div className="absolute bottom-3 left-3 flex gap-3.5 text-xs text-[#DCE9E4] bg-[#16211DD9] border border-[#2C3A35] rounded-md px-3 py-2">
          {(["note", "session", "tag", "entity", "memory", "project"] as const).map((k) => (
            <span key={k} className="flex items-center gap-1.5">
              <span className="w-2 h-2 rounded-full" style={{ background: PALETTE[k] }} />
              {k === "session" && sessionAgents.length > 1 ? `session (${sessionAgents.join(", ")})` : k}
            </span>
          ))}
        </div>
      </div>
      {selected && (
        <div className="w-[330px] shrink-0 bg-surface border border-line rounded-lg p-5 flex flex-col gap-3 overflow-auto">
          <div className="flex gap-2 items-center">
            <span className="text-[11px] font-semibold uppercase tracking-wider text-accent bg-accent-soft px-2 py-0.5 rounded">{selected.source_type}</span>
            {selected.agent && (
              <span data-agent={selected.agent} className="text-[11px] font-semibold uppercase tracking-wider text-muted bg-sunken px-2 py-0.5 rounded">
                {AGENT_NAME[selected.agent] ?? selected.agent}
              </span>
            )}
            {/* Chunk/inbound counts used to be rendered here too, from the
                one-time /api/graph payload fetched on mount — a second,
                independent copy of the same two numbers `DocumentFacts`
                below renders from its own per-click fetch. They agreed only
                by accident: after any re-index while this tab stays open,
                the two would disagree in one panel. `DocumentFacts` is the
                shared facts component precisely so a fact is rendered from
                one source; let it own these. */}
          </div>
          <div className="font-serif font-semibold text-lg leading-snug">{selected.title}</div>
          {selected.source_type === "memory" ? (
            <MemoryDetails node={selected} memories={memories} reach={reach} reachError={reachError} reaching={reaching} onReach={loadReach} />
          ) : selected.source_type === "project" ? (
            <ProjectDetails node={selected} edges={graph?.edges ?? []} />
          ) : (
            <DocumentFacts id={selected.id} />
          )}
          <div>
            <div className="text-[11px] font-semibold uppercase tracking-wider text-muted pb-1.5">Linked notes</div>
            {neighbors.length === 0 && <div className="text-sm text-muted">none</div>}
            {neighbors.map((n, i) => (
              <div key={i} className="flex items-center gap-2 py-1.5 border-t border-sunken text-sm">
                <span className="text-muted">{n.dir}</span>{n.t}
              </div>
            ))}
          </div>
          {tagsOf.length > 0 && (
            <div>
              <div className="text-[11px] font-semibold uppercase tracking-wider text-muted pb-1.5">Tags</div>
              <div className="flex flex-wrap gap-1.5">
                {tagsOf.map((t) => (
                  <span key={t} className="text-[11px] font-mono bg-sunken px-1.5 py-0.5 rounded">#{t}</span>
                ))}
              </div>
            </div>
          )}
          {mentionsOf.length > 0 && (
            <div>
              <div className="text-[11px] font-semibold uppercase tracking-wider text-muted pb-1.5">Mentions</div>
              <div className="flex flex-wrap gap-1.5">
                {mentionsOf.map((m) => (
                  <span key={m} className="text-[11px] bg-sunken px-1.5 py-0.5 rounded">{m}</span>
                ))}
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
