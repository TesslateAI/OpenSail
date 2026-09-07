/**
 * Console API surface: platform-admin user management and username
 * directory search. Self-service account actions live in `api/account.ts`.
 *
 * Routes mirror the verified control-plane dispatch (`crates/voie-cloud/src/
 * integration.rs`): POST /api/admin/users, POST /api/admin/users/:id/
 * reset-password, GET|DELETE /api/admin/users/:id/sessions, and the role
 * and status PATCH routes. Directory search uses
 * `GET /api/projects/users/search?q=`.
 *
 * Every decode goes through one validating normalizer; no wire shape is
 * trusted as-is.
 */

import {
  parsePlatformRole,
  parseUserStatus,
  type PlatformRole,
  type UserStatus,
} from "./admin.ts";
import type { Uuid } from "./dto.ts";
import { fetchJson } from "./http.ts";
import { arrayAt, asNum, asStr, isRecord } from "./validate.ts";

// --- vocabularies -----------------------------------------------------------

/** Membership roles offered when adding a team member. */
export const MEMBER_ROLES = ["owner", "admin", "member", "viewer"] as const;
export type MemberRole = (typeof MEMBER_ROLES)[number];

function textOr(value: unknown, fallback: string): string {
  return typeof value === "string" ? value : fallback;
}

/** Reads the one contractual list envelope: `{items: [...]}`. */
function listItems(raw: unknown): unknown[] {
  return arrayAt(isRecord(raw) ? raw : {}, "items");
}

// --- DTOs -------------------------------------------------------------------

/** One admin user row as emitted by GET /api/admin/users. */
export type ConsoleUserDto = {
  id: Uuid;
  username: string | null;
  displayName: string;
  email: string | null;
  status: UserStatus;
  platformRole: PlatformRole;
  createdAt: string | null;
};

/** Body of POST /api/admin/users; every field is required by the server. */
export type CreateConsoleUserInput = {
  username: string;
  displayName: string;
  email?: string | undefined;
  platformRole: PlatformRole;
  /** Initial native credential; surfaced to the operator exactly once. */
  password: string;
};

/** Acceptance receipt for one admin mutation. */
export type ConsoleMutationResult = {
  updated: boolean;
  userId: Uuid;
};

/** One still-live web session of one user, opaque to the browser. */
export type AdminSessionDto = {
  id: Uuid;
  userId: Uuid;
  createdAt: string | null;
};

/** Result of revoking every web session of one user. */
export type RevokeSessionsResult = {
  revoked: number;
};

/**
 * One directory row resolved by username search. The identifier is carried
 * under either `userId` or `id`; both decode here.
 */
export type DirectoryEntryDto = {
  userId: Uuid;
  username: string | null;
  displayName: string | null;
  email: string | null;
};

// --- normalizers -------------------------------------------------------------

function normalizeConsoleUser(raw: unknown): ConsoleUserDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    username: asStr(record.username),
    displayName: textOr(record.displayName, ""),
    email: asStr(record.email),
    status: parseUserStatus(record.status),
    platformRole: parsePlatformRole(record.platformRole),
    createdAt: asStr(record.createdAt),
  };
}

function normalizeMutation(raw: unknown, fallbackUserId: Uuid): ConsoleMutationResult {
  const record = isRecord(raw) ? raw : {};
  return {
    updated: typeof record.updated === "boolean" ? record.updated : false,
    userId: textOr(record.userId, fallbackUserId),
  };
}

function normalizeAdminSession(raw: unknown): AdminSessionDto {
  const record = isRecord(raw) ? raw : {};
  return {
    id: textOr(record.id, ""),
    userId: textOr(record.userId, ""),
    createdAt: asStr(record.createdAt),
  };
}

function normalizeDirectoryEntry(raw: unknown): DirectoryEntryDto {
  const record = isRecord(raw) ? raw : {};
  return {
    userId: textOr(record.userId, textOr(record.id, "")),
    username: asStr(record.username),
    displayName: asStr(record.displayName),
    email: asStr(record.email),
  };
}

// --- transport ----------------------------------------------------------------

export async function listConsoleUsers(signal?: AbortSignal): Promise<ConsoleUserDto[]> {
  const raw = await fetchJson("/api/admin/users", { signal });
  return listItems(raw).map(normalizeConsoleUser);
}

export async function createConsoleUser(
  input: CreateConsoleUserInput,
  signal?: AbortSignal,
): Promise<ConsoleUserDto> {
  const raw = await fetchJson("/api/admin/users", {
    method: "POST",
    body: {
      username: input.username,
      displayName: input.displayName,
      ...(input.email === undefined ? {} : { email: input.email }),
      platformRole: input.platformRole,
      password: input.password,
    },
    signal,
  });
  return normalizeConsoleUser(raw);
}

export async function setConsoleUserRole(
  userId: Uuid,
  platformRole: PlatformRole,
  signal?: AbortSignal,
): Promise<ConsoleMutationResult> {
  const raw = await fetchJson(`/api/admin/users/${encodeURIComponent(userId)}/role`, {
    method: "PATCH",
    body: { platformRole },
    signal,
  });
  return normalizeMutation(raw, userId);
}

export async function setConsoleUserStatus(
  userId: Uuid,
  status: UserStatus,
  signal?: AbortSignal,
): Promise<ConsoleMutationResult> {
  const raw = await fetchJson(`/api/admin/users/${encodeURIComponent(userId)}/status`, {
    method: "PATCH",
    body: { status },
    signal,
  });
  return normalizeMutation(raw, userId);
}

/** Replaces one user's native credential with an operator-generated secret. */
export async function resetConsolePassword(
  userId: Uuid,
  password: string,
  signal?: AbortSignal,
): Promise<ConsoleMutationResult> {
  const raw = await fetchJson(
    `/api/admin/users/${encodeURIComponent(userId)}/reset-password`,
    { method: "POST", body: { password }, signal },
  );
  return normalizeMutation(raw, userId);
}

export async function listConsoleUserSessions(
  userId: Uuid,
  signal?: AbortSignal,
): Promise<AdminSessionDto[]> {
  const raw = await fetchJson(
    `/api/admin/users/${encodeURIComponent(userId)}/sessions`,
    { signal },
  );
  return listItems(raw).map(normalizeAdminSession);
}

export async function revokeConsoleSessions(
  userId: Uuid,
  signal?: AbortSignal,
): Promise<RevokeSessionsResult> {
  const raw = await fetchJson(
    `/api/admin/users/${encodeURIComponent(userId)}/sessions`,
    { method: "DELETE", signal },
  );
  const record = isRecord(raw) ? raw : {};
  return { revoked: asNum(record.revoked) ?? 0 };
}

/**
 * Resolves users by username through the directory. Empty queries answer
 * nothing without a round trip.
 */
export async function searchDirectoryUsers(
  query: string,
  signal?: AbortSignal,
): Promise<DirectoryEntryDto[]> {
  const trimmed = query.trim();
  if (trimmed.length === 0) return [];
  const params = new URLSearchParams({ q: trimmed });
  const raw = await fetchJson(`/api/projects/users/search?${params.toString()}`, { signal });
  return listItems(raw)
    .map(normalizeDirectoryEntry)
    .filter((entry) => entry.userId.trim().length > 0);
}

// --- credential generation -----------------------------------------------------

const PASSWORD_LENGTH = 20;

/** Unambiguous symbol set; look-alike glyphs are excluded. */
const PASSWORD_UPPER = "ABCDEFGHJKLMNPQRSTUVWXYZ";
const PASSWORD_LOWER = "abcdefghijkmnopqrstuvwxyz";
const PASSWORD_DIGIT = "23456789";
const PASSWORD_SYMBOL = "!@#$%^&*-_=+";

function randomIndex(limit: number): number {
  const bytes = new Uint32Array(1);
  crypto.getRandomValues(bytes);
  return bytes[0]! % limit;
}

function shuffle(values: string[]): string[] {
  for (let i = values.length - 1; i > 0; i -= 1) {
    const j = randomIndex(i + 1);
    const swap = values[i]!;
    values[i] = values[j]!;
    values[j] = swap;
  }
  return values;
}

/**
 * Cryptographically random password carrying at least one character from
 * each class. Shown once by the caller; never persisted in the browser.
 */
export function generatePassword(length: number = PASSWORD_LENGTH): string {
  const pools = [PASSWORD_UPPER, PASSWORD_LOWER, PASSWORD_DIGIT, PASSWORD_SYMBOL];
  const all = pools.join("");
  const chars = pools.map((pool) => pool[randomIndex(pool.length)]!);
  while (chars.length < Math.max(length, pools.length)) {
    chars.push(all[randomIndex(all.length)]!);
  }
  return shuffle(chars).join("");
}
