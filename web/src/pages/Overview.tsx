/**
 * Overview: one bounded snapshot per mount for the selected project. Counts
 * come from the same typed resources the section pages use; the recent audit
 * headline and the activity feed are strictly best-effort decorations and
 * never poll.
 */

import { useCallback } from "react";
import {
  decodeEventItems,
  listAgents,
  listAudit,
  listFeedEvents,
  listSessions,
  listWorkspaces,
  projectBoundWorkspaces,
} from "../api/api.ts";
import type {
  AgentSummaryDto,
  AuditEntryDto,
  RawEventDto,
  SessionSummaryDto,
  WorkspaceSummaryDto,
} from "../api/dto.ts";
import { useConsole } from "../console.tsx";
import { pairSurfaceItems, projectEvents } from "../events/project.ts";
import { useResource } from "../hooks.ts";
import { appHref, Link } from "../router.tsx";
import { Card, PageHeader, StateView } from "../ui/primitives.tsx";

/** Headline size caps; both loads are single-shot, never repeated. */
const AUDIT_HEADLINE_LIMIT = 8;
const ACTIVITY_LINE_LIMIT = 6;

type OverviewSnapshot = {
  agents: AgentSummaryDto[];
  sessions: SessionSummaryDto[];
  workspaces: WorkspaceSummaryDto[];
  /** Workspaces owned by or referenced from this project's sessions. */
  projectWorkspaces: WorkspaceSummaryDto[];
  /** Distinct fabrics behind those workspaces; zero without any. */
  projectFabricCount: number;
  audit: AuditEntryDto[];
  auditHasMore: boolean;
  activityLines: string[];
};

function clip(text: string, max = 96): string {
  const clean = text.replace(/\s+/g, " ").trim();
  return clean.length > max ? `${clean.slice(0, max - 1)}…` : clean;
}

/**
 * Projects the newest surface items into short honest lines. Unknown event
 * vocabulary projects to nothing upstream, so it cannot fabricate lines here.
 */
function activityLinesOf(events: readonly RawEventDto[]): string[] {
  const items = pairSurfaceItems(projectEvents(events));
  const lines: string[] = [];
  for (let index = items.length - 1; index >= 0 && lines.length < ACTIVITY_LINE_LIMIT; index -= 1) {
    const item = items[index];
    if (item === undefined) continue;
    let line: string | null = null;
    switch (item.kind) {
      case "user":
        line = item.text.trim() === "" ? null : `You: ${clip(item.text)}`;
        break;
      case "assistant":
        line = item.text.trim() === "" ? null : `Assistant: ${clip(item.text)}`;
        break;
      case "bash": {
        const outcome = item.status === "ok" ? "finished" : item.status === "failure" ? "failed" : "pending";
        const command = item.call?.command ?? "";
        line = command === "" ? `Bash run ${outcome}` : `Bash ${outcome}: ${clip(command)}`;
        break;
      }
      case "turn-start":
        line = null;
        break;
    }
    if (line !== null) lines.push(line);
  }
  return lines.reverse();
}

export function Overview() {
  const {
    projects,
    projectId,
    selectedProject,
    loading: bootLoading,
    error: bootError,
    reload: reloadBootstrap,
  } = useConsole();

  const load = useCallback(async (signal: AbortSignal): Promise<OverviewSnapshot> => {
    if (projectId === null) {
      return {
        agents: [],
        sessions: [],
        workspaces: [],
        projectWorkspaces: [],
        projectFabricCount: 0,
        audit: [],
        auditHasMore: false,
        activityLines: [],
      };
    }
    const [agents, sessions, workspaces, audit] = await Promise.all([
      listAgents(projectId, signal),
      listSessions(projectId, signal),
      listWorkspaces(signal),
      listAudit(undefined, signal),
    ]);
    // The live feed is optional garnish: one bounded call, failures ignored.
    let activityLines: string[] = [];
    try {
      const feed = await listFeedEvents(0, signal);
      activityLines = activityLinesOf(decodeEventItems(feed.items));
    } catch {
      activityLines = [];
    }
    // Counts stay scoped to the selected project: the workspaces listing
    // spans every project this identity belongs to, so it is narrowed by
    // the same ownership/reference rule the session dialog applies.
    const projectWorkspaces = projectBoundWorkspaces(projectId, workspaces, sessions);
    const projectFabricCount = new Set(projectWorkspaces.map((w) => w.fabricId)).size;
    return {
      agents,
      sessions,
      workspaces,
      projectWorkspaces,
      projectFabricCount,
      audit: audit.entries.slice(0, AUDIT_HEADLINE_LIMIT),
      auditHasMore: audit.hasMore || audit.entries.length > AUDIT_HEADLINE_LIMIT,
      activityLines,
    };
  }, [projectId]);

  const resource = useResource(load, [projectId]);
  const snapshot = resource.data;

  const header = (
    <PageHeader
      title="Overview"
      subtitle={
        selectedProject === null || selectedProject.name.trim() === ""
          ? "All projects"
          : selectedProject.name
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
            detail="Join a project to see its overview."
          />
        )}
      </>
    );
  }

  if (resource.loading) {
    return (
      <>
        {header}
        <StateView state="loading" title="Loading overview" />
      </>
    );
  }
  if (resource.error !== null || snapshot === null) {
    return (
      <>
        {header}
        <StateView
          state="error"
          title="Could not load the overview"
          detail={resource.error?.message ?? "request failed"}
          onRetry={resource.reload}
        />
      </>
    );
  }

  const runningCount = snapshot.sessions.filter((session) => session.running).length;

  return (
    <>
      {header}
      <div className="grid">
        <Card title="Projects">
          <div className="stack">
            <strong className="mono">{projects.length}</strong>
            <Link to={appHref("/project", projectId)}>View project</Link>
          </div>
        </Card>
        <Card title="Sessions">
          <div className="stack">
            <strong className="mono">{snapshot.sessions.length}</strong>
            <span className="muted">{runningCount} running</span>
            <Link to={appHref("/sessions", projectId)}>View sessions</Link>
          </div>
        </Card>
        <Card title="Agents">
          <div className="stack">
            <strong className="mono">{snapshot.agents.length}</strong>
            <Link to={appHref("/agents", projectId)}>View agents</Link>
          </div>
        </Card>
        <Card title="Workspaces">
          <div className="stack">
            <strong className="mono">{snapshot.projectWorkspaces.length}</strong>
            <Link to={appHref("/workspaces", projectId)}>View workspaces</Link>
          </div>
        </Card>
        <Card title="Fabrics">
          <div className="stack">
            <strong className="mono">{snapshot.projectFabricCount}</strong>
            <Link to={appHref("/fabrics", projectId)}>View fabrics</Link>
          </div>
        </Card>
      </div>

      <Card title="Recent audit" actions={<Link to={appHref("/audit", projectId)}>View audit</Link>}>
        {snapshot.audit.length === 0 ? (
          <p className="muted">No audit entries recorded yet.</p>
        ) : (
          <table className="table">
            <thead>
              <tr>
                <th scope="col">Seq</th>
                <th scope="col">Kind</th>
                <th scope="col">Recorded</th>
              </tr>
            </thead>
            <tbody>
              {snapshot.audit.map((entry) => (
                <tr key={entry.seq}>
                  <td className="mono">{entry.seq}</td>
                  <td className="mono">{entry.kind.trim() === "" ? "—" : entry.kind}</td>
                  <td>
                    {entry.occurredAt === null || entry.occurredAt.trim() === ""
                      ? "—"
                      : entry.occurredAt}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {snapshot.auditHasMore ? (
          <p className="muted">Showing the most recent entries only.</p>
        ) : null}
      </Card>

      <Card title="Recent activity">
        {snapshot.activityLines.length === 0 ? (
          <p className="muted">No recent activity.</p>
        ) : (
          <ul>
            {snapshot.activityLines.map((line, index) => (
              <li key={index}>{line}</li>
            ))}
          </ul>
        )}
      </Card>
    </>
  );
}
