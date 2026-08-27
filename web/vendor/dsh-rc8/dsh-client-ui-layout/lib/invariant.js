//#region lib/types/invariant.js
/**
* Package-owned invariant companion for `@deepseek-ai/dsh-client-ui-layout`.
* @module @deepseek-ai/dsh-client-ui-layout/invariant
*/
const PACKAGE_NAME = "@deepseek-ai/dsh-client-ui-layout";
/** Cordis companion plugin name. */
const name = "client-ui-layout-invariant";
/** Service required before the companion can reserve package ownership. */
const inject = ["invariants"];
/**
* No runtime invariant: the shell viewing-state store behind ctx.layout emits
* no cordis events; clamp/prune/concession-chain
* sequencing is asserted directly by this package's columns and service specs.
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
