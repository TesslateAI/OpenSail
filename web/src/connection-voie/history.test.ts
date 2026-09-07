/**
 * Session history paging: reopen must reconstruct the conversation, not the
 * last N raw events.
 *
 *   cd web && node --experimental-strip-types src/connection-voie/history.test.ts
 */
import { createCarrierApi, pageSessionHistory } from "./api.ts";
import type { CanonicalEvent } from "../carrier/types.ts";

type Case = { name: string; run: () => Promise<void> | void };

const cases: Case[] = [];

function test(name: string, run: () => Promise<void> | void): void {
  cases.push({ name, run });
}

function fail(message: string): never {
  throw new Error(message);
}

function eq(actual: unknown, expected: unknown, label: string): void {
  if (!Object.is(actual, expected)) {
    fail(`${label}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

function entry(type: string, seq: number): { event: { type: string; seq: number } } {
  return { event: { type, seq } };
}

/** One user turn plus many tool steps — the live shape that a 50-event slice truncates. */
function toolHeavyLog(): { event: { type: string; seq: number } }[] {
  const log = [entry("turn/start", 1), entry("user/message", 2)];
  let seq = 3;
  for (let step = 0; step < 40; step++) {
    log.push(entry("step/start", seq++));
    log.push(entry("assistant/chunk", seq++));
    log.push(entry("assistant/message", seq++));
    log.push(entry("tool/call", seq++));
    log.push(entry("tool/result", seq++));
    log.push(entry("step/end", seq++));
  }
  return log;
}

test("a tool-heavy turn shorter than maxMessages returns the whole log", () => {
  const log = toolHeavyLog();
  eq(log.length > 50, true, "fixture is larger than a 50-event slice");
  const page = pageSessionHistory(log, undefined, 50);
  eq(page.hasMore, false, "hasMore");
  eq(page.events.length, log.length, "event count");
  eq(page.events[0]?.event.type, "turn/start", "starts at the user turn");
  eq(page.events[1]?.event.type, "user/message", "keeps the user message");
});

test("enough messages cut at turn/start and report older history", () => {
  const log = [
    entry("turn/start", 1),
    entry("user/message", 2),
    entry("assistant/message", 3),
    entry("turn/end", 4),
    entry("turn/start", 5),
    entry("user/message", 6),
    entry("assistant/message", 7),
    entry("turn/end", 8),
    entry("turn/start", 9),
    entry("user/message", 10),
    entry("assistant/message", 11),
    entry("turn/end", 12),
  ];
  const oneTurn = pageSessionHistory(log, undefined, 2);
  eq(oneTurn.hasMore, true, "one-turn hasMore");
  eq(oneTurn.events[0]?.event.seq, 9, "one-turn oldest seq");
  const twoTurns = pageSessionHistory(log, undefined, 4);
  eq(twoTurns.hasMore, true, "two-turn hasMore");
  eq(twoTurns.events[0]?.event.seq, 5, "two-turn oldest seq");
  eq(twoTurns.events[0]?.event.type, "turn/start", "window opens on turn/start");
  eq(
    twoTurns.events.filter((item) => item.event.type === "user/message").length,
    2,
    "user messages in two-turn window",
  );
});

test("beforeSeq pages the preceding contiguous window", () => {
  const log = [
    entry("turn/start", 1),
    entry("user/message", 2),
    entry("assistant/message", 3),
    entry("turn/start", 4),
    entry("user/message", 5),
    entry("assistant/message", 6),
  ];
  const page = pageSessionHistory(log, 4, 50);
  eq(page.hasMore, false, "hasMore");
  eq(page.events[page.events.length - 1]?.event.seq, 3, "tail seq");
  eq((page.events.at(-1)?.event.seq ?? -1) + 1, 4, "contiguous with beforeSeq");
});

const SESSION = "b02da0fc-b568-43be-96da-fe016cb006a5";

function canonical(type: string, seq: number): CanonicalEvent {
  return {
    sessionId: SESSION,
    type,
    data: {},
    globalSeq: seq,
    revision: 1,
    eventIndex: seq,
    appendId: null,
    objectKey: null,
    contentHash: null,
    byteLength: null,
    seq,
    time: 1,
    surfaceOp: undefined,
    sourceEventSeqs: undefined,
  };
}

test("sessions.history issues one bounded loadHistory call", async () => {
  const events: CanonicalEvent[] = [];
  for (let seq = 1; seq <= 1000; seq++) {
    events.push(canonical("assistant/message", seq));
  }
  let calls = 0;
  let runGets = 0;
  let pageArg: { beforeSeq?: number; maxMessages?: number } | undefined;
  const built = createCarrierApi({
    loadBaseline: async () => ({ cursor: "0", sessions: [], agents: [], workspaces: [] }),
    poll: async () => ({ kind: "events", cursor: "0", events: [] }),
    loadHistory: async (_id, _signal, page) => {
      calls += 1;
      pageArg = page;
      return { events: events.slice(-80), hasMore: true, liveRuns: [] };
    },
    mutate: async () => ({
      accepted: false,
      reason: undefined,
      conversationId: undefined,
      runId: undefined,
      state: undefined,
      result: undefined,
    }),
  }, {
    fetchImpl: (async () => {
      runGets += 1;
      return {
        ok: true,
        status: 200,
        json: async () => ({ runs: [] }),
      };
    }) as unknown as typeof fetch,
  });
  const response = await built.api.sessions.history({ sessionId: SESSION, maxMessages: 50 });
  if (!response.result.ok) fail("history refused");
  eq(calls, 1, "one history request");
  eq(runGets, 0, "history page does not GET /runs");
  eq(pageArg?.maxMessages, 50, "maxMessages forwarded");
  eq(response.result.value.hasMore, true, "server hasMore is preserved");
});

test("sessions.history reopen returns the user turn of a long tool log", async () => {
  const events: CanonicalEvent[] = [
    canonical("turn/start", 1),
    canonical("user/message", 2),
  ];
  let seq = 3;
  for (let step = 0; step < 40; step++) {
    events.push(canonical("assistant/message", seq++));
    events.push(canonical("tool/call", seq++));
    events.push(canonical("tool/result", seq++));
  }
  const built = createCarrierApi({
    loadBaseline: async () => ({ cursor: "0", sessions: [], agents: [], workspaces: [] }),
    poll: async () => ({ kind: "events", cursor: "0", events: [] }),
    loadHistory: async () => ({ events, hasMore: false, liveRuns: [] }),
    mutate: async () => ({
      accepted: false,
      reason: undefined,
      conversationId: undefined,
      runId: undefined,
      state: undefined,
      result: undefined,
    }),
  }, {
    fetchImpl: (async () => ({
      ok: true,
      status: 200,
      json: async () => ({ runs: [] }),
    })) as unknown as typeof fetch,
  });
  const response = await built.api.sessions.history({ sessionId: SESSION, maxMessages: 50 });
  if (!response.result.ok) fail("history refused");
  eq(response.result.value.hasMore, false, "hasMore");
  eq(response.result.value.events.length, events.length, "event count");
  eq(response.result.value.events[0]?.event.type, "turn/start", "first type");
  eq(response.result.value.events[1]?.event.type, "user/message", "user message");
});

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
    ? `all ${String(cases.length)} history cases passed`
    : `${String(cases.length - failures)}/${String(cases.length)} passed, ${String(failures)} failed`,
);
const host = globalThis as { process?: { exitCode?: number } };
if (host.process !== undefined && failures > 0) host.process.exitCode = 1;
