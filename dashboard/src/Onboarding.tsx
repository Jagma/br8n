import { useEffect, useRef, useState } from "react";
import { ConfigState, describeError, getConfig, getGraph, saveConfig } from "./api";
import { ErrorBanner } from "./components";
import { AgentCard, primaryButton, secondaryButton, useAgentActions, useAgents } from "./integrations/agents";
import { IndexStatus, useIndexRunner } from "./settings/IndexPanel";
import { sourceProblem } from "./settings/sections";

const STEPS = ["Pick folders", "Connect agents", "Index"] as const;
const GENERIC_QUERY = "what did I work on recently";
const NOT_NOTES = new Set(["transcript", "memory", "project"]);

export async function suggestQuery(): Promise<string> {
  try {
    const graph = await getGraph();
    const notes = graph.nodes.filter((n) => !NOT_NOTES.has(n.source_type) && n.title.trim() !== "");
    notes.sort((a, b) => b.chunks - a.chunks || a.title.localeCompare(b.title));
    return notes[0]?.title.trim() ?? GENERIC_QUERY;
  } catch {
    return GENERIC_QUERY;
  }
}

function Steps({ step }: { step: number }) {
  return (
    <ol className="flex items-center gap-3" data-testid="setup-steps">
      {STEPS.map((s, i) => (
        <li key={s} className="flex items-center gap-2" aria-current={i === step ? "step" : undefined}>
          <span
            className={`w-6 h-6 rounded-full flex items-center justify-center text-xs font-semibold ${i < step ? "bg-accent text-white" : i === step ? "bg-accent-soft text-accent border border-accent" : "bg-sunken text-muted"}`}
          >
            {i + 1}
          </span>
          <span className={`text-sm ${i === step ? "font-semibold" : "text-muted"}`}>{s}</span>
          {i < STEPS.length - 1 && <span className="w-10 border-t border-line" />}
        </li>
      ))}
    </ol>
  );
}

function Folders({ onNext }: { onNext: () => void }) {
  const [state, setState] = useState<ConfigState | null>(null);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  const [list, setList] = useState<string[]>([]);
  const [candidate, setCandidate] = useState("");
  const [verdict, setVerdict] = useState<{ candidate: string; problem: string | null } | null>(null);
  const [saving, setSaving] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const load = async () => {
    try {
      const next = await getConfig();
      setState(next);
      setList(next.effective.sources);
      setLoadErr(null);
    } catch (e) {
      setLoadErr(describeError(e));
    }
  };
  useEffect(() => {
    load();
  }, []);
  const trimmed = candidate.trim();
  const listKey = list.join("\n");
  useEffect(() => {
    if (!state || trimmed === "") return;
    let current = true;
    const timer = setTimeout(() => {
      sourceProblem(state, {})(trimmed, list)
        .then((problem) => current && setVerdict({ candidate: trimmed, problem }))
        .catch(() => current && setVerdict({ candidate: trimmed, problem: null }));
    }, 250);
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [trimmed, listKey, state]);
  if (loadErr) return <ErrorBanner>{loadErr}</ErrorBanner>;
  if (!state) return <p className="text-sm text-muted">Reading config.toml…</p>;
  const checking = trimmed !== "" && verdict?.candidate !== trimmed;
  const problem = !checking && verdict?.candidate === trimmed ? verdict.problem : null;
  const duplicate = list.includes(trimmed);
  const add = () => {
    if (trimmed === "" || checking || problem || duplicate || saving) return;
    setList([...list, trimmed]);
    setCandidate("");
  };
  const save = async () => {
    if (saving || list.length === 0) return;
    setSaving(true);
    setNotice(null);
    try {
      const r = await saveConfig(state.etag, { set: { sources: list } });
      if (r.kind === "saved") {
        setState(r.state);
        onNext();
      } else if (r.kind === "conflict") {
        setNotice("config.toml changed on disk while this page was open, so nothing was saved. It has been read again; check the list and save once more.");
        const next = await getConfig();
        setState(next);
      } else {
        setNotice(`Nothing was saved. ${r.error}`);
      }
    } catch (e) {
      setNotice(`Nothing was saved: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setSaving(false);
    }
  };
  return (
    <div className="flex flex-col gap-4">
      <p className="text-sm text-muted">
        br8n indexes the folders you name here: notes, docs, PDFs. Folders are walked recursively and a single file works too. They are saved to{" "}
        <code className="font-mono text-xs">{state.path}</code> as <code className="font-mono text-xs">sources</code>.
      </p>
      <div className="flex flex-col border border-line rounded-md divide-y divide-sunken" data-testid="setup-folders">
        {list.length === 0 && <span className="text-sm text-muted px-3 py-2">no folders yet</span>}
        {list.map((item) => (
          <div key={item} className="flex items-center gap-2 px-3 py-1.5" data-item={item}>
            <span className="font-mono text-xs grow truncate">{item}</span>
            <button onClick={() => setList(list.filter((x) => x !== item))} disabled={saving} className="text-xs text-muted hover:text-warn">
              remove
            </button>
          </div>
        ))}
      </div>
      <div className="flex gap-2">
        <input
          value={candidate}
          onChange={(e) => setCandidate(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && add()}
          placeholder="/absolute/path/to/notes or ~/notes"
          aria-label="folder to index"
          disabled={saving}
          className="border border-line rounded-md px-2.5 py-1.5 text-sm bg-surface font-mono grow"
        />
        <button onClick={add} disabled={saving || trimmed === "" || checking || !!problem || duplicate} className={secondaryButton}>
          Add folder
        </button>
      </div>
      {checking && <span className="text-xs text-muted">checking…</span>}
      {duplicate && <span className="text-xs text-warn">already in the list</span>}
      {problem && <span className="text-xs text-warn" role="alert">{problem}</span>}
      {notice && <ErrorBanner>{notice}</ErrorBanner>}
      <div className="flex items-center gap-3">
        <button onClick={save} disabled={saving || list.length === 0} className={primaryButton}>
          {saving ? "Saving…" : "Save and continue"}
        </button>
        <button onClick={onNext} disabled={saving} className="text-xs font-semibold text-muted underline">
          continue without saving
        </button>
      </div>
    </div>
  );
}

function Connect({ onNext }: { onNext: () => void }) {
  const { agents, err, reload } = useAgents();
  const actions = useAgentActions(reload);
  const busy = Object.values(actions.busy).some((b) => b !== undefined);
  if (err && !agents) return <ErrorBanner>{err}</ErrorBanner>;
  if (!agents) return <p className="text-sm text-muted">Looking for coding agents on this machine…</p>;
  const detected = agents.agents.filter((a) => a.detected.installed || a.status.state !== "not_connected");
  return (
    <div className="flex flex-col gap-4">
      <p className="text-sm text-muted">
        Connected agents can search br8n through MCP tools, and some get relevant notes added to every prompt. Each connection writes only br8n's own entry and keeps the original file as .br8n-bak. The Integrations tab can change this later.
      </p>
      {detected.length === 0 && <p className="text-sm">No coding agent was found on this machine. The Integrations tab has a snippet for any MCP client.</p>}
      <div className="grid grid-cols-2 gap-4 items-start">
        {detected.map((a) => (
          <AgentCard key={a.id} agent={a} actions={actions} highlight={a.id === "claude-code" ? "recommended" : undefined} />
        ))}
      </div>
      <div>
        <button onClick={onNext} disabled={busy} className={primaryButton}>Continue</button>
      </div>
    </div>
  );
}

function Index({ onDone }: { onDone: (query: string) => void }) {
  const [finishing, setFinishing] = useState(false);
  const finished = useRef(false);
  const runner = useIndexRunner(() => {});
  const started = useRef(false);
  useEffect(() => {
    if (started.current) return;
    started.current = true;
    runner.start(false);
  }, []);
  useEffect(() => {
    if (runner.state.phase !== "done" || finished.current) return;
    finished.current = true;
    setFinishing(true);
    suggestQuery().then(onDone);
  }, [runner.state.phase]);
  return (
    <div className="flex flex-col gap-4">
      <p className="text-sm text-muted">
        br8n reads every file, splits it into chunks and embeds each one with the local model. Only new and changed files are read on later runs.
      </p>
      <IndexStatus runner={runner} />
      {finishing && <span className="text-sm text-muted">Picking a first search from your notes…</span>}
      {runner.state.phase === "failed" && (
        <div>
          <button onClick={() => runner.start(false)} className={primaryButton}>Try again</button>
        </div>
      )}
    </div>
  );
}

export default function Onboarding({ onSkip, onDone }: { onSkip: () => void; onDone: (query: string) => void }) {
  const [step, setStep] = useState(0);
  return (
    <div className="h-full overflow-auto" data-testid="onboarding">
      <div className="max-w-[900px] mx-auto flex flex-col gap-5 py-4">
        <div className="flex items-center gap-4">
          <div className="flex flex-col grow">
            <h1 className="font-serif font-semibold text-2xl">Set up br8n</h1>
            <span className="text-sm text-muted">Nothing is indexed yet. Three steps, and every one can be skipped.</span>
          </div>
          <button onClick={onSkip} className="text-xs font-semibold text-muted underline">Skip setup</button>
        </div>
        <Steps step={step} />
        <section className="bg-surface border border-line rounded-lg p-6 flex flex-col gap-4">
          <h2 className="font-serif font-semibold text-xl">{STEPS[step]}</h2>
          {step === 0 && <Folders onNext={() => setStep(1)} />}
          {step === 1 && <Connect onNext={() => setStep(2)} />}
          {step === 2 && <Index onDone={onDone} />}
        </section>
      </div>
    </div>
  );
}
