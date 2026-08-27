import { useCallback, useState, type ReactNode } from "react";
import {
  DIRECTORY_PLATFORM_ROLES,
  DIRECTORY_STATUSES,
  directoryApi,
  type DirectoryApi,
  type DirectoryStatus,
  type DirectoryUserDto,
  type PlatformRole,
  type UserStatus,
  type Uuid,
} from "../api/directory.ts";
import { Badge, Card } from "../ui/primitives.tsx";
import { UserDirectorySearch } from "./UserDirectorySearch.tsx";

export type AdminUserDirectoryProps = {
  /** Inject a capability-scoped adapter in tests or an embedded admin shell. */
  api?: DirectoryApi | undefined;
  /** Server capability gate for role/status controls. */
  canManageUsers?: boolean | undefined;
  onSelect?: ((user: DirectoryUserDto) => void) | undefined;
  onSelectionChange?: ((user: DirectoryUserDto | null) => void) | undefined;
  selectedUserId?: Uuid | null | undefined;
  /** Called after the corresponding server mutation succeeds. */
  onPlatformRoleChange?:
    | ((user: DirectoryUserDto, role: PlatformRole) => void | Promise<void>)
    | undefined;
  onStatusChange?:
    | ((user: DirectoryUserDto, status: UserStatus) => void | Promise<void>)
    | undefined;
};

function errorMessage(reason: unknown): string {
  return reason instanceof Error ? reason.message : "request failed";
}

function displayLabel(user: DirectoryUserDto): string {
  const displayName = user.displayName?.trim() ?? "";
  if (displayName.length > 0) return displayName;
  const username = user.username?.trim() ?? "";
  return username.length > 0 ? username : "this user";
}

function statusForSelect(status: DirectoryStatus): UserStatus {
  return status === "disabled" ? "disabled" : "active";
}

/**
 * Platform-admin directory. Human labels are the only visible selection
 * affordance; durable user identities are retained solely in callbacks/API
 * arguments. The server remains authoritative for admin capability checks.
 */
export function AdminUserDirectory({
  api = directoryApi,
  canManageUsers = true,
  onSelect,
  onSelectionChange,
  selectedUserId,
  onPlatformRoleChange,
  onStatusChange,
}: AdminUserDirectoryProps) {
  const [busyUserId, setBusyUserId] = useState<Uuid | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [reloadToken, setReloadToken] = useState(0);

  const updatePlatformRole = useCallback(
    async (user: DirectoryUserDto, role: PlatformRole): Promise<void> => {
      if (busyUserId !== null) return;
      const update = api.setPlatformRole;
      if (update === undefined && onPlatformRoleChange === undefined) {
        setActionError("Platform-role management is unavailable for this surface.");
        return;
      }
      setBusyUserId(user.userId);
      setActionError(null);
      try {
        if (update !== undefined) await api.setPlatformRole?.(user.userId, role);
        await onPlatformRoleChange?.(user, role);
        setReloadToken((token) => token + 1);
      } catch (reason: unknown) {
        setActionError(`Changing ${displayLabel(user)} failed: ${errorMessage(reason)}.`);
      } finally {
        setBusyUserId(null);
      }
    },
    [api, busyUserId, onPlatformRoleChange],
  );

  const updateStatus = useCallback(
    async (user: DirectoryUserDto, status: UserStatus): Promise<void> => {
      if (busyUserId !== null) return;
      const update = api.setUserStatus;
      if (update === undefined && onStatusChange === undefined) {
        setActionError("User-status management is unavailable for this surface.");
        return;
      }
      setBusyUserId(user.userId);
      setActionError(null);
      try {
        if (update !== undefined) await api.setUserStatus?.(user.userId, status);
        await onStatusChange?.(user, status);
        setReloadToken((token) => token + 1);
      } catch (reason: unknown) {
        setActionError(`Changing ${displayLabel(user)} failed: ${errorMessage(reason)}.`);
      } finally {
        setBusyUserId(null);
      }
    },
    [api, busyUserId, onStatusChange],
  );

  const renderActions = useCallback(
    (user: DirectoryUserDto): ReactNode => {
      const working = busyUserId === user.userId;
      return (
        <span className="row">
          <select
            aria-label={`Platform role for ${displayLabel(user)}`}
            value={user.platformRole === "admin" ? "admin" : "user"}
            disabled={busyUserId !== null}
            onChange={(event) => {
              const role: PlatformRole = event.target.value === "admin" ? "admin" : "user";
              void updatePlatformRole(user, role);
            }}
          >
            {PLATFORM_ROLE_OPTIONS.map((role) => (
              <option key={role} value={role}>
                {role}
              </option>
            ))}
          </select>
          <select
            aria-label={`Status for ${displayLabel(user)}`}
            value={statusForSelect(user.status)}
            disabled={busyUserId !== null}
            onChange={(event) => {
              const status: UserStatus = event.target.value === "disabled" ? "disabled" : "active";
              void updateStatus(user, status);
            }}
          >
            {USER_STATUS_OPTIONS.map((status) => (
              <option key={status} value={status}>
                {status}
              </option>
            ))}
          </select>
          {working ? <Badge tone="warn">working…</Badge> : null}
        </span>
      );
    },
    [busyUserId, updatePlatformRole, updateStatus],
  );

  const searchAdminUsers = useCallback(
    (query: string, signal?: AbortSignal) => api.searchAdminUsers(query, signal),
    [api],
  );

  const searchProps = {
    search: searchAdminUsers,
    onSelect,
    onSelectionChange,
    selectedUserId,
    searchOnMount: true,
    minQueryLength: 0,
    showPlatformRole: true,
  } as const;

  return (
    <Card title="User directory">
      {canManageUsers ? (
        <UserDirectorySearch
          key={reloadToken}
          {...searchProps}
          renderActions={renderActions}
        />
      ) : (
        <UserDirectorySearch key={reloadToken} {...searchProps} />
      )}
      {actionError !== null ? (
        <p role="alert" className="muted">
          {actionError}
        </p>
      ) : null}
    </Card>
  );
}

const PLATFORM_ROLE_OPTIONS = DIRECTORY_PLATFORM_ROLES.filter(
  (role): role is PlatformRole => role !== "unknown",
);
const USER_STATUS_OPTIONS = DIRECTORY_STATUSES.filter(
  (status): status is UserStatus => status !== "unknown",
);

/** Alias retained for callers that name the surface after its admin route. */
export const AdminDirectory = AdminUserDirectory;
