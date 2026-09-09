/**
 * Audit: newest-first page of normalized audit rows. "Load older" is the only
 * paging affordance: one explicit user click fetches exactly one more bounded
 * page behind a strict `before` cursor; nothing polls or loops. Every row
 * shows the server's normalized fields: actor, resource, and outcome.
 */

import { useCallback, useEffect, useState } from "react";
import { listAudit } from "../api/api.ts";
import type { AuditEntryDto } from "../api/dto.ts";
import { useResource } from "../hooks.ts";
import { Badge, Card, PageHeader, StateView } from "../ui/primitives.tsx";

function shortId(id: string): string {
  return id.length === 0 ? "—" : id.length <= 10 ? id : `${id.slice(0, 8)}…`;
}

/** Outcome tone; unknown labels render honestly as warnings. */
function outcomeTone(outcome: string): "ok" | "fail" | "warn" | "neutral" {
  if (outcome === "ok") return "ok";
  if (outcome === "error") return "fail";
  if (outcome === "refused" || outcome === "unknown") return "warn";
  return "neutral";
}

export function Audit() {
  const load = useCallback((signal: AbortSignal) => listAudit(undefined, signal), []);
  const resource = useResource(load);
  const base = resource.data;

  // Entries accumulated through explicit "Load older" clicks.
  const [older, setOlder] = useState<AuditEntryDto[]>([]);
  const [hasMoreOverride, setHasMoreOverride] = useState<boolean | null>(null);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [olderError, setOlderError] = useState<string | null>(null);

  // A fresh base page (initial load or retry) restarts the walk.
  useEffect(() => {
    setOlder([]);
    setHasMoreOverride(null);
    setOlderError(null);
  }, [base]);

  const seen = new Set<number>();
  const entries: AuditEntryDto[] = [];
  for (const entry of [...(base?.entries ?? []), ...older]) {
    if (seen.has(entry.seq)) continue;
    seen.add(entry.seq);
    entries.push(entry);
  }

  const hasMore = hasMoreOverride ?? base?.hasMore ?? false;
  const oldestSeq = entries.length > 0 ? (entries[entries.length - 1]?.seq ?? null) : null;

  const loadOlder = useCallback(async (): Promise<void> => {
    if (oldestSeq === null || !hasMore || loadingOlder) return;
    setLoadingOlder(true);
    setOlderError(null);
    try {
      const page = await listAudit(oldestSeq);
      const nextEntries = page.entries.filter((entry) => entry.seq < oldestSeq);
      setOlder((previous) => [
        ...previous,
        ...nextEntries,
      ]);
      // Stop if a malformed/inclusive page makes no cursor progress.
      setHasMoreOverride(page.hasMore && nextEntries.length > 0);
    } catch (error: unknown) {
      setOlderError(error instanceof Error ? error.message : "request failed");
    } finally {
      setLoadingOlder(false);
    }
  }, [hasMore, loadingOlder, oldestSeq]);

  const header = (
    <PageHeader title="Audit" subtitle="Control-plane audit trail, most recent first." />
  );

  if (resource.loading) {
    return (
      <>
        {header}
        <StateView state="loading" title="Loading audit trail" />
      </>
    );
  }
  if (resource.error !== null || base === null) {
    return (
      <>
        {header}
        <StateView
          state="error"
          title="Could not load the audit trail"
          detail={resource.error?.message ?? "request failed"}
          onRetry={resource.reload}
        />
      </>
    );
  }

  return (
    <>
      {header}
      {entries.length === 0 ? (
        <StateView
          state="empty"
          title="No audit entries"
          detail="Recorded control-plane actions appear here."
        />
      ) : (
        <Card title="Audit trail" bodyClass="kds-flush">
          <table className="table">
            <thead>
              <tr>
                <th scope="col">Seq</th>
                <th scope="col">Time</th>
                <th scope="col">Kind</th>
                <th scope="col">Actor</th>
                <th scope="col">Resource</th>
                <th scope="col">Outcome</th>
              </tr>
            </thead>
            <tbody>
              {entries.map((entry) => (
                <tr key={entry.seq}>
                  <td className="mono">{entry.seq}</td>
                  <td className="kds-datetime">
                    {entry.occurredAt === null || entry.occurredAt.trim() === ""
                      ? "—"
                      : entry.occurredAt}
                  </td>
                  <td className="mono">{entry.kind.trim() === "" ? "—" : entry.kind}</td>
                  <td className="mono" title={entry.actorUserId ?? undefined}>
                    {entry.actorUserId === null ? (
                      <span className="muted">system</span>
                    ) : (
                      shortId(entry.actorUserId)
                    )}
                  </td>
                  <td>
                    {entry.resourceType.trim() === "" ? (
                      <span className="muted">—</span>
                    ) : (
                      <span className="mono" title={entry.resourceId ?? undefined}>
                        {entry.resourceType} {entry.resourceId === null ? "" : shortId(entry.resourceId)}
                      </span>
                    )}
                  </td>
                  <td>
                    <Badge tone={outcomeTone(entry.outcome)}>
                      {entry.outcome.trim() === "" ? "—" : entry.outcome}
                    </Badge>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </Card>
      )}

      {olderError !== null ? (
        <p role="alert" className="muted">
          Loading older entries failed: {olderError}
        </p>
      ) : null}
      {hasMore ? (
        <div className="actions">
          <button type="button" className="btn" disabled={loadingOlder} onClick={() => void loadOlder()}>
            {loadingOlder ? "Loading…" : "Load older"}
          </button>
        </div>
      ) : (
        <p className="muted">End of the loaded range.</p>
      )}
    </>
  );
}
