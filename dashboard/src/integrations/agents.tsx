import { ReactNode, useCallback, useEffect, useRef, useState } from "react";
import {
  Agent,
  AgentCapabilities,
  AgentChange,
  AgentId,
  Agents,
  connectAgent,
  describeError,
  disconnectAgent,
  getAgents,
} from "../api";

export const CAPABILITIES: [keyof AgentCapabilities, string][] = [
  ["mcp", "MCP tools"],
  ["prompt_hook", "context on every prompt"],
  ["session_hook", "session start"],
  ["transcripts", "sessions indexed"],
  ["instructions", "instructions"],
];

export const AGENT_HINTS: Record<AgentId, { gives: string; url: string }> = {
  "claude-code": {
    gives: "the br8n plugin: MCP tools, context on every prompt and at session start, and its sessions indexed",
    url: "https://claude.com/claude-code",
  },
  codex: { gives: "MCP tools, context on every prompt, and its sessions indexed", url: "https://github.com/openai/codex" },
  "claude-desktop": { gives: "br8n's MCP tools in chat", url: "https://claude.ai/download" },
  cursor: { gives: "br8n's MCP tools in the editor's agent", url: "https://cursor.com" },
  gemini: { gives: "MCP tools and context on every prompt", url: "https://github.com/google-gemini/gemini-cli" },
};

export const primaryButton =
  "px-3.5 py-1.5 bg-accent text-white rounded-md text-xs font-semibold disabled:opacity-50 disabled:cursor-not-allowed";
export const secondaryButton =
  "px-3 py-1.5 text-xs font-semibold text-accent bg-accent-soft border border-line rounded-md disabled:opacity-50 disabled:cursor-not-allowed";
const dangerButton =
  "px-3 py-1.5 text-xs font-semibold text-warn bg-[#F6E4DC] rounded-md disabled:opacity-50 disabled:cursor-not-allowed";

type Action = "connect" | "disconnect";

export type Outcome = { action: Action; change?: AgentChange; error?: string; status?: number };

export type AgentActions = {
  busy: Partial<Record<AgentId, Action>>;
  last: Partial<Record<AgentId, Outcome>>;
  act: (id: AgentId, action: Action, instructions?: boolean) => Promise<void>;
};

export function useAgents() {
  const [agents, setAgents] = useState<Agents | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const load = useCallback(async () => {
    try {
      setAgents(await getAgents());
      setErr(null);
    } catch (e) {
      setErr(describeError(e));
    }
  }, []);
  useEffect(() => {
    load();
  }, [load]);
  return { agents, err, reload: load };
}

export function useAgentActions(onChanged: () => void): AgentActions {
  const [busy, setBusy] = useState<Partial<Record<AgentId, Action>>>({});
  const [last, setLast] = useState<Partial<Record<AgentId, Outcome>>>({});
  const inFlight = useRef(new Set<AgentId>());
  const changed = useRef(onChanged);
  changed.current = onChanged;
  const act = async (id: AgentId, action: Action, instructions?: boolean) => {
    if (inFlight.current.has(id)) return;
    inFlight.current.add(id);
    setBusy((b) => ({ ...b, [id]: action }));
    setLast((l) => ({ ...l, [id]: undefined }));
    try {
      const r = action === "connect" ? await connectAgent(id, instructions) : await disconnectAgent(id);
      const outcome: Outcome =
        r.kind === "done" ? { action, change: r.outcome.change } : { action, error: r.error, status: r.status };
      setLast((l) => ({ ...l, [id]: outcome }));
    } catch (e) {
      setLast((l) => ({ ...l, [id]: { action, error: e instanceof Error ? e.message : String(e) } }));
    } finally {
      inFlight.current.delete(id);
      setBusy((b) => ({ ...b, [id]: undefined }));
      await changed.current();
    }
  };
  return { busy, last, act };
}

export function StatusPill({ agent }: { agent: Agent }) {
  const s = agent.status.state;
  const look =
    s === "connected"
      ? "bg-accent-soft text-accent"
      : s === "not_connected"
        ? "bg-sunken text-muted"
        : "bg-[#F6E4DC] text-warn";
  const label = s === "connected" ? "connected" : s === "not_connected" ? "not connected" : s === "stale" ? "needs repair" : "broken";
  return (
    <span className={`text-[11px] font-semibold uppercase tracking-wider px-2 py-0.5 rounded whitespace-nowrap shrink-0 ${look}`} data-testid="agent-status" data-state={s}>
      {label}
    </span>
  );
}

function Paths({ label, paths }: { label: string; paths: string[] }) {
  if (paths.length === 0) return null;
  return (
    <div className="flex flex-col gap-0.5">
      <span className="text-[11px] font-semibold uppercase tracking-wider opacity-80">{label}</span>
      {paths.map((p) => (
        <span key={p} className="font-mono text-xs break-all">{p}</span>
      ))}
    </div>
  );
}

export function ChangeReport({ outcome, name }: { outcome: Outcome; name: string }) {
  if (outcome.error) {
    return (
      <div className="rounded-md px-3 py-2 text-sm bg-[#F6E4DC] text-warn flex flex-col gap-1" role="alert" data-testid="agent-error">
        <span>{outcome.error}</span>
        {outcome.status === 409 && (
          <span className="text-xs">Nothing was written: the file is exactly as it was. Fix it by hand, then try again.</span>
        )}
      </div>
    );
  }
  const c = outcome.change ?? { files: [], backups: [], notes: [] };
  const nothing = c.files.length === 0 && c.notes.length === 0;
  const verb = outcome.action === "connect" ? "Connected" : "Disconnected";
  return (
    <div className="rounded-md px-3 py-2 text-sm bg-accent-soft text-accent flex flex-col gap-2" role="status" data-testid="agent-change">
      <span className="font-semibold">
        {nothing
          ? outcome.action === "connect"
            ? `${name} was already connected; nothing changed.`
            : `${name} had nothing of br8n's to remove.`
          : `${verb} ${name}.`}
      </span>
      <Paths label="Files written" paths={c.files} />
      <Paths label="Originals kept as .br8n-bak" paths={c.backups} />
      {c.notes.length > 0 && (
        <ul className="flex flex-col gap-0.5 list-disc pl-4">
          {c.notes.map((n) => (
            <li key={n}>{n}</li>
          ))}
        </ul>
      )}
    </div>
  );
}

export function Capabilities({ agent }: { agent: Agent }) {
  return (
    <div className="flex flex-wrap gap-1.5">
      {CAPABILITIES.filter(([k]) => agent.capabilities[k]).map(([k, label]) => (
        <span key={k} className="text-[11px] px-2 py-0.5 rounded-full border border-line text-muted" data-capability={k}>
          {label}
        </span>
      ))}
    </div>
  );
}

export function AgentCard({
  agent,
  actions,
  highlight,
  children,
}: {
  agent: Agent;
  actions: AgentActions;
  highlight?: string;
  children?: ReactNode;
}) {
  const [instructions, setInstructions] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const running = actions.busy[agent.id];
  const locked = running !== undefined;
  const outcome = actions.last[agent.id];
  const state = agent.status.state;
  const repair = state === "stale" || state === "broken";
  const connect = () => {
    setConfirming(false);
    actions.act(agent.id, "connect", agent.capabilities.instructions ? instructions : undefined);
  };
  return (
    <section
      className={`bg-surface border rounded-lg p-5 flex flex-col gap-3 ${highlight ? "border-accent ring-2 ring-accent-soft" : "border-line"}`}
      data-agent={agent.id}
      aria-busy={locked}
    >
      <div className="flex items-center gap-3">
        <h3 className="font-serif font-semibold text-lg whitespace-nowrap">{agent.name}</h3>
        {agent.detected.version && <span className="font-mono text-xs text-muted truncate min-w-0" title={agent.detected.version}>{agent.detected.version}</span>}
        {highlight && <span className="text-[11px] font-semibold text-accent shrink-0">{highlight}</span>}
        <div className="grow" />
        <StatusPill agent={agent} />
      </div>
      {agent.status.reason && <span className="text-xs text-warn">{agent.status.reason}</span>}
      <Capabilities agent={agent} />
      {agent.detected.config_path && (
        <span className="text-xs text-muted">
          Config <span className="font-mono break-all">{agent.detected.config_path}</span>
        </span>
      )}
      {children}
      {agent.capabilities.instructions && state !== "connected" && (
        <label className="flex items-start gap-2 text-sm">
          <input
            type="checkbox"
            checked={instructions}
            onChange={(e) => setInstructions(e.target.checked)}
            disabled={locked}
            aria-label="Also add guidance to AGENTS.md"
            className="mt-1 accent-[#0E6B58]"
          />
          <span className="flex flex-col">
            <span>Also add guidance to AGENTS.md</span>
            <span className="text-xs text-muted">
              Adds a marked block telling {agent.name} when to use br8n's tools. The global AGENTS.md is read in every repository, so this is off unless you want it.
            </span>
          </span>
        </label>
      )}
      {agent.instructions && <span className="text-xs text-muted">AGENTS.md carries br8n's guidance block.</span>}
      {confirming ? (
        <div className="flex items-center gap-2 rounded-md bg-[#F6E4DC] px-3 py-2">
          <span className="text-sm text-warn grow">Remove br8n from {agent.name}? Only br8n's own entries are removed; everything else in the file stays.</span>
          <button onClick={() => { setConfirming(false); actions.act(agent.id, "disconnect"); }} disabled={locked} className={dangerButton}>
            Disconnect
          </button>
          <button onClick={() => setConfirming(false)} disabled={locked} className="text-xs font-semibold text-muted px-2">
            Cancel
          </button>
        </div>
      ) : (
        <div className="flex items-center gap-2">
          {state === "not_connected" && (
            <button onClick={connect} disabled={locked} className={primaryButton}>
              {running === "connect" ? "Connecting…" : "Connect"}
            </button>
          )}
          {repair && (
            <button onClick={connect} disabled={locked} className={primaryButton}>
              {running === "connect" ? "Repairing…" : "Repair"}
            </button>
          )}
          {state !== "not_connected" && (
            <button onClick={() => setConfirming(true)} disabled={locked} className={secondaryButton}>
              {running === "disconnect" ? "Disconnecting…" : "Disconnect"}
            </button>
          )}
          {locked && <span className="text-xs text-muted">Writing {agent.name}'s config. The card is locked until the server answers.</span>}
        </div>
      )}
      {outcome && <ChangeReport outcome={outcome} name={agent.name} />}
    </section>
  );
}
