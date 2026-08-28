/**
 * Scopes & Teams: project collaboration scopes and their membership.
 *
 * A scope is a Project row with a collaboration `kind` (personal | team).
 * The platform-wide table comes from GET /api/admin/scopes with
 * server-aggregated member and workspace counts. Selecting a scope loads
 * membership through the explicit platform-admin recovery surface:
 * GET/POST/DELETE /api/admin/scopes/:id/members. That path does not join
 * the admin to the Team and does not widen the ordinary membership API.
 *
 * Personal membership stays fixed. Durable Team owners stay protected.
 * New members are found by username or display name, never by pasting a
 * UUID. Refusals surface verbatim.
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
import { searchScopeUsers } from "../api/scopes.ts";
import type { UserDirectoryEntryDto, Uuid } from "../api/dto.ts";
import { useResource } from "../hooks.ts";
import { Badge, Card, PageHeader, StateView } from "../ui/primitives.tsx";

function shortId(id: string): string {
  return id.length === 0 ? "—" : id.length <= 10 ? id : `${id.slice(0, 8)}…`;
}

function errorOf(reason: unknown): string {
  return reason instanceof Error ? reason.message : "request failed";
}

function memberLabel(member: AdminScopeMemberDto): string {
  const name = member.displayName?.trim() ?? "";
  if (name !== "") return name;
  const username = member.username?.trim() ?? "";
  if (username !== "") return username;
  const subject = member.subject.trim();
  return subject !== "" ? subject : "—";
}

function usernameOf(member: AdminScopeMemberDto | UserDirectoryEntryDto): string {
  const username = member.username?.trim() ?? "";
  return username !== "" ? username : "—";
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
  const scopesLoad = useCallback((signal: AbortSignal) => api.getAdminScopes(signal), [api]);
  const scopes = useResource(scopesLoad);
  const [selectedId, setSelectedId] = useState<Uuid | null>(null);

  const membersLoad = useCallback(
    (signal: AbortSignal): Promise<AdminScopeMemberDto[]> => {
      if (selectedId === null) return Promise.resolve([]);
      return api.listScopeMembers(selectedId, signal);
    },
    [api, selectedId],
  );
  const members = useResource(membersLoad, [selectedId]);

  const [query, setQuery] = useState("");
  const [results, setResults] = useState<UserDirectoryEntryDto[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [memberRole, setMemberRole] = useState<ProjectRole>("member");
  const [addingMember, setAddingMember] = useState(false);
  const [busyMemberId, setBusyMemberId] = useState<Uuid | null>(null);
  const [memberError, setMemberError] = useState<string | null>(null);

  const selected = useMemo(() => {
    const row = (scopes.data ?? []).find((scope) => scope.id === selectedId);
    return row ?? null;
  }, [scopes.data, selectedId]);

  const existingIds = useMemo(
    () => new Set((members.data ?? []).map((member) => member.userId)),
    [members.data],
  );

  const selectScope = useCallback((id: Uuid): void => {
    setSelectedId(id);
    setMemberError(null);
    setBusyMemberId(null);
    setQuery("");
    setResults(null);
    setSearchError(null);
  }, []);

  const runSearch = useCallback(async (): Promise<void> => {
    const trimmed = query.trim();
    if (searching || trimmed.length === 0) return;
    setSearching(true);
    setSearchError(null);
    try {
      setResults(await searchScopeUsers(trimmed));
    } catch (reason: unknown) {
      setResults(null);
      setSearchError(errorOf(reason));
    } finally {
      setSearching(false);
    }
  }, [query, searching]);

  const addMember = useCallback(
    async (entry: UserDirectoryEntryDto, role: ProjectRole): Promise<void> => {
      if (selectedId === null || addingMember || busyMemberId !== null) return;
      setAddingMember(true);
      setMemberError(null);
      try {
        await api.addScopeMember(selectedId, entry.userId, role);
        setQuery("");
        setResults(null);
        members.reload();
      } catch (reason: unknown) {
        setMemberError(errorOf(reason));
      } finally {
        setAddingMember(false);
      }
    },
    [api, addingMember, busyMemberId, members, selectedId],
  );

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
  const canManage = selectedScope !== null && selectedScope.kind === "team";

  const handleQueryChange = (event: ChangeEvent<HTMLInputElement>): void =>
    setQuery(event.target.value);
  const handleMemberRoleChange = (event: ChangeEvent<HTMLSelectElement>): void => {
    setMemberRole(parseProjectRole(event.target.value));
  };
  const handleSearchSubmit = (event: FormEvent<HTMLFormElement>): void => {
    event.preventDefault();
    void runSearch();
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
                  <th scope="col">Name</th>
                  <th scope="col">Username</th>
                  <th scope="col">Role</th>
                  <th scope="col">Joined</th>
                  {canManage ? <th scope="col">Actions</th> : null}
                </tr>
              </thead>
              <tbody>
                {(members.data ?? []).map((member) => (
                  <tr key={member.userId}>
                    <td>{memberLabel(member)}</td>
                    <td className="mono">{usernameOf(member)}</td>
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
                            aria-label={`Role for ${memberLabel(member)}`}
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
            <form className="stack stack-tight" onSubmit={handleSearchSubmit}>
              <p className="muted">
                Search by username or display name. Adding an existing member reroles them;
                the durable owner cannot be demoted or removed.
              </p>
              <div className="row">
                <input
                  aria-label="Search users by username or display name"
                  placeholder="e.g. jdoe"
                  value={query}
                  disabled={searching || addingMember || busyMemberId !== null}
                  onChange={handleQueryChange}
                />
                <select
                  aria-label="Role to grant"
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
                    searching || query.trim().length === 0
                      ? "btn btn-primary btn-disabled"
                      : "btn btn-primary"
                  }
                  disabled={searching || query.trim().length === 0}
                >
                  {searching ? "Searching…" : "Search"}
                </button>
              </div>
              {searchError !== null ? (
                <p role="alert" className="muted">
                  Searching failed: {searchError} Nothing changed; you can retry.
                </p>
              ) : null}
              {results !== null && results.length === 0 && !searching ? (
                <p className="muted">No users match that name.</p>
              ) : null}
              {results !== null && results.length > 0 ? (
                <table className="table">
                  <thead>
                    <tr>
                      <th scope="col">Name</th>
                      <th scope="col">Username</th>
                      <th scope="col">Add</th>
                    </tr>
                  </thead>
                  <tbody>
                    {results.map((entry) => {
                      const alreadyMember = existingIds.has(entry.userId);
                      const label = entry.displayName?.trim() || entry.username || "—";
                      return (
                        <tr key={entry.userId}>
                          <td>
                            {entry.displayName?.trim() || <span className="muted">—</span>}
                          </td>
                          <td className="mono">{usernameOf(entry)}</td>
                          <td>
                            {alreadyMember ? (
                              <Badge tone="neutral">member</Badge>
                            ) : (
                              <button
                                type="button"
                                className="btn btn-primary"
                                disabled={addingMember || busyMemberId !== null}
                                onClick={() => void addMember(entry, memberRole)}
                              >
                                Add {label} as {memberRole}
                              </button>
                            )}
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              ) : null}
              {memberError !== null ? (
                <p role="alert" className="muted">
                  The membership change was refused: {memberError} Nothing changed; you can retry.
                </p>
              ) : null}
            </form>
          ) : selectedScope.kind === "personal" ? (
            <p className="muted">Personal membership is fixed to the owner.</p>
          ) : (
            <p className="muted">Membership of this scope cannot be changed here.</p>
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
