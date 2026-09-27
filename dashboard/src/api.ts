export type BenchTier = { tier: string; recall_at_5: number; p50_ms: number; p95_ms: number };
export type Stats = { documents: number; chunks: number; by_source: Record<string, number>; sessions_by_agent?: Record<string, number>; model: string | null; skipped: string[]; bench: BenchTier[] | null; memory: Record<string, number> };
export type Progress = { idle?: boolean; pct?: number; docs_done?: number; docs_total?: number; chunks?: number; eta_s?: number };
export type UpdateStatus = { pid: number; started_at: number; from: string; to: string | null; phase: string; message: string; done: boolean; ok: boolean | null };
export type VersionInfo = { installed: string; latest: string | null; url: string | null; checked_at: number | null; error: string | null; update_available: string | null; update: UpdateStatus | null };
export type HitSummary = { chunk_id: string; doc_id: string; title: string; source_type: string; relevance: number; score: number; heading: string; excerpt: string; memory_kind?: string | null };
/// `fused` is everything that cleared the relevance gate and survived
/// diversity selection; `injected` is the subset of those chunk ids the
/// surface's token budget actually admits — what the prompt hook really
/// sends. They are not the same list: see `Retriever::search_explained`.
export type Explain = { stages: { name: string; hits: HitSummary[] }[]; fused: HitSummary[]; threshold: number; injected: string[]; degraded: boolean; elapsed_ms: number; error?: string };
export type GraphNode = { id: string; title: string; source_type: string; chunks: number; inbound: number; memory_kind?: string; memory_id?: string; agent?: string };
export const AGENT_NAME: Record<string, string> = { "claude-code": "Claude Code", codex: "Codex" };
export type GraphEdge = { from: string; to: string; kind: string };
export type Graph = { nodes: GraphNode[]; entities: { id: string; name: string; kind: string }[]; tags: string[]; edges: GraphEdge[] };
export type MemoryFacts = { kind: string; created: number; project?: string | null; origin: string; confidence: number; session?: string | null };
export type Memory = { id: string; uri: string; doc_id: string; title: string; text: string; facts: MemoryFacts };
export type Memories = { memories: Memory[]; unavailable: string | null };

/// `error` and the fact fields are mutually exclusive: the server answers a
/// missing document with `{ error }` at 200, so a caller must check `error`
/// before reading anything else.
export type DocumentDetail = {
  id: string; uri: string; title: string; source_type: string;
  indexed_at: number; chunks: number; inbound: number;
  status: string | null; lifecycle: string; error?: string;
};

/// Thrown by `j` on a non-OK response. Carries the HTTP status so callers can
/// tell a store hiccup (503, already retried once) apart from anything else —
/// the same distinction the server draws between a transient store error and
/// a stable embedding-outage payload (see `dashboard.rs::search`).
export class ApiError extends Error {
  status: number;
  constructor(path: string, status: number) {
    super(`${path}: ${status}`);
    this.name = "ApiError";
    this.status = status;
  }
}

/// Turns a caught fetch failure into a message worth showing a user. A 503
/// that survived the one retry below means the store is mid-swap or
/// otherwise busy; anything else (network failure, unexpected status) gets a
/// generic message rather than a raw stack of plumbing detail.
export function describeError(e: unknown): string {
  if (e instanceof ApiError && e.status === 503) {
    return "The index is busy — try again in a moment.";
  }
  return "Couldn't reach the dashboard's server — try again.";
}

async function j<T>(path: string, attempts: number = 0): Promise<T> {
  const r = await fetch(path);
  if (r.status === 503) {
    if (attempts < 1) {
      // Store temporarily unavailable during restart: retry once with backoff,
      // then surface the error instead of hanging. Prevents dashboard hangs when a store
      // fails to reopen (e.g., mid-swap rename window).
      await new Promise((res) => setTimeout(res, 500));
      return j<T>(path, attempts + 1);
    }
  }
  if (!r.ok) throw new ApiError(path, r.status);
  return r.json();
}
export const getStats = () => j<Stats>("/api/stats");
export const getProgress = () => j<Progress>("/api/progress");
export const getGraph = () => j<Graph>("/api/graph");
export const search = (q: string, tier?: number) =>
  j<Explain>(`/api/search?q=${encodeURIComponent(q)}${tier != null ? `&tier=${tier}` : ""}`);
export const getDocument = (id: string) =>
  j<DocumentDetail>(`/api/document?id=${encodeURIComponent(id)}`);
export const getVersion = (refresh = false) => j<VersionInfo>(`/api/version${refresh ? "?refresh=1" : ""}`);
export const getMemories = () => j<Memories>("/api/memories");
export type Reach = { reach?: { doc_id: string; title: string; uri: string; relevance: number }[]; error?: string };
export const getReach = (id: string) => j<Reach>(`/api/memory/reach?id=${encodeURIComponent(id)}`);
async function post<T>(path: string, body: unknown): Promise<T> {
  const r = await fetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  const text = await r.text();
  let payload: unknown = null;
  try {
    payload = JSON.parse(text);
  } catch {}
  if (!r.ok) {
    const why = (payload as { error?: string } | null)?.error;
    throw new Error(why ?? `${path}: ${r.status}`);
  }
  return payload as T;
}

export const saveMemory = (m: { id?: string; kind: string; text: string; scope: string; confidence: number }) =>
  post<{ id: string; outcome: string; previous_title?: string }>("/api/memory/save", m);
export const deleteMemory = (id: string) => post<{ id: string; title: string }>("/api/memory/delete", { id });
export type AgentId = "claude-code" | "codex" | "claude-desktop" | "cursor" | "gemini";
export type AgentState = "connected" | "not_connected" | "stale" | "broken";
export type AgentCapabilities = { mcp: boolean; prompt_hook: boolean; session_hook: boolean; transcripts: boolean; instructions: boolean };
export type AgentDetected = { installed: boolean; version: string | null; config_path: string | null };
export type Agent = {
  id: AgentId;
  name: string;
  detected: AgentDetected;
  status: { state: AgentState; reason?: string };
  capabilities: AgentCapabilities;
  snippet: string | null;
  instructions?: boolean;
};
export type AgentSnippets = { mcp_json: string; codex_toml: string };
export type Agents = { agents: Agent[]; snippets: AgentSnippets };
export type AgentChange = { files: string[]; backups: string[]; notes: string[] };
export type AgentOutcome = { agent: Agent; change: AgentChange };
export const getAgents = () => j<Agents>("/api/agents");
export type AgentResult = { kind: "done"; outcome: AgentOutcome } | { kind: "refused"; status: number; error: string };
async function agentAction(path: string, body: unknown): Promise<AgentResult> {
  const { status, payload } = await postForStatus(path, body);
  if (status === 200) return { kind: "done", outcome: payload as AgentOutcome };
  return { kind: "refused", status, error: (payload as { error?: string } | null)?.error ?? `${path}: ${status}` };
}
export const connectAgent = (id: AgentId, instructions?: boolean) =>
  agentAction("/api/agents/connect", instructions ? { id, instructions } : { id });
export const disconnectAgent = (id: AgentId) => agentAction("/api/agents/disconnect", { id });
export type InstallFix =
  | { kind: "connect"; agent: AgentId }
  | { kind: "command"; command: string }
  | { kind: "manual"; text: string };
export type InstallCheck = { name: string; ok: boolean; detail: string; fix: InstallFix | null };
export type InstallState = {
  root: string; binary: string; exe: string;
  checks: InstallCheck[];
  config: { path: string; errors: FieldError[] };
  embed: { backend: "ollama" | "remote"; url: string | null; model: string | null };
};
export const getInstall = () => j<InstallState>("/api/install");
export async function startUpdate(): Promise<void> {
  const r = await fetch("/api/update", { method: "POST" });
  if (r.ok) return;
  let reason = `${r.status}`;
  try {
    reason = ((await r.json()) as { error?: string }).error ?? reason;
  } catch {}
  throw new Error(reason);
}
export type TomlValue = string | number | boolean | TomlValue[] | { [key: string]: TomlValue };
export type TomlTable = { [key: string]: TomlValue };

/// `line` is 1-based and points into config.toml; it is null when the problem
/// has no single place in the file (a missing table, for example). `path` is
/// the dotted key, or "" for a syntax error.
export type FieldError = { path: string; message: string; line: number | null };

export type QualityTier = 0 | 1 | 2 | 3 | 4;
export type WeightOverrides = {
  authority: number | null; markdown: number | null; pdf: number | null; web: number | null;
  transcript: number | null; memory: number | null; current: number | null;
  investigating: number | null; proposed: number | null; superseded: number | null;
};
export type SurfaceSettings = { quality: QualityTier | null; threshold: number; max_tokens: number; weights: WeightOverrides };
export type EmbedSettings = {
  model: string; dimensions: number; ollama_url: string; keep_alive: number | string;
  concurrency: number; batch: number; chunk_tokens: number;
  prefix_scheme: "qwen3" | "nomic" | "e5" | "plain" | null;
  contextual: boolean; enrich_model: string;
};
export type DecaySettings = { enabled: boolean; grace_days: number; half_life_days: number; floor: number };
export type WeightSettings = {
  markdown: number; pdf: number; web: number; transcript: number; memory: number; authority: number;
  current: number; investigating: number; proposed: number; superseded: number; decay: DecaySettings;
};
export type PdfSettings = {
  ocr: "auto" | "off" | "force"; ocr_min_confidence: number; dpi: number; model_dir: string | null;
  model_downloads: "if-missing" | "offline"; pdfium_lib_path: string | null; ort_dylib_path: string | null;
};
export type MemorySettings = {
  enabled: boolean; lessons_max_tokens: number; min_confidence: number; duplicate_similarity: number;
  episode_half_life_days: number; episode_decay_floor: number; distill_episodes: boolean;
  distill_after_hours: number; distill_idle_secs: number; distill_model: string; max_memories: number;
};
export type S3Settings = { bucket: string; region: string; prefix: string; profile: string; storage_class: string };
export type DriveSettings = { folder_id: string; client_secret_file: string; token_file: string | null };
export type BackupSettings = {
  enabled: boolean; targets: string[]; include_index: boolean; encrypt: boolean; key_file: string | null;
  keep_generations: number; keep_index: number; s3: S3Settings | null; drive: DriveSettings | null;
};
/// The whole config with every default filled in, as `GET /api/config`
/// returns it in `effective` and `defaults`. Dotted paths into this shape are
/// the keys of a `ConfigPatch`.
export type Settings = {
  index_transcripts: boolean; index_codex_sessions: boolean; index_transcripts_max_age_days: number | null; ignore: string[];
  hook: SurfaceSettings; mcp: SurfaceSettings; embed: EmbedSettings; sources: string[];
  weights: WeightSettings; pdf: PdfSettings; memory: MemorySettings; update: { check: boolean }; backup: BackupSettings;
};

export type SecretState = "set" | "unset";
export type EnvVariable = "BR8N_EMBED_URL" | "BR8N_EMBED_MODEL" | "BR8N_EMBED_TOKEN";
/// `setting` names either a field of `ConfigState.endpoint` or a key of
/// `ConfigState.secrets`. A value that comes from the process environment
/// wins over the env file, so the dashboard cannot change it.
export type EnvOverride = { variable: EnvVariable; setting: "endpoint.url" | "endpoint.model" | "embed.token" };
/// The remote embedding endpoint from the env file beside config.toml (or the
/// process environment). The token is never returned; see `secrets`.
export type EmbedEndpoint = { backend: "ollama" | "remote"; url: string | null; model: string | null; error: string | null };

/// `file` is only what config.toml itself sets: `{}` when there is no file,
/// null when it does not parse. `effective` is what br8n runs with; a key in
/// `effective` that is absent from `file` is at its default. `etag` is the
/// sha256 of the file ("" when there is none) and must be sent back on save.
export type ConfigState = {
  path: string; exists: boolean; etag: string;
  effective: Settings; file: TomlTable | null; defaults: Settings;
  errors: FieldError[];
  secrets: { "embed.token": SecretState; "backup.key": SecretState; "backup.drive_token": SecretState };
  env_overrides: EnvOverride[];
  endpoint: EmbedEndpoint;
};

/// Keys are dotted paths (`"hook.threshold"`). A null in `set` removes the key,
/// the same as listing it in `unset`, so its default applies again.
export type ConfigPatch = { set?: Record<string, TomlValue | null>; unset?: string[] };
/// `errors` is everything wrong with the patched file; `blocking` is the part a
/// save would refuse, which leaves out problems the file already had.
export type ConfigCheck = { errors: FieldError[]; blocking: FieldError[] };
export type ConfigSaveResult =
  | { kind: "saved"; state: ConfigState }
  | { kind: "conflict"; etag: string; error: string }
  | { kind: "invalid"; errors: FieldError[]; error: string };
/// A field that is absent is left alone; null removes it from the env file.
export type EmbedEndpointChange = { url?: string | null; model?: string | null; token?: string | null };
export type EmbedEndpointResult =
  | { kind: "saved"; state: ConfigState }
  | { kind: "invalid"; errors: FieldError[]; error: string };
export type EmbedTest = {
  ok: boolean; latency_ms: number; dimensions: number | null;
  backend: "ollama" | "remote"; url: string | null; model: string | null; error?: string;
};

async function postForStatus(path: string, body: unknown): Promise<{ status: number; payload: unknown }> {
  const r = await fetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  let payload: unknown = null;
  try {
    payload = await r.json();
  } catch {}
  return { status: r.status, payload };
}

function failure(path: string, status: number, payload: unknown): Error {
  return new Error((payload as { error?: string } | null)?.error ?? `${path}: ${status}`);
}

export const getConfig = () => j<ConfigState>("/api/config");
export async function saveConfig(etag: string, patch: ConfigPatch): Promise<ConfigSaveResult> {
  const { status, payload } = await postForStatus("/api/config", { etag, ...patch });
  if (status === 200) return { kind: "saved", state: payload as ConfigState };
  const p = payload as { error?: string; etag?: string; errors?: FieldError[] } | null;
  if (status === 409 && typeof p?.etag === "string") return { kind: "conflict", etag: p.etag, error: p.error ?? "" };
  if (status === 422 && p?.errors) return { kind: "invalid", errors: p.errors, error: p.error ?? "" };
  throw failure("/api/config", status, payload);
}
export async function checkConfig(patch: ConfigPatch): Promise<ConfigCheck> {
  const { status, payload } = await postForStatus("/api/config/check", patch);
  if (status === 200) return payload as ConfigCheck;
  throw failure("/api/config/check", status, payload);
}
export async function setEmbedEndpoint(change: EmbedEndpointChange): Promise<EmbedEndpointResult> {
  const { status, payload } = await postForStatus("/api/config/embed", change);
  if (status === 200) return { kind: "saved", state: payload as ConfigState };
  const p = payload as { error?: string; errors?: FieldError[] } | null;
  if (status === 422 && p?.errors) return { kind: "invalid", errors: p.errors, error: p.error ?? "" };
  throw failure("/api/config/embed", status, payload);
}
export async function testEmbedding(): Promise<EmbedTest> {
  const { status, payload } = await postForStatus("/api/embed/test", {});
  if (status === 200) return payload as EmbedTest;
  throw failure("/api/embed/test", status, payload);
}
export type IndexStart = { kind: "started"; pid: number; reindex: boolean } | { kind: "busy"; error: string };
export async function startIndex(reindex = false): Promise<IndexStart> {
  const { status, payload } = await postForStatus("/api/index", { reindex });
  const p = payload as { pid?: number; reindex?: boolean; error?: string } | null;
  if (status === 202) return { kind: "started", pid: p?.pid ?? 0, reindex: p?.reindex ?? reindex };
  if (status === 409) return { kind: "busy", error: p?.error ?? "an index is already running" };
  throw failure("/api/index", status, payload);
}
export type IndexRun = { pid: number; reindex: boolean; finished: boolean; ok: boolean | null; code: number | null; log: string };
export const getIndexRun = () => j<{ run: IndexRun | null }>("/api/index");

export const PALETTE = {
  note: "#2E9E78", session: "#A86FC9", tag: "#CC7A3E", entity: "#4E92CF",
  memory: "#C2557A", project: "#7A8B99",
  accent: "#0E6B58", ink: "#16211D", muted: "#657771", line: "#D3DCD8", viewport: "#0E1A16",
};
