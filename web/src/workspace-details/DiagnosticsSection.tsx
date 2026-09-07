import { useCallback } from "react";
import {
  getWorkspaceDiagnostics,
  type WorkspaceDiagnosticsDto,
} from "../api/workspace-details.ts";
import type { Uuid } from "../api/dto.ts";
import { useResource } from "../hooks.ts";
import { Badge, Card, StateView } from "../ui/primitives.tsx";
import { formatDate, shortId, stateLabel, stateTone } from "./model.ts";

export type DiagnosticsSectionProps = {
  workspaceId: Uuid;
  /** Must come from a server-declared platform-admin capability. */
  canViewDiagnostics: boolean;
};

function renderDiagnostics(diagnostics: WorkspaceDiagnosticsDto) {
  return (
    <>
      <p className="muted">
        The server granted the administrator diagnostics capability. These infrastructure facts
        are hidden from ordinary workspace members.
      </p>
      <table className="table">
        <tbody>
          <tr>
            <th scope="row">Workspace ID</th>
            <td className="mono" title={diagnostics.workspaceId}>
              {shortId(diagnostics.workspaceId)}
            </td>
          </tr>
          <tr>
            <th scope="row">Project ID</th>
            <td className="mono" title={diagnostics.projectId}>
              {shortId(diagnostics.projectId)}
            </td>
          </tr>
          <tr>
            <th scope="row">Fabric ID</th>
            <td className="mono" title={diagnostics.fabricId ?? undefined}>
              {diagnostics.fabricId === null || diagnostics.fabricId.trim() === ""
                ? "—"
                : shortId(diagnostics.fabricId)}
            </td>
          </tr>
          <tr>
            <th scope="row">Fabric name</th>
            <td>
              {diagnostics.fabricName === null || diagnostics.fabricName.trim() === ""
                ? "—"
                : diagnostics.fabricName}
            </td>
          </tr>
          <tr>
            <th scope="row">Lifecycle state</th>
            <td>
              <Badge tone={stateTone(diagnostics.state)}>{diagnostics.state}</Badge>
              <span className="muted"> ({stateLabel(diagnostics.state)})</span>
            </td>
          </tr>
          <tr>
            <th scope="row">Execution generation</th>
            <td className="mono">
              {diagnostics.execGeneration === null ? "—" : diagnostics.execGeneration}
            </td>
          </tr>
          <tr>
            <th scope="row">Node</th>
            <td>
              {diagnostics.nodeName === null || diagnostics.nodeName.trim() === ""
                ? "—"
                : diagnostics.nodeName}
            </td>
          </tr>
          <tr>
            <th scope="row">Runtime</th>
            <td>
              {diagnostics.runtime === null || diagnostics.runtime.trim() === ""
                ? "—"
                : diagnostics.runtime}
            </td>
          </tr>
          <tr>
            <th scope="row">Created</th>
            <td>{formatDate(diagnostics.createdAt)}</td>
          </tr>
        </tbody>
      </table>
    </>
  );
}

/**
 * Admin-only underlay projection. The component fails closed: ordinary
 * members perform no diagnostics request and receive no diagnostics markup.
 */
export function DiagnosticsSection({ workspaceId, canViewDiagnostics }: DiagnosticsSectionProps) {
  const load = useCallback(
    (signal: AbortSignal) =>
      canViewDiagnostics
        ? getWorkspaceDiagnostics(workspaceId, signal)
        : Promise.resolve(null),
    [canViewDiagnostics, workspaceId],
  );
  const resource = useResource(load, [canViewDiagnostics, workspaceId]);

  if (!canViewDiagnostics) return null;

  return (
    <Card title="Diagnostics & underlay">
      {resource.loading ? (
        <StateView state="loading" title="Loading administrator diagnostics" />
      ) : resource.error !== null ? (
        <StateView
          state="error"
          title="Could not load administrator diagnostics"
          detail={resource.error.message}
          onRetry={resource.reload}
        />
      ) : resource.data === null ? (
        <StateView
          state="empty"
          title="No diagnostics available"
          detail="The server did not return an underlay projection for this workspace."
        />
      ) : (
        renderDiagnostics(resource.data)
      )}
    </Card>
  );
}
