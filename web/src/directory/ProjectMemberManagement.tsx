import { useCallback, useMemo, useState } from "react";
import {
  SCOPE_ROLES,
  directoryApi,
  type DirectoryApi,
  type DirectoryScopeMemberDto,
  type DirectoryUserDto,
  type ScopeRole,
  type Uuid,
} from "../api/directory.ts";
import { useResource } from "../hooks.ts";
import { Badge, Card, StateView } from "../ui/primitives.tsx";
import { UserDirectorySearch } from "./UserDirectorySearch.tsx";

export type MembershipChange =
  | { kind: "added" | "role-updated"; member: DirectoryScopeMemberDto }
  | { kind: "removed"; member: DirectoryScopeMemberDto };

export type ProjectMemberManagementProps = {
  scopeId: Uuid;
  /** Inject a capability-scoped adapter; the server remains authoritative. */
  api?: DirectoryApi | undefined;
  /** The owning page should pass the server-emitted manage-members capability. */
  canManage?: boolean | undefined;
  /** Own-role and own-membership controls stay disabled when this is present. */
  currentUserId?: Uuid | null | undefined;
  onMemberSelect?: ((member: DirectoryScopeMemberDto) => void) | undefined;
  onMemberSelectionChange?:
    | ((member: DirectoryScopeMemberDto | null) => void)
    | undefined;
  onUserSelect?: ((user: DirectoryUserDto) => void) | undefined;
  onUserSelectionChange?: ((user: DirectoryUserDto | null) => void) | undefined;
  onMembershipChange?: ((change: MembershipChange) => void | Promise<void>) | undefined;
};

function errorMessage(reason: unknown): string {
  return reason instanceof Error ? reason.message : "request failed";
}

function displayLabel(user: DirectoryUserDto): string {
  const name = user.displayName?.trim() ?? "";
  if (name.length > 0) return name;
  const handle = user.username?.trim() ?? "";
  return handle.length > 0 ? handle : "Unnamed user";
}

function field(value: string | null): string {
  const text = value?.trim() ?? "";
  return text.length > 0 ? text : "—";
}

function statusTone(status: DirectoryScopeMemberDto["status"]): "ok" | "fail" | "warn" {
  if (status === "active") return "ok";
  if (status === "disabled") return "fail";
  return "warn";
}

function statusLabel(status: DirectoryScopeMemberDto["status"]): string {
  return status === "unknown" ? "status unavailable" : status;
}

/**
 * Project roster plus a human-label user picker. IDs are used only as React
 * keys and adapter arguments; no UUID or provider subject is pasted or shown
 * to an operator. All membership refusals come from the server seam.
 */
export function ProjectMemberManagement({
  scopeId,
  api = directoryApi,
  canManage = false,
  currentUserId = null,
  onMemberSelect,
  onMemberSelectionChange,
  onUserSelect,
  onUserSelectionChange,
  onMembershipChange,
}: ProjectMemberManagementProps) {
  const [selectedUser, setSelectedUser] = useState<DirectoryUserDto | null>(null);
  const [selectedRole, setSelectedRole] = useState<ScopeRole>("member");
  const [busyUserId, setBusyUserId] = useState<Uuid | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const loadMembers = useCallback(
    (signal: AbortSignal): Promise<DirectoryScopeMemberDto[]> =>
      api.listScopeMembers(scopeId, signal),
    [api, scopeId],
  );
  const resource = useResource(loadMembers, [api, scopeId]);
  const members = resource.data ?? [];
  const existingUserIds = useMemo(
    () => new Set(members.map((member) => member.userId)),
    [members],
  );

  const searchUsers = useCallback(
    (query: string, signal?: AbortSignal): Promise<readonly DirectoryUserDto[]> =>
      api.searchScopeUsers(query, signal),
    [api],
  );

  const handleUserSelect = useCallback(
    (user: DirectoryUserDto): void => {
      setSelectedUser(user);
      setSelectedRole("member");
      onUserSelect?.(user);
      onUserSelectionChange?.(user);
    },
    [onUserSelect, onUserSelectionChange],
  );

  const handleUserSelectionChange = useCallback(
    (user: DirectoryUserDto | null): void => {
      if (user !== null) return;
      setSelectedUser(null);
      onUserSelectionChange?.(null);
    },
    [onUserSelectionChange],
  );

  const selectMember = useCallback(
    (member: DirectoryScopeMemberDto): void => {
      onMemberSelect?.(member);
      onMemberSelectionChange?.(member);
    },
    [onMemberSelect, onMemberSelectionChange],
  );

  const addSelectedMember = useCallback(async (): Promise<void> => {
    if (!canManage || selectedUser === null || busyUserId !== null) return;
    if (selectedUser.status === "disabled") {
      setActionError("Disabled users cannot be added to a Project scope.");
      return;
    }
    if (existingUserIds.has(selectedUser.userId)) {
      setActionError(`${displayLabel(selectedUser)} is already a member of this scope.`);
      return;
    }

    setBusyUserId(selectedUser.userId);
    setActionError(null);
    try {
      const member = await api.addScopeMember(
        scopeId,
        selectedUser.userId,
        selectedRole,
      );
      resource.reload();
      await onMembershipChange?.({ kind: "added", member });
      setSelectedUser(null);
      setSelectedRole("member");
    } catch (reason: unknown) {
      setActionError(`Adding ${displayLabel(selectedUser)} failed: ${errorMessage(reason)}.`);
    } finally {
      setBusyUserId(null);
    }
  }, [
    api,
    busyUserId,
    canManage,
    existingUserIds,
    onMembershipChange,
    resource,
    scopeId,
    selectedRole,
    selectedUser,
  ]);

  const updateRole = useCallback(
    async (member: DirectoryScopeMemberDto, role: ScopeRole): Promise<void> => {
      if (!canManage || busyUserId !== null || member.userId === currentUserId) return;
      setBusyUserId(member.userId);
      setActionError(null);
      try {
        const updated = await api.addScopeMember(scopeId, member.userId, role);
        resource.reload();
        await onMembershipChange?.({ kind: "role-updated", member: updated });
      } catch (reason: unknown) {
        setActionError(`Changing ${displayLabel(member)} failed: ${errorMessage(reason)}.`);
        resource.reload();
      } finally {
        setBusyUserId(null);
      }
    },
    [api, busyUserId, canManage, currentUserId, onMembershipChange, resource, scopeId],
  );

  const removeMember = useCallback(
    async (member: DirectoryScopeMemberDto): Promise<void> => {
      if (!canManage || busyUserId !== null || member.userId === currentUserId) return;
      setBusyUserId(member.userId);
      setActionError(null);
      try {
        await api.removeScopeMember(scopeId, member.userId);
        resource.reload();
        await onMembershipChange?.({ kind: "removed", member });
      } catch (reason: unknown) {
        setActionError(`Removing ${displayLabel(member)} failed: ${errorMessage(reason)}.`);
      } finally {
        setBusyUserId(null);
      }
    },
    [api, busyUserId, canManage, currentUserId, onMembershipChange, resource, scopeId],
  );

  if (resource.loading && resource.data === null) {
    return (
      <Card title="Project members">
        <StateView state="loading" title="Loading members" />
      </Card>
    );
  }
  if (resource.error !== null && resource.data === null) {
    return (
      <Card title="Project members">
        <StateView
          state="error"
          title="Could not load members"
          detail={resource.error.message}
          onRetry={resource.reload}
        />
      </Card>
    );
  }

  return (
    <Card title={`Project members (${members.length})`}>
      {members.length === 0 ? (
        <StateView
          state="empty"
          title="No members yet"
          detail={canManage ? "Search by username, display name, or email to add a member." : undefined}
        />
      ) : (
        <table className="table directory-members">
          <thead>
            <tr>
              <th scope="col">Name</th>
              <th scope="col">Username</th>
              <th scope="col">Email</th>
              <th scope="col">Status</th>
              <th scope="col">Role</th>
              {canManage ? <th scope="col">Actions</th> : null}
            </tr>
          </thead>
          <tbody>
            {members.map((member) => {
              const ownMembership = member.userId === currentUserId;
              const working = busyUserId === member.userId;
              return (
                <tr key={member.userId} className={member.status === "disabled" ? "directory-row-disabled" : undefined}>
                  <td>
                    <button
                      type="button"
                      className="btn"
                      onClick={() => selectMember(member)}
                    >
                      {displayLabel(member)}
                    </button>
                    {member.status === "disabled" ? <span className="muted"> (disabled)</span> : null}
                  </td>
                  <td className="mono">{field(member.username)}</td>
                  <td>{field(member.email)}</td>
                  <td>
                    <Badge tone={statusTone(member.status)}>{statusLabel(member.status)}</Badge>
                  </td>
                  <td>
                    {working ? <Badge tone="warn">working…</Badge> : <Badge>{member.role}</Badge>}
                  </td>
                  {canManage ? (
                    <td>
                      <span className="row">
                        <select
                          aria-label={`Role for ${displayLabel(member)}`}
                          value={member.role}
                          disabled={busyUserId !== null || ownMembership}
                          title={ownMembership ? "Your own role is managed by another owner or admin" : undefined}
                          onChange={(event) => {
                            const role = SCOPE_ROLES.find((candidate) => candidate === event.target.value) ?? "viewer";
                            void updateRole(member, role);
                          }}
                        >
                          {SCOPE_ROLES.map((role) => (
                            <option key={role} value={role}>
                              {role}
                            </option>
                          ))}
                        </select>
                        <button
                          type="button"
                          className="btn btn-danger"
                          disabled={busyUserId !== null || ownMembership}
                          title={ownMembership ? "You cannot remove yourself from the scope" : undefined}
                          onClick={() => void removeMember(member)}
                        >
                          Remove
                        </button>
                      </span>
                    </td>
                  ) : null}
                </tr>
              );
            })}
          </tbody>
        </table>
      )}

      {resource.error !== null ? (
        <p role="alert" className="muted">
          Refreshing members failed: {resource.error.message}.
        </p>
      ) : null}

      {canManage ? (
        <div className="stack stack-tight">
          <UserDirectorySearch
            search={searchUsers}
            onSelect={handleUserSelect}
            onSelectionChange={handleUserSelectionChange}
            selectedUserId={selectedUser?.userId ?? null}
            emptyMessage="No users match that search."
          />
          {selectedUser !== null ? (
            <div className="row spread">
              <span className="muted">
                {existingUserIds.has(selectedUser.userId)
                  ? `${displayLabel(selectedUser)} is already a member.`
                  : `Add ${displayLabel(selectedUser)} as`}
              </span>
              {!existingUserIds.has(selectedUser.userId) ? (
                <>
                  <select
                    aria-label={`Role for ${displayLabel(selectedUser)}`}
                    value={selectedRole}
                    disabled={busyUserId !== null || selectedUser.status === "disabled"}
                    onChange={(event) => {
                      const role = SCOPE_ROLES.find((candidate) => candidate === event.target.value) ?? "member";
                      setSelectedRole(role);
                    }}
                  >
                    {SCOPE_ROLES.map((role) => (
                      <option key={role} value={role}>
                        {role}
                      </option>
                    ))}
                  </select>
                  <button
                    type="button"
                    className="btn btn-primary"
                    disabled={busyUserId !== null || selectedUser.status === "disabled"}
                    onClick={() => void addSelectedMember()}
                  >
                    {busyUserId === selectedUser.userId ? "Adding…" : "Add member"}
                  </button>
                </>
              ) : null}
            </div>
          ) : null}
          {actionError !== null ? (
            <p role="alert" className="muted">
              {actionError}
            </p>
          ) : null}
        </div>
      ) : (
        <p className="muted">Membership changes need the manage-members capability in this scope.</p>
      )}
    </Card>
  );
}

/** Alias for pages that call the feature a scope member directory. */
export const ScopeMemberDirectory = ProjectMemberManagement;
