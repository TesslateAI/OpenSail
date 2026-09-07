/**
 * The DSH `ConnectionHandle` face over the canonical VOIE carrier.
 *
 * `api` mirrors the DSH IApiClient surface the pinned rc.8 runtime consumes:
 * sessions (list/search/create/history/prompt/cancel/rename/fork/attachment/
 * updateQueue/models/selectModel), host (describe/pickDirectory/listDirectory/
 * createDirectory/openPath), workspace (list/create/rename/delete/insertBefore/
 * insertSessionBefore/archiveSession), events (mux/host streams), plus the
 * domains the VOIE surface does not serve (subagents, skills, agentPresets,
 * goals, settings, credentials, llm) answered with the empty/read-only truth
 * so the stock bundles never see a missing service.
 *
 * Every response rides the `{ rpcId, result: { ok, value | error } }` shape
 * the runtime validates. Event identity is canonical: a `session/event`
 * frame carries the producer's own `{type, seq, time, data}` envelope plus
 * optional `surfaceOp`/`sourceEventSeqs`, with `globalSeq`/`revision`
 * preserved on the carrier event; frames whose durable line lacks `seq`/`time`
 * are dropped (fail-closed, never fabricated).
 */
import type {
  AgentRow,
  Baseline,
  CanonicalEvent,
  HistoryPage,
  Mutation,
  MutationResult,
  RunRow,
  SessionId,
  SessionRow,
  WorkspaceRow,
} from "../carrier/types.ts";
import { VoieCarrier, type VoieCarrierOptions } from "../carrier/voie.ts";
import { arrayAt, asBoolOr, asNum, asStr, isRecord } from "../api/validate.ts";
import { getVoieDshHostContext } from "./host-context.ts";

type RpcId = string;
type RpcResult<T> = { ok: true; value: T } | { ok: false; error: { code: string; message: string; details: unknown } };
type RpcResponse<T> = { rpcId: RpcId; result: RpcResult<T> };

type RpcRequest<P> = { rpcId: RpcId; payload: P };

type SessionEventEnvelope = {
  type: string;
  seq: number;
  time: number;
  data: unknown;
  surfaceOp?: unknown;
  sourceEventSeqs?: unknown;
};

type ToolEventView =
  | { for: "call"; view: { card: string; title: string; cwd?: string } }
  | { for: "result"; view: { card: string; output?: string; exitCode?: number } };

type SessionEventFrame = {
  type: "session/event";
  sessionId: SessionId;
  event: SessionEventEnvelope;
  view?: ToolEventView;
};

/** One queued seat row as the vendored dock consumes it: keyed by runId. */
type QueueItemFrame = {
  id: string;
  placement: "queued";
  message: {
    id: string;
    role: "user";
    content: Array<{ type: "text"; text: string }>;
    source: { kind: string };
  };
};

/** The pending/queue seat projection; wholesale-replaces the dock rows. */
type QueueFrame = {
  type: "session/queue";
  sessionId: SessionId;
  items: QueueItemFrame[];
};

type MuxFrame = SessionEventFrame | QueueFrame;
type HostWorkspaceView = {
  workspaceId: string;
  path: string;
  title: string;
  sessionIds: SessionId[];
  createdAt: string;
  updatedAt: string;
};

type HostFrame =
  | { type: "host/session-status"; sessionId: SessionId; running: boolean }
  | { type: "host/agent-error"; sessionId: SessionId; message: string }
  | { type: "host/workspace-changed"; workspace: HostWorkspaceView };

type HistoryEntry = { event: SessionEventEnvelope; view?: ToolEventView };
type ProjectionsBlock = { asOfSeq: number; values: Record<string, unknown> };
type HistoryValue = {
  events: HistoryEntry[];
  hasMore: boolean;
  projections?: ProjectionsBlock;
};

type HistoryPageItem = { event: { type: string; seq: number } };

/**
 * DSH `session.history` pages on message boundaries, not raw event count.
 * Count `user/message` and `assistant/message` backwards from `beforeSeq`
 * (or the tail), then cut at the `turn/start` that closes that window.
 * `hasMore` is true when older events remain; the runtime uses that to
 * prepend. Slicing the last N events drops the user turn and most of a
 * tool-heavy log, then lying with `hasMore: false` makes it unrecoverable.
 */
export function pageSessionHistory<T extends HistoryPageItem>(
  entries: readonly T[],
  beforeSeq: number | undefined,
  maxMessages: number,
): { events: T[]; hasMore: boolean } {
  const limit = Math.max(1, Math.floor(maxMessages));
  let end = entries.length;
  if (beforeSeq !== undefined) {
    const cut = entries.findIndex((entry) => entry.event.seq >= beforeSeq);
    end = cut === -1 ? entries.length : cut;
  }
  let start = 0;
  let messages = 0;
  for (let i = end - 1; i >= 0; i--) {
    const type = entries[i]?.event.type;
    if (type === "user/message" || type === "assistant/message") messages += 1;
    if (type === "turn/start" && messages >= limit) {
      start = i;
      break;
    }
  }
  return { events: entries.slice(start, end), hasMore: start > 0 };
}

type SessionSummary = {
  sessionId: SessionId;
  updatedAt: number;
  running: boolean;
  blank: boolean;
  cwd?: string;
  projections?: ProjectionsBlock;
};

type WorkspaceView = {
  workspaceId: string;
  path: string;
  title: string;
  sessionIds: SessionId[];
  createdAt: string;
  updatedAt: string;
  state?: string | null;
};

type ConnectionSinks = {
  onMuxEnvelope?: (envelope: RpcRequest<MuxFrame>) => void;
  onHostEnvelope?: (envelope: RpcRequest<HostFrame>) => void;
  onConnected?: (description: unknown) => void;
  onStateChange?: (state: "connected" | "reconnecting") => void;
};

function rpcId(): RpcId {
  return crypto.randomUUID();
}

function ok<T>(value: T): RpcResponse<T> {
  return { rpcId: rpcId(), result: { ok: true, value } };
}

function fail(code: string, message: string, details: unknown = {}): RpcResponse<never> {
  return { rpcId: rpcId(), result: { ok: false, error: { code, message, details } } };
}

/** Extracts text from DSH prompt content parts (text-only under VOIE). */
function promptTextOf(payload: unknown): string {
  const record = isRecord(payload) ? payload : {};
  const content = record["content"];
  if (!Array.isArray(content)) return "";
  return content
    .flatMap((part) => {
      const item = isRecord(part) ? part : null;
      if (item === null || item["type"] !== "text" || typeof item["text"] !== "string") return [];
      return [item["text"]];
    })
    .join("\n");
}

function sessionIdOf(payload: unknown): SessionId {
  const record = isRecord(payload) ? payload : {};
  const value = record["sessionId"];
  return typeof value === "string" ? value : "";
}

function intentIdOf(payload: unknown): string {
  const record = isRecord(payload) ? payload : {};
  const value = record["intentId"];
  return typeof value === "string" && value !== "" ? value : crypto.randomUUID();
}

function numberAt(payload: unknown, key: string): number | undefined {
  const record = isRecord(payload) ? payload : {};
  return asNum(record[key]) ?? undefined;
}

function stringAt(payload: unknown, key: string): string | undefined {
  const record = isRecord(payload) ? payload : {};
  const value = record[key];
  return typeof value === "string" ? value : undefined;
}

/**
 * Maps one canonical event to the DSH `session/event` envelope. The producer's
 * own `seq`/`time`/`surfaceOp`/`sourceEventSeqs` are carried verbatim; a
 * durable line without numeric `seq`/`time` cannot be placed on the DSH
 * surface, so it is dropped (never guessed).
 */
function eventEnvelopeOf(event: CanonicalEvent): SessionEventEnvelope | null {
  if (event.seq === null || event.time === null) return null;
  const envelope: SessionEventEnvelope = { type: event.type, seq: event.seq, time: event.time, data: event.data };
  if (event.surfaceOp !== undefined) envelope.surfaceOp = event.surfaceOp;
  if (event.sourceEventSeqs !== undefined) envelope.sourceEventSeqs = event.sourceEventSeqs;
  return envelope;
}

/** Bash tool render intent derived from canonical tool events, when present. */
function toolViewOf(event: CanonicalEvent): ToolEventView | undefined {
  if (!isRecord(event.data)) return undefined;
  if (event.type === "tool/call") {
    if (event.data["name"] !== "bash") return undefined;
    const argumentsRaw = typeof event.data["arguments"] === "string" ? event.data["arguments"] : "";
    let parsed: unknown = null;
    try {
      parsed = JSON.parse(argumentsRaw) as unknown;
    } catch {
      parsed = null;
    }
    const args = isRecord(parsed) ? parsed : {};
    const title = typeof args["command"] === "string" ? args["command"] : argumentsRaw;
    const cwd = typeof args["workdir"] === "string" ? args["workdir"] : undefined;
    const view: { card: "terminal"; title: string; cwd?: string } = { card: "terminal", title };
    if (cwd !== undefined) view.cwd = cwd;
    return { for: "call", view };
  }
  if (event.type === "tool/result") {
    const message = isRecord(event.data["message"]) ? event.data["message"] : null;
    const content = message === null ? undefined : message["content"];
    const first = Array.isArray(content) ? content[0] : undefined;
    const item = isRecord(first) ? first : null;
    if (item === null || item["type"] !== "tool-result") return undefined;
    const body = Array.isArray(item["content"]) ? item["content"] : [];
    const output = body
      .flatMap((part) => {
        const p = isRecord(part) ? part : null;
        if (p === null || p["type"] !== "text" || typeof p["text"] !== "string") return [];
        return [p["text"]];
      })
      .join("\n");
    return { for: "result", view: { card: "terminal", output, exitCode: item["isError"] === true ? 1 : 0 } };
  }
  return undefined;
}

/** Sorts canonical events into durable order (oldest first, by store sequence). */
function inDurableOrder(events: readonly CanonicalEvent[]): CanonicalEvent[] {
  return [...events].sort((a, b) => a.globalSeq - b.globalSeq || a.eventIndex - b.eventIndex);
}

/**
 * Builds the DSH IApiClient-shaped face over a carrier. The face is the sole
 * consumer of the carrier seam; every read projects the canonical baseline,
 * every write rides a single-attempt mutation.
 */
let syncWorkspaces: (signal?: AbortSignal) => Promise<void> = async () => {};

/** Refresh control Workspace rows into the live DSH list before New Chat. */
export function syncVoieWorkspaces(signal?: AbortSignal): Promise<void> {
  return syncWorkspaces(signal);
}

export function createCarrierApi(
  carrier: {
    loadBaseline(signal?: AbortSignal): Promise<Baseline>;
    poll(cursor: string, signal?: AbortSignal): Promise<{ kind: "events"; cursor: string; events: readonly CanonicalEvent[] } | { kind: "stale" }>;
    loadHistory(
      sessionId: SessionId,
      signal?: AbortSignal,
      page?: { beforeSeq?: number; maxMessages?: number },
    ): Promise<HistoryPage>;
    mutate(mutation: Mutation, signal?: AbortSignal): Promise<MutationResult>;
  },
  net: { fetchImpl?: typeof fetch } = {},
) {
  let baselinePromise: Promise<Baseline> | null = null;
  const baseline = (signal?: AbortSignal): Promise<Baseline> => (baselinePromise ??= carrier.loadBaseline(signal));
  const refreshBaseline = (signal?: AbortSignal): Promise<Baseline> => {
    baselinePromise = carrier.loadBaseline(signal);
    return baselinePromise;
  };

  const updatedAtOf = (session: SessionRow): number => {
    const parsed = Date.parse(session.createdAt ?? "");
    return Number.isFinite(parsed) ? parsed : 0;
  };

  function notifySessionsChanged(): void {
    if (typeof document === "undefined") return;
    document.dispatchEvent(new Event("voie-sessions-changed"));
  }

  // ---- durable run truth: the runs resource is the sole queue source ----

  /** Authorized same-origin reader for the conversation-scoped runs resource. */
  const runsFetchImpl = net.fetchImpl ?? globalThis.fetch.bind(globalThis);

  /**
   * Decodes one raw runs payload; rows missing any required key are dropped
   * fail-closed (a half-known row is never projected).
   */
  function decodeRunsOf(body: unknown): RunRow[] {
    const items = isRecord(body) ? arrayAt(body, "runs") : [];
    const rows: RunRow[] = [];
    for (const raw of items) {
      if (!isRecord(raw)) continue;
      const runId = asStr(raw.runId);
      const seq = asNum(raw.seq);
      const state = asStr(raw.state);
      if (runId === null || seq === null || !Number.isInteger(seq) || state === null) continue;
      rows.push({ runId, seq, state, prompt: asStr(raw.prompt), actorUserId: asStr(raw.actorUserId) });
    }
    return rows;
  }

  /**
   * The durable queue seat for one conversation: GET /api/conversations/:id/runs
   * with the session cookie (server-side membership scoping bounds the
   * answer). Projection input only — no seat is ever synthesized from a
   * mutation result.
   */
  async function loadConversationRuns(sessionId: SessionId, signal?: AbortSignal): Promise<RunRow[]> {
    const response = await runsFetchImpl(
      `/api/conversations/${encodeURIComponent(sessionId)}/runs`,
      { method: "GET", headers: { accept: "application/json" }, credentials: "same-origin", ...(signal === undefined ? {} : { signal }) },
    );
    if (!response.ok) throw new Error(`GET /api/conversations/${sessionId}/runs failed: HTTP ${String(response.status)}`);
    return decodeRunsOf(await response.json());
  }

  /** Last emitted per-conversation projection signature (frame dedupe). */
  const queueSignatures = new Map<SessionId, string>();
  /** Last emitted active-seat flag per conversation. */
  const runningCache = new Map<SessionId, boolean>();
  /** Conversations whose projection is mid-flight; bursts coalesce. */
  const reconciling = new Set<SessionId>();
  /** Sessions that requested a projection while one was already in flight. */
  const pendingReconcile = new Set<SessionId>();
  /** Open/reopen must replay status even when the flag did not change. */
  const replayStatus = new Set<SessionId>();
  /** In-flight prompt admissions keyed by conversation. Same intentId may
   *  share the promise; a different prompt waits, then admits its own Run. */
  type InFlightPrompt = { intentId: string; work: Promise<MutationResult> };
  const inflightPrompt = new Map<SessionId, InFlightPrompt>();
  /** Live DSH mux/host sinks; tests that only read pump.buffer may leave these unset. */
  let muxSink: ConnectionSinks["onMuxEnvelope"];
  let hostSink: ConnectionSinks["onHostEnvelope"];

  function emitMux(frame: MuxFrame): void {
    pump.push(frame);
    muxSink?.({ rpcId: rpcId(), payload: frame });
  }

  function emitHost(frame: HostFrame): void {
    hostPump.push(frame);
    hostSink?.({ rpcId: rpcId(), payload: frame });
  }

  /** Static membership table: run states that still occupy a seat. */
  const LIVE_RUN_STATES: Record<string, true> = { accepted: true, dispatched: true };

  /** Sorts live runs into dispatch order: server seq ascending, id tiebreak. */
  function inQueueOrder(runs: readonly RunRow[]): RunRow[] {
    return [...runs].sort((a, b) => a.seq - b.seq || (a.runId < b.runId ? -1 : a.runId > b.runId ? 1 : 0));
  }

  function cancelAlreadySettled(state: string | undefined): boolean {
    return state === "unknown" || state === "cancelled" || state === "terminal";
  }

  async function cancelExactRun(runId: string, signal?: AbortSignal): Promise<{ accepted: boolean; state: string | undefined }> {
    const response = await runsFetchImpl(
      `/api/runs/${encodeURIComponent(runId)}/cancel`,
      {
        method: "POST",
        headers: { accept: "application/json", "x-voie-intent": "mutate", "content-type": "application/json" },
        credentials: "same-origin",
        ...(signal === undefined ? {} : { signal }),
      },
    );
    if (!response.ok) {
      throw new Error(`POST /api/runs/${runId}/cancel failed: HTTP ${String(response.status)}`);
    }
    const body: unknown = await response.json();
    const record = isRecord(body) ? body : {};
    return { accepted: asBoolOr(record.accepted, false), state: asStr(record.state) ?? undefined };
  }

  /**
   * Projects one conversation's current runs into the pending/queue seat:
   * every `accepted` run becomes a queued dock row keyed by its runId (the
   * row's cancel rides POST /api/runs/:id/cancel), a `dispatched` run is the
   * active turn (running=true), and terminal labels have already left both
   * seats. Emits only on change; an empty-but-changed seat emits an empty
   * `session/queue` frame so the dock clears. An unreadable resource keeps
   * the last good projection — never a guessed seat.
   */
  async function projectQueueSeat(
    sessionId: SessionId,
    signal?: AbortSignal,
    preloaded?: readonly RunRow[],
  ): Promise<void> {
    if (reconciling.has(sessionId)) {
      pendingReconcile.add(sessionId);
      return;
    }
    reconciling.add(sessionId);
    try {
      let runs: RunRow[];
      try {
        runs = preloaded !== undefined
          ? [...preloaded]
          : await loadConversationRuns(sessionId, signal);
      } catch {
        return;
      }
      const live = inQueueOrder(runs.filter((run) => LIVE_RUN_STATES[run.state] === true));
      const signature = JSON.stringify(live.map((run) => [run.runId, run.seq, run.state]));
      if (queueSignatures.get(sessionId) !== signature) {
        queueSignatures.set(sessionId, signature);
        emitMux({
          type: "session/queue",
          sessionId,
          items: live.filter((run) => run.state === "accepted").map((run): QueueItemFrame => ({
            id: run.runId,
            placement: "queued",
            message: {
              id: run.runId,
              role: "user",
              content: [{ type: "text", text: run.prompt ?? "" }],
              source: { kind: "voie-run" },
            },
          })),
        });
      }
      const running = runs.some((run) => LIVE_RUN_STATES[run.state] === true);
      const previous = runningCache.get(sessionId);
      runningCache.set(sessionId, running);
      const replay = replayStatus.delete(sessionId);
      if (replay || previous !== running) {
        emitHost({ type: "host/session-status", sessionId, running });
        // A Run can settle `unknown` with no session events (child death,
        // restart classify). Tell DSH the live turn ended; do not fire this
        // on first projection of an already-dead conversation.
        if (previous === true && running === false && runs.some((run) => run.state === "unknown")) {
          emitHost({
            type: "host/agent-error",
            sessionId,
            message: "The run ended without a result and will not be replayed.",
          });
        }
        if (previous !== running) {
          void refreshBaseline(signal).catch(() => {});
        }
      }
    } finally {
      reconciling.delete(sessionId);
      if (pendingReconcile.delete(sessionId)) {
        void projectQueueSeat(sessionId, signal);
      }
    }
  }

  /**
   * Reconciles queue seats from durable truth. Called after the baseline
   * lands, over each poll batch's touched conversations, and by consumers
   * needing an immediate sweep; identity comes solely from the runs
   * resource, so a reload reconstructs exactly this projection.
   */
  async function reconcileQueues(requested?: ReadonlySet<SessionId>, signal?: AbortSignal): Promise<void> {
    const ids = new Set<SessionId>(requested ?? []);
    for (const sessionId of queueSignatures.keys()) ids.add(sessionId); // settle-out sweeps
    for (const sessionId of runningCache.keys()) ids.add(sessionId);
    try {
      const data = await baseline(signal);
      for (const session of data.sessions) if (session.running) ids.add(session.id);
    } catch {
      // A failed baseline read still lets requested identities reconcile.
    }
    await Promise.all([...ids].map((sessionId) => projectQueueSeat(sessionId, signal)));
  }

  const sessions = {
    list: async (): Promise<RpcResponse<{ items: SessionSummary[] }>> => {
      const data = await baseline();
      return ok({
        items: data.sessions.map((session) => {
          const summary: SessionSummary = {
            sessionId: session.id,
            updatedAt: updatedAtOf(session),
            running: runningCache.get(session.id) ?? session.running,
            blank: session.headRevision === 0,
            cwd: `/workspaces/${session.workspaceId}`,
          };
          return summary;
        }),
      });
    },
    search: async (): Promise<RpcResponse<{ items: unknown[]; hasMore: boolean }>> => ok({ items: [], hasMore: false }),
    create: async (payload: unknown, signal?: AbortSignal): Promise<RpcResponse<{ sessionId: SessionId }>> => {
      const workspaceId = stringAt(payload, "workspaceId");
      if (workspaceId === undefined) {
        return fail("workspace-not-found", "VOIE session creation requires a workspaceId", { workspaceId: "" });
      }
      const data = await refreshBaseline(signal);
      const workspace = data.workspaces.find((w) => w.id === workspaceId);
      const listedProject = workspace?.projectId ?? "";
      const projectId = listedProject !== "" ? listedProject : getVoieDshHostContext().projectId;
      if (projectId === "") {
        return fail("workspace-not-found", "workspace is not visible to this session", { workspaceId });
      }
      const requestedRaw = stringAt(payload, "agentId");
      const requestedAgent =
        requestedRaw !== undefined && requestedRaw !== "" ? requestedRaw : undefined;
      try {
        const result = await carrier.mutate({
          op: "conversation.create",
          intentId: crypto.randomUUID(),
          projectId,
          ...(requestedAgent === undefined ? {} : { agentId: requestedAgent }),
          workspaceId,
        }, signal);
        if (!result.accepted || result.conversationId === undefined || result.conversationId === "") {
          return fail("internal", result.reason ?? "conversation create refused", { workspaceId });
        }
        await refreshBaseline(signal).catch(() => {});
        notifySessionsChanged();
        return ok({ sessionId: result.conversationId });
      } catch (error) {
        return fail("internal", error instanceof Error ? error.message : "conversation create failed", { workspaceId });
      }
    },
    history: async (payload: unknown): Promise<RpcResponse<HistoryValue>> => {
      const requested = sessionIdOf(payload);
      if (requested === "") return ok({ events: [], hasMore: false });
      const beforeSeq = numberAt(payload, "beforeSeq");
      const maxMessages = numberAt(payload, "maxMessages") ?? 50;
      const pageOpt: { maxMessages: number; beforeSeq?: number } = { maxMessages };
      if (beforeSeq !== undefined) pageOpt.beforeSeq = beforeSeq;
      const loaded = await carrier.loadHistory(requested, undefined, pageOpt);
      const mapped = inDurableOrder(loaded.events)
        .map((event) => {
          const envelope = eventEnvelopeOf(event);
          if (envelope === null) return null;
          const view = toolViewOf(event);
          const entry: HistoryEntry = { event: envelope };
          if (view !== undefined) entry.view = view;
          return entry;
        })
        .filter((entry): entry is HistoryEntry => entry !== null);
      const page = pageSessionHistory(mapped, beforeSeq, maxMessages);
      const tailSeq = page.events.length > 0 ? page.events[page.events.length - 1]?.event.seq ?? -1 : -1;
      const projections: ProjectionsBlock = { asOfSeq: tailSeq, values: {} };
      replayStatus.add(requested);
      await projectQueueSeat(requested, undefined, loaded.liveRuns);
      return ok({
        events: page.events,
        hasMore: loaded.hasMore || page.hasMore,
        projections,
      });
    },
    models: async (): Promise<RpcResponse<unknown>> =>
      ok({
        current: { provider: "voie-parent", model: "voie-scripted" },
        routable: true,
        groups: [],
        failures: [],
      }),
    selectModel: async (): Promise<RpcResponse<unknown>> =>
      ok({ selected: { provider: "voie-parent", model: "voie-scripted" } }),
    prompt: async (payload: unknown, signal?: AbortSignal): Promise<RpcResponse<{ accepted: true }>> => {
      const sessionId = sessionIdOf(payload);
      if (sessionId === "") return fail("session-not-found", "sessionId is required", { sessionId });
      const text = promptTextOf(payload).trim();
      if (text === "") return fail("internal", "empty prompt", {});
      const intentId = intentIdOf(payload);
      const steer = stringAt(payload, "mode") === "steer";
      const existing = inflightPrompt.get(sessionId);
      let slot: InFlightPrompt;
      if (existing !== undefined && existing.intentId === intentId) {
        slot = existing;
      } else {
        const predecessor = existing?.work;
        const work = (async (): Promise<MutationResult> => {
          if (predecessor !== undefined) {
            await predecessor.catch(() => undefined);
          }
          if (steer) {
            const cancelled = await carrier.mutate({
              op: "conversation.cancel",
              intentId: crypto.randomUUID(),
              conversationId: sessionId,
            }, signal);
            if (!cancelled.accepted && !cancelAlreadySettled(cancelled.state) && cancelled.state !== "idle") {
              return cancelled;
            }
          }
          const result = await carrier.mutate({
            op: "conversation.message",
            intentId,
            conversationId: sessionId,
            prompt: text,
          }, signal);
          return result;
        })();
        slot = { intentId, work };
        inflightPrompt.set(sessionId, slot);
      }
      try {
        const result = await slot.work;
        if (!result.accepted) {
          return fail("internal", result.reason ?? "message refused", {});
        }
        notifySessionsChanged();
        await reconcileQueues(new Set([sessionId]), signal).catch(() => {});
        return ok({ accepted: true });
      } finally {
        const current = inflightPrompt.get(sessionId);
        if (current?.work === slot.work) inflightPrompt.delete(sessionId);
      }
    },
    cancel: async (payload: unknown, signal?: AbortSignal): Promise<RpcResponse<{ accepted: true }>> => {
      const sessionId = sessionIdOf(payload);
      if (sessionId === "") return fail("session-not-found", "sessionId is required", { sessionId });
      try {
        const pending = inflightPrompt.get(sessionId);
        if (pending !== undefined) {
          await pending.work.catch(() => undefined);
        }
        const result = await carrier.mutate({
          op: "conversation.cancel",
          intentId: crypto.randomUUID(),
          conversationId: sessionId,
        }, signal);
        if (!result.accepted && !cancelAlreadySettled(result.state) && result.state !== "idle") {
          return fail("internal", result.reason ?? `cancel refused (${result.state ?? "unknown"})`, { sessionId });
        }
        await reconcileQueues(new Set([sessionId]), signal).catch(() => {});
        return ok({ accepted: true });
      } catch (error) {
        return fail("internal", error instanceof Error ? error.message : "cancel failed", { sessionId });
      }
    },
    rename: async (payload: unknown): Promise<RpcResponse<unknown>> =>
      fail("internal", "VOIE conversations are not renamable through this carrier", {}),
    fork: async (payload: unknown): Promise<RpcResponse<unknown>> =>
      fail("fork-unavailable", "VOIE does not fork conversations", {}),
    attachment: async (payload: unknown): Promise<RpcResponse<unknown>> =>
      fail("attachment-error", "VOIE does not serve image attachments", { reason: "attachment unsupported" }),
    updateQueue: async (payload: unknown, signal?: AbortSignal): Promise<RpcResponse<{ accepted: true }>> => {
      // The pending/queue seat is projected from the conversation-scoped
      // runs resource, never synthesized from mutation results: every queued
      // dock row IS an accepted durable run keyed by its own runId, so
      // removing it resolves that exact run through the run-scoped cancel
      // resource (POST /api/runs/:id/cancel) — never the session's first
      // active run. A row already settled (missing from the fresh runs
      // truth, or terminal) converges: the requested effect — the row out of
      // the queue — already holds durably. Steering interrupts the live
      // dispatched turn (conversation.cancel) so the durable queue can
      // dispatch; it does not inject a row ahead of earlier accepted runs.
      const action = isRecord(payload) ? payload["action"] : undefined;
      const kind = isRecord(action) ? action["kind"] : undefined;
      if (kind === "remove" || kind === "cancel") {
        const sessionId = stringAt(payload, "sessionId") ?? "";
        const itemId = stringAt(payload, "itemId") ?? "";
        if (sessionId === "") return fail("session-not-found", "sessionId is required", { sessionId });
        if (itemId === "") return fail("internal", "cancelling a queued row requires its run item id", { itemId });
        try {
          const runs = await loadConversationRuns(sessionId, signal);
          const target = runs.find((run) => run.runId === itemId);
          if (target === undefined || LIVE_RUN_STATES[target.state] !== true) {
            // Verified absent from the live seat: converged.
            return ok({ accepted: true });
          }
          const cancelled = await cancelExactRun(itemId, signal);
          if (cancelled.accepted || cancelAlreadySettled(cancelled.state)) return ok({ accepted: true });
          return fail("cancel-refused", `cancel refused (${cancelled.state ?? "unknown"})`, { runId: itemId });
        } catch (error) {
          return fail("internal", error instanceof Error ? error.message : "queue cancel failed", { runId: itemId });
        }
      }
      if (kind === "steer") {
        const sessionId = stringAt(payload, "sessionId") ?? "";
        if (sessionId === "") return fail("session-not-found", "sessionId is required", { sessionId });
        try {
          const result = await carrier.mutate({
            op: "conversation.cancel",
            intentId: crypto.randomUUID(),
            conversationId: sessionId,
          }, signal);
          if (!result.accepted && !cancelAlreadySettled(result.state) && result.state !== "idle") {
            return fail("internal", result.reason ?? `steer refused (${result.state ?? "unknown"})`, { sessionId });
          }
          await reconcileQueues(new Set([sessionId]), signal).catch(() => {});
          return ok({ accepted: true });
        } catch (error) {
          return fail("internal", error instanceof Error ? error.message : "queue steer failed", { sessionId });
        }
      }
      return fail(
        "internal",
        "unsupported queue action",
        { itemId: stringAt(payload, "itemId") ?? "" },
      );
    },
  };

  const host = {
    describe: async (): Promise<RpcResponse<unknown>> => {
      const data = await baseline();
      return ok({
        version: "0.1.0-rc.8",
        cwd: "/",
        provider: "voie-parent",
        model: "voie-scripted",
        attachedSessions: data.sessions.length,
        home: "/",
        canOpenPath: false,
      });
    },
    pickDirectory: async (): Promise<RpcResponse<{ path: null }>> => ok({ path: null }),
    listDirectory: async (payload: unknown): Promise<RpcResponse<unknown>> =>
      fail("directory-unreadable", "VOIE serves no host filesystem browser", { path: stringAt(payload, "path") ?? "/" }),
    createDirectory: async (payload: unknown): Promise<RpcResponse<unknown>> =>
      fail("directory-create-failed", "VOIE serves no host filesystem browser", { path: stringAt(payload, "path") ?? "/" }),
    openPath: async (payload: unknown): Promise<RpcResponse<unknown>> =>
      fail("internal", "VOIE serves no host filesystem opener", {}),
  };

  const workspaceViews = (data: Baseline): WorkspaceView[] =>
    data.workspaces.map((workspace) => {
      const sessionIds = data.sessions
        .filter((session) => session.workspaceId === workspace.id)
        .map((session) => session.id);
      const createdAt = workspace.createdAt ?? "";
      return {
        workspaceId: workspace.id,
        path: `/workspaces/${workspace.id}`,
        title: workspace.fabricName?.trim() || workspace.id,
        sessionIds,
        createdAt,
        updatedAt: createdAt,
        state: workspace.state,
      };
    });

  const hostWorkspaceView = (view: WorkspaceView): HostWorkspaceView => ({
    workspaceId: view.workspaceId,
    path: view.path,
    title: view.title,
    sessionIds: view.sessionIds,
    createdAt: view.createdAt,
    updatedAt: view.updatedAt,
  });

  const publishWorkspaceViews = (data: Baseline): void => {
    for (const view of workspaceViews(data)) {
      emitHost({ type: "host/workspace-changed", workspace: hostWorkspaceView(view) });
    }
  };

  syncWorkspaces = async (signal?: AbortSignal): Promise<void> => {
    const data = await refreshBaseline(signal);
    publishWorkspaceViews(data);
  };

  const workspace = {
    list: async (): Promise<RpcResponse<{ items: WorkspaceView[]; archivedSessionIds: SessionId[] }>> => {
      const data = await refreshBaseline();
      return ok({ items: workspaceViews(data), archivedSessionIds: [] });
    },
    create: async (payload: unknown): Promise<RpcResponse<unknown>> =>
      fail("workspace-invalid-path", "VOIE workspaces are provisioned outside this carrier", { path: stringAt(payload, "path") ?? "" }),
    rename: async (payload: unknown): Promise<RpcResponse<unknown>> =>
      fail("workspace-not-found", "VOIE workspaces are not renamable through this carrier", { workspaceId: stringAt(payload, "workspaceId") ?? "" }),
    delete: async (payload: unknown): Promise<RpcResponse<unknown>> =>
      fail("workspace-not-found", "VOIE workspace deletion is not served", { workspaceId: stringAt(payload, "workspaceId") ?? "" }),
    insertBefore: async (payload: unknown): Promise<RpcResponse<unknown>> =>
      fail("workspace-not-found", "VOIE workspace ordering is not served", { workspaceId: stringAt(payload, "workspaceId") ?? "" }),
    insertSessionBefore: async (payload: unknown): Promise<RpcResponse<unknown>> =>
      fail("workspace-not-found", "VOIE workspace ordering is not served", { workspaceId: stringAt(payload, "workspaceId") ?? "" }),
    archiveSession: async (payload: unknown): Promise<RpcResponse<unknown>> =>
      fail("session-not-found", "VOIE has no archive", { sessionId: sessionIdOf(payload) }),
  };

  // Frame pump shared by the mux stream and the connection handle's sinks.
  const pump = {
    buffer: [] as MuxFrame[],
    waiters: [] as Array<(frames: MuxFrame[]) => void>,
    push(frame: MuxFrame): void {
      this.buffer.push(frame);
      for (const wake of this.waiters.splice(0)) wake(this.buffer.splice(0));
    },
    async *stream(signal: AbortSignal): AsyncGenerator<RpcRequest<MuxFrame>> {
      while (!signal.aborted) {
        if (this.buffer.length > 0) {
          const frame = this.buffer.shift() as MuxFrame;
          yield { rpcId: rpcId(), payload: frame };
          continue;
        }
        const frames = await new Promise<MuxFrame[] | null>((resolve) => {
          const wake = (value: MuxFrame[]) => resolve(value);
          this.waiters.push(wake);
          const onAbort = () => {
            const index = this.waiters.indexOf(wake);
            if (index !== -1) this.waiters.splice(index, 1);
            resolve(null);
          };
          signal.addEventListener("abort", onAbort, { once: true });
        });
        if (frames === null) return;
        for (const frame of frames) yield { rpcId: rpcId(), payload: frame };
      }
    },
  };

  const hostPump = {
    buffer: [] as HostFrame[],
    waiters: [] as Array<(frames: HostFrame[]) => void>,
    push(frame: HostFrame): void {
      this.buffer.push(frame);
      for (const wake of this.waiters.splice(0)) wake(this.buffer.splice(0));
    },
    async *stream(signal: AbortSignal): AsyncGenerator<RpcRequest<HostFrame>> {
      while (!signal.aborted) {
        if (this.buffer.length > 0) {
          const frame = this.buffer.shift() as HostFrame;
          yield { rpcId: rpcId(), payload: frame };
          continue;
        }
        const frames = await new Promise<HostFrame[] | null>((resolve) => {
          const wake = (value: HostFrame[]) => resolve(value);
          this.waiters.push(wake);
          const onAbort = () => {
            const index = this.waiters.indexOf(wake);
            if (index !== -1) this.waiters.splice(index, 1);
            resolve(null);
          };
          signal.addEventListener("abort", onAbort, { once: true });
        });
        if (frames === null) return;
        for (const frame of frames) yield { rpcId: rpcId(), payload: frame };
      }
    },
  };

  const events = {
    mux: (_payload: unknown, signal: AbortSignal): AsyncGenerator<RpcRequest<MuxFrame>> => pump.stream(signal),
    host: (_payload: unknown, signal: AbortSignal): AsyncGenerator<RpcRequest<HostFrame>> => hostPump.stream(signal),
  };

  const empty = async (): Promise<RpcResponse<Record<string, never>>> => ok({});
  const emptyList = async (): Promise<RpcResponse<{ items: never[] }>> => ok({ items: [] });

  const face = {
    sessions,
    subagents: {
      list: async (): Promise<RpcResponse<{ entries: never[]; parentAvailable: boolean }>> => ok({ entries: [], parentAvailable: false }),
      history: async (): Promise<RpcResponse<{ events: never[]; hasMore: boolean }>> => ok({ events: [], hasMore: false }),
      prompt: async (): Promise<RpcResponse<never>> => fail("subagent-not-found", "VOIE has no subagents", {}),
      interrupt: empty,
    },
    host,
    workspace,
    skills: { list: emptyList },
    agentPresets: {
      list: emptyList,
      select: empty,
      read: empty,
      copy: empty,
      openDocument: empty,
      remove: empty,
    },
    events,
    goals: {
      create: empty,
      edit: empty,
      pause: empty,
      resume: empty,
      complete: empty,
      clear: empty,
    },
    settings: {
      describe: async (): Promise<RpcResponse<unknown>> => ok({ writable: false, hasDocument: false, namespaces: [] }),
      openDocument: empty,
      update: empty,
      replace: empty,
      mutate: empty,
    },
    credentials: {
      describe: async (): Promise<RpcResponse<{ credentials: Record<string, never> }>> => ok({ credentials: {} }),
      set: empty,
      unset: empty,
    },
    llm: {
      providers: emptyList,
      models: emptyList,
      discoverModels: emptyList,
    },
    respond: async (): Promise<RpcResponse<never>> => fail("internal", "no answerable interaction is pending", {}),
  };

  return {
    api: face,
    baseline,
    refreshBaseline,
    pump,
    hostPump,
    reconcileQueues,
    setSinks(next: ConnectionSinks): void {
      muxSink = next.onMuxEnvelope;
      hostSink = next.onHostEnvelope;
    },
  };
}

/**
 * Creates the DSH `ConnectionHandle` over a `VoieCarrier` (or any carrier
 * implementing the same seam). The returned `start()` runs the bounded
 * long-poll loop: baseline → connected → poll; on stale it re-reads the
 * baseline and resumes with the fresh cursor.
 */
export function createConnectionHandle(carrier: {
  loadBaseline(signal?: AbortSignal): Promise<Baseline>;
  poll(cursor: string, signal?: AbortSignal): Promise<{ kind: "events"; cursor: string; events: readonly CanonicalEvent[] } | { kind: "stale" }>;
  loadHistory(
    sessionId: SessionId,
    signal?: AbortSignal,
    page?: { beforeSeq?: number; maxMessages?: number },
  ): Promise<HistoryPage>;
  mutate(mutation: Mutation, signal?: AbortSignal): Promise<MutationResult>;
} = new VoieCarrier()) {
  const built = createCarrierApi(carrier);
  let started = false;

  return {
    api: built.api,
    isLoopback: false,
    hostDescription: {
      getSnapshot: () => undefined,
      subscribe: () => () => {},
    },
    rpc: {
      call: async (): Promise<never> => {
        throw new Error("connection RPC channel is not served by this carrier");
      },
    },
    start(sinks: ConnectionSinks) {
      if (started) throw new Error("connection: the stream loop is already owned by another consumer");
      started = true;
      built.setSinks(sinks);
      const ac = new AbortController();
      void (async () => {
        const baseline = await built.baseline();
        let cursor = baseline.cursor;
        sinks.onStateChange?.("connected");
        sinks.onConnected?.(await built.api.host.describe());
        // Seed the queue seats from durable runs truth so the docks match a
        // reload exactly; subsequent poll batches and refreshes reconcile
        // per affected conversation.
        void built.reconcileQueues(undefined, ac.signal).catch(() => {});
        while (!ac.signal.aborted) {
          try {
            const result = await carrier.poll(cursor, ac.signal);
            if (result.kind === "stale") {
              sinks.onStateChange?.("reconnecting");
              const fresh = await built.refreshBaseline();
              cursor = fresh.cursor;
              void built.reconcileQueues(undefined, ac.signal).catch(() => {});
              sinks.onStateChange?.("connected");
              continue;
            }
            cursor = result.cursor;
            for (const event of result.events) {
              const envelope = eventEnvelopeOf(event);
              if (envelope === null) continue;
              const frame: SessionEventFrame = { type: "session/event", sessionId: event.sessionId, event: envelope };
              const view = toolViewOf(event);
              if (view !== undefined) frame.view = view;
              built.pump.push(frame);
              sinks.onMuxEnvelope?.({ rpcId: rpcId(), payload: frame });
            }
            // Reconcile after every poll, including empty batches. A Run can
            // go unknown/terminal with no session events (activation child
            // death, restart classify). Skipping that sweep left
            // host/session-status running=true and the chat stuck on
            // "Deep diving...". Identity still comes from the runs resource.
            const touched = new Set<SessionId>(result.events.map((event) => event.sessionId));
            void built.reconcileQueues(touched, ac.signal).catch(() => {});
          } catch {
            if (ac.signal.aborted) return;
            sinks.onStateChange?.("reconnecting");
            // Bounded backoff before the next generation: refresh the baseline
            // and re-enter the poll loop.
            await new Promise<void>((resolve) => setTimeout(resolve, 1_000));
            try {
              const fresh = await built.refreshBaseline(ac.signal);
              cursor = fresh.cursor;
              void built.reconcileQueues(undefined, ac.signal).catch(() => {});
              sinks.onStateChange?.("connected");
            } catch {
              // The abort path lands here on shutdown; the loop condition
              // exits next.
            }
          }
        }
      })();
      return {
        stop: () => {
          ac.abort();
          started = false;
        },
      };
    },
  };
}

export type { VoieCarrierOptions };
