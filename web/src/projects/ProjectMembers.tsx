/**
 * Project members: roster plus the invite lifecycle for a team project. Search
 * runs against the user directory by username or display name; adding an
 * existing member reroles them, removals are refused by the control plane
 * for protected owners. Every action is gated by the manage-members
 * capability of the project.
 */

import { useCallback, useMemo, useState, type ChangeEvent, type FormEvent } from "react";
import { addProjectMember, getProject, removeProjectMember, searchProjectUsers } from "../api/api.ts";
import {
  ROLES,
  parseRole,
  type ProjectMemberDto,
  type Role,
  type UserDirectoryEntryDto,
  type Uuid,
} from "../api/dto.ts";
import { useResource } from "../hooks.ts";
import { Badge, Card, StateView } from "../ui/primitives.tsx";
import { memberLabel, PROJECT_ROLE_TONES, shortId } from "./model.ts";

export type ProjectMembersProps = {
  projectId: Uuid;
  canManage: boolean;
};

export function ProjectMembers({ projectId, canManage }: ProjectMembersProps) {
  const [query, setQuery] = useState("");
  const [searched, setSearched] = useState(false);
  const [results, setResults] = useState<UserDirectoryEntryDto[] | null>(null);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [selectedUserId, setSelectedUserId] = useState<Uuid | null>(null);
  const [memberRole, setMemberRole] = useState<Role>("member");
  const [adding, setAdding] = useState(false);
  const [busyUserId, setBusyUserId] = useState<Uuid | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const load = useCallback(
    async (signal: AbortSignal): Promise<ProjectMemberDto[]> => {
      const detail = await getProject(projectId, signal);
      return detail.members;
    },
    [projectId],
  );
  const resource = useResource(load, [projectId]);

  const existingUserIds = useMemo(
    () => new Set((resource.data ?? []).map((member) => member.userId)),
    [resource.data],
  );

  const search = useCallback(async (): Promise<void> => {
    const trimmed = query.trim();
    if (trimmed.length === 0) return;
    setSearchError(null);
    try {
      setResults(await searchProjectUsers(trimmed));
      setSearched(true);
    } catch (reason: unknown) {
      setSearchError(reason instanceof Error ? reason.message : "request failed");
    }
  }, [query]);

  const handleQueryChange = useCallback((event: ChangeEvent<HTMLInputElement>) => {
    setQuery(event.target.value);
    setSearched(false);
    setResults(null);
  }, []);

  const handleRoleChange = useCallback((event: ChangeEvent<HTMLSelectElement>) => {
    setMemberRole(parseRole(event.target.value));
  }, []);

  const handleSearchSubmit = useCallback(
    (event: FormEvent<HTMLFormElement>) => {
      event.preventDefault();
      void search();
    },
    [search],
  );

  const add = useCallback(async (): Promise<void> => {
    if (selectedUserId === null || adding || busyUserId !== null) return;
    setAdding(true);
    setActionError(null);
    try {
      await addProjectMember(projectId, selectedUserId, memberRole);
      resource.reload();
      setSelectedUserId(null);
      setSearched(false);
      setResults(null);
      setQuery("");
    } catch (reason: unknown) {
      setActionError(reason instanceof Error ? reason.message : "request failed");
    } finally {
      setAdding(false);
    }
  }, [adding, busyUserId, memberRole, resource, projectId, selectedUserId]);

  const rerole = useCallback(
    async (userId: Uuid, role: Role): Promise<void> => {
      if (busyUserId !== null) return;
      setBusyUserId(userId);
      setActionError(null);
      try {
        await addProjectMember(projectId, userId, role);
        resource.reload();
      } catch (reason: unknown) {
        setActionError(reason instanceof Error ? reason.message : "request failed");
        resource.reload();
      } finally {
        setBusyUserId(null);
      }
    },
    [busyUserId, resource, projectId],
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
    [busyUserId, resource, projectId],
  );

  if (resource.loading) {
    return (
      <>
        <Card title="Members">
          <StateView state="loading" title="Loading members" />
        </Card>
      </>
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
  const selected = results?.find((entry) => entry.userId === selectedUserId) ?? null;

  return (
    <Card title={`Members (${members.length})`}>
      {members.length === 0 ? (
        <StateView
          state="empty"
          title="No members yet"
          detail={canManage ? "Invite someone by username or display name to start collaborating." : undefined}
        />
      ) : (
        <table className="table">
          <thead>
            <tr>
              <th scope="col">Name</th>
              <th scope="col">Username</th>
              <th scope="col">User</th>
              <th scope="col">Role</th>
              {canManage ? <th scope="col">Actions</th> : null}
            </tr>
          </thead>
          <tbody>
            {members.map((member) => (
              <tr key={member.userId}>
                <td>{memberLabel(member)}</td>
                <td className="mono">
                  {member.username !== null && member.username.trim() !== "" ? (
                    member.username
                  ) : (
                    <span className="muted">—</span>
                  )}
                </td>
                <td className="mono" title={member.userId}>
                  {shortId(member.userId)}
                </td>
                <td>
                  {canManage && busyUserId === member.userId ? (
                    <Badge tone="warn">working…</Badge>
                  ) : (
                    <Badge tone={PROJECT_ROLE_TONES[member.role]}>{member.role}</Badge>
                  )}
                </td>
                {canManage ? (
                  <td>
                    <span className="row">
                      <select
                        aria-label={`Role for ${member.userId}`}
                        value={member.role}
                        disabled={busyUserId !== null}
                        onChange={(event) => {
                          void rerole(member.userId, parseRole(event.target.value));
                        }}
                      >
                        {ROLES.map((role) => (
                          <option key={role} value={role}>
                            {role}
                          </option>
                        ))}
                      </select>
                      <button
                        type="button"
                        className="btn"
                        disabled={busyUserId !== null}
                        onClick={() => void remove(member.userId)}
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
            Adding an existing user reroles them; removals of protected owners are refused by the
            control plane.
          </p>
          <div className="row">
            <input
              aria-label="Search users"
              placeholder="Search by username or display name"
              value={query}
              disabled={adding || busyUserId !== null}
              onChange={handleQueryChange}
            />
            <button
              type="submit"
              className="btn"
              disabled={adding || busyUserId !== null || query.trim().length === 0}
            >
              Search
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
                  <th scope="col">Status</th>
                  <th scope="col">Role to grant</th>
                  <th scope="col">Invite</th>
                </tr>
              </thead>
              <tbody>
                {results.map((entry) => {
                  const alreadyMember = existingUserIds.has(entry.userId);
                  const displayName = entry.displayName?.trim() ?? "";
                  const username = entry.username?.trim() ?? "";
                  return (
                    <tr key={entry.userId} className={selectedUserId === entry.userId ? "session-row-active" : undefined}>
                      <td>{displayName !== "" ? displayName : <span className="muted">—</span>}</td>
                      <td className="mono">{username !== "" ? username : <span className="muted">—</span>}</td>
                      <td>
                        <Badge tone={entry.status === "active" ? "ok" : "fail"}>
                          {entry.status ?? "unknown"}
                        </Badge>
                      </td>
                      <td>
                        <select
                          aria-label={`Role to grant ${entry.username ?? entry.userId}`}
                          value={selectedUserId === entry.userId ? memberRole : "member"}
                          disabled={adding || busyUserId !== null || alreadyMember}
                          onChange={(event) => {
                            setSelectedUserId(entry.userId);
                            setMemberRole(parseRole(event.target.value));
                          }}
                        >
                          {ROLES.map((role) => (
                            <option key={role} value={role}>
                              {role}
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
                            disabled={adding || busyUserId !== null}
                            onClick={() => {
                              const wasSelected = selectedUserId === entry.userId;
                              setSelectedUserId(entry.userId);
                              if (!wasSelected) setMemberRole("member");
                            }}
                          >
                            {selectedUserId === entry.userId ? "Selected — add" : "Add"}
                          </button>
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          ) : null}
          {selectedUserId !== null && selected !== null ? (
            <div className="row spread">
              <span className="muted">
                Adding {selected.username !== null && selected.username.trim() !== "" ? selected.username : selected.displayName ?? selected.userId}
                {" "}as <span className="mono">{memberRole}</span>.
              </span>
              <button
                type="button"
                className={adding ? "btn btn-primary btn-disabled" : "btn btn-primary"}
                disabled={adding || busyUserId !== null}
                onClick={() => void add()}
              >
                {adding ? "Adding…" : `Add ${memberRole}`}
              </button>
            </div>
          ) : null}
          {actionError !== null ? (
            <p role="alert" className="muted">
              The membership change was refused: {actionError} Nothing changed; you can retry.
            </p>
          ) : null}
        </form>
      ) : (
        <p className="muted">
          Membership changes need the manage-members capability in this project.
        </p>
      )}
    </Card>
  );
}
