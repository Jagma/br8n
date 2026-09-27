import { useEffect, useState } from "react";
import { BarChart, Bar, XAxis, YAxis, LabelList, ResponsiveContainer, CartesianGrid } from "recharts";
import { AGENT_NAME, getProgress, Stats, Progress, PALETTE } from "../api";
import { ErrorBanner } from "../components";
import VersionCard from "../VersionCard";
import InstallCard from "../InstallCard";

function Tile({ label, value, sub }: { label: string; value: string; sub: string }) {
  return (
    <div className="flex-1 bg-surface border border-line rounded-lg px-4 py-4 flex flex-col gap-1">
      <span className="text-[11px] font-semibold uppercase tracking-wider text-muted">{label}</span>
      <span className="font-mono text-2xl">{value}</span>
      <span className="text-xs text-muted">{sub}</span>
    </div>
  );
}

function RunBench() {
  return <p className="text-muted text-sm">Run <code className="font-mono bg-sunken px-1 rounded">br8n bench</code> to populate this chart.</p>;
}

/// `stats` is owned by `App`, which already fetches it for the header. Fetching
/// it here as well meant two `/api/stats` calls — two store opens — on every
/// page load for one payload, and two places deciding what a failed load looks
/// like. `err` is App's fetch failure, passed down.
export default function HealthTab({
  stats,
  err,
  active,
  onOpenSetting,
}: {
  stats: Stats | null;
  err: string | null;
  active: boolean;
  onOpenSetting: (key: string) => void;
}) {
  const [prog, setProg] = useState<Progress>({ idle: true });
  useEffect(() => {
    // This tab stays mounted once visited, so the poll has to stop itself when
    // the tab is not on screen — otherwise first visit means polling for the
    // life of the page.
    if (!active) return;
    // The 2s progress poll is secondary and self-healing (a failed tick just
    // tries again next cycle), so it stays silent on a single miss; the stats
    // load, by contrast, must be able to say it failed rather than leave this
    // tab reading "Loading…" forever with no way to tell failure from slow.
    const t = setInterval(() => getProgress().then(setProg).catch(() => {}), 2000);
    return () => clearInterval(t);
  }, [active]);
  if (err) return <div className="m-4"><ErrorBanner>{err}</ErrorBanner></div>;
  if (!stats) return <p className="text-muted p-4">Loading…</p>;
  const agents = Object.entries(stats.sessions_by_agent ?? {}).filter(([, v]) => v > 0);
  const byAgent = agents.length > 1 ? ` (${agents.map(([k, v]) => `${v} ${AGENT_NAME[k] ?? k}`).join(", ")})` : "";
  const src = Object.entries(stats.by_source).map(([k, v]) => `${v} ${k}${k === "transcript" ? byAgent : ""}`).join(" · ");
  const mem = Object.entries(stats.memory ?? {}).map(([k, v]) => `${v} ${k}`).join(" · ");
  return (
    <div className="flex flex-col gap-3.5 h-full overflow-auto">
      <VersionCard active={active} indexing={!prog.idle} />
      <InstallCard active={active} onOpenSetting={onOpenSetting} />
      <div className="flex gap-3.5">
        <Tile label="Documents" value={String(stats.documents)} sub={src || "—"} />
        <Tile label="Chunks" value={stats.chunks.toLocaleString()} sub="512-token target" />
        <Tile label="Model" value={stats.model?.split("@")[0] ?? "—"} sub={stats.model ?? "not stamped"} />
        <Tile label="Memory" value={String(Object.values(stats.memory ?? {}).reduce((a, b) => a + b, 0))} sub={mem || "none saved"} />
      </div>
      {!prog.idle && (
        <div className="bg-surface border border-line rounded-lg px-4 py-3 flex items-center gap-3.5">
          <span className="text-[11px] font-semibold uppercase tracking-wider text-muted">Indexing</span>
          <div className="grow h-2 bg-sunken rounded overflow-hidden">
            <div className="h-full bg-accent rounded" style={{ width: `${prog.pct ?? 0}%` }} />
          </div>
          <span className="font-mono text-xs">
            {prog.pct?.toFixed(1)}% · {prog.docs_done}/{prog.docs_total} docs · {Math.round((prog.eta_s ?? 0) / 60)}m left
          </span>
        </div>
      )}
      <div className="flex gap-3.5 shrink-0">
        <div className="flex-[3] bg-surface border border-line rounded-lg p-4 flex flex-col gap-2">
          <span className="text-sm font-semibold">Recall@5 by tier</span>
          {stats.bench?.length ? (
            <ResponsiveContainer width="100%" height={240}>
              <BarChart data={stats.bench} margin={{ top: 18, left: -22 }}>
                <CartesianGrid stroke={PALETTE.line} vertical={false} />
                <XAxis dataKey="tier" tick={{ fontSize: 11, fill: PALETTE.muted }} axisLine={{ stroke: PALETTE.line }} tickLine={false} />
                <YAxis domain={[0, 1]} tick={{ fontSize: 11, fill: PALETTE.muted }} axisLine={false} tickLine={false} />
                <Bar dataKey="recall_at_5" fill={PALETTE.accent} radius={[4, 4, 0, 0]}>
                  <LabelList dataKey="recall_at_5" position="top" style={{ fontSize: 12, fill: PALETTE.ink, fontFamily: "var(--font-mono)" }} formatter={(v: number) => v.toFixed(2)} />
                </Bar>
              </BarChart>
            </ResponsiveContainer>
          ) : (
            <RunBench />
          )}
        </div>
        <div className="flex-[2] bg-surface border border-line rounded-lg p-4 flex flex-col gap-2">
          <span className="text-sm font-semibold">Latency p50</span>
          {!stats.bench?.length && <RunBench />}
          {(stats.bench ?? []).map((b) => (
            <div key={b.tier} className="flex items-center gap-2.5">
              <span className="w-[74px] text-xs text-muted">{b.tier}</span>
              <div className="h-2 bg-accent rounded-r" style={{ width: `${Math.max(4, (b.p50_ms / 400) * 130)}px` }} />
              <span className="font-mono text-xs">{b.p50_ms} ms</span>
            </div>
          ))}
        </div>
        <div className="flex-[3] bg-surface border border-line rounded-lg p-4 flex flex-col min-h-0">
          <div className="flex items-baseline gap-2 pb-2">
            <span className="text-sm font-semibold">Skipped files</span>
            <span className="font-mono text-[11px] text-muted">{stats.skipped.length}</span>
          </div>
          <div className="overflow-auto flex flex-col max-h-72">
            {stats.skipped.length === 0 && <span className="text-sm text-muted">none</span>}
            {stats.skipped.map((s) => (
              <div key={s} className="font-mono text-[11px] py-1.5 border-t border-sunken truncate">{s}</div>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}
