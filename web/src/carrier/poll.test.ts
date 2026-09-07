/**
 * Behavioral tests for `VoieCarrier.poll()` and the carrier mutation seam,
 * run against a scripted fake HTTP layer and an injected manual clock.
 *
 * Runner (matches repo tooling — Node inside the dev flake; the package has
 * neither vitest nor @types/node, so this is a dependency-free runner):
 *
 *   cd web && nix develop -c bash -c \
 *     "node --experimental-strip-types src/carrier/poll.test.ts"
 *
 * Project rule compliance (`ts-no-test-timers`): no wall-clock sleeps and no
 * test-side setTimeout anywhere. Time lives behind the production
 * `VoieCarrierSchedulers` seam; the cases drive the manual clock explicitly.
 */
import { VoieCarrier, type VoieCarrierSchedulers } from "./voie.ts";
import type { PollResult } from "./types.ts";
import { createCarrierApi } from "../connection-voie/api.ts";

// ------------------------------------------------------------------ harness

type Case = { name: string; run: () => Promise<void> | void };

const cases: Case[] = [];

/** Registers one behavioral case; executed sequentially by the runner below. */
function test(name: string, run: () => Promise<void> | void): void {
  cases.push({ name, run });
}

/** Structural failure used by `eq`. */
function fail(message: string): never {
  throw new Error(message);
}

/** Deep structural equality with undefined-valued keys treated as absent. */
function eq(actual: unknown, expected: unknown, label: string): void {
  const same = (left: unknown, right: unknown): boolean => {
    if (Object.is(left, right)) return true;
    if (Array.isArray(left) && Array.isArray(right)) {
      return left.length === right.length && left.every((value, index) => same(value, right[index]));
    }
    if (
      typeof left === "object" &&
      left !== null &&
      !Array.isArray(left) &&
      typeof right === "object" &&
      right !== null &&
      !Array.isArray(right)
    ) {
      const leftEntries = Object.entries(left as Record<string, unknown>).filter(([, v]) => v !== undefined);
      const rightRecord = right as Record<string, unknown>;
      const rightEntries = Object.entries(rightRecord).filter(([, v]) => v !== undefined);
      return (
        leftEntries.length === rightEntries.length &&
        leftEntries.every(([key, value]) => key in rightRecord && same(value, rightRecord[key]))
      );
    }
    return false;
  };
  if (!same(actual, expected)) {
    fail(`${label}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

type ManualClock = VoieCarrierSchedulers & { tick(ms: number): void; pendingCount(): number };

/** Deterministic scheduler: pauses are explicit queue entries, released by `tick`. */
function manualClock(): ManualClock {
  let now = 0;
  let nextHandle = 1;
  const paused = new Map<number, () => void>();
  return {
    schedule(delayMs: number, run: () => void): number {
      void delayMs; // coarse pacing only; the case controls release timing
      const handle = nextHandle;
      nextHandle += 1;
      paused.set(handle, run);
      return handle;
    },
    clear(handle: number): void {
      paused.delete(handle);
    },
    now: (): number => now,
    tick(ms: number): void {
      now += ms;
      const due = [...paused.values()];
      paused.clear();
      for (const run of due) run();
    },
    pendingCount: (): number => paused.size,
  };
}

/** Lets the polled chain settle its synchronous microtask segments. */
async function drainTasks(slots = 48): Promise<void> {
  for (let slot = 0; slot < slots; slot += 1) await Promise.resolve();
}

type FetchCall = {
  url: string;
  signal: AbortSignal | null | undefined;
  headers: Record<string, unknown>;
  body?: string;
};

type ScriptPage =
  | { kind: "json"; status: number; body: unknown }
  | { kind: "expect-aborted" };

/** Deterministic fetch double: pops scripted pages, records every request. */
function scriptFetch(pages: ScriptPage[], calls: FetchCall[]): typeof fetch {
  return ((url: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const call: FetchCall = {
      url: String(url),
      signal: init?.signal,
      headers: { ...(init?.headers as Record<string, unknown> | undefined) },
      ...(typeof init?.body === "string" ? { body: init.body } : {}),
    };
    calls.push(call);
    const page = pages.shift();
    if (page === undefined) {
      throw new Error(`unexpected fetch #${calls.length}: ${call.url}`);
    }
    if (page.kind === "expect-aborted") {
      const aborted = call.signal?.aborted === true;
      return Promise.reject(new Error(aborted ? "aborted before send" : "page expected an aborted signal"));
    }
    const response = {
      ok: page.status >= 200 && page.status < 300,
      status: page.status,
      json: async (): Promise<unknown> => page.body,
    };
    return Promise.resolve(response as unknown as Response);
  }) as unknown as typeof fetch;
}

/** One canonical append batch exactly as the feed serializes it. */
function appendBatch(
  sessionId: string,
  globalSeq: number,
  revision: number,
  lines: Array<{ type: string; seq?: number; time?: number; data?: unknown }>,
): Record<string, unknown> {
  const bytes = btoa(lines.map((line) => JSON.stringify(line)).join("\n"));
  return {
    sessionId,
    globalSeq,
    revision,
    appendId: `append-${globalSeq}`,
    objectKey: `objects/${globalSeq}`,
    contentHash: "a".repeat(64),
    byteLength: 128,
    bytes,
  };
}

const SESSION_A = "11111111-1111-4111-8111-111111111111";

/**
 * Drives a running `poll()` under the manual clock until it settles or the
 * bounded tick budget is spent; overspend surfaces below as a failed await.
 */
async function pump(
  pollPromise: Promise<unknown>,
  clock: ManualClock,
  maxTicks: number,
  tickMs: number,
): Promise<unknown> {
  let settled = false;
  const observed = pollPromise.then(
    (value) => {
      settled = true;
      return value;
    },
    (reason) => {
      settled = true;
      throw reason;
    },
  );
  for (let tick = 0; tick < maxTicks && !settled; tick += 1) {
    clock.tick(tickMs);
    await drainTasks();
  }
  return observed;
}

// -------------------------------------------------------------------- cases

test("poll decodes one canonical batch preserving identity and order", async () => {
  const calls: FetchCall[] = [];
  const carrier = new VoieCarrier({
    fetchImpl: scriptFetch(
      [
        {
          kind: "json",
          status: 200,
          body: {
            items: [
              appendBatch(SESSION_A, 41, 7, [
                { type: "user/message", seq: 11, time: 1717171717000, data: { text: "hello" } },
                // Producer omitted seq/time on this line: they must stay null.
                { type: "assistant/message", data: { text: "hi" } },
              ]),
              // Already-consumed rows at/below the requested cursor never
              // re-cross the seam.
              appendBatch("22222222-2222-4222-8222-222222222222", 39, 2, [
                { type: "user/message", seq: 1, time: 1000 },
              ]),
            ],
            cursor: 41,
          },
        },
      ],
      calls,
    ),
    holdMs: 5_000,
    intervalMs: 5,
  });

  const result = await carrier.poll("40");

  eq(calls.map((call) => call.url), ["/api/events?after=40&wait=1"], "single held long-poll");
  eq(result.kind, "events", "result kind");
  if (result.kind !== "events") return fail("unreachable");
  eq(result.cursor, "41", "returned cursor");
  eq(result.events.length, 2, "decoded count skips the stale row");

  const first = result.events[0];
  if (first === undefined) return fail("first event missing");
  eq(first.sessionId, SESSION_A, "sessionId");
  eq(first.type, "user/message", "line type");
  eq(first.data, { text: "hello" }, "producer payload verbatim");
  eq(first.globalSeq, 41, "globalSeq");
  eq(first.revision, 7, "revision");
  eq(first.eventIndex, 0, "eventIndex");
  eq(first.appendId, "append-41", "appendId");
  eq(first.objectKey, "objects/41", "objectKey");
  eq(first.contentHash, "a".repeat(64), "contentHash");
  eq(first.byteLength, 128, "byteLength");
  eq(first.seq, 11, "producer seq");
  eq(first.time, 1717171717000, "producer time");
  eq(first.surfaceOp, undefined, "absent surfaceOp stays undefined");
  eq(first.sourceEventSeqs, undefined, "absent citations stay undefined");

  const second = result.events[1];
  if (second === undefined) return fail("second event missing");
  eq(second.type, "assistant/message", "second line type");
  eq(second.eventIndex, 1, "second line position");
  eq(second.seq, null, "omitted producer seq decodes to null");
  eq(second.time, null, "omitted producer time decodes to null");
});

test("poll wait=1 returns empty events without a client re-read loop", async () => {
  const calls: FetchCall[] = [];
  const carrier = new VoieCarrier({
    fetchImpl: scriptFetch(
      [{ kind: "json", status: 200, body: { items: [], cursor: 44 } }],
      calls,
    ),
    holdMs: 30_000,
    intervalMs: 10,
  });

  const result = await carrier.poll("40");

  eq(calls.map((call) => call.url), ["/api/events?after=40&wait=1"], "one held request");
  eq(result.kind, "events", "result kind");
  if (result.kind !== "events") return fail("unreachable");
  eq(result.cursor, "44", "cursor advances from the empty wait");
  eq(result.events.length, 0, "empty wait invents no events");
});

test("poll reports a stale server cursor without decoding regressions", async () => {
  const calls: FetchCall[] = [];
  const carrier = new VoieCarrier({
    fetchImpl: scriptFetch(
      [
        {
          kind: "json",
          status: 200,
          body: {
            items: [appendBatch(SESSION_A, 49, 3, [{ type: "user/message", seq: 1, time: 1 }])],
            cursor: 49,
          },
        },
      ],
      calls,
    ),
    holdMs: 30_000,
    intervalMs: 2,
  });

  const result = await carrier.poll("50");

  eq(calls.map((call) => call.url), ["/api/events?after=50&wait=1"], "requested cursor echoed");
  eq(result, { kind: "stale" }, "regressed feed reported as stale");
});

test("poll abort during the held request unwinds promptly", async () => {
  const calls: FetchCall[] = [];
  const controller = new AbortController();
  const carrier = new VoieCarrier({
    fetchImpl: scriptFetch([{ kind: "expect-aborted" }], calls),
    holdMs: 30_000,
    intervalMs: 5,
  });

  controller.abort();
  let rejection: unknown;
  try {
    await carrier.poll("40", controller.signal);
  } catch (reason) {
    rejection = reason;
  }
  const message = rejection instanceof Error ? rejection.message : String(rejection ?? "");
  if (!message.includes("aborted before send")) {
    fail(`abort must unwind promptly with the abort failure, got: ${rejection === undefined ? "resolution" : message}`);
  }
});

test("mutations ride the admission gate markers from the central seam", async () => {
  const calls: FetchCall[] = [];
  const carrier = new VoieCarrier({
    fetchImpl: scriptFetch(
      [
        {
          kind: "json",
          status: 200,
          body: {
            accepted: true,
            conversationId: SESSION_A,
            runId: "33333333-3333-4333-8333-333333333333",
            state: "accepted",
          },
        },
      ],
      calls,
    ),
    holdMs: 30_000,
    intervalMs: 2,
  });

  const result = await carrier.mutate({
    op: "conversation.create",
    intentId: "44444444-4444-4444-8444-444444444444",
    projectId: "55555555-5555-4555-8555-555555555555",
    agentId: "66666666-6666-4666-8666-666666666666",
    workspaceId: "77777777-7777-4777-8777-777777777777",
  });

  eq(result.accepted, true, "mutation accepted");
  const post = calls[0];
  if (post === undefined) return fail("mutation request missing");
  if (!post.url.endsWith("/api/conversations")) return fail(`unexpected path ${post.url}`);
  eq(post.headers["x-voie-intent"], "mutate", "intent marker enforced centrally");
  eq(post.headers["content-type"], "application/json", "json content-type present");
  eq(post.headers["accept"], "application/json", "accept preserved");
});

test("project workspace listings map projectId", async () => {
  const projectId = "81b1d0ec-943f-4416-adee-955b54103b39";
  const workspaceId = "14b13978-e58b-4bcf-8dad-0b59f3de0ce6";
  const agentId = "dca589bc-3d9a-457b-b86c-6e5f07249666";
  const calls: FetchCall[] = [];
  const carrier = new VoieCarrier({
    projectId,
    fetchImpl: scriptFetch(
      [
        { kind: "json", status: 200, body: { items: [] } },
        { kind: "json", status: 200, body: { items: [{ id: agentId, projectId, name: "Default" }] } },
        {
          kind: "json",
          status: 200,
          body: { items: [{ id: workspaceId, projectId, state: "ready", label: "Smoke lab" }] },
        },
        { kind: "json", status: 200, body: { items: [], cursor: 0 } },
      ],
      calls,
    ),
    holdMs: 30_000,
    intervalMs: 2,
  });
  const baseline = await carrier.loadBaseline();
  eq(baseline.workspaces.length, 1, "one project workspace");
  eq(baseline.workspaces[0]?.id, workspaceId, "workspace id preserved");
  eq(baseline.workspaces[0]?.projectId, projectId, "projectId is the conversation projectId");
  eq(baseline.workspaces[0]?.fabricName, "Smoke lab", "project label is the workspace title");
});

test("conversation create omits a blank agentId", async () => {
  const calls: FetchCall[] = [];
  const carrier = new VoieCarrier({
    fetchImpl: scriptFetch(
      [
        {
          kind: "json",
          status: 200,
          body: {
            accepted: true,
            conversationId: SESSION_A,
            runId: "33333333-3333-4333-8333-333333333333",
            state: "accepted",
          },
        },
      ],
      calls,
    ),
    holdMs: 30_000,
    intervalMs: 2,
  });
  await carrier.mutate({
    op: "conversation.create",
    intentId: "44444444-4444-4444-8444-444444444444",
    projectId: "55555555-5555-4555-8555-555555555555",
    agentId: "",
    workspaceId: "77777777-7777-4777-8777-777777777777",
  });
  const post = calls[0];
  if (post === undefined) return fail("mutation request missing");
  const body = JSON.parse(post.body ?? "{}") as Record<string, unknown>;
  eq(Object.hasOwn(body, "agentId"), false, "blank agentId is omitted");
  eq(Object.hasOwn(body, "conversationId"), false, "client does not mint Session identity");
  eq(body.projectId, "55555555-5555-4555-8555-555555555555", "projectId still rides");
});

test("bodyless cancel still carries both admission markers", async () => {
  const calls: FetchCall[] = [];
  const carrier = new VoieCarrier({
    fetchImpl: scriptFetch(
      [
        {
          kind: "json",
          status: 200,
          body: { accepted: true, state: "cancelling" },
        },
      ],
      calls,
    ),
    holdMs: 30_000,
    intervalMs: 2,
  });

  const result = await carrier.mutate({
    op: "conversation.cancel",
    intentId: "99999999-9999-4999-8999-999999999999",
    conversationId: SESSION_A,
  });

  eq(result.accepted, true, "cancel accepted");
  eq(calls.length, 1, "one conversation cancel post");
  const cancelPost = calls[0];
  if (cancelPost === undefined) return fail("cancel request missing");
  if (!cancelPost.url.endsWith(`/api/conversations/${SESSION_A}/cancel`)) {
    return fail(`unexpected path ${cancelPost.url}`);
  }
  eq(cancelPost.headers["x-voie-intent"], "mutate", "intent marker on bodyless post");
  eq(cancelPost.headers["content-type"], "application/json", "json content-type without body");
});

// --------------------------------------------------- queue-seat face cases
//
// These drive `createCarrierApi` through its injected runs-fetch
// seam (scripted pages), asserting the queue seat is projected from durable
// truth, that cancels target the exact run id, that settlement reconciles,
// and that steering stays honestly refused.

const SESSION_B = "22222222-2222-4222-8222-222222222222";
const RUN_QUEUED = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa1";
const RUN_ACTIVE = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa2";
const RUN_TERMINAL = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa3";

type FaceCarrier = {
  loadBaseline(signal?: AbortSignal): Promise<{ cursor: string; sessions: Array<{ id: string; running: boolean }>; agents: unknown[]; workspaces: unknown[] }>;
  poll(cursor: string, signal?: AbortSignal): Promise<PollResult>;
  loadHistory(
    sessionId: string,
    signal?: AbortSignal,
    page?: { beforeSeq?: number; maxMessages?: number },
  ): Promise<{ events: unknown[]; hasMore: boolean }>;
  mutate(mutation: unknown, signal?: AbortSignal): Promise<unknown>;
};

/** Minimal carrier stub: baseline carries the running flags tests need. */
function stubCarrier(runningSessions: string[] = []): FaceCarrier {
  return {
    loadBaseline: async () => ({
      cursor: "0",
      sessions: runningSessions.map((id) => ({ id, running: true })),
      agents: [],
      workspaces: [],
    }),
    poll: async () => ({ kind: "events", cursor: "0", events: [] }) as PollResult,
    loadHistory: async () => ({ events: [], hasMore: false }),
    mutate: async () => ({ accepted: false }),
  };
}

function runsBody(rows: Array<Record<string, unknown>>): { kind: "json"; status: number; body: { runs: Array<Record<string, unknown>> } } {
  return { kind: "json", status: 200, body: { runs: rows } };
}

/** Path-keyed conversation runs + cancel so prompt/Stop cannot race scripted pages. */
function conversationRunsFetch(
  calls: FetchCall[],
  live: Array<Record<string, unknown>>,
  cancelBody: Record<string, unknown> = { accepted: true, state: "cancelling" },
): typeof fetch {
  return ((url: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const href = String(url);
    const call: FetchCall = {
      url: href,
      signal: init?.signal,
      headers: { ...(init?.headers as Record<string, unknown> | undefined) },
      ...(typeof init?.body === "string" ? { body: init.body } : {}),
    };
    calls.push(call);
    if (href.includes("/cancel") && (init?.method ?? "GET") === "POST") {
      const runId = href.split("/api/runs/")[1]?.split("/cancel")[0] ?? "";
      return Promise.resolve({
        ok: true,
        status: 200,
        json: async () => ({ runId, ...cancelBody }),
      } as Response);
    }
    if (href.includes("/api/conversations/") && href.endsWith("/runs")) {
      return Promise.resolve({
        ok: true,
        status: 200,
        json: async () => ({ runs: live }),
      } as Response);
    }
    throw new Error(`unexpected fetch: ${href}`);
  }) as unknown as typeof fetch;
}

function isSessionStatus<T extends { type: string }>(
  frame: T,
): frame is T & { type: "host/session-status"; running: boolean } {
  return frame.type === "host/session-status";
}

function isAgentError<T extends { type: string }>(
  frame: T,
): frame is T & { type: "host/agent-error"; message: string } {
  return frame.type === "host/agent-error";
}

test("queue projection rides durable conversation runs into queued dock rows", async () => {
  const calls: FetchCall[] = [];
  const built = createCarrierApi(stubCarrier([SESSION_B]) as never, {
    fetchImpl: scriptFetch(
      [
        runsBody([
          { runId: RUN_ACTIVE, seq: 1, state: "dispatched", prompt: "first", actorUserId: "u1" },
          { runId: RUN_QUEUED, seq: 2, state: "accepted", prompt: "second", actorUserId: "u1" },
          { runId: RUN_TERMINAL, seq: 3, state: "completed", prompt: "done", actorUserId: "u1" },
        ]),
      ],
      calls,
    ),
  });
  await built.reconcileQueues(undefined, undefined);
  const get = calls[0];
  if (get === undefined) return fail("runs fetch missing");
  if (!get.url.endsWith(`/api/conversations/${SESSION_B}/runs`)) return fail(`unexpected path ${get.url}`);
  eq(get.headers["accept"], "application/json", "accept preserved on runs read");
  const queueFrames = built.pump.buffer.filter((frame) => frame.type === "session/queue");
  if (queueFrames.length !== 1) return fail(`expected 1 queue frame, got ${queueFrames.length}`);
  const frame = queueFrames[0];
  if (frame === undefined || frame.type !== "session/queue") return fail("queue frame missing");
  // accepted=queued, dispatched=active: only the accepted run is a dock row.
  eq(frame.items.length, 1, "only accepted rows are dock items");
  eq(frame.items[0]?.id, RUN_QUEUED, "dock row id is the runId");
  eq(frame.items[0]?.placement, "queued", "accepted maps to queued placement");
  eq(frame.items[0]?.message.content, [{ type: "text", text: "second" }], "prompt projects into the message text block");
  const hostFrames = built.hostPump.buffer.filter(isSessionStatus);
  eq(hostFrames.length, 1, "one running-status frame emitted");
  eq(hostFrames[0]?.running, true, "dispatched run marks the session active");
});

test("queue projection is delivered on the live mux sink DSH actually consumes", async () => {
  const mux: Array<{ type?: string; items?: unknown[] }> = [];
  const built = createCarrierApi(stubCarrier([SESSION_B]) as never, {
    fetchImpl: scriptFetch(
      [
        runsBody([
          { runId: RUN_ACTIVE, seq: 1, state: "dispatched", prompt: "first", actorUserId: "u1" },
          { runId: RUN_QUEUED, seq: 2, state: "accepted", prompt: "Follow-up queued", actorUserId: "u1" },
        ]),
      ],
      [],
    ),
  });
  built.setSinks({
    onMuxEnvelope: (envelope) => {
      mux.push(envelope.payload as { type?: string; items?: unknown[] });
    },
  });
  await built.reconcileQueues(undefined, undefined);
  const queue = mux.filter((frame) => frame.type === "session/queue");
  eq(queue.length, 1, "live mux sink receives the session/queue frame");
  eq((queue[0]?.items ?? []).length, 1, "live sink carries the accepted follow-up row");
});

test("queue remove cancels the item's own run, never the first active", async () => {
  const calls: FetchCall[] = [];
  const built = createCarrierApi(stubCarrier() as never, {
    fetchImpl: scriptFetch(
      [
        runsBody([
          { runId: RUN_ACTIVE, seq: 1, state: "accepted", prompt: "first", actorUserId: "u1" },
          { runId: RUN_QUEUED, seq: 2, state: "accepted", prompt: "second", actorUserId: "u1" },
        ]),
        { kind: "json", status: 200, body: { accepted: true, state: "cancelling" } },
      ],
      calls,
    ),
  });
  const response = await built.api.sessions.updateQueue({
    sessionId: SESSION_B,
    itemId: RUN_QUEUED,
    action: { kind: "remove" },
  });
  if (!response.result.ok || response.result.value.accepted !== true) return fail("remove not accepted");
  eq(calls.length, 2, "resolve plus run-scoped cancel");
  const cancelPost = calls[1];
  if (cancelPost === undefined) return fail("cancel request missing");
  if (!cancelPost.url.endsWith(`/api/runs/${RUN_QUEUED}/cancel`)) return fail(`cancel hit the wrong run: ${cancelPost.url}`);
  eq(cancelPost.headers["x-voie-intent"], "mutate", "intent marker on run cancel");
  eq(cancelPost.headers["content-type"], "application/json", "json content-type on run cancel");
});

test("reconcile clears settled rows so reload reconstructs identical state", async () => {
  const calls: FetchCall[] = [];
  const built = createCarrierApi(stubCarrier([SESSION_B]) as never, {
    fetchImpl: scriptFetch(
      [
        runsBody([
          { runId: RUN_QUEUED, seq: 1, state: "accepted", prompt: "pending", actorUserId: "u1" },
          { runId: RUN_ACTIVE, seq: 2, state: "accepted", prompt: "next", actorUserId: "u1" },
        ]),
        runsBody([{ runId: RUN_ACTIVE, seq: 2, state: "dispatched", prompt: "next", actorUserId: "u1" }]),
      ],
      calls,
    ),
  });
  await built.reconcileQueues(undefined, undefined);
  // Second sweep: the first accepted settled (gone from truth) and the next
  // accepted became dispatched: the seat must re-project to exactly the new
  // durable truth — empty dock plus active turn.
  await built.reconcileQueues(undefined, undefined);
  const queueFrames = built.pump.buffer.filter((frame) => frame.type === "session/queue");
  eq(queueFrames.length, 2, "two distinct projections emitted");
  const first = queueFrames[0];
  const second = queueFrames[1];
  if (first === undefined || second === undefined) return fail("queue frames missing");
  eq(first.items.length, 2, "first projection has both queued rows");
  eq(second.items.length, 0, "settled seat cleared");
  const hostFrames = built.hostPump.buffer.filter(isSessionStatus);
  // running stays true across both sweeps (the dispatched run is still
  // live), so the status frame emits once and coalesces the unchanged flag.
  eq(hostFrames.length, 1, "unchanged running flag coalesces");
  eq(hostFrames[0]?.running, true, "queued-then-dispatched seat marks running");
  // A fresh instance re-derived from the same final truth emits the same
  // queue projection: reload reconstructs identical state.
  const reloadCalls: FetchCall[] = [];
  const reloaded = createCarrierApi(stubCarrier([SESSION_B]) as never, {
    fetchImpl: scriptFetch(
      [
        runsBody([{ runId: RUN_ACTIVE, seq: 2, state: "dispatched", prompt: "next", actorUserId: "u1" }]),
      ],
      reloadCalls,
    ),
  });
  await reloaded.reconcileQueues(undefined, undefined);
  const reloadFrames = reloaded.pump.buffer.filter((frame) => frame.type === "session/queue");
  eq(reloadFrames.length, 1, "reload emits one projection");
  eq(reloadFrames[0]?.items, [], "reload reconstructs the settled seat");
});

test("queue steering interrupts the live turn", async () => {
  const mutates: string[] = [];
  const built = createCarrierApi(
    {
      ...stubCarrier(),
      mutate: async (mutation: { op: string }) => {
        mutates.push(mutation.op);
        if (mutation.op === "conversation.cancel") {
          return {
            accepted: true,
            runId: RUN_ACTIVE,
            reason: undefined,
            conversationId: SESSION_B,
            state: "cancel-requested",
            result: undefined,
          };
        }
        return fail(`unexpected mutate ${mutation.op}`);
      },
    } as never,
    { fetchImpl: conversationRunsFetch([], []) },
  );
  const response = await built.api.sessions.updateQueue({
    sessionId: SESSION_B,
    itemId: RUN_QUEUED,
    action: { kind: "steer" },
  });
  if (!response.result.ok) return fail(`steer must interrupt: ${response.result.error.message}`);
  eq(mutates, ["conversation.cancel"], "steer cancels the live conversation turn");
});

test("steer prompt cancels the live turn then admits the follow-up", async () => {
  const mutates: string[] = [];
  const built = createCarrierApi(
    {
      ...stubCarrier(),
      mutate: async (mutation: { op: string }) => {
        mutates.push(mutation.op);
        return {
          accepted: true,
          runId: RUN_ACTIVE,
          reason: undefined,
          conversationId: SESSION_B,
          state: mutation.op === "conversation.cancel" ? "cancel-requested" : "accepted",
          result: undefined,
        };
      },
    } as never,
    { fetchImpl: conversationRunsFetch([], []) },
  );
  const response = await built.api.sessions.prompt({
    sessionId: SESSION_B,
    intentId: "cccccccc-cccc-4ccc-8ccc-ccccccccccc3",
    mode: "steer",
    content: [{ type: "text", text: "redirect" }],
  });
  if (!response.result.ok) return fail(`steer prompt must succeed: ${response.result.error.message}`);
  eq(mutates, ["conversation.cancel", "conversation.message"], "steer interrupts then admits");
});

test("next accepted promotes to active when the predecessor settles", async () => {
  const calls: FetchCall[] = [];
  const built = createCarrierApi(stubCarrier([SESSION_B]) as never, {
    fetchImpl: scriptFetch(
      [
        // Turn one active; one follow-up queued behind it.
        runsBody([
          { runId: RUN_ACTIVE, seq: 1, state: "dispatched", prompt: "first", actorUserId: "u1" },
          { runId: RUN_QUEUED, seq: 2, state: "accepted", prompt: "second", actorUserId: "u1" },
        ]),
        // Predecessor settled; the follow-up became the dispatched turn.
        runsBody([
          { runId: RUN_ACTIVE, seq: 1, state: "completed", prompt: "first", actorUserId: "u1" },
          { runId: RUN_QUEUED, seq: 2, state: "dispatched", prompt: "second", actorUserId: "u1" },
        ]),
      ],
      calls,
    ),
  });
  await built.reconcileQueues(undefined, undefined);
  await built.reconcileQueues(undefined, undefined);
  const queueFrames = built.pump.buffer.filter((frame) => frame.type === "session/queue");
  eq(queueFrames.length, 2, "two projections");
  eq(queueFrames[0]?.items.length, 1, "first: one queued row behind the active turn");
  eq(queueFrames[0]?.items[0]?.id, RUN_QUEUED, "first: queued row is the follow-up");
  eq(queueFrames[1]?.items.length, 0, "second: dock cleared after promotion to active");
  const hostFrames = built.hostPump.buffer.filter(isSessionStatus);
  eq(hostFrames.length, 1, "running unchanged (a dispatched run remains)");
  eq(hostFrames[0]?.running, true, "promoted turn still marks the session active");
});

test("a run that dies unknown with no events clears running and reports the death", async () => {
  const built = createCarrierApi(stubCarrier([SESSION_B]) as never, {
    fetchImpl: scriptFetch(
      [
        runsBody([{ runId: RUN_ACTIVE, seq: 1, state: "dispatched", prompt: "build it", actorUserId: "u1" }]),
        runsBody([{ runId: RUN_ACTIVE, seq: 1, state: "unknown", prompt: "build it", actorUserId: "u1" }]),
      ],
      [],
    ),
  });
  await built.reconcileQueues(undefined, undefined);
  await built.reconcileQueues(undefined, undefined);
  const hostFrames = built.hostPump.buffer.filter(isSessionStatus);
  eq(hostFrames.length, 2, "running flag emits on enter and on death");
  eq(hostFrames[0]?.running, true, "dispatched marks the session active");
  eq(hostFrames[1]?.running, false, "unknown with no live seat clears running");
  const errors = built.hostPump.buffer.filter(isAgentError);
  eq(errors.length, 1, "true-to-false unknown death emits one agent error");
  eq(errors[0]?.message, "The run ended without a result and will not be replayed.", "death copy is the durable unknown outcome");
});

test("first projection of an already-unknown conversation does not invent a live error", async () => {
  const built = createCarrierApi(stubCarrier([SESSION_B]) as never, {
    fetchImpl: scriptFetch(
      [runsBody([{ runId: RUN_ACTIVE, seq: 1, state: "unknown", prompt: "old", actorUserId: "u1" }])],
      [],
    ),
  });
  await built.reconcileQueues(undefined, undefined);
  const hostFrames = built.hostPump.buffer.filter(isSessionStatus);
  eq(hostFrames.length, 1, "one status frame for the initial false seat");
  eq(hostFrames[0]?.running, false, "unknown projects as not running");
  const errors = built.hostPump.buffer.filter(isAgentError);
  eq(errors.length, 0, "reload of a dead conversation is not a new live failure");
});

test("immediate Stop waits for the in-flight prompt then cancels the conversation", async () => {
  const RUN_PROMPT = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbb1";
  const mutates: string[] = [];
  let releasePrompt: ((result: {
    accepted: boolean;
    runId: string;
    reason: undefined;
    conversationId: string;
    state: string;
    result: undefined;
  }) => void) | undefined;
  const promptGate = new Promise<{
    accepted: boolean;
    runId: string;
    reason: undefined;
    conversationId: string;
    state: string;
    result: undefined;
  }>((resolve) => {
    releasePrompt = resolve;
  });
  const built = createCarrierApi(
    {
      ...stubCarrier(),
      mutate: async (mutation: { op: string }) => {
        mutates.push(mutation.op);
        if (mutation.op === "conversation.cancel") {
          return {
            accepted: true,
            runId: RUN_PROMPT,
            reason: undefined,
            conversationId: SESSION_B,
            state: "cancelling",
            result: undefined,
          };
        }
        return promptGate;
      },
    } as never,
    { fetchImpl: conversationRunsFetch([], []) },
  );
  const prompt = built.api.sessions.prompt({
    sessionId: SESSION_B,
    intentId: "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
    content: [{ type: "text", text: "hello" }],
  });
  const stop = built.api.sessions.cancel({ sessionId: SESSION_B });
  releasePrompt?.({
    accepted: true,
    runId: RUN_PROMPT,
    reason: undefined,
    conversationId: SESSION_B,
    state: "accepted",
    result: undefined,
  });
  const promptResult = await prompt;
  const stopResult = await stop;
  if (!promptResult.result.ok) return fail("prompt must stay a single accepted attempt");
  if (!stopResult.result.ok) return fail(`immediate Stop failed: ${stopResult.result.error.message}`);
  eq(mutates.filter((op) => op === "conversation.cancel").length, 1, "one conversation cancel");
  eq(mutates.includes("conversation.message"), true, "prompt still uses message admission");
});

test("a different prompt does not inherit an in-flight admission", async () => {
  const RUN_A = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbb2";
  const RUN_B = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbb3";
  const INTENT_A = "cccccccc-cccc-4ccc-8ccc-ccccccccccc1";
  const INTENT_B = "cccccccc-cccc-4ccc-8ccc-ccccccccccc2";
  const admitted: string[] = [];
  let releaseA: ((result: {
    accepted: boolean;
    runId: string;
    reason: undefined;
    conversationId: string;
    state: string;
    result: undefined;
  }) => void) | undefined;
  const gateA = new Promise<{
    accepted: boolean;
    runId: string;
    reason: undefined;
    conversationId: string;
    state: string;
    result: undefined;
  }>((resolve) => {
    releaseA = resolve;
  });
  let releaseB: ((result: {
    accepted: boolean;
    runId: string;
    reason: undefined;
    conversationId: string;
    state: string;
    result: undefined;
  }) => void) | undefined;
  const gateB = new Promise<{
    accepted: boolean;
    runId: string;
    reason: undefined;
    conversationId: string;
    state: string;
    result: undefined;
  }>((resolve) => {
    releaseB = resolve;
  });
  let mutateCount = 0;
  const built = createCarrierApi(
    {
      ...stubCarrier(),
      mutate: async (mutation: { prompt?: string }) => {
        mutateCount += 1;
        admitted.push(String(mutation.prompt ?? ""));
        if (mutateCount === 1) return gateA;
        return gateB;
      },
    } as never,
    { fetchImpl: conversationRunsFetch([], []) },
  );
  const promptA = built.api.sessions.prompt({
    sessionId: SESSION_B,
    intentId: INTENT_A,
    content: [{ type: "text", text: "first" }],
  });
  await drainTasks();
  eq(mutateCount, 1, "first prompt started admission");
  const promptB = built.api.sessions.prompt({
    sessionId: SESSION_B,
    intentId: INTENT_B,
    content: [{ type: "text", text: "second" }],
  });
  await drainTasks();
  eq(mutateCount, 1, "second prompt must not mutate until the first admits");
  eq(admitted, ["first"], "second prompt must not inherit the first text");
  releaseA?.({
    accepted: true,
    runId: RUN_A,
    reason: undefined,
    conversationId: SESSION_B,
    state: "accepted",
    result: undefined,
  });
  await drainTasks();
  eq(mutateCount, 2, "second prompt admits its own mutation");
  eq(admitted, ["first", "second"], "second prompt text is sent");
  releaseB?.({
    accepted: true,
    runId: RUN_B,
    reason: undefined,
    conversationId: SESSION_B,
    state: "accepted",
    result: undefined,
  });
  const firstResult = await promptA;
  const secondResult = await promptB;
  if (!firstResult.result.ok) return fail(`first prompt refused: ${firstResult.result.error.message}`);
  if (!secondResult.result.ok) return fail(`second prompt refused: ${secondResult.result.error.message}`);
});

test("reopen hydrates running from durable conversation runs", async () => {
  const calls: FetchCall[] = [];
  const mutates: string[] = [];
  const built = createCarrierApi(
    {
      loadBaseline: async () => ({
        cursor: "0",
        sessions: [{ id: SESSION_B, running: false }],
        agents: [],
        workspaces: [],
      }),
      poll: async () => ({ kind: "events", cursor: "0", events: [] }),
      loadHistory: async () => ({
        events: [],
        hasMore: false,
        liveRuns: [{ runId: RUN_ACTIVE, seq: 1, state: "dispatched", prompt: "live", actorUserId: "u1" }],
      }),
      mutate: async (mutation: { op: string; conversationId?: string }) => {
        mutates.push(mutation.op);
        if (mutation.op === "conversation.cancel") {
          return {
            accepted: true,
            reason: undefined,
            conversationId: mutation.conversationId,
            runId: RUN_ACTIVE,
            state: "cancelling",
            result: undefined,
          };
        }
        return { accepted: false, reason: undefined, conversationId: undefined, runId: undefined, state: undefined, result: undefined };
      },
    } as never,
    {
      fetchImpl: conversationRunsFetch(calls, [
        { runId: RUN_ACTIVE, seq: 1, state: "dispatched", prompt: "live", actorUserId: "u1" },
      ]),
    },
  );
  const opened = await built.api.sessions.history({ sessionId: SESSION_B, maxMessages: 50 });
  if (!opened.result.ok) return fail("history refused");
  const hostFrames = built.hostPump.buffer.filter(isSessionStatus);
  eq(hostFrames.length, 1, "open replays host/session-status");
  eq(hostFrames[0]?.sessionId, SESSION_B, "status is for the opened conversation");
  eq(hostFrames[0]?.running, true, "dispatched Run hydrates running");
  eq(
    calls.some((call) => /\/runs(\?|$)/.test(call.url)),
    false,
    "open uses history liveRuns, not GET /runs",
  );
  const listed = await built.api.sessions.list();
  if (!listed.result.ok) return fail("list after open refused");
  eq(listed.result.value.items[0]?.running, true, "list.running overlays the same durable Run");
  const stop = await built.api.sessions.cancel({ sessionId: SESSION_B });
  if (!stop.result.ok) return fail(`reopen Stop failed: ${stop.result.error.message}`);
  eq(mutates.includes("conversation.cancel"), true, "Stop uses conversation cancel");
  eq(
    calls.some((call) => call.url.includes("/api/runs/") && call.url.includes("/cancel")),
    false,
    "Stop must not pick a Run in the browser",
  );
});

test("session Stop posts one conversation cancel", async () => {
  const mutates: Array<{ op: string; conversationId?: string }> = [];
  const built = createCarrierApi({
    ...stubCarrier(),
    mutate: async (mutation: { op: string; conversationId?: string }) => {
      mutates.push(mutation);
      return {
        accepted: true,
        reason: undefined,
        conversationId: mutation.conversationId,
        runId: RUN_ACTIVE,
        state: "cancelling",
        result: undefined,
      };
    },
  } as never);
  const stop = await built.api.sessions.cancel({ sessionId: SESSION_B });
  if (!stop.result.ok) return fail(`Stop refused: ${stop.result.error.message}`);
  eq(mutates.length, 1, "one cancel mutation");
  eq(mutates[0]?.op, "conversation.cancel", "conversation-scoped cancel");
  eq(mutates[0]?.conversationId, SESSION_B, "cancel names the session");
});

test("session Stop treats an already-unknown Run as settled success", async () => {
  const mutates: string[] = [];
  const built = createCarrierApi({
    ...stubCarrier(),
    mutate: async (mutation: { op: string }) => {
      mutates.push(mutation.op);
      return {
        accepted: true,
        reason: undefined,
        conversationId: SESSION_B,
        runId: RUN_ACTIVE,
        state: "unknown",
        result: undefined,
      };
    },
  } as never);
  const stop = await built.api.sessions.cancel({ sessionId: SESSION_B });
  if (!stop.result.ok) {
    return fail(`already-unknown Stop failed: ${stop.result.error.message}`);
  }
  eq(mutates, ["conversation.cancel"], "cancellation is not duplicated");
  eq(
    stop.result.error !== undefined && String(stop.result.error.message).includes("no active run"),
    false,
    "already-unknown Stop must not report no active run",
  );
  eq(
    stop.result.error !== undefined && String(stop.result.error.message).includes("cancel refused"),
    false,
    "already-unknown Stop must not toast cancel refused",
  );
});

test("baseline reads the event cursor without history bytes", async () => {
  const calls: FetchCall[] = [];
  const carrier = new VoieCarrier({
    fetchImpl: scriptFetch(
      [
        { kind: "json", status: 200, body: { items: [] } },
        { kind: "json", status: 200, body: { items: [] } },
        { kind: "json", status: 200, body: { items: [] } },
        { kind: "json", status: 200, body: { items: [], cursor: 9000 } },
      ],
      calls,
    ),
    holdMs: 30_000,
    intervalMs: 2,
  });
  const baseline = await carrier.loadBaseline();
  eq(baseline.cursor, "9000", "cursor is the head seq");
  eq(
    calls.some((call) => call.url === "/api/events?head=1"),
    true,
    "head cursor request",
  );
  eq(
    calls.some((call) => call.url.includes("/api/events?after=0")),
    false,
    "does not load after=0 history bytes",
  );
});

test("opening history is one bounded request", async () => {
  const calls: FetchCall[] = [];
  const batch = appendBatch(SESSION_A, 1000, 1000, [
    { type: "user/message", seq: 1000, time: 1, data: { text: "hi" } },
  ]);
  const carrier = new VoieCarrier({
    fetchImpl: scriptFetch(
      [{ kind: "json", status: 200, body: { items: [batch], hasMore: true } }],
      calls,
    ),
    holdMs: 30_000,
    intervalMs: 2,
  });
  const page = await carrier.loadHistory(SESSION_A, undefined, { maxMessages: 50 });
  eq(calls.length, 1, "one history request");
  eq(
    /\/api\/conversations\/.+\/history\?/.test(calls[0]?.url ?? ""),
    true,
    "history path",
  );
  eq((calls[0]?.url ?? "").includes("maxMessages=50"), true, "page bound");
  eq(page.hasMore, true, "hasMore");
  eq(page.events.length, 1, "decoded page events");
  eq(
    calls.some((call) => /\/runs(\?|$)/.test(call.url)),
    false,
    "no per-run GET",
  );
  eq(
    calls.some((call) => /blob|objects\//i.test(call.url)),
    false,
    "no blob GET",
  );
});

// ------------------------------------------------------------------- runner

let failures = 0;
for (const current of cases) {
  try {
    await current.run();
    console.log(`ok - ${current.name}`);
  } catch (reason) {
    failures += 1;
    const detail = reason instanceof Error ? reason.message : String(reason);
    console.log(`not ok - ${current.name}`);
    console.log(`  ${detail.split("\n").join("\n  ")}`);
  }
}
console.log(
  failures === 0
    ? `all ${String(cases.length)} carrier behavior cases passed`
    : `${String(cases.length - failures)}/${String(cases.length)} passed, ${String(failures)} failed`,
);
const host = globalThis as { process?: { exitCode?: number } };
if (host.process !== undefined && failures > 0) host.process.exitCode = 1;
