/**
 * Human-readable user directory surfaces.
 *
 * These components are intentionally unmounted by default. A page supplies
 * the server capability and the DirectoryApi seam when it owns the relevant
 * admin or Project route.
 */

export { AdminDirectory, AdminUserDirectory } from "./AdminUserDirectory.tsx";
export type { AdminUserDirectoryProps } from "./AdminUserDirectory.tsx";
export {
  DirectorySearch,
  UserDirectorySearch,
} from "./UserDirectorySearch.tsx";
export type { UserDirectorySearchProps } from "./UserDirectorySearch.tsx";
