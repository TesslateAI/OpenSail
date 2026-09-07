/**
 * Same-origin VOIE carrier over the control-plane resource API.
 *
 * Baseline: one GET per resource (`/api/sessions`, `/api/agents`,
 * `/api/workspaces`) plus one GET `/api/events?head=1` for the cursor.
 * That cursor read does not load Session history bytes.
 * Poll: one held GET `/api/events?after=<cursor>&wait=1`. The server holds
 * until events arrive or the wait bound elapses. HTTP 409 (or a returned
 * cursor below the requested cursor) means the cursor is stale: the
 * consumer re-reads the baseline and re-polls.
 * Mutations: one POST per user action, keyed by the caller's intent id; the
 * browser in-flight set is the local fence and the server's run identity +
 * request-hash dedup is the durable fence.
 *
 * Canonical identity is never synthesized: `globalSeq`/`revision` come from
 * the store reference, `eventIndex` from the line position inside the batch,
 * and `seq`/`time` are the producer's own fields carried verbatim (null when
 * the durable line genuinely lacks them). No order, timestamp, or lifecycle
 * frame is ever derived from projected blocks.
 *
 * Transport: same-origin fetch with explicit `credentials: "same-origin"` so
 * the opaque `voie_session` cookie rides every request. No Whaled bearer
 * gate, no DSH provider, no credentials, no separate web process.
 */
import { arrayAt, asBoolOr, asNum, asStr, isRecord } from "../api/validate.ts";
import type {
  AgentRow,
  Baseline,
  CanonicalEvent,
  HistoryPage,
  Iso8601,
  Mutation,
  MutationResult,
  PollResult,
  RunRow,
  SessionRow,
  WorkspaceRow,
  VoieCarrierFace,
} from "./types.ts";

/** Wall-clock and timer seams; defaults ride the browser globals. */
export type VoieCarrierSchedulers = {
  /** Schedules the inter-read pause; returns a cancellable handle. */
  schedule(delayMs: number, run: () => void): number;
  /** Cancels one handle from `schedule`. */
  clear(handle: number): void;
  /** Feeds the hold-bound deadline. */
  now(): number;
};

export type VoieCarrierOptions = {
  fetchImpl?: typeof fetch;
  origin?: string;
  /** Upper bound of one `poll` call, in ms. */
  holdMs?: number;
  /** Pacing between empty feed reads inside one `poll` call, in ms. */
  intervalMs?: number;
  /** Timer/wall-clock seams; defaults bind the host globals. */
  schedulers?: VoieCarrierSchedulers;
  /** Project boundary for baseline listings (`/api/projects/:id/*`); absent
   *  keeps the unscoped control-plane resources. */
  projectId?: string;
};

type CanonicalItem = {
  sessionId: string;
  globalSeq: number;
  revision: number;
  appendId: string | null;
  objectKey: string | null;
  contentHash: string | null;
  byteLength: number | null;
  bytes: string | null;
};

type SessionSummary = {
  id: string;
  projectId: string;
  agentId: string;
  workspaceId: string;
  running: boolean;
  headRevision: number;
  writerGeneration: number | null;
  attentionGeneration: number | null;
  createdAt: Iso8601 | null;
};

type AgentSummary = {
  id: string;
  projectId: string;
  name: string;
  model: string | null;
  systemPrompt: string | null;
  bashEnabled: boolean;
  maxTokens: number | null;
};

type WorkspaceSummary = {
  id: string;
  projectId: string;
  fabricId: string | null;
  fabricName: string | null;
  state: string | null;
  execGeneration: number | null;
  createdAt: Iso8601 | null;
};

class VoieHttpError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

/** Decodes base64 batch bytes into its JSON lines (UTF-8). */
function decodeEventBytes(value: string): string | null {
  try {
    const binary = atob(value);
    const bytes = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1) {
      bytes[index] = binary.charCodeAt(index);
    }
    return new TextDecoder().decode(bytes);
  } catch {
    return null;
  }
}

/**
 * Expands one canonical append batch into its events. The batch envelope
 * (`sessionId`, `globalSeq`, `revision`, append identity) applies to every
 * line; `eventIndex` is the line position inside the batch; `seq`/`time` are
 * the producer's own fields when the durable line carries them — never
 * derived from position, never backfilled.
 */
function canonicalEventsOf(item: CanonicalItem): CanonicalEvent[] {
  if (item.bytes === null) return [];
  const decoded = decodeEventBytes(item.bytes);
  if (decoded === null) return [];
  const events: CanonicalEvent[] = [];
  for (const [eventIndex, line] of decoded.split("\n").entries()) {
    if (line.trim().length === 0) continue;
    let parsed: unknown;
    try {
      parsed = JSON.parse(line) as unknown;
    } catch {
      continue;
    }
    const record = isRecord(parsed) ? parsed : null;
    if (record === null || typeof record["type"] !== "string") continue;
    events.push({
      sessionId: item.sessionId,
      type: record["type"],
      data: record["data"] ?? null,
      globalSeq: item.globalSeq,
      revision: item.revision,
      eventIndex,
      appendId: item.appendId,
      objectKey: item.objectKey,
      contentHash: item.contentHash,
      byteLength: item.byteLength,
      seq: asNum(record["seq"]),
      time: asNum(record["time"]),
      // Producer-authored placement metadata, carried verbatim.
      surfaceOp: record["surfaceOp"],
      sourceEventSeqs: record["sourceEventSeqs"],
    });
  }
  return events;
}

function canonicalItemsOf(raw: unknown): CanonicalItem[] {
  const record = isRecord(raw) ? raw : {};
  return arrayAt(record, "items")
    .map((item): CanonicalItem | null => {
      if (!isRecord(item)) return null;
      const sessionId = asStr(item.sessionId);
      const globalSeq = asNum(item.globalSeq);
      const revision = asNum(item.revision);
      const bytes = asStr(item.bytes);
      if (sessionId === null || globalSeq === null || revision === null || bytes === null) return null;
      return {
        sessionId,
        globalSeq,
        revision,
        appendId: asStr(item.appendId),
        objectKey: asStr(item.objectKey),
        contentHash: asStr(item.contentHash),
        byteLength: asNum(item.byteLength),
        bytes,
      };
    })
    .filter((item): item is CanonicalItem => item !== null);
}

function feedCursorOf(raw: unknown, fallback: number): number {
  const record = isRecord(raw) ? raw : {};
  return asNum(record.cursor) ?? fallback;
}

function liveRunsOf(record: Record<string, unknown>): RunRow[] {
  const rows: RunRow[] = [];
  for (const item of arrayAt(record, "liveRuns")) {
    if (!isRecord(item)) continue;
    const runId = asStr(item.runId);
    const seq = asNum(item.seq);
    const state = asStr(item.state);
    if (runId === null || seq === null || !Number.isInteger(seq) || state === null) continue;
    rows.push({
      runId,
      seq,
      state,
      prompt: asStr(item.prompt),
      actorUserId: asStr(item.actorUserId),
    });
  }
  return rows;
}

function sessionSummariesOf(raw: unknown): SessionSummary[] {
  const record = isRecord(raw) ? raw : {};
  return arrayAt(record, "items")
    .map((item): SessionSummary | null => {
      if (!isRecord(item)) return null;
      const id = asStr(item.id);
      if (id === null) return null;
      return {
        id,
        projectId: asStr(item.projectId) ?? "",
        agentId: asStr(item.agentId) ?? "",
        workspaceId: asStr(item.workspaceId) ?? "",
        running: asBoolOr(item.running, false),
        headRevision: asNum(item.headRevision) ?? 0,
        writerGeneration: asNum(item.writerGeneration),
        attentionGeneration: asNum(item.attentionGeneration),
        createdAt: asStr(item.createdAt),
      };
    })
    .filter((session): session is SessionSummary => session !== null);
}

function agentSummariesOf(raw: unknown): AgentSummary[] {
  const record = isRecord(raw) ? raw : {};
  return arrayAt(record, "items")
    .map((item): AgentSummary | null => {
      if (!isRecord(item)) return null;
      const id = asStr(item.id);
      if (id === null) return null;
      return {
        id,
        projectId: asStr(item.projectId) ?? "",
        name: asStr(item.name) ?? "",
        model: asStr(item.model),
        systemPrompt: asStr(item.systemPrompt),
        bashEnabled: asBoolOr(item.bashEnabled, false),
        maxTokens: asNum(item.maxTokens),
      };
    })
    .filter((agent): agent is AgentSummary => agent !== null);
}

function presentId(value: string | undefined): string | undefined {
  return value !== undefined && value !== "" ? value : undefined;
}

function workspaceSummariesOf(raw: unknown): WorkspaceSummary[] {
  const record = isRecord(raw) ? raw : {};
  return arrayAt(record, "items")
    .map((item): WorkspaceSummary | null => {
      if (!isRecord(item)) return null;
      const id = asStr(item.id);
      if (id === null) return null;
      // Project listings (`GET /api/projects/:id/workspaces`) name the owner
      // `projectId`; leftover rows may still emit `scopeId`. They are the same identity.
      return {
        id,
        projectId: asStr(item.projectId) || asStr(item.scopeId) || "",
        fabricId: asStr(item.fabricId),
        // Scoped listings carry `label`; unscoped `/api/workspaces` may
        // carry `fabricName`. DSH workspace title uses this field.
        fabricName: asStr(item.fabricName) || asStr(item.label),
        state: asStr(item.state),
        execGeneration: asNum(item.execGeneration),
        createdAt: asStr(item.createdAt),
      };
    })
    .filter((workspace): workspace is WorkspaceSummary => workspace !== null);
}

/**
 * Same-origin production carrier: authoritative baseline, bounded long-poll
 * with stale detection, paged per-session history, single-attempt mutations.
 */
export class VoieCarrier implements VoieCarrierFace {
  private readonly fetchImpl: typeof fetch;
  private readonly origin: string;
  private readonly holdMs: number;
  private readonly intervalMs: number;
  private readonly schedulers: VoieCarrierSchedulers;
  private readonly projectId: string | null;
  private readonly inflight = new Set<string>();

  constructor(options: VoieCarrierOptions = {}) {
    // Browsers require `fetch` to be called with its global receiver; the
    // bound function also keeps the same-origin default safe when this class
    // is bundled into a classic-script plugin factory.
    this.fetchImpl = options.fetchImpl ?? globalThis.fetch.bind(globalThis);
    this.origin = options.origin ?? "";
    this.holdMs = options.holdMs ?? 30_000;
    this.intervalMs = options.intervalMs ?? 1_000;
    this.schedulers =
      options.schedulers ?? {
        schedule: (delayMs, run) => Number(setTimeout(run, delayMs)),
        clear: (handle) => clearTimeout(handle),
        now: () => Date.now(),
      };
    this.projectId = options.projectId ?? null;
  }

  private async fetchJson(
    path: string,
    init: RequestInit,
    signal?: AbortSignal,
  ): Promise<unknown> {
    const method = init.method ?? "GET";
    // Central mutation-admission seam: the control plane's same_origin_json
    // gate requires `x-voie-intent: mutate` AND an application/json
    // content-type on every non-GET, bodyless cancels included. Caller
    // headers win except for those two markers, which are always enforced
    // here so no call site can forget them. The browser attaches the exact
    // same-origin Origin header automatically; this wrapper never touches it.
    const callerHeaders = isRecord(init.headers) ? { ...init.headers } : {};
    const headers: Record<string, string> = {
      accept: "application/json",
      ...callerHeaders,
      ...(method === "GET"
        ? {}
        : { "x-voie-intent": "mutate", "content-type": "application/json" }),
    };
    const response = await this.fetchImpl(`${this.origin}${path}`, {
      ...init,
      headers,
      // The opaque `voie_session` cookie is the sole credential; same-origin
      // scope keeps it on the control plane and never leaks it elsewhere.
      credentials: "same-origin",
      ...(signal === undefined ? {} : { signal }),
    });
    if (!response.ok) {
      let error = `HTTP ${String(response.status)}`;
      try {
        const body = (await response.json()) as unknown;
        const record = isRecord(body) ? body : null;
        const message = record === null ? null : asStr(record.error);
        if (message !== null && message !== "") error = message;
      } catch {
        // Non-JSON error bodies keep the HTTP fallback.
      }
      throw new VoieHttpError(response.status, `${init.method ?? "GET"} ${path} failed: ${error}`);
    }
    return (await response.json()) as unknown;
  }

  /** Resource path under the mount's scope boundary, when one is set. */
  private resource(path: string): string {
    if (this.projectId === null) return `/api/${path}`;
    return `/api/projects/${encodeURIComponent(this.projectId)}/${path}`;
  }

  /** Session rows from any listing that serves the session-row shape. */
  private toSessionRows(raw: unknown): SessionRow[] {
    return sessionSummariesOf(raw).map((session) => ({
      id: session.id,
      projectId: session.projectId,
      agentId: session.agentId,
      workspaceId: session.workspaceId,
      running: session.running,
      headRevision: session.headRevision,
      writerGeneration: session.writerGeneration,
      attentionGeneration: session.attentionGeneration,
      createdAt: session.createdAt,
    }));
  }

  async loadBaseline(signal?: AbortSignal): Promise<Baseline> {
    const [sessionsRaw, agentsRaw, workspacesRaw, eventsRaw] = await Promise.all([
      this.fetchJson(this.resource("sessions"), { method: "GET", headers: { accept: "application/json" } }, signal),
      this.fetchJson(this.resource("agents"), { method: "GET", headers: { accept: "application/json" } }, signal),
      this.fetchJson(this.resource("workspaces"), { method: "GET", headers: { accept: "application/json" } }, signal),
      this.fetchJson("/api/events?head=1", { method: "GET", headers: { accept: "application/json" } }, signal),
    ]);
    const sessions = this.toSessionRows(sessionsRaw);
    const agents: AgentRow[] = agentSummariesOf(agentsRaw).map((agent) => ({
      id: agent.id,
      projectId: agent.projectId,
      name: agent.name,
      model: agent.model,
      systemPrompt: agent.systemPrompt,
      bashEnabled: agent.bashEnabled,
      maxTokens: agent.maxTokens,
    }));
    const workspaces: WorkspaceRow[] = workspaceSummariesOf(workspacesRaw).map((workspace) => ({
      id: workspace.id,
      projectId: workspace.projectId,
      fabricId: workspace.fabricId,
      fabricName: workspace.fabricName,
      state: workspace.state,
      execGeneration: workspace.execGeneration,
      createdAt: workspace.createdAt,
    }));
    return { cursor: String(feedCursorOf(eventsRaw, 0)), sessions, agents, workspaces };
  }

  /**
   * One held long-poll over the canonical event feed. The server waits until
   * events arrive or its wait bound elapses. HTTP 409 or a cursor below the
   * requested cursor is `{ kind: "stale" }`.
   */
  async poll(cursor: string, signal?: AbortSignal): Promise<PollResult> {
    const requestedCursor = Number(cursor);
    const after = requestedCursor;
    let raw: unknown;
    try {
      raw = await this.fetchJson(
        `/api/events?after=${encodeURIComponent(String(after))}&wait=1`,
        { method: "GET", headers: { accept: "application/json" } },
        signal,
      );
    } catch (error) {
      if (error instanceof VoieHttpError && error.status === 409) {
        return { kind: "stale" };
      }
      throw error;
    }
    const serverCursor = feedCursorOf(raw, after);
    if (serverCursor < after) return { kind: "stale" };
    if (serverCursor > after) {
      const events = canonicalItemsOf(raw)
        .filter((item) => item.globalSeq > after)
        .flatMap(canonicalEventsOf);
      if (events.length > 0) {
        return { kind: "events", cursor: String(serverCursor), events };
      }
    }
    return { kind: "events", cursor: String(serverCursor > after ? serverCursor : after), events: [] };
  }

  /**
   * One bounded history page. The server reads PostgreSQL payloads for the
   * requested window; the browser never walks every append or Blob object.
   */
  async loadHistory(
    sessionId: string,
    signal?: AbortSignal,
    page?: { beforeSeq?: number; maxMessages?: number },
  ): Promise<HistoryPage> {
    const maxMessages = page?.maxMessages ?? 128;
    const before = page?.beforeSeq;
    const query = new URLSearchParams();
    query.set("maxMessages", String(maxMessages));
    if (before !== undefined) query.set("beforeSeq", String(before));
    const raw = await this.fetchJson(
      `/api/conversations/${encodeURIComponent(sessionId)}/history?${query.toString()}`,
      { method: "GET", headers: { accept: "application/json" } },
      signal,
    );
    const record = isRecord(raw) ? raw : {};
    const items = canonicalItemsOf(raw);
    const liveRuns = liveRunsOf(record);
    return {
      events: items.flatMap(canonicalEventsOf),
      hasMore: asBoolOr(record.hasMore, false),
      running: asBoolOr(record.running, liveRuns.length > 0),
      liveRuns,
    };
  }

  /**
   * Conversations bound to one workspace under the scoped listing contract
   * (`/api/workspaces/:id/conversations`); rows decode exactly like the
   * authoritative session list.
   */
  async loadWorkspaceConversations(
    workspaceId: string,
    signal?: AbortSignal,
  ): Promise<SessionRow[]> {
    const raw = await this.fetchJson(
      `/api/workspaces/${encodeURIComponent(workspaceId)}/conversations`,
      { method: "GET", headers: { accept: "application/json" } },
      signal,
    );
    return this.toSessionRows(raw);
  }

  async mutate(mutation: Mutation, signal?: AbortSignal): Promise<MutationResult> {
    const key = mutation.intentId;
    if (this.inflight.has(key)) {
      return { accepted: false, reason: "duplicate in-flight mutation", conversationId: undefined, runId: undefined, state: undefined, result: undefined };
    }
    this.inflight.add(key);
    try {
      switch (mutation.op) {
        case "conversation.create": {
          const raw = await this.fetchJson(
            "/api/conversations",
            {
              method: "POST",
              headers: { "content-type": "application/json", accept: "application/json" },
              body: JSON.stringify({
                projectId: mutation.projectId,
                ...(presentId(mutation.agentId) === undefined
                  ? {}
                  : { agentId: mutation.agentId }),
                workspaceId: mutation.workspaceId,
                intentId: mutation.intentId,
              }),
            },
            signal,
          );
          const record = isRecord(raw) ? raw : {};
          return {
            accepted: asBoolOr(record.accepted, false),
            reason: asStr(record.reason) ?? undefined,
            conversationId: asStr(record.conversationId) ?? undefined,
            runId: asStr(record.runId) ?? undefined,
            state: asStr(record.state) ?? undefined,
            result: undefined,
          };
        }
        case "conversation.message": {
          const raw = await this.fetchJson(
            `/api/conversations/${encodeURIComponent(mutation.conversationId)}/messages`,
            {
              method: "POST",
              headers: { "content-type": "application/json", accept: "application/json" },
              body: JSON.stringify({
                intentId: mutation.intentId,
                prompt: mutation.prompt,
              }),
            },
            signal,
          );
          const record = isRecord(raw) ? raw : {};
          return {
            accepted: asBoolOr(record.accepted, false),
            reason: asStr(record.reason) ?? undefined,
            conversationId: asStr(record.conversationId) ?? mutation.conversationId,
            runId: asStr(record.runId) ?? undefined,
            state: asStr(record.state) ?? undefined,
            result: record["result"],
          };
        }
        case "conversation.cancel": {
          const cancelRaw = await this.fetchJson(
            `/api/conversations/${encodeURIComponent(mutation.conversationId)}/cancel`,
            { method: "POST", headers: { accept: "application/json" } },
            signal,
          );
          const record = isRecord(cancelRaw) ? cancelRaw : {};
          const state = asStr(record.state) ?? undefined;
          const accepted =
            asBoolOr(record.accepted, false) ||
            state === "unknown" ||
            state === "cancelled" ||
            state === "terminal" ||
            state === "idle";
          return {
            accepted,
            reason: accepted ? undefined : `cancel refused (${String(state ?? "unknown")})`,
            conversationId: mutation.conversationId,
            runId: asStr(record.runId) ?? undefined,
            state,
            result: undefined,
          };
        }
      }
    } finally {
      this.inflight.delete(key);
    }
  }
}
