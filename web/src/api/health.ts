/**
 * Platform-admin health adapter for the same-origin VOIE API.
 *
 * The reads in this module are verified control-plane surfaces:
 *
 *   GET /healthz                    -> plain-text liveness
 *   GET /readyz                     -> plain-text fail-closed readiness
 *   GET /api/admin/users            -> platform-admin gate (403 otherwise)
 *   GET /api/fabrics                -> membership-scoped Fabric rows
 *   GET /api/workspaces             -> membership-scoped workspace rows
 *   GET /api/audit-events           -> membership-scoped audit rows
 *
 * `getSnapshot` checks the admin gate before it requests any Fabric or
 * workspace rows. It returns only aggregate Fabric capacity and lifecycle
 * status; it never exposes workspace, Session, endpoint, or credential
 * internals through this surface. Deployment/service health is represented by
 * the verified liveness and readiness probes, and underlay alerts are the
 * recent non-`ok` audit outcomes without their payloads.
 *
 * There is no verified retry or reconcile HTTP route in the current control
 * plane. `HealthActionDto` and `runAction` are an explicit seam for a future
 * server-issued admin action API. The default snapshot grants no operations.
 * A caller must pass an action descriptor emitted by the server and the UI
 * must have the matching server capability before `runAction` is called.
 * Routes are never constructed from a Fabric or workspace id.
 */

import type { AdminWorkspaceDto } from "./admin.ts";
import { adminApi } from "./admin.ts";
import type { AuditEntryDto, FabricDto, Iso8601, Uuid } from "./dto.ts";
import { fetchJson, newIntentId } from "./http.ts";
import { asBoolOr, asNum, asStr, isRecord } from "./validate.ts";

// --- vocabularies ---------------------------------------------------------

export const HEALTH_STATUSES = ["healthy", "degraded", "unhealthy", "unknown"] as const;
export type HealthStatus = (typeof HEALTH_STATUSES)[number];

export const ALERT_SEVERITIES = ["info", "warning", "critical", "unknown"] as const;
export type AlertSeverity = (typeof ALERT_SEVERITIES)[number];

/** Action ids recognized by the retry/reconcile affordance seam. */
export const HEALTH_ACTION_IDS = [
  "retry",
  "retry-control",
  "reconcile",
  "reconcile-fabric",
] as const;
export type HealthActionId = (typeof HEALTH_ACTION_IDS)[number];

export function parseHealthStatus(value: unknown): HealthStatus {
  if (typeof value !== "string") return "unknown";
  const label = value.trim().toLowerCase();
  if (
    label === "healthy" ||
    label === "ok" ||
    label === "ready" ||
    label === "up" ||
    label === "online"
  ) {
    return "healthy";
  }
  if (
    label === "degraded" ||
    label === "warning" ||
    label === "warn" ||
    label === "not-ready" ||
    label === "not_ready" ||
    label === "pending"
  ) {
    return "degraded";
  }
  if (
    label === "unhealthy" ||
    label === "down" ||
    label === "offline" ||
    label === "unreachable" ||
    label === "failed" ||
    label === "error"
  ) {
    return "unhealthy";
  }
  return "unknown";
}

export function parseAlertSeverity(value: unknown): AlertSeverity {
  if (typeof value !== "string") return "unknown";
  const label = value.trim().toLowerCase();
  if (label === "info" || label === "information") return "info";
  if (label === "warning" || label === "warn") return "warning";
  if (label === "critical" || label === "fatal") return "critical";
  return "unknown";
}

function textOr(value: unknown, fallback: string): string {
  return asStr(value) ?? fallback;
}

function observationAt(
  record: Record<string, unknown>,
  fallback: Iso8601 | null = null,
): Iso8601 | null {
  return asStr(record.lastObservedAt) ?? asStr(record.observedAt) ?? fallback;
}

function aggregateStatus(statuses: readonly HealthStatus[]): HealthStatus {
  if (statuses.length === 0) return "unknown";
  if (statuses.some((status) => status === "unhealthy")) return "unhealthy";
  if (statuses.some((status) => status === "degraded")) return "degraded";
  if (statuses.every((status) => status === "healthy")) return "healthy";
  return "unknown";
}

// --- scoped health DTOs ---------------------------------------------------

/** One observed liveness, readiness, or dependency result. */
export type HealthCheckDto = {
  id: string;
  label: string;
  status: HealthStatus;
  detail: string | null;
  httpStatus: number | null;
  lastObservedAt: Iso8601 | null;
};

/** Control-process health aggregated from the verified text probes. */
export type ControlReadinessDto = {
  status: HealthStatus;
  liveness: HealthCheckDto;
  readiness: HealthCheckDto;
  checks: HealthCheckDto[];
  lastObservedAt: Iso8601 | null;
};

/** Aggregate Fabric capacity; no workspace/session internals are included. */
export type FabricCapacityDto = {
  /** Number of membership-visible workspace lifecycle rows on this Fabric. */
  used: number;
  /** No quota is emitted by the verified API, so this remains null. */
  limit: number | null;
  /** No free-capacity value is emitted by the verified API, so this remains null. */
  available: number | null;
  ready: number;
  creating: number;
  fenced: number;
};

/** One aggregate Fabric status row. */
export type FabricHealthDto = {
  id: Uuid;
  name: string;
  status: HealthStatus;
  detail: string | null;
  capacity: FabricCapacityDto;
  lastObservedAt: Iso8601 | null;
  /** Per-Fabric actions are accepted only when explicitly server-issued. */
  actions: HealthActionDto[];
};

/** One deployment/service observation represented by a verified probe. */
export type DeploymentServiceDto = {
  id: string;
  name: string;
  kind: string;
  status: HealthStatus;
  detail: string | null;
  httpStatus: number | null;
  lastObservedAt: Iso8601 | null;
};

/** One underlay alert derived from a non-`ok` audit outcome. */
export type UnderlayAlertDto = {
  id: string;
  seq: number;
  severity: AlertSeverity;
  source: string;
  message: string;
  detail: string | null;
  occurredAt: Iso8601 | null;
  lastObservedAt: Iso8601 | null;
};

/** A concrete same-origin operation route issued by the server. */
export type HealthActionDto = {
  id: string;
  label: string;
  method: "POST";
  href: string;
  /** Server-issued target; null means a control-wide action. */
  targetId: Uuid | null;
};

/** Server-owned gate for health reads and retry/reconcile actions. */
export type HealthCapabilitiesDto = {
  read: boolean;
  operate: boolean;
  actions: HealthActionDto[];
};

/** Complete read-only platform-admin health projection. */
export type HealthSnapshotDto = {
  lastObservedAt: Iso8601 | null;
  control: ControlReadinessDto;
  fabrics: FabricHealthDto[];
  services: DeploymentServiceDto[];
  alerts: UnderlayAlertDto[];
  capabilities: HealthCapabilitiesDto;
};

/** Result returned by a server-issued retry/reconcile action. */
export type HealthActionResultDto = {
  accepted: boolean;
  actionId: string | null;
  detail: string | null;
  lastObservedAt: Iso8601 | null;
};

// --- verified read aggregation --------------------------------------------

const PROBE_TIMEOUT_MS = 10_000;

async function probeText(
  path: string,
  id: string,
  label: string,
  signal?: AbortSignal,
): Promise<HealthCheckDto> {
  const timeout = AbortSignal.timeout(PROBE_TIMEOUT_MS);
  const requestSignal = signal === undefined ? timeout : AbortSignal.any([timeout, signal]);
  try {
    const response = await fetch(path, {
      method: "GET",
      credentials: "same-origin",
      headers: { accept: "text/plain" },
      signal: requestSignal,
    });
    const body = (await response.text()).trim();
    return {
      id,
      label,
      status: response.ok ? "healthy" : "unhealthy",
      detail: body.length === 0 ? null : body,
      httpStatus: response.status,
      lastObservedAt: new Date().toISOString(),
    };
  } catch (reason: unknown) {
    if (signal?.aborted) throw reason;
    return {
      id,
      label,
      status: "unknown",
      detail: reason instanceof Error ? reason.message : "probe failed",
      httpStatus: null,
      lastObservedAt: new Date().toISOString(),
    };
  }
}

async function verifyPlatformAdmin(signal?: AbortSignal): Promise<void> {
  // This verified read is the platform-admin capability gate. The response
  // body is deliberately discarded; no user rows enter the health DTO.
  await fetchJson("/api/admin/users", { signal });
}

async function readControl(signal?: AbortSignal): Promise<ControlReadinessDto> {
  const [liveness, readiness] = await Promise.all([
    probeText("/healthz", "liveness", "Control liveness", signal),
    probeText("/readyz", "readiness", "Control readiness", signal),
  ]);
  const checks = [liveness, readiness];
  return {
    status: aggregateStatus(checks.map((check) => check.status)),
    liveness,
    readiness,
    checks,
    lastObservedAt: new Date().toISOString(),
  };
}

type WorkspaceCounts = {
  used: number;
  ready: number;
  creating: number;
  fenced: number;
};

function emptyWorkspaceCounts(): WorkspaceCounts {
  return { used: 0, ready: 0, creating: 0, fenced: 0 };
}

function fabricStatus(counts: WorkspaceCounts): HealthStatus {
  if (counts.creating > 0 || counts.fenced > 0) return "degraded";
  if (counts.used > 0) return "healthy";
  return "unknown";
}

function aggregateFabrics(
  fabrics: readonly FabricDto[],
  workspaces: readonly AdminWorkspaceDto[],
  observedAt: Iso8601,
): FabricHealthDto[] {
  const countsByFabric = new Map<Uuid, WorkspaceCounts>();
  for (const fabric of fabrics) countsByFabric.set(fabric.id, emptyWorkspaceCounts());
  for (const workspace of workspaces) {
    const counts = countsByFabric.get(workspace.fabricId);
    if (counts === undefined) continue;
    counts.used += 1;
    if (workspace.state === "ready") counts.ready += 1;
    if (workspace.state === "creating") counts.creating += 1;
    if (workspace.state === "fenced") counts.fenced += 1;
  }
  return fabrics.map((fabric) => {
    const counts = countsByFabric.get(fabric.id) ?? emptyWorkspaceCounts();
    return {
      id: fabric.id,
      name: fabric.name,
      status: fabricStatus(counts),
      detail: null,
      capacity: {
        used: counts.used,
        limit: null,
        available: null,
        ready: counts.ready,
        creating: counts.creating,
        fenced: counts.fenced,
      },
      lastObservedAt: observedAt,
      actions: [],
    };
  });
}

function deploymentServices(control: ControlReadinessDto): DeploymentServiceDto[] {
  return [control.liveness, control.readiness].map((check) => ({
    id: `control-${check.id}`,
    name: check.label,
    kind: check.id === "liveness" ? "process" : "readiness",
    status: check.status,
    detail: check.detail,
    httpStatus: check.httpStatus,
    lastObservedAt: check.lastObservedAt,
  }));
}

function severityForOutcome(outcome: string): AlertSeverity {
  if (outcome === "refused") return "warning";
  if (outcome === "error" || outcome === "unknown") return "critical";
  return "unknown";
}

function alertsFromAudit(
  entries: readonly AuditEntryDto[],
  observedAt: Iso8601,
): UnderlayAlertDto[] {
  const alerts: UnderlayAlertDto[] = [];
  for (const entry of entries) {
    const outcome = entry.outcome.trim().toLowerCase();
    if (outcome === "ok") continue;
    const source = entry.resourceType.trim();
    const message = entry.kind.trim();
    alerts.push({
      id: `audit-${entry.seq}`,
      seq: entry.seq,
      severity: severityForOutcome(outcome),
      source: source.length === 0 ? "control plane" : source,
      message: message.length === 0 ? "Recorded operation did not complete successfully" : message,
      detail: outcome.length === 0 ? null : `Outcome: ${outcome}`,
      occurredAt: entry.occurredAt,
      lastObservedAt: observedAt,
    });
  }
  return alerts;
}

// --- action seam ----------------------------------------------------------

function actionPath(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const path = value.trim();
  if (!path.startsWith("/api/") || path.startsWith("//") || /[\u0000-\u001f]/u.test(path)) {
    return null;
  }
  return path;
}

/** Normalizes one server-issued action descriptor; invalid actions are inert. */
export function normalizeHealthAction(
  raw: unknown,
  fallbackId?: string,
): HealthActionDto | null {
  const record = isRecord(raw) ? raw : {};
  const id = (asStr(record.id) ?? fallbackId ?? "").trim();
  const label = (asStr(record.label) ?? asStr(record.name) ?? "").trim();
  const method = (asStr(record.method) ?? "").trim().toUpperCase();
  const href = actionPath(record.href ?? record.path);
  if (id.length === 0 || label.length === 0 || method !== "POST" || href === null) return null;
  return {
    id,
    label,
    method: "POST",
    href,
    targetId: asStr(record.targetId) ?? asStr(record.fabricId),
  };
}

function normalizeActionResult(raw: unknown, fallbackActionId: string): HealthActionResultDto {
  const record = isRecord(raw) ? raw : {};
  return {
    accepted: asBoolOr(record.accepted, false),
    actionId: asStr(record.actionId) ?? asStr(record.id) ?? fallbackActionId,
    detail: asStr(record.detail) ?? asStr(record.reason) ?? asStr(record.message),
    lastObservedAt: observationAt(record),
  };
}

// --- adapter seam ---------------------------------------------------------

/** Health transport consumed by the platform-admin components. */
export interface HealthApi {
  getSnapshot(signal?: AbortSignal): Promise<HealthSnapshotDto>;
  /**
   * Invokes exactly one concrete action previously issued by the server.
   * The UI must check the matching `read`/`operate` capability first.
   */
  runAction(action: HealthActionDto, signal?: AbortSignal): Promise<HealthActionResultDto>;
}

export class HttpHealthApi implements HealthApi {
  async getSnapshot(signal?: AbortSignal): Promise<HealthSnapshotDto> {
    await verifyPlatformAdmin(signal);
    const [control, fabrics, workspaces, audit] = await Promise.all([
      readControl(signal),
      adminApi.listFabrics(signal),
      adminApi.listUnderlayWorkspaces(signal),
      adminApi.listAudit(undefined, signal),
    ]);
    const observedAt = new Date().toISOString();
    return {
      lastObservedAt: observedAt,
      control,
      fabrics: aggregateFabrics(fabrics, workspaces, observedAt),
      services: deploymentServices(control),
      alerts: alertsFromAudit(audit.entries, observedAt),
      // No current verified health-action endpoint grants operations. A
      // future server-issued capability projection may be supplied by the
      // mounting seam; this default remains read-only.
      capabilities: { read: true, operate: false, actions: [] },
    };
  }

  async runAction(
    action: HealthActionDto,
    signal?: AbortSignal,
  ): Promise<HealthActionResultDto> {
    if (
      (!isRetryAction(action) && !isReconcileAction(action)) ||
      action.method !== "POST" ||
      actionPath(action.href) === null
    ) {
      throw new Error("health action is not a server-issued retry or reconcile POST");
    }
    const body: { intentId: string; targetId?: Uuid } = { intentId: newIntentId() };
    if (action.targetId !== null) body.targetId = action.targetId;
    const raw = await fetchJson(action.href, {
      method: "POST",
      body,
      signal,
    });
    return normalizeActionResult(raw, action.id);
  }
}

/** Default read-only transport used by the platform-admin health surface. */
export const healthApi: HealthApi = new HttpHealthApi();

/** Retry controls are inert unless the server issued one of these ids. */
export function isRetryAction(action: HealthActionDto): boolean {
  return action.id === "retry" || action.id === "retry-control";
}

/** Reconcile controls are inert unless the server issued one of these ids. */
export function isReconcileAction(action: HealthActionDto): boolean {
  return action.id === "reconcile" || action.id === "reconcile-fabric";
}
