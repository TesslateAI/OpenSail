/**
 * Scope workspaces: the sharing boundary for sessions, listed per scope with
 * durable creator attribution. The "Share" action hands off to the
 * membership surface so the scope's collaboration controls stay in one
 * place.
 */

import { useCallback, useState, type ChangeEvent } from "react";
import { getScope, listScopeWorkspaces, createScopeWorkspace } from "../api/scopes.ts";
import type { ScopeMemberDto, ScopeWorkspaceDto, Uuid } from "../api/dto.ts";
import { newIntentId } from "../api/http.ts";
import { useResource } from "../hooks.ts";
import { Card, PageHeader, StateView } from "../ui/primitives.tsx";
import { creatorLabel, formatDate, shortId } from "./model.ts";

export type ScopeWorkspacesProps = {
  scopeId: Uuid;
  meUserId: Uuid | null;
  canOperate: boolean;
  canManage: boolean;
  onShare?: (() => void) | undefined;
  subtitle?: string | undefined;
};

export function ScopeWorkspaces({
  scopeId,
  meUserId,
  canOperate,
  canManage,
  onShare,
  subtitle = "Shared session homes for this scope.",
}: ScopeWorkspacesProps) {
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  const [workspaceName, setWorkspaceName] = useState("");

  const handleNameChange = useCallback((event: ChangeEvent<HTMLInputElement>) => {
    setWorkspaceName(event.target.value);
  }, []);

  const loadWorkspaces = useCallback(
    async (signal: AbortSignal): Promise<ScopeWorkspaceDto[]> =>
      listScopeWorkspaces(scopeId, signal),
    [scopeId],
  );
  const workspaces = useResource(loadWorkspaces, [scopeId]);

  // Creator labels resolve against the scope roster; the detail resource
  // carries it, so one fetch serves attribution for every row.
  const loadRoster = useCallback(
    async (signal: AbortSignal): Promise<ScopeMemberDto[]> => {
      const detail = await getScope(scopeId, signal);
      return detail.members;
    },
    [scopeId],
  );
  const roster = useResource(loadRoster, [scopeId]);

  const create = useCallback(async (): Promise<void> => {
    if (creating) return;
    setCreating(true);
    setCreateError(null);
    try {
      await createScopeWorkspace(scopeId, newIntentId(), workspaceName.trim());
      workspaces.reload();
      setWorkspaceName("");
    } catch (reason: unknown) {
      setCreateError(reason instanceof Error ? reason.message : "request failed");
    } finally {
      setCreating(false);
    }
  }, [creating, scopeId, workspaceName, workspaces]);

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

  if (workspaces.loading || roster.loading) {
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
                <th scope="col">Created by</th>
                <th scope="col">Created</th>
              </tr>
            </thead>
            <tbody>
              {items.map((workspace) => (
                <tr key={workspace.id}>
                  <td className="mono" title={workspace.id}>
                    {workspace.label !== null && workspace.label.trim() !== ""
                      ? workspace.label
                      : shortId(workspace.id)}
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
