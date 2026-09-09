/**
 * Admin console — self-service account page.
 *
 * One authoritative seam: GET /api/account drives every card (profile,
 * credential state, identities, sessions). The credential change posts to
 * POST /api/account/password — the server signs the other sessions out on
 * success — and profile edits PATCH /api/account/profile then refetch the
 * overview so nothing renders stale. Full sign-out rides the verified
 * /logout route from the shared transport module.
 */

import { useCallback, useState, type FormEvent } from "react";
import {
  changePassword,
  fetchAccount,
  revokeOtherSessions,
  revokeSession,
  updateProfile,
  type AccountOverview,
} from "../api/account.ts";
import { logout } from "../api/http.ts";
import { useResource } from "../hooks.ts";
import { Badge } from "../design-system/components/Badge";
import { Button } from "../design-system/components/Button";
import { Card, PageHeader } from "../design-system/components/Card";
import { Field } from "../design-system/components/Field";
import { StateView } from "../design-system/components/StateView";

function errorOf(reason: unknown): string {
  return reason instanceof Error ? reason.message : "request failed";
}

export function AccountPage() {
  const resource = useResource(fetchAccount);
  const overview = resource.data;

  // Profile-edit form state.
  const [profileBusy, setProfileBusy] = useState(false);
  const [profileNotice, setProfileNotice] = useState<string | null>(null);

  // Credential-change form state.
  const [currentPassword, setCurrentPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [passwordBusy, setPasswordBusy] = useState(false);
  const [pageError, setPageError] = useState<string | null>(null);
  const [pageNotice, setPageNotice] = useState<string | null>(null);

  const mismatch =
    confirmPassword.length > 0 && newPassword !== confirmPassword
      ? "The confirmation does not match."
      : null;
  const passwordDisabled =
    passwordBusy ||
    currentPassword.length === 0 ||
    newPassword.length === 0 ||
    newPassword !== confirmPassword;

  const handleProfileSubmit = useCallback(
    async (event: FormEvent<HTMLFormElement>): Promise<void> => {
      event.preventDefault();
      if (profileBusy || overview === null) return;
      const data = new FormData(event.currentTarget);
      const displayName = String(data.get("displayName") ?? "").trim();
      const email = String(data.get("email") ?? "").trim();
      if (displayName.length === 0) return;
      setProfileBusy(true);
      setPageError(null);
      setProfileNotice(null);
      try {
        await updateProfile({ displayName, email });
        setProfileNotice("Profile saved.");
        resource.reload();
      } catch (reason: unknown) {
        setPageError(errorOf(reason));
      } finally {
        setProfileBusy(false);
      }
    },
    [overview, profileBusy, resource],
  );

  const submitPasswordChange = useCallback(async (): Promise<void> => {
    if (passwordDisabled) return;
    setPasswordBusy(true);
    setPageError(null);
    setPageNotice(null);
    try {
      await changePassword({ currentPassword, newPassword });
      setCurrentPassword("");
      setNewPassword("");
      setConfirmPassword("");
      setPageNotice("The password was changed; your other sessions were signed out.");
      resource.reload();
    } catch (reason: unknown) {
      setPageError(errorOf(reason));
    } finally {
      setPasswordBusy(false);
    }
  }, [currentPassword, newPassword, passwordDisabled, resource]);

  const handlePasswordSubmit = useCallback(
    (event: FormEvent<HTMLFormElement>): void => {
      event.preventDefault();
      void submitPasswordChange();
    },
    [submitPasswordChange],
  );

  if (resource.loading) {
    return (
      <>
        <PageHeader title="Account" subtitle="Your profile, credential, identities, and sessions." />
        <StateView title="Loading account" />
      </>
    );
  }

  return (
    <>
      <PageHeader title="Account" subtitle="Your profile, credential, identities, and sessions." />

      {resource.error !== null ? (
        <StateView
          title="Could not load account"
          detail={resource.error.message}
          action={<Button onClick={resource.reload}>Retry</Button>}
        />
      ) : overview === null ? null : (
        <>
          {pageNotice !== null ? <p className="kds-muted">{pageNotice}</p> : null}
          {pageError !== null ? (
            <p role="alert" className="kds-muted">
              The change was refused: {pageError} Nothing changed; you can retry.
            </p>
          ) : null}

          <div className="kds-stack">
            <Card title="Profile">
              <table className="kds-table">
                <tbody>
                  <tr>
                    <th scope="row">Username</th>
                    <td className="kds-mono">{overview.profile.username}</td>
                  </tr>
                  <tr>
                    <th scope="row">Display name</th>
                    <td>{overview.profile.displayName}</td>
                  </tr>
                </tbody>
              </table>

              <form
                className="kds-stack"
                onSubmit={(event) => void handleProfileSubmit(event)}
              >
                <div className="kds-row">
                  <input
                    className="kds-input"
                    name="displayName"
                    aria-label="Display name"
                    placeholder={overview.profile.displayName}
                    defaultValue={overview.profile.displayName}
                    disabled={profileBusy}
                  />
                  <input
                    className="kds-input"
                    name="email"
                    aria-label="Email"
                    placeholder="Email"
                    defaultValue={overview.profile.email}
                    disabled={profileBusy}
                  />
                  <Button type="submit" disabled={profileBusy}>
                    {profileBusy ? "Saving…" : "Save profile"}
                  </Button>
                </div>
                {profileNotice !== null ? <p className="kds-muted">{profileNotice}</p> : null}
              </form>
            </Card>

            <Card title="Change password" bodyClass="kds-pad">
              {overview.hasNativeCredential ? (
                <form className="kds-stack" onSubmit={handlePasswordSubmit}>
                  <p className="kds-muted">
                    Changing your password signs your other sessions out immediately.
                  </p>
                  <Field label="Current password">
                    <input
                      className="kds-input"
                      type="password"
                      value={currentPassword}
                      disabled={passwordBusy}
                      autoComplete="current-password"
                      onChange={(event) => setCurrentPassword(event.target.value)}
                    />
                  </Field>
                  <Field label="New password">
                    <input
                      className="kds-input"
                      type="password"
                      value={newPassword}
                      disabled={passwordBusy}
                      autoComplete="new-password"
                      onChange={(event) => setNewPassword(event.target.value)}
                    />
                  </Field>
                  <Field label="Confirm new password" hint={mismatch ?? undefined}>
                    <input
                      className="kds-input"
                      type="password"
                      value={confirmPassword}
                      disabled={passwordBusy}
                      autoComplete="new-password"
                      onChange={(event) => setConfirmPassword(event.target.value)}
                    />
                  </Field>
                  <div className="kds-row">
                    <Button type="submit" variant="primary" disabled={passwordDisabled}>
                      {passwordBusy ? "Saving…" : "Change password"}
                    </Button>
                  </div>
                </form>
              ) : (
                <p className="kds-muted">
                  This account signs in through an external identity and carries no native
                  credential to change.
                </p>
              )}
            </Card>

            <Card
              title={`Linked identities (${overview.identities.length})`}
              actions={
                overview.linkableProviders.length > 0 ? (
                  <span className="kds-row">
                    {overview.linkableProviders.map((provider) => (
                      <Badge key={provider} tone="info">
                        {provider}
                      </Badge>
                    ))}
                  </span>
                ) : undefined
              }
            >
              {overview.identities.length === 0 ? (
                <StateView
                  title="No linked identities"
                  detail={
                    overview.linkableProviders.length > 0
                      ? `Linkable providers: ${overview.linkableProviders.join(", ")}.`
                      : "Sign-in currently uses your native credential."
                  }
                />
              ) : (
                <table className="kds-table">
                  <thead>
                    <tr>
                      <th scope="col">Provider</th>
                    </tr>
                  </thead>
                  <tbody>
                    {overview.identities.map((identity) => (
                      <tr key={identity.provider}>
                        <td>{identity.provider}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </Card>

            <Card
              title={`Active sessions (${overview.sessions.length})`}
              actions={
                <div className="kds-row">
                  <Button
                    size="sm"
                    onClick={() =>
                      void revokeOtherSessions()
                        .then(() => {
                          setPageNotice("Your other sessions were revoked.");
                          resource.reload();
                        })
                        .catch((reason: unknown) => setPageError(errorOf(reason)))
                    }
                  >
                    Revoke other sessions
                  </Button>
                  <Button size="sm" variant="danger" onClick={() => void logout()}>
                    Log out
                  </Button>
                </div>
              }
            >
              {overview.sessions.length === 0 ? (
                <StateView title="No active sessions" />
              ) : (
                <table className="kds-table">
                  <thead>
                    <tr>
                      <th scope="col">Session</th>
                      <th scope="col">Started</th>
                      <th scope="col"></th>
                    </tr>
                  </thead>
                  <tbody>
                    {overview.sessions.map((session) => (
                      <tr key={session.sessionId}>
                        <td>
                          Session {session.sessionId.slice(0, 8)}{" "}
                          {session.current ? <Badge tone="ok">this device</Badge> : null}
                        </td>
                        <td className="kds-muted kds-datetime">{session.createdAt}</td>
                        <td>
                          {!session.current ? (
                            <Button
                              size="sm"
                              onClick={() =>
                                void revokeSession(session.sessionId)
                                  .then(resource.reload)
                                  .catch((reason: unknown) => setPageError(errorOf(reason)))
                              }
                            >
                              Revoke
                            </Button>
                          ) : null}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </Card>
          </div>
        </>
      )}
    </>
  );
}

