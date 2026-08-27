/**
 * Shared browser platform modules. Seeding, bundling externals, and Vite
 * aliases consume this list so their module identities cannot drift.
 * @module @deepseek-ai/dsh-client-web/src/platform
 */
/** The module specifiers the shell shares into the frozen module table. */
export declare const PLATFORM_MODULES: readonly ["react", "react/jsx-runtime", "react-dom", "react-dom/client", "@deepseek-ai/cordis", "@deepseek-ai/dsh-client-ui-slots", "@deepseek-ai/dsh-client-ui-primitives"];
/** Client-bundle specifiers whose factories the parser preloads before the shell starts. */
export declare const PRELOADED_CLIENT_EXTERNALS: readonly ["@deepseek-ai/dsh-client-runtime/client"];
/** One platform module specifier (a seed-table key). */
export type PlatformModule = (typeof PLATFORM_MODULES)[number];
//# sourceMappingURL=platform.d.ts.map