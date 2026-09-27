import { useEffect, useRef, useState } from "react";
import { getIndexRun, getProgress, Progress, startIndex } from "../api";

export type IndexState =
  | { phase: "idle" }
  | { phase: "starting"; reindex: boolean }
  | { phase: "running"; reindex: boolean; pid: number; progress: Progress }
  | { phase: "done"; reindex: boolean }
  | { phase: "failed"; reason: string };

export type IndexRunner = { state: IndexState; start: (reindex: boolean) => void };

function reason(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export function useIndexRunner(onFinished: () => void): IndexRunner {
  const [state, setState] = useState<IndexState>({ phase: "idle" });
  const finished = useRef(onFinished);
  finished.current = onFinished;
  const pid = state.phase === "running" ? state.pid : null;

  useEffect(() => {
    if (pid === null) return;
    let stopped = false;
    const tick = async () => {
      try {
        const [progress, { run }] = await Promise.all([getProgress(), getIndexRun()]);
        if (stopped) return;
        if (run && run.pid === pid && run.finished) {
          stopped = true;
          if (run.ok) setState({ phase: "done", reindex: run.reindex });
          else
            setState({
              phase: "failed",
              reason: `The index failed: br8n index exited${run.code === null ? " on a signal" : ` with code ${run.code}`}; its output is in ${run.log}`,
            });
          finished.current();
          return;
        }
        setState((s) => (s.phase === "running" && s.pid === pid ? { ...s, progress } : s));
      } catch {
        return;
      }
    };
    const timer = setInterval(tick, 700);
    tick();
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  }, [pid]);

  const start = (reindex: boolean) => {
    if (state.phase === "starting" || state.phase === "running") return;
    setState({ phase: "starting", reindex });
    startIndex(reindex)
      .then((r) => {
        if (r.kind === "busy") setState({ phase: "failed", reason: `Not started: ${r.error}. Wait for it to finish, then try again.` });
        else setState({ phase: "running", reindex: r.reindex, pid: r.pid, progress: { idle: true } });
      })
      .catch((e) => setState({ phase: "failed", reason: `Not started: ${reason(e)}` }));
  };

  return { state, start };
}

export function IndexStatus({ runner }: { runner: IndexRunner }) {
  const s = runner.state;
  if (s.phase === "idle") return null;
  if (s.phase === "starting") return <div className="text-sm text-muted">Starting {s.reindex ? "a full re-index" : "an index"}…</div>;
  if (s.phase === "running") {
    const p = s.progress;
    return (
      <div className="flex items-center gap-3" data-testid="index-progress">
        <span className="text-[11px] font-semibold uppercase tracking-wider text-muted shrink-0">
          {s.reindex ? "Re-indexing" : "Indexing"}
        </span>
        <div className="grow h-2 bg-sunken rounded overflow-hidden">
          <div className="h-full bg-accent rounded transition-all" style={{ width: `${p.idle ? 0 : (p.pct ?? 0)}%` }} />
        </div>
        <span className="font-mono text-xs shrink-0">
          {p.idle ? "preparing…" : `${(p.pct ?? 0).toFixed(1)}% · ${p.docs_done ?? 0}/${p.docs_total ?? 0} docs`}
        </span>
      </div>
    );
  }
  if (s.phase === "done")
    return (
      <div className="rounded-md px-3 py-2 text-sm bg-accent-soft text-accent" role="status">
        {s.reindex ? "Full re-index finished." : "Index finished."} The header counts and the graph are refreshed.
      </div>
    );
  return (
    <div className="rounded-md px-3 py-2 text-sm bg-[#F6E4DC] text-warn" role="alert">
      {s.reason}
    </div>
  );
}

export function busy(runner: IndexRunner): boolean {
  return runner.state.phase === "starting" || runner.state.phase === "running";
}
