import { useEffect, useMemo, useRef, useState } from "react";
import {
  checkConfig,
  ConfigCheck,
  ConfigState,
  describeError,
  EmbedEndpointChange,
  FieldError,
  getConfig,
  saveConfig,
  setEmbedEndpoint,
  Stats,
  TomlValue,
} from "../api";
import { ErrorBanner } from "../components";
import { Ctx } from "../settings/fields";
import { busy, IndexStatus, useIndexRunner } from "../settings/IndexPanel";
import {
  at,
  change,
  defaultOf,
  describe,
  fieldId,
  inFile,
  needsReindex,
  Pending,
  Section,
  SECTIONS,
  sectionOf,
  toPatch,
} from "../settings/model";
import {
  AdvancedSection,
  DocumentsSection,
  EmbeddingSection,
  MemorySection,
  RetrievalSection,
  SourcesSection,
  UpdatesSection,
} from "../settings/sections";

export type SettingsRequest = { section: Section; n: number; focus?: string };

type Notice = { text: string; warn: boolean; reindex?: boolean };

type Checked = { key: string; result: ConfigCheck };

function reason(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function sameError(a: FieldError, b: FieldError): boolean {
  return a.path === b.path && a.message === b.message;
}

function merge(a: FieldError[], b: FieldError[]): FieldError[] {
  return [...a, ...b.filter((e) => !a.some((x) => sameError(x, e)))];
}

const HOOK_NOTE = "The prompt hook reads config.toml on every prompt, so saved changes apply from your next prompt; nothing needs a restart.";

export default function SettingsTab({
  stats,
  request,
  onSaved,
  onIndexed,
}: {
  stats: Stats | null;
  request: SettingsRequest | null;
  onSaved: () => void;
  onIndexed: () => void;
}) {
  const [state, setState] = useState<ConfigState | null>(null);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  const [pending, setPending] = useState<Pending>({});
  const [checked, setChecked] = useState<Checked | null>(null);
  const [serverErrors, setServerErrors] = useState<FieldError[]>([]);
  const [section, setSection] = useState<Section>(request?.section ?? "Sources");
  const [preview, setPreview] = useState(false);
  const [saving, setSaving] = useState(false);
  const [endpointBusy, setEndpointBusy] = useState(false);
  const [conflict, setConflict] = useState<string | null>(null);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [focus, setFocus] = useState<{ path: string; n: number } | null>(null);
  const writing = useRef(false);
  const pane = useRef<HTMLDivElement>(null);
  const runner = useIndexRunner(onIndexed);

  const load = async (keepPending: boolean) => {
    setLoadErr(null);
    try {
      const next = await getConfig();
      setState(next);
      if (!keepPending) setPending({});
      setServerErrors([]);
      return next;
    } catch (e) {
      setLoadErr(describeError(e));
      return null;
    }
  };

  useEffect(() => {
    load(false);
  }, []);

  useEffect(() => {
    if (!request) return;
    if (request.focus) {
      setSection(sectionOf(request.focus));
      setFocus({ path: request.focus, n: request.n });
    } else setSection(request.section);
  }, [request?.n]);

  const patchKey = useMemo(() => JSON.stringify(toPatch(pending)), [pending]);
  const dirtyCount = Object.keys(pending).length;

  useEffect(() => {
    setServerErrors([]);
    if (dirtyCount === 0) {
      setChecked(null);
      return;
    }
    let current = true;
    const timer = setTimeout(() => {
      checkConfig(toPatch(pending))
        .then((result) => current && setChecked({ key: patchKey, result }))
        .catch(() => current && setChecked({ key: patchKey, result: { errors: [], blocking: [] } }));
    }, 300);
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [patchKey]);

  useEffect(() => {
    if (!focus || !state) return;
    const parts = focus.path.split(".");
    let target: HTMLElement | null = null;
    while (parts.length && !target) {
      target = document.getElementById(fieldId(parts.join(".")));
      parts.pop();
    }
    if (!target) {
      if (section !== "Advanced") {
        setSection("Advanced");
        return;
      }
      target = document.getElementById(fieldId("raw"));
    }
    if (!target) return;
    target.scrollIntoView({ block: "center" });
    target.classList.add("ring-2", "ring-warn", "rounded-md");
    const shown = target;
    setTimeout(() => shown.classList.remove("ring-2", "ring-warn", "rounded-md"), 1600);
    setFocus(null);
  }, [focus, section, state]);

  useEffect(() => {
    if (!focus) pane.current?.scrollTo({ top: 0 });
  }, [section]);

  if (loadErr && !state) {
    return (
      <div className="h-full flex flex-col items-center gap-3 pt-8">
        <ErrorBanner>{loadErr}</ErrorBanner>
        <button onClick={() => load(false)} className="text-xs font-semibold text-accent underline">
          Try again
        </button>
      </div>
    );
  }
  if (!state) return <p className="text-sm text-muted p-4">Loading…</p>;

  const fresh = checked !== null && checked.key === patchKey;
  const checking = dirtyCount > 0 && !fresh;
  const blocking = merge(fresh ? checked.result.blocking : [], serverErrors);
  const errors = merge(dirtyCount === 0 ? state.errors : fresh ? checked.result.errors : state.errors, serverErrors);
  const locked = saving || endpointBusy;
  const unparseable = state.file === null;

  const set = (path: string, value: TomlValue | null) => {
    if (writing.current) return;
    setPreview(false);
    setPending((p) => change(state, p, path, value));
  };
  const ctx: Ctx = { state, pending, errors, blocking, locked, set };

  const jump = (path: string) => {
    setSection(sectionOf(path));
    setFocus({ path, n: Date.now() });
  };

  const discard = () => {
    if (writing.current) return;
    setPending({});
    setPreview(false);
    setServerErrors([]);
  };

  const save = async () => {
    if (writing.current || blocking.length > 0 || checking) return;
    const sent = pending;
    writing.current = true;
    setSaving(true);
    setNotice(null);
    try {
      const result = await saveConfig(state.etag, toPatch(sent));
      if (result.kind === "saved") {
        setState(result.state);
        setPending({});
        setPreview(false);
        setServerErrors([]);
        setConflict(null);
        setNotice({ text: `Saved to ${result.state.path}. ${HOOK_NOTE}`, warn: false, reindex: needsReindex(sent) });
        onSaved();
      } else if (result.kind === "conflict") {
        setPreview(false);
        setConflict(result.error || "config.toml changed on disk since this page read it.");
      } else {
        setServerErrors(result.errors);
        setPreview(false);
        setNotice({ text: `Nothing was saved. ${result.error}`, warn: true });
      }
    } catch (e) {
      setNotice({ text: `Nothing was saved: ${reason(e)}`, warn: true });
    } finally {
      writing.current = false;
      setSaving(false);
    }
  };

  const onEndpoint = async (changeSet: EmbedEndpointChange): Promise<FieldError[] | null> => {
    if (writing.current) return null;
    writing.current = true;
    setEndpointBusy(true);
    try {
      const result = await setEmbedEndpoint(changeSet);
      if (result.kind === "saved") {
        setState(result.state);
        onSaved();
        return null;
      }
      return result.errors;
    } catch (e) {
      return [{ path: "endpoint.url", message: reason(e), line: null }];
    } finally {
      writing.current = false;
      setEndpointBusy(false);
    }
  };

  const dirtyIn = (s: Section) => Object.keys(pending).filter((p) => sectionOf(p) === s).length;
  const errorsIn = (s: Section) => errors.filter((e) => sectionOf(e.path) === s).length;

  const shown = (path: string, value: unknown, fromFile: boolean) =>
    fromFile ? describe(value) : `default ${describe(defaultOf(state, path))}`;

  return (
    <div className="h-full flex gap-4 min-h-0" data-testid="settings">
      <nav className="w-[210px] shrink-0 bg-surface border border-line rounded-lg p-2 flex flex-col gap-0.5">
        {SECTIONS.map((s) => (
          <button
            key={s}
            onClick={() => setSection(s)}
            aria-current={s === section ? "page" : undefined}
            className={`text-left px-3 py-2 rounded-md text-sm flex items-center gap-2 ${s === section ? "font-semibold text-accent bg-accent-soft" : "text-ink hover:bg-sunken"}`}
          >
            <span className="grow">{s}</span>
            {errorsIn(s) > 0 && <span className="w-1.5 h-1.5 rounded-full bg-warn" title="has problems" />}
            {dirtyIn(s) > 0 && <span className="font-mono text-[10px] text-accent">{dirtyIn(s)}</span>}
          </button>
        ))}
        <div className="grow" />
        <div className="px-3 py-2 text-[11px] text-muted break-all">
          <div className="font-semibold uppercase tracking-wider pb-1">File</div>
          <span className="font-mono">{state.path}</span>
          {!state.exists && <div className="pt-1">not created yet; the first save creates it</div>}
        </div>
      </nav>
      <div className="grow flex flex-col min-h-0 min-w-0 gap-3">
        <div ref={pane} className="grow overflow-auto min-h-0 flex flex-col gap-4 pr-1">
          {conflict && (
            <div className="rounded-md px-4 py-3 text-sm bg-[#F6E4DC] text-warn flex items-center gap-3" role="alert" data-testid="conflict">
              <span className="grow">
                config.toml changed on disk since this page read it, so nothing was saved. Reload to see the new file and lose your
                {` ${dirtyCount}`} unsaved change{dirtyCount === 1 ? "" : "s"}, or keep editing: your changes stay and the next save applies
                them on top of the new file.
              </span>
              <button
                onClick={async () => {
                  setConflict(null);
                  await load(false);
                }}
                disabled={locked}
                className="text-xs font-semibold text-white bg-warn rounded-md px-2.5 py-1.5 shrink-0"
              >
                Reload (discard mine)
              </button>
              <button
                onClick={async () => {
                  setConflict(null);
                  await load(true);
                }}
                disabled={locked}
                className="text-xs font-semibold text-warn bg-surface border border-line rounded-md px-2.5 py-1.5 shrink-0"
              >
                Keep editing
              </button>
            </div>
          )}
          {saving && (
            <div className="rounded-md px-4 py-2 text-sm bg-sunken text-muted">
              Saving config.toml. The form is locked until the server answers.
            </div>
          )}
          {notice && (
            <div
              className={`rounded-md px-4 py-2.5 text-sm flex flex-col gap-2 ${notice.warn ? "bg-[#F6E4DC] text-warn" : "bg-accent-soft text-accent"}`}
              role="status"
              data-testid="settings-notice"
            >
              <div className="flex items-start gap-3">
                <span className="grow">{notice.text}</span>
                <button onClick={() => setNotice(null)} className="text-xs shrink-0">dismiss</button>
              </div>
              {notice.reindex && (
                <div className="flex items-center gap-3 text-warn">
                  <span className="grow">
                    The embedding model or dimensions changed, so the index must be rebuilt from scratch
                    (<code className="font-mono">br8n index --reindex</code>). Search keeps using the old index until it finishes.
                  </span>
                  <button
                    onClick={() => runner.start(true)}
                    disabled={busy(runner)}
                    className="px-3 py-1.5 bg-warn text-white rounded-md text-xs font-semibold disabled:opacity-50 shrink-0"
                  >
                    Start full re-index
                  </button>
                </div>
              )}
              {notice.reindex && <IndexStatus runner={runner} />}
            </div>
          )}
          {loadErr && <ErrorBanner>{loadErr}</ErrorBanner>}
          {unparseable && (
            <ErrorBanner>
              config.toml does not parse, so br8n is running on defaults and the form shows them. Fix the syntax error by hand;
              until then nothing can be saved from here.
            </ErrorBanner>
          )}
          {state.errors.length > 0 && (
            <div className="bg-surface border border-[#E8C3B3] rounded-lg px-4 py-3 flex flex-col gap-1.5" data-testid="file-errors">
              <span className="text-sm font-semibold text-warn">
                {state.errors.length} problem{state.errors.length === 1 ? "" : "s"} already in config.toml
              </span>
              <span className="text-xs text-muted">
                These were in the file before this page opened. They do not block saving other settings.
              </span>
              {state.errors.map((e) => (
                <button
                  key={`${e.path}:${e.message}`}
                  onClick={() => jump(e.path)}
                  className="text-left text-sm flex gap-2 hover:underline"
                >
                  <span className="font-mono text-xs text-muted shrink-0 pt-0.5">{e.line !== null ? `line ${e.line}` : "—"}</span>
                  <span className="font-mono text-xs shrink-0 pt-0.5">{e.path || "syntax"}</span>
                  <span className="text-warn">{e.message}</span>
                </button>
              ))}
            </div>
          )}
          <div className="flex items-baseline gap-3">
            <h2 className="font-serif font-semibold text-xl">{section}</h2>
            <span className="text-xs text-muted">Saved changes reach the prompt hook on its next prompt, with no restart.</span>
          </div>
          {section === "Sources" && <SourcesSection ctx={ctx} runner={runner} />}
          {section === "Retrieval" && <RetrievalSection ctx={ctx} bench={stats?.bench ?? null} />}
          {section === "Embedding" && <EmbeddingSection ctx={ctx} onEndpoint={onEndpoint} />}
          {section === "Memory" && <MemorySection ctx={ctx} />}
          {section === "Documents" && <DocumentsSection ctx={ctx} />}
          {section === "Updates & backup" && <UpdatesSection ctx={ctx} />}
          {section === "Advanced" && (
            <AdvancedSection
              ctx={ctx}
              onRaw={(next) => {
                setPreview(false);
                setPending(next);
              }}
            />
          )}
          <div className="h-2 shrink-0" />
        </div>
        {dirtyCount > 0 && (
          <div className="shrink-0 bg-surface border border-line rounded-lg px-4 py-3 flex flex-col gap-3 shadow-[0_-4px_16px_rgba(22,33,29,0.08)]" data-testid="save-bar">
            {preview && (
              <div className="flex flex-col gap-1.5 max-h-[240px] overflow-auto" data-testid="diff-preview">
                <span className="text-[11px] font-semibold uppercase tracking-wider text-muted">This will be written to config.toml</span>
                {Object.entries(pending).map(([path, value]) => (
                  <button
                    key={path}
                    onClick={() => jump(path)}
                    className="text-left font-mono text-xs flex gap-2 items-baseline hover:bg-sunken rounded px-1"
                  >
                    <span className="font-semibold shrink-0">{path}</span>
                    <span className="text-muted truncate">{shown(path, at(state.file, path), inFile(state, path))}</span>
                    <span className="text-muted shrink-0">→</span>
                    <span className="text-accent truncate">{value === null ? `default ${describe(defaultOf(state, path))}` : describe(value)}</span>
                  </button>
                ))}
                {needsReindex(pending) && (
                  <span className="text-xs text-warn">The embedding model or dimensions change: the index will need a full re-index.</span>
                )}
              </div>
            )}
            <div className="flex items-center gap-3">
              <span className="text-sm font-semibold">
                {dirtyCount} unsaved change{dirtyCount === 1 ? "" : "s"}
              </span>
              {checking && <span className="text-xs text-muted">checking…</span>}
              {!checking && blocking.length > 0 && (
                <button onClick={() => jump(blocking[0].path)} className="text-xs text-warn underline">
                  fix {blocking.length} problem{blocking.length === 1 ? "" : "s"} to save
                </button>
              )}
              <div className="grow" />
              <button
                onClick={discard}
                disabled={locked}
                className="text-xs font-semibold text-warn bg-surface border border-line rounded-md px-3 py-2 disabled:opacity-50"
              >
                Discard
              </button>
              {preview ? (
                <>
                  <button onClick={() => setPreview(false)} disabled={locked} className="text-xs font-semibold text-muted px-2 py-2">
                    Back
                  </button>
                  <button
                    onClick={save}
                    disabled={locked || checking || blocking.length > 0 || unparseable}
                    className="px-4 py-2 bg-accent text-white rounded-md text-sm font-semibold disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    {saving ? "Saving…" : "Write config.toml"}
                  </button>
                </>
              ) : (
                <button
                  onClick={() => setPreview(true)}
                  disabled={locked || checking || blocking.length > 0 || unparseable}
                  className="px-4 py-2 bg-accent text-white rounded-md text-sm font-semibold disabled:opacity-50 disabled:cursor-not-allowed"
                >
                  Save
                </button>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
