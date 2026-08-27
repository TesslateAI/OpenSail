import type { AlertSeverity, HealthActionDto, HealthStatus } from "../api/health.ts";
import type { BadgeTone, CardVariant } from "../ui/primitives.tsx";

export function healthTone(status: HealthStatus): BadgeTone {
  if (status === "healthy") return "ok";
  if (status === "degraded") return "warn";
  if (status === "unhealthy") return "fail";
  return "neutral";
}

export function healthCardVariant(status: HealthStatus): CardVariant {
  if (status === "unhealthy") return "failure";
  if (status === "unknown") return "unknown";
  return "default";
}

export function summaryHealthStatus(statuses: readonly HealthStatus[]): HealthStatus {
  if (statuses.length === 0) return "unknown";
  if (statuses.some((status) => status === "unhealthy")) return "unhealthy";
  if (statuses.some((status) => status === "degraded")) return "degraded";
  if (statuses.every((status) => status === "healthy")) return "healthy";
  return "unknown";
}

export function alertTone(severity: AlertSeverity): BadgeTone {
  if (severity === "critical") return "fail";
  if (severity === "warning") return "warn";
  if (severity === "info") return "neutral";
  return "neutral";
}

export function observedText(value: string | null): string {
  if (value === null || value.trim().length === 0) return "Not reported";
  return value;
}

export function actionKey(action: HealthActionDto): string {
  const target = action.targetId === null ? "control" : action.targetId;
  return `${action.id}:${target}:${action.href}`;
}

export function shortId(id: string): string {
  if (id.length === 0) return "—";
  if (id.length <= 10) return id;
  return `${id.slice(0, 8)}…`;
}
