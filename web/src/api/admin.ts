/**
 * Admin resource adapter for the same-origin VOIE API.
 *
 * Every admin panel talks to this adapter (`AdminApi`), never to raw fetch
 * or to the project-scoped resource functions in `api.ts`. DTOs for admin
 * resources live here, self-contained, so panels render exactly what the
 * control plane emits today — including the team project role `admin`, the
 * project collaboration `kind`, and the workspace lifecycle `state`,
 * none of which the shared console DTOs carry.
 *
 * Admin routes:
 *   GET    /api/admin/users                    -> {items:[User]}
 *   POST   /api/admin/users                    {username,displayName,email?,password,platformRole}
 *   PATCH  /api/admin/users/:id/role           {platformRole} -> {updated,userId}
 *   PATCH  /api/admin/users/:id/status         {status} -> {updated,userId}
 *   POST   /api/admin/users/:id/reset-password {password} -> {updated,userId}
 *   GET    /api/admin/projects                   -> {items:[GlobalProject]}
 *   GET    /api/admin/projects/:id/members       -> {items:[Member]}
 *   POST   /api/admin/projects/:id/members       {userId,role} -> Member
 *   DELETE /api/admin/projects/:id/members/:userId
 *   GET    /api/admin/fabrics                  -> {items:[Fabric]}
 *   GET    /api/admin/workspaces               -> {items:[Workspace]}
 *   GET    /api/admin/audit                    -> {items:[AuditEntry],cursor}
 *   GET    /api/admin/health                   -> {database,blob,auth,fabric,workspaces,storage?}
 *
 * The server mints User identity on create. Team-member recovery is an
 * explicit platform-admin surface: it does not join the admin to the Team
 * and does not widen the ordinary membership routes.
 *
 * Authorization is server-emitted. `/api/admin/*` answers 403 unless the
 * caller carries the platform `admin` role. Every mutation is re-authorized
 * server-side. The UI never derives permissions from role labels.
 */

import { listAudit as fetchAuditPage, normalizeAuditEntry, query } from "./api.ts";
import type { AuditEntryDto, AuditPageDto, CapabilitiesDto, FabricDto, Uuid } from "./dto.ts";
import { fetchJson } from "./http.ts";
import { arrayAt, asBoolOr, asNum, asStr, isRecord, recordAt } from "./validate.ts";

// --- vocabularies ---------------------------------------------------------
/** Server-side admin audit window clamp is 1..=500; stay inside it. */
export const ADMIN_AUDIT_PAGE_LIMIT = 50;

/** Platform roles the control plane assigns to canonical Users. */
export const PLATFORM_ROLES = ["user", "admin"] as const;
export type PlatformRole = (typeof PLATFORM_ROLES)[number];

/** Durable User statuses; a disabled User cannot authenticate. */
export const USER_STATUSES = ["active", "disabled"] as const;
export type UserStatus = (typeof USER_STATUSES)[number];

/** Project membership roles as the control plane emits them today. */
export const PROJECT_ROLES = ["owner", "admin", "member", "viewer"] as const;
export type ProjectRole = (typeof PROJECT_ROLES)[number];

/** Collaboration kinds carried on the project row (`projects.kind`). */
export const PROJECT_KINDS = ["personal", "team"] as const;
export type ProjectKind = (typeof PROJECT_KINDS)[number];
/** Identity modes the control plane reports for auth health. */
export const ADMIN_AUTH_MODES = ["native", "oidc", "both"] as const;
export type AdminAuthMode = (typeof ADMIN_AUTH_MODES)[number];

export function parseAdminAuthMode(value: unknown): AdminAuthMode {
  return ADMIN_AUTH_MODES.find((mode) => mode === value) ?? "native";
}

/** Durable workspace lifecycle states (`workspaces.state`). */
export const WORKSPACE_STATES = ["creating", "ready", "fenced", "archived"] as const;
export type WorkspaceState = (typeof WORKSPACE_STATES)[number];

export function parsePlatformRole(value: unknown): PlatformRole {
  return PLATFORM_ROLES.find((role) => role === value) ?? "user";
}

/** Unknown statuses render as disabled: the safe display for an admin. */
export function parseUserStatus(value: unknown): UserStatus {
  return USER_STATUSES.find((status) => status === value) ?? "disabled";
}

export function parseProjectRole(value: unknown): ProjectRole {
  return PROJECT_ROLES.find((role) => role === value) ?? "viewer";
}

export function parseProjectKind(value: unknown): ProjectKind {
  return PROJECT_KINDS.find((kind) => kind === value) ?? "personal";
}

/** Mirrors the control plane's own fallback for an unparsed row. */
export function parseWorkspaceState(value: unknown): WorkspaceState {
  return WORKSPACE_STATES.find((state) => state === value) ?? "ready";
}

// --- DTOs ------------------------------------------------------------------

/** One canonical User row as emitted by GET /api/admin/users. */
export type AdminUserDto = {
  id: Uuid;
  /** Native login name; null for provider-linked users without one. */
  username: string | null;
  displayName: string;
  email: string | null;
  status: UserStatus;
  platformRole: PlatformRole;
  createdAt: string | null;
  updatedAt: string | null;
};

/** Server-minted identity plus native credential for POST /api/admin/users. */
export type CreateAdminUserInput = {
  username: string;
  displayName: string;
  email?: string | undefined;
  password: string;
  platformRole: PlatformRole;
};

/** Acceptance of one admin mutation (role/status/password). */
export type AdminMutationResult = {
  updated: boolean;
  userId: Uuid;
};

/** One Project row with the collaboration `kind` and caller role. */
export type AdminProjectSummaryDto = {
  id: Uuid;
  name: string;
  kind: ProjectKind;
  role: ProjectRole;
  ownerUserId: Uuid;
  createdAt: string | null;
  capabilities: CapabilitiesDto;
};

/** One membership row of a Project. */
export type AdminProjectMemberDto = {
  userId: Uuid;
  username: string | null;
  displayName: string | null;
  subject: string;
  role: ProjectRole;
  createdAt: string | null;
};

/** One underlay workspace row with its lifecycle state. */
export type AdminWorkspaceDto = {
  id: Uuid;
  projectId: Uuid;
  fabricId: Uuid;
  fabricName: string | null;
  createdByUserId: string | null;
  createdAt: string | null;
  execGeneration: number | null;
  state: WorkspaceState;
};

/** One Project row in the platform-wide view with member/workspace counts. */
export type AdminGlobalProjectDto = {
  id: Uuid;
  name: string;
  kind: ProjectKind;
  ownerUserId: Uuid;
  memberCount: number;
  workspaceCount: number;
};

/** One underlay workspace row in the platform-wide view. */
export type AdminGlobalWorkspaceDto = {
  id: Uuid;
  fabricId: Uuid;
  fabricName: string | null;
  projectId: Uuid;
  label: string | null;
  state: WorkspaceState;
  createdAt: string | null;
};

/** One bounded page of the platform-wide audit feed (ascending seq). */
export type AdminAuditPageDto = {
  entries: AuditEntryDto[];
  nextAfter: number | null;
};

/** Verbatim Fabric capacity facts emitted under GET /api/admin/health.storage. */
export type AdminStorageFactsDto = {
  deviceBytes: number;
  health: string;
  runtimePoolBytes: number;
  runtimePoolUsedBytes: number;
  workspacePoolBytes: number;
  workspacePoolUsedBytes: number;
  workspaceLogicalBudgetBytes: number;
  workspaceLogicalAllocatedBytes: number;
  workspaceRestoreHeadroomBytes: number;
  workspaceRestoreAllocatedBytes: number;
  linearBudgetBytes: number;
  linearAllocatedBytes: number;
  databasesBytes: number;
  deploymentsBytes: number;
  recoveryReserveBytes: number;
  emergencyFloorBytes: number;
  physicalFreeBytes: number;
};

/** Verbatim control health facts emitted by GET /api/admin/health. */
export type AdminHealthFactsDto = {
  databaseOk: boolean;
  blobConfigured: boolean;
  authMode: AdminAuthMode;
  fabricRegistered: boolean;
  workspaceCreating: number;
  workspaceReady: number;
  workspaceFenced: number;
  /** Present only when the Fabric answered `/v1/capacity`. */
  storage: AdminStorageFactsDto | null;
};

// --- adapter seam ----------------------------------------------------------

/** The admin transport: every admin panel depends on this interface. */
export interface AdminApi {
  listUsers(signal?: AbortSignal): Promise<AdminUserDto[]>;
  setPlatformRole(
    userId: Uuid,
    platformRole: PlatformRole,
    signal?: AbortSignal,
  ): Promise<AdminMutationResult>;
  setUserStatus(
    userId: Uuid,
    status: UserStatus,
    signal?: AbortSignal,
  ): Promise<AdminMutationResult>;
  createUser(input: CreateAdminUserInput, signal?: AbortSignal): Promise<AdminUserDto>;
  resetPassword(userId: Uuid, password: string, signal?: AbortSignal): Promise<AdminMutationResult>;
  listProjects(signal?: AbortSignal): Promise<AdminProjectSummaryDto[]>;
  listProjectMembers(projectId: Uuid, signal?: AbortSignal): Promise<AdminProjectMemberDto[]>;
  addProjectMember(
    projectId: Uuid,
    userId: Uuid,
    role: ProjectRole,
    signal?: AbortSignal,
  ): Promise<AdminProjectMemberDto>;
  removeProjectMember(projectId: Uuid, userId: Uuid, signal?: AbortSignal): Promise<void>;
  listFabrics(signal?: AbortSignal): Promise<FabricDto[]>;
  listUnderlayWorkspaces(signal?: AbortSignal): Promise<AdminWorkspaceDto[]>;
  listAudit(before?: number, signal?: AbortSignal): Promise<AuditPageDto>;
  getAdminProjects(signal?: AbortSignal): Promise<AdminGlobalProjectDto[]>;
  getAdminFabrics(signal?: AbortSignal): Promise<FabricDto[]>;
  getAdminWorkspaces(signal?: AbortSignal): Promise<AdminGlobalWorkspaceDto[]>;
  getAdminAudit(after?: number, signal?: AbortSignal): Promise<AdminAuditPageDto>;
  getAdminHealth(signal?: AbortSignal): Promise<AdminHealthFactsDto>;
}

// --- normalizers -----------------------------------------------------------

function textOr(value: unknown, fallback: string): string {
  return asStr(value) ?? fallback;
}

/** Reads the one contractual list envelope: `{items: [...]}`. */
function listItems(raw: unknown): unknown[] {
  return arrayAt(isRecord(raw) ? raw : {}, "items");
}

function parseCapabilities(record: Record<string, unknown>): CapabilitiesDto {
  const raw = isRecord(record.capabilities) ? record.capabilities : {};
  return {
    read: asBoolOr(raw.read, false),
    operateSessions: asBoolOr(raw.operateSessions, false),
    manageMembers: asBoolOr(raw.manageMembers, false),
  };
}

function normalizeAdminUser(raw: unknown): AdminUserDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    username: asStr(record.username),
    displayName: textOr(record.displayName, ""),
    email: asStr(record.email),
    status: parseUserStatus(record.status),
    platformRole: parsePlatformRole(record.platformRole),
    createdAt: asStr(record.createdAt),
    updatedAt: asStr(record.updatedAt),
  };
}

function normalizeMutationResult(raw: unknown, fallbackUserId: Uuid): AdminMutationResult {
  const record = isRecord(raw) ? raw : {};
  return {
    updated: asBoolOr(record.updated, false),
    userId: textOr(record.userId, fallbackUserId),
  };
}

function normalizeProjectSummary(raw: unknown): AdminProjectSummaryDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    name: textOr(record.name, ""),
    kind: parseProjectKind(record.kind),
    role: parseProjectRole(record.role),
    ownerUserId: textOr(record.ownerUserId, ""),
    createdAt: asStr(record.createdAt),
    capabilities: parseCapabilities(record),
  };
}

function normalizeProjectMember(raw: unknown): AdminProjectMemberDto {
  const record = isRecord(raw) ? raw : {};
  return {
    userId: textOr(record.userId, ""),
    username: asStr(record.username),
    displayName: asStr(record.displayName),
    subject: textOr(record.subject, ""),
    role: parseProjectRole(record.role),
    createdAt: asStr(record.createdAt),
  };
}

function normalizeFabric(raw: unknown): FabricDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    name: textOr(record.name, ""),
    createdAt: asStr(record.createdAt),
  };
}

function normalizeUnderlayWorkspace(raw: unknown): AdminWorkspaceDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    projectId: textOr(record.projectId, ""),
    fabricId: textOr(record.fabricId, ""),
    fabricName: asStr(record.fabricName),
    createdByUserId: asStr(record.createdByUserId),
    createdAt: asStr(record.createdAt),
    execGeneration: asNum(record.execGeneration),
    state: parseWorkspaceState(record.state),
  };
}
function normalizeGlobalProject(raw: unknown): AdminGlobalProjectDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    name: textOr(record.name, ""),
    kind: parseProjectKind(record.kind),
    ownerUserId: textOr(record.ownerUserId, ""),
    memberCount: asNum(record.memberCount) ?? 0,
    workspaceCount: asNum(record.workspaceCount) ?? 0,
  };
}

function normalizeGlobalWorkspace(raw: unknown): AdminGlobalWorkspaceDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    fabricId: textOr(record.fabricId, ""),
    fabricName: asStr(record.fabricName),
    projectId: textOr(record.projectId, ""),
    label: asStr(record.label),
    state: parseWorkspaceState(record.state),
    createdAt: asStr(record.createdAt),
  };
}

function normalizeAdminAuditPage(raw: unknown): AdminAuditPageDto {
  const record = isRecord(raw) ? raw : {};
  const items = listItems(raw);
  const cursor = asNum(record.cursor);
  const entries = items.map(normalizeAuditEntry);
  // The server owns the page boundary: `cursor` is null exactly when no
  // further rows exist past this page (a short page or the feed's end).
  // Malformed payloads without a cursor stop paging rather than loop.
  return { entries, nextAfter: cursor };
}

function nestedRecord(raw: unknown): Record<string, unknown> | null {
  return isRecord(raw) ? raw : null;
}

function normalizeAdminStorage(raw: unknown): AdminStorageFactsDto | null {
  if (!isRecord(raw)) return null;
  const runtime = nestedRecord(raw.runtime);
  const workspaces = nestedRecord(raw.workspaces);
  const linear = nestedRecord(raw.linear);
  const recovery = nestedRecord(raw.recovery);
  const deviceBytes = asNum(raw.deviceBytes);
  const health = asStr(raw.health);
  const runtimePoolBytes = asNum(runtime?.poolBytes);
  const runtimePoolUsedBytes = asNum(runtime?.usedBytes);
  const workspacePoolBytes = asNum(workspaces?.poolBytes);
  const workspacePoolUsedBytes = asNum(workspaces?.poolUsedBytes);
  const workspaceLogicalBudgetBytes = asNum(workspaces?.logicalBudgetBytes);
  const workspaceLogicalAllocatedBytes = asNum(workspaces?.logicalAllocatedBytes);
  const workspaceRestoreHeadroomBytes = asNum(workspaces?.restoreHeadroomBytes);
  const workspaceRestoreAllocatedBytes = asNum(workspaces?.restoreAllocatedBytes);
  const linearBudgetBytes = asNum(linear?.budgetBytes);
  const linearAllocatedBytes = asNum(linear?.allocatedBytes);
  const databasesBytes = asNum(linear?.databasesBytes);
  const deploymentsBytes = asNum(linear?.deploymentsBytes);
  const recoveryReserveBytes = asNum(recovery?.reserveBytes);
  const emergencyFloorBytes = asNum(recovery?.emergencyFloorBytes);
  const physicalFreeBytes = asNum(recovery?.physicalFreeBytes);
  if (
    deviceBytes === null ||
    health === null ||
    runtimePoolBytes === null ||
    runtimePoolUsedBytes === null ||
    workspacePoolBytes === null ||
    workspacePoolUsedBytes === null ||
    workspaceLogicalBudgetBytes === null ||
    workspaceLogicalAllocatedBytes === null ||
    workspaceRestoreHeadroomBytes === null ||
    workspaceRestoreAllocatedBytes === null ||
    linearBudgetBytes === null ||
    linearAllocatedBytes === null ||
    databasesBytes === null ||
    deploymentsBytes === null ||
    recoveryReserveBytes === null ||
    emergencyFloorBytes === null ||
    physicalFreeBytes === null
  ) {
    return null;
  }
  return {
    deviceBytes,
    health,
    runtimePoolBytes,
    runtimePoolUsedBytes,
    workspacePoolBytes,
    workspacePoolUsedBytes,
    workspaceLogicalBudgetBytes,
    workspaceLogicalAllocatedBytes,
    workspaceRestoreHeadroomBytes,
    workspaceRestoreAllocatedBytes,
    linearBudgetBytes,
    linearAllocatedBytes,
    databasesBytes,
    deploymentsBytes,
    recoveryReserveBytes,
    emergencyFloorBytes,
    physicalFreeBytes,
  };
}

function normalizeAdminHealth(raw: unknown): AdminHealthFactsDto {
  const record = isRecord(raw) ? raw : {};
  const database = recordAt(record, "database");
  const blob = recordAt(record, "blob");
  const auth = recordAt(record, "auth");
  const fabric = recordAt(record, "fabric");
  const workspaces = recordAt(record, "workspaces");
  return {
    databaseOk: asBoolOr(database?.ok, false),
    blobConfigured: asBoolOr(blob?.configured, false),
    authMode: parseAdminAuthMode(auth?.mode),
    fabricRegistered: asBoolOr(fabric?.registered, false),
    workspaceCreating: asNum(workspaces?.creating) ?? 0,
    workspaceReady: asNum(workspaces?.ready) ?? 0,
    workspaceFenced: asNum(workspaces?.fenced) ?? 0,
    storage: normalizeAdminStorage(record.storage),
  };
}

// --- transport -------------------------------------------------------------

export class HttpAdminApi implements AdminApi {
  async listUsers(signal?: AbortSignal): Promise<AdminUserDto[]> {
    const raw = await fetchJson("/api/admin/users", { signal });
    return listItems(raw).map(normalizeAdminUser);
  }

  async setPlatformRole(
    userId: Uuid,
    platformRole: PlatformRole,
    signal?: AbortSignal,
  ): Promise<AdminMutationResult> {
    const raw = await fetchJson(`/api/admin/users/${encodeURIComponent(userId)}/role`, {
      method: "PATCH",
      body: { platformRole },
      signal,
    });
    return normalizeMutationResult(raw, userId);
  }

  async setUserStatus(
    userId: Uuid,
    status: UserStatus,
    signal?: AbortSignal,
  ): Promise<AdminMutationResult> {
    const raw = await fetchJson(`/api/admin/users/${encodeURIComponent(userId)}/status`, {
      method: "PATCH",
      body: { status },
      signal,
    });
    return normalizeMutationResult(raw, userId);
  }

  async createUser(input: CreateAdminUserInput, signal?: AbortSignal): Promise<AdminUserDto> {
    const raw = await fetchJson("/api/admin/users", {
      method: "POST",
      body: {
        username: input.username,
        displayName: input.displayName,
        ...(input.email === undefined ? {} : { email: input.email }),
        password: input.password,
        platformRole: input.platformRole,
      },
      signal,
    });
    return normalizeAdminUser(raw);
  }

  async resetPassword(
    userId: Uuid,
    password: string,
    signal?: AbortSignal,
  ): Promise<AdminMutationResult> {
    const raw = await fetchJson(
      `/api/admin/users/${encodeURIComponent(userId)}/reset-password`,
      {
        method: "POST",
        body: { password },
        signal,
      },
    );
    return normalizeMutationResult(raw, userId);
  }

  async listProjects(signal?: AbortSignal): Promise<AdminProjectSummaryDto[]> {
    const raw = await fetchJson("/api/projects", { signal });
    return listItems(raw).map(normalizeProjectSummary);
  }

  async listProjectMembers(projectId: Uuid, signal?: AbortSignal): Promise<AdminProjectMemberDto[]> {
    const raw = await fetchJson(`/api/admin/projects/${encodeURIComponent(projectId)}/members`, {
      signal,
    });
    return listItems(raw).map(normalizeProjectMember);
  }

  async addProjectMember(
    projectId: Uuid,
    userId: Uuid,
    role: ProjectRole,
    signal?: AbortSignal,
  ): Promise<AdminProjectMemberDto> {
    const raw = await fetchJson(`/api/admin/projects/${encodeURIComponent(projectId)}/members`, {
      method: "POST",
      body: { userId, role },
      signal,
    });
    return normalizeProjectMember(raw);
  }

  async removeProjectMember(projectId: Uuid, userId: Uuid, signal?: AbortSignal): Promise<void> {
    await fetchJson(
      `/api/admin/projects/${encodeURIComponent(projectId)}/members/${encodeURIComponent(userId)}`,
      { method: "DELETE", signal },
    );
  }

  async listFabrics(signal?: AbortSignal): Promise<FabricDto[]> {
    const raw = await fetchJson("/api/fabrics", { signal });
    return listItems(raw).map(normalizeFabric);
  }

  async listUnderlayWorkspaces(signal?: AbortSignal): Promise<AdminWorkspaceDto[]> {
    const raw = await fetchJson("/api/workspaces", { signal });
    return listItems(raw).map(normalizeUnderlayWorkspace);
  }

  async listAudit(before?: number, signal?: AbortSignal): Promise<AuditPageDto> {
    // The shared audit normalizer is complete; only the route is reused.
    return fetchAuditPage(before, signal);
  }
  async getAdminProjects(signal?: AbortSignal): Promise<AdminGlobalProjectDto[]> {
    const raw = await fetchJson("/api/admin/projects", { signal });
    return listItems(raw).map(normalizeGlobalProject);
  }

  async getAdminFabrics(signal?: AbortSignal): Promise<FabricDto[]> {
    const raw = await fetchJson("/api/admin/fabrics", { signal });
    return listItems(raw).map(normalizeFabric);
  }

  async getAdminWorkspaces(signal?: AbortSignal): Promise<AdminGlobalWorkspaceDto[]> {
    const raw = await fetchJson("/api/admin/workspaces", { signal });
    return listItems(raw).map(normalizeGlobalWorkspace);
  }

  async getAdminAudit(after?: number, signal?: AbortSignal): Promise<AdminAuditPageDto> {
    const raw = await fetchJson(
      query("/api/admin/audit", { after, limit: ADMIN_AUDIT_PAGE_LIMIT }),
      { signal },
    );
    return normalizeAdminAuditPage(raw);
  }

  async getAdminHealth(signal?: AbortSignal): Promise<AdminHealthFactsDto> {
    const raw = await fetchJson("/api/admin/health", { signal });
    return normalizeAdminHealth(raw);
  }
}

/** Default transport used by the panels; injectable for tests. */
export const adminApi: AdminApi = new HttpAdminApi();
