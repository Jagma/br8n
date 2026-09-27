import type { ConfigPatch, ConfigState, FieldError, TomlTable, TomlValue } from "../api";

export type Pending = Record<string, TomlValue | null>;

export const TIERS = ["instant", "fast", "balanced", "thorough", "exhaustive"] as const;

export const SURFACE_DEFAULT_TIER: Record<"hook" | "mcp", number> = { hook: 1, mcp: 3 };

export const SECTIONS = ["Sources", "Retrieval", "Embedding", "Memory", "Documents", "Updates & backup", "Advanced"] as const;
export type Section = (typeof SECTIONS)[number];

export function isTable(v: unknown): v is TomlTable {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

export function at(root: unknown, path: string): unknown {
  let node: unknown = root;
  for (const segment of path.split(".")) {
    if (!isTable(node) || !(segment in node)) return undefined;
    node = node[segment];
  }
  return node;
}

function canonical(v: unknown): unknown {
  if (Array.isArray(v)) return v.map(canonical);
  if (isTable(v)) return Object.fromEntries(Object.keys(v).sort().map((k) => [k, canonical(v[k])]));
  return v;
}

export function same(a: unknown, b: unknown): boolean {
  return JSON.stringify(canonical(a)) === JSON.stringify(canonical(b));
}

function clone<T>(v: T): T {
  return JSON.parse(JSON.stringify(v)) as T;
}

export function withPending(file: TomlTable, pending: Pending): TomlTable {
  const out = clone(file);
  for (const [path, value] of Object.entries(pending)) {
    const parts = path.split(".");
    const leaf = parts.pop() as string;
    let node: TomlTable = out;
    let missing = false;
    for (const segment of parts) {
      if (!isTable(node[segment])) {
        if (value === null) {
          missing = true;
          break;
        }
        node[segment] = {};
      }
      node = node[segment] as TomlTable;
    }
    if (missing) continue;
    if (value === null) delete node[leaf];
    else node[leaf] = clone(value);
  }
  return out;
}

export function diff(before: TomlTable, after: TomlTable, prefix = "", out: Pending = {}): Pending {
  for (const [key, value] of Object.entries(after)) {
    const path = prefix ? `${prefix}.${key}` : key;
    const old = before[key];
    if (isTable(value) && isTable(old)) diff(old, value, path, out);
    else if (!same(value, old)) out[path] = value;
  }
  for (const key of Object.keys(before)) {
    if (!(key in after)) out[prefix ? `${prefix}.${key}` : key] = null;
  }
  return out;
}

export function toPatch(pending: Pending): ConfigPatch {
  const set: Record<string, TomlValue> = {};
  const unset: string[] = [];
  for (const [path, value] of Object.entries(pending)) {
    if (value === null) unset.push(path);
    else set[path] = value;
  }
  return { set, unset };
}

export function inFile(state: ConfigState, path: string): boolean {
  return state.file !== null && at(state.file, path) !== undefined;
}

export function original(state: ConfigState, path: string): unknown {
  if (state.file === null) return at(state.effective, path);
  const v = at(state.file, path);
  return v === undefined ? at(state.defaults, path) : v;
}

export function defaultOf(state: ConfigState, path: string): unknown {
  return at(state.defaults, path);
}

export function valueOf(state: ConfigState, pending: Pending, path: string): unknown {
  if (path in pending) {
    const v = pending[path];
    return v === null ? defaultOf(state, path) : v;
  }
  return original(state, path);
}

export function isDefault(state: ConfigState, pending: Pending, path: string): boolean {
  if (path in pending) return pending[path] === null;
  return !inFile(state, path);
}

export function change(state: ConfigState, pending: Pending, path: string, value: TomlValue | null): Pending {
  const next = { ...pending };
  if (value === null) {
    if (inFile(state, path)) next[path] = null;
    else delete next[path];
    return next;
  }
  if (inFile(state, path) && same(value, at(state.file, path))) delete next[path];
  else next[path] = value;
  return next;
}

export function describe(v: unknown): string {
  if (v === undefined) return "unset";
  if (v === null) return "none";
  if (typeof v === "string") return JSON.stringify(v);
  return JSON.stringify(v);
}

export function sectionOf(path: string): Section {
  const head = path.split(".")[0];
  if (/^(hook|mcp)\.weights/.test(path) || head === "weights") return "Advanced";
  if (head === "sources" || head === "ignore" || head.startsWith("index_transcripts") || head === "index_codex_sessions") return "Sources";
  if (head === "hook" || head === "mcp") return "Retrieval";
  if (head === "embed" || head === "endpoint") return "Embedding";
  if (head === "memory") return "Memory";
  if (head === "pdf") return "Documents";
  if (head === "update" || head === "backup") return "Updates & backup";
  return "Advanced";
}

export function fieldId(path: string): string {
  return `field-${path.replace(/[^a-z0-9_]+/gi, "-")}`;
}

export function errorsFor(errors: FieldError[], path: string): FieldError[] {
  return errors.filter((e) => e.path === path || e.path.startsWith(`${path}.`));
}

export function needsReindex(pending: Pending): boolean {
  return "embed.model" in pending || "embed.dimensions" in pending;
}
