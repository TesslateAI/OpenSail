/**
 * Users: platform-admin management of canonical Users.
 *
 * Rows come from GET /api/admin/users (platform admin only; the server
 * answers 403 for everyone else). Mutations use PATCH
 * /api/admin/users/:id/role and /api/admin/users/:id/status — the verified
 * surfaces. Create-user and password-reset call the adapter's assumed
 * routes; until the server lands them, refusals surface verbatim and the
 * user table keeps working. The control plane owns authorization and any
 * protection rules; refusals are shown as-is.
 */

import { useCallback, useState, type ChangeEvent, type FormEvent } from "react";
import {
  adminApi,
  PLATFORM_ROLES,
  parsePlatformRole,
  USER_STATUSES,
  type AdminApi,
  type AdminUserDto,
  type PlatformRole,
  type UserStatus,
} from "../api/admin.ts";
import type { Uuid } from "../api/dto.ts";
import { ApiError, newIntentId } from "../api/http.ts";
import { useResource } from "../hooks.ts";
import { Badge, Card, PageHeader, StateView } from "../ui/primitives.tsx";

function shortId(id: string): string {
  return id.length === 0 ? "—" : id.length <= 10 ? id : `${id.slice(0, 8)}…`;
}

function errorOf(reason: unknown): string {
  return reason instanceof Error ? reason.message : "request failed";
}

function displayNameOf(user: AdminUserDto): string {
  const name = user.displayName.trim();
  if (name.length > 0) return name;
  return user.username !== null && user.username.trim().length > 0 ? user.username : "—";
}

type AdminUsersProps = { api?: AdminApi | undefined };

export function AdminUsers({ api = adminApi }: AdminUsersProps) {
  const load = useCallback((signal: AbortSignal) => api.listUsers(signal), [api]);
  const resource = useResource(load);
  const users = resource.data ?? [];

  // One in-flight mutation at a time, keyed by target user.
  const [busyUserId, setBusyUserId] = useState<Uuid | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  // Create-user form.
  const [creating, setCreating] = useState(false);
  const [createUsername, setCreateUsername] = useState("");
  const [createPassword, setCreatePassword] = useState("");
  const [createPlatformRole, setCreatePlatformRole] = useState<PlatformRole>("user");
  const [creatingBusy, setCreatingBusy] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);

  // Password-reset form for one user.
  const [resetUserId, setResetUserId] = useState<Uuid | null>(null);
  const [resetPassword, setResetPassword] = useState("");
  const [resettingBusy, setResettingBusy] = useState(false);
  const [resetError, setResetError] = useState<string | null>(null);

  const setPlatformRole = useCallback(
    async (userId: Uuid, platformRole: PlatformRole): Promise<void> => {
      if (busyUserId !== null) return;
      setBusyUserId(userId);
      setActionError(null);
      try {
        await api.setPlatformRole(userId, platformRole);
        resource.reload();
      } catch (reason: unknown) {
        setActionError(errorOf(reason));
        resource.reload();
      } finally {
        setBusyUserId(null);
      }
    },
    [api, busyUserId, resource],
  );

  const setUserStatus = useCallback(
    async (userId: Uuid, status: UserStatus): Promise<void> => {
      if (busyUserId !== null) return;
      setBusyUserId(userId);
      setActionError(null);
      try {
        await api.setUserStatus(userId, status);
        resource.reload();
      } catch (reason: unknown) {
        setActionError(errorOf(reason));
        resource.reload();
      } finally {
        setBusyUserId(null);
      }
    },
    [api, busyUserId, resource],
  );

  const submitCreate = useCallback(async (): Promise<void> => {
    if (creatingBusy || resource.data === null) return;
    const username = createUsername.trim();
    if (username.length === 0 || createPassword.length === 0) return;
    setCreatingBusy(true);
    setCreateError(null);
    try {
      await api.createUser({
        id: newIntentId(),
        username,
        password: createPassword,
        platformRole: createPlatformRole,
      });
      setCreateUsername("");
      setCreatePassword("");
      setCreating(false);
      resource.reload();
    } catch (reason: unknown) {
      setCreateError(errorOf(reason));
    } finally {
      setCreatingBusy(false);
    }
  }, [api, createPassword, createPlatformRole, createUsername, creatingBusy, resource]);
  const submitReset = useCallback(async (): Promise<void> => {
    if (resettingBusy || resetUserId === null || resetPassword.length === 0) return;
    setResettingBusy(true);
    setResetError(null);
    try {
      await api.resetPassword(resetUserId, resetPassword);
      setResetUserId(null);
      setResetPassword("");
      resource.reload();
    } catch (reason: unknown) {
      setResetError(errorOf(reason));
    } finally {
      setResettingBusy(false);
    }
  }, [api, resettingBusy, resetPassword, resetUserId, resource]);
  const startReset = useCallback((userId: Uuid): void => {
    setResetPassword("");
    setResetError(null);
    setResetUserId(userId);
  }, []);

  const header = (
    <PageHeader title="Users" subtitle="Platform users; only the admin platform role can manage them." />
  );

  if (resource.loading) {
    return (
      <>
        {header}
        <StateView state="loading" title="Loading users" />
      </>
    );
  }
  if (resource.error !== null) {
    const forbidden = resource.error instanceof ApiError && resource.error.status === 403;
    return (
      <>
        {header}
        <StateView
          state="error"
          title={forbidden ? "Platform admin access required" : "Could not load users"}
          detail={resource.error.message}
          onRetry={resource.reload}
        />
      </>
    );
  }

  const handleCreateSubmit = (event: FormEvent<HTMLFormElement>): void => {
    event.preventDefault();
    void submitCreate();
  };

  const handleResetSubmit = (event: FormEvent<HTMLFormElement>): void => {
    event.preventDefault();
    void submitReset();
  };

  return (
    <>
      {header}

      <Card
        title="User management"
        actions={
          <button
            type="button"
            className="btn"
            onClick={() => {
              setCreateError(null);
              setCreating((open) => !open);
            }}
          >
            {creating ? "Close form" : "Create user"}
          </button>
        }
      >
        {creating ? (
          <form className="stack stack-tight" onSubmit={handleCreateSubmit}>
            <p className="muted">
              Creates a canonical User with a native credential and their personal scope. The
              server enforces the username and password rules; refusals appear here verbatim.
            </p>
            <div className="row">
              <div className="field">
                <label htmlFor="admin-user-username">Username</label>
                <input
                  id="admin-user-username"
                  value={createUsername}
                  disabled={creatingBusy}
                  autoFocus
                  onChange={(event: ChangeEvent<HTMLInputElement>) =>
                    setCreateUsername(event.target.value)
                  }
                />
              </div>
              <div className="field">
                <label htmlFor="admin-user-password">Password</label>
                <input
                  id="admin-user-password"
                  type="password"
                  value={createPassword}
                  disabled={creatingBusy}
                  autoComplete="new-password"
                  onChange={(event: ChangeEvent<HTMLInputElement>) =>
                    setCreatePassword(event.target.value)
                  }
                />
              </div>
              <div className="field">
                <label htmlFor="admin-user-platform-role">Platform role</label>
                <select
                  id="admin-user-platform-role"
                  value={createPlatformRole}
                  disabled={creatingBusy}
                  onChange={(event: ChangeEvent<HTMLSelectElement>) =>
                    setCreatePlatformRole(parsePlatformRole(event.target.value))
                  }
                >
                  {PLATFORM_ROLES.map((role) => (
                    <option key={role} value={role}>
                      {role}
                    </option>
                  ))}
                </select>
              </div>
              <button
                type="submit"
                className={
                  creatingBusy || createUsername.trim().length === 0 || createPassword.length === 0
                    ? "btn btn-primary btn-disabled"
                    : "btn btn-primary"
                }
                disabled={
                  creatingBusy || createUsername.trim().length === 0 || createPassword.length === 0
                }
              >
                {creatingBusy ? "Creating…" : "Create user"}
              </button>
            </div>
            {createError !== null ? (
              <p role="alert" className="muted">
                Creating the user failed: {createError}. Nothing was created; you can retry.
              </p>
            ) : null}
          </form>
        ) : null}

        {users.length === 0 ? (
          <StateView
            state="empty"
            title="No users listed"
            detail="Canonical users appear here."
          />
        ) : (
          <table className="table">
            <thead>
              <tr>
                <th scope="col">User</th>
                <th scope="col">Username</th>
                <th scope="col">Status</th>
                <th scope="col">Platform role</th>
                <th scope="col">Created</th>
                <th scope="col">Actions</th>
              </tr>
            </thead>
            <tbody>
              {users.map((user) => (
                <tr key={user.id}>
                  <td>
                    <span className="mono" title={user.id}>
                      {shortId(user.id)}
                    </span>{" "}
                    <span>{displayNameOf(user)}</span>
                  </td>
                  <td className="mono">
                    {user.username === null || user.username.trim() === "" ? "—" : user.username}
                  </td>
                  <td>
                    <Badge tone={user.status === "active" ? "ok" : "fail"}>{user.status}</Badge>
                  </td>
                  <td>
                    {busyUserId === user.id ? (
                      <Badge tone="warn">working…</Badge>
                    ) : (
                      <Badge tone={user.platformRole === "admin" ? "accent" : "neutral"}>
                        {user.platformRole}
                      </Badge>
                    )}
                  </td>
                  <td>
                    {user.createdAt === null || user.createdAt.trim() === ""
                      ? "—"
                      : user.createdAt}
                  </td>
                  <td>
                    <span className="row">
                      <select
                        aria-label={`Platform role for ${user.id}`}
                        value={user.platformRole}
                        disabled={busyUserId !== null}
                        onChange={(event) => {
                          void setPlatformRole(user.id, parsePlatformRole(event.target.value));
                        }}
                      >
                        {PLATFORM_ROLES.map((role) => (
                          <option key={role} value={role}>
                            {role}
                          </option>
                        ))}
                      </select>
                      <select
                        aria-label={`Status for ${user.id}`}
                        value={user.status}
                        disabled={busyUserId !== null}
                        onChange={(event) => {
                          void setUserStatus(
                            user.id,
                            event.target.value === "active" ? "active" : "disabled",
                          );
                        }}
                      >
                        {USER_STATUSES.map((status) => (
                          <option key={status} value={status}>
                            {status}
                          </option>
                        ))}
                      </select>
                      <button
                        type="button"
                        className="btn"
                        disabled={busyUserId !== null}
                        onClick={() => startReset(user.id)}
                      >
                        Reset password
                      </button>
                    </span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}

        {actionError !== null ? (
          <p role="alert" className="muted">
            The change was refused: {actionError} Nothing changed; you can retry.
          </p>
        ) : null}

        {resetUserId !== null ? (
          <form className="stack stack-tight" onSubmit={handleResetSubmit}>
            <p className="muted">
              Replaces the native credential of{" "}
              <span className="mono">{shortId(resetUserId)}</span>. The password is sent once,
              never echoed, and never stored in the browser.
            </p>
            <div className="row">
              <input
                aria-label="New password"
                type="password"
                value={resetPassword}
                disabled={resettingBusy}
                autoComplete="new-password"
                autoFocus
                onChange={(event: ChangeEvent<HTMLInputElement>) =>
                  setResetPassword(event.target.value)
                }
              />
              <button
                type="submit"
                className={
                  resettingBusy || resetPassword.length === 0
                    ? "btn btn-primary btn-disabled"
                    : "btn btn-primary"
                }
                disabled={resettingBusy || resetPassword.length === 0}
              >
                {resettingBusy ? "Resetting…" : "Set password"}
              </button>
              <button
                type="button"
                className="btn"
                disabled={resettingBusy}
                onClick={() => setResetUserId(null)}
              >
                Cancel
              </button>
            </div>
            {resetError !== null ? (
              <p role="alert" className="muted">
                Resetting the password failed: {resetError} The current credential stays valid.
              </p>
            ) : null}
          </form>
        ) : null}
      </Card>
    </>
  );
}
