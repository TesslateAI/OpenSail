/**
 * Admin console — platform user management.
 *
 * The table lists every canonical User with status, platform role, resolved
 * team memberships, and per-row actions: create (modal), disable/enable,
 * one-time password reset, platform role change, and web-session revocation.
 * Team membership is computed best-effort from the caller-visible scope
 * roster; a scope that fails to resolve simply renders no names.
 */

import { useCallback, useMemo, useState } from "react";
import {
  createConsoleUser,
  generatePassword,
  listConsoleUserSessions,
  listConsoleUsers,
  resetConsolePassword,
  revokeConsoleSessions,
  setConsoleUserRole,
  setConsoleUserStatus,
  type ConsoleUserDto,
} from "../api/console.ts";
import { PLATFORM_ROLES, parsePlatformRole } from "../api/admin.ts";
import type { PlatformRole, UserStatus } from "../api/admin.ts";
import { ApiError } from "../api/http.ts";
import { getScope, listScopes } from "../api/scopes.ts";
import { useResource } from "../hooks.ts";
import { Badge } from "../design-system/components/Badge";
import { Button } from "../design-system/components/Button";
import { Card, PageHeader } from "../design-system/components/Card";
import { Field } from "../design-system/components/Field";
import { Modal } from "../design-system/components/Modal";
import { StateView } from "../design-system/components/StateView";

/** Compact id for cramped cells; identical to the console's shortId rule. */
function shortId(id: string): string {
  return id.length === 0 ? "—" : id.length <= 10 ? id : `${id.slice(0, 8)}…`;
}

function errorOf(reason: unknown): string {
  return reason instanceof Error ? reason.message : "request failed";
}

function labelOf(user: ConsoleUserDto): string {
  const name = user.displayName.trim();
  if (name.length > 0) return name;
  const username = user.username?.trim() ?? "";
  return username !== "" ? username : "—";
}

function usernameOf(user: ConsoleUserDto): string {
  const username = user.username?.trim() ?? "";
  return username !== "" ? username : "—";
}

type TeamsMap = ReadonlyMap<string, readonly string[]>;

/** Resolves team-scope membership names for every listed user, best-effort. */
async function loadTeamsMap(signal: AbortSignal): Promise<TeamsMap> {
  const scopes = await listScopes(signal);
  const teams = scopes.filter((scope) => scope.kind === "team");
  const maps = await Promise.all(
    teams.map(async (scope) => {
      try {
        const detail = await getScope(scope.id, signal);
        return detail.members.map((member) => [member.userId, scope.name] as const);
      } catch {
        // An unresolvable scope contributes no rows; the table stays usable.
        return [] as ReadonlyArray<readonly [string, string]>;
      }
    }),
  );
  const map = new Map<string, string[]>();
  for (const pairs of maps.flat()) {
    const known = map.get(pairs[0]);
    map.set(pairs[0], known === undefined ? [pairs[1]] : [...known, pairs[1]]);
  }
  return map;
}

export function AdminUsersPage() {
  const users = useResource(listConsoleUsers);
  const teams = useResource(loadTeamsMap);

  const [filter, setFilter] = useState("");
  const [busyUserId, setBusyUserId] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  // Create-user modal state.
  const [creating, setCreating] = useState(false);
  const [createUsername, setCreateUsername] = useState("");
  const [createDisplayName, setCreateDisplayName] = useState("");
  const [createEmail, setCreateEmail] = useState("");
  const [createRole, setCreateRole] = useState<PlatformRole>("user");
  const [createPassword, setCreatePassword] = useState(() => generatePassword());
  const [creatingBusy, setCreatingBusy] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);

  // Reset-password modal state: open with a fresh secret; after success the
  // same modal shows it exactly once before closing.
  const [resetTarget, setResetTarget] = useState<ConsoleUserDto | null>(null);
  const [resetPassword, setResetPassword] = useState(() => generatePassword());
  const [resettingBusy, setResettingBusy] = useState(false);
  const [resetError, setResetError] = useState<string | null>(null);
  const [revealedPassword, setRevealedPassword] = useState<string | null>(null);

  // Session-inspection modal state.
  const [sessionRows, setSessionRows] = useState<
    ReadonlyArray<{ id: string; createdAt: string | null }> | null
  >(null);
  const [sessionsLabel, setSessionsLabel] = useState("");

  const visible = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    if (needle.length === 0) return users.data ?? [];
    return (users.data ?? []).filter((user) =>
      `${user.username ?? ""} ${user.displayName} ${user.email ?? ""}`
        .toLowerCase()
        .includes(needle),
    );
  }, [users.data, filter]);

  const runOne = useCallback(
    async (userId: string, action: () => Promise<void>, done?: () => void): Promise<void> => {
      if (busyUserId !== null) return;
      setBusyUserId(userId);
      setActionError(null);
      setNotice(null);
      try {
        await action();
        done?.();
      } catch (reason: unknown) {
        setActionError(errorOf(reason));
      } finally {
        setBusyUserId(null);
      }
    },
    [busyUserId],
  );

  const toggleStatus = useCallback(
    (user: ConsoleUserDto): void => {
      const next: UserStatus = user.status === "active" ? "disabled" : "active";
      void runOne(user.id, async () => {
        await setConsoleUserStatus(user.id, next);
        users.reload();
      }, () => setNotice(`${labelOf(user)} is now ${next}.`));
    },
    [runOne, users],
  );

  const changeRole = useCallback(
    (user: ConsoleUserDto, role: PlatformRole): void => {
      void runOne(user.id, async () => {
        await setConsoleUserRole(user.id, role);
        users.reload();
      }, () => setNotice(`${labelOf(user)} now carries the ${role} platform role.`));
    },
    [runOne, users],
  );

  const revokeSessions = useCallback(
    (user: ConsoleUserDto): void => {
      void runOne(user.id, async () => {
        const result = await revokeConsoleSessions(user.id);
        users.reload();
        setNotice(
          result.revoked === 1
            ? `One web session of ${labelOf(user)} was revoked.`
            : `${result.revoked} web sessions of ${labelOf(user)} were revoked.`,
        );
      });
    },
    [runOne, users],
  );

  const submitCreate = useCallback(async (): Promise<void> => {
    if (creatingBusy) return;
    const username = createUsername.trim();
    const displayName = createDisplayName.trim();
    if (username.length === 0 || displayName.length === 0 || createPassword.length === 0) return;
    setCreatingBusy(true);
    setCreateError(null);
    try {
      const email = createEmail.trim();
      await createConsoleUser({
        username,
        displayName,
        email: email === "" ? undefined : email,
        platformRole: createRole,
        password: createPassword,
      });
      setCreating(false);
      setCreateUsername("");
      setCreateDisplayName("");
      setCreateEmail("");
      setCreateRole("user");
      setCreatePassword(generatePassword());
      users.reload();
      setNotice(`User ${username} was created.`);
    } catch (reason: unknown) {
      setCreateError(errorOf(reason));
    } finally {
      setCreatingBusy(false);
    }
  }, [createDisplayName, createEmail, createPassword, createRole, createUsername, creatingBusy, users]);

  const openReset = useCallback((user: ConsoleUserDto): void => {
    setResetTarget(user);
    setResetPassword(generatePassword());
    setResetError(null);
    setRevealedPassword(null);
  }, []);

  const submitReset = useCallback(async (): Promise<void> => {
    if (resettingBusy || resetTarget === null) return;
    setResettingBusy(true);
    setResetError(null);
    try {
      await resetConsolePassword(resetTarget.id, resetPassword);
      // Shown exactly once; closing the modal discards it forever.
      setRevealedPassword(resetPassword);
    } catch (reason: unknown) {
      setResetError(errorOf(reason));
    } finally {
      setResettingBusy(false);
    }
  }, [resetPassword, resetTarget, resettingBusy]);

  const inspectSessions = useCallback(
    (user: ConsoleUserDto): void => {
      void runOne(user.id, async () => {
        const sessions = await listConsoleUserSessions(user.id);
        setSessionsLabel(labelOf(user));
        setSessionRows(sessions.map((session) => ({ id: session.id, createdAt: session.createdAt })));
      });
    },
    [runOne],
  );

  if (users.loading) {
    return (
      <>
        <PageHeader title="Users" subtitle="Every canonical user on the platform." />
        <StateView title="Loading users" detail="Fetching the directory." />
      </>
    );
  }

  const forbidden = users.error instanceof ApiError && users.error.status === 403;

  return (
    <>
      <PageHeader
        title="Users"
        subtitle="Platform identity: creation, credentials, roles, and live sessions."
        actions={
          <Button
            variant="primary"
            onClick={() => {
              setCreateError(null);
              setCreating(true);
            }}
          >
            Create user
          </Button>
        }
      />

      {notice !== null ? <p className="kds-muted">{notice}</p> : null}
      {actionError !== null ? (
        <p role="alert" className="kds-muted">
          The change was refused: {actionError} Nothing changed; you can retry.
        </p>
      ) : null}

      {forbidden ? (
        <StateView
          title="Platform admin access required"
          detail={users.error?.message}
          action={<Button onClick={users.reload}>Retry</Button>}
        />
      ) : (
        <Card>
          <div className="kds-row">
            <input
              className="kds-input"
              aria-label="Filter users"
              placeholder="Filter by username, name, or email"
              value={filter}
              onChange={(event) => setFilter(event.target.value)}
            />
          </div>

          {visible.length === 0 ? (
            <StateView
              title="No users match"
              detail="Adjust the filter, or create a user to get started."
            />
          ) : (
            <table className="kds-table">
              <thead>
                <tr>
                  <th scope="col">Username</th>
                  <th scope="col">Name</th>
                  <th scope="col">Status</th>
                  <th scope="col">Platform role</th>
                  <th scope="col">Teams</th>
                  <th scope="col">Created</th>
                  <th scope="col">Actions</th>
                </tr>
              </thead>
              <tbody>
                {visible.map((user) => {
                  const teamNames = teams.data?.get(user.id) ?? [];
                  const busy = busyUserId === user.id;
                  return (
                    <tr key={user.id}>
                      <td className="kds-mono" title={user.id}>
                        {usernameOf(user)}
                      </td>
                      <td>{labelOf(user)}</td>
                      <td>
                        <Badge tone={user.status === "active" ? "ok" : "fail"} dot>
                          {user.status}
                        </Badge>
                      </td>
                      <td>
                        <select
                          className="kds-select kds-select-sm"
                          aria-label={`Platform role for ${usernameOf(user)}`}
                          value={user.platformRole}
                          disabled={busyUserId !== null}
                          onChange={(event) => changeRole(user, parsePlatformRole(event.target.value))}
                        >
                          {PLATFORM_ROLES.map((role) => (
                            <option key={role} value={role}>
                              {role}
                            </option>
                          ))}
                        </select>
                      </td>
                      <td>
                        {teamNames.length === 0 ? (
                          <span className="kds-muted">—</span>
                        ) : (
                          <span className="kds-trunc" title={teamNames.join(", ")}>
                            {teamNames.join(", ")}
                          </span>
                        )}
                      </td>
                      <td className="kds-muted">{user.createdAt ?? "—"}</td>
                      <td>
                        <span className="kds-row">
                          <Button
                            size="sm"
                            disabled={busyUserId !== null}
                            onClick={() => toggleStatus(user)}
                          >
                            {busy ? "Working…" : user.status === "active" ? "Disable" : "Enable"}
                          </Button>
                          <Button
                            size="sm"
                            disabled={busyUserId !== null}
                            onClick={() => openReset(user)}
                          >
                            Reset password
                          </Button>
                          <Button
                            size="sm"
                            disabled={busyUserId !== null}
                            onClick={() => revokeSessions(user)}
                          >
                            Revoke sessions
                          </Button>
                          <Button size="sm" disabled={busy} onClick={() => inspectSessions(user)}>
                            Sessions
                          </Button>
                        </span>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          )}
        </Card>
      )}

      {creating ? (
        <Modal
          title="Create user"
          subtitle="Mints a canonical User with a native credential."
          closeIcon="×"
          onClose={() => setCreating(false)}
          footer={
            <>
              <Button disabled={creatingBusy} onClick={() => setCreating(false)}>
                Cancel
              </Button>
              <Button
                variant="primary"
                disabled={
                  creatingBusy ||
                  createUsername.trim().length === 0 ||
                  createDisplayName.trim().length === 0 ||
                  createPassword.length === 0
                }
                onClick={() => void submitCreate()}
              >
                {creatingBusy ? "Creating…" : "Create user"}
              </Button>
            </>
          }
        >
          <form
            className="kds-stack"
            onSubmit={(event) => {
              event.preventDefault();
              void submitCreate();
            }}
          >
            <Field label="Username">
              <input
                className="kds-input"
                value={createUsername}
                disabled={creatingBusy}
                autoFocus
                autoComplete="off"
                onChange={(event) => setCreateUsername(event.target.value)}
              />
            </Field>
            <Field label="Display name">
              <input
                className="kds-input"
                value={createDisplayName}
                disabled={creatingBusy}
                onChange={(event) => setCreateDisplayName(event.target.value)}
              />
            </Field>
            <Field label="Email" hint="Optional; recorded on the user row.">
              <input
                className="kds-input"
                type="email"
                value={createEmail}
                disabled={creatingBusy}
                onChange={(event) => setCreateEmail(event.target.value)}
              />
            </Field>
            <Field label="Platform role">
              <select
                className="kds-select"
                value={createRole}
                disabled={creatingBusy}
                onChange={(event) => setCreateRole(parsePlatformRole(event.target.value))}
              >
                {PLATFORM_ROLES.map((role) => (
                  <option key={role} value={role}>
                    {role}
                  </option>
                ))}
              </select>
            </Field>
            <Field label="Initial password" hint="Delivered out of band; never shown here again.">
              <div className="kds-row">
                <input
                  className="kds-input kds-mono"
                  value={createPassword}
                  disabled={creatingBusy}
                  autoComplete="new-password"
                  onChange={(event) => setCreatePassword(event.target.value)}
                />
                <Button disabled={creatingBusy} onClick={() => setCreatePassword(generatePassword())}>
                  Regenerate
                </Button>
              </div>
            </Field>
            {createError !== null ? (
              <p role="alert" className="kds-muted">
                Creating failed: {createError} Nothing was created; you can retry.
              </p>
            ) : null}
          </form>
        </Modal>
      ) : null}

      {resetTarget !== null ? (
        <Modal
          title={revealedPassword === null ? `Reset password — ${labelOf(resetTarget)}` : "Password reset"}
          subtitle={
            revealedPassword === null
              ? "Generates a new native credential; the previous password stops verifying immediately."
              : "Copy it now. It is shown once and never stored in the browser."
          }
          closeIcon="×"
          onClose={() => setResetTarget(null)}
          footer={
            revealedPassword === null ? (
              <>
                <Button disabled={resettingBusy} onClick={() => setResetTarget(null)}>
                  Cancel
                </Button>
                <Button variant="primary" disabled={resettingBusy} onClick={() => void submitReset()}>
                  {resettingBusy ? "Setting…" : "Set password"}
                </Button>
              </>
            ) : (
              <Button variant="primary" onClick={() => setResetTarget(null)}>
                Done
              </Button>
            )
          }
        >
          {revealedPassword === null ? (
            <Field label="New password">
              <div className="kds-row">
                <input
                  className="kds-input kds-mono"
                  value={resetPassword}
                  disabled={resettingBusy}
                  autoComplete="new-password"
                  onChange={(event) => setResetPassword(event.target.value)}
                />
                <Button disabled={resettingBusy} onClick={() => setResetPassword(generatePassword())}>
                  Regenerate
                </Button>
              </div>
            </Field>
          ) : (
            <div className="kds-stack">
              <p className="kds-mono" style={{ wordBreak: "break-all" }}>
                {revealedPassword}
              </p>
              <p className="kds-muted">The previous credential no longer verifies.</p>
            </div>
          )}
          {resetError !== null ? (
            <p role="alert" className="kds-muted">
              Resetting failed: {resetError} The current credential stays valid.
            </p>
          ) : null}
        </Modal>
      ) : null}

      {sessionRows !== null ? (
        <Modal
          title={`Web sessions — ${sessionsLabel}`}
          subtitle="Server-side sessions still inside the TTL."
          closeIcon="×"
          onClose={() => setSessionRows(null)}
          footer={
            <Button variant="primary" onClick={() => setSessionRows(null)}>
              Close
            </Button>
          }
        >
          {sessionRows.length === 0 ? (
            <StateView title="No active sessions" />
          ) : (
            <table className="kds-table">
              <thead>
                <tr>
                  <th scope="col">Session</th>
                  <th scope="col">Started</th>
                </tr>
              </thead>
              <tbody>
                {sessionRows.map((row) => (
                  <tr key={row.id}>
                    <td className="kds-mono">{shortId(row.id)}</td>
                    <td className="kds-muted">{row.createdAt ?? "—"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </Modal>
      ) : null}
    </>
  );
}
