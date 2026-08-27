/**
 * `connection-voie` — the same-origin DSH connection plugin for the VOIE
 * control plane.
 *
 * This module is the compose entrypoint that replaces the stock
 * `@deepseek-ai/dsh-client-connection` bundle. It provides the DSH
 * `ctx.connection` handle backed by the canonical `VoieCarrier`.
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
