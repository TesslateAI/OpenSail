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

type FetchCall = { url: string; signal: AbortSignal | null | undefined; headers: Record<string, unknown> };

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

  eq(calls.map((call) => call.url), ["/api/events?after=40"], "single paged read");
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

test("poll advances the cursor across empty responses before succeeding", async () => {
  const calls: FetchCall[] = [];
  const clock = manualClock();
  const carrier = new VoieCarrier({
    fetchImpl: scriptFetch(
      [
        { kind: "json", status: 200, body: { items: [], cursor: 44 } },
        {
          kind: "json",
          status: 200,
          body: {
            items: [
              appendBatch(SESSION_A, 45, 1, [{ type: "run/dispatched", seq: 2, time: 1717171718000 }]),
            ],
            cursor: 45,
          },
        },
      ],
      calls,
    ),
    schedulers: clock,
    holdMs: 30_000,
    intervalMs: 10,
  });

  const pending = carrier.poll("40");
  await drainTasks();
  eq(clock.pendingCount(), 1, "paced re-read armed after the empty page");

  const result = (await pump(pending, clock, 3, 10)) as PollResult;

  eq(calls.map((call) => call.url), ["/api/events?after=40", "/api/events?after=44"], "advanced after-reread");
  eq(result.kind, "events", "result kind");
  if (result.kind !== "events") return fail("unreachable");
  eq(result.cursor, "45", "cursor rides the advanced server value");
  eq(result.events.length, 1, "fresh batch decoded");
  const event = result.events[0];
  if (event === undefined) return fail("event missing");
  eq(event.type, "run/dispatched", "decoded type");
  eq(event.globalSeq, 45, "decoded globalSeq");
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

  eq(calls.map((call) => call.url), ["/api/events?after=50"], "requested cursor echoed");
  eq(result, { kind: "stale" }, "regressed feed reported as stale");
});

test("poll expires at its deadline returning empty with the advanced cursor", async () => {
  const calls: FetchCall[] = [];
  const pages: ScriptPage[] = [];
  for (let seq = 41; seq <= 60; seq += 1) {
    pages.push({ kind: "json", status: 200, body: { items: [], cursor: seq } });
  }
  const clock = manualClock();
  const carrier = new VoieCarrier({
    fetchImpl: scriptFetch(pages, calls),
    schedulers: clock,
    holdMs: 80,
    intervalMs: 10,
  });

  const result = (await pump(carrier.poll("40"), clock, 24, 10)) as PollResult;

  eq(result.kind, "events", "result kind");
  if (result.kind !== "events") return fail("unreachable");
  eq(result.events, [], "deadline yields no invented events");
  if (calls.length < 2) return fail("paced re-reads expected");
  let lastPageCursor = 40;
  for (let index = 0; index < calls.length; index += 1) {
    // Call N asks after=<cursor of page N-1>; pages advance one per response.
    eq(calls[index]?.url, `/api/events?after=${String(lastPageCursor)}`, "monotonic cursor chain");
    lastPageCursor += 1;
  }
  eq(result.cursor, String(lastPageCursor), "final cursor matches last consumed page");
});

test("poll honors an abort signaled mid-pause by unwinding promptly", async () => {
  const calls: FetchCall[] = [];
  const clock = manualClock();
  const controller = new AbortController();
  const carrier = new VoieCarrier({
    fetchImpl: scriptFetch(
      [
        { kind: "json", status: 200, body: { items: [], cursor: 41 } },
        { kind: "expect-aborted" },
      ],
      calls,
    ),
    schedulers: clock,
    holdMs: 30_000,
    intervalMs: 5,
  });

  const pending = carrier.poll("40", controller.signal);
  await drainTasks();
  eq(clock.pendingCount(), 1, "inter-read pause armed");

  controller.abort();

  let rejection: unknown;
  try {
    await pump(pending, clock, 4, 5);
  } catch (reason) {
    rejection = reason;
  }
  const message = rejection instanceof Error ? rejection.message : String(rejection ?? "");
  if (!message.includes("aborted before send")) {
    fail(`abort must unwind promptly with the abort failure, got: ${rejection === undefined ? "resolution" : message}`);
  }
  eq(clock.pendingCount(), 0, "armed pause cleared by the abort listener");
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
    conversationId: SESSION_A,
    projectId: "55555555-5555-4555-8555-555555555555",
    agentId: "66666666-6666-4666-8666-666666666666",
    workspaceId: "77777777-7777-4777-8777-777777777777",
    prompt: "hello vo",
  });

  eq(result.accepted, true, "mutation accepted");
  const post = calls[0];
  if (post === undefined) return fail("mutation request missing");
  if (!post.url.endsWith("/api/conversations")) return fail(`unexpected path ${post.url}`);
  eq(post.headers["x-voie-intent"], "mutate", "intent marker enforced centrally");
  eq(post.headers["content-type"], "application/json", "json content-type present");
  eq(post.headers["accept"], "application/json", "accept preserved");
});

test("bodyless cancel still carries both admission markers", async () => {
  const calls: FetchCall[] = [];
  const carrier = new VoieCarrier({
    fetchImpl: scriptFetch(
      [
        {
          kind: "json",
          status: 200,
          body: {
            items: [
              {
                id: "88888888-8888-4888-8888-888888888888",
                sessionId: SESSION_A,
                state: "accepted",
              },
            ],
          },
        },
        { kind: "json", status: 200, body: { accepted: true, state: "cancelling" } },
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
  eq(calls.length, 2, "run resolve plus cancel post");
  const cancelPost = calls[1];
  if (cancelPost === undefined) return fail("cancel request missing");
  if (!cancelPost.url.includes("/cancel")) return fail(`unexpected path ${cancelPost.url}`);
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
  loadHistory(sessionId: string, signal?: AbortSignal): Promise<unknown[]>;
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
    loadHistory: async () => [],
    mutate: async () => ({ accepted: false }),
  };
}

function runsBody(rows: Array<Record<string, unknown>>): { kind: "json"; status: number; body: { runs: Array<Record<string, unknown>> } } {
  return { kind: "json", status: 200, body: { runs: rows } };
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
  const hostFrames = built.hostPump.buffer.filter((h) => h.type === "host/session-status");
  eq(hostFrames.length, 1, "one running-status frame emitted");
  eq(hostFrames[0]?.running, true, "dispatched run marks the session active");
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
  const hostFrames = built.hostPump.buffer.filter((h) => h.type === "host/session-status");
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

test("queue steering stays honestly refused", async () => {
  const calls: FetchCall[] = [];
  const built = createCarrierApi(stubCarrier() as never, {
    fetchImpl: scriptFetch([], calls),
  });
  const response = await built.api.sessions.updateQueue({
    sessionId: SESSION_B,
    itemId: RUN_QUEUED,
    action: { kind: "steer" },
  });
  if (response.result.ok) return fail("steer must not succeed");
  eq(response.result.error.code, "steer-unavailable", "honest steering refusal code");
  eq(calls.length, 0, "steer performs no network writes");
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
  const hostFrames = built.hostPump.buffer.filter((h) => h.type === "host/session-status");
  eq(hostFrames.length, 1, "running unchanged (a dispatched run remains)");
  eq(hostFrames[0]?.running, true, "promoted turn still marks the session active");
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
