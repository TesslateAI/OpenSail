import { useCallback, useMemo, useState } from "react";
import {
  healthApi,
  isReconcileAction,
  isRetryAction,
  type HealthActionDto,
  type HealthApi,
  type HealthCapabilitiesDto,
} from "../api/health.ts";
import { useResource } from "../hooks.ts";
import { PageHeader, StateView } from "../ui/primitives.tsx";
import { ControlReadiness } from "./ControlReadiness.tsx";
import { DeploymentServices } from "./DeploymentServices.tsx";
import { FabricsCapacity } from "./FabricsCapacity.tsx";
import { actionKey, observedText } from "./presentation.ts";
import { UnderlayAlerts } from "./UnderlayAlerts.tsx";

export type AdminHealthProps = {
  api?: HealthApi | undefined;
  /**
   * Optional capability/action projection obtained from a server-issued admin
   * action endpoint. When omitted, the surface remains read-only because the
   * verified aggregation advertises no operations.
   */
  serverCapabilities?: HealthCapabilitiesDto | undefined;
};

function effectiveCapabilities(
  verified: HealthCapabilitiesDto,
  server: HealthCapabilitiesDto | undefined,
): HealthCapabilitiesDto {
  if (server === undefined) return verified;
  return {
    // A caller-provided action projection cannot elevate the verified admin
    // read gate. Both server-owned read decisions must be true.
    read: verified.read && server.read,
    operate: verified.read && server.read && server.operate,
    actions: [...server.actions],
  };
}

function sameAction(left: HealthActionDto, right: HealthActionDto): boolean {
  return actionKey(left) === actionKey(right);
}

function errorText(reason: unknown): string {
  return reason instanceof Error ? reason.message : "request failed";
}

export function AdminHealth({ api = healthApi, serverCapabilities }: AdminHealthProps = {}) {
  const load = useCallback((signal: AbortSignal) => api.getSnapshot(signal), [api]);
  const resource = useResource(load);
  const snapshot = resource.data;
  const [busyActionKey, setBusyActionKey] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const capabilities = useMemo(
    () =>
      snapshot === null
        ? null
        : effectiveCapabilities(snapshot.capabilities, serverCapabilities),
    [serverCapabilities, snapshot],
  );
  const actions = useMemo(() => {
    if (snapshot === null || capabilities === null) return [];
    const merged = [
      ...capabilities.actions,
      ...snapshot.fabrics.flatMap((fabric) => fabric.actions),
    ];
    const unique: HealthActionDto[] = [];
    for (const action of merged) {
      if (!unique.some((existing) => sameAction(existing, action))) unique.push(action);
    }
    return unique;
  }, [capabilities, snapshot]);

  const runAction = useCallback(
    async (action: HealthActionDto): Promise<void> => {
      if (busyActionKey !== null) return;
      if (
        capabilities === null ||
        !capabilities.read ||
        !capabilities.operate ||
        !actions.some((advertised) => sameAction(advertised, action))
      ) {
        setActionError("The server did not issue this health action for the current admin session.");
        return;
      }
      setBusyActionKey(actionKey(action));
      setActionError(null);
      try {
        const result = await api.runAction(action);
        if (!result.accepted) {
          setActionError(result.detail ?? "The server did not accept the health action.");
        }
        resource.reload();
      } catch (reason: unknown) {
        setActionError(errorText(reason));
        resource.reload();
      } finally {
        setBusyActionKey(null);
      }
    },
    [actions, api, busyActionKey, capabilities, resource],
  );

  const header = (
    <PageHeader
      title="Platform health"
      subtitle="Read-only control, deployment, Fabric, and underlay observations."
      actions={
        <button type="button" className="btn" disabled={resource.loading} onClick={resource.reload}>
          {resource.loading ? "Refreshing…" : "Refresh"}
        </button>
      }
    />
  );

  if (resource.loading && snapshot === null) {
    return (
      <>
        {header}
        <StateView state="loading" title="Loading platform health" />
      </>
    );
  }
  if (resource.error !== null || snapshot === null) {
    return (
      <>
        {header}
        <StateView
          state="error"
          title="Could not load platform health"
          detail={resource.error?.message ?? "request failed"}
          onRetry={resource.reload}
        />
      </>
    );
  }
  if (capabilities === null || !capabilities.read) {
    return (
      <>
        {header}
        <StateView
          state="error"
          title="Platform admin capability required"
          detail="The server did not grant this session access to platform health. No Fabric or underlay rows were rendered."
          onRetry={resource.reload}
        />
      </>
    );
  }

  const retryAction =
    capabilities.operate
      ? actions.find((action) => isRetryAction(action) && action.targetId === null) ?? null
      : null;
  const hasAction = actions.some(
    (action) => isRetryAction(action) || isReconcileAction(action),
  );

  return (
    <>
      {header}
      <p className="muted">Snapshot last observed: {observedText(snapshot.lastObservedAt)}</p>
      {actionError === null ? null : (
        <p role="alert" className="muted">
          Health action failed: {actionError}
        </p>
      )}
      <ControlReadiness
        control={snapshot.control}
        retryAction={retryAction}
        busyActionKey={busyActionKey}
        onAction={(action) => {
          void runAction(action);
        }}
      />
      <FabricsCapacity
        fabrics={snapshot.fabrics}
        actions={actions}
        canOperate={capabilities.operate}
        busyActionKey={busyActionKey}
        onAction={(action) => {
          void runAction(action);
        }}
      />
      <DeploymentServices services={snapshot.services} />
      <UnderlayAlerts alerts={snapshot.alerts} />
      {capabilities.operate && hasAction ? null : (
        <p className="muted">
          Retry and reconcile controls remain hidden until the server issues an explicit admin
          capability and concrete action route.
        </p>
      )}
    </>
  );
}
