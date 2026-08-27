/**
 * Same-origin account API for the VOIE web surface, typed against the
 * documented `/api/account` contract:
 *
 * - GET    /api/account                        -> `AccountOverview`
 * - PATCH  /api/account/profile                `{displayName,email}`
 * - POST   /api/account/password               `{currentPassword,newPassword}`;
 *           the server revokes the caller's other sessions on success
 * - DELETE /api/account/sessions/:sessionId
 * - POST   /api/account/sessions/revoke-others
 *
 * Every request goes through the shared same-origin `fetchJson` transport,
 * so mutations carry the conventional `content-type` plus `x-voie-intent`
 * headers and 401 answers degrade to the central login redirect. Failures
 * surface as `ApiError`. Web sessions are opaque and server-side: the
 * browser holds no credential, role, or capability and derives none.
 *
 * Mutations are single-attempt and are never retried or replayed by the
 * browser (D011); reads are authoritative baseline reads with no caching.
 */

import { fetchJson } from "./http.ts";
import { arrayAt, asBoolOr, asStr, isRecord } from "./validate.ts";

/** The native user as the server presents it to this browser session. */
export interface AccountProfile {
  readonly userId: string;
  readonly username: string;
  readonly displayName: string;
  readonly email: string;
}

/** One active web session of this user, opaque beyond its id and stamp. */
export interface AccountSession {
  readonly sessionId: string;
  /** True when this session performed the request behind the snapshot. */
  readonly current: boolean;
  /** ISO 8601 creation timestamp rendered verbatim. */
  readonly createdAt: string;
}

/**
 * One external identity linked to the native user. The contract advertises
 * `provider`; any extra provider-defined fields are tolerated at runtime but
 * deliberately not typed or rendered until the server documents them.
 */
export interface AccountIdentity {
  readonly provider: string;
}

/** Authoritative account baseline; the browser treats it as disposable. */
export interface AccountOverview {
  readonly profile: AccountProfile;
  /** True when this user signs in with a native password credential. */
  readonly hasNativeCredential: boolean;
  readonly identities: readonly AccountIdentity[];
  readonly sessions: readonly AccountSession[];
  /**
   * Providers the server advertises as linkable. Empty means the server
   * offers no linking; callers hide every link affordance in that case.
   */
  readonly linkableProviders: readonly string[];
}

/** Body of `PATCH /api/account/profile`. */
export interface ProfileUpdateInput {
  readonly displayName: string;
  readonly email: string;
}

/** Body of `POST /api/account/password`. Success revokes other sessions. */
export interface PasswordChangeInput {
  readonly currentPassword: string;
  readonly newPassword: string;
}

const ACCOUNT_ROOT = "/api/account";

function textOr(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function normalizeProfile(raw: unknown): AccountProfile {
  const record = isRecord(raw) ? raw : {};
  return {
    userId: textOr(record.userId),
    username: textOr(record.username),
    displayName: textOr(record.displayName),
    email: textOr(record.email),
  };
}

function normalizeSessions(raw: unknown): readonly AccountSession[] {
  return arrayAt(isRecord(raw) ? raw : {}, "sessions")
    .map((entry): AccountSession | null => {
      const record = isRecord(entry) ? entry : {};
      const sessionId = textOr(record.sessionId);
      if (sessionId === "") return null;
      return {
        sessionId,
        current: asBoolOr(record.current, false),
        createdAt: textOr(record.createdAt),
      };
    })
    .filter((session): session is AccountSession => session !== null);
}

function normalizeIdentities(raw: unknown): readonly AccountIdentity[] {
  return arrayAt(isRecord(raw) ? raw : {}, "identities")
    .map((entry): AccountIdentity | null => {
      const provider = textOr(isRecord(entry) ? entry.provider : undefined);
      if (provider === "") return null;
      return { provider };
    })
    .filter((identity): identity is AccountIdentity => identity !== null);
}

function normalizeLinkableProviders(raw: unknown): readonly string[] {
  return arrayAt(isRecord(raw) ? raw : {}, "linkableProviders")
    .map((provider) => asStr(provider))
    .filter((provider): provider is string => provider !== null && provider !== "");
}

function normalizeOverview(raw: unknown): AccountOverview {
  const record = isRecord(raw) ? raw : {};
  return {
    profile: normalizeProfile(record.profile),
    hasNativeCredential: asBoolOr(record.hasNativeCredential, false),
    identities: normalizeIdentities(record),
    sessions: normalizeSessions(record),
    linkableProviders: normalizeLinkableProviders(record),
  };
}

/**
 * Reads the authoritative account overview (`GET /api/account`). The signal
 * aborts one read; aborts never become user-visible errors because the owner
 * unmounted.
 */
export async function fetchAccount(signal?: AbortSignal): Promise<AccountOverview> {
  return normalizeOverview(await fetchJson(ACCOUNT_ROOT, { signal }));
}

/**
 * Updates the profile (`PATCH /api/account/profile`). The response carries
 * no documented body; callers re-read the overview afterwards so nothing on
 * screen claims more than the server confirmed.
 */
export async function updateProfile(input: ProfileUpdateInput): Promise<void> {
  await fetchJson(`${ACCOUNT_ROOT}/profile`, { method: "PATCH", body: input });
}

/**
 * Changes the native credential (`POST /api/account/password`). The server
 * revokes the caller's OTHER sessions on success; the current session and
 * its cookie stay valid.
 */
export async function changePassword(input: PasswordChangeInput): Promise<void> {
  await fetchJson(`${ACCOUNT_ROOT}/password`, { method: "POST", body: input });
}

/** Revokes one web session by id (`DELETE /api/account/sessions/:sessionId`). */
export async function revokeSession(sessionId: string): Promise<void> {
  await fetchJson(`${ACCOUNT_ROOT}/sessions/${encodeURIComponent(sessionId)}`, {
    method: "DELETE",
  });
}

/** Revokes every web session except the current one (POST revoke-others). */
export async function revokeOtherSessions(): Promise<void> {
  await fetchJson(`${ACCOUNT_ROOT}/sessions/revoke-others`, { method: "POST", body: {} });
}
