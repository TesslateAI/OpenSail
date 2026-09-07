/**
 * System Audit: the platform-wide audit trail through the admin adapter.
 *
 * Rows come from GET /api/admin/audit, an ascending `after`-cursor walk with
 * a bounded page limit (ADMIN_AUDIT_PAGE_LIMIT). "Load more" is the only
 * paging affordance: one explicit click fetches exactly one more bounded
 * page past the last emitted seq; nothing polls. Humanized labels (actor
 * display name, Project name) are applied where the platform directory can
 * resolve them; raw ids stay visible only in tooltips.
 */

import { useCallback, useEffect, useState } from "react";
import { adminApi, ADMIN_AUDIT_PAGE_LIMIT, type AdminApi } from "../api/admin.ts";
import type { AuditEntryDto } from "../api/dto.ts";
import { useResource } from "../hooks.ts";
import { Badge, PageHeader, StateView } from "../ui/primitives.tsx";

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

type AdminSystemAuditProps = { api?: AdminApi | undefined };

export function AdminSystemAudit({ api = adminApi }: AdminSystemAuditProps) {
  // First page: ascending from the newest emitted seq, bounded by the limit.
  const load = useCallback((signal: AbortSignal) => api.getAdminAudit(undefined, signal), [api]);
  const resource = useResource(load);

  // Humanized actor labels: the platform user directory (displayName or
  // username) when resolvable; raw ids stay tooltip-only otherwise.
  const usersLoad = useCallback((signal: AbortSignal) => api.listUsers(signal), [api]);
  const users = useResource(usersLoad);
  const userLabel = useCallback(
    (id: string | null): string | null => {
      if (id === null) return null;
      const row = (users.data ?? []).find((user) => user.id === id);
      if (row === undefined) return null;
      if (row.displayName.trim() !== "") return row.displayName;
      return row.username;
    },
    [users.data],
  );

  // Humanized Project labels: the platform-wide Project listing when resolvable.
  const projectsLoad = useCallback((signal: AbortSignal) => api.getAdminProjects(signal), [api]);
  const projects = useResource(projectsLoad);
  const projectLabel = useCallback(
    (id: string | null): string | null => {
      if (id === null) return null;
      const row = (projects.data ?? []).find((project) => project.id === id);
      return row === undefined ? null : row.name;
    },
    [projects.data],
  );

  // Entries accumulated through explicit "Load more" clicks.
  const [more, setMore] = useState<AuditEntryDto[]>([]);
  const [nextAfter, setNextAfter] = useState<number | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);
  const [moreError, setMoreError] = useState<string | null>(null);

  // A fresh base page (initial load or retry) restarts the walk.
  useEffect(() => {
    setMore([]);
    setNextAfter(null);
    setMoreError(null);
  }, [resource.data]);

  const entries: AuditEntryDto[] = [...(resource.data?.entries ?? []), ...more];

  const loadMore = useCallback(async (): Promise<void> => {
    if (nextAfter === null || loadingMore) return;
    setLoadingMore(true);
    setMoreError(null);
    try {
      const page = await api.getAdminAudit(nextAfter);
      // Append only rows strictly past the cursor: monotonic seq, no dups.
      const fresh = page.entries.filter((entry) => entry.seq > (nextAfter ?? -1));
      setMore((previous) => [...previous, ...fresh]);
      setNextAfter(page.nextAfter);
      // Stop if a malformed/inclusive page makes no cursor progress.
      if (fresh.length === 0 || page.nextAfter === null) setNextAfter(null);
    } catch (reason: unknown) {
      setMoreError(reason instanceof Error ? reason.message : "request failed");
    } finally {
      setLoadingMore(false);
    }
  }, [api, loadingMore, nextAfter]);

  const header = (
    <PageHeader title="System Audit" subtitle="Platform-wide audit trail, oldest first." />
  );

  if (resource.loading) {
    return (
      <>
        {header}
        <StateView state="loading" title="Loading audit trail" />
      </>
    );
  }
  if (resource.error !== null || resource.data === null) {
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
        <table className="table">
          <thead>
            <tr>
              <th scope="col">Seq</th>
              <th scope="col">Time</th>
              <th scope="col">Kind</th>
              <th scope="col">Actor</th>
              <th scope="col">Project</th>
              <th scope="col">Resource</th>
              <th scope="col">Outcome</th>
            </tr>
          </thead>
          <tbody>
            {entries.map((entry) => (
              <tr key={entry.seq}>
                <td className="mono">{entry.seq}</td>
                <td>
                  {entry.occurredAt === null || entry.occurredAt.trim() === ""
                    ? "—"
                    : entry.occurredAt}
                </td>
                <td className="mono">{entry.kind.trim() === "" ? "—" : entry.kind}</td>
                <td className="mono" title={entry.actorUserId ?? undefined}>
                  {entry.actorUserId === null ? (
                    <span className="muted">system</span>
                  ) : (
                    <span title={entry.actorUserId}>
                      {userLabel(entry.actorUserId) ?? shortId(entry.actorUserId)}
                    </span>
                  )}
                </td>
                <td className="mono" title={entry.projectId ?? undefined}>
                  {entry.projectId === null ? (
                    <span className="muted">—</span>
                  ) : (
                    <span title={entry.projectId}>
                      {projectLabel(entry.projectId) ?? shortId(entry.projectId)}
                    </span>
                  )}
                </td>
                <td>
                  {entry.resourceType.trim() === "" ? (
                    <span className="muted">—</span>
                  ) : (
                    <span className="mono" title={entry.resourceId ?? undefined}>
                      {entry.resourceType}{" "}
                      {entry.resourceId === null ? "" : shortId(entry.resourceId)}
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
      )}

      {moreError !== null ? (
        <p role="alert" className="muted">
          Loading more entries failed: {moreError}
        </p>
      ) : null}
      {nextAfter !== null ? (
        <div className="actions">
          <button
            type="button"
            className="btn"
            disabled={loadingMore}
            onClick={() => void loadMore()}
          >
            {loadingMore ? "Loading…" : "Load more"}
          </button>
        </div>
      ) : (
        <p className="muted">End of the loaded range.</p>
      )}
      <p className="muted">Page limit: {ADMIN_AUDIT_PAGE_LIMIT} entries per load.</p>
    </>
  );
}
