/**
 * Workspace details adapter for the same-origin VOIE API.
 *
 * Product-named surface: one Workspace bound to a collaboration Scope
 * (personal | team), the recent Conversations that ran in it, the Agent
 * presets available to that scope, and workspace lifecycle operations.
 * The browser receives product DTOs only; Fabric, node, Kata, and execution
 * implementation details are rendered nowhere except the separately gated
 * administrator diagnostics section.
 *
 * Product endpoint contract:
 *   GET    /api/workspaces/:workspaceId                       -> WorkspaceDetails
 *   GET    /api/workspaces/:workspaceId/diagnostics            -> WorkspaceDiagnostics (admin)
 *   GET    /api/scopes/:scopeId                               -> WorkspaceScopeSharing
 *   GET    /api/workspaces/:workspaceId/conversations         -> {items:[Conversation]}
 *   GET    /api/scopes/:scopeId/agent-presets                 -> {items:[AgentPreset]}
 *   POST   /api/workspaces/:workspaceId/replace               -> ReplaceResult
 *   DELETE /api/workspaces/:workspaceId                       -> 204 or {deleted}
 * The detail, conversation, scope, and preset routes are the product API
 * contract. A backing control-plane adapter may map them to project/session
 * resources while those routes converge. Mutations are single-attempt;
 * refusals surface verbatim and the browser derives no permission.
 */

import {
  parseScopeKind,
  parseScopeRole,
  type CapabilitiesDto,
  type ScopeKind,
  type ScopeRole,
  type Uuid,
} from "./dto.ts";
import { ApiError, fetchJson } from "./http.ts";
import { arrayAt, asBoolOr, asNum, asStr, isRecord } from "./validate.ts";

/** Durable workspace lifecycle states (`workspaces.state`). */
export const WORKSPACE_LIFECYCLE_STATES = ["creating", "ready", "fenced"] as const;
export type WorkspaceLifecycleState = (typeof WORKSPACE_LIFECYCLE_STATES)[number];

/** Mirrors the control plane's safe fallback for an unparsed row. */
export function parseWorkspaceLifecycleState(value: unknown): WorkspaceLifecycleState {
  return WORKSPACE_LIFECYCLE_STATES.find((state) => state === value) ?? "ready";
}

// --- DTOs ------------------------------------------------------------------

/** One workspace as the conventional details surface presents it. */
export type WorkspaceDetailsDto = {
  id: Uuid;
  /** Product-visible workspace name. */
  name: string;
  /** Owning collaboration scope identity. */
  scopeId: Uuid;
  /** Durable creator; the UI labels it "You" for the acting user. */
  createdByUserId: Uuid | null;
  createdAt: string | null;
  /** Workspace lifecycle state rendered as a plain product badge. */
  state: WorkspaceLifecycleState;
};

/** Administrator-only underlay facts for one workspace. */
export type WorkspaceDiagnosticsDto = {
  workspaceId: Uuid;
  scopeId: Uuid;
  fabricId: Uuid | null;
  fabricName: string | null;
  state: WorkspaceLifecycleState;
  execGeneration: number | null;
  createdByUserId: Uuid | null;
  createdAt: string | null;
  nodeName: string | null;
  runtime: string | null;
};

/** One recent conversation bound to the workspace. */
export type WorkspaceConversationDto = {
  id: Uuid;
  workspaceId: Uuid;
  agentId: Uuid;
  /** Owning scope when the ledger row carries one (`GET /api/conversations`). */
  projectId?: Uuid | null;
  /** Server-provided display title; `null` when the server names none. */
  title: string | null;
  running: boolean;
  headRevision: number;
  createdAt: string | null;
};

/** One named agent preset available in the owning scope. */
export type WorkspaceAgentPresetDto = {
  id: Uuid;
  scopeId: Uuid;
  name: string;
  model: string | null;
  systemPrompt: string | null;
  bashEnabled: boolean;
  maxTokens: number | null;
};

/** One scope membership row used for member-visible sharing state. */
export type WorkspaceMemberDto = {
  userId: Uuid;
  username: string | null;
  displayName: string | null;
  subject: string;
  role: ScopeRole;
  createdAt: string | null;
};

/** The collaboration scope the workspace is shared into. */
export type WorkspaceScopeSharingDto = {
  id: Uuid;
  name: string;
  kind: ScopeKind;
  /** The acting user's role; display-only, never an authority. */
  role: ScopeRole;
  ownerUserId: Uuid;
  createdAt: string | null;
  members: WorkspaceMemberDto[];
  capabilities: CapabilitiesDto;
};

/** Result of a successful backing-allocation replacement. */
export type WorkspaceReplaceResultDto = {
  id: Uuid;
  execGeneration: number | null;
};


// --- normalizers -----------------------------------------------------------

function textOr(value: unknown, fallback: string): string {
  return asStr(value) ?? fallback;
}

function parseCapabilities(record: Record<string, unknown>): CapabilitiesDto {
  const raw = isRecord(record.capabilities) ? record.capabilities : {};
  return {
    read: asBoolOr(raw.read, false),
    operateSessions: asBoolOr(raw.operateSessions, false),
    manageMembers: asBoolOr(raw.manageMembers, false),
  };
}

function normalizeWorkspace(raw: unknown): WorkspaceDetailsDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    name: textOr(record.name, ""),
    scopeId: textOr(record.scopeId, ""),
    createdByUserId: asStr(record.createdByUserId),
    createdAt: asStr(record.createdAt),
    state: parseWorkspaceLifecycleState(record.state),
  };
}

function normalizeDiagnostics(raw: unknown): WorkspaceDiagnosticsDto {
  const record = isRecord(raw) ? raw : {};
  return {
    workspaceId: textOr(record.workspaceId, ""),
    scopeId: textOr(record.scopeId, ""),
    fabricId: asStr(record.fabricId),
    fabricName: asStr(record.fabricName),
    state: parseWorkspaceLifecycleState(record.state),
    execGeneration: asNum(record.execGeneration),
    createdByUserId: asStr(record.createdByUserId),
    createdAt: asStr(record.createdAt),
    nodeName: asStr(record.nodeName),
    runtime: asStr(record.runtime),
  };
}

function normalizeConversation(raw: unknown): WorkspaceConversationDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, textOr(record.conversationId, "")),
    workspaceId: textOr(record.workspaceId, ""),
    agentId: textOr(record.agentId, ""),
    projectId: asStr(record.projectId),
    title: asStr(record.title),
    running: asBoolOr(record.running, false),
    headRevision: asNum(record.headRevision) ?? 0,
    createdAt: asStr(record.createdAt),
  };
}

function normalizePreset(raw: unknown): WorkspaceAgentPresetDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    scopeId: textOr(record.scopeId, ""),
    name: textOr(record.name, ""),
    model: asStr(record.model),
    systemPrompt: asStr(record.systemPrompt),
    bashEnabled: asBoolOr(record.bashEnabled, true),
    maxTokens: asNum(record.maxTokens),
  };
}

function normalizeMember(raw: unknown): WorkspaceMemberDto {
  const record = isRecord(raw) ? raw : {};
  return {
    userId: textOr(record.userId, ""),
    username: asStr(record.username),
    displayName: asStr(record.displayName),
    subject: textOr(record.subject, ""),
    role: parseScopeRole(record.role),
    createdAt: asStr(record.createdAt),
  };
}

function normalizeScopeSharing(raw: unknown): WorkspaceScopeSharingDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    name: textOr(record.name, ""),
    kind: parseScopeKind(record.kind),
    role: parseScopeRole(record.role),
    ownerUserId: textOr(record.ownerUserId, ""),
    createdAt: asStr(record.createdAt),
    members: arrayAt(record, "members").map(normalizeMember),
    capabilities: parseCapabilities(record),
  };
}

/** Reads the one contractual list envelope: `{items: [...]}`. */
function listItems(raw: unknown): unknown[] {
  return arrayAt(isRecord(raw) ? raw : {}, "items");
}

// --- reads -----------------------------------------------------------------

/** Loads one Workspace through the product-named resource endpoint. */
export async function getWorkspaceDetails(
  workspaceId: Uuid,
  signal?: AbortSignal,
): Promise<WorkspaceDetailsDto> {
  const raw = await fetchJson(`/api/workspaces/${encodeURIComponent(workspaceId)}`, { signal });
  const workspace = normalizeWorkspace(raw);
  if (workspace.id === "" || workspace.scopeId === "") {
    throw new ApiError(502, "workspace response omitted its identity");
  }
  return workspace;
}

/** Loads raw underlay facts only through the administrator capability gate. */
export async function getWorkspaceDiagnostics(
  workspaceId: Uuid,
  signal?: AbortSignal,
): Promise<WorkspaceDiagnosticsDto> {
  const raw = await fetchJson(
    `/api/workspaces/${encodeURIComponent(workspaceId)}/diagnostics`,
    { signal },
  );
  const diagnostics = normalizeDiagnostics(raw);
  if (diagnostics.workspaceId === "" || diagnostics.scopeId === "") {
    throw new ApiError(502, "workspace diagnostics omitted its identity");
  }
  return diagnostics;
}

/** Lists recent product Conversations bound to one Workspace. */
export async function listWorkspaceConversations(
  workspaceId: Uuid,
  signal?: AbortSignal,
): Promise<WorkspaceConversationDto[]> {
  const raw = await fetchJson(
    `/api/workspaces/${encodeURIComponent(workspaceId)}/conversations`,
    { signal },
  );
  return listItems(raw)
    .map(normalizeConversation)
    .filter(
      (conversation) =>
        conversation.id !== "" &&
        conversation.workspaceId === workspaceId &&
        conversation.agentId !== "",
    );
}

/**
 * Lists the caller's product Conversations across scopes from the canonical
 * ledger route. Rows keep their owning scope so callers can group by it.
 */
export async function listConversations(
  signal?: AbortSignal,
): Promise<WorkspaceConversationDto[]> {
  const raw = await fetchJson("/api/conversations", { signal });
  return listItems(raw)
    .map(normalizeConversation)
    .filter((conversation) => conversation.id !== "" && conversation.workspaceId !== "");
}

/** Loads the scope and its member-visible sharing state. */
export async function getWorkspaceScope(
  scopeId: Uuid,
  signal?: AbortSignal,
): Promise<WorkspaceScopeSharingDto> {
  const raw = await fetchJson(`/api/scopes/${encodeURIComponent(scopeId)}`, { signal });
  return normalizeScopeSharing(raw);
}

/** Lists the agent presets available in one product Scope. */
export async function listWorkspaceAgentPresets(
  scopeId: Uuid,
  signal?: AbortSignal,
): Promise<WorkspaceAgentPresetDto[]> {
  const raw = await fetchJson(
    `/api/scopes/${encodeURIComponent(scopeId)}/agent-presets`,
    { signal },
  );
  return listItems(raw).map(normalizePreset).filter((preset) => preset.id !== "");
}

// --- lifecycle mutations ----------------------------------------------------

/** Replaces one workspace's backing allocation, once confirmed by the UI. */
export async function replaceWorkspace(
  workspaceId: Uuid,
  signal?: AbortSignal,
): Promise<WorkspaceReplaceResultDto> {
  const raw = await fetchJson(
    `/api/workspaces/${encodeURIComponent(workspaceId)}/replace`,
    { method: "POST", signal },
  );
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, workspaceId),
    execGeneration: asNum(record.execGeneration),
  };
}

/** Deletes one workspace, once confirmed by the UI. */
export async function deleteWorkspace(
  workspaceId: Uuid,
  signal?: AbortSignal,
): Promise<void> {
  await fetchJson(`/api/workspaces/${encodeURIComponent(workspaceId)}`, {
    method: "DELETE",
    signal,
  });
}
