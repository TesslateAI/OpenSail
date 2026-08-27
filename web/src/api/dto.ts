/**
 * Typed local DTOs for the voie-cloud same-origin resource API.
 *
 * Shapes mirror the Rust control-plane types and its JSON conventions
 * (`camelCase` keys, SQL `NULL` mapped to `null`, UUID strings, ISO-8601
 * timestamps). The console never talks to credentials, providers, or Fabric
 * transports directly; it only reads these resources over same-origin fetch.
 */

export type Uuid = string;
export type Iso8601 = string;

/** Recursive plain-JSON value as decoded from the wire (`serde_json::Value`). */
export type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

/** Frozen Release 0 Project membership roles (`auth::Role`). */
export const ROLES = ["owner", "member", "viewer"] as const;
export type Role = (typeof ROLES)[number];

/** Durable run attempt states (`runs.state`). */
export const RUN_STATES = ["accepted", "dispatched", "terminal", "unknown", "cancelled"] as const;
export type RunState = (typeof RUN_STATES)[number];

export function parseRole(value: unknown): Role {
  return ROLES.find((role) => role === value) ?? "viewer";
}

export function parseRunState(value: unknown): RunState {
  return RUN_STATES.find((state) => state === value) ?? "unknown";
}

/**
 * Server-emitted permission set derived from the frozen role permits. The
 * console renders and gates exactly what this says; it never re-derives
 * permissions from the role label.
 */
export type CapabilitiesDto = {
  read: boolean;
  operateSessions: boolean;
  manageMembers: boolean;
};

/**
 * Login surfaces answered by `/api/auth/capabilities`, read before any
 * session cookie exists. The login page renders exactly what this says and
 * never guesses which provider route the server accepts.
 */
export type ExternalAuthProviderDto = {
  /** Stable server identifier (for example, `oidc`). */
  id: string;
  /** Server-provided action label. */
  label: string;
  /** Same-origin route that starts this provider's login flow. */
  href: string;
};

export type AuthCapabilitiesDto = {
  /** Server enables native username/password login (`POST /login`). */
  native: boolean;
  /** Optional external identity-provider actions (`GET /login/oidc`). */
  external: ExternalAuthProviderDto[];
};

/** Current authenticated internal User/account snapshot. */
export type MeDto = {
  userId: Uuid;
  username?: string | null;
  displayName?: string | null;
  platformRole?: string | null;
};
// --- projects -------------------------------------------------------------

export type CreateProjectInput = {
  /** Client-minted project identity required by the control plane. */
  id: Uuid;
  name: string;
};

export type ProjectSummaryDto = {
  id: Uuid;
  name: string;
  role: Role;
  createdAt: Iso8601 | null;
  capabilities: CapabilitiesDto;
};

export type ProjectMemberDto = {
  userId: Uuid;
  subject: string;
  role: Role;
  createdAt: Iso8601 | null;
};

export type ProjectDetailDto = {
  id: Uuid;
  name: string;
  ownerUserId: Uuid;
  role: Role;
  createdAt: Iso8601 | null;
  members: ProjectMemberDto[];
  capabilities: CapabilitiesDto;
};

// --- agents ----------------------------------------------------------------

export type CreateAgentInput = {
  /** Client-minted agent identity required by the control plane. */
  id: Uuid;
  name: string;
  /**
   * Model assignment is control-owned: registration never sends it and the
   * server binds its configured default. Optional purely while parallel
   * callers migrate off the field.
   */
  model?: string;
  systemPrompt: string;
  /** The single Release 0 tool capability; absent server-side defaults true. */
  bashEnabled: boolean;
  maxTokens: number;
};

/** PATCH /api/agents/:id body: every field is optional server-side. */
export type AgentPatchInput = {
  /** Never sent by this client; model selection belongs to the control plane. */
  model?: string;
  systemPrompt: string;
  bashEnabled: boolean;
  maxTokens: number;
};

export type AgentSummaryDto = {
  id: Uuid;
  projectId: Uuid;
  name: string;
  model: string | null;
  systemPrompt: string | null;
  bashEnabled: boolean;
  maxTokens: number | null;
};

// --- sessions --------------------------------------------------------------

export type SessionSummaryDto = {
  id: Uuid;
  projectId: Uuid;
  agentId: Uuid;
  workspaceId: Uuid;
  /** Server-provided display title; `null` when the server names none. */
  title: string | null;
  writerGeneration: number | null;
  attentionGeneration: number | null;
  headRevision: number;
  running: boolean;
  createdAt: Iso8601 | null;
  /**
   * Capabilities scoped to this session's project. The sessions listing omits
   * them; only the session detail resource emits them.
   */
  capabilities: CapabilitiesDto | null;
};

export type CreateSessionInput = {
  /** Client-minted session identity required by the control plane. */
  id: Uuid;
  agentId: Uuid;
  workspaceId: Uuid;
};

// --- workspaces / fabrics ----------------------------------------------------

export type WorkspaceSummaryDto = {
  id: Uuid;
  fabricId: Uuid;
  fabricName: string | null;
  createdAt: Iso8601 | null;
  /**
   * Owning project, when the resource states it. Destructive actions stay
   * hidden until this is present and matches the acting project.
   */
  projectId?: Uuid;
};

export type FabricDto = {
  id: Uuid;
  name: string;
  createdAt: Iso8601 | null;
};

// --- audit -----------------------------------------------------------------

/** One normalized audit row as emitted by `/api/audit-events`. */
export type AuditEntryDto = {
  seq: number;
  projectId: Uuid | null;
  sessionId: Uuid | null;
  runId: Uuid | null;
  actorUserId: Uuid | null;
  occurredAt: Iso8601 | null;
  kind: string;
  resourceType: string;
  resourceId: Uuid | null;
  outcome: string;
  metadata: JsonValue | null;
  payload: string | null;
};

export type AuditPageDto = {
  entries: AuditEntryDto[];
  hasMore: boolean;
};

// --- runs ------------------------------------------------------------------

export type RunDto = {
  id: Uuid;
  intentId: Uuid;
  sessionId: Uuid;
  state: RunState;
  result: string | null;
  acceptedAt: Iso8601 | null;
  dispatchedAt: Iso8601 | null;
  terminalAt: Iso8601 | null;
  cancelledAt: Iso8601 | null;
};

export type StartRunInput = {
  /** Client-minted durable run identity required by the control plane. */
  runId: Uuid;
  /** Opaque single-attempt caller intent; one UUID per user action. */
  intentId: Uuid;
  prompt: string;
};

/** Response of `POST /api/sessions/:id/runs`; poll `/api/runs/:id` after. */
export type StartRunResultDto = {
  accepted: boolean;
  runId: Uuid;
  intentId: Uuid;
  state: RunState;
  reason: string | null;
};

export type CancelRunResultDto = {
  runId: Uuid;
  state: RunState;
  /** Raw server state label; carries values like `cancel-requested`. */
  stateLabel: string;
  accepted: boolean;
};

// --- events ------------------------------------------------------------------

/**
 * One raw persisted session event: the pinned session-log vocabulary
 * (`user/message`, `assistant/message`, `tool/call`, `tool/result`,
 * `turn/start`, `turn/end`). Unknown vocabularies are projected to nothing by
 * the canonical projector; they are never fabricated into UI.
 */
export type RawEventDto = {
  revision: number;
  type: string;
  data: unknown;
  sessionId?: Uuid;
  /** Sole global append sequence; canonical appends always carry it. */
  globalSeq: number;
  eventIndex?: number;
  appendId?: Uuid;
  objectKey?: string;
  contentHash?: string;
  byteLength?: number;
  /** Original base64 event batch when the resource API supplies it. */
  bytes?: string;
};

/** Canonical persisted append returned by `/api/events` resources. */
export type CanonicalEventItemDto = {
  sessionId: Uuid;
  revision: number;
  globalSeq: number;
  appendId: Uuid;
  objectKey: string;
  contentHash: string;
  byteLength: number;
  bytes: string;
};

export type SessionEventsPageDto = {
  sessionId: Uuid;
  cursor: number;
  items: CanonicalEventItemDto[];
};

export type FeedPageDto = {
  cursor: number;
  items: CanonicalEventItemDto[];
};
// --- scopes ------------------------------------------------------------------

/** Scope collaboration kinds (`projects.kind`): a personal scope is the
 * single-user home; a team scope is the multi-user collaboration surface.
 * There is no first-class Teams table; the project row carries the kind. */
export const SCOPE_KINDS = ["personal", "team"] as const;
export type ScopeKind = (typeof SCOPE_KINDS)[number];

/** Scope membership roles (`auth::Role`): the team-style management
 * vocabulary; `admin` manages members, the durable owner stays `owner`. */
export const SCOPE_ROLES = ["owner", "admin", "member", "viewer"] as const;
export type ScopeRole = (typeof SCOPE_ROLES)[number];

export function parseScopeKind(value: unknown): ScopeKind {
  return SCOPE_KINDS.find((kind) => kind === value) ?? "personal";
}

export function parseScopeRole(value: unknown): ScopeRole {
  return SCOPE_ROLES.find((role) => role === value) ?? "viewer";
}

/** One scope as listed by the product API; capabilities gate every action. */
export type ScopeSummaryDto = {
  id: Uuid;
  name: string;
  kind: ScopeKind;
  role: ScopeRole;
  ownerUserId: Uuid;
  createdAt: Iso8601 | null;
  capabilities: CapabilitiesDto;
};

/** One membership row; identity labels come from the user directory. */
export type ScopeMemberDto = {
  userId: Uuid;
  /** Canonical username when the user has one; null for legacy users. */
  username: string | null;
  /** Human display name; the UI falls back to username when empty. */
  displayName: string | null;
  /** Legacy provider subject when present; native users carry the username. */
  subject: string | null;
  role: ScopeRole;
  createdAt: Iso8601 | null;
};

export type ScopeDetailDto = ScopeSummaryDto & {
  members: ScopeMemberDto[];
};

export type CreateScopeInput = {
  /** Client-minted team-scope identity required by the control plane;
   * POST /api/scopes always creates a team collaboration scope. */
  id: Uuid;
  name: string;
};

/** Ordinary scope workspace metadata; Fabric allocation details stay on the
 * workspace diagnostics resource, not in the scope-management listing. */
export type ScopeWorkspaceDto = {
  id: Uuid;
  /** Human name; the server defaults to "Workspace" when creation omits it. */
  label: string | null;
  scopeId: Uuid;
  /** Lifecycle state as emitted by the control plane. */
  state: string | null;
  /** Durable creator; the UI labels it "You" for the acting user. */
  createdByUserId: Uuid | null;
  createdAt: Iso8601 | null;
};

/** One user-directory row used to find members by username or display name. */
export type UserDirectoryEntryDto = {
  userId: Uuid;
  username: string | null;
  displayName: string | null;
  email: string | null;
  status: string | null;
  platformRole: string | null;
};

/** Named agent-settings bundle a scope can apply when registering agents. */
export type AgentPresetDto = {
  id: Uuid;
  scopeId: Uuid;
  name: string;
  model: string | null;
  systemPrompt: string | null;
  bashEnabled: boolean;
  maxTokens: number | null;
};

export type CreateAgentPresetInput = {
  /** Client-minted preset identity required by the control plane. */
    id: Uuid;
  name: string;
  systemPrompt: string;
  bashEnabled: boolean;
  maxTokens: number;
};

/** PATCH body for one preset; every field is optional server-side. */
export type AgentPresetPatchInput = {
  name: string;
  systemPrompt: string;
  bashEnabled: boolean;
  maxTokens: number;
};
