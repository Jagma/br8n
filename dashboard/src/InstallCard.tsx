import { useEffect, useRef, useState } from "react";
import { connectAgent, describeError, EmbedTest, getInstall, InstallCheck, InstallState, testEmbedding } from "./api";
import { ChangeReport, Outcome, primaryButton, secondaryButton } from "./integrations/agents";

const CHECK_LABEL: Record<string, string> = {
  binary: "Installed binary",
  plugin: "Plugin files",
  marketplace: "Claude Code marketplace",
  registration: "Claude Code plugin",
  claude: "Claude Code",
  path: "br8n on PATH",
};

function Fix({ check, busy, first, onConnect }: { check: InstallCheck; busy: boolean; first: boolean; onConnect: (agent: "claude-code") => void }) {
  const [copied, setCopied] = useState(false);
  const fix = check.fix;
  if (!fix) return null;
  if (fix.kind === "connect" && !first) return <span className="text-xs text-muted">the same Connect fixes this</span>;
  if (fix.kind === "connect") {
    return (
      <button onClick={() => onConnect("claude-code")} disabled={busy} className={primaryButton}>
        {busy ? "Connecting…" : "Connect Claude Code"}
      </button>
    );
  }
  if (fix.kind === "manual") return <span className="text-xs text-muted">{fix.text}</span>;
  return (
    <span className="flex items-center gap-2 min-w-0">
      <code className="font-mono text-xs bg-sunken rounded px-2 py-1 truncate" title={fix.command}>{fix.command}</code>
      <button
        onClick={() => navigator.clipboard.writeText(fix.command).then(() => setCopied(true)).catch(() => setCopied(false))}
        className="text-xs font-semibold text-accent shrink-0"
      >
        {copied ? "copied" : "copy"}
      </button>
    </span>
  );
}

function EmbedLine({ install, active }: { install: InstallState; active: boolean }) {
  const [test, setTest] = useState<EmbedTest | null>(null);
  const [testing, setTesting] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const tested = useRef(false);
  const run = () => {
    setTesting(true);
    setErr(null);
    testEmbedding()
      .then(setTest)
      .catch((e) => setErr(e instanceof Error ? e.message : String(e)))
      .finally(() => setTesting(false));
  };
  useEffect(() => {
    if (!active || tested.current) return;
    tested.current = true;
    run();
  }, [active]);
  const where = `${install.embed.backend === "remote" ? "remote endpoint" : "Ollama"} ${install.embed.url ?? "(not set)"} · ${install.embed.model ?? "no model"}`;
  return (
    <div className="flex items-center gap-3 py-2 border-t border-sunken" data-testid="embed-line">
      <span className={`w-2 h-2 rounded-full shrink-0 ${test?.ok ? "bg-accent" : test || err ? "bg-warn" : "bg-line"}`} />
      <span className="text-sm font-semibold w-[190px] shrink-0">Embeddings</span>
      <span className="text-sm grow min-w-0">
        <span className="font-mono text-xs text-muted break-all">{where}</span>
        {testing && <span className="text-xs text-muted"> · testing…</span>}
        {!testing && test?.ok && <span className="text-xs text-accent"> · reachable, {test.dimensions} dimensions in {test.latency_ms} ms</span>}
        {!testing && test && !test.ok && <span className="text-xs text-warn"> · {test.error ?? "not reachable"}</span>}
        {!testing && err && <span className="text-xs text-warn"> · {err}</span>}
      </span>
      <button onClick={run} disabled={testing} className={secondaryButton}>Test again</button>
    </div>
  );
}

export default function InstallCard({ active, onOpenSetting }: { active: boolean; onOpenSetting: (key: string) => void }) {
  const [install, setInstall] = useState<InstallState | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const load = () =>
    getInstall()
      .then((v) => {
        setInstall(v);
        setErr(null);
      })
      .catch((e) => setErr(describeError(e)));
  useEffect(() => {
    if (active) load();
  }, [active]);
  const connect = async (agent: "claude-code") => {
    if (connecting) return;
    setConnecting(true);
    setOutcome(null);
    try {
      const r = await connectAgent(agent);
      setOutcome(r.kind === "done" ? { action: "connect", change: r.outcome.change } : { action: "connect", error: r.error, status: r.status });
    } catch (e) {
      setOutcome({ action: "connect", error: e instanceof Error ? e.message : String(e) });
    } finally {
      setConnecting(false);
      load();
    }
  };
  if (!install) {
    return err ? (
      <div className="bg-surface border border-line rounded-lg px-4 py-3 text-sm text-warn">Install checks unavailable: {err}</div>
    ) : null;
  }
  const failed = install.checks.filter((c) => !c.ok);
  const firstConnect = failed.find((c) => c.fix?.kind === "connect")?.name;
  const errors = install.config.errors;
  const problems = failed.length + errors.length;
  return (
    <section className="bg-surface border border-line rounded-lg px-4 py-3 flex flex-col" data-testid="install-card">
      <div className="flex items-center gap-3 pb-2">
        <span className="text-[11px] font-semibold uppercase tracking-wider text-muted">Install</span>
        <span className="font-mono text-xs text-muted truncate grow" title={install.root}>{install.root}</span>
        <span className={`text-xs font-semibold ${problems ? "text-warn" : "text-accent"}`} data-testid="install-summary">
          {problems === 0 ? `all ${install.checks.length} checks pass` : `${problems} problem${problems === 1 ? "" : "s"}`}
        </span>
        <button onClick={load} className="text-xs font-semibold text-accent">recheck</button>
      </div>
      {failed.map((c) => (
        <div key={c.name} className="flex items-center gap-3 py-2 border-t border-sunken" data-check={c.name}>
          <span className="w-2 h-2 rounded-full bg-warn shrink-0" />
          <span className="text-sm font-semibold w-[190px] shrink-0">{CHECK_LABEL[c.name] ?? c.name}</span>
          <span className="text-sm text-muted grow min-w-0 break-words">{c.detail}</span>
          <span className="shrink-0 max-w-[45%]">
            <Fix check={c} busy={connecting} first={c.name === firstConnect} onConnect={connect} />
          </span>
        </div>
      ))}
      {failed.length > 0 && (
        <span className="text-xs text-muted pb-2">
          The binary, plugin and PATH checks describe what <code className="font-mono">br8n install</code> sets up; a dashboard run from a build that is not installed fails them by design.
        </span>
      )}
      {outcome && (
        <div className="pb-2">
          <ChangeReport outcome={outcome} name="Claude Code" />
        </div>
      )}
      {errors.map((e) => (
        <div key={`${e.path}:${e.message}`} className="flex items-center gap-3 py-2 border-t border-sunken" data-config-error={e.path}>
          <span className="w-2 h-2 rounded-full bg-warn shrink-0" />
          <span className="text-sm font-semibold w-[190px] shrink-0">config.toml{e.line !== null ? ` line ${e.line}` : ""}</span>
          <span className="text-sm grow min-w-0">
            <span className="font-mono text-xs">{e.path || "syntax"}</span> <span className="text-warn">{e.message}</span>
          </span>
          <button onClick={() => onOpenSetting(e.path)} className={secondaryButton}>Open in Settings</button>
        </div>
      ))}
      <EmbedLine install={install} active={active} />
    </section>
  );
}
