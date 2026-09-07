/**
 * Ordinary Team membership: roster, one-click add, writable role change,
 * and remove. Canonical Owner is displayed, never edited.
 */

import { useCallback, useMemo, useState, type ChangeEvent, type FormEvent } from "react";
import {
  addProjectMember,
  listProjectMembers,
  removeProjectMember,
  searchMemberCandidates,
  updateProjectMember,
} from "../api/api.ts";
import {
  parseWritableRole,
  WRITABLE_ROLES,
  type MemberCandidateDto,
  type ProjectMemberDto,
  type Role,
  type Uuid,
  type WritableRole,
} from "../api/dto.ts";
import { useResource } from "../hooks.ts";
import { memberLabel, PROJECT_ROLE_TONES } from "../projects/model.ts";
import { Badge, Card, StateView } from "../ui/primitives.tsx";

export type TeamMembersProps = {
  projectId: Uuid;
  ownerUserId: Uuid;
  canManage: boolean;
};

function usernameOf(member: { username: string | null }): string {
  const username = member.username?.trim() ?? "";
  return username !== "" ? username : "—";
}

function roleLabel(role: Role): string {
  switch (role) {
    case "owner":
      return "Owner";
    case "admin":
      return "Admin";
    case "member":
      return "Member";
    case "viewer":
      return "Viewer";
  }
}

function isCanonicalOwner(member: ProjectMemberDto, ownerUserId: Uuid): boolean {
  return member.userId === ownerUserId || member.role === "owner";
}

export function TeamMembers({ projectId, ownerUserId, canManage }: TeamMembersProps) {
  const [query, setQuery] = useState("");
  const [searched, setSearched] = useState(false);
  const [results, setResults] = useState<MemberCandidateDto[] | null>(null);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [candidateRoles, setCandidateRoles] = useState<Record<string, WritableRole>>({});
  const [addingUserId, setAddingUserId] = useState<Uuid | null>(null);
  const [busyUserId, setBusyUserId] = useState<Uuid | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const load = useCallback(
    async (signal: AbortSignal): Promise<ProjectMemberDto[]> => {
      return listProjectMembers(projectId, signal);
    },
    [projectId],
  );
  const resource = useResource(load, [projectId]);

  const search = useCallback(async (): Promise<void> => {
    const trimmed = query.trim();
    if (trimmed.length < 2) return;
    setSearchError(null);
    try {
      setResults(await searchMemberCandidates(projectId, trimmed));
      setSearched(true);
    } catch (reason: unknown) {
      setSearchError(reason instanceof Error ? reason.message : "request failed");
    }
  }, [projectId, query]);

  const add = useCallback(
    async (entry: MemberCandidateDto): Promise<void> => {
      if (addingUserId !== null || busyUserId !== null) return;
      const role = candidateRoles[entry.userId] ?? "member";
      setAddingUserId(entry.userId);
      setActionError(null);
      try {
        await addProjectMember(projectId, entry.userId, role);
        resource.reload();
        setResults((current) => current?.filter((item) => item.userId !== entry.userId) ?? null);
      } catch (reason: unknown) {
        setActionError(reason instanceof Error ? reason.message : "request failed");
      } finally {
        setAddingUserId(null);
      }
    },
    [addingUserId, busyUserId, candidateRoles, projectId, resource],
  );

  const rerole = useCallback(
    async (userId: Uuid, role: WritableRole): Promise<void> => {
      if (busyUserId !== null) return;
      setBusyUserId(userId);
      setActionError(null);
      try {
        await updateProjectMember(projectId, userId, role);
        resource.reload();
      } catch (reason: unknown) {
        setActionError(reason instanceof Error ? reason.message : "request failed");
        resource.reload();
      } finally {
        setBusyUserId(null);
      }
    },
    [busyUserId, projectId, resource],
  );

  const remove = useCallback(
    async (userId: Uuid): Promise<void> => {
      if (busyUserId !== null) return;
      setBusyUserId(userId);
      setActionError(null);
      try {
        await removeProjectMember(projectId, userId);
        resource.reload();
      } catch (reason: unknown) {
        setActionError(reason instanceof Error ? reason.message : "request failed");
      } finally {
        setBusyUserId(null);
      }
    },
    [busyUserId, projectId, resource],
  );

  const existingUserIds = useMemo(
    () => new Set((resource.data ?? []).map((member) => member.userId)),
    [resource.data],
  );

  if (resource.loading) {
    return (
      <Card title="Members">
        <StateView state="loading" title="Loading members" />
      </Card>
    );
  }
  if (resource.error !== null) {
    return (
      <Card title="Members">
        <StateView
          state="error"
          title="Could not load members"
          detail={resource.error.message}
          onRetry={resource.reload}
        />
      </Card>
    );
  }

  const members = resource.data ?? [];
  const busy = addingUserId !== null || busyUserId !== null;

  return (
    <Card title={`Members (${members.length})`}>
      {members.length === 0 ? (
        <StateView state="empty" title="No members yet" />
      ) : (
        <table className="table">
          <thead>
            <tr>
              <th scope="col">Name</th>
              <th scope="col">Username</th>
              <th scope="col">Role</th>
              {canManage ? <th scope="col">Actions</th> : null}
            </tr>
          </thead>
          <tbody>
            {members.map((member) => {
              const ownerRow = isCanonicalOwner(member, ownerUserId);
              return (
                <tr key={member.userId}>
                  <td>{memberLabel(member)}</td>
                  <td className="mono">{usernameOf(member)}</td>
                  <td>
                    {canManage && busyUserId === member.userId ? (
                      <Badge tone="warn">working…</Badge>
                    ) : (
                      <Badge tone={PROJECT_ROLE_TONES[member.role]}>{roleLabel(member.role)}</Badge>
                    )}
                  </td>
                  {canManage ? (
                    <td>
                      {ownerRow ? (
                        <span className="muted">Owner</span>
                      ) : (
                        <span className="row">
                          <select
                            aria-label={`Role for ${memberLabel(member)}`}
                            value={member.role === "owner" ? "admin" : member.role}
                            disabled={busy}
                            onChange={(event) => {
                              void rerole(member.userId, parseWritableRole(event.target.value));
                            }}
                          >
                            {WRITABLE_ROLES.map((role) => (
                              <option key={role} value={role}>
                                {roleLabel(role)}
                              </option>
                            ))}
                          </select>
                          <button
                            type="button"
                            className="btn"
                            disabled={busy}
                            onClick={() => void remove(member.userId)}
                          >
                            Remove
                          </button>
                        </span>
                      )}
                    </td>
                  ) : null}
                </tr>
              );
            })}
          </tbody>
        </table>
      )}

      {canManage ? (
        <form
          className="stack stack-tight"
          onSubmit={(event: FormEvent<HTMLFormElement>) => {
            event.preventDefault();
            void search();
          }}
        >
          <div className="row">
            <input
              aria-label="Search users"
              placeholder="Search users"
              value={query}
              disabled={busy}
              onChange={(event: ChangeEvent<HTMLInputElement>) => {
                setQuery(event.target.value);
                setSearched(false);
                setResults(null);
              }}
            />
            <button
              type="submit"
              className="btn"
              disabled={busy || query.trim().length < 2}
            >
              Search users
            </button>
          </div>
          {searchError !== null ? (
            <p role="alert" className="muted">
              Searching users failed: {searchError}. Nothing changed; you can retry.
            </p>
          ) : null}
          {searched && results !== null && results.length === 0 ? (
            <p className="muted">No users match that name.</p>
          ) : null}
          {results !== null && results.length > 0 ? (
            <table className="table">
              <thead>
                <tr>
                  <th scope="col">Name</th>
                  <th scope="col">Username</th>
                  <th scope="col">Role</th>
                  <th scope="col">Add</th>
                </tr>
              </thead>
              <tbody>
                {results.map((entry) => {
                  const alreadyMember = existingUserIds.has(entry.userId);
                  const displayName = entry.displayName?.trim() ?? "";
                  const role = candidateRoles[entry.userId] ?? "member";
                  return (
                    <tr key={entry.userId}>
                      <td>{displayName !== "" ? displayName : <span className="muted">—</span>}</td>
                      <td className="mono">{usernameOf(entry)}</td>
                      <td>
                        <select
                          aria-label={`Role for ${entry.username ?? entry.userId}`}
                          value={role}
                          disabled={busy || alreadyMember}
                          onChange={(event) => {
                            const next = parseWritableRole(event.target.value);
                            setCandidateRoles((current) => ({ ...current, [entry.userId]: next }));
                          }}
                        >
                          {WRITABLE_ROLES.map((option) => (
                            <option key={option} value={option}>
                              {roleLabel(option)}
                            </option>
                          ))}
                        </select>
                      </td>
                      <td>
                        {alreadyMember ? (
                          <Badge tone="neutral">member</Badge>
                        ) : (
                          <button
                            type="button"
                            className="btn btn-primary"
                            disabled={busy}
                            onClick={() => void add(entry)}
                          >
                            {addingUserId === entry.userId ? "Adding…" : "Add"}
                          </button>
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          ) : null}
          {actionError !== null ? (
            <p role="alert" className="muted">
              The membership change was refused: {actionError} Nothing changed; you can retry.
            </p>
          ) : null}
        </form>
      ) : (
        <p className="muted">Membership changes need the manage-members capability in this team.</p>
      )}
    </Card>
  );
}
