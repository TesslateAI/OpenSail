//#region lib/types/invariant.js
/**
* Package-owned invariant companion for `@deepseek-ai/dsh-client-ui-trajectory`.
* @module @deepseek-ai/dsh-client-ui-trajectory/invariant
*/
const PACKAGE_NAME = "@deepseek-ai/dsh-client-ui-trajectory";
/** Cordis companion plugin name. */
const name = "client-ui-trajectory-invariant";
/** Service required before the companion can reserve package ownership. */
const inject = ["invariants"];
/**
* No runtime invariant: a pure-consumer plugin — it emits no cordis events
* and owns no mutable cross-plugin state; its view-slot registration is a
* plain effect whose disposal the slot ledger's own specs and this
* package's behavior specs observe directly.
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
