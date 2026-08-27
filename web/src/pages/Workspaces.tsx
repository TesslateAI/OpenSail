/** Workspaces: bounded global listing across fabrics plus creation on the
 * deployment-selected Fabric (POST /api/projects/:id/workspaces) gated by
 * the operate-sessions capability. Teardown (DELETE
 * /api/projects/:id/workspaces/:id) is destructive and project-scoped: it is
 * offered only when the workspace row itself states ownership of the acting
 * project and the manage-members capability is granted. Replacement
 * (POST /api/projects/:id/workspaces/:id/replace) is the operate-gated
 * lifecycle operation for recycling the backing allocation. */

import { useCallback, useState } from "react";
import { createWorkspace, deleteWorkspace, listWorkspaces, replaceWorkspace } from "../api/api.ts";
import type { Uuid, WorkspaceSummaryDto } from "../api/dto.ts";
import { newIntentId } from "../api/http.ts";
import { useConsole } from "../console.tsx";
import { useResource } from "../hooks.ts";
import { Badge, PageHeader, StateView } from "../ui/primitives.tsx";

function shortId(id: string): string {
  return id.length === 0 ? "—" : id.length <= 10 ? id : `${id.slice(0, 8)}…`;
}

export function Workspaces() {
  const { projectId, canOperate, canManageMembers } = useConsole();
  const load = useCallback(
    async (signal: AbortSignal): Promise<WorkspaceSummaryDto[]> => listWorkspaces(signal),
    [],
  );
  const resource = useResource(load);

  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  const [deletingId, setDeletingId] = useState<Uuid | null>(null);
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const [replacingId, setReplacingId] = useState<Uuid | null>(null);
  const [replaceError, setReplaceError] = useState<string | null>(null);
  const [replaceSuccess, setReplaceSuccess] = useState<string | null>(null);

  const create = useCallback(async (): Promise<void> => {
    if (projectId === null || creating) return;
    setCreating(true);
    setCreateError(null);
    try {
      await createWorkspace(projectId, newIntentId());
      resource.reload();
    } catch (reason: unknown) {
      setCreateError(reason instanceof Error ? reason.message : "request failed");
    } finally {
      setCreating(false);
    }
  }, [creating, projectId, resource]);

  const remove = useCallback(
    async (workspaceId: Uuid): Promise<void> => {
      if (projectId === null || deletingId !== null) return;
      setDeletingId(workspaceId);
      setDeleteError(null);
      try {
        await deleteWorkspace(projectId, workspaceId);
        resource.reload();
      } catch (reason: unknown) {
        setDeleteError(reason instanceof Error ? reason.message : "request failed");
      } finally {
        setDeletingId(null);
      }
    },
    [deletingId, projectId, resource],
  );

  const replace = useCallback(
    async (workspaceId: Uuid): Promise<void> => {
      if (projectId === null || replacingId !== null) return;
      setReplacingId(workspaceId);
      setReplaceError(null);
      setReplaceSuccess(null);
      try {
        const result = await replaceWorkspace(projectId, workspaceId);
        setReplaceSuccess(`Replaced allocation; exec generation ${result.execGeneration ?? "—"}`);
        resource.reload();
      } catch (reason: unknown) {
        setReplaceError(reason instanceof Error ? reason.message : "request failed");
      } finally {
        setReplacingId(null);
      }
    },
    [projectId, replacingId, resource],
  );

  const header = (
    <PageHeader
      title="Workspaces"
      subtitle="Workspaces across all fabrics."
      actions={
        canOperate && projectId !== null ? (
          <button
            type="button"
            className={creating ? "btn btn-primary btn-disabled" : "btn btn-primary"}
            disabled={creating}
            onClick={() => void create()}
          >
            {creating ? "Creating…" : "New workspace"}
          </button>
        ) : null
      }
    />
  );

  if (resource.loading) {
    return (
      <>
        {header}
        <StateView state="loading" title="Loading workspaces" />
      </>
    );
  }
  if (resource.error !== null) {
    return (
      <>
        {header}
        <StateView
          state="error"
          title="Could not load workspaces"
          detail={resource.error.message}
          onRetry={resource.reload}
        />
      </>
    );
  }

  const workspaces = resource.data ?? [];
  const canDelete = (workspace: WorkspaceSummaryDto): boolean =>
    canManageMembers && projectId !== null && workspace.projectId === projectId;
  const canReplace = (workspace: WorkspaceSummaryDto): boolean =>
    canOperate && projectId !== null && workspace.projectId === projectId;

  return (
    <>
      {header}
      {!canOperate ? (
        <p className="muted">
          Creating a workspace needs the operate-sessions capability in the selected project.
        </p>
      ) : null}
      {createError !== null ? (
        <p role="alert" className="muted">
          Creating the workspace failed: {createError} Nothing was created; you can retry.
        </p>
      ) : null}
      {deleteError !== null ? (
        <p role="alert" className="muted">
          Tearing the workspace down failed: {deleteError} The workspace is unchanged.
        </p>
      ) : null}
      {replaceError !== null ? (
        <p role="alert" className="muted">
          Replacing the workspace failed: {replaceError} The workspace is unchanged.
        </p>
      ) : null}
      {replaceSuccess !== null ? (
        <p className="muted">{replaceSuccess}</p>
      ) : null}

      {workspaces.length === 0 ? (
        <StateView
          state="empty"
          title="No workspaces yet"
          detail={
            canOperate
              ? "Create a workspace to give sessions a durable home on the selected Fabric."
              : "Workspaces appear here once they are created in this deployment."
          }
        />
      ) : (
        <table className="table">
          <thead>
            <tr>
              <th scope="col">Workspace</th>
              <th scope="col">Fabric</th>
              <th scope="col">Created</th>
              <th scope="col">Actions</th>
            </tr>
          </thead>
          <tbody>
            {workspaces.map((workspace) => (
              <tr key={workspace.id}>
                <td className="mono" title={workspace.id}>
                  {shortId(workspace.id)}
                </td>
                <td>
                  {workspace.fabricName !== null && workspace.fabricName.trim() !== "" ? (
                    workspace.fabricName
                  ) : (
                    <span className="muted" title={workspace.fabricId}>
                      fabric {shortId(workspace.fabricId)}
                    </span>
                  )}
                </td>
                <td>
                  {workspace.createdAt === null || workspace.createdAt.trim() === ""
                    ? "—"
                    : workspace.createdAt}
                </td>
                <td>
                  <span className="actions">
                    {canReplace(workspace) ? (
                      replacingId !== null && replacingId === workspace.id ? (
                        <Badge tone="warn">replacing…</Badge>
                      ) : (
                        <button
                          type="button"
                          className="btn"
                          disabled={replacingId !== null || deletingId !== null}
                          title="Replace backing allocation via Fabric"
                          onClick={() => void replace(workspace.id)}
                        >
                          Replace
                        </button>
                      )
                    ) : canOperate ? (
                      <span className="muted" title="Replace needs project-owned workspace">—</span>
                    ) : null}
                    {canDelete(workspace) ? (
                      deletingId !== null && deletingId === workspace.id ? (
                        <Badge tone="warn">tearing down…</Badge>
                      ) : (
                        <button
                          type="button"
                          className="btn"
                          disabled={deletingId !== null || replacingId !== null}
                          title="Tear this workspace down through the Fabric"
                          onClick={() => void remove(workspace.id)}
                        >
                          Delete
                        </button>
                      )
                    ) : canManageMembers ? (
                      <span
                        className="muted"
                        title="Deletion waits for authoritative per-resource ownership in the API"
                      >
                        —
                      </span>
                    ) : null}
                    {!canOperate && !canManageMembers ? (
                      <span className="muted">—</span>
                    ) : null}
                  </span>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {canManageMembers || canOperate ? (
        <p className="muted">
          Delete appears once the console sees ownership; replace recycles the allocation while
          keeping the workspace identity. Both are refused while a lifecycle operation is fenced.
        </p>
      ) : null}
    </>
  );
}
