import React, { useEffect, useState } from "react";
import type { ConfigState, FieldError, TomlValue } from "../api";
import { defaultOf, describe, errorsFor, fieldId, isDefault, Pending, valueOf } from "./model";

export type Ctx = {
  state: ConfigState;
  pending: Pending;
  errors: FieldError[];
  blocking: FieldError[];
  locked: boolean;
  set: (path: string, value: TomlValue | null) => void;
};

export const inputClass =
  "border border-line rounded-md px-2.5 py-1.5 text-sm bg-surface disabled:opacity-50 disabled:cursor-not-allowed";

export function Card({ title, children, aside }: { title: string; children: React.ReactNode; aside?: React.ReactNode }) {
  return (
    <section className="bg-surface border border-line rounded-lg p-5 flex flex-col gap-4">
      <div className="flex items-center gap-3">
        <h3 className="text-sm font-semibold grow">{title}</h3>
        {aside}
      </div>
      {children}
    </section>
  );
}

export function Label({ children }: { children: React.ReactNode }) {
  return <span className="text-[11px] font-semibold uppercase tracking-wider text-muted">{children}</span>;
}

export function FieldErrors({ errors, blocking }: { errors: FieldError[]; blocking: FieldError[] }) {
  if (errors.length === 0) return null;
  return (
    <div className="flex flex-col gap-0.5">
      {errors.map((e) => {
        const introduced = blocking.some((b) => b.path === e.path && b.message === e.message);
        return (
          <span key={`${e.path}:${e.message}`} className="text-xs text-warn" role="alert">
            {e.message}
            {!introduced && <span className="text-muted"> (already in config.toml)</span>}
          </span>
        );
      })}
    </div>
  );
}

export function Shell({
  ctx,
  path,
  label,
  hint,
  override,
  children,
  showDefault = true,
}: {
  ctx: Ctx;
  path: string;
  label: string;
  hint?: React.ReactNode;
  override?: string | null;
  children: React.ReactNode;
  showDefault?: boolean;
}) {
  const atDefault = isDefault(ctx.state, ctx.pending, path);
  const changed = path in ctx.pending;
  const errors = errorsFor(ctx.errors, path);
  return (
    <div id={fieldId(path)} className="flex flex-col gap-1.5 scroll-mt-4" data-path={path}>
      <div className="flex items-center gap-2 min-h-[20px]">
        <Label>{label}</Label>
        {changed && <span className="text-[10px] font-semibold text-accent">edited</span>}
        {atDefault && !changed && <span className="text-[10px] text-muted bg-sunken rounded px-1.5">default</span>}
        {override && <span className="text-[10px] font-semibold text-warn">overridden by ${override}</span>}
        <div className="grow" />
        {showDefault && (
          <span className="font-mono text-[11px] text-muted truncate max-w-[260px]" title={describe(defaultOf(ctx.state, path))}>
            default {describe(defaultOf(ctx.state, path))}
          </span>
        )}
        {!atDefault && !override && (
          <button
            onClick={() => ctx.set(path, null)}
            disabled={ctx.locked}
            className="text-[11px] font-semibold text-accent underline disabled:opacity-50 disabled:cursor-not-allowed"
          >
            reset to default
          </button>
        )}
      </div>
      {children}
      {hint && <span className="text-xs text-muted">{hint}</span>}
      <FieldErrors errors={errors} blocking={ctx.blocking} />
    </div>
  );
}

function numberText(v: unknown): string {
  return v === null || v === undefined ? "" : String(v);
}

export function NumberInput({
  ctx,
  path,
  optional,
  disabled,
  className,
}: {
  ctx: Ctx;
  path: string;
  optional?: boolean;
  disabled?: boolean;
  className?: string;
}) {
  const value = valueOf(ctx.state, ctx.pending, path);
  const [text, setText] = useState(numberText(value));
  useEffect(() => {
    const parsed = text.trim() === "" ? null : Number(text);
    if (parsed !== value && !(typeof value === "string" && value === text)) setText(numberText(value));
  }, [value]);
  const push = (raw: string) => {
    setText(raw);
    const trimmed = raw.trim();
    if (trimmed === "") return ctx.set(path, optional ? null : "");
    const n = Number(trimmed);
    ctx.set(path, Number.isFinite(n) ? n : trimmed);
  };
  return (
    <input
      type="text"
      inputMode="decimal"
      value={text}
      onChange={(e) => push(e.target.value)}
      disabled={ctx.locked || disabled}
      placeholder={optional ? "not set" : undefined}
      aria-label={path}
      className={`${inputClass} font-mono ${className ?? "w-[140px]"}`}
    />
  );
}

export function NumberField(props: {
  ctx: Ctx;
  path: string;
  label: string;
  hint?: React.ReactNode;
  optional?: boolean;
}) {
  return (
    <Shell ctx={props.ctx} path={props.path} label={props.label} hint={props.hint}>
      <NumberInput {...props} />
    </Shell>
  );
}

export function TextField({
  ctx,
  path,
  label,
  hint,
  optional,
  placeholder,
  override,
  mono = true,
}: {
  ctx: Ctx;
  path: string;
  label: string;
  hint?: React.ReactNode;
  optional?: boolean;
  placeholder?: string;
  override?: string | null;
  mono?: boolean;
}) {
  const value = valueOf(ctx.state, ctx.pending, path);
  return (
    <Shell ctx={ctx} path={path} label={label} hint={hint} override={override}>
      <input
        type="text"
        value={value === null || value === undefined ? "" : String(value)}
        onChange={(e) => ctx.set(path, optional && e.target.value === "" ? null : e.target.value)}
        disabled={ctx.locked || !!override}
        placeholder={placeholder ?? (optional ? "not set" : undefined)}
        aria-label={path}
        className={`${inputClass} ${mono ? "font-mono" : ""}`}
      />
    </Shell>
  );
}

export function Toggle({
  checked,
  onChange,
  disabled,
  label,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
  label: string;
}) {
  return (
    <button
      role="switch"
      aria-checked={checked}
      aria-label={label}
      onClick={() => onChange(!checked)}
      disabled={disabled}
      className={`w-9 h-5 rounded-full relative transition-colors disabled:opacity-50 disabled:cursor-not-allowed ${checked ? "bg-accent" : "bg-line"}`}
    >
      <span
        className={`absolute top-0.5 w-4 h-4 rounded-full bg-surface shadow transition-all ${checked ? "left-[18px]" : "left-0.5"}`}
      />
    </button>
  );
}

export function ToggleField({ ctx, path, label, hint }: { ctx: Ctx; path: string; label: string; hint?: React.ReactNode }) {
  const value = valueOf(ctx.state, ctx.pending, path) === true;
  return (
    <Shell ctx={ctx} path={path} label={label} hint={hint}>
      <div className="flex items-center gap-2.5">
        <Toggle checked={value} onChange={(v) => ctx.set(path, v)} disabled={ctx.locked} label={path} />
        <span className="text-sm">{value ? "on" : "off"}</span>
      </div>
    </Shell>
  );
}

export function SelectField({
  ctx,
  path,
  label,
  hint,
  options,
  optional,
}: {
  ctx: Ctx;
  path: string;
  label: string;
  hint?: React.ReactNode;
  options: readonly string[];
  optional?: string;
}) {
  const value = valueOf(ctx.state, ctx.pending, path);
  return (
    <Shell ctx={ctx} path={path} label={label} hint={hint}>
      <select
        value={value === null || value === undefined ? "" : String(value)}
        onChange={(e) => ctx.set(path, e.target.value === "" ? null : e.target.value)}
        disabled={ctx.locked}
        aria-label={path}
        className={`${inputClass} w-[220px]`}
      >
        {optional !== undefined && <option value="">{optional}</option>}
        {options.map((o) => (
          <option key={o} value={o}>{o}</option>
        ))}
      </select>
    </Shell>
  );
}

export function Segmented<T extends string | number>({
  options,
  value,
  onChange,
  disabled,
  render,
}: {
  options: readonly T[];
  value: T;
  onChange: (v: T) => void;
  disabled?: boolean;
  render: (v: T) => React.ReactNode;
}) {
  return (
    <div className="flex gap-0.5 bg-sunken rounded-md p-0.5 w-fit" role="radiogroup">
      {options.map((o) => (
        <button
          key={String(o)}
          role="radio"
          aria-checked={o === value}
          onClick={() => onChange(o)}
          disabled={disabled}
          className={`px-3 py-1.5 text-xs rounded disabled:cursor-not-allowed ${o === value ? "font-semibold text-accent bg-surface shadow-sm" : "text-muted"}`}
        >
          {render(o)}
        </button>
      ))}
    </div>
  );
}
