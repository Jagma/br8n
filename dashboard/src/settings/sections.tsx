import { ReactNode, useEffect, useState } from "react";
import { parse as parseToml, stringify as stringifyToml } from "smol-toml";
import {
  BenchTier,
  checkConfig,
  ConfigState,
  EmbedEndpointChange,
  EmbedTest,
  FieldError,
  TomlTable,
  TomlValue,
  testEmbedding,
} from "../api";
import {
  Card,
  Ctx,
  FieldErrors,
  inputClass,
  Label,
  NumberField,
  NumberInput,
  Segmented,
  SelectField,
  Shell,
  TextField,
  ToggleField,
} from "./fields";
import { busy, IndexRunner, IndexStatus } from "./IndexPanel";
import { at, diff, fieldId, isTable, Pending, SURFACE_DEFAULT_TIER, TIERS, valueOf, withPending } from "./model";

const primary =
  "px-4 py-2 bg-accent text-white rounded-md text-sm font-semibold disabled:opacity-50 disabled:cursor-not-allowed";
const secondary =
  "px-3 py-1.5 text-xs font-semibold text-accent bg-accent-soft border border-line rounded-md disabled:opacity-50 disabled:cursor-not-allowed";
const danger =
  "px-3 py-1.5 text-xs font-semibold text-warn bg-[#F6E4DC] rounded-md disabled:opacity-50 disabled:cursor-not-allowed";

function Code({ children }: { children: ReactNode }) {
  return <code className="font-mono bg-sunken px-1 rounded text-[12px]">{children}</code>;
}

function stringList(v: unknown): string[] {
  return Array.isArray(v) ? v.map(String) : [];
}

function ListEditor({
  ctx,
  path,
  placeholder,
  validate,
  addLabel,
  itemErrors,
}: {
  ctx: Ctx;
  path: string;
  placeholder: string;
  validate?: (candidate: string, list: string[]) => Promise<string | null>;
  addLabel: string;
  itemErrors?: (item: string) => string[];
}) {
  const list = stringList(valueOf(ctx.state, ctx.pending, path));
  const [candidate, setCandidate] = useState("");
  const [verdict, setVerdict] = useState<{ candidate: string; problem: string | null } | null>(null);
  const trimmed = candidate.trim();
  const listKey = list.join("\n");

  useEffect(() => {
    if (!validate || trimmed === "") return;
    let current = true;
    const timer = setTimeout(() => {
      validate(trimmed, list)
        .then((problem) => current && setVerdict({ candidate: trimmed, problem }))
        .catch(() => current && setVerdict({ candidate: trimmed, problem: null }));
    }, 300);
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [trimmed, listKey]);

  const checking = !!validate && trimmed !== "" && verdict?.candidate !== trimmed;
  const problem = !checking && verdict?.candidate === trimmed ? verdict.problem : null;
  const duplicate = list.includes(trimmed);
  const add = () => {
    if (trimmed === "" || duplicate || problem || checking) return;
    ctx.set(path, [...list, trimmed]);
    setCandidate("");
  };
  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-col border border-line rounded-md divide-y divide-sunken">
        {list.length === 0 && <span className="text-sm text-muted px-3 py-2">none</span>}
        {list.map((item) => {
          const errors = itemErrors?.(item) ?? [];
          return (
            <div key={item} className="flex items-center gap-2 px-3 py-1.5" data-item={item}>
              <span className={`font-mono text-xs grow truncate ${errors.length ? "text-warn" : ""}`} title={item}>
                {item}
              </span>
              {errors.length > 0 && <span className="text-[11px] text-warn shrink-0">missing</span>}
              <button
                onClick={() => ctx.set(path, list.filter((x) => x !== item))}
                disabled={ctx.locked}
                className="text-xs text-muted hover:text-warn disabled:opacity-50 disabled:cursor-not-allowed"
                aria-label={`remove ${item}`}
              >
                remove
              </button>
            </div>
          );
        })}
      </div>
      <div className="flex gap-2">
        <input
          value={candidate}
          onChange={(e) => setCandidate(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && add()}
          placeholder={placeholder}
          disabled={ctx.locked}
          aria-label={`add to ${path}`}
          className={`${inputClass} font-mono grow`}
        />
        <button onClick={add} disabled={ctx.locked || trimmed === "" || duplicate || !!problem || checking} className={secondary}>
          {addLabel}
        </button>
      </div>
      {trimmed !== "" && checking && <span className="text-xs text-muted">checking…</span>}
      {duplicate && <span className="text-xs text-warn">already in the list</span>}
      {problem && (
        <span className="text-xs text-warn" role="alert" data-testid={`${path}-candidate-error`}>
          {problem}
        </span>
      )}
    </div>
  );
}

export function sourceProblem(state: ConfigState, pending: Pending) {
  return async (candidate: string, list: string[]): Promise<string | null> => {
    if (!candidate.startsWith("/") && !candidate.startsWith("~")) {
      return "Use an absolute path, or one that starts with ~/.";
    }
    const probe = { ...pending, sources: [...list, candidate] };
    const set: Record<string, TomlValue> = {};
    const unset: string[] = [];
    for (const [k, v] of Object.entries(probe)) {
      if (v === null) unset.push(k);
      else set[k] = v;
    }
    const { errors } = await checkConfig({ set, unset });
    const tail = candidate.replace(/^~/, "").replace(/\/+$/, "");
    const mine = errors.find((e) => e.path === "sources" && e.message.includes(tail));
    return mine ? mine.message.replace(", and `br8n index` refuses to run until it does", "") : null;
  };
}

export function SourcesSection({ ctx, runner }: { ctx: Ctx; runner: IndexRunner }) {
  const sourceErrors = (item: string) => {
    const tail = item.replace(/^~/, "").replace(/\/+$/, "");
    return ctx.errors.filter((e) => e.path === "sources" && e.message.includes(tail)).map((e) => e.message);
  };
  return (
    <>
      <Card title="Folders and files">
        <Shell
          ctx={ctx}
          path="sources"
          label="Sources"
          hint={<>Folders are walked recursively; a single file works too. Changes apply at the next index.</>}
        >
          <ListEditor
            ctx={ctx}
            path="sources"
            placeholder="/absolute/path/to/notes or ~/notes"
            addLabel="Add folder"
            validate={sourceProblem(ctx.state, ctx.pending)}
            itemErrors={sourceErrors}
          />
        </Shell>
        <Shell ctx={ctx} path="ignore" label="Ignore globs" hint="Matched against paths inside each source.">
          <ListEditor ctx={ctx} path="ignore" placeholder="**/node_modules/**" addLabel="Add glob" />
        </Shell>
      </Card>
      <Card title="Agent sessions">
        <ToggleField
          ctx={ctx}
          path="index_transcripts"
          label="Index session transcripts"
          hint="Claude Code's sessions from ~/.claude/projects. Off turns off every agent's sessions, Codex's included."
        />
        <ToggleField
          ctx={ctx}
          path="index_codex_sessions"
          label="Index Codex sessions"
          hint={<>Codex's sessions from <Code>$CODEX_HOME/sessions</Code> (<Code>~/.codex/sessions</Code>). Only counts while session transcripts are on.</>}
        />
        <NumberField
          ctx={ctx}
          path="index_transcripts_max_age_days"
          label="Max age in days"
          optional
          hint="Leave empty to index sessions of any age."
        />
      </Card>
      <Card
        title="Index"
        aside={
          <button onClick={() => runner.start(false)} disabled={busy(runner)} className={primary}>
            {busy(runner) ? "Indexing…" : "Re-index now"}
          </button>
        }
      >
        <p className="text-sm text-muted">
          Runs <Code>br8n index</Code> in the background with the SAVED settings: only new and changed files are read.
          {Object.keys(ctx.pending).length > 0 && " You have unsaved changes; save them first if they should count."}
        </p>
        <IndexStatus runner={runner} />
      </Card>
    </>
  );
}

function benchFor(bench: BenchTier[] | null, tier: string): BenchTier | undefined {
  return bench?.find((b) => b.tier === tier);
}

function SurfaceCard({ ctx, surface, title, bench }: { ctx: Ctx; surface: "hook" | "mcp"; title: string; bench: BenchTier[] | null }) {
  const qualityPath = `${surface}.quality`;
  const raw = valueOf(ctx.state, ctx.pending, qualityPath);
  const tier = typeof raw === "number" ? raw : SURFACE_DEFAULT_TIER[surface];
  const thresholdPath = `${surface}.threshold`;
  const threshold = valueOf(ctx.state, ctx.pending, thresholdPath);
  const sliderValue = typeof threshold === "number" ? threshold : 0;
  return (
    <Card title={title}>
      <Shell ctx={ctx} path={qualityPath} label="Quality tier" showDefault={false}
        hint={<>Default for this surface: {TIERS[SURFACE_DEFAULT_TIER[surface]]}. Higher tiers find more and take longer.</>}>
        <Segmented
          options={[0, 1, 2, 3, 4] as const}
          value={tier as 0 | 1 | 2 | 3 | 4}
          onChange={(t) => ctx.set(qualityPath, t)}
          disabled={ctx.locked}
          render={(t) => {
            const b = benchFor(bench, TIERS[t]);
            return (
              <span className="flex flex-col items-center leading-tight">
                <span>{TIERS[t]}</span>
                {b && (
                  <span className="font-mono text-[10px] text-muted font-normal">
                    R@5 {b.recall_at_5.toFixed(2)} · {Math.round(b.p50_ms)}ms
                  </span>
                )}
              </span>
            );
          }}
        />
        {!bench && <span className="text-xs text-muted">Run <Code>br8n bench</Code> to see each tier's measured recall and latency here.</span>}
      </Shell>
      <Shell ctx={ctx} path={thresholdPath} label="Relevance threshold"
        hint="Hits below this relevance are never injected. Lower finds more, and lets more noise in.">
        <div className="flex items-center gap-3">
          <input
            type="range"
            min={0}
            max={1}
            step={0.01}
            value={sliderValue}
            onChange={(e) => ctx.set(thresholdPath, Number(e.target.value))}
            disabled={ctx.locked}
            aria-label={`${thresholdPath} slider`}
            className="grow accent-[#0E6B58]"
          />
          <NumberInput ctx={ctx} path={thresholdPath} className="w-[90px]" />
        </div>
      </Shell>
      <NumberField ctx={ctx} path={`${surface}.max_tokens`} label="Max tokens" hint="The token budget for what one search hands back." />
    </Card>
  );
}

export function RetrievalSection({ ctx, bench }: { ctx: Ctx; bench: BenchTier[] | null }) {
  return (
    <>
      <SurfaceCard ctx={ctx} surface="hook" title="Prompt hook" bench={bench} />
      <SurfaceCard ctx={ctx} surface="mcp" title="MCP tool (br8n_search)" bench={bench} />
    </>
  );
}

type EndpointErrors = FieldError[];

function overrideFor(state: ConfigState, setting: string): string | null {
  return state.env_overrides.find((o) => o.setting === setting)?.variable ?? null;
}

function TestConnection({ ctx }: { ctx: Ctx }) {
  const [result, setResult] = useState<EmbedTest | null>(null);
  const [testing, setTesting] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const expected = at(ctx.state.effective, "embed.dimensions");
  const unsaved = Object.keys(ctx.pending).some((k) => k.startsWith("embed."));
  const run = () => {
    setTesting(true);
    setFailure(null);
    setResult(null);
    testEmbedding()
      .then(setResult)
      .catch((e) => setFailure(e instanceof Error ? e.message : String(e)))
      .finally(() => setTesting(false));
  };
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-3">
        <button onClick={run} disabled={testing} className={secondary}>
          {testing ? "Testing…" : "Test connection"}
        </button>
        <span className="text-xs text-muted">
          Embeds one sentence with the saved settings{unsaved ? "; save first to test your unsaved changes" : ""}.
        </span>
      </div>
      {result && result.ok && (
        <div className="rounded-md px-3 py-2 text-sm bg-accent-soft text-accent" data-testid="embed-test-result">
          Connected to {result.backend === "remote" ? "the remote endpoint" : "Ollama"} in {result.latency_ms} ms · {result.dimensions} dimensions
          {result.model ? ` · ${result.model}` : ""}
          {typeof expected === "number" && result.dimensions !== null && result.dimensions !== expected && (
            <span className="text-warn"> · the index expects {expected} dimensions, so this model needs a full re-index</span>
          )}
        </div>
      )}
      {result && !result.ok && (
        <div className="rounded-md px-3 py-2 text-sm bg-[#F6E4DC] text-warn" data-testid="embed-test-result">
          Could not embed through {result.url ?? result.backend}: {result.error ?? "unknown error"}
        </div>
      )}
      {failure && <div className="rounded-md px-3 py-2 text-sm bg-[#F6E4DC] text-warn">{failure}</div>}
    </div>
  );
}

function RemoteEndpoint({
  state,
  locked,
  onChange,
}: {
  state: ConfigState;
  locked: boolean;
  onChange: (change: EmbedEndpointChange) => Promise<EndpointErrors | null>;
}) {
  const [url, setUrl] = useState(state.endpoint.url ?? "");
  const [model, setModel] = useState(state.endpoint.model ?? "");
  const [replacing, setReplacing] = useState(false);
  const [token, setToken] = useState("");
  const [errors, setErrors] = useState<EndpointErrors>([]);
  const [saved, setSaved] = useState<string | null>(null);
  useEffect(() => {
    setUrl(state.endpoint.url ?? "");
    setModel(state.endpoint.model ?? "");
  }, [state.endpoint.url, state.endpoint.model]);
  const tokenSet = state.secrets["embed.token"] === "set";
  const urlOverride = overrideFor(state, "endpoint.url");
  const modelOverride = overrideFor(state, "endpoint.model");
  const tokenOverride = overrideFor(state, "embed.token");
  const modelChanged = state.endpoint.model !== null && model.trim() !== state.endpoint.model;

  const submit = async (change: EmbedEndpointChange, done: string) => {
    setSaved(null);
    const problems = await onChange(change);
    setErrors(problems ?? []);
    if (!problems) {
      setSaved(done);
      setReplacing(false);
      setToken("");
    }
  };
  const save = () => {
    const change: EmbedEndpointChange = {};
    if (!urlOverride && url.trim() !== (state.endpoint.url ?? "")) change.url = url.trim() || null;
    if (!modelOverride && model.trim() !== (state.endpoint.model ?? "")) change.model = model.trim() || null;
    if (replacing && token !== "") change.token = token;
    submit(change, "Endpoint saved to the env file beside config.toml.");
  };
  const errorFor = (path: string) => errors.filter((e) => e.path === path);
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-1.5">
        <div className="flex items-center gap-2">
          <Label>Endpoint URL</Label>
          {urlOverride && <span className="text-[10px] font-semibold text-warn">overridden by ${urlOverride}</span>}
        </div>
        <input value={url} onChange={(e) => setUrl(e.target.value)} disabled={locked || !!urlOverride}
          placeholder="https://host/v1" aria-label="endpoint.url" className={`${inputClass} font-mono`} />
        <FieldErrors errors={errorFor("endpoint.url")} blocking={errorFor("endpoint.url")} />
      </div>
      <div className="flex flex-col gap-1.5">
        <div className="flex items-center gap-2">
          <Label>Model (wire name)</Label>
          {modelOverride && <span className="text-[10px] font-semibold text-warn">overridden by ${modelOverride}</span>}
        </div>
        <input value={model} onChange={(e) => setModel(e.target.value)} disabled={locked || !!modelOverride}
          placeholder="text-embedding-model" aria-label="endpoint.model" className={`${inputClass} font-mono`} />
        {modelChanged && (
          <span className="text-xs text-warn">
            If this serves different weights than the ones the index was built with, run a full re-index after saving.
          </span>
        )}
        <FieldErrors errors={errorFor("endpoint.model")} blocking={errorFor("endpoint.model")} />
      </div>
      <div id={fieldId("embed.token")} className="flex flex-col gap-1.5 scroll-mt-4">
        <div className="flex items-center gap-2">
          <Label>Token</Label>
          <span className={`text-[11px] font-semibold ${tokenSet ? "text-accent" : "text-muted"}`} data-testid="token-state">
            {tokenSet ? "set" : "not set"}
          </span>
          {tokenOverride && <span className="text-[10px] font-semibold text-warn">overridden by ${tokenOverride}</span>}
          <div className="grow" />
          {!replacing && !tokenOverride && (
            <button onClick={() => setReplacing(true)} disabled={locked} className={secondary}>
              {tokenSet ? "Replace" : "Set token"}
            </button>
          )}
          {tokenSet && !tokenOverride && (
            <button onClick={() => submit({ token: null }, "Token removed.")} disabled={locked} className={danger}>
              Clear
            </button>
          )}
        </div>
        {replacing && (
          <div className="flex gap-2">
            <input type="password" value={token} onChange={(e) => setToken(e.target.value)} disabled={locked}
              autoComplete="off" placeholder="new token" aria-label="embed.token" className={`${inputClass} font-mono grow`} />
            <button onClick={() => { setReplacing(false); setToken(""); }} className="text-xs text-muted">cancel</button>
          </div>
        )}
        <span className="text-xs text-muted">Write-only: the dashboard never shows a saved token.</span>
        <FieldErrors errors={errorFor("embed.token")} blocking={errorFor("embed.token")} />
      </div>
      <div className="flex items-center gap-3">
        <button onClick={save} disabled={locked} className={primary}>Save endpoint</button>
        {saved && <span className="text-sm text-accent" role="status">{saved}</span>}
      </div>
    </div>
  );
}

export function EmbeddingSection({
  ctx,
  onEndpoint,
}: {
  ctx: Ctx;
  onEndpoint: (change: EmbedEndpointChange) => Promise<EndpointErrors | null>;
}) {
  const { state } = ctx;
  const backend = state.endpoint.backend;
  const [mode, setMode] = useState<"ollama" | "remote">(backend);
  const [switching, setSwitching] = useState(false);
  useEffect(() => setMode(backend), [backend]);
  const reindexWarning = "embed.model" in ctx.pending || "embed.dimensions" in ctx.pending;
  return (
    <>
      <Card title="Where embeddings come from">
        <Segmented
          options={["ollama", "remote"] as const}
          value={mode}
          onChange={setMode}
          disabled={ctx.locked}
          render={(m) => (m === "ollama" ? "Local Ollama" : "Remote endpoint")}
        />
        {state.endpoint.error && <div className="text-sm text-warn">The env file has a problem: {state.endpoint.error}</div>}
        {mode === "ollama" && backend === "remote" && (
          <div className="bg-sunken rounded-md px-3 py-2.5 flex items-center gap-3 text-sm">
            <span className="grow">A remote endpoint is in use. Switching removes its URL from the env file; the model and token stay.</span>
            <button
              onClick={async () => {
                setSwitching(true);
                await onEndpoint({ url: null });
                setSwitching(false);
              }}
              disabled={ctx.locked || switching || !!overrideFor(state, "endpoint.url")}
              className={secondary}
            >
              Use local Ollama
            </button>
          </div>
        )}
        {mode === "remote" && <RemoteEndpoint state={state} locked={ctx.locked} onChange={onEndpoint} />}
        <TestConnection ctx={ctx} />
      </Card>
      <Card title="Model">
        {reindexWarning && (
          <div className="rounded-md px-3 py-2.5 text-sm bg-[#F6E4DC] text-warn" role="alert" data-testid="reindex-warning">
            Changing the embedding model or its dimensions makes the existing index unreadable: every document has to be
            embedded again with <Code>br8n index --reindex</Code>. You can start that from here after saving.
          </div>
        )}
        {mode === "ollama" && <TextField ctx={ctx} path="embed.ollama_url" label="Ollama URL" />}
        <TextField ctx={ctx} path="embed.model" label="Embedding model"
          hint={mode === "remote" ? "The name the index is stamped with; the endpoint's wire name is set above." : undefined} />
        <NumberField ctx={ctx} path="embed.dimensions" label="Dimensions" />
        <SelectField ctx={ctx} path="embed.prefix_scheme" label="Prefix scheme" options={["qwen3", "nomic", "e5", "plain"]}
          optional="detect from the model" />
      </Card>
      <Card title="Throughput">
        <div className="grid grid-cols-2 gap-x-6 gap-y-4">
          {mode === "ollama" && (
            <Shell ctx={ctx} path="embed.keep_alive" label="Keep alive" hint="Seconds, or an Ollama duration such as 30m.">
              <KeepAliveInput ctx={ctx} />
            </Shell>
          )}
          <NumberField ctx={ctx} path="embed.concurrency" label="Concurrency" />
          <NumberField ctx={ctx} path="embed.batch" label="Batch size" />
          <NumberField ctx={ctx} path="embed.chunk_tokens" label="Chunk tokens" />
        </div>
      </Card>
      <Card title="Contextual enrichment">
        <ToggleField ctx={ctx} path="embed.contextual" label="Enrich chunks with context" />
        <TextField ctx={ctx} path="embed.enrich_model" label="Enrichment model" />
      </Card>
    </>
  );
}

function KeepAliveInput({ ctx }: { ctx: Ctx }) {
  const value = valueOf(ctx.state, ctx.pending, "embed.keep_alive");
  return (
    <input
      value={value === null || value === undefined ? "" : String(value)}
      onChange={(e) => {
        const raw = e.target.value.trim();
        ctx.set("embed.keep_alive", /^-?\d+$/.test(raw) ? Number(raw) : raw);
      }}
      disabled={ctx.locked}
      aria-label="embed.keep_alive"
      className={`${inputClass} font-mono w-[140px]`}
    />
  );
}

export function MemorySection({ ctx }: { ctx: Ctx }) {
  return (
    <>
      <Card title="Memories">
        <ToggleField ctx={ctx} path="memory.enabled" label="Enabled" />
        <div className="grid grid-cols-2 gap-x-6 gap-y-4">
          <NumberField ctx={ctx} path="memory.lessons_max_tokens" label="Lessons token budget" />
          <NumberField ctx={ctx} path="memory.min_confidence" label="Min confidence (0-100)" />
          <NumberField ctx={ctx} path="memory.duplicate_similarity" label="Duplicate similarity (0-1)" />
          <NumberField ctx={ctx} path="memory.max_memories" label="Max memories" />
          <NumberField ctx={ctx} path="memory.episode_half_life_days" label="Episode half-life (days)" />
          <NumberField ctx={ctx} path="memory.episode_decay_floor" label="Episode decay floor (0-1)" />
        </div>
      </Card>
      <Card title="Session distillation">
        <ToggleField ctx={ctx} path="memory.distill_episodes" label="Distill sessions into episodes" />
        <div className="grid grid-cols-2 gap-x-6 gap-y-4">
          <NumberField ctx={ctx} path="memory.distill_after_hours" label="Distill after (hours)" />
          <NumberField ctx={ctx} path="memory.distill_idle_secs" label="Idle before distilling (s)" />
        </div>
        <TextField ctx={ctx} path="memory.distill_model" label="Distillation model" />
      </Card>
    </>
  );
}

export function DocumentsSection({ ctx }: { ctx: Ctx }) {
  return (
    <Card title="PDF and OCR">
      <SelectField ctx={ctx} path="pdf.ocr" label="OCR" options={["auto", "off", "force"]}
        hint="auto recognises only pages that have no text layer." />
      <div className="grid grid-cols-2 gap-x-6 gap-y-4">
        <NumberField ctx={ctx} path="pdf.ocr_min_confidence" label="Min OCR confidence (0-1)" />
        <NumberField ctx={ctx} path="pdf.dpi" label="Render DPI" />
      </div>
      <SelectField ctx={ctx} path="pdf.model_downloads" label="OCR model downloads" options={["if-missing", "offline"]} />
      <TextField ctx={ctx} path="pdf.model_dir" label="OCR model folder" optional />
      <TextField ctx={ctx} path="pdf.pdfium_lib_path" label="PDFium library" optional />
      <TextField ctx={ctx} path="pdf.ort_dylib_path" label="ONNX Runtime library" optional />
    </Card>
  );
}

function TargetsField({ ctx }: { ctx: Ctx }) {
  const targets = stringList(valueOf(ctx.state, ctx.pending, "backup.targets"));
  const flip = (t: string, on: boolean) => ctx.set("backup.targets", on ? [...targets.filter((x) => x !== t), t] : targets.filter((x) => x !== t));
  return (
    <Shell ctx={ctx} path="backup.targets" label="Targets">
      <div className="flex gap-4">
        {["s3", "drive"].map((t) => (
          <label key={t} className="flex items-center gap-1.5 text-sm">
            <input type="checkbox" checked={targets.includes(t)} onChange={(e) => flip(t, e.target.checked)} disabled={ctx.locked} />
            {t === "s3" ? "Amazon S3" : "Google Drive"}
          </label>
        ))}
      </div>
    </Shell>
  );
}

function SecretState({ label, state, hint }: { label: string; state: "set" | "unset"; hint: string }) {
  return (
    <div className="flex items-center gap-2 text-sm">
      <Label>{label}</Label>
      <span className={`text-[11px] font-semibold ${state === "set" ? "text-accent" : "text-muted"}`}>{state === "set" ? "present" : "not present"}</span>
      <span className="text-xs text-muted">{hint}</span>
    </div>
  );
}

export function UpdatesSection({ ctx }: { ctx: Ctx }) {
  const { state } = ctx;
  return (
    <>
      <Card title="Updates">
        <ToggleField ctx={ctx} path="update.check" label="Check for new releases" hint="Checked in the background at session start." />
      </Card>
      <Card title="Backup">
        <ToggleField ctx={ctx} path="backup.enabled" label="Enabled" />
        <TargetsField ctx={ctx} />
        <div className="grid grid-cols-2 gap-x-6 gap-y-4">
          <ToggleField ctx={ctx} path="backup.include_index" label="Include the index" />
          <ToggleField ctx={ctx} path="backup.encrypt" label="Encrypt" />
          <NumberField ctx={ctx} path="backup.keep_generations" label="Keep generations" />
          <NumberField ctx={ctx} path="backup.keep_index" label="Keep index copies" />
        </div>
        <TextField ctx={ctx} path="backup.key_file" label="Key file" optional placeholder="beside the database" />
        <SecretState label="Encryption key" state={state.secrets["backup.key"]} hint="Created by br8n backup; never shown here." />
      </Card>
      <div id={fieldId("backup.s3")}>
        <Card title="Amazon S3">
          <FieldErrors errors={ctx.errors.filter((e) => e.path === "backup.s3")} blocking={ctx.blocking} />
          <div className="grid grid-cols-2 gap-x-6 gap-y-4">
            <TextField ctx={ctx} path="backup.s3.bucket" label="Bucket" />
            <TextField ctx={ctx} path="backup.s3.region" label="Region" />
            <TextField ctx={ctx} path="backup.s3.prefix" label="Prefix" />
            <TextField ctx={ctx} path="backup.s3.profile" label="AWS profile" />
            <TextField ctx={ctx} path="backup.s3.storage_class" label="Storage class" />
          </div>
        </Card>
      </div>
      <div id={fieldId("backup.drive")}>
        <Card title="Google Drive">
          <FieldErrors errors={ctx.errors.filter((e) => e.path === "backup.drive")} blocking={ctx.blocking} />
          <TextField ctx={ctx} path="backup.drive.folder_id" label="Folder id" />
          <TextField ctx={ctx} path="backup.drive.client_secret_file" label="Client secret file" />
          <TextField ctx={ctx} path="backup.drive.token_file" label="Token file" optional placeholder="beside the database" />
          <SecretState label="Drive token" state={state.secrets["backup.drive_token"]} hint="Written by br8n backup auth; never shown here." />
        </Card>
      </div>
    </>
  );
}

const WEIGHT_KEYS = ["markdown", "pdf", "web", "transcript", "memory", "authority", "current", "investigating", "proposed", "superseded"] as const;

export function AdvancedSection({ ctx, onRaw }: { ctx: Ctx; onRaw: (next: Pending) => void }) {
  return (
    <>
      <Card title="Ranking weights">
        <p className="text-sm text-muted">
          Multipliers on a hit's relevance. A surface override replaces the global value for that surface only; leave it empty to
          inherit.
        </p>
        <div className="grid grid-cols-[140px_1fr_1fr_1fr] gap-x-4 gap-y-2 items-start">
          <span />
          <Label>Global</Label>
          <Label>Prompt hook</Label>
          <Label>MCP</Label>
          {WEIGHT_KEYS.map((k) => (
            <WeightRow key={k} ctx={ctx} name={k} />
          ))}
        </div>
      </Card>
      <Card title="Usage decay (transcripts)">
        <ToggleField ctx={ctx} path="weights.decay.enabled" label="Enabled" />
        <div className="grid grid-cols-3 gap-x-6 gap-y-4">
          <NumberField ctx={ctx} path="weights.decay.grace_days" label="Grace (days)" />
          <NumberField ctx={ctx} path="weights.decay.half_life_days" label="Half-life (days)" />
          <NumberField ctx={ctx} path="weights.decay.floor" label="Floor (0-1)" />
        </div>
      </Card>
      <RawToml ctx={ctx} onApply={onRaw} />
    </>
  );
}

function WeightCell({ ctx, path, optional }: { ctx: Ctx; path: string; optional?: boolean }) {
  const inherited = optional && valueOf(ctx.state, ctx.pending, path) == null;
  const changed = path in ctx.pending;
  const errors = ctx.errors.filter((e) => e.path === path);
  return (
    <div id={fieldId(path)} className="flex flex-col gap-0.5 scroll-mt-4">
      <div className="flex items-center gap-1.5">
        <NumberInput ctx={ctx} path={path} optional={optional} className="w-[90px]" />
        {changed && <span className="text-[10px] font-semibold text-accent">edited</span>}
        {inherited && !changed && <span className="text-[10px] text-muted">inherits</span>}
      </div>
      <FieldErrors errors={errors} blocking={ctx.blocking} />
    </div>
  );
}

function WeightRow({ ctx, name }: { ctx: Ctx; name: string }) {
  const fallback = at(ctx.state.defaults, `weights.${name}`);
  return (
    <>
      <span className="text-sm pt-1.5">
        {name} <span className="font-mono text-[11px] text-muted">({String(fallback)})</span>
      </span>
      <WeightCell ctx={ctx} path={`weights.${name}`} />
      <WeightCell ctx={ctx} path={`hook.weights.${name}`} optional />
      <WeightCell ctx={ctx} path={`mcp.weights.${name}`} optional />
    </>
  );
}

function RawToml({ ctx, onApply }: { ctx: Ctx; onApply: (next: Pending) => void }) {
  const file = ctx.state.file;
  const mirror = file === null ? "" : stringifyToml(withPending(file, ctx.pending));
  const [draft, setDraft] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const text = draft ?? mirror;
  const apply = () => {
    if (draft === null || file === null) return;
    let parsed: unknown;
    try {
      parsed = parseToml(draft);
    } catch (e) {
      setProblem(e instanceof Error ? e.message : String(e));
      return;
    }
    if (!isTable(parsed)) return;
    setProblem(null);
    onApply(diff(file, parsed as TomlTable));
    setDraft(null);
  };
  const copy = () => {
    navigator.clipboard?.writeText(text).then(
      () => setCopied(true),
      () => setCopied(false),
    );
    setTimeout(() => setCopied(false), 1500);
  };
  return (
    <div id={fieldId("raw")} className="scroll-mt-4">
      <Card
        title="Raw config.toml"
        aside={
          <button onClick={copy} className={secondary}>
            {copied ? "Copied" : "Copy"}
          </button>
        }
      >
        {file === null ? (
          <p className="text-sm text-warn">
            config.toml does not parse, so it cannot be edited here. Fix it by hand at <Code>{ctx.state.path}</Code>; the error
            is listed at the top of this page.
          </p>
        ) : (
          <>
            <p className="text-sm text-muted">
              The keys this file sets, with your unsaved changes applied. Comments are not shown here and are kept on save.
              Apply turns your edits into unsaved changes; they are checked and saved like any other.
            </p>
            <textarea
              value={text}
              onChange={(e) => setDraft(e.target.value)}
              readOnly={ctx.locked}
              spellCheck={false}
              rows={Math.min(30, Math.max(8, text.split("\n").length + 1))}
              aria-label="raw config.toml"
              className="border border-line rounded-md px-3 py-2 font-mono text-xs bg-surface leading-relaxed resize-y"
            />
            {problem && <span className="text-xs text-warn" role="alert">TOML syntax error: {problem}</span>}
            <div className="flex gap-2">
              <button onClick={apply} disabled={draft === null || ctx.locked} className={primary}>Apply to form</button>
              <button onClick={() => { setDraft(null); setProblem(null); }} disabled={draft === null} className={secondary}>
                Revert
              </button>
            </div>
          </>
        )}
      </Card>
    </div>
  );
}

