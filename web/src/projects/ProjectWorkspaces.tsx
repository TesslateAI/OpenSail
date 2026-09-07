/**
 * Project workspaces: the sharing boundary for sessions, listed per project with
 * durable creator attribution. The "Share" action hands off to the
 * membership surface so the project's collaboration controls stay in one
 * place.
 */

import { useCallback, useState, type ChangeEvent } from "react";
import { getProject, listProjectWorkspaces, createWorkspace } from "../api/api.ts";
import type { ProjectMemberDto, ProjectWorkspaceDto, Uuid } from "../api/dto.ts";
import { newIntentId } from "../api/http.ts";
import { useBoundedPoll, useResource } from "../hooks.ts";
import { Badge, Card, PageHeader, StateView } from "../ui/primitives.tsx";
import { rememberWorkspace } from "../portal/last-workspace.ts";
import { syncVoieWorkspaces } from "../connection-voie/api.ts";
import { creatorLabel, formatDate, shortId } from "./model.ts";

export type ProjectWorkspacesProps = {
  projectId: Uuid;
  meUserId: Uuid | null;
  canOperate: boolean;
  canManage: boolean;
  onShare?: (() => void) | undefined;
  subtitle?: string | undefined;
};

function workspaceStateLabel(state: string | null): string {
  switch (state) {
    case "ready":
      return "Ready";
    case "fenced":
      return "Temporarily unavailable";
    case "archived":
      return "Archived";
    default:
      return "Preparing";
  }
}

function workspaceStateTone(state: string | null): "ok" | "warn" | "neutral" {
  return state === "ready" ? "ok" : "warn";
}

export function ProjectWorkspaces({
  projectId,
  meUserId,
  canOperate,
  canManage,
  onShare,
  subtitle = "Shared session homes for this project.",
}: ProjectWorkspacesProps) {
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  const [workspaceName, setWorkspaceName] = useState("");

  const handleNameChange = useCallback((event: ChangeEvent<HTMLInputElement>) => {
    setWorkspaceName(event.target.value);
  }, []);

  const loadWorkspaces = useCallback(
    async (signal: AbortSignal): Promise<ProjectWorkspaceDto[]> =>
      listProjectWorkspaces(projectId, signal),
    [projectId],
  );
  const workspaces = useResource(loadWorkspaces, [projectId]);

  // Creator labels resolve against the project roster; the detail resource
  // carries it, so one fetch serves attribution for every row.
  const loadRoster = useCallback(
    async (signal: AbortSignal): Promise<ProjectMemberDto[]> => {
      const detail = await getProject(projectId, signal);
      return detail.members;
    },
    [projectId],
  );
  const roster = useResource(loadRoster, [projectId]);

  const pendingProvision = (workspaces.data ?? []).some(
    (workspace) => workspace.state === "creating" || workspace.state === null,
  );
  const refreshWorkspaces = useCallback(
    async (_signal: AbortSignal): Promise<void> => {
      workspaces.reload();
    },
    [workspaces.reload],
  );
  useBoundedPoll(refreshWorkspaces, 2000, pendingProvision);

  const create = useCallback(async (): Promise<void> => {
    if (creating) return;
    setCreating(true);
    setCreateError(null);
    try {
      const created = await createWorkspace(projectId, newIntentId(), workspaceName.trim());
      rememberWorkspace(projectId, created.id);
      void syncVoieWorkspaces().catch(() => {});
      workspaces.reload();
      setWorkspaceName("");
    } catch (reason: unknown) {
      setCreateError(reason instanceof Error ? reason.message : "request failed");
    } finally {
      setCreating(false);
    }
  }, [creating, projectId, workspaceName, workspaces]);

  const header = (
    <PageHeader
      title="Workspaces"
      subtitle={subtitle}
      actions={
        <span className="actions">
          {canManage && onShare !== undefined ? (
            <button type="button" className="btn" onClick={onShare}>
              Share
            </button>
          ) : null}
          {canOperate ? (
            <span className="row">
              <input
                aria-label="Workspace name"
                placeholder="Workspace name"
                value={workspaceName}
                disabled={creating}
                onChange={handleNameChange}
              />
              <button
                type="button"
                className={creating ? "btn btn-primary btn-disabled" : "btn btn-primary"}
                disabled={creating || workspaceName.trim().length === 0}
                onClick={() => void create()}
              >
                {creating ? "Creating…" : "New workspace"}
              </button>
            </span>
          ) : null}
        </span>
      }
    />
  );

  if (
    (workspaces.loading && workspaces.data === null) ||
    (roster.loading && roster.data === null)
  ) {
    return (
      <>
        {header}
        <StateView state="loading" title="Loading workspaces" />
      </>
    );
  }
  if (workspaces.error !== null) {
    return (
      <>
        {header}
        <StateView
          state="error"
          title="Could not load workspaces"
          detail={workspaces.error.message}
          onRetry={workspaces.reload}
        />
      </>
    );
  }

  const items = workspaces.data ?? [];
  const members = roster.data ?? [];

  return (
    <>
      {header}
      {!canOperate ? (
        <p className="muted">
          Creating a workspace needs the operate-sessions capability in this scope.
        </p>
      ) : null}
      {createError !== null ? (
        <p role="alert" className="muted">
          Creating the workspace failed: {createError} Nothing was created; you can retry.
        </p>
      ) : null}
      {items.length === 0 ? (
        <StateView
          state="empty"
          title="No workspaces yet"
          detail={
            canOperate
              ? "Create a workspace to give sessions a durable home in this scope."
              : "Workspaces appear here once they are created in this scope."
          }
        />
      ) : (
        <Card>
          <table className="table">
            <thead>
              <tr>
                <th scope="col">Workspace</th>
                <th scope="col">State</th>
                <th scope="col">Created by</th>
                <th scope="col">Created</th>
              </tr>
            </thead>
            <tbody>
              {items.map((workspace) => (
                <tr
                  key={workspace.id}
                  data-workspace-id={workspace.id}
                  data-workspace-state={workspace.state ?? "creating"}
                >
                  <td className="mono" title={workspace.id}>
                    {workspace.label !== null && workspace.label.trim() !== ""
                      ? workspace.label
                      : shortId(workspace.id)}
                  </td>
                  <td>
                    <Badge tone={workspaceStateTone(workspace.state)}>
                      {workspaceStateLabel(workspace.state)}
                    </Badge>
                  </td>
                  <td title={workspace.createdByUserId ?? undefined}>
                    {creatorLabel(workspace.createdByUserId, meUserId, members)}
                  </td>
                  <td>
                    {formatDate(workspace.createdAt)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </Card>
      )}
    </>
  );
}
