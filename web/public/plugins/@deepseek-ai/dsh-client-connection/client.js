window.__ModuleLoader__.load({ id: "@deepseek-ai/dsh-client-connection", factory: (require) => {
const module = { exports: {} };
const exports = module.exports;

"use strict";
var __defProp = Object.defineProperty;
var __getOwnPropDesc = Object.getOwnPropertyDescriptor;
var __getOwnPropNames = Object.getOwnPropertyNames;
var __hasOwnProp = Object.prototype.hasOwnProperty;
var __export = (target, all) => {
  for (var name in all)
    __defProp(target, name, { get: all[name], enumerable: true });
};
var __copyProps = (to, from, except, desc) => {
  if (from && typeof from === "object" || typeof from === "function") {
    for (let key of __getOwnPropNames(from))
      if (!__hasOwnProp.call(to, key) && key !== except)
        __defProp(to, key, { get: () => from[key], enumerable: !(desc = __getOwnPropDesc(from, key)) || desc.enumerable });
  }
  return to;
};
var __toCommonJS = (mod) => __copyProps(__defProp({}, "__esModule", { value: true }), mod);

// src/connection-voie/plugin.ts
var plugin_exports = {};
__export(plugin_exports, {
  VoieCarrier: () => VoieCarrier,
  apply: () => apply,
  createCarrierApi: () => createCarrierApi,
  createConnectionHandle: () => createConnectionHandle,
  inject: () => inject
});
module.exports = __toCommonJS(plugin_exports);

// src/api/validate.ts
function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
function arrayAt(value, key) {
  const field = value[key];
  return Array.isArray(field) ? field : [];
}
function asStr(value) {
  return typeof value === "string" ? value : null;
}
function asNum(value) {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}
function asBoolOr(value, fallback) {
  return typeof value === "boolean" ? value : fallback;
}

// src/carrier/voie.ts
var FEED_PAGE_LIMIT = 512;
var VoieHttpError = class extends Error {
  status;
  constructor(status, message) {
    super(message);
    this.status = status;
  }
};
function decodeEventBytes(value) {
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
function canonicalEventsOf(item) {
  if (item.bytes === null) return [];
  const decoded = decodeEventBytes(item.bytes);
  if (decoded === null) return [];
  const events = [];
  for (const [eventIndex, line] of decoded.split("\n").entries()) {
    if (line.trim().length === 0) continue;
    let parsed;
    try {
      parsed = JSON.parse(line);
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
      sourceEventSeqs: record["sourceEventSeqs"]
    });
  }
  return events;
}
function canonicalItemsOf(raw) {
  const record = isRecord(raw) ? raw : {};
  return arrayAt(record, "items").map((item) => {
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
      bytes
    };
  }).filter((item) => item !== null);
}
function feedCursorOf(raw, fallback) {
  const record = isRecord(raw) ? raw : {};
  return asNum(record.cursor) ?? fallback;
}
function sessionSummariesOf(raw) {
  const record = isRecord(raw) ? raw : {};
  return arrayAt(record, "items").map((item) => {
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
      createdAt: asStr(item.createdAt)
    };
  }).filter((session) => session !== null);
}
function agentSummariesOf(raw) {
  const record = isRecord(raw) ? raw : {};
  return arrayAt(record, "items").map((item) => {
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
      maxTokens: asNum(item.maxTokens)
    };
  }).filter((agent) => agent !== null);
}
function workspaceSummariesOf(raw) {
  const record = isRecord(raw) ? raw : {};
  return arrayAt(record, "items").map((item) => {
    if (!isRecord(item)) return null;
    const id = asStr(item.id);
    if (id === null) return null;
    return {
      id,
      projectId: asStr(item.projectId) ?? "",
      fabricId: asStr(item.fabricId),
      fabricName: asStr(item.fabricName),
      state: asStr(item.state),
      execGeneration: asNum(item.execGeneration),
      createdAt: asStr(item.createdAt)
    };
  }).filter((workspace) => workspace !== null);
}
var VoieCarrier = class {
  fetchImpl;
  origin;
  holdMs;
  intervalMs;
  schedulers;
  scopeId;
  inflight = /* @__PURE__ */ new Set();
  constructor(options = {}) {
    this.fetchImpl = options.fetchImpl ?? globalThis.fetch.bind(globalThis);
    this.origin = options.origin ?? "";
    this.holdMs = options.holdMs ?? 3e4;
    this.intervalMs = options.intervalMs ?? 1e3;
    this.schedulers = options.schedulers ?? {
      schedule: (delayMs, run) => Number(setTimeout(run, delayMs)),
      clear: (handle) => clearTimeout(handle),
      now: () => Date.now()
    };
    this.scopeId = options.scopeId ?? null;
  }
  async fetchJson(path, init, signal) {
    const method = init.method ?? "GET";
    const callerHeaders = isRecord(init.headers) ? { ...init.headers } : {};
    const headers = {
      accept: "application/json",
      ...callerHeaders,
      ...method === "GET" ? {} : { "x-voie-intent": "mutate", "content-type": "application/json" }
    };
    const response = await this.fetchImpl(`${this.origin}${path}`, {
      ...init,
      headers,
      // The opaque `voie_session` cookie is the sole credential; same-origin
      // scope keeps it on the control plane and never leaks it elsewhere.
      credentials: "same-origin",
      ...signal === void 0 ? {} : { signal }
    });
    if (!response.ok) {
      let error = `HTTP ${String(response.status)}`;
      try {
        const body = await response.json();
        const record = isRecord(body) ? body : null;
        const message = record === null ? null : asStr(record.error);
        if (message !== null && message !== "") error = message;
      } catch {
      }
      throw new VoieHttpError(response.status, `${init.method ?? "GET"} ${path} failed: ${error}`);
    }
    return await response.json();
  }
  /** Resource path under the mount's scope boundary, when one is set. */
  resource(path) {
    if (this.scopeId === null) return `/api/${path}`;
    return `/api/scopes/${encodeURIComponent(this.scopeId)}/${path}`;
  }
  /** Session rows from any listing that serves the session-row shape. */
  toSessionRows(raw) {
    return sessionSummariesOf(raw).map((session) => ({
      id: session.id,
      projectId: session.projectId,
      agentId: session.agentId,
      workspaceId: session.workspaceId,
      running: session.running,
      headRevision: session.headRevision,
      writerGeneration: session.writerGeneration,
      attentionGeneration: session.attentionGeneration,
      createdAt: session.createdAt
    }));
  }
  async loadBaseline(signal) {
    const [sessionsRaw, agentsRaw, workspacesRaw, eventsRaw] = await Promise.all([
      this.fetchJson(this.resource("sessions"), { method: "GET", headers: { accept: "application/json" } }, signal),
      this.fetchJson(this.resource("agents"), { method: "GET", headers: { accept: "application/json" } }, signal),
      this.fetchJson(this.resource("workspaces"), { method: "GET", headers: { accept: "application/json" } }, signal),
      this.fetchJson("/api/events?after=0", { method: "GET", headers: { accept: "application/json" } }, signal)
    ]);
    const sessions = this.toSessionRows(sessionsRaw);
    const agents = agentSummariesOf(agentsRaw).map((agent) => ({
      id: agent.id,
      projectId: agent.projectId,
      name: agent.name,
      model: agent.model,
      systemPrompt: agent.systemPrompt,
      bashEnabled: agent.bashEnabled,
      maxTokens: agent.maxTokens
    }));
    const workspaces = workspaceSummariesOf(workspacesRaw).map((workspace) => ({
      id: workspace.id,
      projectId: workspace.projectId,
      fabricId: workspace.fabricId,
      fabricName: workspace.fabricName,
      state: workspace.state,
      execGeneration: workspace.execGeneration,
      createdAt: workspace.createdAt
    }));
    return { cursor: String(feedCursorOf(eventsRaw, 0)), sessions, agents, workspaces };
  }
  /**
   * One bounded long-poll cycle over the canonical event feed. Every response
   * carries append batches (`items`) plus the server cursor; batches are
   * decoded into canonical events on arrival. The loop paces re-reads at
   * `intervalMs` until fresh events arrive or the `holdMs` bound elapses.
   *
   * Cursor discipline: every request asks `?after=<currentCursor>`; the
   * current cursor advances from every response, including an empty one, and
   * the advanced value rides home when the deadline expires. A server cursor
   * below the requested cursor — or HTTP 409 — yields `{ kind: "stale" }`:
   * the consumer re-reads the baseline and resumes from its fresh cursor.
   * Rows at or below the requested cursor never cross the seam twice.
   */
  async poll(cursor, signal) {
    const requestedCursor = Number(cursor);
    const deadline = this.schedulers.now() + this.holdMs;
    let currentCursor = requestedCursor;
    for (; ; ) {
      const after = currentCursor;
      let raw;
      try {
        raw = await this.fetchJson(
          `/api/events?after=${encodeURIComponent(String(after))}`,
          { method: "GET", headers: { accept: "application/json" } },
          signal
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
        currentCursor = serverCursor;
        const events = canonicalItemsOf(raw).filter((item) => item.globalSeq > after).flatMap(canonicalEventsOf);
        if (events.length > 0) {
          return { kind: "events", cursor: String(currentCursor), events };
        }
      }
      const remaining = deadline - this.schedulers.now();
      if (remaining <= 0) break;
      await new Promise((resolve) => {
        const handle = this.schedulers.schedule(Math.min(this.intervalMs, remaining), () => resolve());
        signal?.addEventListener(
          "abort",
          () => {
            this.schedulers.clear(handle);
            resolve();
          },
          { once: true }
        );
      });
    }
    return { kind: "events", cursor: String(currentCursor), events: [] };
  }
  /**
   * Full canonical history for one session: pages `/api/sessions/:id/events`
   * with the feed cursor until the server returns fewer than a page (the
   * control plane caps each read at 512 appends). Events arrive in the
   * store's durable order — oldest first, never re-sorted.
   */
  async loadHistory(sessionId, signal) {
    const events = [];
    let cursor = 0;
    for (; ; ) {
      const raw = await this.fetchJson(
        `/api/sessions/${encodeURIComponent(sessionId)}/events?after=${encodeURIComponent(String(cursor))}`,
        { method: "GET", headers: { accept: "application/json" } },
        signal
      );
      const items = canonicalItemsOf(raw);
      events.push(...items.flatMap(canonicalEventsOf));
      if (items.length < FEED_PAGE_LIMIT) break;
      const next = feedCursorOf(raw, cursor);
      if (next <= cursor) break;
      cursor = next;
    }
    return events;
  }
  /**
   * Conversations bound to one workspace under the scoped listing contract
   * (`/api/workspaces/:id/conversations`); rows decode exactly like the
   * authoritative session list.
   */
  async loadWorkspaceConversations(workspaceId, signal) {
    const raw = await this.fetchJson(
      `/api/workspaces/${encodeURIComponent(workspaceId)}/conversations`,
      { method: "GET", headers: { accept: "application/json" } },
      signal
    );
    return this.toSessionRows(raw);
  }
  async mutate(mutation, signal) {
    const key = mutation.intentId;
    if (this.inflight.has(key)) {
      return { accepted: false, reason: "duplicate in-flight mutation", conversationId: void 0, runId: void 0, state: void 0, result: void 0 };
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
                conversationId: mutation.conversationId,
                projectId: mutation.projectId,
                agentId: mutation.agentId,
                workspaceId: mutation.workspaceId,
                intentId: mutation.intentId,
                prompt: mutation.prompt
              })
            },
            signal
          );
          const record = isRecord(raw) ? raw : {};
          return {
            accepted: asBoolOr(record.accepted, false),
            reason: asStr(record.reason) ?? void 0,
            conversationId: asStr(record.conversationId) ?? mutation.conversationId,
            runId: asStr(record.runId) ?? void 0,
            state: asStr(record.state) ?? void 0,
            result: void 0
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
                prompt: mutation.prompt
              })
            },
            signal
          );
          const record = isRecord(raw) ? raw : {};
          return {
            accepted: asBoolOr(record.accepted, false),
            reason: asStr(record.reason) ?? void 0,
            conversationId: asStr(record.conversationId) ?? mutation.conversationId,
            runId: asStr(record.runId) ?? void 0,
            state: asStr(record.state) ?? void 0,
            result: record["result"]
          };
        }
        case "conversation.cancel": {
          const runsRaw = await this.fetchJson(
            "/api/runs",
            { method: "GET", headers: { accept: "application/json" } },
            signal
          );
          const candidate = arrayAt(isRecord(runsRaw) ? runsRaw : {}, "items").map((raw) => {
            if (!isRecord(raw)) return null;
            const id = asStr(raw.id);
            const sessionId = asStr(raw.sessionId);
            const state = asStr(raw.state);
            if (id === null || sessionId === null || state === null) return null;
            return { id, sessionId, state };
          }).filter((run) => run !== null).find(
            (run) => run.sessionId === mutation.conversationId && (run.state === "accepted" || run.state === "dispatched")
          );
          if (candidate === void 0) {
            return { accepted: false, reason: "no active run to cancel", conversationId: mutation.conversationId, runId: void 0, state: void 0, result: void 0 };
          }
          const cancelRaw = await this.fetchJson(
            `/api/runs/${encodeURIComponent(candidate.id)}/cancel`,
            { method: "POST", headers: { accept: "application/json" } },
            signal
          );
          const record = isRecord(cancelRaw) ? cancelRaw : {};
          return {
            accepted: asBoolOr(record.accepted, false),
            reason: record.accepted === true ? void 0 : `cancel refused (${String(record.state ?? "unknown")})`,
            conversationId: mutation.conversationId,
            runId: asStr(record.runId) ?? candidate.id,
            state: asStr(record.state) ?? void 0,
            result: void 0
          };
        }
      }
    } finally {
      this.inflight.delete(key);
    }
  }
};

// src/connection-voie/api.ts
function rpcId() {
  return crypto.randomUUID();
}
function ok(value) {
  return { rpcId: rpcId(), result: { ok: true, value } };
}
function fail(code, message, details = {}) {
  return { rpcId: rpcId(), result: { ok: false, error: { code, message, details } } };
}
function promptTextOf(payload) {
  const record = isRecord(payload) ? payload : {};
  const content = record["content"];
  if (!Array.isArray(content)) return "";
  return content.flatMap((part) => {
    const item = isRecord(part) ? part : null;
    if (item === null || item["type"] !== "text" || typeof item["text"] !== "string") return [];
    return [item["text"]];
  }).join("\n");
}
function sessionIdOf(payload) {
  const record = isRecord(payload) ? payload : {};
  const value = record["sessionId"];
  return typeof value === "string" ? value : "";
}
function intentIdOf(payload) {
  const record = isRecord(payload) ? payload : {};
  const value = record["intentId"];
  return typeof value === "string" && value !== "" ? value : crypto.randomUUID();
}
function numberAt(payload, key) {
  const record = isRecord(payload) ? payload : {};
  return asNum(record[key]) ?? void 0;
}
function stringAt(payload, key) {
  const record = isRecord(payload) ? payload : {};
  const value = record[key];
  return typeof value === "string" ? value : void 0;
}
function eventEnvelopeOf(event) {
  if (event.seq === null || event.time === null) return null;
  const envelope = { type: event.type, seq: event.seq, time: event.time, data: event.data };
  if (event.surfaceOp !== void 0) envelope.surfaceOp = event.surfaceOp;
  if (event.sourceEventSeqs !== void 0) envelope.sourceEventSeqs = event.sourceEventSeqs;
  return envelope;
}
function toolViewOf(event) {
  if (!isRecord(event.data)) return void 0;
  if (event.type === "tool/call") {
    if (event.data["name"] !== "bash") return void 0;
    const argumentsRaw = typeof event.data["arguments"] === "string" ? event.data["arguments"] : "";
    let parsed = null;
    try {
      parsed = JSON.parse(argumentsRaw);
    } catch {
      parsed = null;
    }
    const args = isRecord(parsed) ? parsed : {};
    const title = typeof args["command"] === "string" ? args["command"] : argumentsRaw;
    const cwd = typeof args["workdir"] === "string" ? args["workdir"] : void 0;
    const view = { card: "terminal", title };
    if (cwd !== void 0) view.cwd = cwd;
    return { for: "call", view };
  }
  if (event.type === "tool/result") {
    const message = isRecord(event.data["message"]) ? event.data["message"] : null;
    const content = message === null ? void 0 : message["content"];
    const first = Array.isArray(content) ? content[0] : void 0;
    const item = isRecord(first) ? first : null;
    if (item === null || item["type"] !== "tool-result") return void 0;
    const body = Array.isArray(item["content"]) ? item["content"] : [];
    const output = body.flatMap((part) => {
      const p = isRecord(part) ? part : null;
      if (p === null || p["type"] !== "text" || typeof p["text"] !== "string") return [];
      return [p["text"]];
    }).join("\n");
    return { for: "result", view: { card: "terminal", output, exitCode: item["isError"] === true ? 1 : 0 } };
  }
  return void 0;
}
function inDurableOrder(events) {
  return [...events].sort((a, b) => a.globalSeq - b.globalSeq || a.eventIndex - b.eventIndex);
}
function createCarrierApi(carrier, net = {}) {
  let baselinePromise = null;
  const baseline = (signal) => baselinePromise ??= carrier.loadBaseline(signal);
  const refreshBaseline = (signal) => {
    baselinePromise = carrier.loadBaseline(signal);
    return baselinePromise;
  };
  const updatedAtOf = (session) => {
    const parsed = Date.parse(session.createdAt ?? "");
    return Number.isFinite(parsed) ? parsed : 0;
  };
  const pendingConversations = /* @__PURE__ */ new Map();
  const promoting = /* @__PURE__ */ new Set();
  const visibleBaseline = async () => {
    const data = await baseline();
    if (pendingConversations.size === 0) return data;
    const created = (/* @__PURE__ */ new Date()).toISOString();
    const synthetics = [...pendingConversations.entries()].map(
      ([id, entry]) => ({
        id,
        projectId: entry.projectId,
        agentId: entry.agentId ?? "",
        workspaceId: entry.workspaceId,
        running: false,
        headRevision: 0,
        writerGeneration: null,
        attentionGeneration: null,
        createdAt: created
      })
    );
    return { ...data, sessions: [...synthetics, ...data.sessions] };
  };
  const runsFetchImpl = net.fetchImpl ?? globalThis.fetch.bind(globalThis);
  function decodeRunsOf(body) {
    const items = isRecord(body) ? arrayAt(body, "runs") : [];
    const rows = [];
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
  async function loadConversationRuns(sessionId, signal) {
    const response = await runsFetchImpl(
      `/api/conversations/${encodeURIComponent(sessionId)}/runs`,
      { method: "GET", headers: { accept: "application/json" }, credentials: "same-origin", ...signal === void 0 ? {} : { signal } }
    );
    if (!response.ok) throw new Error(`GET /api/conversations/${sessionId}/runs failed: HTTP ${String(response.status)}`);
    return decodeRunsOf(await response.json());
  }
  const queueSignatures = /* @__PURE__ */ new Map();
  const runningCache = /* @__PURE__ */ new Map();
  const reconciling = /* @__PURE__ */ new Set();
  const LIVE_RUN_STATES = { accepted: true, dispatched: true };
  function inQueueOrder(runs) {
    return [...runs].sort((a, b) => a.seq - b.seq || (a.runId < b.runId ? -1 : a.runId > b.runId ? 1 : 0));
  }
  async function projectQueueSeat(sessionId, signal) {
    if (pendingConversations.has(sessionId)) return;
    if (reconciling.has(sessionId)) return;
    reconciling.add(sessionId);
    try {
      let runs;
      try {
        runs = await loadConversationRuns(sessionId, signal);
      } catch {
        return;
      }
      const live = inQueueOrder(runs.filter((run) => LIVE_RUN_STATES[run.state] === true));
      const signature = JSON.stringify(live.map((run) => [run.runId, run.seq, run.state]));
      if (queueSignatures.get(sessionId) !== signature) {
        queueSignatures.set(sessionId, signature);
        pump.push({
          type: "session/queue",
          sessionId,
          items: live.filter((run) => run.state === "accepted").map((run) => ({
            id: run.runId,
            placement: "queued",
            message: {
              id: run.runId,
              role: "user",
              content: [{ type: "text", text: run.prompt ?? "" }],
              source: { kind: "voie-run" }
            }
          }))
        });
      }
      const running = runs.some((run) => LIVE_RUN_STATES[run.state] === true);
      if (runningCache.get(sessionId) !== running) {
        runningCache.set(sessionId, running);
        hostPump.push({ type: "host/session-status", sessionId, running });
      }
    } finally {
      reconciling.delete(sessionId);
    }
  }
  async function reconcileQueues(requested, signal) {
    const ids = new Set(requested ?? []);
    for (const sessionId of queueSignatures.keys()) ids.add(sessionId);
    for (const sessionId of runningCache.keys()) ids.add(sessionId);
    try {
      const data = await baseline(signal);
      for (const session of data.sessions) if (session.running) ids.add(session.id);
    } catch {
    }
    await Promise.all([...ids].map((sessionId) => projectQueueSeat(sessionId, signal)));
  }
  const sessions = {
    list: async () => {
      const data = await visibleBaseline();
      return ok({
        items: data.sessions.map((session) => {
          const summary = {
            sessionId: session.id,
            updatedAt: updatedAtOf(session),
            running: session.running,
            blank: session.headRevision === 0
          };
          return summary;
        })
      });
    },
    search: async () => ok({ items: [], hasMore: false }),
    create: async (payload) => {
      const workspaceId = stringAt(payload, "workspaceId");
      if (workspaceId === void 0) {
        return fail("workspace-not-found", "VOIE session creation requires a workspaceId", { workspaceId: "" });
      }
      const data = await baseline();
      const workspace2 = data.workspaces.find((w) => w.id === workspaceId);
      if (workspace2 === void 0) {
        return fail("workspace-not-found", "workspace is not visible to this session", { workspaceId });
      }
      const requestedAgent = stringAt(payload, "agentId");
      const resolvedAgent = requestedAgent ?? data.agents.find((a) => a.projectId === workspace2.projectId)?.id;
      const sessionId = crypto.randomUUID();
      pendingConversations.set(sessionId, {
        projectId: workspace2.projectId,
        ...resolvedAgent === void 0 ? {} : { agentId: resolvedAgent },
        workspaceId
      });
      return ok({ sessionId });
    },
    history: async (payload) => {
      const requested = sessionIdOf(payload);
      if (requested === "") return ok({ events: [], hasMore: false });
      if (pendingConversations.has(requested)) {
        return ok({ events: [], hasMore: false });
      }
      const beforeSeq = numberAt(payload, "beforeSeq");
      const maxMessages = numberAt(payload, "maxMessages") ?? 50;
      const all = inDurableOrder(await carrier.loadHistory(requested));
      let window = all.map((event) => {
        const envelope = eventEnvelopeOf(event);
        if (envelope === null) return null;
        const view = toolViewOf(event);
        const entry = { event: envelope };
        if (view !== void 0) entry.view = view;
        return entry;
      }).filter((entry) => entry !== null);
      if (beforeSeq !== void 0) {
        window = window.filter((entry) => entry.event.seq < beforeSeq).slice(-Math.max(maxMessages, 1));
      } else {
        window = window.slice(-Math.max(maxMessages, 1));
      }
      const tailSeq = window.length > 0 ? window[window.length - 1]?.event.seq ?? -1 : -1;
      const projections = { asOfSeq: tailSeq, values: {} };
      return ok({ events: window, hasMore: false, projections });
    },
    models: async () => ok({
      current: { provider: "voie-parent", model: "voie-scripted" },
      routable: true,
      groups: [],
      failures: []
    }),
    selectModel: async () => ok({ selected: { provider: "voie-parent", model: "voie-scripted" } }),
    prompt: async (payload, signal) => {
      const sessionId = sessionIdOf(payload);
      if (sessionId === "") return fail("session-not-found", "sessionId is required", { sessionId });
      const text = promptTextOf(payload).trim();
      if (text === "") return fail("internal", "empty prompt", {});
      const pendingEntry = pendingConversations.get(sessionId);
      if (pendingEntry !== void 0) {
        if (promoting.has(sessionId)) {
          return fail("internal", "the first prompt is already submitting", { sessionId });
        }
        promoting.add(sessionId);
        try {
          const result2 = await carrier.mutate({
            op: "conversation.create",
            intentId: intentIdOf(payload),
            conversationId: sessionId,
            projectId: pendingEntry.projectId,
            ...pendingEntry.agentId === void 0 ? {} : { agentId: pendingEntry.agentId },
            workspaceId: pendingEntry.workspaceId,
            prompt: text
          }, signal);
          if (!result2.accepted) {
            return fail("conversation-create-refused", result2.reason ?? "first prompt refused", { sessionId });
          }
          pendingConversations.delete(sessionId);
          await refreshBaseline(signal);
          void reconcileQueues(/* @__PURE__ */ new Set([sessionId]), signal).catch(() => {
          });
          return ok({ accepted: true });
        } finally {
          promoting.delete(sessionId);
        }
      }
      const result = await carrier.mutate({
        op: "conversation.message",
        intentId: intentIdOf(payload),
        conversationId: sessionId,
        prompt: text
      }, signal);
      if (!result.accepted) return fail("internal", result.reason ?? "message refused", {});
      void reconcileQueues(/* @__PURE__ */ new Set([sessionId]), signal).catch(() => {
      });
      return ok({ accepted: true });
    },
    cancel: async (payload, signal) => {
      const sessionId = sessionIdOf(payload);
      if (sessionId === "") return fail("session-not-found", "sessionId is required", { sessionId });
      const result = await carrier.mutate({
        op: "conversation.cancel",
        intentId: intentIdOf(payload),
        conversationId: sessionId
      }, signal);
      if (!result.accepted) return fail("internal", result.reason ?? "cancel refused", {});
      void reconcileQueues(/* @__PURE__ */ new Set([sessionId]), signal).catch(() => {
      });
      return ok({ accepted: true });
    },
    rename: async (payload) => fail("internal", "VOIE conversations are not renamable through this carrier", {}),
    fork: async (payload) => fail("fork-unavailable", "VOIE does not fork conversations", {}),
    attachment: async (payload) => fail("attachment-error", "VOIE does not serve image attachments", { reason: "attachment unsupported" }),
    updateQueue: async (payload, signal) => {
      const action = isRecord(payload) ? payload["action"] : void 0;
      const kind = isRecord(action) ? action["kind"] : void 0;
      if (kind === "remove" || kind === "cancel") {
        const sessionId = stringAt(payload, "sessionId") ?? "";
        const itemId = stringAt(payload, "itemId") ?? "";
        if (sessionId === "") return fail("session-not-found", "sessionId is required", { sessionId });
        if (itemId === "") return fail("internal", "cancelling a queued row requires its run item id", { itemId });
        try {
          const runs = await loadConversationRuns(sessionId, signal);
          const target = runs.find((run) => run.runId === itemId);
          if (target === void 0 || LIVE_RUN_STATES[target.state] !== true) {
            return ok({ accepted: true });
          }
          const response = await runsFetchImpl(
            `/api/runs/${encodeURIComponent(itemId)}/cancel`,
            {
              method: "POST",
              headers: { accept: "application/json", "x-voie-intent": "mutate", "content-type": "application/json" },
              credentials: "same-origin",
              ...signal === void 0 ? {} : { signal }
            }
          );
          if (!response.ok) {
            return fail("cancel-refused", `POST /api/runs/${itemId}/cancel failed: HTTP ${String(response.status)}`, { runId: itemId });
          }
          const body = await response.json();
          const record = isRecord(body) ? body : {};
          if (asBoolOr(record.accepted, false)) return ok({ accepted: true });
          return fail("cancel-refused", `cancel refused (${asStr(record.state) ?? "unknown"})`, { runId: itemId });
        } catch (error) {
          return fail("internal", error instanceof Error ? error.message : "queue cancel failed", { runId: itemId });
        }
      }
      return fail(
        "steer-unavailable",
        "VOIE has no injection steering; follow-ups dispatch in durable order",
        { itemId: stringAt(payload, "itemId") ?? "" }
      );
    }
  };
  const host = {
    describe: async () => {
      const data = await baseline();
      return ok({
        version: "0.1.0-rc.8",
        cwd: "/",
        provider: "voie-parent",
        model: "voie-scripted",
        attachedSessions: data.sessions.length,
        home: "/",
        canOpenPath: false
      });
    },
    pickDirectory: async () => ok({ path: null }),
    listDirectory: async (payload) => fail("directory-unreadable", "VOIE serves no host filesystem browser", { path: stringAt(payload, "path") ?? "/" }),
    createDirectory: async (payload) => fail("directory-create-failed", "VOIE serves no host filesystem browser", { path: stringAt(payload, "path") ?? "/" }),
    openPath: async (payload) => fail("internal", "VOIE serves no host filesystem opener", {})
  };
  const workspaceViews = (data) => data.workspaces.map((workspace2) => {
    const sessionIds = data.sessions.filter((session) => session.workspaceId === workspace2.id).map((session) => session.id);
    const createdAt = workspace2.createdAt ?? "";
    return {
      workspaceId: workspace2.id,
      path: `/workspaces/${workspace2.id}`,
      title: workspace2.fabricName ?? workspace2.id,
      sessionIds,
      createdAt,
      updatedAt: createdAt
    };
  });
  const workspace = {
    list: async () => {
      const data = await visibleBaseline();
      return ok({ items: workspaceViews(data), archivedSessionIds: [] });
    },
    create: async (payload) => fail("workspace-invalid-path", "VOIE workspaces are provisioned outside this carrier", { path: stringAt(payload, "path") ?? "" }),
    rename: async (payload) => fail("workspace-not-found", "VOIE workspaces are not renamable through this carrier", { workspaceId: stringAt(payload, "workspaceId") ?? "" }),
    delete: async (payload) => fail("workspace-not-found", "VOIE workspace deletion is not served", { workspaceId: stringAt(payload, "workspaceId") ?? "" }),
    insertBefore: async (payload) => fail("workspace-not-found", "VOIE workspace ordering is not served", { workspaceId: stringAt(payload, "workspaceId") ?? "" }),
    insertSessionBefore: async (payload) => fail("workspace-not-found", "VOIE workspace ordering is not served", { workspaceId: stringAt(payload, "workspaceId") ?? "" }),
    archiveSession: async (payload) => fail("session-not-found", "VOIE has no archive", { sessionId: sessionIdOf(payload) })
  };
  const pump = {
    buffer: [],
    waiters: [],
    push(frame) {
      this.buffer.push(frame);
      for (const wake of this.waiters.splice(0)) wake(this.buffer.splice(0));
    },
    async *stream(signal) {
      while (!signal.aborted) {
        if (this.buffer.length > 0) {
          const frame = this.buffer.shift();
          yield { rpcId: rpcId(), payload: frame };
          continue;
        }
        const frames = await new Promise((resolve) => {
          const wake = (value) => resolve(value);
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
    }
  };
  const hostPump = {
    buffer: [],
    waiters: [],
    push(frame) {
      this.buffer.push(frame);
      for (const wake of this.waiters.splice(0)) wake(this.buffer.splice(0));
    },
    async *stream(signal) {
      while (!signal.aborted) {
        if (this.buffer.length > 0) {
          const frame = this.buffer.shift();
          yield { rpcId: rpcId(), payload: frame };
          continue;
        }
        const frames = await new Promise((resolve) => {
          const wake = (value) => resolve(value);
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
    }
  };
  const events = {
    mux: (_payload, signal) => pump.stream(signal),
    host: (_payload, signal) => hostPump.stream(signal)
  };
  const empty = async () => ok({});
  const emptyList = async () => ok({ items: [] });
  const face = {
    sessions,
    subagents: {
      list: async () => ok({ entries: [], parentAvailable: false }),
      history: async () => ok({ events: [], hasMore: false }),
      prompt: async () => fail("subagent-not-found", "VOIE has no subagents", {}),
      interrupt: empty
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
      remove: empty
    },
    events,
    goals: {
      create: empty,
      edit: empty,
      pause: empty,
      resume: empty,
      complete: empty,
      clear: empty
    },
    settings: {
      describe: async () => ok({ writable: false, hasDocument: false, namespaces: [] }),
      openDocument: empty,
      update: empty,
      replace: empty,
      mutate: empty
    },
    credentials: {
      describe: async () => ok({ credentials: {} }),
      set: empty,
      unset: empty
    },
    llm: {
      providers: emptyList,
      models: emptyList,
      discoverModels: emptyList
    },
    respond: async () => fail("internal", "no answerable interaction is pending", {})
  };
  return {
    api: face,
    baseline,
    refreshBaseline,
    pump,
    hostPump,
    reconcileQueues
  };
}
function createConnectionHandle(carrier = new VoieCarrier()) {
  const built = createCarrierApi(carrier);
  let started = false;
  return {
    api: built.api,
    isLoopback: false,
    hostDescription: {
      getSnapshot: () => void 0,
      subscribe: () => () => {
      }
    },
    rpc: {
      call: async () => {
        throw new Error("connection RPC channel is not served by this carrier");
      }
    },
    start(sinks) {
      if (started) throw new Error("connection: the stream loop is already owned by another consumer");
      started = true;
      const ac = new AbortController();
      void (async () => {
        const baseline = await built.baseline();
        let cursor = baseline.cursor;
        sinks.onStateChange?.("connected");
        sinks.onConnected?.(await built.api.host.describe());
        void built.reconcileQueues(void 0, ac.signal).catch(() => {
        });
        while (!ac.signal.aborted) {
          try {
            const result = await carrier.poll(cursor, ac.signal);
            if (result.kind === "stale") {
              sinks.onStateChange?.("reconnecting");
              const fresh = await built.refreshBaseline();
              cursor = fresh.cursor;
              void built.reconcileQueues(void 0, ac.signal).catch(() => {
              });
              sinks.onStateChange?.("connected");
              continue;
            }
            cursor = result.cursor;
            for (const event of result.events) {
              const envelope = eventEnvelopeOf(event);
              if (envelope === null) continue;
              const frame = { type: "session/event", sessionId: event.sessionId, event: envelope };
              const view = toolViewOf(event);
              if (view !== void 0) frame.view = view;
              built.pump.push(frame);
              sinks.onMuxEnvelope?.({ rpcId: rpcId(), payload: frame });
            }
            const touched = new Set(result.events.map((event) => event.sessionId));
            if (touched.size > 0) void built.reconcileQueues(touched, ac.signal).catch(() => {
            });
          } catch {
            if (ac.signal.aborted) return;
            sinks.onStateChange?.("reconnecting");
            await new Promise((resolve) => setTimeout(resolve, 1e3));
            try {
              const fresh = await built.refreshBaseline(ac.signal);
              cursor = fresh.cursor;
              void built.reconcileQueues(void 0, ac.signal).catch(() => {
              });
              sinks.onStateChange?.("connected");
            } catch {
            }
          }
        }
      })();
      return {
        stop: () => {
          ac.abort();
          started = false;
        }
      };
    }
  };
}

// src/connection-voie/plugin-fn.ts
var DSH_MOUNT_ID = "voie-dsh-root";
function mountScopedCarrier() {
  const host = document.getElementById(DSH_MOUNT_ID);
  const scopeId = host?.dataset.voieScopeId ?? "";
  return new VoieCarrier(scopeId === "" ? {} : { scopeId });
}
var inject = [];
function commandRefused() {
  return {
    ok: false,
    error: {
      code: "unknown-command",
      message: "VOIE serves no slash-command executor through this carrier",
      details: {}
    }
  };
}
function createVoieRemoteCommands() {
  return {
    execute: async () => commandRefused()
  };
}
function createVoieRemote() {
  return {
    // `remote.commands` is a separate injected service key; this accessor
    // keeps the stock `remote.commands.execute(...)` call shape working for
    // consumers that read it as a property of `remote`.
    commands: createVoieRemoteCommands(),
    $dispatch: () => {
    }
  };
}
function apply(ctx) {
  ctx.provide("connection", createConnectionHandle(mountScopedCarrier()));
  ctx.provide("remote", createVoieRemote());
  ctx.provide("remote.commands", createVoieRemoteCommands());
}

return module.exports;
}});

