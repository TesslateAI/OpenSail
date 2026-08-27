//#region lib/types/invariant.js
/**
* Package-owned invariant companion for `@deepseek-ai/dsh-typert-registry`.
* @module @deepseek-ai/dsh-typert-registry/invariant
*/
const PACKAGE_NAME = "@deepseek-ai/dsh-typert-registry";
/** Cordis companion plugin name. */
const name = "typert-registry-invariant";
/** Service required before the companion can reserve package ownership. */
const inject = ["invariants"];
/**
* No runtime invariant: schema and package-reflection records mutate together
* inside register/dispose, with no independent event or second data source to
* cross-check; duplicate identities fail at the owning operation boundary.
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
