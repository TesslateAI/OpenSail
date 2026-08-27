/**
 * Pre-session authentication resources for the login surface.
 *
 * `GET /api/auth/capabilities` is the server-owned login contract:
 * `{native: boolean, external: [{id, label, href}]}`. Native credentials
 * are sent once as an `application/x-www-form-urlencoded` POST to `/login`;
 * the control plane answers 303 and sets the opaque `voie_session` cookie.
 * External actions are ordinary same-origin navigations to the server-provided
 * href (currently `/login/oidc`). No credential or token is persisted here.
 *
 * These calls intentionally bypass `fetchJson`. The JSON transport redirects
 * every 401 to `/login`, while a 401 from a native login attempt means that
 * the credentials were refused and must be rendered inline.
 */

import {
  arrayAt,
  asBoolOr,
  asStr,
  isRecord,
} from "./validate.ts";
import type { AuthCapabilitiesDto, ExternalAuthProviderDto } from "./dto.ts";
import { ApiError } from "./http.ts";

const AUTH_TIMEOUT_MS = 10_000;
const MAX_ERROR_BODY_BYTES = 512;

function requestSignal(caller?: AbortSignal): AbortSignal {
  const timeout = AbortSignal.timeout(AUTH_TIMEOUT_MS);
  return caller === undefined ? timeout : AbortSignal.any([timeout, caller]);
}

async function responseMessage(response: Response): Promise<string> {
  let text = "";
  try {
    text = (await response.text()).slice(0, MAX_ERROR_BODY_BYTES).trim();
  } catch {
    // Keep the status-derived message below.
  }
  if (text.length === 0) return `request failed (${response.status})`;
  try {
    const parsed: unknown = JSON.parse(text);
    if (isRecord(parsed) && typeof parsed.error === "string") return parsed.error;
  } catch {
    // The auth routes currently answer plain text on failure.
  }
  return text;
}

async function errorFrom(response: Response): Promise<ApiError> {
  return new ApiError(response.status, await responseMessage(response));
}

function normalizeExternal(value: unknown): ExternalAuthProviderDto | null {
  if (!isRecord(value)) return null;
  const id = asStr(value.id)?.trim();
  const label = asStr(value.label)?.trim();
  const href = asStr(value.href)?.trim();
  // Provider actions stay on the serving origin. The core contract currently
  // emits `/login/oidc`; reject malformed or cross-origin-looking paths.
  if (
    id === undefined ||
    id.length === 0 ||
    label === undefined ||
    label.length === 0 ||
    href === undefined ||
    href.length === 0 ||
    !href.startsWith("/") ||
    href.startsWith("//")
  ) {
    return null;
  }
  return { id, label, href };
}

function normalizeCapabilities(value: unknown): AuthCapabilitiesDto {
  const record = isRecord(value) ? value : {};
  const external: ExternalAuthProviderDto[] = [];
  for (const item of arrayAt(record, "external")) {
    const provider = normalizeExternal(item);
    if (provider !== null) external.push(provider);
  }
  return {
    native: asBoolOr(record.native, false),
    external,
  };
}

/** Reads the server-declared login surfaces without arming the 401 redirect. */
export async function getAuthCapabilities(
  signal?: AbortSignal,
): Promise<AuthCapabilitiesDto> {
  let response: Response;
  try {
    response = await fetch("/api/auth/capabilities", {
      method: "GET",
      credentials: "same-origin",
      cache: "no-store",
      headers: { accept: "application/json" },
      signal: requestSignal(signal),
    });
  } catch {
    throw new Error("Could not reach the sign-in service.");
  }
  if (!response.ok) throw await errorFrom(response);
  try {
    return normalizeCapabilities(await response.json());
  } catch {
    throw new Error("The sign-in service returned unreadable capabilities.");
  }
}

/**
 * Sends one native username/password attempt. The body is form-encoded to
 * match the current control-plane route. A failed 401 is thrown as an
 * `ApiError` and never invokes the global login redirect; callers keep the
 * page mounted and render its message inline. On success, the browser has
 * received the opaque session cookie before this resolves.
 */
export async function loginNative(
  username: string,
  password: string,
  signal?: AbortSignal,
): Promise<void> {
  const body = new URLSearchParams();
  body.set("username", username);
  body.set("password", password);

  let response: Response;
  try {
    response = await fetch("/login", {
      method: "POST",
      credentials: "same-origin",
      headers: { "content-type": "application/x-www-form-urlencoded;charset=UTF-8" },
      body,
      redirect: "follow",
      signal: requestSignal(signal),
    });
  } catch {
    throw new Error("Could not reach the sign-in service.");
  }

  // Native success is 303 -> `/`; fetch follows that redirect and exposes
  // the final console response. Accept a direct 2xx or 303 as well.
  if (response.ok || response.redirected || response.status === 303) return;

  if (response.status === 401) {
    // Consume the body so no credential-shaped response is retained by this
    // module, while keeping the user-facing message generic.
    try {
      await response.text();
    } catch {
      // The fixed message is still safe and useful.
    }
    throw new ApiError(401, "Invalid username or password.");
  }
  if (response.status === 404) {
    throw new ApiError(404, "Native sign-in is not enabled on this server.");
  }
  throw await errorFrom(response);
}
