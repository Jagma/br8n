import { useEffect, useState } from "react";
import { describeError, getVersion, startUpdate, VersionInfo } from "./api";

function ago(unix: number | null): string {
  if (!unix) return "never";
  const s = Math.max(0, Math.floor(Date.now() / 1000) - unix);
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}

export default function VersionCard({ active, indexing }: { active: boolean; indexing: boolean }) {
  const [v, setV] = useState<VersionInfo | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    if (!active) return;
    const tick = () => getVersion().then(setV).catch(() => {});
    tick();
    const t = setInterval(tick, 2000);
    return () => clearInterval(t);
  }, [active]);

  const check = () => {
    setBusy(true);
    setErr(null);
    getVersion(true).then(setV).catch((e) => setErr(describeError(e))).finally(() => setBusy(false));
  };
  const update = () => {
    setBusy(true);
    setErr(null);
    startUpdate().then(() => getVersion().then(setV)).catch((e) => setErr(describeError(e))).finally(() => setBusy(false));
  };

  if (!v) return null;
  const u = v.update;
  const running = u && !u.done;
  const btn = "px-3 py-1 rounded-md text-sm font-medium border border-line disabled:opacity-40";
  let body: JSX.Element;
  if (running) {
    body = <span className="text-sm">Updating to {u.to ?? "…"}: <span className="font-mono">{u.phase}</span> {u.message && <span className="text-muted">— {u.message}</span>}</span>;
  } else if (u && u.done && u.ok) {
    body = <span className="text-sm">Updated to <span className="font-mono">{u.to}</span>. Restart Claude Code, and this dashboard: this page is still served by the old binary.</span>;
  } else if (u && u.done && u.ok === false) {
    body = <span className="text-sm text-warn">Update failed: {u.message}. See db.log beside the index.</span>;
  } else if (v.update_available) {
    body = (
      <span className="text-sm">
        <span className="font-mono">{v.update_available}</span> available, you have <span className="font-mono">{v.installed}</span>
        {v.url && <> · <a className="text-accent underline" href={v.url} target="_blank" rel="noreferrer">release notes</a></>}
        {v.error && <span className="text-warn"> · last check failed {ago(v.checked_at)}: {v.error}</span>}
      </span>
    );
  } else if (v.error) {
    body = <span className="text-sm text-warn">update check failed {ago(v.checked_at)}: {v.error}</span>;
  } else {
    body = <span className="text-sm text-muted">up to date · checked {ago(v.checked_at)}</span>;
  }
  const updateSucceeded = !!(u && u.done && u.ok);
  const showUpdate = !updateSucceeded && !running && (v.update_available || (u && u.done && u.ok === false));
  const showCheck = !updateSucceeded && !running;
  return (
    <div className="bg-surface border border-line rounded-lg px-4 py-3 flex items-center gap-3.5">
      <span className="text-[11px] font-semibold uppercase tracking-wider text-muted">br8n</span>
      <span className="font-mono text-sm">{v.installed}</span>
      <div className="grow">{body}</div>
      {err && <span className="text-xs text-warn">{err}</span>}
      {indexing && !running && <span className="text-xs text-muted">an index is running</span>}
      {showUpdate && <button className={`${btn} bg-accent text-white border-accent`} disabled={busy || indexing} onClick={update}>Update</button>}
      {showCheck && <button className={btn} disabled={busy} onClick={check}>Check now</button>}
    </div>
  );
}
