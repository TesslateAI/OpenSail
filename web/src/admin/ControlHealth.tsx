/**
 * Control Health: verbatim facts from GET /api/admin/health.
 *
 * The control plane answers one health projection (database, blob, auth,
 * fabric, workspace counts, and Fabric storage when the Fabric answered).
 * This panel renders those facts as plain key/value rows — no charts, no
 * client-side aggregation — so the admin console shows exactly what the
 * server reports. The existing aggregated health surface (client-probed)
 * lives under `health/`; this is the server-verbatim counterpart.
 */

import { useCallback } from "react";
import {
  adminApi,
  type AdminApi,
  type AdminHealthFactsDto,
  type AdminStorageFactsDto,
} from "../api/admin.ts";
import { useResource } from "../hooks.ts";
import { Badge, Card, PageHeader, StateView } from "../ui/primitives.tsx";

function factTone(ok: boolean): "ok" | "fail" | "neutral" {
  return ok ? "ok" : "fail";
}

function storageHealthTone(health: string): "ok" | "fail" | "neutral" {
  if (health === "healthy") return "ok";
  if (health === "critical") return "fail";
  if (health === "warning") return "neutral";
  return "neutral";
}

function gib(bytes: number): string {
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GiB`;
}

function storageRows(
  storage: AdminStorageFactsDto,
): Array<[string, string, "ok" | "fail" | "neutral"]> {
  return [
    ["Storage health", storage.health, storageHealthTone(storage.health)],
    ["Device", gib(storage.deviceBytes), "neutral"],
    [
      "Workspace pool",
      `${gib(storage.workspacePoolUsedBytes)} used / ${gib(storage.workspacePoolBytes)} thin pool`,
      "neutral",
    ],
    [
      "Workspace logical quota",
      `${gib(storage.workspaceLogicalAllocatedBytes)} / ${gib(storage.workspaceLogicalBudgetBytes)}`,
      "neutral",
    ],
    [
      "Workspace restore headroom",
      `${gib(storage.workspaceRestoreAllocatedBytes)} / ${gib(storage.workspaceRestoreHeadroomBytes)}`,
      "neutral",
    ],
    [
      "Databases + Deployments",
      `${gib(storage.linearAllocatedBytes)} / ${gib(storage.linearBudgetBytes)}`,
      "neutral",
    ],
    ["Databases", gib(storage.databasesBytes), "neutral"],
    ["Deployments", gib(storage.deploymentsBytes), "neutral"],
    [
      "Recovery reserve",
      `${gib(storage.recoveryReserveBytes)} required · ${gib(storage.physicalFreeBytes)} physically free`,
      "neutral",
    ],
    ["Emergency floor", gib(storage.emergencyFloorBytes), "neutral"],
    [
      "Runtime",
      `${gib(storage.runtimePoolUsedBytes)} / ${gib(storage.runtimePoolBytes)}`,
      "neutral",
    ],
  ];
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
  const capacityRows =
    facts.storage === null
      ? ([["Fabric storage", "not reported", "fail"]] as Array<
          [string, string, "ok" | "fail" | "neutral"]
        >)
      : storageRows(facts.storage);

  return (
    <>
      {header}
      <Card title="Control facts" bodyClass="kds-flush">
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
        <p className="muted table-note">
          Values are emitted verbatim by <code>GET /api/admin/health</code>; no client-side
          aggregation or charts.
        </p>
      </Card>
      <Card title="Fabric capacity" bodyClass="kds-flush">
        <table className="table">
          <thead>
            <tr>
              <th scope="col">Fact</th>
              <th scope="col">Value</th>
            </tr>
          </thead>
          <tbody>
            {capacityRows.map(([label, value, tone]) => (
              <tr key={label}>
                <td>{label}</td>
                <td>
                  <Badge tone={tone}>{value}</Badge>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        <p className="muted table-note">
          Product budget, recovery reserve, physically free extents, and per-kind allocated
          bytes come from the Fabric <code>/v1/capacity</code> report nested under{" "}
          <code>storage</code>. The recovery reserve is unused VG space, not an LV.
        </p>
      </Card>
    </>
  );
}
