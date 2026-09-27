import { useEffect, useState } from "react";
import { describeError, getDocument, type DocumentDetail } from "./api";

/// The document facts panel, shared by the graph node panel and an expanded
/// search result. Fetches on mount and whenever `id` changes.
export function DocumentFacts({ id }: { id: string }) {
  const [d, setD] = useState<DocumentDetail | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    setD(null);
    setFailure(null);
    // `live` guards the fast-click case: selecting three nodes in a row starts
    // three fetches, and without this the FIRST to resolve can be the LAST to
    // arrive and paint another document's facts under the current selection.
    // `describeError` (api.ts) exists precisely so a caller can tell a 503
    // (the store mid-swap — transient, worth a retry) from an unreachable
    // server; collapsing both to one generic string told a user hitting a
    // re-index that their details were unloadable when the honest answer was
    // "try again in a moment".
    getDocument(id).then((r) => live && setD(r)).catch((e) => live && setFailure(describeError(e)));
    return () => { live = false; };
  }, [id]);

  if (failure) return <div className="text-[11px] text-muted">{failure}</div>;
  if (!d) return <div className="text-[11px] text-muted">Loading…</div>;
  if (d.error) return <div className="text-[11px] text-muted">{d.error}</div>;
  return (
    <dl className="text-[11px] grid grid-cols-[auto_1fr] gap-x-2 gap-y-1">
      <dt className="text-muted">Path</dt>
      <dd className="font-mono truncate" title={d.uri}>{d.uri}</dd>
      <dt className="text-muted">Indexed</dt>
      <dd>{new Date(d.indexed_at * 1000).toLocaleString()}</dd>
      <dt className="text-muted">Status</dt>
      <dd>
        {d.lifecycle}
        {/* The vault's own word, shown only when it differs from the derived
            rung — that is exactly the `proposed` note promoted to
            `Investigating` by an inbound link, and without this the panel
            cannot say why. */}
        {d.status && d.status.toLowerCase() !== d.lifecycle.toLowerCase() && (
          <span className="text-muted"> (vault says “{d.status}”)</span>
        )}
      </dd>
      <dt className="text-muted">Size</dt>
      <dd>{d.chunks} chunks · {d.inbound} inbound</dd>
    </dl>
  );
}
