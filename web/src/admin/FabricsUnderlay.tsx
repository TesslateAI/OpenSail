/**
 * Fabrics & Underlay: read-only platform infrastructure surface.
 *
 * Fabrics come from GET /api/admin/fabrics, workspaces from GET
 * /api/admin/workspaces. Both listings are platform-wide and answer 403
 * unless the caller carries the platform `admin` role; the panel renders
 * exactly the emitted rows and never invents infrastructure. Workspace
 * lifecycle states are the durable vocabulary (creating | ready | fenced);
 * malformed labels follow the adapter's conservative fallback.
 */

import { useCallback } from "react";
import {
  adminApi,
  WORKSPACE_STATES,
  type AdminApi,
  type AdminGlobalWorkspaceDto,
} from "../api/admin.ts";
import type { FabricDto } from "../api/dto.ts";
import { useResource } from "../hooks.ts";
import { Badge, Card, PageHeader, StateView } from "../ui/primitives.tsx";

function shortId(id: string): string {
  return id.length === 0 ? "—" : id.length <= 10 ? id : `${id.slice(0, 8)}…`;
}

function toneOf(state: string): "ok" | "warn" | "neutral" {
  if (state === "ready") return "ok";
  if (state === "creating" || state === "fenced") return "warn";
  return "neutral";
}

type AdminFabricsUnderlayProps = { api?: AdminApi | undefined };

export function AdminFabricsUnderlay({ api = adminApi }: AdminFabricsUnderlayProps) {
  const fabricsLoad = useCallback((signal: AbortSignal) => api.getAdminFabrics(signal), [api]);
  const fabrics = useResource(fabricsLoad);

  const workspacesLoad = useCallback(
    (signal: AbortSignal) => api.getAdminWorkspaces(signal),
    [api],
  );
  const workspaces = useResource(workspacesLoad);

  const header = (
    <PageHeader
      title="Fabrics & Underlay"
      subtitle="Connected fabrics and the workspaces provisioned onto them."
    />
  );

  const fabricError = fabrics.error;
  const workspaceError = workspaces.error;

  return (
    <>
      {header}

      <Card title={`Fabrics (${(fabrics.data ?? []).length})`}>
        {fabrics.loading ? (
          <StateView state="loading" title="Loading fabrics" />
        ) : fabricError !== null ? (
          <StateView
            state="error"
            title="Could not load fabrics"
            detail={fabricError.message}
            onRetry={fabrics.reload}
          />
        ) : (fabrics.data ?? []).length === 0 ? (
          <StateView
            state="empty"
            title="No fabrics connected"
            detail="Connected fabrics appear here."
          />
        ) : (
          <table className="table">
            <thead>
              <tr>
                <th scope="col">Name</th>
                <th scope="col">ID</th>
                <th scope="col">Created</th>
              </tr>
            </thead>
            <tbody>
              {(fabrics.data ?? []).map((fabric: FabricDto) => (
                <tr key={fabric.id}>
                  <td>{fabric.name.trim() === "" ? "—" : fabric.name}</td>
                  <td className="mono" title={fabric.id}>
                    {shortId(fabric.id)}
                  </td>
                  <td>
                    {fabric.createdAt === null || fabric.createdAt.trim() === ""
                      ? "—"
                      : fabric.createdAt}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Card>

      <Card title={`Underlay workspaces (${(workspaces.data ?? []).length})`}>
        {workspaces.loading ? (
          <StateView state="loading" title="Loading workspaces" />
        ) : workspaceError !== null ? (
          <StateView
            state="error"
            title="Could not load workspaces"
            detail={workspaceError.message}
            onRetry={workspaces.reload}
          />
        ) : (workspaces.data ?? []).length === 0 ? (
          <StateView
            state="empty"
            title="No workspaces listed"
            detail="Provisioned workspaces appear here."
          />
        ) : (
          <table className="table">
            <thead>
              <tr>
                <th scope="col">Workspace</th>
                <th scope="col">Fabric</th>
                <th scope="col">Project</th>
                <th scope="col">State</th>
                <th scope="col">Created</th>
              </tr>
            </thead>
            <tbody>
              {[...(workspaces.data ?? [])]
                .sort((a, b) => a.id.localeCompare(b.id))
                .map((workspace: AdminGlobalWorkspaceDto) => (
                  <tr key={workspace.id}>
                    <td className="mono" title={workspace.id}>
                      {workspace.label === null || workspace.label.trim() === ""
                        ? shortId(workspace.id)
                        : workspace.label}
                    </td>
                    <td className="mono" title={workspace.fabricId}>
                      {workspace.fabricName === null || workspace.fabricName.trim() === ""
                        ? shortId(workspace.fabricId)
                        : workspace.fabricName}
                    </td>
                    <td className="mono" title={workspace.projectId}>
                      {shortId(workspace.projectId)}
                    </td>
                    <td>
                      <Badge tone={toneOf(workspace.state)}>{workspace.state}</Badge>
                    </td>
                    <td>
                      {workspace.createdAt === null || workspace.createdAt.trim() === ""
                        ? "—"
                        : workspace.createdAt}
                    </td>
                  </tr>
                ))}
            </tbody>
          </table>
        )}
        <p className="muted">
          Lifecycle states: {WORKSPACE_STATES.join(" · ")}. Unknown labels are shown as-is.
        </p>
      </Card>
    </>
  );
}
