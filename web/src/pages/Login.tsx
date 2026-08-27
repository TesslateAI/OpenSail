/**
 * Login: unauthenticated gate. The server declares the available surfaces
 * through `GET /api/auth/capabilities`; this page renders its native form and
 * each external-provider action without guessing an auth mode.
 *
 * Native credentials are kept only in transient form state and one
 * same-origin POST. They are never written to browser storage. A successful
 * POST leaves the opaque `voie_session` cookie as the only auth artifact;
 * a failed POST stays on this page and renders its error inline.
 */

import { useCallback, useState, type ChangeEvent, type FormEvent } from "react";
import { getAuthCapabilities, loginNative } from "../api/auth.ts";
import type { AuthCapabilitiesDto } from "../api/dto.ts";
import { useResource } from "../hooks.ts";
import { Card, PageHeader, StateView } from "../ui/primitives.tsx";

export function Login() {
  const loadCapabilities = useCallback(
    (signal: AbortSignal): Promise<AuthCapabilitiesDto> => getAuthCapabilities(signal),
    [],
  );
  const resource = useResource(loadCapabilities);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);

  const handleUsernameChange = useCallback(
    (event: ChangeEvent<HTMLInputElement>): void => setUsername(event.target.value),
    [],
  );
  const handlePasswordChange = useCallback(
    (event: ChangeEvent<HTMLInputElement>): void => setPassword(event.target.value),
    [],
  );

  const handleSubmit = useCallback(
    async (event: FormEvent<HTMLFormElement>): Promise<void> => {
      event.preventDefault();
      if (submitting || username.trim().length === 0 || password.length === 0) return;

      const attemptedUsername = username.trim();
      const attemptedPassword = password;
      setSubmitting(true);
      setSubmitError(null);
      // Do not retain the password after constructing the one request body.
      setPassword("");
      try {
        await loginNative(attemptedUsername, attemptedPassword);
        // The server set the opaque session cookie on the successful 303.
        // Reload the console root so its normal authenticated bootstrap runs.
        window.location.assign("/");
      } catch (reason: unknown) {
        setSubmitError(reason instanceof Error ? reason.message : "Sign-in failed.");
      } finally {
        setSubmitting(false);
      }
    },
    [password, submitting, username],
  );

  const header = <PageHeader title="Sign in" subtitle="Authentication required." />;
  if (resource.loading) {
    return (
      <>
        {header}
        <StateView
          state="loading"
          title="Checking sign-in options"
          detail="Contacting the control plane for available sign-in methods."
        />
      </>
    );
  }
  if (resource.error !== null) {
    return (
      <>
        {header}
        <StateView
          state="error"
          title="Sign-in options unavailable"
          detail={resource.error.message}
          onRetry={resource.reload}
        />
      </>
    );
  }

  const capabilities = resource.data;
  if (capabilities === null) {
    return (
      <>
        {header}
        <StateView state="loading" title="Checking sign-in options" />
      </>
    );
  }

  const external = capabilities.external;
  if (!capabilities.native && external.length === 0) {
    return (
      <>
        {header}
        <StateView
          state="empty"
          title="No sign-in methods are enabled"
          detail="Ask an administrator to enable native or external sign-in for this server."
        />
      </>
    );
  }

  const canSubmit =
    !submitting && username.trim().length > 0 && password.length > 0;

  return (
    <>
      {header}
      <div className="boot-card">
        <Card title="Sign in">
          <div className="stack">
            {capabilities.native ? (
              <form className="stack" autoComplete="off" onSubmit={handleSubmit}>
                <div className="field">
                  <label htmlFor="login-username">Username</label>
                  <input
                    id="login-username"
                    name="username"
                    type="text"
                    autoComplete="off"
                    maxLength={64}
                    value={username}
                    onChange={handleUsernameChange}
                    disabled={submitting}
                    autoFocus
                  />
                </div>
                <div className="field">
                  <label htmlFor="login-password">Password</label>
                  <input
                    id="login-password"
                    name="password"
                    type="password"
                    autoComplete="new-password"
                    maxLength={256}
                    value={password}
                    onChange={handlePasswordChange}
                    disabled={submitting}
                  />
                </div>
                {submitError !== null ? (
                  <p role="alert" className="muted">
                    {submitError}
                  </p>
                ) : null}
                <div className="actions">
                  <button
                    type="submit"
                    className={canSubmit ? "btn btn-primary" : "btn btn-primary btn-disabled"}
                    disabled={!canSubmit}
                  >
                    {submitting ? "Signing in…" : "Sign in"}
                  </button>
                </div>
              </form>
            ) : null}

            {capabilities.native && external.length > 0 ? (
              <p className="muted">Or continue with an external identity provider.</p>
            ) : null}

            {external.length > 0 ? (
              <div className="actions">
                {external.map((provider) => (
                  <a key={`${provider.id}:${provider.href}`} className="btn" href={provider.href}>
                    {provider.label}
                  </a>
                ))}
              </div>
            ) : null}

            {capabilities.native ? (
              <p className="muted">Credentials are sent once and never stored by the console.</p>
            ) : null}
          </div>
        </Card>
      </div>
    </>
  );
}
