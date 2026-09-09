/** Fabrics: bounded global listing of connected fabrics. Read-only. */

import { useCallback } from "react";
import { listFabrics } from "../api/api.ts";
import type { FabricDto } from "../api/dto.ts";
import { useResource } from "../hooks.ts";
import { Card, PageHeader, StateView } from "../ui/primitives.tsx";

export function Fabrics() {
  const load = useCallback((signal: AbortSignal) => listFabrics(signal), []);
  const resource = useResource(load);

  const header = <PageHeader title="Fabrics" subtitle="Fabrics known to the control plane." />;

  if (resource.loading) {
    return (
      <>
        {header}
        <StateView state="loading" title="Loading fabrics" />
      </>
    );
  }
  if (resource.error !== null) {
    return (
      <>
        {header}
        <StateView
          state="error"
          title="Could not load fabrics"
          detail={resource.error.message}
          onRetry={resource.reload}
        />
      </>
    );
  }

  const fabrics = resource.data ?? [];

  return (
    <>
      {header}
      {fabrics.length === 0 ? (
        <StateView
          state="empty"
          title="No fabrics connected"
          detail="Connected fabrics appear here."
        />
      ) : (
        <Card title="Fabrics" bodyClass="kds-flush">
          <table className="table">
            <thead>
              <tr>
                <th scope="col">Name</th>
                <th scope="col">ID</th>
                <th scope="col">Created</th>
              </tr>
            </thead>
            <tbody>
              {fabrics.map((fabric) => (
                <tr key={fabric.id}>
                  <td>{fabric.name.trim() === "" ? "—" : fabric.name}</td>
                  <td className="mono" title={fabric.id}>
                    {fabric.id.length === 0
                      ? "—"
                      : fabric.id.length <= 10
                        ? fabric.id
                        : `${fabric.id.slice(0, 8)}…`}
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
        </Card>
      )}
    </>
  );
}
