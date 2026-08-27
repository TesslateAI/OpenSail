/**
 * `connection-voie` plugin entrypoint for the DSH rc.8 graph composer.
 *
 * Bundle the graph from this file (compose entry) in place of the stock
 * `@deepseek-ai/dsh-client-connection` plugin: it provides `ctx.connection`
 * backed by the canonical same-origin `VoieCarrier`.
 */
export { VoieCarrier, type VoieCarrierOptions } from "../carrier/voie.ts";
export { createConnectionHandle, createCarrierApi } from "./api.ts";
export { inject, apply } from "./plugin-fn.ts";
export type {
  AgentRow,
  Baseline,
  CanonicalEvent,
  Mutation,
  MutationResult,
  PollResult,
  SessionRow,
  SessionId,
  WorkspaceRow,
  VoieCarrierFace,
} from "../carrier/types.ts";
