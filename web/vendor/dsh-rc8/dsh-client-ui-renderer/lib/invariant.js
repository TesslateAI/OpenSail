//#region lib/types/invariant.js
/**
* Package-owned invariant companion for `@deepseek-ai/dsh-client-ui-renderer`.
* @module @deepseek-ai/dsh-client-ui-renderer/invariant
*/
const PACKAGE_NAME = "@deepseek-ai/dsh-client-ui-renderer";
/** Cordis companion plugin name. */
const name = "client-ui-renderer-invariant";
/** Service required before the companion can reserve package ownership. */
const inject = ["invariants"];
/**
* No runtime invariant: the package installs the render adapter and provides a
* mount callback but owns no event stream or mutable cross-plugin data relation.
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
