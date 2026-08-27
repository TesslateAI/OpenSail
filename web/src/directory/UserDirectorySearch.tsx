import {
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  type FormEvent,
  type ReactNode,
} from "react";
import {
  type DirectorySearchFn,
  type DirectoryStatus,
  type DirectoryUserDto,
  type Uuid,
} from "../api/directory.ts";
import { Badge } from "../ui/primitives.tsx";

export type UserDirectorySearchProps = {
  /** Search implementation supplied by the owning capability/API surface. */
  search: DirectorySearchFn;
  /** Called when an active result is selected. */
  onSelect?: ((user: DirectoryUserDto) => void) | undefined;
  /** Called for both a result selection and an explicit selection clear. */
  onSelectionChange?: ((user: DirectoryUserDto | null) => void) | undefined;
  /** Controlled selection identity; the identity remains internal to the component. */
  selectedUserId?: Uuid | null | undefined;
  /** Disables the query and selection controls without changing the results. */
  disabled?: boolean | undefined;
  /** Visible field label and accessible section name. */
  label?: string | undefined;
  placeholder?: string | undefined;
  /** `0` permits an initial/list-all query; name searches normally use `1`. */
  minQueryLength?: number | undefined;
  initialQuery?: string | undefined;
  /** Admin directories can list users on first mount; invite pickers usually wait. */
  searchOnMount?: boolean | undefined;
  /** Adds the platform-role column to the result table. */
  showPlatformRole?: boolean | undefined;
  /** Optional owner-supplied actions rendered after the status columns. */
  renderActions?: ((user: DirectoryUserDto) => ReactNode) | undefined;
  emptyMessage?: string | undefined;
};

function errorMessage(reason: unknown): string {
  return reason instanceof Error ? reason.message : "request failed";
}

function displayName(user: DirectoryUserDto): string {
  const name = user.displayName?.trim() ?? "";
  if (name.length > 0) return name;
  const username = user.username?.trim() ?? "";
  return username.length > 0 ? username : "Unnamed user";
}

function username(user: DirectoryUserDto): string {
  const value = user.username?.trim() ?? "";
  return value.length > 0 ? value : "—";
}

function email(user: DirectoryUserDto): string {
  const value = user.email?.trim() ?? "";
  return value.length > 0 ? value : "—";
}

function statusTone(status: DirectoryStatus): "ok" | "fail" | "warn" {
  if (status === "active") return "ok";
  if (status === "disabled") return "fail";
  return "warn";
}

function statusLabel(status: DirectoryStatus): string {
  return status === "unknown" ? "status unavailable" : status;
}

/**
 * Accessible search/table surface shared by platform-admin and Project
 * membership screens. It never renders a UUID or provider subject; result
 * actions receive the internal identity through typed callbacks only.
 */
export function UserDirectorySearch({
  search,
  onSelect,
  onSelectionChange,
  selectedUserId,
  disabled = false,
  label = "Search users",
  placeholder = "Search by username, display name, or email",
  minQueryLength = 1,
  initialQuery = "",
  searchOnMount = false,
  showPlatformRole = false,
  renderActions,
  emptyMessage = "No users match that search.",
}: UserDirectorySearchProps) {
  const [query, setQuery] = useState(initialQuery);
  const [results, setResults] = useState<DirectoryUserDto[]>([]);
  const [searched, setSearched] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [internalSelectedId, setInternalSelectedId] = useState<Uuid | null>(null);
  const requestRef = useRef<AbortController | null>(null);
  const inputId = `directory-search-${useId().replaceAll(":", "")}`;
  const selectionId = selectedUserId === undefined ? internalSelectedId : selectedUserId;

  const executeSearch = useCallback(
    async (rawQuery: string): Promise<void> => {
      const trimmed = rawQuery.trim();
      if (trimmed.length < minQueryLength) {
        requestRef.current?.abort();
        setResults([]);
        setSearched(false);
        setError(
          minQueryLength === 0
            ? null
            : `Enter at least ${minQueryLength} character${minQueryLength === 1 ? "" : "s"}.`,
        );
        return;
      }

      requestRef.current?.abort();
      const controller = new AbortController();
      requestRef.current = controller;
      setLoading(true);
      setError(null);
      try {
        const nextResults = await search(trimmed, controller.signal);
        if (controller.signal.aborted) return;
        setResults([...nextResults]);
        setSearched(true);
      } catch (reason: unknown) {
        if (controller.signal.aborted) return;
        setResults([]);
        setSearched(true);
        setError(errorMessage(reason));
      } finally {
        if (!controller.signal.aborted) setLoading(false);
      }
    },
    [minQueryLength, search],
  );

  useEffect(() => {
    if (searchOnMount) void executeSearch(initialQuery);
    return () => requestRef.current?.abort();
  }, [executeSearch, initialQuery, searchOnMount]);

  const clearSelection = useCallback((): void => {
    if (selectedUserId === undefined) setInternalSelectedId(null);
    onSelectionChange?.(null);
  }, [onSelectionChange, selectedUserId]);

  const handleQueryChange = useCallback(
    (value: string): void => {
      setQuery(value);
      setResults([]);
      setSearched(false);
      setError(null);
      clearSelection();
    },
    [clearSelection],
  );

  const handleSubmit = useCallback(
    (event: FormEvent<HTMLFormElement>): void => {
      event.preventDefault();
      void executeSearch(query);
    },
    [executeSearch, query],
  );

  const selectUser = useCallback(
    (user: DirectoryUserDto): void => {
      if (disabled || user.status === "disabled") return;
      if (selectedUserId === undefined) setInternalSelectedId(user.userId);
      onSelect?.(user);
      onSelectionChange?.(user);
    },
    [disabled, onSelect, onSelectionChange, selectedUserId],
  );

  const hasActions = renderActions !== undefined;

  return (
    <section className="directory-search" aria-label={label}>
      <form className="stack stack-tight" onSubmit={handleSubmit}>
        <label htmlFor={inputId}>{label}</label>
        <div className="row">
          <input
            id={inputId}
            type="search"
            value={query}
            placeholder={placeholder}
            disabled={disabled || loading}
            onChange={(event) => handleQueryChange(event.target.value)}
          />
          <button
            type="submit"
            className="btn btn-primary"
            disabled={disabled || loading || query.trim().length < minQueryLength}
          >
            {loading ? "Searching…" : "Search"}
          </button>
          {query.length > 0 ? (
            <button
              type="button"
              className="btn"
              disabled={disabled || loading}
              onClick={() => handleQueryChange("")}
            >
              Clear
            </button>
          ) : null}
        </div>
      </form>

      {error !== null ? (
        <p role="alert" className="muted">
          {error}
        </p>
      ) : null}
      {loading ? <p className="muted" role="status">Searching directory…</p> : null}
      {!loading && searched && results.length === 0 && error === null ? (
        <p className="muted" role="status">
          {emptyMessage}
        </p>
      ) : null}

      {results.length > 0 ? (
        <table className="table directory-results">
          <thead>
            <tr>
              <th scope="col">Name</th>
              <th scope="col">Username</th>
              <th scope="col">Email</th>
              <th scope="col">Status</th>
              {showPlatformRole ? <th scope="col">Platform role</th> : null}
              {hasActions ? <th scope="col">Actions</th> : null}
            </tr>
          </thead>
          <tbody>
            {results.map((user) => {
              const isDisabled = user.status === "disabled";
              const isSelected = selectionId === user.userId;
              const labelText = displayName(user);
              return (
                <tr
                  key={user.userId}
                  className={`${isSelected ? "session-row-active " : ""}${isDisabled ? "directory-row-disabled" : ""}`}
                >
                  <td>
                    <button
                      type="button"
                      className="btn"
                      disabled={disabled || isDisabled}
                      aria-disabled={isDisabled || disabled}
                      title={isDisabled ? "Disabled users cannot be selected" : undefined}
                      onClick={() => selectUser(user)}
                    >
                      {labelText}
                    </button>
                    {isDisabled ? <span className="muted"> (disabled)</span> : null}
                  </td>
                  <td className="mono">{username(user)}</td>
                  <td>{email(user)}</td>
                  <td>
                    <Badge tone={statusTone(user.status)}>{statusLabel(user.status)}</Badge>
                  </td>
                  {showPlatformRole ? (
                    <td>
                      <Badge tone={user.platformRole === "admin" ? "accent" : "neutral"}>
                        {user.platformRole}
                      </Badge>
                    </td>
                  ) : null}
                  {hasActions ? <td>{renderActions(user)}</td> : null}
                </tr>
              );
            })}
          </tbody>
        </table>
      ) : null}
    </section>
  );
}

/** Short alias for consumers that use the directory feature name. */
export const DirectorySearch = UserDirectorySearch;
