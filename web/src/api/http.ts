/**
 * Same-origin JSON transport. Every request stays on the serving origin,
 * carries web-session cookies implicitly, and is bounded by a timeout plus an
 * optional caller abort signal. Decoded bodies come back as `unknown`; each
 * resource function in `api.ts` owns one validating normalizer for its shape.
 */

export class ApiError extends Error {
  readonly status: number;
  readonly approvalId: string | null;

  constructor(status: number, message: string, approvalId: string | null = null) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.approvalId = approvalId;
  }
}

const DEFAULT_TIMEOUT_MS = 10_000;
const MAX_ERROR_BODY_BYTES = 512;

function combinedSignal(timeoutMs: number, caller?: AbortSignal): AbortSignal {
  const timeout = AbortSignal.timeout(timeoutMs);
  if (caller === undefined) return timeout;
  return AbortSignal.any([timeout, caller]);
}

function errorMessageOf(parsed: unknown): string | null {
  if (typeof parsed !== "object" || parsed === null || !("error" in parsed)) return null;
  const error: unknown = parsed.error;
  return typeof error === "string" ? error : null;
}

function approvalIdOf(parsed: unknown): string | null {
  if (typeof parsed !== "object" || parsed === null || !("approvalId" in parsed)) return null;
  const value: unknown = parsed.approvalId;
  return typeof value === "string" && value.trim() !== "" ? value : null;
}

async function errorFrom(response: Response): Promise<ApiError> {
  let message = `request failed (${response.status})`;
  let approvalId: string | null = null;
  try {
    const text = (await response.text()).slice(0, MAX_ERROR_BODY_BYTES);
    if (text.length > 0) {
      try {
        const parsed: unknown = JSON.parse(text);
        message = errorMessageOf(parsed) ?? text;
        approvalId = approvalIdOf(parsed);
      } catch {
        message = text;
      }
    }
  } catch {
    // Keep the status-derived message.
  }
  return new ApiError(response.status, message, approvalId);
}

export type RequestOptions = {
  method?: "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
  body?: unknown;
  signal?: AbortSignal | undefined;
  timeoutMs?: number | undefined;
};

/** Performs one bounded same-origin request and decodes the JSON body. */
export async function fetchJson(path: string, options: RequestOptions = {}): Promise<unknown> {
  const { method = "GET", body, signal, timeoutMs = DEFAULT_TIMEOUT_MS } = options;
  const headers: Record<string, string> = { accept: "application/json" };
  if (body !== undefined || method !== "GET") {
    headers["content-type"] = "application/json";
  }
  if (method !== "GET") {
    headers["x-voie-intent"] = "mutate";
  }
  const request: RequestInit = { method, credentials: "same-origin", headers };
  request.signal = combinedSignal(timeoutMs, signal);
  if (body !== undefined) {
    request.body = JSON.stringify(body);
  }
  const response = await fetch(path, request);
  if (!response.ok) {
    const error = await errorFrom(response);
    if (error.status === 401) redirectToLogin();
    throw error;
  }
  if (response.status === 204) return undefined;
  return response.json();
}

let loginRedirectArmed = false;

/**
 * OIDC redirect handling: send the browser to the relying-party login route.
 * While the browser is already on the login surface (the pre-session
 * bootstrap answers 401 there too), no redirect is issued — the page is the
 * destination, and re-assigning would reload it in a loop.
 */
export function redirectToLogin(): void {
  if (loginRedirectArmed) return;
  if (window.location.pathname === "/login") return;
  loginRedirectArmed = true;
  window.location.assign("/login");
}

/** Ends the web session and returns to the login route. */
export async function logout(): Promise<void> {
  try {
    await fetchJson("/logout", { method: "POST", timeoutMs: 5_000 });
  } catch {
    // The session may already be gone; the redirect below is still correct.
  }
  redirectToLogin();
}

/** Fresh opaque intent id for one user action (single-attempt semantics). */
export function newIntentId(): string {
  if (typeof crypto.randomUUID === "function") return crypto.randomUUID();
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}
