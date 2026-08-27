/**
 * Control Health: verbatim facts from GET /api/admin/health.
 *
 * The control plane answers one flat health projection (database, blob,
 * auth, fabric, workspace counts). This panel renders those facts as plain
 * key/value rows — no charts, no client-side aggregation — so the admin
 * console shows exactly what the server reports. The existing aggregated
 * health surface (client-probed) lives under `health/`; this is the
 * server-verbatim counterpart.
 */

import { useCallback } from "react";
import { adminApi, type AdminApi, type AdminHealthFactsDto } from "../api/admin.ts";
import { useResource } from "../hooks.ts";
import { Badge, Card, PageHeader, StateView } from "../ui/primitives.tsx";

function factTone(ok: boolean): "ok" | "fail" | "neutral" {
  return ok ? "ok" : "fail";
}

type AdminControlHealthProps = { api?: AdminApi | undefined };

export function AdminControlHealth({ api = adminApi }: AdminControlHealthProps) {
  const load = useCallback((signal: AbortSignal) => api.getAdminHealth(signal), [api]);
  const resource = useResource(load);

  const header = (
    <PageHeader title="Control Health" subtitle="Verbatim facts from the control plane." />
  );

  if (resource.loading) {
    return (
      <>
        {header}
        <StateView state="loading" title="Loading control health" />
      </>
    );
  }
  if (resource.error !== null || resource.data === null) {
    return (
      <>
        {header}
        <StateView
          state="error"
          title="Could not load control health"
          detail={resource.error?.message ?? "request failed"}
          onRetry={resource.reload}
        />
      </>
    );
  }

  const facts: AdminHealthFactsDto = resource.data;
  const rows: Array<[string, string, "ok" | "fail" | "neutral"]> = [
    ["Database", facts.databaseOk ? "ok" : "unavailable", factTone(facts.databaseOk)],
    ["Blob store", facts.blobConfigured ? "configured" : "not configured", factTone(facts.blobConfigured)],
    ["Auth mode", facts.authMode, "neutral"],
    ["Fabric", facts.fabricRegistered ? "registered" : "not registered", factTone(facts.fabricRegistered)],
    ["Workspaces creating", String(facts.workspaceCreating), "neutral"],
    ["Workspaces ready", String(facts.workspaceReady), "neutral"],
    ["Workspaces fenced", String(facts.workspaceFenced), "neutral"],
  ];

  return (
    <>
      {header}
      <Card title="Control facts">
        <table className="table">
          <thead>
            <tr>
              <th scope="col">Fact</th>
              <th scope="col">Value</th>
            </tr>
          </thead>
          <tbody>
            {rows.map(([label, value, tone]) => (
              <tr key={label}>
                <td>{label}</td>
                <td>
                  <Badge tone={tone}>{value}</Badge>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        <p className="muted">
          Values are emitted verbatim by <code>GET /api/admin/health</code>; no client-side
          aggregation or charts.
        </p>
      </Card>
    </>
  );
}
