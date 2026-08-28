/**
 * Native VOIE carrier contract: the canonical identity surface shared by the
 * same-origin `connection-voie` carrier and its DSH runtime consumer.
 *
 * Canonical identity mapping (never synthesized):
 * - `globalSeq`  — the store's sole global append sequence
 *   (`session_events.global_seq`), monotonic across every session. The event
 *   feed cursor is the last observed `globalSeq`.
 * - `revision`   — per-session append revision (`head_revision + 1` per
 *   durable append).
 * - `eventIndex` — line index inside one canonical append batch (`bytes`).
 * - `seq`/`time` — producer-authored fields carried verbatim inside each log
 *   line. The carrier never computes, reorders, or backfills them, and it
 *   never fabricates lifecycle or order frames from projected blocks.
 *
 * Every endpoint the carrier calls is a VOIE control-plane resource over a
 * same-origin opaque session cookie (`voie_session`). No Whaled bearer gate,
 * no DSH provider, no credentials, no separate web process.
 */

export type SessionId = string;
export type ProjectId = string;
export type AgentId = string;
export type WorkspaceId = string;
export type RunId = string;
export type IntentId = string;
export type FabricId = string;

/** ISO-8601 timestamp as emitted by the control plane (`created_at::text`). */
export type Iso8601 = string;

/**
 * One canonical session-log event as decoded from the event feed: the
 * store's reference envelope plus the producer's log line, verbatim.
 *
 * `seq`/`time` are `null` only when the durable line genuinely omits them;
 * they are never derived from position or from any other event.
 */
export type CanonicalEvent = {
  /** Owning session (the batch's `sessionId`). */
  sessionId: SessionId;
  /** Log vocabulary (`user/message`, `assistant/message`, ...). */
  type: string;
  /** Producer-authored payload, verbatim. */
  data: unknown;
  /** Global append sequence; the feed cursor axis. */
  globalSeq: number;
  /** Per-session append revision. */
  revision: number;
  /** Line index inside the append batch. */
  eventIndex: number;
  /** Batch identity derived by the store (stable across retries). */
  appendId: string | null;
  /** Blob provenance (immutable object key). */
  objectKey: string | null;
  /** Blob provenance (SHA-256 of the batch bytes, hex). */
  contentHash: string | null;
  /** Blob provenance (batch byte length). */
  byteLength: number | null;
  /** Producer-authored sequence within the session log; null when absent. */
  seq: number | null;
  /** Producer-authored Unix epoch ms; null when absent. */
  time: number | null;
  /** Producer-authored surface placement, verbatim; undefined when absent. */
  surfaceOp: unknown;
  /** Producer-authored source-event citations, verbatim; undefined when absent. */
  sourceEventSeqs: unknown;
};

/**
 * One session row from the authoritative `/api/sessions` resource. Display
 * titles come from the first run prompt when present; the DSH runtime may
 * also derive a title from `session/title` events. Neither path synthesizes
 * a title from the session id.
 */
export type SessionRow = {
  id: SessionId;
  projectId: ProjectId;
  agentId: AgentId;
  workspaceId: WorkspaceId;
  /** True while the session holds an accepted or dispatched run. */
  running: boolean;
  /** Current durable log head (`head_revision`); 0 = empty log. */
  headRevision: number;
  writerGeneration: number | null;
  attentionGeneration: number | null;
  createdAt: Iso8601 | null;
  /** First-run prompt, bounded; null when the session has no runs. */
  title?: string | null;
};

/** One agent row from `/api/agents`. */
export type AgentRow = {
  id: AgentId;
  projectId: ProjectId;
  name: string;
  model: string | null;
  systemPrompt: string | null;
  bashEnabled: boolean;
  maxTokens: number | null;
};

/** One workspace row from `/api/workspaces`. */
export type WorkspaceRow = {
  id: WorkspaceId;
  projectId: ProjectId;
  fabricId: FabricId | null;
  fabricName: string | null;
  state: string | null;
  execGeneration: number | null;
  createdAt: Iso8601 | null;
};

/**
 * The authoritative baseline: the session list, the workspace/agent rows the
 * sessions reference, and the global event-feed cursor. One GET per resource;
 * the cursor is the last `globalSeq` the feed reported.
 */
export type Baseline = {
  cursor: string;
  sessions: SessionRow[];
  agents: AgentRow[];
  workspaces: WorkspaceRow[];
};

/**
 * One durable run row from the conversation-scoped runs resource
 * (`/api/conversations/:id/runs`). Membership-scoped server-side; ordered by
 * the control plane's per-session `seq`. The browser queue seat projects
 * these rows directly: `accepted` renders as a queued row whose cancel rides
 * `POST /api/runs/:id/cancel`, `dispatched` is the active turn, and any
 * other label is terminal — the row has left both seats.
 */
export type RunRow = {
  runId: RunId;
  /** Durable per-session run sequence; the queue order axis. */
  seq: number;
  state: string;
  /** Producer-stored prompt text; null when the row omits it. */
  prompt: string | null;
  /** Owning user id; null when the row omits it. */
  actorUserId: string | null;
};

/** One bounded long-poll cycle over the global event feed. */
export type PollResult =
  | { kind: "events"; cursor: string; events: readonly CanonicalEvent[] }
  /** The server refused the cursor: re-read the baseline, then re-poll. */
  | { kind: "stale" };

/**
 * One user action, keyed by a caller-minted intent id (one UUID per action).
 * The browser in-flight set is the local fence; the server's run identity and
 * request-hash dedup is the durable fence, so a retry of the identical
 * payload is idempotent and a changed payload under the same intent is a
 * conflict.
 */
export type Mutation =
  | {
      op: "conversation.create";
      intentId: IntentId;
      /** Caller-minted conversation (session) identity. */
      conversationId: SessionId;
      projectId: ProjectId;
      /** The conversation's agent; omitted when the surface defers to the
       *  control plane default. An absent key never serializes. */
      agentId?: AgentId | undefined;
      workspaceId: WorkspaceId;
      prompt: string;
    }
  | {
      op: "conversation.message";
      intentId: IntentId;
      conversationId: SessionId;
      prompt: string;
    }
  | {
      op: "conversation.cancel";
      intentId: IntentId;
      conversationId: SessionId;
    };

/** Acceptance of one mutation, or the business refusal. Every key is always
 *  present; an absent fact is `undefined`, never an omitted property. */
export type MutationResult = {
  accepted: boolean;
  /** Business reason when refused (or an HTTP-level failure). */
  reason: string | undefined;
  conversationId: SessionId | undefined;
  runId: RunId | undefined;
  /** Server run-state label at acceptance/refusal. */
  state: string | undefined;
  /** Retained terminal result on an idempotent replay. */
  result: unknown;
};

/** The carrier seam: authoritative baseline, bounded poll, history, mutations. */
export interface VoieCarrierFace {
  loadBaseline(signal?: AbortSignal): Promise<Baseline>;
  poll(cursor: string, signal?: AbortSignal): Promise<PollResult>;
  /** Per-session canonical log, oldest first, cursor 0. */
  loadHistory(sessionId: SessionId, signal?: AbortSignal): Promise<CanonicalEvent[]>;
  mutate(mutation: Mutation, signal?: AbortSignal): Promise<MutationResult>;
}
