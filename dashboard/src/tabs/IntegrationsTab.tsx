import { useEffect, useState } from "react";
import { Agent, ConfigState, getConfig, Stats } from "../api";
import { ErrorBanner } from "../components";
import { AGENT_HINTS, AgentCard, secondaryButton, useAgentActions, useAgents } from "../integrations/agents";

function Snippet({ label, text }: { label: string; text: string }) {
  const [copied, setCopied] = useState<"yes" | "no" | null>(null);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied("yes");
    } catch {
      setCopied("no");
    }
  };
  return (
    <div className="flex flex-col gap-1.5 min-w-0 flex-1" data-snippet={label}>
      <div className="flex items-center gap-2">
        <span className="text-[11px] font-semibold uppercase tracking-wider text-muted grow">{label}</span>
        {copied === "yes" && <span className="text-xs text-accent">copied</span>}
        {copied === "no" && <span className="text-xs text-warn">the browser refused; select the text and copy it</span>}
        <button onClick={copy} className={secondaryButton}>Copy</button>
      </div>
      <pre className="font-mono text-xs bg-sunken rounded-md p-3 overflow-auto whitespace-pre select-all">{text}</pre>
    </div>
  );
}

function SessionsLine({ agent, config, stats, onOpenSources }: { agent: Agent; config: ConfigState | null; stats: Stats | null; onOpenSources: () => void }) {
  if (!agent.capabilities.transcripts || !config) return null;
  const all = config.effective.index_transcripts;
  const on = agent.id === "codex" ? all && config.effective.index_codex_sessions : all;
  const key = agent.id === "codex" && all ? "index_codex_sessions" : "index_transcripts";
  const count = stats?.sessions_by_agent?.[agent.id];
  return (
    <div className="flex items-center gap-2 text-sm" data-testid="sessions-line">
      <span className={on ? "text-accent" : "text-muted"}>
        {on ? "Sessions are indexed" : "Sessions are not indexed"}
        {on && count !== undefined ? ` (${count} in the index)` : ""}
      </span>
      <span className="font-mono text-[11px] text-muted">{key} = {String(key === "index_transcripts" ? all : config.effective.index_codex_sessions)}</span>
      <button onClick={onOpenSources} className="text-xs font-semibold text-accent underline">Settings › Sources</button>
    </div>
  );
}

export default function IntegrationsTab({
  stats,
  active,
  onOpenSources,
}: {
  stats: Stats | null;
  active: boolean;
  onOpenSources: () => void;
}) {
  const { agents, err, reload } = useAgents();
  const [config, setConfig] = useState<ConfigState | null>(null);
  const actions = useAgentActions(reload);
  useEffect(() => {
    if (!active) return;
    getConfig().then(setConfig).catch(() => setConfig(null));
  }, [active]);
  if (err && !agents) {
    return (
      <div className="h-full flex flex-col items-center gap-3 pt-8">
        <ErrorBanner>{err}</ErrorBanner>
        <button onClick={reload} className="text-xs font-semibold text-accent underline">Try again</button>
      </div>
    );
  }
  if (!agents) return <p className="text-sm text-muted p-4">Looking for agents…</p>;
  const detected = agents.agents.filter((a) => a.detected.installed || a.status.state !== "not_connected");
  const missing = agents.agents.filter((a) => !detected.includes(a));
  return (
    <div className="h-full overflow-auto" data-testid="integrations">
      <div className="max-w-[1100px] mx-auto flex flex-col gap-4 pb-6">
        <div className="flex items-baseline gap-3">
          <h2 className="font-serif font-semibold text-xl">Integrations</h2>
          <span className="text-xs text-muted grow">
            Connecting writes only br8n's own entry into each agent's config, keeps the original beside it as .br8n-bak, and never rewrites a file it cannot parse.
          </span>
          <button onClick={reload} className={secondaryButton}>Refresh</button>
        </div>
        {err && <ErrorBanner>{err}</ErrorBanner>}
        {detected.length === 0 && (
          <div className="bg-surface border border-line rounded-lg p-5 text-sm text-muted">
            No coding agent was found on this machine. Any MCP client can still use br8n with the snippet below.
          </div>
        )}
        <div className="grid grid-cols-2 gap-4 items-start">
          {detected.map((a) => (
            <AgentCard key={a.id} agent={a} actions={actions}>
              <SessionsLine agent={a} config={config} stats={stats} onOpenSources={onOpenSources} />
            </AgentCard>
          ))}
        </div>
        {missing.length > 0 && (
          <details className="bg-surface border border-line rounded-lg px-5 py-3" data-testid="not-installed">
            <summary className="text-sm font-semibold cursor-pointer">
              Not installed <span className="font-mono text-xs text-muted">{missing.length}</span>
            </summary>
            <div className="flex flex-col divide-y divide-sunken pt-2">
              {missing.map((a) => (
                <div key={a.id} className="flex items-center gap-3 py-2" data-missing={a.id}>
                  <span className="text-sm font-semibold w-[130px] shrink-0">{a.name}</span>
                  <span className="text-sm text-muted grow">br8n would give it {AGENT_HINTS[a.id].gives}.</span>
                  <a href={AGENT_HINTS[a.id].url} target="_blank" rel="noreferrer" className="text-xs font-semibold text-accent underline shrink-0">
                    Get {a.name}
                  </a>
                </div>
              ))}
            </div>
          </details>
        )}
        <section className="bg-surface border border-line rounded-lg p-5 flex flex-col gap-3" data-testid="other-mcp">
          <div className="flex flex-col gap-1">
            <h3 className="font-serif font-semibold text-lg">Any other MCP client</h3>
            <span className="text-sm text-muted">
              Paste one of these into the client's MCP config. Both run the installed br8n binary by its absolute path.
            </span>
          </div>
          <div className="flex gap-4">
            <Snippet label="JSON (mcpServers)" text={agents.snippets.mcp_json} />
            <Snippet label="Codex TOML" text={agents.snippets.codex_toml} />
          </div>
        </section>
      </div>
    </div>
  );
}
