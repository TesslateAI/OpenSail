/**
 * Portal account settings — the functional slice over the documented
 * `/api/account` contract. One authoritative overview loads on mount and
 * re-reads after every mutation, so nothing on screen claims more than the
 * server confirmed: profile edits PATCH both editable fields, a password
 * change requires the current password and reports its other-session
 * revocation, sessions list only truthful columns (short id, current badge,
 * created stamp), and external identities render read-only. Link
 * affordances stay hidden whenever `linkableProviders` comes back empty.
 */

import { useCallback, useState, type FormEvent } from "react";
import {
  changePassword,
  fetchAccount,
  revokeOtherSessions,
  revokeSession,
  updateProfile,
  type AccountOverview,
  type PasswordChangeInput,
  type ProfileUpdateInput,
} from "../api/account.ts";
import { useResource } from "../hooks.ts";
import { Badge } from "../design-system/components/Badge";
import { Button } from "../design-system/components/Button";
import { Card, PageHeader } from "../design-system/components/Card";
import { Field } from "../design-system/components/Field";
import { StateView } from "../design-system/components/StateView";

function errorOf(reason: unknown): string {
  return reason instanceof Error ? reason.message : "request failed";
}

/** Module-level loader keeps a stable `useResource` effect dependency. */
async function loadOverview(signal: AbortSignal): Promise<AccountOverview> {
  return fetchAccount(signal);
}

export function UserSettingsPanel() {
  const resource = useResource(loadOverview);

  // Shared mutation feedback: one in-flight flag, one dismiss-on-next-action
  // notice, and one refusal banner for the last failed mutation.
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  // Credential-change form state; current password is mandatory per contract.
  const [currentPassword, setCurrentPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");

  const mismatch =
    confirmPassword.length > 0 && newPassword !== confirmPassword
      ? "The confirmation does not match."
      : null;
  const passwordDisabled =
    busy ||
    currentPassword.length === 0 ||
    newPassword.length === 0 ||
    newPassword !== confirmPassword;

  const saveProfile = useCallback(
    async (input: ProfileUpdateInput): Promise<void> => {
      setBusy(true);
      setNotice(null);
      setActionError(null);
      try {
        await updateProfile(input);
        setNotice("Profile saved.");
        resource.reload();
      } catch (reason: unknown) {
        setActionError(errorOf(reason));
      } finally {
        setBusy(false);
      }
    },
    [resource],
  );

  const handleProfileSubmit = useCallback(
    (event: FormEvent<HTMLFormElement>): void => {
      event.preventDefault();
      const data = new FormData(event.currentTarget);
      const name = String(data.get("displayName") ?? "").trim();
      const mail = String(data.get("email") ?? "").trim();
      if (name.length > 0 && mail.length > 0) void saveProfile({ displayName: name, email: mail });
    },
    [saveProfile],
  );

  const changeCredential = useCallback(
    async (input: PasswordChangeInput): Promise<void> => {
      setBusy(true);
      setNotice(null);
      setActionError(null);
      try {
        await changePassword(input);
        setCurrentPassword("");
        setNewPassword("");
        setConfirmPassword("");
        setNotice("The password was changed. Your other sessions were signed out.");
        resource.reload();
      } catch (reason: unknown) {
        setActionError(errorOf(reason));
      } finally {
        setBusy(false);
      }
    },
    [resource],
  );

  const handlePasswordSubmit = useCallback((): void => {
    if (passwordDisabled) return;
    void changeCredential({ currentPassword, newPassword });
  }, [changeCredential, currentPassword, newPassword, passwordDisabled]);

  const revokeOne = useCallback(
    async (sessionId: string): Promise<void> => {
      setBusy(true);
      setNotice(null);
      setActionError(null);
      try {
        await revokeSession(sessionId);
        setNotice("The session was revoked.");
        resource.reload();
      } catch (reason: unknown) {
        setActionError(errorOf(reason));
      } finally {
        setBusy(false);
      }
    },
    [resource],
  );

  const revokeOthers = useCallback(async (): Promise<void> => {
    setBusy(true);
    setNotice(null);
    setActionError(null);
    try {
      await revokeOtherSessions();
      setNotice("Every other session was revoked.");
      resource.reload();
    } catch (reason: unknown) {
      setActionError(errorOf(reason));
    } finally {
      setBusy(false);
    }
  }, [resource]);

  if (resource.loading) {
    return (
      <>
        <PageHeader title="Account settings" subtitle="Profile, password, identities, and sessions." />
        <StateView title="Loading account" />
      </>
    );
  }

  if (resource.error !== null || resource.data === null) {
    return (
      <>
        <PageHeader title="Account settings" subtitle="Profile, password, identities, and sessions." />
        <StateView
          title="Account unavailable"
          detail={
            resource.error?.message ?? "The server answered without account data."
          }
          action={<Button onClick={resource.reload}>Retry</Button>}
        />
      </>
    );
  }

  const overview = resource.data;
  const profile = overview.profile;

  return (
    <div className="account-panel">
      <PageHeader
        title="Account settings"
        subtitle="Profile, password, identities, and sessions."
      />

      {notice !== null ? <p className="kds-muted">{notice}</p> : null}
      {actionError !== null ? (
        <p role="alert" className="kds-muted">
          The change was refused: {actionError} Nothing changed; you can retry.
        </p>
      ) : null}

      <div className="kds-stack">
        <Card title="Profile">
          <form className="kds-stack" onSubmit={handleProfileSubmit}>
            <Field label="Username">
              <p className="kds-mono">{profile.username.length > 0 ? profile.username : "—"}</p>
            </Field>
            <Field label="Display name">
              <input
                className="kds-input"
                name="displayName"
                autoComplete="off"
                disabled={busy}
                placeholder={profile.displayName.length > 0 ? profile.displayName : "Display name"}
                defaultValue={profile.displayName}
              />
            </Field>
            <Field label="Email">
              <input
                className="kds-input"
                name="email"
                type="email"
                autoComplete="off"
                disabled={busy}
                placeholder={profile.email.length > 0 ? profile.email : "you@example.com"}
                defaultValue={profile.email}
              />
            </Field>
            <div className="kds-row">
              <Button type="submit" variant="primary" disabled={busy}>
                {busy ? "Saving…" : "Save profile"}
              </Button>
            </div>
          </form>
        </Card>

        <Card title="Change password">
          {overview.hasNativeCredential ? (
            <form
              className="kds-stack"
              onSubmit={(event) => {
                event.preventDefault();
                handlePasswordSubmit();
              }}
            >
              <p className="kds-muted">
                Changing your password signs your other sessions out of native sign-in. This
                device stays signed in.
              </p>
              <Field label="Current password">
                <input
                  className="kds-input"
                  type="password"
                  value={currentPassword}
                  disabled={busy}
                  required
                  autoComplete="current-password"
                  onChange={(event) => setCurrentPassword(event.target.value)}
                />
              </Field>
              <Field label="New password">
                <input
                  className="kds-input"
                  type="password"
                  value={newPassword}
                  disabled={busy}
                  required
                  autoComplete="new-password"
                  onChange={(event) => setNewPassword(event.target.value)}
                />
              </Field>
              <Field label="Confirm new password" hint={mismatch ?? undefined}>
                <input
                  className="kds-input"
                  type="password"
                  value={confirmPassword}
                  disabled={busy}
                  autoComplete="new-password"
                  onChange={(event) => setConfirmPassword(event.target.value)}
                />
              </Field>
              <div className="kds-row">
                <Button type="submit" variant="primary" disabled={passwordDisabled}>
                  {busy ? "Saving…" : "Change password"}
                </Button>
              </div>
            </form>
          ) : (
            <StateView
              title="Native sign-in inactive"
              detail="This account has no native password credential, so there is nothing to change here."
            />
          )}
        </Card>

        <Card title="Active sessions" actions={
          <Button size="sm" disabled={busy} onClick={() => void revokeOthers()}>
            Revoke other sessions
          </Button>
        }>
          {overview.sessions.length === 0 ? (
            <StateView title="No active sessions reported" />
          ) : (
            <table className="kds-table">
              <thead>
                <tr>
                  <th scope="col">Session</th>
                  <th scope="col">Created</th>
                  <th scope="col"></th>
                </tr>
              </thead>
              <tbody>
                {overview.sessions.map((session) => (
                  <tr key={session.sessionId}>
                    <td>
                      <span className="kds-mono">{session.sessionId.slice(0, 8)}</span>{" "}
                      {session.current ? <Badge tone="ok">this device</Badge> : null}
                    </td>
                    <td className="kds-muted kds-datetime">{session.createdAt}</td>
                    <td>
                      {session.current ? null : (
                        <span className="kds-table-actions">
                        <Button
                          size="sm"
                          variant="danger"
                          disabled={busy}
                          onClick={() => void revokeOne(session.sessionId)}
                        >
                          Revoke
                        </Button>
                        </span>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </Card>

        <Card title={`External identities (${overview.identities.length})`}>
          {overview.identities.length === 0 ? (
            <StateView
              title="No linked identities"
              detail={
                overview.hasNativeCredential
                  ? "Sign-in uses your native password credential."
                  : undefined
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
          {overview.linkableProviders.length === 0 ? null : (
            <p className="kds-muted">
              Providers this server offers for linking:{" "}
              {overview.linkableProviders.join(", ")}
            </p>
          )}
        </Card>
      </div>
    </div>
  );
}
