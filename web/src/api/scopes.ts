/**
 * Resource functions for the product-named scope API. Every function owns
 * one validating normalizer so each DTO returned to the UI is fully checked;
 * malformed server payloads degrade to null-safe fields instead of lying.
 *
 * The control plane persists scopes as projects (`projects.kind` is the
 * collaboration scope); this adapter maps the product contract (`/api/scopes`,
 * personal | team kinds, owner | admin | member | viewer roles) onto the
 * backing project resources so scope components never couple to the storage
 * shape. Envelope conventions match the rest of the console: list resources
 * answer `{items: [...]}`, single resources answer one bare JSON object, and
 * errors carry `{error}` (decoded by `http.ts`).
 */

import {
  parseScopeKind,
  parseScopeRole,
  type ScopeRole,
  type AgentPresetDto,
  type AgentPresetPatchInput,
  type CapabilitiesDto,
  type CreateAgentPresetInput,
  type CreateScopeInput,
  type ScopeDetailDto,
  type ScopeMemberDto,
  type ScopeSummaryDto,
  type ScopeWorkspaceDto,
  type Uuid,
  type UserDirectoryEntryDto,
} from "./dto.ts";
import { fetchJson } from "./http.ts";
import { arrayAt, asBoolOr, asNum, asStr, isRecord } from "./validate.ts";

function textOr(value: unknown, fallback: string): string {
  return asStr(value) ?? fallback;
}

/** Reads the one contractual list envelope: `{items: [...]}`. */
function listItems(raw: unknown): unknown[] {
  return arrayAt(isRecord(raw) ? raw : {}, "items");
}

/** Parses the server-emitted capability set; absent sets gate everything off. */
function parseCapabilities(record: Record<string, unknown>): CapabilitiesDto {
  const raw = isRecord(record.capabilities) ? record.capabilities : {};
  return {
    read: asBoolOr(raw.read, false),
    operateSessions: asBoolOr(raw.operateSessions, false),
    manageMembers: asBoolOr(raw.manageMembers, false),
  };
}

// --- normalizers -----------------------------------------------------------

function normalizeScopeSummary(raw: unknown): ScopeSummaryDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    name: textOr(record.name, ""),
    kind: parseScopeKind(record.kind),
    role: parseScopeRole(record.role),
    ownerUserId: textOr(record.ownerUserId, ""),
    createdAt: asStr(record.createdAt),
    capabilities: parseCapabilities(record),
  };
}

function normalizeScopeMember(raw: unknown): ScopeMemberDto {
  const record = isRecord(raw) ? raw : {};
  return {
    userId: textOr(record.userId, ""),
    username: asStr(record.username),
    displayName: asStr(record.displayName),
    subject: asStr(record.subject),
    role: parseScopeRole(record.role),
    createdAt: asStr(record.createdAt),
  };
}

function normalizeScopeDetail(raw: unknown): ScopeDetailDto {
  const record = isRecord(raw) ? raw : {};
  const summary = normalizeScopeSummary(raw);
  return {
    ...summary,
    members: arrayAt(record, "members").map(normalizeScopeMember),
  };
}

function normalizeScopeWorkspace(
  raw: unknown,
  fallbackScopeId = "",
): ScopeWorkspaceDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    label: asStr(record.label),
    scopeId: textOr(record.scopeId, fallbackScopeId),
    state: asStr(record.state),
    createdByUserId: asStr(record.createdByUserId),
    createdAt: asStr(record.createdAt),
  };
}

function normalizeUserDirectoryEntry(raw: unknown): UserDirectoryEntryDto {
  const record = isRecord(raw) ? raw : {};
  return {
    userId: textOr(record.userId, ""),
    username: asStr(record.username),
    displayName: asStr(record.displayName),
    email: asStr(record.email),
    status: asStr(record.status),
    platformRole: asStr(record.platformRole),
  };
}

function normalizeAgentPreset(
  raw: unknown,
  fallbackScopeId = "",
): AgentPresetDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    scopeId: textOr(record.scopeId, fallbackScopeId),
    name: textOr(record.name, ""),
    model: asStr(record.model),
    // Wire key is `prompt`; the product surface keeps the label systemPrompt.
    systemPrompt: asStr(record.prompt),
    bashEnabled: asBoolOr(record.bashEnabled, true),
    maxTokens: asNum(record.maxTokens),
  };
}

// --- scopes -----------------------------------------------------------------

export async function listScopes(signal?: AbortSignal): Promise<ScopeSummaryDto[]> {
  const raw = await fetchJson("/api/scopes", { signal });
  return listItems(raw).map(normalizeScopeSummary);
}

export async function getScope(scopeId: Uuid, signal?: AbortSignal): Promise<ScopeDetailDto> {
  const raw = await fetchJson(`/api/scopes/${encodeURIComponent(scopeId)}`, { signal });
  return normalizeScopeDetail(raw);
}

export async function createScope(
  input: CreateScopeInput,
  signal?: AbortSignal,
): Promise<ScopeSummaryDto> {
  const raw = await fetchJson("/api/scopes", { method: "POST", body: input, signal });
  return normalizeScopeSummary(raw);
}

export async function renameScope(
  scopeId: Uuid,
  name: string,
  signal?: AbortSignal,
): Promise<void> {
  await fetchJson(`/api/scopes/${encodeURIComponent(scopeId)}`, {
    method: "PATCH",
    body: { name },
    signal,
  });
}

/** Adds or reroles one membership; the server owns every protection rule. */
export async function addScopeMember(
  scopeId: Uuid,
  userId: Uuid,
  role: ScopeRole,
  signal?: AbortSignal,
): Promise<ScopeMemberDto> {
  const raw = await fetchJson(`/api/scopes/${encodeURIComponent(scopeId)}/members`, {
    method: "POST",
    body: { userId, role },
    signal,
  });
  return normalizeScopeMember(raw);
}

/** Removes one membership; refusals surface as ApiError messages verbatim. */
export async function removeScopeMember(
  scopeId: Uuid,
  userId: Uuid,
  signal?: AbortSignal,
): Promise<void> {
  await fetchJson(
    `/api/scopes/${encodeURIComponent(scopeId)}/members/${encodeURIComponent(userId)}`,
    { method: "DELETE", signal },
  );
}

/** Lists the roster for one scope. */
export async function listScopeMembers(
  scopeId: Uuid,
  signal?: AbortSignal,
): Promise<ScopeMemberDto[]> {
  const raw = await fetchJson(`/api/scopes/${encodeURIComponent(scopeId)}/members`, { signal });
  return listItems(raw).map(normalizeScopeMember);
}

// --- workspaces ---------------------------------------------------------------

export async function listScopeWorkspaces(
  scopeId: Uuid,
  signal?: AbortSignal,
): Promise<ScopeWorkspaceDto[]> {
  const raw = await fetchJson(`/api/scopes/${encodeURIComponent(scopeId)}/workspaces`, { signal });
  return listItems(raw).map((item) => normalizeScopeWorkspace(item, scopeId));
}

/**
 * Creates one ordinary scope workspace under `/api/scopes/:id/workspaces`;
 * Fabric allocation details stay on the workspace diagnostics resource.
 */
export async function createScopeWorkspace(
  scopeId: Uuid,
  workspaceId: Uuid,
  label?: string,
  signal?: AbortSignal,
): Promise<{ id: Uuid; scopeId: Uuid }> {
  // Fabric provisioning is synchronous here and realistic runs take well
  // over the transport default; give the create its own bounded budget.
  const raw = await fetchJson(`/api/scopes/${encodeURIComponent(scopeId)}/workspaces`, {
    method: "POST",
    body:
      label === undefined || label.trim() === ""
        ? { id: workspaceId }
        : { id: workspaceId, label: label.trim() },
    signal,
    timeoutMs: 60_000,
  });
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, workspaceId),
    scopeId: textOr(record.scopeId, scopeId),
  };
}
// --- user directory ------------------------------------------------------------

/** Searches the user directory by username or display name for invites. */
export async function searchScopeUsers(
  query: string,
  signal?: AbortSignal,
): Promise<UserDirectoryEntryDto[]> {
  const params = new URLSearchParams({ q: query });
  const raw = await fetchJson(`/api/scopes/users/search?${params.toString()}`, { signal });
  return listItems(raw).map(normalizeUserDirectoryEntry);
}

// --- agent presets --------------------------------------------------------------

export async function listAgentPresets(
  scopeId: Uuid,
  signal?: AbortSignal,
): Promise<AgentPresetDto[]> {
  const raw = await fetchJson(`/api/scopes/${encodeURIComponent(scopeId)}/agent-presets`, {
    signal,
  });
  return listItems(raw).map((item) => normalizeAgentPreset(item, scopeId));
}

export async function createAgentPreset(
  scopeId: Uuid,
  input: CreateAgentPresetInput,
  signal?: AbortSignal,
): Promise<AgentPresetDto> {
  const raw = await fetchJson(
    `/api/scopes/${encodeURIComponent(scopeId)}/agent-presets`,
    {
      method: "POST",
      body: {
        id: input.id,
        name: input.name,
        prompt: input.systemPrompt,
        bashEnabled: input.bashEnabled,
        maxTokens: input.maxTokens,
      },
      signal,
    },
  );
  return normalizeAgentPreset(raw, scopeId);
}

export async function updateAgentPreset(
  scopeId: Uuid,
  presetId: Uuid,
  patch: AgentPresetPatchInput,
  signal?: AbortSignal,
): Promise<AgentPresetDto> {
  const raw = await fetchJson(
    `/api/scopes/${encodeURIComponent(scopeId)}/agent-presets/${encodeURIComponent(presetId)}`,
    {
      method: "PATCH",
      body: {
        name: patch.name,
        prompt: patch.systemPrompt,
        bashEnabled: patch.bashEnabled,
        maxTokens: patch.maxTokens,
      },
      signal,
    },
  );
  return normalizeAgentPreset(raw, scopeId);
}

export async function deleteAgentPreset(
  scopeId: Uuid,
  presetId: Uuid,
  signal?: AbortSignal,
): Promise<void> {
  await fetchJson(
    `/api/scopes/${encodeURIComponent(scopeId)}/agent-presets/${encodeURIComponent(presetId)}`,
    { method: "DELETE", signal },
  );
}
