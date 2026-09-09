import {
  isReconcileAction,
  type FabricCapacityDto,
  type FabricHealthDto,
  type HealthActionDto,
} from "../api/health.ts";
import { Badge, Card, StateView } from "../ui/primitives.tsx";
import { actionKey, healthCardVariant, healthTone, observedText } from "./presentation.ts";

export type FabricsCapacityProps = {
  fabrics: readonly FabricHealthDto[];
  actions: readonly HealthActionDto[];
  canOperate: boolean;
  busyActionKey: string | null;
  onAction: (action: HealthActionDto) => void;
};

function summaryStatus(fabrics: readonly FabricHealthDto[]) {
  if (fabrics.length === 0) return "unknown" as const;
  if (fabrics.some((fabric) => fabric.status === "unhealthy")) return "unhealthy" as const;
  if (fabrics.some((fabric) => fabric.status === "degraded")) return "degraded" as const;
  if (fabrics.every((fabric) => fabric.status === "healthy")) return "healthy" as const;
  return "unknown" as const;
}

function gib(bytes: number | null): string | null {
  if (bytes === null) return null;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GiB`;
}

function ratio(used: number | null, total: number | null): string | null {
  const usedText = gib(used);
  const totalText = gib(total);
  if (usedText !== null && totalText !== null) return `${usedText} / ${totalText}`;
  return totalText;
}

function capacityText(capacity: FabricCapacityDto): string {
  const logical = ratio(
    capacity.workspaceLogicalAllocatedBytes,
    capacity.workspaceLogicalBudgetBytes,
  );
  const pool = gib(capacity.workspacePoolBytes);
  if (logical !== null && pool !== null) {
    return `Workspace pool ${pool} · logical ${logical}`;
  }
  return "Fabric storage not reported";
}

function allocationsText(capacity: FabricCapacityDto): string {
  const parts: string[] = [];
  const used = gib(capacity.workspacePoolUsedBytes);
  if (used !== null) parts.push(`${used} workspace blocks used`);
  const restore = gib(capacity.workspaceRestoreHeadroomBytes);
  if (restore !== null) {
    const restoreUsed = gib(capacity.workspaceRestoreAllocatedBytes);
    parts.push(
      restoreUsed === null
        ? `${restore} restore headroom`
        : `${restoreUsed} / ${restore} restore headroom`,
    );
  }
  const linear = ratio(capacity.linearAllocatedBytes, capacity.linearBudgetBytes);
  if (linear !== null) parts.push(`Databases + Deployments ${linear}`);
  return parts.join(" · ");
}

function reserveText(capacity: FabricCapacityDto): string {
  const reserve = gib(capacity.recoveryReserveBytes);
  const free = gib(capacity.physicalFreeBytes);
  const runtime = ratio(capacity.runtimePoolUsedBytes, capacity.runtimePoolBytes);
  const parts: string[] = [];
  if (reserve !== null) parts.push(`${reserve} recovery reserve`);
  if (free !== null) parts.push(`${free} physically free`);
  if (runtime !== null) parts.push(`${runtime} runtime`);
  return parts.join(" · ");
}

function stateText(capacity: FabricCapacityDto): string {
  return `ready ${capacity.ready} · creating ${capacity.creating} · fenced ${capacity.fenced} · archived ${capacity.archived}`;
}

function reconcileFor(
  actions: readonly HealthActionDto[],
  fabricId: string,
  canOperate: boolean,
): HealthActionDto | null {
  if (!canOperate) return null;
  return (
    actions.find(
      (action) => isReconcileAction(action) && action.targetId === fabricId,
    ) ?? null
  );
}

export function FabricsCapacity({
  fabrics,
  actions,
  canOperate,
  busyActionKey,
  onAction,
}: FabricsCapacityProps) {
  const status = summaryStatus(fabrics);
  return (
    <Card title={`Fabrics capacity & status (${fabrics.length})`} variant={healthCardVariant(status)} bodyClass="kds-flush">
      {fabrics.length === 0 ? (
        <StateView
          state="empty"
          title="No Fabric observations"
          detail="The verified Fabric listing returned no rows for this admin session."
        />
      ) : (
        <>
          <table className="table">
            <thead>
              <tr>
                <th scope="col">Fabric</th>
                <th scope="col">Status</th>
                <th scope="col">Capacity</th>
                <th scope="col">Lifecycle rows</th>
                <th scope="col">Last observed</th>
                <th scope="col">Action</th>
              </tr>
            </thead>
            <tbody>
              {fabrics.map((fabric) => {
                const action = reconcileFor(actions, fabric.id, canOperate);
                const key = action === null ? null : actionKey(action);
                const allocatedKinds = allocationsText(fabric.capacity);
                return (
                  <tr key={fabric.id}>
                    <td>
                      {fabric.name.trim().length === 0 ? "Unnamed Fabric" : fabric.name}
                    </td>
                    <td>
                      <Badge tone={healthTone(fabric.status)}>{fabric.status}</Badge>
                    </td>
                    <td>
                      {capacityText(fabric.capacity)}
                      <br />
                      <span className="muted">{reserveText(fabric.capacity) || "No quota reported"}</span>
                      {allocatedKinds ? (
                        <>
                          <br />
                          <span className="muted">{allocatedKinds}</span>
                        </>
                      ) : null}
                    </td>
                    <td className="mono">{stateText(fabric.capacity)}</td>
                    <td className="kds-datetime">{observedText(fabric.lastObservedAt)}</td>
                    <td>
                      {action === null ? (
                        <span className="muted">Not issued</span>
                      ) : (
                        <button
                          type="button"
                          className="btn"
                          disabled={busyActionKey !== null}
                          onClick={() => onAction(action)}
                        >
                          {busyActionKey === key ? "Reconciling…" : action.label}
                        </button>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          <p className="muted table-note">
            Workspace thin-pool bytes, logical Workspace quota, Database/Deployment linear
            allocation, recovery reserve, and runtime pool come from Fabric. Lifecycle counts
            stay aggregate and are not storage capacity. Workspace and Session internals stay
            out of the platform health projection. Reconcile controls appear only for a
            concrete server-issued action targeted at the Fabric.
          </p>
        </>
      )}
    </Card>
  );
}
