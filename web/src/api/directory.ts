/**
 * User-directory and project-membership API seam.
 *
 * The directory is deliberately a thin same-origin adapter. It does not own
 * authorization, identity matching, or membership policy; those remain
 * server decisions. Components receive a DirectoryApi so a page can use the
 * real adapter or a capability-scoped test/demonstration adapter.
 */

import {
  PLATFORM_ROLES,
  USER_STATUSES,
  type PlatformRole,
  type UserStatus,
} from "./admin.ts";
import { fetchJson } from "./http.ts";
import { type Uuid } from "./dto.ts";
import { arrayAt, asBoolOr, asStr, isRecord } from "./validate.ts";

// --- vocabularies ---------------------------------------------------------

/** A missing status is kept visible instead of being mistaken for active. */
export const DIRECTORY_STATUSES = ["active", "disabled", "unknown"] as const;
export type DirectoryStatus = (typeof DIRECTORY_STATUSES)[number];

/** A missing platform role is visible in a directory result. */
export const DIRECTORY_PLATFORM_ROLES = [...PLATFORM_ROLES, "unknown"] as const;
export type DirectoryPlatformRole = (typeof DIRECTORY_PLATFORM_ROLES)[number];

/** Re-export the server vocabularies for picker implementations. */
export { PLATFORM_ROLES, USER_STATUSES };
export type { PlatformRole, UserStatus, Uuid };

export function parseDirectoryStatus(value: unknown): DirectoryStatus {
  if (value === "active" || value === "disabled") return value;
  return "unknown";
}

export function parseDirectoryPlatformRole(value: unknown): DirectoryPlatformRole {
  if (value === "user" || value === "admin") return value;
  return "unknown";
}

// --- DTOs -----------------------------------------------------------------

/**
 * One canonical user row. `userId` is an internal value used for mutations;
 * directory views intentionally render human labels instead of this identity.
 */
export type DirectoryUserDto = {
  userId: Uuid;
  username: string | null;
  displayName: string | null;
  email: string | null;
  status: DirectoryStatus;
  platformRole: DirectoryPlatformRole;
};

/** Result returned by role/status mutations. */
export type DirectoryMutationDto = {
  updated: boolean;
  userId: Uuid;
};

/** Callback-friendly search function used by directory components. */
export type DirectorySearchFn = (
  query: string,
  signal?: AbortSignal,
) => Promise<readonly DirectoryUserDto[]>;

/**
 * Narrow server seam for directory and Project member management. Components
 * do not construct URLs or infer capabilities; callers can inject a narrower
 * implementation when a surface has fewer permissions.
 */
export interface DirectoryApi {
  /** Lists canonical users for the platform-admin directory. */
  listAdminUsers(signal?: AbortSignal): Promise<DirectoryUserDto[]>;
  /** Searches the platform-admin result set by human-readable fields. */
  searchAdminUsers(query: string, signal?: AbortSignal): Promise<DirectoryUserDto[]>;
  /** Optional on surfaces that expose platform-admin controls. */
  setPlatformRole?(
    userId: Uuid,
    platformRole: PlatformRole,
    signal?: AbortSignal,
  ): Promise<DirectoryMutationDto>;
  /** Optional on surfaces that expose platform-admin controls. */
  setUserStatus?(
    userId: Uuid,
    status: UserStatus,
    signal?: AbortSignal,
  ): Promise<DirectoryMutationDto>;
}

// --- response normalizers -------------------------------------------------

function listItems(raw: unknown): unknown[] {
  if (Array.isArray(raw)) return raw;
  return arrayAt(isRecord(raw) ? raw : {}, "items");
}

function nestedUser(record: Record<string, unknown>): Record<string, unknown> {
  return isRecord(record.user) ? record.user : {};
}

function userField(record: Record<string, unknown>, user: Record<string, unknown>, key: string): unknown {
  return record[key] ?? user[key];
}

function normalizeUser(raw: unknown): DirectoryUserDto {
  const record = isRecord(raw) ? raw : {};
  const user = nestedUser(record);
  return {
    userId: asStr(userField(record, user, "userId")) ?? asStr(userField(record, user, "id")) ?? "",
    username: asStr(userField(record, user, "username")),
    displayName: asStr(userField(record, user, "displayName")),
    email: asStr(userField(record, user, "email")),
    status: parseDirectoryStatus(userField(record, user, "status")),
    platformRole: parseDirectoryPlatformRole(userField(record, user, "platformRole")),
  };
}

function normalizeMutation(raw: unknown, fallbackUserId: Uuid): DirectoryMutationDto {
  const record = isRecord(raw) ? raw : {};
  return {
    updated: asBoolOr(record.updated, true),
    userId: asStr(record.userId) ?? fallbackUserId,
  };
}

function matchesQuery(user: DirectoryUserDto, query: string): boolean {
  const needle = query.trim().toLocaleLowerCase();
  if (needle.length === 0) return true;
  return [user.username, user.displayName, user.email]
    .filter((value): value is string => value !== null)
    .some((value) => value.toLocaleLowerCase().includes(needle));
}

// --- HTTP adapter ---------------------------------------------------------

/**
 * Same-origin implementation. Admin user listing is intentionally fetched
 * from `/api/admin/users` without a speculative query contract, then filtered
 * locally by username, display name, or email. Project member search uses the verified
 * `q` query parameter.
 */
export class HttpDirectoryApi implements DirectoryApi {
  async listAdminUsers(signal?: AbortSignal): Promise<DirectoryUserDto[]> {
    const raw = await fetchJson("/api/admin/users", { signal });
    return listItems(raw).map(normalizeUser).filter((user) => user.userId.trim().length > 0);
  }

  async searchAdminUsers(query: string, signal?: AbortSignal): Promise<DirectoryUserDto[]> {
    const users = await this.listAdminUsers(signal);
    return users.filter((user) => matchesQuery(user, query));
  }

  async setPlatformRole(
    userId: Uuid,
    platformRole: PlatformRole,
    signal?: AbortSignal,
  ): Promise<DirectoryMutationDto> {
    const raw = await fetchJson(`/api/admin/users/${encodeURIComponent(userId)}/role`, {
      method: "PATCH",
      body: { platformRole },
      signal,
    });
    return normalizeMutation(raw, userId);
  }

  async setUserStatus(
    userId: Uuid,
    status: UserStatus,
    signal?: AbortSignal,
  ): Promise<DirectoryMutationDto> {
    const raw = await fetchJson(`/api/admin/users/${encodeURIComponent(userId)}/status`, {
      method: "PATCH",
      body: { status },
      signal,
    });
    return normalizeMutation(raw, userId);
  }
}

/** Default same-origin adapter; pages may inject a capability-scoped seam. */
export const directoryApi: DirectoryApi = new HttpDirectoryApi();

// Function exports keep resource-style callers independent of the class.
export const listAdminUsers = (signal?: AbortSignal): Promise<DirectoryUserDto[]> =>
  directoryApi.listAdminUsers(signal);
export const searchAdminUsers = (
  query: string,
  signal?: AbortSignal,
): Promise<DirectoryUserDto[]> => directoryApi.searchAdminUsers(query, signal);
