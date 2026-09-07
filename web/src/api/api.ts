/**
 * Resource functions for the same-origin VOIE API. Each function owns one
 * validating normalizer so every DTO returned to the UI is fully checked;
 * malformed server payloads degrade to null-safe fields instead of lying.
 *
 * Envelope conventions are contractual, not guessed: list resources answer
 * `{items: [...]}`, single resources answer one bare JSON object, and error
 * answers carry `{error}` (decoded by `http.ts`). Anything outside that is a
 * malformed payload and degrades through the validators below.
 */

import {
  parseProjectKind,
  parseRole,
  parseRunState,
  type AgentPatchInput,
  type AgentPresetDto,
  type AgentPresetPatchInput,
  type AgentSummaryDto,
  type AuditEntryDto,
  type AuditPageDto,
  type CanonicalEventItemDto,
  type CancelRunResultDto,
  type CapabilitiesDto,
  type CreateAgentInput,
  type CreateAgentPresetInput,
  type CreateProjectInput,
  type CreateSessionInput,
  type FabricDto,
  type FeedPageDto,
  type MeDto,
  type ProjectDetailDto,
  type ProjectMemberDto,
  type ProjectSummaryDto,
  type ProjectWorkspaceDto,
  type RawEventDto,
  type Role,
  type RunDto,
  type RunState,
  type SessionEventsPageDto,
  type SessionSummaryDto,
  type StartRunInput,
  type StartRunResultDto,
  type UserDirectoryEntryDto,
  type Uuid,
  type WorkspaceSummaryDto,
} from "./dto.ts";
import { fetchJson } from "./http.ts";
import { arrayAt, asBoolOr, asJson, asNum, asStr, isRecord } from "./validate.ts";

/** Server-side audit window clamp is 1..=256; stay inside it. */
const AUDIT_PAGE_LIMIT = 50;

function textOr(value: unknown, fallback: string): string {
  return asStr(value) ?? fallback;
}

function globalSeqOf(record: Record<string, unknown>): number {
  // Canonical appends always carry the sole global sequence; no fallback.
  return asNum(record.globalSeq) ?? 0;
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

export function query(path: string, params: Record<string, string | number | undefined>): string {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined) search.set(key, String(value));
  }
  const encoded = search.toString();
  return encoded.length === 0 ? path : `${path}?${encoded}`;
}

/** Reads the one contractual list envelope: `{items: [...]}`. */
function listItems(raw: unknown): unknown[] {
  return arrayAt(isRecord(raw) ? raw : {}, "items");
}

// --- normalizers -----------------------------------------------------------

function normalizeMe(raw: unknown): MeDto {
  const record = isRecord(raw) ? raw : {};
  // Contract of `GET /api/me`: userId plus optional username, displayName,
  // and platformRole. Absent or blank values collapse to null so callers
  // can fall back sensibly instead of rendering empty chips.
  return {
    userId: textOr(record.userId, ""),
    username: optionalText(record.username),
    displayName: optionalText(record.displayName),
    platformRole: optionalText(record.platformRole),
  };
}

/** Non-empty strings pass through as text; everything else collapses. */
function optionalText(value: unknown): string | null {
  return typeof value === "string" && value.trim() !== "" ? value : null;
}

function normalizeProjectSummary(raw: unknown): ProjectSummaryDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    name: textOr(record.name, ""),
    kind: parseProjectKind(record.kind),
    ownerUserId: textOr(record.ownerUserId, ""),
    role: parseRole(record.role),
    createdAt: asStr(record.createdAt),
    capabilities: parseCapabilities(record),
  };
}

function normalizeProjectMember(raw: unknown): ProjectMemberDto {
  const record = isRecord(raw) ? raw : {};
  return {
    userId: textOr(record.userId, ""),
    username: optionalText(record.username),
    displayName: optionalText(record.displayName),
    subject: textOr(record.subject, ""),
    role: parseRole(record.role),
    createdAt: asStr(record.createdAt),
  };
}

function normalizeProjectDetail(raw: unknown): ProjectDetailDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    name: textOr(record.name, ""),
    kind: parseProjectKind(record.kind),
    ownerUserId: textOr(record.ownerUserId, ""),
    role: parseRole(record.role),
    createdAt: asStr(record.createdAt),
    members: arrayAt(record, "members").map(normalizeProjectMember),
    capabilities: parseCapabilities(record),
  };
}

function normalizeAgent(raw: unknown): AgentSummaryDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    projectId: textOr(record.projectId, ""),
    name: textOr(record.name, ""),
    model: asStr(record.model),
    systemPrompt: asStr(record.systemPrompt),
    bashEnabled: asBoolOr(record.bashEnabled, true),
    maxTokens: asNum(record.maxTokens),
  };
}

function normalizeSessionSummary(raw: unknown): SessionSummaryDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    projectId: textOr(record.projectId, ""),
    agentId: textOr(record.agentId, ""),
    workspaceId: textOr(record.workspaceId, ""),
    title: asStr(record.title),
    writerGeneration: asNum(record.writerGeneration),
    attentionGeneration: asNum(record.attentionGeneration),
    headRevision: asNum(record.headRevision) ?? 0,
    running: asBoolOr(record.running, false),
    createdAt: asStr(record.createdAt),
    // Only the detail resource emits capabilities; listings omit them.
    capabilities: isRecord(record.capabilities) ? parseCapabilities(record) : null,
  };
}

function normalizeWorkspace(raw: unknown): WorkspaceSummaryDto {
  const record = isRecord(raw) ? raw : {};
  const workspace: WorkspaceSummaryDto = {
    id: textOr(record.id, ""),
    fabricId: textOr(record.fabricId, ""),
    fabricName: asStr(record.fabricName),
    createdAt: asStr(record.createdAt),
  };
  // Ownership is consumed only when the server states it; destructive
  // gating never guesses it from the selected project.
  const projectId = asStr(record.projectId);
  if (projectId !== null && projectId !== "") workspace.projectId = projectId;
  return workspace;
}

function normalizeFabric(raw: unknown): FabricDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    name: textOr(record.name, ""),
    createdAt: asStr(record.createdAt),
  };
}

export function normalizeAuditEntry(raw: unknown): AuditEntryDto {
  const record = isRecord(raw) ? raw : {};
  return {
    seq: asNum(record.seq) ?? 0,
    projectId: asStr(record.projectId),
    sessionId: asStr(record.sessionId),
    runId: asStr(record.runId),
    actorUserId: asStr(record.actorUserId),
    occurredAt: asStr(record.occurredAt),
    kind: textOr(record.kind, ""),
    resourceType: textOr(record.resourceType, ""),
    resourceId: asStr(record.resourceId),
    outcome: textOr(record.outcome, ""),
    metadata: asJson(record.metadata),
    payload: asStr(record.payload),
  };
}

function normalizeRun(raw: unknown): RunDto {
  const record = isRecord(raw) ? raw : {};
  const state: RunState = parseRunState(record.state);
  return {
    id: textOr(record.id, ""),
    intentId: textOr(record.intentId, ""),
    sessionId: textOr(record.sessionId, ""),
    state,
    result: asStr(record.result),
    acceptedAt: asStr(record.acceptedAt),
    dispatchedAt: asStr(record.dispatchedAt),
    terminalAt: asStr(record.terminalAt),
    cancelledAt: asStr(record.cancelledAt),
  };
}

function normalizeRawEvent(raw: unknown): RawEventDto {
  const record = isRecord(raw) ? raw : {};
  const revision = asNum(record.revision) ?? 0;
  const event: RawEventDto = {
    revision,
    type: textOr(record.type, ""),
    data: record.data ?? null,
    globalSeq: globalSeqOf(record),
    eventIndex: asNum(record.eventIndex) ?? 0,
  };
  applyEventMetadata(event, record);
  return event;
}

function normalizeCanonicalItem(raw: unknown): CanonicalEventItemDto {
  const record = isRecord(raw) ? raw : {};
  return {
    sessionId: textOr(record.sessionId, ""),
    revision: asNum(record.revision) ?? 0,
    globalSeq: globalSeqOf(record),
    appendId: textOr(record.appendId, ""),
    objectKey: textOr(record.objectKey, ""),
    contentHash: textOr(record.contentHash, ""),
    byteLength: asNum(record.byteLength) ?? 0,
    bytes: textOr(record.bytes, ""),
  };
}

function applyEventMetadata(
  event: RawEventDto,
  record: Record<string, unknown>,
): void {
  const sessionId = asStr(record.sessionId);
  const appendId = asStr(record.appendId);
  const objectKey = asStr(record.objectKey);
  const contentHash = asStr(record.contentHash);
  const byteLength = asNum(record.byteLength);
  const bytes = asStr(record.bytes);
  if (sessionId !== null) event.sessionId = sessionId;
  if (appendId !== null) event.appendId = appendId;
  if (objectKey !== null) event.objectKey = objectKey;
  if (contentHash !== null) event.contentHash = contentHash;
  if (byteLength !== null) event.byteLength = byteLength;
  if (bytes !== null) event.bytes = bytes;
}

function decodeEventBytes(value: string): string | null {
  try {
    const binary = atob(value);
    const bytes = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1) {
      bytes[index] = binary.charCodeAt(index);
    }
    return new TextDecoder().decode(bytes);
  } catch {
    return null;
  }
}

/** Expands one canonical `{globalSeq, revision, bytes}` append into events. */
function expandEventItem(raw: unknown): RawEventDto[] {
  const record = isRecord(raw) ? raw : {};
  const encoded = asStr(record.bytes);
  if (encoded !== null) {
    const decoded = decodeEventBytes(encoded);
    if (decoded === null) return [];
    const expanded: RawEventDto[] = [];
    for (const [eventIndex, line] of decoded.split("\n").entries()) {
      if (line.trim().length === 0) continue;
      let parsed: unknown;
      try {
        parsed = JSON.parse(line);
      } catch {
        continue;
      }
      if (!isRecord(parsed) || typeof parsed.type !== "string") continue;
      const event: RawEventDto = {
        revision: asNum(record.revision) ?? 0,
        globalSeq: globalSeqOf(record),
        eventIndex,
        type: parsed.type,
        data: parsed.data ?? null,
        bytes: encoded,
      };
      applyEventMetadata(event, record);
      expanded.push(event);
    }
    return expanded;
  }
  if (typeof record.type !== "string") return [];
  return [normalizeRawEvent(record)];
}

/** Decodes canonical append bytes into known session-log lines. */
export function decodeEventItems(items: readonly CanonicalEventItemDto[]): RawEventDto[] {
  return items.flatMap(expandEventItem);
}

// --- identity --------------------------------------------------------------

export async function getMe(signal?: AbortSignal): Promise<MeDto> {
  return normalizeMe(await fetchJson("/api/me", { signal }));
}

// --- projects --------------------------------------------------------------

export async function listProjects(signal?: AbortSignal): Promise<ProjectSummaryDto[]> {
  const raw = await fetchJson("/api/projects", { signal });
  return listItems(raw).map(normalizeProjectSummary);
}

export async function getProject(projectId: Uuid, signal?: AbortSignal): Promise<ProjectDetailDto> {
  const raw = await fetchJson(`/api/projects/${encodeURIComponent(projectId)}`, { signal });
  return normalizeProjectDetail(raw);
}

/** Adds or reroles one membership; the server owns every protection rule. */
export async function addProjectMember(
  projectId: Uuid,
  userId: Uuid,
  role: Role,
  signal?: AbortSignal,
): Promise<ProjectMemberDto> {
  const raw = await fetchJson(`/api/projects/${encodeURIComponent(projectId)}/members`, {
    method: "POST",
    body: { userId, role },
    signal,
  });
  return normalizeProjectMember(raw);
}

/** Removes one membership; refusals surface as ApiError messages verbatim. */
export async function removeProjectMember(
  projectId: Uuid,
  userId: Uuid,
  signal?: AbortSignal,
): Promise<void> {
  await fetchJson(
    `/api/projects/${encodeURIComponent(projectId)}/members/${encodeURIComponent(userId)}`,
    { method: "DELETE", signal },
  );
}

/** Lists the roster for one project via GET /api/projects/:id/members. */
export async function listProjectMembers(
  projectId: Uuid,
  signal?: AbortSignal,
): Promise<ProjectMemberDto[]> {
  const raw = await fetchJson(
    `/api/projects/${encodeURIComponent(projectId)}/members`,
    { signal },
  );
  return listItems(raw).map(normalizeProjectMember);
}

// --- agents ----------------------------------------------------------------

export async function listAgents(
  projectId: Uuid,
  signal?: AbortSignal,
): Promise<AgentSummaryDto[]> {
  // The listing spans every project this identity belongs to; the caller's
  // project filter below stays authoritative because the resource takes no
  // query parameters.
  const raw = await fetchJson("/api/agents", { signal });
  return listItems(raw)
    .map(normalizeAgent)
    .filter((agent) => agent.projectId === "" || agent.projectId === projectId);
}

export async function getAgent(agentId: Uuid, signal?: AbortSignal): Promise<AgentSummaryDto> {
  const raw = await fetchJson(`/api/agents/${encodeURIComponent(agentId)}`, { signal });
  return normalizeAgent(raw);
}

// --- sessions ---------------------------------------------------------------

export async function listSessions(
  projectId: Uuid,
  signal?: AbortSignal,
): Promise<SessionSummaryDto[]> {
  // Same shape as agents: one membership-scoped listing, filtered here.
  const raw = await fetchJson("/api/sessions", { signal });
  return listItems(raw)
    .map(normalizeSessionSummary)
    .filter((session) => session.projectId === "" || session.projectId === projectId);
}

export async function getSession(sessionId: Uuid, signal?: AbortSignal): Promise<SessionSummaryDto> {
  const raw = await fetchJson(`/api/sessions/${encodeURIComponent(sessionId)}`, { signal });
  return normalizeSessionSummary(raw);
}

export async function createSession(
  projectId: Uuid,
  input: CreateSessionInput,
  signal?: AbortSignal,
): Promise<SessionSummaryDto> {
  const raw = await fetchJson(`/api/projects/${encodeURIComponent(projectId)}/sessions`, {
    method: "POST",
    body: input,
    signal,
  });
  return normalizeSessionSummary(raw);
}

// --- agents ----------------------------------------------------------------

export async function createAgent(
  projectId: Uuid,
  input: CreateAgentInput,
  signal?: AbortSignal,
): Promise<AgentSummaryDto> {
  const raw = await fetchJson(`/api/projects/${encodeURIComponent(projectId)}/agents`, {
    method: "POST",
    body: {
      id: input.id,
      name: input.name,
      model: input.model,
      systemPrompt: input.systemPrompt,
      bashEnabled: input.bashEnabled,
      max_tokens: input.maxTokens,
    },
    signal,
  });
  return normalizeAgent(raw);
}

export async function updateAgent(
  agentId: Uuid,
  patch: AgentPatchInput,
  signal?: AbortSignal,
): Promise<AgentSummaryDto> {
  const raw = await fetchJson(`/api/agents/${encodeURIComponent(agentId)}`, {
    method: "PATCH",
    body: {
      model: patch.model,
      systemPrompt: patch.systemPrompt,
      bashEnabled: patch.bashEnabled,
      max_tokens: patch.maxTokens,
    },
    signal,
  });
  return normalizeAgent(raw);
}

// --- workspaces / fabrics ----------------------------------------------------

export async function listWorkspaces(signal?: AbortSignal): Promise<WorkspaceSummaryDto[]> {
  const raw = await fetchJson("/api/workspaces", { signal });
  return listItems(raw).map(normalizeWorkspace);
}

/**
 * Workspace ids the given project can legitimately act on: stated ownership
 * when the row carries it, otherwise reference from one of the project's
 * own sessions (the same visibility rule the listing resource applies).
 */
export function projectBoundWorkspaces(
  projectId: Uuid,
  workspaces: readonly WorkspaceSummaryDto[],
  sessions: readonly SessionSummaryDto[],
): WorkspaceSummaryDto[] {
  const referenced = new Set<Uuid>();
  for (const session of sessions) {
    if (
      (session.projectId === "" || session.projectId === projectId) &&
      session.workspaceId !== ""
    ) {
      referenced.add(session.workspaceId);
    }
  }
  return workspaces.filter(
    (workspace) =>
      (workspace.projectId !== undefined && workspace.projectId === projectId) ||
      referenced.has(workspace.id),
  );
}

/**
 * Creates one Workspace bound to the deployment-selected Fabric. Refusals are
 * real boundary outcomes: 503 (no single Fabric configured), 409 (identity
 * conflict), or 502 (the Fabric rejected creation).
 */
export async function createWorkspace(
  projectId: Uuid,
  workspaceId: Uuid,
  label?: string,
  signal?: AbortSignal,
): Promise<{ id: Uuid; fabricId: Uuid; projectId: Uuid }> {
  const raw = await fetchJson(`/api/projects/${encodeURIComponent(projectId)}/workspaces`, {
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
    fabricId: textOr(record.fabricId, ""),
    projectId: textOr(record.projectId, projectId),
  };
}

/**
 * Tears down one unreferenced Workspace. Owner-only server-side; refusals
 * (409 while sessions still reference it) surface verbatim in the UI.
 */
export async function deleteWorkspace(
  projectId: Uuid,
  workspaceId: Uuid,
  signal?: AbortSignal,
): Promise<void> {
  await fetchJson(
    `/api/projects/${encodeURIComponent(projectId)}/workspaces/${encodeURIComponent(workspaceId)}`,
    { method: "DELETE", signal },
  );
}

/**
 * Replaces one Workspace's backing Fabric allocation. Requires the
 * operate-sessions capability server-side; 409 while lifecycle is fenced.
 */
export async function replaceWorkspace(
  projectId: Uuid,
  workspaceId: Uuid,
  signal?: AbortSignal,
): Promise<{ id: Uuid; fabricId: Uuid; execGeneration: number | null }> {
  const raw = await fetchJson(
    `/api/projects/${encodeURIComponent(projectId)}/workspaces/${encodeURIComponent(workspaceId)}/replace`,
    { method: "POST", signal },
  );
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, workspaceId),
    fabricId: textOr(record.fabricId, ""),
    execGeneration: asNum(record.execGeneration),
  };
}

export async function listFabrics(signal?: AbortSignal): Promise<FabricDto[]> {
  const raw = await fetchJson("/api/fabrics", { signal });
  return listItems(raw).map(normalizeFabric);
}

export async function createProject(
  input: CreateProjectInput,
  signal?: AbortSignal,
): Promise<ProjectSummaryDto> {
  const raw = await fetchJson("/api/projects", { method: "POST", body: input, signal });
  return normalizeProjectSummary(raw);
}

export async function updateProject(
  projectId: Uuid,
  name: string,
  signal?: AbortSignal,
): Promise<void> {
  await fetchJson(`/api/projects/${encodeURIComponent(projectId)}`, {
    method: "PATCH",
    body: { name },
    signal,
  });
}

function normalizeProjectWorkspace(
  raw: unknown,
  fallbackProjectId = "",
): ProjectWorkspaceDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    label: asStr(record.label),
    projectId: textOr(record.projectId, fallbackProjectId),
    state: asStr(record.state),
    createdByUserId: asStr(record.createdByUserId),
    createdAt: asStr(record.createdAt),
  };
}

export async function listProjectWorkspaces(
  projectId: Uuid,
  signal?: AbortSignal,
): Promise<ProjectWorkspaceDto[]> {
  const raw = await fetchJson(`/api/projects/${encodeURIComponent(projectId)}/workspaces`, {
    signal,
  });
  return listItems(raw).map((item) => normalizeProjectWorkspace(item, projectId));
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

/** Searches the user directory by username or display name for invites. */
export async function searchProjectUsers(
  queryText: string,
  signal?: AbortSignal,
): Promise<UserDirectoryEntryDto[]> {
  const params = new URLSearchParams({ q: queryText });
  const raw = await fetchJson(`/api/projects/users/search?${params.toString()}`, { signal });
  return listItems(raw).map(normalizeUserDirectoryEntry);
}

function normalizeAgentPreset(raw: unknown, fallbackProjectId = ""): AgentPresetDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    projectId: textOr(record.projectId, fallbackProjectId),
    name: textOr(record.name, ""),
    model: asStr(record.model),
    systemPrompt: asStr(record.prompt),
    bashEnabled: asBoolOr(record.bashEnabled, true),
    maxTokens: asNum(record.maxTokens),
  };
}

export async function listAgentPresets(
  projectId: Uuid,
  signal?: AbortSignal,
): Promise<AgentPresetDto[]> {
  const raw = await fetchJson(`/api/projects/${encodeURIComponent(projectId)}/agent-presets`, {
    signal,
  });
  return listItems(raw).map((item) => normalizeAgentPreset(item, projectId));
}

export async function createAgentPreset(
  projectId: Uuid,
  input: CreateAgentPresetInput,
  signal?: AbortSignal,
): Promise<AgentPresetDto> {
  const raw = await fetchJson(`/api/projects/${encodeURIComponent(projectId)}/agent-presets`, {
    method: "POST",
    body: {
      id: input.id,
      name: input.name,
      prompt: input.systemPrompt,
      bashEnabled: input.bashEnabled,
      maxTokens: input.maxTokens,
    },
    signal,
  });
  return normalizeAgentPreset(raw, projectId);
}

export async function updateAgentPreset(
  projectId: Uuid,
  presetId: Uuid,
  patch: AgentPresetPatchInput,
  signal?: AbortSignal,
): Promise<AgentPresetDto> {
  const raw = await fetchJson(
    `/api/projects/${encodeURIComponent(projectId)}/agent-presets/${encodeURIComponent(presetId)}`,
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
  return normalizeAgentPreset(raw, projectId);
}

export async function deleteAgentPreset(
  projectId: Uuid,
  presetId: Uuid,
  signal?: AbortSignal,
): Promise<void> {
  await fetchJson(
    `/api/projects/${encodeURIComponent(projectId)}/agent-presets/${encodeURIComponent(presetId)}`,
    { method: "DELETE", signal },
  );
}

// --- audit -----------------------------------------------------------------

export async function listAudit(before?: number, signal?: AbortSignal): Promise<AuditPageDto> {
  const raw = await fetchJson(query("/api/audit-events", { before, limit: AUDIT_PAGE_LIMIT }), {
    signal,
  });
  const entries = listItems(raw).map(normalizeAuditEntry);
  return { entries, hasMore: entries.length >= AUDIT_PAGE_LIMIT };
}

// --- runs ------------------------------------------------------------------

export async function listRuns(signal?: AbortSignal): Promise<RunDto[]> {
  const raw = await fetchJson("/api/runs", { signal });
  return listItems(raw).map(normalizeRun);
}

/**
 * Accepts one durable run attempt. The answer is an acceptance receipt, not a
 * run resource: poll `/api/runs/:id` for timestamps and result.
 */
export async function startRun(
  sessionId: Uuid,
  input: StartRunInput,
  signal?: AbortSignal,
): Promise<StartRunResultDto> {
  const raw = await fetchJson(`/api/sessions/${encodeURIComponent(sessionId)}/runs`, {
    method: "POST",
    body: input,
    signal,
  });
  const record = isRecord(raw) ? raw : {};
  return {
    accepted: asBoolOr(record.accepted, false),
    runId: textOr(record.runId, input.runId),
    intentId: textOr(record.intentId, ""),
    state: parseRunState(record.state),
    reason: asStr(record.reason),
  };
}

export async function getRun(runId: Uuid, signal?: AbortSignal): Promise<RunDto> {
  const raw = await fetchJson(`/api/runs/${encodeURIComponent(runId)}`, { signal });
  return normalizeRun(raw);
}

export async function cancelRun(runId: Uuid, signal?: AbortSignal): Promise<CancelRunResultDto> {
  const raw = await fetchJson(`/api/runs/${encodeURIComponent(runId)}/cancel`, {
    method: "POST",
    signal,
  });
  const record = isRecord(raw) ? raw : {};
  const stateLabel = textOr(record.state, "");
  return {
    runId: textOr(record.runId, runId),
    state: parseRunState(record.state),
    // Carries labels outside the durable vocabulary, e.g. `cancel-requested`.
    stateLabel,
    accepted: asBoolOr(record.accepted, false),
  };
}

// --- events ------------------------------------------------------------------

export async function listSessionEvents(
  sessionId: Uuid,
  after: number,
  signal?: AbortSignal,
): Promise<SessionEventsPageDto> {
  const raw = await fetchJson(
    query(`/api/sessions/${encodeURIComponent(sessionId)}/events`, { after }),
    { signal },
  );
  const record = isRecord(raw) ? raw : {};
  return {
    sessionId: textOr(record.sessionId, sessionId),
    cursor: asNum(record.cursor) ?? after,
    items: listItems(raw).map(normalizeCanonicalItem),
  };
}

export async function listFeedEvents(after: number, signal?: AbortSignal): Promise<FeedPageDto> {
  const raw = await fetchJson(query("/api/events", { after }), { signal });
  const record = isRecord(raw) ? raw : {};
  return {
    cursor: asNum(record.cursor) ?? after,
    items: listItems(raw).map(normalizeCanonicalItem),
  };
}
