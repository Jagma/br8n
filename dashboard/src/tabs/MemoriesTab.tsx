import { useEffect, useMemo, useRef, useState } from "react";
import {
  deleteMemory,
  describeError,
  getMemories,
  getReach,
  Memories,
  Memory,
  PALETTE,
  saveMemory,
} from "../api";
import { ErrorBanner } from "../components";

const KINDS = ["lesson", "fact", "episode"] as const;

const KIND_SHADE: Record<string, string> = {
  lesson: "#D96E96", fact: "#C2557A", episode: "#9E3F5F",
};

type Draft = { id: string | null; kind: string; text: string; scope: string; global: boolean; confidence: string };

type ReachRow = { doc_id: string; title: string; uri: string; relevance: number };

type Notice = { text: string; warn: boolean; open?: string; stale?: string; kept?: string };

type Busy = { op: "save" | "delete"; label: string };

type Leave = { to: "pick"; id: string } | { to: "new" } | { to: "close" };

type Saved = Awaited<ReturnType<typeof saveMemory>>;

const NEW_DRAFT: Draft = { id: null, kind: "lesson", text: "", scope: "", global: true, confidence: "100" };

function draftFrom(m: Memory): Draft {
  return {
    id: m.id,
    kind: m.facts.kind,
    text: m.text,
    scope: m.facts.project ?? "",
    global: m.facts.project == null,
    confidence: String(m.facts.confidence),
  };
}

function lineBreaksAsTyped(text: string): string {
  return text.replace(/\r\n?/g, "\n");
}

function sameDraft(a: Draft, b: Draft): boolean {
  return (
    a.kind === b.kind &&
    lineBreaksAsTyped(a.text) === lineBreaksAsTyped(b.text) &&
    a.global === b.global &&
    (a.global || a.scope === b.scope) &&
    a.confidence === b.confidence
  );
}

function refusal(d: Draft): string | null {
  if (!d.global && d.scope.trim() === "") {
    return "Not saved: global is unticked but the project path is empty. Enter an absolute project path, or tick global.";
  }
  if (!/^\d+$/.test(d.confidence) || Number(d.confidence) > 100) {
    return "Not saved: confidence must be a whole number from 0 to 100.";
  }
  return null;
}

function scopeOf(m: Memory): string {
  return m.facts.project ?? "global";
}

function firstLine(text: string): string {
  const line = text.split("\n").find((l) => l.trim() !== "");
  return (line ?? text).trim();
}

function clip(text: string, max = 60): string {
  const line = firstLine(text);
  return line.length > max ? `${line.slice(0, max)}…` : line;
}

function day(secs: number): string {
  return new Date(secs * 1000).toLocaleDateString();
}

function reason(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function savedText(saved: Saved, editedId: string | null): string {
  if (saved.outcome !== "replaced") return `Saved as ${saved.id}.`;
  if (editedId === null) {
    const gone = saved.previous_title ? `“${saved.previous_title}”` : "a memory the server did not name";
    return `Saved as ${saved.id}, and it REPLACED an existing memory that said nearly the same thing: ${gone}. That earlier memory is gone.`;
  }
  return saved.id === editedId
    ? `Saved your edit to ${saved.id}.`
    : `Saved your edit as ${saved.id} (it was ${editedId}).`;
}

function KindChip({ kind }: { kind: string }) {
  return (
    <span
      className="text-[10px] font-semibold uppercase tracking-wider text-white rounded px-1.5 py-0.5 shrink-0"
      style={{ background: KIND_SHADE[kind] ?? PALETTE.memory }}
    >
      {kind}
    </span>
  );
}

export default function MemoriesTab({ onWritten }: { onWritten: () => void }) {
  const [data, setData] = useState<Memories | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [kind, setKind] = useState<string>("all");
  const [project, setProject] = useState<string>("all");
  const [needle, setNeedle] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [baseline, setBaseline] = useState<Draft | null>(null);
  const [panelErr, setPanelErr] = useState<string | null>(null);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [busy, setBusy] = useState<Busy | null>(null);
  const [confirming, setConfirming] = useState<{ id: string; title: string } | null>(null);
  const [leaving, setLeaving] = useState<Leave | null>(null);
  const [reach, setReach] = useState<ReachRow[] | null>(null);
  const [reachError, setReachError] = useState<string | null>(null);
  const [reaching, setReaching] = useState(false);
  const reachFor = useRef<string | null>(null);
  const panelGeneration = useRef(0);
  const writing = useRef(false);

  const firstLoad = () => {
    setErr(null);
    setData(null);
    getMemories().then(setData).catch((e) => setErr(describeError(e)));
  };

  const load = async (): Promise<Memories> => {
    const d = await getMemories();
    if (d.unavailable) throw new Error(d.unavailable);
    setData(d);
    return d;
  };

  useEffect(firstLoad, []);

  const all = useMemo(
    () => [...(data?.memories ?? [])].sort((a, b) => b.facts.created - a.facts.created),
    [data],
  );
  const selected = all.find((m) => m.id === selectedId) ?? null;

  const scopes = useMemo(() => [...new Set(all.map(scopeOf))].sort(), [all]);
  const scope = project === "all" || scopes.includes(project) ? project : "all";

  useEffect(() => {
    if (scope !== project) setProject(scope);
  }, [scope, project]);

  const shown = useMemo(() => {
    const n = needle.trim().toLowerCase();
    return all.filter((m) => {
      if (kind !== "all" && m.facts.kind !== kind) return false;
      if (scope !== "all" && scopeOf(m) !== scope) return false;
      if (n === "") return true;
      return (
        m.text.toLowerCase().includes(n) ||
        m.title.toLowerCase().includes(n) ||
        m.id.toLowerCase().includes(n)
      );
    });
  }, [all, kind, scope, needle]);

  const dirty = draft !== null && baseline !== null && !sameDraft(draft, baseline);
  const locked = busy !== null;

  const openPanel = (id: string | null, next: Draft | null) => {
    panelGeneration.current += 1;
    setSelectedId(id);
    setDraft(next);
    setBaseline(next);
    setPanelErr(null);
    setConfirming(null);
    setLeaving(null);
    setReach(null);
    setReachError(null);
    setReaching(false);
    reachFor.current = null;
  };

  const go = (to: Leave) => {
    if (to.to !== "pick") {
      setNotice((n) => (n ? { ...n, kept: undefined } : n));
      return openPanel(null, to.to === "new" ? { ...NEW_DRAFT } : null);
    }
    const m = all.find((x) => x.id === to.id);
    if (!m) {
      setLeaving(null);
      setNotice({ text: `${to.id} is not in the list, so it cannot be opened.`, warn: true });
      return;
    }
    if (notice?.open === to.id) setNotice(null);
    else setNotice((n) => (n ? { ...n, kept: undefined } : n));
    openPanel(m.id, draftFrom(m));
  };

  const request = (to: Leave) => {
    if (writing.current) return;
    if (to.to === "pick" && to.id === selectedId) return;
    if (dirty) {
      setLeaving(to);
      return;
    }
    go(to);
  };

  const edit = (patch: Partial<Draft>) => {
    panelGeneration.current += 1;
    setPanelErr(null);
    setDraft((d) => (d ? { ...d, ...patch } : d));
  };

  const reload = async () => {
    try {
      await load();
      setNotice((n) => (n ? { ...n, stale: undefined } : n));
    } catch (e) {
      const why = describeError(e);
      setNotice((n) => (n ? { ...n, stale: why } : n));
    }
  };

  const save = async () => {
    if (!draft || writing.current) return;
    const problem = refusal(draft);
    if (problem) {
      setNotice(null);
      setPanelErr(problem);
      return;
    }
    const sent = draft;
    const unchangedText = baseline !== null && lineBreaksAsTyped(sent.text) === lineBreaksAsTyped(baseline.text);
    const token = panelGeneration.current;
    const stillHere = () => panelGeneration.current === token;
    writing.current = true;
    setBusy({ op: "save", label: clip(sent.text) });
    setPanelErr(null);
    setNotice(null);
    setConfirming(null);
    setLeaving(null);
    try {
      let saved: Saved;
      try {
        saved = await saveMemory({
          kind: sent.kind,
          text: unchangedText && baseline ? baseline.text : sent.text,
          scope: sent.global ? "global" : sent.scope,
          confidence: Number(sent.confidence),
          ...(sent.id ? { id: sent.id } : {}),
        });
      } catch (e) {
        if (stillHere()) setPanelErr(reason(e));
        else setNotice({ text: `“${clip(sent.text)}” was not saved: ${reason(e)}`, warn: true });
        return;
      }
      if (saved.outcome !== "duplicate") onWritten();
      let fresh: Memories | null = null;
      let stale: string | undefined;
      try {
        fresh = await load();
      } catch (e) {
        stale = describeError(e);
      }
      if (saved.outcome === "duplicate") {
        setNotice({
          text: `Nothing was written: a very similar memory already exists as ${saved.id}.`,
          kept: stillHere() ? "What you typed is still in the panel." : undefined,
          warn: true,
          open: saved.id,
          stale,
        });
        return;
      }
      const landed = fresh?.memories.find((m) => m.id === saved.id) ?? null;
      const replacedAnother = saved.outcome === "replaced" && sent.id === null;
      const text = savedText(saved, sent.id);
      if (fresh && !landed) {
        setNotice({
          text: `${text} But no memory with id ${saved.id} came back when the list reloaded.`,
          warn: true,
        });
      } else {
        setNotice({ text, warn: replacedAnother, stale });
      }
      if (stillHere() && landed) {
        openPanel(landed.id, draftFrom(landed));
      } else if (stillHere() && !fresh) {
        const after = { ...sent, id: saved.id };
        openPanel(saved.id, after);
      }
    } finally {
      writing.current = false;
      setBusy(null);
    }
  };

  const remove = async (target: { id: string; title: string }) => {
    if (writing.current) return;
    if (selectedId !== target.id) {
      setConfirming(null);
      setNotice({
        text: `Nothing was deleted: the panel no longer shows “${target.title}”, the memory you confirmed.`,
        warn: true,
      });
      return;
    }
    const token = panelGeneration.current;
    writing.current = true;
    setBusy({ op: "delete", label: target.title });
    setPanelErr(null);
    setNotice(null);
    setLeaving(null);
    try {
      let gone;
      try {
        gone = await deleteMemory(target.id);
      } catch (e) {
        if (panelGeneration.current === token) setPanelErr(reason(e));
        else setNotice({ text: `“${target.title}” was not deleted: ${reason(e)}`, warn: true });
        return;
      }
      onWritten();
      let stale: string | undefined;
      try {
        await load();
      } catch (e) {
        stale = describeError(e);
      }
      if (panelGeneration.current === token) openPanel(null, null);
      setNotice({ text: `Deleted “${gone.title}”.`, warn: false, stale });
    } finally {
      writing.current = false;
      setBusy(null);
    }
  };

  const loadReach = async () => {
    if (!selected || writing.current) return;
    const id = selected.id;
    reachFor.current = id;
    setReaching(true);
    setReach(null);
    setReachError(null);
    try {
      const r = await getReach(id);
      if (reachFor.current !== id) return;
      if (r.error) setReachError(r.error);
      else setReach(r.reach ?? []);
    } catch (e) {
      if (reachFor.current !== id) return;
      setReachError(describeError(e));
    } finally {
      if (reachFor.current === id) setReaching(false);
    }
  };

  const unavailable = err ?? data?.unavailable ?? null;
  if (unavailable) {
    return (
      <div className="h-full flex flex-col items-center gap-3 pt-8">
        <ErrorBanner>{unavailable}</ErrorBanner>
        <button onClick={firstLoad} className="text-xs font-semibold text-accent underline">
          Try again
        </button>
      </div>
    );
  }
  if (!data) return <p className="text-sm text-muted p-4">Loading…</p>;

  const openId = notice?.open ?? null;
  const leavingTitle =
    leaving?.to === "pick" ? all.find((m) => m.id === leaving.id)?.title ?? leaving.id : null;
  const destination =
    leaving?.to === "pick"
      ? `open ${leaving.id}, “${clip(leavingTitle ?? "", 48)}”`
      : leaving?.to === "new"
        ? "start a new memory"
        : "close the panel";
  const lost = !draft
    ? ""
    : draft.id
      ? `your unsaved changes to “${clip(selected?.title ?? draft.id, 48)}”`
      : draft.text.trim()
        ? `the new ${draft.kind} you were writing, “${clip(draft.text, 48)}”,`
        : `the new ${draft.kind} you started`;

  return (
    <div className="flex flex-col gap-3.5 h-full min-h-0">
      <div className="flex items-center gap-3">
        <div className="flex gap-0.5 bg-sunken rounded-md p-0.5">
          {(["all", ...KINDS] as const).map((k) => (
            <button
              key={k}
              onClick={() => setKind(k)}
              className={`px-2.5 py-1 text-xs rounded ${k === kind ? "font-semibold text-accent bg-surface" : "text-muted"}`}
            >
              {k}
            </button>
          ))}
        </div>
        <select
          value={scope}
          onChange={(e) => setProject(e.target.value)}
          className="border border-line rounded-md px-2 py-1.5 text-xs bg-surface max-w-[220px]"
        >
          <option value="all">every scope</option>
          {scopes.map((s) => (
            <option key={s} value={s}>{s}</option>
          ))}
        </select>
        <input
          value={needle}
          onChange={(e) => setNeedle(e.target.value)}
          placeholder="Filter by text or id…"
          className="grow border border-line rounded-md px-3 py-2 text-sm bg-surface"
        />
        <span className="font-mono text-xs text-muted">
          {shown.length === all.length ? `${all.length}` : `${shown.length} of ${all.length}`}
        </span>
        <button
          onClick={() => request({ to: "new" })}
          disabled={locked}
          className="px-4 py-2 bg-accent text-white rounded-md text-sm font-semibold disabled:opacity-50 disabled:cursor-not-allowed"
        >
          New
        </button>
      </div>
      {busy && (
        <div className="rounded-md px-4 py-2 text-sm bg-sunken text-muted">
          {busy.op === "save"
            ? `Saving “${busy.label}”. Other memories, New and close are locked until the server answers; embedding can take tens of seconds when Ollama is busy.`
            : `Deleting “${busy.label}”. Other memories, New and close are locked until the server answers.`}
        </div>
      )}
      {notice && (
        <div
          className={`rounded-md px-4 py-2 text-sm flex items-start gap-3 ${notice.warn || notice.stale ? "bg-[#F6E4DC] text-warn" : "bg-accent-soft text-accent"}`}
        >
          <span className="grow">
            {notice.text}
            {notice.kept && ` ${notice.kept}`}
            {notice.stale && ` The list could not be reloaded, so it may be out of date: ${notice.stale}`}
          </span>
          {notice.stale && (
            <button onClick={reload} className="text-xs font-semibold underline shrink-0">
              Reload the list
            </button>
          )}
          {openId && (
            <button
              onClick={() => request({ to: "pick", id: openId })}
              disabled={locked}
              className="text-xs font-semibold underline shrink-0 disabled:opacity-50 disabled:cursor-not-allowed"
            >
              Open {openId}
            </button>
          )}
          <button onClick={() => setNotice(null)} className="text-xs shrink-0">
            dismiss
          </button>
        </div>
      )}
      <div className="flex gap-4 grow min-h-0">
        <div className="grow bg-surface border border-line rounded-lg flex flex-col min-h-0 overflow-auto">
          {all.length === 0 && (
            <p className="text-sm text-muted p-4">
              No memories yet — write one with <code className="font-mono bg-sunken px-1 rounded">br8n memory add</code> or the New button.
            </p>
          )}
          {all.length > 0 && shown.length === 0 && (
            <p className="text-sm text-muted p-4">No memory matches these filters.</p>
          )}
          {shown.map((m) => (
            <button
              key={m.id}
              onClick={() => request({ to: "pick", id: m.id })}
              disabled={locked}
              className={`w-full text-left px-3 py-2.5 border-t border-sunken flex items-center gap-2.5 min-w-0 disabled:cursor-not-allowed ${m.id === selectedId ? "bg-accent-soft" : ""} ${locked && m.id !== selectedId ? "opacity-50" : ""}`}
            >
              <KindChip kind={m.facts.kind} />
              <span className="text-[11px] text-muted shrink-0 w-[82px]">{day(m.facts.created)}</span>
              <span
                className="text-[11px] text-muted shrink-0 w-[120px] truncate font-mono"
                title={scopeOf(m)}
              >
                {scopeOf(m)}
              </span>
              <span className="text-xs grow truncate">{firstLine(m.text)}</span>
              <span
                className="font-mono text-[11px] text-muted shrink-0 w-[92px] truncate text-right"
                title={m.id}
              >
                {m.id}
              </span>
            </button>
          ))}
        </div>
        {draft && (
          <div className="w-[380px] shrink-0 bg-surface border border-line rounded-lg p-5 flex flex-col gap-3 overflow-auto">
            <div className="flex items-center gap-2">
              <KindChip kind={draft.kind} />
              <span className="text-[11px] font-semibold uppercase tracking-wider text-muted">
                {draft.id ? "Editing" : "New memory"}
              </span>
              {dirty && <span className="text-[11px] text-warn">unsaved</span>}
              <div className="grow" />
              <button
                onClick={() => request({ to: "close" })}
                disabled={locked}
                className="text-xs text-muted px-1.5 disabled:opacity-40 disabled:cursor-not-allowed"
              >
                close
              </button>
            </div>
            {leaving && (
              <div className="bg-[#F6E4DC] text-warn rounded-md px-3 py-2.5 flex flex-col gap-2">
                <span className="text-sm">
                  Discard {lost} and {destination}? Nothing is written until you press Save.
                </span>
                <div className="flex gap-2">
                  <button
                    onClick={() => {
                      if (!writing.current) go(leaving);
                    }}
                    disabled={locked}
                    className="text-xs font-semibold text-white bg-warn rounded-md px-2.5 py-1.5"
                  >
                    Discard
                  </button>
                  <button
                    onClick={() => setLeaving(null)}
                    className="text-xs font-semibold text-warn bg-surface border border-line rounded-md px-2.5 py-1.5"
                  >
                    Keep editing
                  </button>
                </div>
              </div>
            )}
            {selected && (
              <div className="font-serif font-semibold text-lg leading-snug">{selected.title}</div>
            )}
            {selected && (
              <dl className="text-[11px] grid grid-cols-[auto_1fr] gap-x-2 gap-y-1">
                <dt className="text-muted">Id</dt>
                <dd className="font-mono truncate" title={selected.id}>{selected.id}</dd>
                <dt className="text-muted">Created</dt>
                <dd>{new Date(selected.facts.created * 1000).toLocaleString()}</dd>
                <dt className="text-muted">Origin</dt>
                <dd>{selected.facts.origin}</dd>
                {selected.facts.session && (
                  <>
                    <dt className="text-muted">Session</dt>
                    <dd className="font-mono truncate" title={selected.facts.session}>
                      {selected.facts.session}
                    </dd>
                  </>
                )}
              </dl>
            )}
            <label className="flex flex-col gap-1">
              <span className="text-[11px] font-semibold uppercase tracking-wider text-muted">Text</span>
              <textarea
                value={draft.text}
                onChange={(e) => edit({ text: e.target.value })}
                readOnly={locked}
                rows={8}
                className="border border-line rounded-md px-2.5 py-2 text-sm bg-surface leading-relaxed resize-y read-only:opacity-60"
              />
              <span className="font-mono text-[10px] text-muted">{draft.text.trim().length} characters</span>
            </label>
            <div className="flex gap-2.5">
              <label className="flex flex-col gap-1 grow">
                <span className="text-[11px] font-semibold uppercase tracking-wider text-muted">Kind</span>
                <select
                  value={draft.kind}
                  onChange={(e) => edit({ kind: e.target.value })}
                  disabled={locked}
                  className="border border-line rounded-md px-2 py-1.5 text-sm bg-surface disabled:opacity-60"
                >
                  {KINDS.map((k) => (
                    <option key={k} value={k}>{k}</option>
                  ))}
                </select>
              </label>
              <label className="flex flex-col gap-1 w-[110px]">
                <span className="text-[11px] font-semibold uppercase tracking-wider text-muted">Confidence</span>
                <input
                  type="number"
                  min={0}
                  max={100}
                  step={1}
                  value={draft.confidence}
                  onChange={(e) => edit({ confidence: e.target.value })}
                  disabled={locked}
                  className="border border-line rounded-md px-2 py-1.5 text-sm bg-surface font-mono disabled:opacity-60"
                />
              </label>
            </div>
            <div className="flex flex-col gap-1">
              <span className="text-[11px] font-semibold uppercase tracking-wider text-muted">Scope</span>
              <div className="flex items-center gap-2.5">
                <label className="flex items-center gap-1.5 text-xs shrink-0">
                  <input
                    type="checkbox"
                    checked={draft.global}
                    onChange={(e) => edit({ global: e.target.checked })}
                    disabled={locked}
                  />
                  global
                </label>
                <input
                  value={draft.scope}
                  onChange={(e) => edit({ scope: e.target.value })}
                  disabled={draft.global || locked}
                  placeholder="/absolute/project/path"
                  className="grow border border-line rounded-md px-2 py-1.5 text-xs bg-surface font-mono disabled:opacity-45"
                />
              </div>
            </div>
            <div className="flex items-center gap-2.5">
              <button
                onClick={save}
                disabled={locked}
                className="px-4 py-2 bg-accent text-white rounded-md text-sm font-semibold disabled:opacity-50 disabled:cursor-not-allowed"
              >
                {busy?.op === "save" ? "Saving…" : "Save"}
              </button>
              {selected && (
                <button
                  onClick={loadReach}
                  disabled={reaching || locked}
                  className="text-xs font-semibold text-accent bg-accent-soft border border-line rounded-md px-2.5 py-1.5 disabled:opacity-50 disabled:cursor-not-allowed"
                >
                  {reaching ? "Reaching…" : "What would this pull in?"}
                </button>
              )}
              <div className="grow" />
              {selected && !confirming && (
                <button
                  onClick={() => setConfirming({ id: selected.id, title: selected.title })}
                  disabled={locked}
                  className="text-xs font-semibold text-warn bg-[#F6E4DC] rounded-md px-2.5 py-1.5 disabled:opacity-50 disabled:cursor-not-allowed"
                >
                  Delete
                </button>
              )}
            </div>
            {confirming && (
              <div className="bg-[#F6E4DC] text-warn rounded-md px-3 py-2.5 flex flex-col gap-2">
                <span className="text-sm">
                  Delete “{confirming.title}” ({confirming.id})? A memory exists nowhere else, so this cannot be undone.
                </span>
                <div className="flex gap-2">
                  <button
                    onClick={() => remove(confirming)}
                    disabled={locked}
                    className="text-xs font-semibold text-white bg-warn rounded-md px-2.5 py-1.5 disabled:opacity-50"
                  >
                    {busy?.op === "delete" ? "Deleting…" : "Delete it"}
                  </button>
                  <button
                    onClick={() => setConfirming(null)}
                    disabled={locked}
                    className="text-xs font-semibold text-warn bg-surface border border-line rounded-md px-2.5 py-1.5 disabled:opacity-50"
                  >
                    Keep it
                  </button>
                </div>
              </div>
            )}
            {panelErr && <ErrorBanner>{panelErr}</ErrorBanner>}
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
                    <span className="truncate" title={r.uri}>{r.title}</span>
                    <span className="text-muted shrink-0 font-mono text-xs">{r.relevance.toFixed(2)}</span>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
