/**
 * Admin management panels, exported as a single mount surface.
 *
 * The carrier/shell integration mounts these when the platform-admin entry
 * exists; until then they compile and remain unmounted. Every panel talks
 * only to its admin adapter (`api/admin.ts`, `api/console.ts`) and the shared
 * UI primitives.
 */

export { AdminUsers } from "./Users.tsx";
export { AdminUsersPage } from "./AdminUsersPage.tsx";
export { TeamMembersPage } from "./TeamMembersPage.tsx";
export { AccountPage } from "./AccountPage.tsx";
export { AdminScopesTeams } from "./ScopesTeams.tsx";
export { AdminFabricsUnderlay } from "./FabricsUnderlay.tsx";
export { AdminSystemAudit } from "./SystemAudit.tsx";
export { AdminAuth } from "./Auth.tsx";
export { AdminControlHealth } from "./ControlHealth.tsx";
