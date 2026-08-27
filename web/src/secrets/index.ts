/**
 * Secret vault surface, exported as a single mount point.
 *
 * The shell integration mounts this inside a scope context (personal or
 * team); until then it compiles and remains unmounted. The surface talks
 * only to the secrets adapter (`api/secrets.ts`) and shared UI primitives,
 * and never touches secret values outside request bodies.
 */

export { SecretVault } from "./SecretVault.tsx";
