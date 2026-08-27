/**
 * Scopes & Teams: project collaboration scopes and their membership.
 *
 * A scope is a Project row with a collaboration `kind` (personal | team).
 * The platform-wide table comes from GET /api/admin/scopes with
 * server-aggregated member and workspace counts. Selecting a scope loads its
 * membership through the verified project-members surface: GET
 * /api/projects/:id/members, POST /api/projects/:id/members
 * (add-or-rerole, one call), DELETE /api/projects/:id/members/:userId.
 * Reroles of the durable owner are refused server-side; refusals surface
 * verbatim. Manage affordances are gated on the caller's own server-emitted
 * capabilities from the caller-scoped projects listing; no UI computes
 * permissions.
 */

import { useCallback, useMemo, useState, type ChangeEvent, type FormEvent } from "react";
import {
  adminApi,
  PROJECT_KINDS,
  PROJECT_ROLES,
  parseProjectRole,
  type AdminApi,
  type AdminGlobalScopeDto,
  type AdminScopeMemberDto,
  type ProjectRole,
} from "../api/admin.ts";
import type { Uuid } from "../api/dto.ts";
import { useResource } from "../hooks.ts";
import { Badge, Card, PageHeader, StateView } from "../ui/primitives.tsx";

function shortId(id: string): string {
  return id.length === 0 ? "—" : id.length <= 10 ? id : `${id.slice(0, 8)}…`;
}

function errorOf(reason: unknown): string {
  return reason instanceof Error ? reason.message : "request failed";
}

/** Badge tone per project role; the only place this mapping lives. */
const ROLE_TONE: Record<ProjectRole, "accent" | "ok" | "warn" | "neutral"> = {
  owner: "accent",
  admin: "ok",
  member: "neutral",
  viewer: "neutral",
};

type AdminScopesTeamsProps = { api?: AdminApi | undefined };

export function AdminScopesTeams({ api = adminApi }: AdminScopesTeamsProps) {
  // Platform-wide scopes with server-aggregated member/workspace counts.
  const scopesLoad = useCallback((signal: AbortSignal) => api.getAdminScopes(signal), [api]);
  const scopes = useResource(scopesLoad);
  const [selectedId, setSelectedId] = useState<Uuid | null>(null);

  // The membership card keeps its capability gates: the caller's own role on
  // the selected scope comes from the caller-scoped projects listing.
  const callerScopesLoad = useCallback((signal: AbortSignal) => api.listScopes(signal), [api]);
  const callerScopes = useResource(callerScopesLoad);

  const membersLoad = useCallback(
    (signal: AbortSignal): Promise<AdminScopeMemberDto[]> => {
      if (selectedId === null) return Promise.resolve([]);
      return api.listScopeMembers(selectedId, signal);
    },
    [api, selectedId],
  );
  const members = useResource(membersLoad, [selectedId]);

  const [memberUserId, setMemberUserId] = useState("");
  const [memberRole, setMemberRole] = useState<ProjectRole>("member");
  const [addingMember, setAddingMember] = useState(false);
  const [busyMemberId, setBusyMemberId] = useState<Uuid | null>(null);
  const [memberError, setMemberError] = useState<string | null>(null);

  const selected = useMemo(() => {
    const row = (scopes.data ?? []).find((scope) => scope.id === selectedId);
    return row ?? null;
  }, [scopes.data, selectedId]);

  const selectScope = useCallback((id: Uuid): void => {
    setSelectedId(id);
    setMemberError(null);
    setBusyMemberId(null);
  }, []);

  const submitMember = useCallback(async (): Promise<void> => {
    if (selectedId === null || addingMember) return;
    const trimmed = memberUserId.trim();
    if (trimmed.length === 0) return;
    setAddingMember(true);
    setMemberError(null);
    try {
      await api.addScopeMember(selectedId, trimmed, memberRole);
      setMemberUserId("");
      members.reload();
    } catch (reason: unknown) {
      setMemberError(errorOf(reason));
    } finally {
      setAddingMember(false);
    }
  }, [api, addingMember, memberRole, memberUserId, members, selectedId]);

  const rerole = useCallback(
    async (userId: Uuid, role: ProjectRole): Promise<void> => {
      if (selectedId === null || busyMemberId !== null) return;
      setBusyMemberId(userId);
      setMemberError(null);
      try {
        await api.addScopeMember(selectedId, userId, role);
        members.reload();
      } catch (reason: unknown) {
        setMemberError(errorOf(reason));
        members.reload();
      } finally {
        setBusyMemberId(null);
      }
    },
    [api, busyMemberId, members, selectedId],
  );

  const removeMember = useCallback(
    async (userId: Uuid): Promise<void> => {
      if (selectedId === null || busyMemberId !== null) return;
      setBusyMemberId(userId);
      setMemberError(null);
      try {
        await api.removeScopeMember(selectedId, userId);
        members.reload();
      } catch (reason: unknown) {
        setMemberError(errorOf(reason));
      } finally {
        setBusyMemberId(null);
      }
    },
    [api, busyMemberId, members, selectedId],
  );

  const header = (
    <PageHeader title="Scopes & Teams" subtitle="Project collaboration scopes and their members." />
  );

  if (scopes.loading) {
    return (
      <>
        {header}
        <StateView state="loading" title="Loading scopes" />
      </>
    );
  }
  if (scopes.error !== null) {
    return (
      <>
        {header}
        <StateView
          state="error"
          title="Could not load scopes"
          detail={scopes.error.message}
          onRetry={scopes.reload}
        />
      </>
    );
  }

  const scopeRows = scopes.data ?? [];
  const selectedScope = selected;
  // Capabilities travel on the caller-scoped listing; the global view carries
  // counts only. Absent caller scope row -> no manage affordances.
  const callerSelected = useMemo(() => {
    const row = (callerScopes.data ?? []).find((scope) => scope.id === selectedId);
    return row ?? null;
  }, [callerScopes.data, selectedId]);
  const canManage = callerSelected !== null && callerSelected.capabilities.manageMembers;

  const handleMemberUserIdChange = (event: ChangeEvent<HTMLInputElement>): void =>
    setMemberUserId(event.target.value);
  const handleMemberRoleChange = (event: ChangeEvent<HTMLSelectElement>): void => {
    setMemberRole(parseProjectRole(event.target.value));
  };
  const handleMemberSubmit = (event: FormEvent<HTMLFormElement>): void => {
    event.preventDefault();
    void submitMember();
  };

  return (
    <>
      {header}

      {scopeRows.length === 0 ? (
        <StateView
          state="empty"
          title="No scopes listed"
          detail="Platform scopes appear here."
        />
      ) : (
        <table className="table">
          <thead>
            <tr>
              <th scope="col">Name</th>
              <th scope="col">Kind</th>
              <th scope="col">Owner</th>
              <th scope="col">Members</th>
              <th scope="col">Workspaces</th>
            </tr>
          </thead>
          <tbody>
            {scopeRows.map((scope: AdminGlobalScopeDto) => (
              <tr key={scope.id}>
                <td>
                  <button
                    type="button"
                    className="btn"
                    onClick={() => selectScope(scope.id)}
                    aria-pressed={scope.id === selectedId}
                  >
                    {scope.name.trim() === "" ? "—" : scope.name}
                  </button>
                </td>
                <td>
                  <Badge tone={scope.kind === "team" ? "accent" : "neutral"}>{scope.kind}</Badge>
                </td>
                <td className="mono" title={scope.ownerUserId}>
                  {shortId(scope.ownerUserId)}
                </td>
                <td>{scope.memberCount}</td>
                <td>{scope.workspaceCount}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      {selectedScope === null ? (
        <Card title="Team members">
          <p className="muted">Select a scope above to manage its membership.</p>
        </Card>
      ) : (
        <Card title={`Team members — ${selectedScope.name.trim() === "" ? "unscoped" : selectedScope.name}`}>
          {members.loading ? (
            <StateView state="loading" title="Loading members" />
          ) : members.error !== null ? (
            <StateView
              state="error"
              title="Could not load members"
              detail={members.error.message}
              onRetry={members.reload}
            />
          ) : (members.data ?? []).length === 0 ? (
            <StateView state="empty" title="No members listed" />
          ) : (
            <table className="table">
              <thead>
                <tr>
                  <th scope="col">Subject</th>
                  <th scope="col">User</th>
                  <th scope="col">Role</th>
                  <th scope="col">Joined</th>
                  {canManage ? <th scope="col">Actions</th> : null}
                </tr>
              </thead>
              <tbody>
                {(members.data ?? []).map((member) => (
                  <tr key={member.userId}>
                    <td>{member.subject.trim() === "" ? "—" : member.subject}</td>
                    <td className="mono" title={member.userId}>
                      {shortId(member.userId)}
                    </td>
                    <td>
                      {canManage && busyMemberId === member.userId ? (
                        <Badge tone="warn">working…</Badge>
                      ) : (
                        <Badge tone={ROLE_TONE[member.role]}>{member.role}</Badge>
                      )}
                    </td>
                    <td>
                      {member.createdAt === null || member.createdAt.trim() === ""
                        ? "—"
                        : member.createdAt}
                    </td>
                    {canManage ? (
                      <td>
                        <span className="row">
                          <select
                            aria-label={`Role for ${member.userId}`}
                            value={member.role}
                            disabled={busyMemberId !== null}
                            onChange={(event) => {
                              void rerole(member.userId, parseProjectRole(event.target.value));
                            }}
                          >
                            {PROJECT_ROLES.map((role) => (
                              <option key={role} value={role}>
                                {role}
                              </option>
                            ))}
                          </select>
                          <button
                            type="button"
                            className="btn"
                            disabled={busyMemberId !== null}
                            onClick={() => void removeMember(member.userId)}
                          >
                            Remove
                          </button>
                        </span>
                      </td>
                    ) : null}
                  </tr>
                ))}
              </tbody>
            </table>
          )}

          {canManage ? (
            <form className="stack stack-tight" onSubmit={handleMemberSubmit}>
              <p className="muted">
                Adding an existing user reroles them; removals of the durable owner are refused by
                the control plane.
              </p>
              <div className="row">
                <input
                  aria-label="User id"
                  placeholder="User id (UUID)"
                  value={memberUserId}
                  disabled={addingMember || busyMemberId !== null}
                  onChange={handleMemberUserIdChange}
                />
                <select
                  aria-label="Role"
                  value={memberRole}
                  disabled={addingMember || busyMemberId !== null}
                  onChange={handleMemberRoleChange}
                >
                  {PROJECT_ROLES.map((role) => (
                    <option key={role} value={role}>
                      {role}
                    </option>
                  ))}
                </select>
                <button
                  type="submit"
                  className={
                    addingMember || memberUserId.trim().length === 0
                      ? "btn btn-primary btn-disabled"
                      : "btn btn-primary"
                  }
                  disabled={addingMember || memberUserId.trim().length === 0}
                >
                  {addingMember ? "Saving…" : "Add or update member"}
                </button>
              </div>
              {memberError !== null ? (
                <p role="alert" className="muted">
                  The membership change was refused: {memberError} Nothing changed; you can retry.
                </p>
              ) : null}
            </form>
          ) : (
            <p className="muted">
              Membership changes need the manage-members capability in this scope.
            </p>
          )}
        </Card>
      )}

      {scopeRows.length > 0 ? (
        <p className="muted">
          Scope kinds: <code>personal</code> is a single-user scope; <code>team</code> is
          multi-user collaboration ({PROJECT_KINDS.join(" / ")}). Counts are server-aggregated;
          member and workspace totals are platform-wide.
        </p>
      ) : null}
    </>
  );
}
