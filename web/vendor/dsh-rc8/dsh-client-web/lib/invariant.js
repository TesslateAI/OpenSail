//#region lib/types/invariant.js
/**
* Package-owned invariant companion for `@deepseek-ai/dsh-client-web`.
* @module @deepseek-ai/dsh-client-web/invariant
*/
const PACKAGE_NAME = "@deepseek-ai/dsh-client-web";
/** Cordis companion plugin name. */
const name = "client-web-invariant";
/** Service required before the companion can reserve package ownership. */
const inject = ["invariants"];
/**
* No runtime invariant: the vite entry shell — boot glue and module-table
* seeding with no cordis events and no cross-plugin mutable state; the boot
* chain (loading page → settled → one-flip UI) is asserted by the web smoke
* e2e against the real carrier.
*/
const install = () => {};
/**
* Register this package's invariant companion.
* @param ctx - Cordis context carrying the invariant service.
* @returns the installed registration's disposer after setup succeeds.
*/
const apply = (ctx) => Promise.resolve(ctx.invariants.register(PACKAGE_NAME, install));
//#endregion
export { apply, inject, name };

//# sourceMappingURL=invariant.js.map