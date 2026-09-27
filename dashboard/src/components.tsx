import React from "react";

/// The one error banner. All three tabs can fail, and all three failed in their
/// own markup — four copies of the same class string, which is three chances
/// for the tabs to disagree about what a failure looks like after a palette
/// change.
export function ErrorBanner({ children }: { children: React.ReactNode }) {
  return <div className="bg-[#F6E4DC] text-warn rounded-md px-4 py-2.5 text-sm">{children}</div>;
}

/// A labelled dashed rule marking where a cut falls inside a column — the gate
/// in the fused column, the token budget in the injected one. Both are the same
/// statement ("everything below here did not make it"), so they are one shape.
export function Divider({ label }: { label: string }) {
  return (
    <div className="flex items-center gap-2 py-0.5">
      <div className="grow border-t-2 border-dashed border-warn" />
      <span className="font-mono text-[11px] font-medium text-warn">{label}</span>
      <div className="grow border-t-2 border-dashed border-warn" />
    </div>
  );
}
