/**
 * Admin console — team member management.
 *
 * One team scope at a time: the roster renders every membership with its
 * role, owners and admins can change roles (the control plane upserts the
 * row), and new members are resolved by username through the directory —
 * never by pasting a UUID. Protected-owner refusals surface verbatim.
 */

import { useCallback, useMemo, useState, type FormEvent } from "react";
import {
  addProjectMember,
  getProject,
  listProjects,
  removeProjectMember,
} from "../api/api.ts";
import { searchDirectoryUsers, type DirectoryEntryDto } from "../api/console.ts";
import { ROLES, parseRole } from "../api/dto.ts";
import type { ProjectMemberDto, ProjectSummaryDto, Role } from "../api/dto.ts";
import { useResource } from "../hooks.ts";
import { Badge } from "../design-system/components/Badge";
import { Button } from "../design-system/components/Button";
import { Card, PageHeader } from "../design-system/components/Card";
import { StateView } from "../design-system/components/StateView";

/** Badge tone per membership role; mirrors the scope roster mapping. */
const ROLE_TONES: Record<Role, "accent" | "warn" | "ok" | "neutral"> = {
  owner: "accent",
  admin: "warn",
  member: "ok",
  viewer: "neutral",
};

function errorOf(reason: unknown): string {
  return reason instanceof Error ? reason.message : "request failed";
}

function labelOf(member: ProjectMemberDto): string {
  const name = member.displayName?.trim() ?? "";
  if (name !== "") return name;
  const username = member.username?.trim() ?? "";
  if (username !== "") return username;
  return member.subject?.trim() !== "" ? member.subject?.trim() ?? "—" : "—";
}

function usernameOrDash(member: ProjectMemberDto | DirectoryEntryDto): string {
  const username = member.username?.trim() ?? "";
  return username !== "" ? username : "—";
}

/** Team scopes only; a personal scope has no collaboration surface. */
async function loadTeamScopes(signal: AbortSignal): Promise<ProjectSummaryDto[]> {
  const scopes = await listProjects(signal);
  return scopes.filter((scope) => scope.kind === "team");
}

export function TeamMembersPage() {
  const scopes = useResource(loadTeamScopes);
  const [selectedScopeId, setSelectedScopeId] = useState<string | null>(null);
  const rosterScopeId = selectedScopeId ?? scopes.data?.[0]?.id ?? null;
  const selectedScope = useMemo(
    () => scopes.data?.find((scope) => scope.id === rosterScopeId) ?? null,
    [scopes.data, rosterScopeId],
  );

  const loadRoster = useCallback(
    async (signal: AbortSignal): Promise<ProjectMemberDto[]> => {
      if (rosterScopeId === null) return [];
      const detail = await getProject(rosterScopeId, signal);
      return detail.members;
    },
    [rosterScopeId],
  );
  const roster = useResource(loadRoster, [rosterScopeId]);

  const canManage = selectedScope?.capabilities.manageMembers === true;

  // Add-by-username state.
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<readonly DirectoryEntryDto[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [grantRole, setGrantRole] = useState<Role>("member");
  const [addingBusy, setAddingBusy] = useState(false);

  // Roster mutation state.
  const [busyUserId, setBusyUserId] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const runSearch = useCallback(async (): Promise<void> => {
    if (searching) return;
    setSearching(true);
    setSearchError(null);
    try {
      setResults(await searchDirectoryUsers(query));
    } catch (reason: unknown) {
      setResults(null);
      setSearchError(errorOf(reason));
    } finally {
      setSearching(false);
    }
  }, [query, searching]);

  const handleSearchSubmit = useCallback(
    (event: FormEvent<HTMLFormElement>): void => {
      event.preventDefault();
      void runSearch();
    },
    [runSearch],
  );

  const existingIds = useMemo(
    () => new Set((roster.data ?? []).map((member) => member.userId)),
    [roster.data],
  );

  const addMember = useCallback(
    async (entry: DirectoryEntryDto, role: Role): Promise<void> => {
      if (addingBusy || busyUserId !== null || rosterScopeId === null) return;
      setAddingBusy(true);
      setActionError(null);
      try {
        await addProjectMember(rosterScopeId, entry.userId, role);
        setResults(null);
        setQuery("");
        roster.reload();
      } catch (reason: unknown) {
        setActionError(errorOf(reason));
      } finally {
        setAddingBusy(false);
      }
    },
    [addingBusy, busyUserId, roster, rosterScopeId],
  );

  const reroleMember = useCallback(
    async (member: ProjectMemberDto, role: Role): Promise<void> => {
      if (busyUserId !== null || rosterScopeId === null) return;
      setBusyUserId(member.userId);
      setActionError(null);
      try {
        await addProjectMember(rosterScopeId, member.userId, role);
        roster.reload();
      } catch (reason: unknown) {
        setActionError(errorOf(reason));
      } finally {
        setBusyUserId(null);
      }
    },
    [busyUserId, roster, rosterScopeId],
  );

  const removeMember = useCallback(
    async (member: ProjectMemberDto): Promise<void> => {
      if (busyUserId !== null || rosterScopeId === null) return;
      setBusyUserId(member.userId);
      setActionError(null);
      try {
        await removeProjectMember(rosterScopeId, member.userId);
        roster.reload();
      } catch (reason: unknown) {
        setActionError(errorOf(reason));
      } finally {
        setBusyUserId(null);
      }
    },
    [busyUserId, roster, rosterScopeId],
  );

  const members = roster.data ?? [];

  return (
    <>
      <PageHeader
        title="Team members"
        subtitle="Membership of each team scope: roles, invites, and removals."
        actions={
          <select
            className="kds-select"
            aria-label="Team scope"
            value={rosterScopeId ?? ""}
            disabled={scopes.loading || (scopes.data?.length ?? 0) <= 1}
            onChange={(event) => setSelectedScopeId(event.target.value)}
          >
            {(scopes.data ?? []).map((scope) => (
              <option key={scope.id} value={scope.id}>
                {scope.name}
              </option>
            ))}
          </select>
        }
      />

      {scopes.error !== null ? (
        <StateView
          title="Could not load team scopes"
          detail={scopes.error.message}
          action={<Button onClick={scopes.reload}>Retry</Button>}
        />
      ) : (scopes.data?.length ?? 0) === 0 && !scopes.loading ? (
        <StateView
          title="No team scopes yet"
          detail="Create a team scope to start collaborating."
        />
      ) : rosterScopeId === null ? (
        <StateView title="Preparing scopes" />
      ) : (
        <>
          {actionError !== null ? (
            <p role="alert" className="kds-muted">
              The change was refused: {actionError} Nothing changed; you can retry.
            </p>
          ) : null}

          <Card title={`Members (${members.length})`}>
            {roster.loading ? (
              <StateView title="Loading members" />
            ) : members.length === 0 ? (
              <StateView
                title="No members yet"
                detail={canManage ? "Invite someone by username below." : undefined}
              />
            ) : (
              <table className="kds-table">
                <thead>
                  <tr>
                    <th scope="col">Name</th>
                    <th scope="col">Username</th>
                    <th scope="col">Role</th>
                    {canManage ? <th scope="col">Actions</th> : null}
                  </tr>
                </thead>
                <tbody>
                  {members.map((member) => (
                    <tr key={member.userId}>
                      <td>{labelOf(member)}</td>
                      <td className="kds-mono">{usernameOrDash(member)}</td>
                      <td>
                        {busyUserId === member.userId ? (
                          <Badge tone="pending">working…</Badge>
                        ) : (
                          <Badge tone={ROLE_TONES[member.role]}>{member.role}</Badge>
                        )}
                      </td>
                      {canManage ? (
                        <td>
                          <span className="kds-row">
                            <select
                              className="kds-select kds-select-sm"
                              aria-label={`Role for ${labelOf(member)}`}
                              value={member.role}
                              disabled={busyUserId !== null}
                              onChange={(event) =>
                                void reroleMember(member, parseRole(event.target.value))
                              }
                            >
                              {ROLES.map((role) => (
                                <option key={role} value={role}>
                                  {role}
                                </option>
                              ))}
                            </select>
                            <Button
                              size="sm"
                              variant="danger"
                              disabled={busyUserId !== null}
                              onClick={() => void removeMember(member)}
                            >
                              Remove
                            </Button>
                          </span>
                        </td>
                      ) : null}
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </Card>

          {canManage ? (
            <Card title="Add member" bodyClass="kds-pad">
              <form className="kds-stack" onSubmit={handleSearchSubmit}>
                <p className="kds-muted">
                  Search by username or display name. Adding an existing member reroles them;
                  the durable owner cannot be demoted or removed.
                </p>
                <div className="kds-row">
                  <input
                    className="kds-input"
                    aria-label="Search users by username"
                    placeholder="e.g. jdoe"
                    value={query}
                    disabled={searching || addingBusy || busyUserId !== null}
                    onChange={(event) => setQuery(event.target.value)}
                  />
                  <Button
                    type="submit"
                    disabled={searching || query.trim().length === 0}
                  >
                    {searching ? "Searching…" : "Search"}
                  </Button>
                </div>
                {searchError !== null ? (
                  <p role="alert" className="kds-muted">
                    Searching failed: {searchError} Nothing changed; you can retry.
                  </p>
                ) : null}
                {results !== null && results.length === 0 && !searching ? (
                  <p className="kds-muted">No users match that name.</p>
                ) : null}
              </form>

              {results !== null && results.length > 0 ? (
                <table className="kds-table">
                  <thead>
                    <tr>
                      <th scope="col">Name</th>
                      <th scope="col">Username</th>
                      <th scope="col">Role to grant</th>
                      <th scope="col">Add</th>
                    </tr>
                  </thead>
                  <tbody>
                    {results.map((entry) => {
                      const alreadyMember = existingIds.has(entry.userId);
                      const label = entry.displayName?.trim() || entry.username || entry.userId;
                      return (
                        <tr key={entry.userId}>
                          <td>{entry.displayName?.trim() || <span className="kds-muted">—</span>}</td>
                          <td className="kds-mono">{usernameOrDash(entry)}</td>
                          <td>
                            <select
                              className="kds-select kds-select-sm"
                              aria-label={`Role to grant ${label}`}
                              value={grantRole}
                              disabled={alreadyMember || addingBusy}
                              onChange={(event) => setGrantRole(parseRole(event.target.value))}
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
                              <Button
                                size="sm"
                                variant="primary"
                                disabled={addingBusy || busyUserId !== null}
                                onClick={() => void addMember(entry, grantRole)}
                              >
                                Add as {grantRole}
                              </Button>
                            )}
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              ) : null}
            </Card>
          ) : (
            <p className="kds-muted">
              Membership changes need the manage-members capability in this team.
            </p>
          )}
        </>
      )}
    </>
  );
}
