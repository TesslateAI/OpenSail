/**
 * Resolve a workspace-relative path into the Host-facing spelling used by openPath.
 * @param cwd - session workspace root, when known.
 * @param path - absolute or workspace-relative path.
 * @returns an absolute path when a workspace root is available, otherwise the original path.
 */
export declare function resolveWorkspacePath(cwd: string | undefined, path: string): string;
/**
 * Display-only POSIX home abbreviation. Windows drive and UNC paths stay
 * verbatim, including when `home` itself is a Windows path. A missing, empty,
 * or filesystem-root `home` leaves `path` unchanged so `/` cannot become `~`.
 * @param path - absolute or already-short display path.
 * @param home - host account home from `host.describe`; absent skips abbreviation.
 * @returns `~` or `~/…` for the POSIX home and its descendants, otherwise `path`.
 */
export declare function abbreviateHomePath(path: string, home?: string): string;
//# sourceMappingURL=path.d.ts.map