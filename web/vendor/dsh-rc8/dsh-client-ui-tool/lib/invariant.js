//#region lib/types/invariant.js
/**
* Package-owned invariant companion for `@deepseek-ai/dsh-client-ui-tool`.
* @module @deepseek-ai/dsh-client-ui-tool/invariant
*/
const PACKAGE_NAME = "@deepseek-ai/dsh-client-ui-tool";
/** Cordis companion plugin name. */
const name = "client-ui-tool-invariant";
/** Service required before the companion can reserve package ownership. */
const inject = ["invariants"];
/**
* No runtime invariant: Tool composition is browser-only and contributes no
* events or cross-plugin mutable state; slot ownership is checked by ui-slots.
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
