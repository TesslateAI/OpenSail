/**
 * Sessions: bounded project-scoped listing with running-state freshness from
 * the shared visibility-aware poll hook (single-flight, pauses when hidden).
 * The chat-first hero composer starts a session and its first message in one
 * motion; the listing below doubles as the session sidebar with recency
 * ordering, running badges, and title fallback to the session id.
 */

import { useCallback, useEffect, useState } from "react";
import { listSessions } from "../api/api.ts";
import type { SessionSummaryDto } from "../api/dto.ts";
import { useConsole } from "../console.tsx";
import { useBoundedPoll, useResource } from "../hooks.ts";
import { appHref, Link, useRouter } from "../router.tsx";
import { Badge, PageHeader, StateView } from "../ui/primitives.tsx";
import { HeroComposer } from "../ui/HeroComposer.tsx";

const POLL_INTERVAL_MS = 5_000;

function shortId(id: string): string {
  return id.length === 0 ? "—" : id.length <= 10 ? id : `${id.slice(0, 8)}…`;
}

/** Display title: the server title when present, else the short id. */
function sessionTitle(session: SessionSummaryDto): string {
  return session.title === null || session.title.trim() === "" ? shortId(session.id) : session.title;
}

export function Sessions() {
  const {
    projectId,
    canOperate,
    loading: bootLoading,
    error: bootError,
    reload: reloadBootstrap,
  } = useConsole();
  const { location } = useRouter();
  const [polled, setPolled] = useState<SessionSummaryDto[] | null>(null);

  const load = useCallback(async (signal: AbortSignal): Promise<SessionSummaryDto[]> => {
    if (projectId === null) return [];
    return listSessions(projectId, signal);
  }, [projectId]);
  const resource = useResource(load, [projectId]);

  // A new project invalidates whatever a previous project's polls observed.
  useEffect(() => {
    setPolled(null);
  }, [projectId]);

  const tick = useCallback(async (signal: AbortSignal): Promise<void> => {
    if (projectId === null) return;
    setPolled(await listSessions(projectId, signal));
  }, [projectId]);
  useBoundedPoll(
    tick,
    POLL_INTERVAL_MS,
    projectId !== null && resource.data !== null && resource.error === null,
  );

  const header = (
    <PageHeader
      title="Sessions"
      subtitle="Recorded sessions for the selected project."
      actions={
        <Link className="btn" to={appHref("/agents", projectId)}>
          Manage agents
        </Link>
      }
    />
  );

  if (projectId === null) {
    return (
      <>
        {header}
        {bootLoading ? (
          <StateView state="loading" title="Loading workspace" />
        ) : bootError !== null ? (
          <StateView
            state="error"
            title="Could not load projects"
            detail={bootError.message}
            onRetry={reloadBootstrap}
          />
        ) : (
          <StateView
            state="empty"
            title="No project selected"
            detail="Join a project to see its sessions."
          />
        )}
      </>
    );
  }

  if (resource.loading) {
    return (
      <>
        {header}
        <StateView state="loading" title="Loading sessions" />
      </>
    );
  }
  if (resource.error !== null) {
    return (
      <>
        {header}
        <StateView
          state="error"
          title="Could not load sessions"
          detail={resource.error.message}
          onRetry={resource.reload}
        />
      </>
    );
  }

  const sessions = polled ?? resource.data ?? [];
  // Recency ordering: newest first by created time; sessions without a
  // parseable timestamp sort last, keeping the list stable.
  const ordered = [...sessions].sort((a, b) => {
    const at = (session: SessionSummaryDto): number => {
      if (session.createdAt === null || session.createdAt.trim() === "") {
        return Number.MIN_SAFE_INTEGER;
      }
      const parsed = Date.parse(session.createdAt);
      return Number.isNaN(parsed) ? Number.MIN_SAFE_INTEGER : parsed;
    };
    return at(b) - at(a);
  });
  const activeSessionId = location.route.name === "session" ? location.route.sessionId : null;

  return (
    <>
      {header}
      {canOperate ? <HeroComposer projectId={projectId} /> : null}

      {ordered.length === 0 ? (
        <StateView
          state="empty"
          title="No sessions yet"
          detail={
            canOperate
              ? "Describe a task above to start your first session."
              : "No sessions have been created in this project."
          }
        />
      ) : (
        <table className="table">
          <thead>
            <tr>
              <th scope="col">Session</th>
              <th scope="col">Agent</th>
              <th scope="col">Workspace</th>
              <th scope="col">Status</th>
              <th scope="col">Head rev</th>
              <th scope="col">Created</th>
            </tr>
          </thead>
          <tbody>
            {ordered.map((session) => (
              <tr
                key={session.id}
                className={session.id === activeSessionId ? "session-row-active" : undefined}
              >
                <td title={session.id}>
                  <Link to={appHref(`/sessions/${encodeURIComponent(session.id)}`, projectId)}>
                    {sessionTitle(session)}
                  </Link>
                  {session.title === null || session.title.trim() === "" ? null : (
                    <span className="mono muted"> {shortId(session.id)}</span>
                  )}
                </td>
                <td className="mono" title={session.agentId}>
                  {shortId(session.agentId)}
                </td>
                <td className="mono" title={session.workspaceId}>
                  {shortId(session.workspaceId)}
                </td>
                <td>
                  {session.running ? (
                    <Badge tone="accent">running</Badge>
                  ) : (
                    <Badge tone="neutral">idle</Badge>
                  )}
                </td>
                <td className="mono">{session.headRevision}</td>
                <td>
                  {session.createdAt === null || session.createdAt.trim() === ""
                    ? "—"
                    : session.createdAt}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </>
  );
}
