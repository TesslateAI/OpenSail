/**
 * Resource functions for the scoped secret vault API. Every function owns
 * one validating normalizer so each DTO returned to the UI is fully checked;
 * malformed server payloads degrade to null-safe fields instead of lying.
 *
 * Secret values are write-only: they travel only inside HTTPS request
 * bodies (create/update/rotate) and are never present in any response.
 * The server owns all secret material (Azure Key Vault under workload
 * identity in production; encrypted server-owned storage in local dev)
 * and answers metadata/version/audit only. This module never persists,
 * caches, or logs values, and the UI never renders one back.
 *
 * Wire contract (backend packet):
 *   GET    /api/scopes/:scopeId/secrets  -> { secrets: [...], canWrite }
 *   POST   /api/scopes/:scopeId/secrets  body {name, value} -> { secret }
 *   PUT    /api/secrets/:id              body {value} -> { secret }
 *   POST   /api/secrets/:id/rotate       body {value} -> { secret }
 *   DELETE /api/secrets/:id              -> 204, no body
 *   GET    /api/secrets/:id/audit        -> { events: [...] }
 *
 * Every request goes through the shared same-origin transport (`http.ts`):
 * cookie session, bounded timeout, caller abort, `x-voie-intent: mutate`
 * on non-GET, and 401 redirect handling.
 */

import type { Iso8601, Uuid } from "./dto.ts";
import { fetchJson } from "./http.ts";
import { arrayAt, asBoolOr, asNum, asStr, isRecord } from "./validate.ts";

function textOr(value: unknown, fallback: string): string {
  return asStr(value) ?? fallback;
}

// --- DTOs ------------------------------------------------------------------

/** One secret metadata row; a secret value is never part of this shape. */
export type SecretMetadataDto = {
  id: Uuid;
  scopeId: Uuid;
  name: string;
  /** Monotonic version counter maintained by the server. */
  version: number;
  /** Durable creator; the UI labels it with the compact user id. */
  createdBy: Uuid;
  createdAt: Iso8601 | null;
  updatedAt: Iso8601 | null;
  /** Server-authoritative write capability for this secret. */
  canWrite: boolean;
};

/** GET /api/scopes/:scopeId/secrets envelope. */
export type SecretListDto = {
  secrets: SecretMetadataDto[];
  /** Server-authoritative write capability for the whole scope. */
  canWrite: boolean;
};

/** POST /api/scopes/:scopeId/secrets body; the value leaves the browser once. */
export type CreateSecretInput = {
  name: string;
  value: string;
};

/** PUT /api/secrets/:id body; replaces the value under a new version. */
export type UpdateSecretInput = {
  value: string;
};

/** POST /api/secrets/:id/rotate body; forces a new value and version. */
export type RotateSecretInput = {
  value: string;
};

export const SECRET_AUDIT_ACTIONS = ["created", "updated", "rotated", "deleted"] as const;
export type SecretAuditAction = (typeof SECRET_AUDIT_ACTIONS)[number];

export function parseSecretAuditAction(value: unknown): SecretAuditAction {
  return SECRET_AUDIT_ACTIONS.find((action) => action === value) ?? "updated";
}

/** One audit event; unknown versions render as a dash. */
export type SecretAuditEventDto = {
  action: SecretAuditAction;
  actor: Uuid;
  at: Iso8601 | null;
  version: number | null;
};

/** GET /api/secrets/:id/audit envelope. */
export type SecretAuditDto = {
  events: SecretAuditEventDto[];
};

// --- normalizers -----------------------------------------------------------

function normalizeSecretMetadata(raw: unknown): SecretMetadataDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    scopeId: textOr(record.scopeId, ""),
    name: textOr(record.name, ""),
    version: asNum(record.version) ?? 0,
    createdBy: textOr(record.createdBy, ""),
    createdAt: asStr(record.createdAt),
    updatedAt: asStr(record.updatedAt),
    canWrite: asBoolOr(record.canWrite, false),
  };
}

function normalizeSecretList(raw: unknown): SecretListDto {
  const record = isRecord(raw) ? raw : {};
  return {
    secrets: arrayAt(record, "secrets").map(normalizeSecretMetadata),
    canWrite: asBoolOr(record.canWrite, false),
  };
}

function normalizeSecretResponse(raw: unknown): SecretMetadataDto {
  const record = isRecord(raw) ? raw : {};
  return normalizeSecretMetadata(record.secret);
}

function normalizeSecretAudit(raw: unknown): SecretAuditDto {
  const record = isRecord(raw) ? raw : {};
  return {
    events: arrayAt(record, "events").map((entry: unknown) => {
      const event = isRecord(entry) ? entry : {};
      return {
        action: parseSecretAuditAction(event.action),
        actor: textOr(event.actor, ""),
        at: asStr(event.at),
        version: asNum(event.version),
      };
    }),
  };
}

// --- secrets ----------------------------------------------------------------

/** Lists metadata for one scope; values are never part of any response. */
export async function listSecrets(scopeId: Uuid, signal?: AbortSignal): Promise<SecretListDto> {
  return normalizeSecretList(
    await fetchJson(`/api/scopes/${encodeURIComponent(scopeId)}/secrets`, { signal }),
  );
}

/** Creates one secret; the value is sent once and never returned. */
export async function createSecret(
  scopeId: Uuid,
  input: CreateSecretInput,
  signal?: AbortSignal,
): Promise<SecretMetadataDto> {
  return normalizeSecretResponse(
    await fetchJson(`/api/scopes/${encodeURIComponent(scopeId)}/secrets`, {
      method: "POST",
      body: input,
      signal,
    }),
  );
}

/** Replaces one secret value; the server bumps the version. */
export async function updateSecret(
  id: Uuid,
  input: UpdateSecretInput,
  signal?: AbortSignal,
): Promise<SecretMetadataDto> {
  return normalizeSecretResponse(
    await fetchJson(`/api/secrets/${encodeURIComponent(id)}`, {
      method: "PUT",
      body: input,
      signal,
    }),
  );
}

/** Rotates one secret to a fresh value; the server records a rotate event. */
export async function rotateSecret(
  id: Uuid,
  input: RotateSecretInput,
  signal?: AbortSignal,
): Promise<SecretMetadataDto> {
  return normalizeSecretResponse(
    await fetchJson(`/api/secrets/${encodeURIComponent(id)}/rotate`, {
      method: "POST",
      body: input,
      signal,
    }),
  );
}

/** Deletes one secret; the server answers 204 and the value is unrecoverable. */
export async function deleteSecret(id: Uuid, signal?: AbortSignal): Promise<void> {
  await fetchJson(`/api/secrets/${encodeURIComponent(id)}`, { method: "DELETE", signal });
}

/** Reads the audit trail of one secret; metadata only, never values. */
export async function fetchSecretAudit(id: Uuid, signal?: AbortSignal): Promise<SecretAuditDto> {
  return normalizeSecretAudit(
    await fetchJson(`/api/secrets/${encodeURIComponent(id)}/audit`, { signal }),
  );
}

// --- display vocabulary ------------------------------------------------------

/** Badge tone per audit action; the only place this mapping lives. */
export const SECRET_AUDIT_ACTION_TONES: Record<
  SecretAuditAction,
  "ok" | "neutral" | "warn" | "fail"
> = {
  created: "ok",
  updated: "neutral",
  rotated: "warn",
  deleted: "fail",
};

/** Human label per audit action; the only place this mapping lives. */
export const SECRET_AUDIT_ACTION_LABELS: Record<SecretAuditAction, string> = {
  created: "Created",
  updated: "Updated",
  rotated: "Rotated",
  deleted: "Deleted",
};
