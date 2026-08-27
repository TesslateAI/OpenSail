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

function capacityText(capacity: FabricCapacityDto): string {
  const used = `${capacity.used} allocated`;
  if (capacity.limit === null) return used;
  return `${used} / ${capacity.limit} limit`;
}

function stateText(capacity: FabricCapacityDto): string {
  return `ready ${capacity.ready} · creating ${capacity.creating} · fenced ${capacity.fenced}`;
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
    <Card title={`Fabrics capacity & status (${fabrics.length})`} variant={healthCardVariant(status)}>
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
                      <span className="muted">No quota reported</span>
                    </td>
                    <td className="mono">{stateText(fabric.capacity)}</td>
                    <td>{observedText(fabric.lastObservedAt)}</td>
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
          <p className="muted">
            This view shows aggregate lifecycle counts only. Workspace and Session internals stay
            out of the platform health projection. Reconcile controls appear only for a concrete
            server-issued action targeted at the Fabric.
          </p>
        </>
      )}
    </Card>
  );
}
