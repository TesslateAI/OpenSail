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
    for (let key2 of __getOwnPropNames(from))
      if (!__hasOwnProp.call(to, key2) && key2 !== except)
        __defProp(to, key2, { get: () => from[key2], enumerable: !(desc = __getOwnPropDesc(from, key2)) || desc.enumerable });
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
function arrayAt(value, key2) {
  const field = value[key2];
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
function liveRunsOf(record) {
  const rows = [];
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
      actorUserId: asStr(item.actorUserId)
    });
  }
  return rows;
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
function presentId(value) {
  return value !== void 0 && value !== "" ? value : void 0;
}
function workspaceSummariesOf(raw) {
  const record = isRecord(raw) ? raw : {};
  return arrayAt(record, "items").map((item) => {
    if (!isRecord(item)) return null;
    const id = asStr(item.id);
    if (id === null) return null;
    return {
      id,
      projectId: asStr(item.projectId) || asStr(item.scopeId) || "",
      fabricId: asStr(item.fabricId),
      // Scoped listings carry `label`; unscoped `/api/workspaces` may
      // carry `fabricName`. DSH workspace title uses this field.
      fabricName: asStr(item.fabricName) || asStr(item.label),
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
  projectId;
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
    this.projectId = options.projectId ?? null;
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
    if (this.projectId === null) return `/api/${path}`;
    return `/api/projects/${encodeURIComponent(this.projectId)}/${path}`;
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
      this.fetchJson("/api/events?head=1", { method: "GET", headers: { accept: "application/json" } }, signal)
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
   * One held long-poll over the canonical event feed. The server waits until
   * events arrive or its wait bound elapses. HTTP 409 or a cursor below the
   * requested cursor is `{ kind: "stale" }`.
   */
  async poll(cursor, signal) {
    const requestedCursor = Number(cursor);
    const after = requestedCursor;
    let raw;
    try {
      raw = await this.fetchJson(
        `/api/events?after=${encodeURIComponent(String(after))}&wait=1`,
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
      const events = canonicalItemsOf(raw).filter((item) => item.globalSeq > after).flatMap(canonicalEventsOf);
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
  async loadHistory(sessionId, signal, page) {
    const maxMessages = page?.maxMessages ?? 128;
    const before = page?.beforeSeq;
    const query = new URLSearchParams();
    query.set("maxMessages", String(maxMessages));
    if (before !== void 0) query.set("beforeSeq", String(before));
    const raw = await this.fetchJson(
      `/api/conversations/${encodeURIComponent(sessionId)}/history?${query.toString()}`,
      { method: "GET", headers: { accept: "application/json" } },
      signal
    );
    const record = isRecord(raw) ? raw : {};
    const items = canonicalItemsOf(raw);
    const liveRuns = liveRunsOf(record);
    return {
      events: items.flatMap(canonicalEventsOf),
      hasMore: asBoolOr(record.hasMore, false),
      running: asBoolOr(record.running, liveRuns.length > 0),
      liveRuns
    };
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
    const key2 = mutation.intentId;
    if (this.inflight.has(key2)) {
      return { accepted: false, reason: "duplicate in-flight mutation", conversationId: void 0, runId: void 0, state: void 0, result: void 0 };
    }
    this.inflight.add(key2);
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
                ...presentId(mutation.agentId) === void 0 ? {} : { agentId: mutation.agentId },
                workspaceId: mutation.workspaceId,
                intentId: mutation.intentId
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
          const cancelRaw = await this.fetchJson(
            `/api/conversations/${encodeURIComponent(mutation.conversationId)}/cancel`,
            { method: "POST", headers: { accept: "application/json" } },
            signal
          );
          const record = isRecord(cancelRaw) ? cancelRaw : {};
          const state = asStr(record.state) ?? void 0;
          const accepted = asBoolOr(record.accepted, false) || state === "unknown" || state === "cancelled" || state === "terminal" || state === "idle";
          return {
            accepted,
            reason: accepted ? void 0 : `cancel refused (${String(state ?? "unknown")})`,
            conversationId: mutation.conversationId,
            runId: asStr(record.runId) ?? void 0,
            state,
            result: void 0
          };
        }
      }
    } finally {
      this.inflight.delete(key2);
    }
  }
};

// src/connection-voie/host-context.ts
var context = { projectId: "" };
function getVoieDshHostContext() {
  return context;
}

// src/connection-voie/api.ts
function pageSessionHistory(entries, beforeSeq, maxMessages) {
  const limit = Math.max(1, Math.floor(maxMessages));
  let end = entries.length;
  if (beforeSeq !== void 0) {
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
function numberAt(payload, key2) {
  const record = isRecord(payload) ? payload : {};
  return asNum(record[key2]) ?? void 0;
}
function stringAt(payload, key2) {
  const record = isRecord(payload) ? payload : {};
  const value = record[key2];
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
var syncWorkspaces = async () => {
};
function syncVoieWorkspaces(signal) {
  return syncWorkspaces(signal);
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
  function notifySessionsChanged() {
    if (typeof document === "undefined") return;
    document.dispatchEvent(new Event("voie-sessions-changed"));
  }
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
  const pendingReconcile = /* @__PURE__ */ new Set();
  const replayStatus = /* @__PURE__ */ new Set();
  const inflightPrompt = /* @__PURE__ */ new Map();
  let muxSink;
  let hostSink;
  function emitMux(frame) {
    pump.push(frame);
    muxSink?.({ rpcId: rpcId(), payload: frame });
  }
  function emitHost(frame) {
    hostPump.push(frame);
    hostSink?.({ rpcId: rpcId(), payload: frame });
  }
  const LIVE_RUN_STATES = { accepted: true, dispatched: true };
  function inQueueOrder(runs) {
    return [...runs].sort((a, b) => a.seq - b.seq || (a.runId < b.runId ? -1 : a.runId > b.runId ? 1 : 0));
  }
  function cancelAlreadySettled(state) {
    return state === "unknown" || state === "cancelled" || state === "terminal";
  }
  async function cancelExactRun(runId, signal) {
    const response = await runsFetchImpl(
      `/api/runs/${encodeURIComponent(runId)}/cancel`,
      {
        method: "POST",
        headers: { accept: "application/json", "x-voie-intent": "mutate", "content-type": "application/json" },
        credentials: "same-origin",
        ...signal === void 0 ? {} : { signal }
      }
    );
    if (!response.ok) {
      throw new Error(`POST /api/runs/${runId}/cancel failed: HTTP ${String(response.status)}`);
    }
    const body = await response.json();
    const record = isRecord(body) ? body : {};
    return { accepted: asBoolOr(record.accepted, false), state: asStr(record.state) ?? void 0 };
  }
  async function projectQueueSeat(sessionId, signal, preloaded) {
    if (reconciling.has(sessionId)) {
      pendingReconcile.add(sessionId);
      return;
    }
    reconciling.add(sessionId);
    try {
      let runs;
      try {
        runs = preloaded !== void 0 ? [...preloaded] : await loadConversationRuns(sessionId, signal);
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
      const previous = runningCache.get(sessionId);
      runningCache.set(sessionId, running);
      const replay = replayStatus.delete(sessionId);
      if (replay || previous !== running) {
        emitHost({ type: "host/session-status", sessionId, running });
        if (previous === true && running === false && runs.some((run) => run.state === "unknown")) {
          emitHost({
            type: "host/agent-error",
            sessionId,
            message: "The run ended without a result and will not be replayed."
          });
        }
        if (previous !== running) {
          void refreshBaseline(signal).catch(() => {
          });
        }
      }
    } finally {
      reconciling.delete(sessionId);
      if (pendingReconcile.delete(sessionId)) {
        void projectQueueSeat(sessionId, signal);
      }
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
      const data = await baseline();
      return ok({
        items: data.sessions.map((session) => {
          const summary = {
            sessionId: session.id,
            updatedAt: updatedAtOf(session),
            running: runningCache.get(session.id) ?? session.running,
            blank: false,
            cwd: `/workspaces/${session.workspaceId}`
          };
          return summary;
        })
      });
    },
    search: async () => ok({ items: [], hasMore: false }),
    create: async (payload, signal) => {
      const workspaceId = stringAt(payload, "workspaceId");
      if (workspaceId === void 0) {
        return fail("workspace-not-found", "VOIE session creation requires a workspaceId", { workspaceId: "" });
      }
      const data = await refreshBaseline(signal);
      const workspace2 = data.workspaces.find((w) => w.id === workspaceId);
      const listedProject = workspace2?.projectId ?? "";
      const projectId = listedProject !== "" ? listedProject : getVoieDshHostContext().projectId;
      if (projectId === "") {
        return fail("workspace-not-found", "workspace is not visible to this session", { workspaceId });
      }
      const requestedRaw = stringAt(payload, "agentId");
      const requestedAgent = requestedRaw !== void 0 && requestedRaw !== "" ? requestedRaw : void 0;
      const sessionId = crypto.randomUUID();
      try {
        const result = await carrier.mutate({
          op: "conversation.create",
          intentId: crypto.randomUUID(),
          conversationId: sessionId,
          projectId,
          ...requestedAgent === void 0 ? {} : { agentId: requestedAgent },
          workspaceId
        }, signal);
        if (!result.accepted) {
          return fail("internal", result.reason ?? "conversation create refused", { workspaceId });
        }
        await refreshBaseline(signal).catch(() => {
        });
        notifySessionsChanged();
        return ok({ sessionId: result.conversationId ?? sessionId });
      } catch (error) {
        return fail("internal", error instanceof Error ? error.message : "conversation create failed", { workspaceId });
      }
    },
    history: async (payload) => {
      const requested = sessionIdOf(payload);
      if (requested === "") return ok({ events: [], hasMore: false });
      const beforeSeq = numberAt(payload, "beforeSeq");
      const maxMessages = numberAt(payload, "maxMessages") ?? 50;
      const pageOpt = { maxMessages };
      if (beforeSeq !== void 0) pageOpt.beforeSeq = beforeSeq;
      const loaded = await carrier.loadHistory(requested, void 0, pageOpt);
      const mapped = inDurableOrder(loaded.events).map((event) => {
        const envelope = eventEnvelopeOf(event);
        if (envelope === null) return null;
        const view = toolViewOf(event);
        const entry = { event: envelope };
        if (view !== void 0) entry.view = view;
        return entry;
      }).filter((entry) => entry !== null);
      const page = pageSessionHistory(mapped, beforeSeq, maxMessages);
      const tailSeq = page.events.length > 0 ? page.events[page.events.length - 1]?.event.seq ?? -1 : -1;
      const projections = { asOfSeq: tailSeq, values: {} };
      replayStatus.add(requested);
      await projectQueueSeat(requested, void 0, loaded.liveRuns);
      return ok({
        events: page.events,
        hasMore: loaded.hasMore || page.hasMore,
        projections
      });
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
      const intentId = intentIdOf(payload);
      const existing = inflightPrompt.get(sessionId);
      let slot;
      if (existing !== void 0 && existing.intentId === intentId) {
        slot = existing;
      } else {
        const predecessor = existing?.work;
        const work = (async () => {
          if (predecessor !== void 0) {
            await predecessor.catch(() => void 0);
          }
          const result = await carrier.mutate({
            op: "conversation.message",
            intentId,
            conversationId: sessionId,
            prompt: text
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
        await reconcileQueues(/* @__PURE__ */ new Set([sessionId]), signal).catch(() => {
        });
        return ok({ accepted: true });
      } finally {
        const current = inflightPrompt.get(sessionId);
        if (current?.work === slot.work) inflightPrompt.delete(sessionId);
      }
    },
    cancel: async (payload, signal) => {
      const sessionId = sessionIdOf(payload);
      if (sessionId === "") return fail("session-not-found", "sessionId is required", { sessionId });
      try {
        const pending2 = inflightPrompt.get(sessionId);
        if (pending2 !== void 0) {
          await pending2.work.catch(() => void 0);
        }
        const result = await carrier.mutate({
          op: "conversation.cancel",
          intentId: crypto.randomUUID(),
          conversationId: sessionId
        }, signal);
        if (!result.accepted && !cancelAlreadySettled(result.state) && result.state !== "idle") {
          return fail("internal", result.reason ?? `cancel refused (${result.state ?? "unknown"})`, { sessionId });
        }
        await reconcileQueues(/* @__PURE__ */ new Set([sessionId]), signal).catch(() => {
        });
        return ok({ accepted: true });
      } catch (error) {
        return fail("internal", error instanceof Error ? error.message : "cancel failed", { sessionId });
      }
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
          const cancelled = await cancelExactRun(itemId, signal);
          if (cancelled.accepted || cancelAlreadySettled(cancelled.state)) return ok({ accepted: true });
          return fail("cancel-refused", `cancel refused (${cancelled.state ?? "unknown"})`, { runId: itemId });
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
      title: workspace2.fabricName?.trim() || workspace2.id,
      sessionIds,
      createdAt,
      updatedAt: createdAt,
      state: workspace2.state
    };
  });
  const hostWorkspaceView = (view) => ({
    workspaceId: view.workspaceId,
    path: view.path,
    title: view.title,
    sessionIds: view.sessionIds,
    createdAt: view.createdAt,
    updatedAt: view.updatedAt
  });
  const publishWorkspaceViews = (data) => {
    for (const view of workspaceViews(data)) {
      emitHost({ type: "host/workspace-changed", workspace: hostWorkspaceView(view) });
    }
  };
  syncWorkspaces = async (signal) => {
    const data = await refreshBaseline(signal);
    publishWorkspaceViews(data);
  };
  const workspace = {
    list: async () => {
      const data = await refreshBaseline();
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
    reconcileQueues,
    setSinks(next) {
      muxSink = next.onMuxEnvelope;
      hostSink = next.onHostEnvelope;
    }
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
      built.setSinks(sinks);
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
            void built.reconcileQueues(touched, ac.signal).catch(() => {
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

// src/connection-voie/brand-mark.tsx
var import_jsx_runtime = require("react/jsx-runtime");
var markStyle = (size) => ({
  background: "var(--kds-primary, #2563eb)",
  borderRadius: Math.max(3, Math.round(size * 0.12)),
  display: "block",
  flex: "0 0 auto",
  height: size,
  width: size
});
function VoieBrandMark({ size }) {
  return /* @__PURE__ */ (0, import_jsx_runtime.jsx)("span", { "aria-hidden": "true", "data-voie-brand-mark": "", style: markStyle(size) });
}

// src/connection-voie/conversation-frame.tsx
var import_react = require("react");
var import_jsx_runtime2 = require("react/jsx-runtime");
function assignPaneGeometry(pane) {
  const width = pane.clientWidth;
  if (width <= 0) return;
  const contentPx = Math.max(0, width - 48);
  const composerPx = Math.max(0, width - 24);
  const root = pane.querySelector("[data-phase]");
  if (root === null) return;
  const previous = Number.parseInt(root.style.getPropertyValue("--dsh-chat-content-width"), 10);
  if (Number.isFinite(previous) && Math.abs(previous - contentPx) < 8) return;
  root.style.setProperty("--dsh-chat-content-width", `${String(contentPx)}px`);
  root.style.setProperty("--dsh-composer-card-max-width", `${String(composerPx)}px`);
}
function VoieConversationPane({
  useStore,
  useSessions,
  actions,
  renderSlot
}) {
  const details = useStore((state) => state.details);
  const detailsSession = useSessions((state) => {
    const current = state.current;
    return current !== void 0 && state.byId[current]?.blank === false ? current : void 0;
  });
  const open = details > 0 && detailsSession !== void 0;
  const lastSession = (0, import_react.useRef)(detailsSession);
  const paneRef = (0, import_react.useRef)(null);
  const stageRef = (0, import_react.useRef)(null);
  (0, import_react.useLayoutEffect)(() => {
    if (detailsSession === void 0) return;
    if (lastSession.current !== void 0 && lastSession.current !== detailsSession) {
      actions.closeDetails();
    }
    lastSession.current = detailsSession;
  }, [actions, detailsSession]);
  (0, import_react.useLayoutEffect)(() => {
    const pane = paneRef.current;
    const stage = stageRef.current;
    if (pane === null) return;
    let raf = 0;
    const schedule = () => {
      if (raf !== 0) return;
      raf = requestAnimationFrame(() => {
        raf = 0;
        assignPaneGeometry(pane);
      });
    };
    schedule();
    const resize = new ResizeObserver(schedule);
    resize.observe(pane);
    const mount = new MutationObserver(schedule);
    mount.observe(stage ?? pane, { childList: true });
    return () => {
      if (raf !== 0) cancelAnimationFrame(raf);
      resize.disconnect();
      mount.disconnect();
    };
  }, []);
  return /* @__PURE__ */ (0, import_jsx_runtime2.jsxs)(
    "div",
    {
      ref: paneRef,
      className: "voie-conversation-pane",
      "data-details-open": open ? "true" : void 0,
      children: [
        /* @__PURE__ */ (0, import_jsx_runtime2.jsx)("div", { ref: stageRef, className: "voie-conversation-pane__stage", children: renderSlot("conversation", {}) }),
        /* @__PURE__ */ (0, import_jsx_runtime2.jsx)("div", { className: "voie-conversation-pane__details", children: renderSlot("details", {}) }),
        /* @__PURE__ */ (0, import_jsx_runtime2.jsx)("div", { className: "voie-conversation-pane__overlay", "data-shell-overlay": "", children: renderSlot("shell.overlay", {}) })
      ]
    }
  );
}

// src/connection-voie/last-workspace.ts
var memory = /* @__PURE__ */ new Map();
function key(projectId) {
  return `voie:lastWorkspace:${projectId}`;
}
function browserStore(name) {
  try {
    const value = globalThis[name];
    return value ?? null;
  } catch {
    return null;
  }
}
function readStore(store, projectId) {
  try {
    return store.getItem(key(projectId))?.trim() ?? "";
  } catch {
    return "";
  }
}
function lastWorkspace(projectId) {
  const scope = projectId.trim();
  if (scope === "") return "";
  const fromMemory = memory.get(scope);
  if (fromMemory !== void 0 && fromMemory !== "") return fromMemory;
  const session = browserStore("sessionStorage");
  const fromSession = session === null ? "" : readStore(session, scope);
  if (fromSession !== "") {
    memory.set(scope, fromSession);
    return fromSession;
  }
  const local = browserStore("localStorage");
  const fromLocal = local === null ? "" : readStore(local, scope);
  if (fromLocal !== "") {
    memory.set(scope, fromLocal);
    return fromLocal;
  }
  return "";
}

// src/connection-voie/hero-workspace.tsx
var import_react2 = require("react");
var import_jsx_runtime3 = require("react/jsx-runtime");
function newestReadyId(items) {
  const ready = items.filter((item) => item.state === "ready" || item.state === void 0);
  const pool = ready.length > 0 ? ready : items;
  const sorted = [...pool].sort(
    (left, right) => (right.createdAt ?? "").localeCompare(left.createdAt ?? "")
  );
  return sorted[0]?.workspaceId;
}
function labelOf(item) {
  const title = item.title.trim();
  return title === "" ? item.workspaceId.slice(0, 8) : title;
}
function VoieHeroWorkspace({
  open,
  selectedId,
  onPick,
  onClose,
  useWorkspaces
}) {
  const view = useWorkspaces((state) => ({
    items: state.items,
    phase: state.phase,
    recentWorkspaceId: state.recentWorkspaceId
  }));
  const tried = (0, import_react2.useRef)(void 0);
  const menu = (0, import_react2.useRef)(null);
  (0, import_react2.useEffect)(() => {
    if (selectedId !== void 0 && selectedId !== "") {
      tried.current = selectedId;
      return;
    }
    if (view.phase !== "ready") return;
    const preferred = lastWorkspace(getVoieDshHostContext().projectId) || getVoieDshHostContext().workspaceId || "";
    if (preferred !== "") {
      tried.current = preferred;
      return;
    }
    const target = newestReadyId(view.items) ?? view.recentWorkspaceId;
    if (target === void 0 || tried.current === target) return;
    tried.current = target;
    onPick(target);
  }, [onPick, selectedId, view.items, view.phase, view.recentWorkspaceId]);
  (0, import_react2.useEffect)(() => {
    if (!open) return;
    const onDoc = (event) => {
      const node = event.target;
      if (!(node instanceof Node)) return;
      if (menu.current?.contains(node)) return;
      onClose();
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [open, onClose]);
  if (!open) return null;
  const pick = (workspaceId) => (event) => {
    event.preventDefault();
    event.stopPropagation();
    onPick(workspaceId);
  };
  return /* @__PURE__ */ (0, import_jsx_runtime3.jsx)("div", { ref: menu, role: "menu", "data-voie-workspace-picker": "", "aria-label": "Workspaces", children: view.items.length === 0 ? /* @__PURE__ */ (0, import_jsx_runtime3.jsx)("p", { children: view.phase === "ready" ? "No workspaces yet." : "Loading workspaces\u2026" }) : view.items.map((item) => {
    const selected = item.workspaceId === selectedId;
    return /* @__PURE__ */ (0, import_jsx_runtime3.jsx)(
      "button",
      {
        type: "button",
        role: "menuitem",
        "data-workspace-id": item.workspaceId,
        "aria-current": selected ? "true" : void 0,
        onClick: pick(item.workspaceId),
        children: labelOf(item)
      },
      item.workspaceId
    );
  }) });
}

// src/connection-voie/layout.ts
var import_client = require("@deepseek-ai/dsh-client-runtime/client");
function createVoieLayoutStore() {
  return (0, import_client.defineStore)({
    init: () => ({ details: 0 }),
    actions: {
      openDetails: (draft) => {
        if (draft.details === 0) draft.details = 380;
      },
      closeDetails: (draft) => {
        draft.details = 0;
      },
      toggleSidebar: () => {
      }
    }
  });
}
var VoieLayoutController = class {
  #panels;
  attachPanels(actions) {
    this.#panels = actions;
  }
  toggleSidebar() {
    this.#require().toggleSidebar();
  }
  openDetails() {
    this.#require().openDetails();
  }
  closeDetails() {
    this.#require().closeDetails();
  }
  #require() {
    if (this.#panels === void 0) {
      throw new Error("layout: panel actions not wired (root entry not mounted)");
    }
    return this.#panels;
  }
};

// src/connection-voie/new-chat.ts
var VOIE_NEW_CHAT_EVENT = "voie-new-chat";
var starter = null;
var listening = false;
function workspaceIdFromEvent(event) {
  const detail = event.detail;
  if (typeof detail?.workspaceId !== "string") return "";
  return detail.workspaceId.trim();
}
function resolveWorkspaceId(event) {
  const fromEvent = workspaceIdFromEvent(event);
  if (fromEvent !== "") return fromEvent;
  const ctx = getVoieDshHostContext();
  const fromStorage = lastWorkspace(ctx.projectId);
  if (fromStorage !== "") return fromStorage;
  return ctx.workspaceId?.trim() ?? "";
}
function onNewChat(event) {
  const startSession = starter;
  if (startSession === null) return;
  const workspaceId = resolveWorkspaceId(event);
  window.setTimeout(() => {
    if (starter === null) return;
    try {
      if (workspaceId !== "") starter(workspaceId);
      else starter();
    } catch {
    }
  }, 0);
}
function bindVoieNewChatListener(startSession) {
  starter = startSession;
  if (listening) return;
  window.addEventListener(VOIE_NEW_CHAT_EVENT, onNewChat);
  listening = true;
}

// src/connection-voie/session-nav.ts
var VOIE_OPEN_CONVERSATION_EVENT = "voie-open-conversation";
var nav = null;
var listening2 = false;
var pending;
var wanted;
var opening = false;
function conversationIdFromHost() {
  return getVoieDshHostContext().conversationId;
}
function conversationIdFromEvent(event) {
  const detail = event.detail;
  if (typeof detail?.conversationId !== "string") return void 0;
  const id = detail.conversationId.trim();
  return id === "" ? void 0 : id;
}
function cancelPending() {
  pending?.();
  pending = void 0;
}
function openWhenListed(sessions, id) {
  wanted = id;
  cancelPending();
  const tryOpen = () => {
    if (wanted !== id) return true;
    const snap = sessions.list.getSnapshot();
    if (snap.byId[id] === void 0) return false;
    if (snap.current === id) {
      cancelPending();
      return true;
    }
    if (opening) return true;
    opening = true;
    try {
      sessions.open(id);
    } finally {
      opening = false;
    }
    cancelPending();
    return true;
  };
  if (tryOpen()) return;
  const unsubscribe = sessions.list.subscribe(() => {
    tryOpen();
  });
  pending = unsubscribe;
}
function onOpen(event) {
  const sessions = nav;
  const id = conversationIdFromEvent(event);
  if (sessions === null || id === void 0) return;
  openWhenListed(sessions, id);
}
function syncFromHost() {
  const sessions = nav;
  if (sessions === null) return;
  const id = conversationIdFromHost();
  if (id === void 0) {
    cancelPending();
    wanted = void 0;
    return;
  }
  openWhenListed(sessions, id);
}
function bindVoieSessionNav(sessions) {
  nav = sessions;
  if (!listening2) {
    window.addEventListener(VOIE_OPEN_CONVERSATION_EVENT, onOpen);
    listening2 = true;
  }
  syncFromHost();
}

// src/connection-voie/plugin-fn.ts
function mountScopedCarrier() {
  const projectId = getVoieDshHostContext().projectId;
  return new VoieCarrier(projectId === "" ? {} : { projectId });
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
function createVoieSettingsScope() {
  const snapshot = {
    status: "unavailable",
    value: void 0,
    base: void 0,
    user: void 0,
    revision: void 0,
    writable: false,
    mode: "memory"
  };
  return {
    bind(_spec) {
      return {
        getSnapshot: () => snapshot,
        subscribe: () => () => {
        },
        set: async () => {
        },
        unset: async () => {
        }
      };
    }
  };
}
function createVoieTheme() {
  const snapshot = {
    active: {
      colorScheme: "light",
      tokens: {}
    }
  };
  return {
    getTheme: () => snapshot
  };
}
function apply(ctx) {
  ctx.provide("connection", createConnectionHandle(mountScopedCarrier()));
  ctx.provide("remote", createVoieRemote());
  ctx.provide("remote.commands", createVoieRemoteCommands());
  ctx.provide("settingsScope", createVoieSettingsScope());
  ctx.provide("theme", createVoieTheme());
  const layout = new VoieLayoutController();
  ctx.provide("layout", layout);
  ctx.inject(["slots"], (slotCtx) => {
    slotCtx.slots?.register(
      {
        name: "root",
        children: {
          conversation: { kind: "single", scope: "session-maybe" },
          details: { kind: "single", scope: "session" },
          "shell.overlay": { kind: "list", scope: "root" }
        },
        store: createVoieLayoutStore,
        inject: (actions) => {
          layout.attachPanels(actions);
          return {};
        }
      },
      VoieConversationPane
    );
    slotCtx.slots?.inject(
      "conversation.hero.workspace",
      () => slotCtx.slots?.register({ name: "conversation.hero.workspace" }, VoieHeroWorkspace)
    );
    slotCtx.slots?.inject(
      "conversation.hero.brand.mark",
      () => slotCtx.slots?.register({ name: "conversation.hero.brand.mark" }, VoieBrandMark)
    );
  });
  ctx.inject(["workspaces", "sessions"], (navCtx) => {
    bindVoieNewChatListener((workspaceId) => {
      void (async () => {
        const projectId = getVoieDshHostContext().projectId;
        const target = (workspaceId?.trim() || lastWorkspace(projectId) || getVoieDshHostContext().workspaceId || "").trim();
        if (target === "") return;
        await syncVoieWorkspaces().catch(() => {
        });
        navCtx.sessions?.clear();
        const create = navCtx.sessions?.create;
        if (create === void 0) return;
        try {
          const id = await create({ workspaceId: target });
          navCtx.sessions?.open(id);
        } catch {
        }
      })();
    });
    if (navCtx.sessions !== void 0) bindVoieSessionNav(navCtx.sessions);
  });
}

return module.exports;
}});

