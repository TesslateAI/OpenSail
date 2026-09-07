/**
 * Deep-link scope decisions for `/chat/:id` without a matching `?project=`.
 *
 *   cd web && node --experimental-strip-types src/portal/conversation-scope.test.ts
 */
import {
  conversationHref,
  conversationScopeDecision,
} from "./conversation-scope.ts";

type Case = { name: string; run: () => void };

const cases: Case[] = [];

function test(name: string, run: () => void): void {
  cases.push({ name, run });
}

function fail(message: string): never {
  throw new Error(message);
}

function eq(actual: unknown, expected: unknown, label: string): void {
  const same = (left: unknown, right: unknown): boolean => {
    if (Object.is(left, right)) return true;
    if (Array.isArray(left) && Array.isArray(right)) {
      return left.length === right.length && left.every((value, index) => same(value, right[index]));
    }
    if (
      left !== null &&
      right !== null &&
      typeof left === "object" &&
      typeof right === "object"
    ) {
      const leftRecord = left as Record<string, unknown>;
      const rightRecord = right as Record<string, unknown>;
      const keys = new Set([...Object.keys(leftRecord), ...Object.keys(rightRecord)]);
      for (const key of keys) {
        if (!same(leftRecord[key], rightRecord[key])) return false;
      }
      return true;
    }
    return false;
  };
  if (!same(actual, expected)) {
    fail(`${label}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

const CHAT = "67654dcf-35eb-4233-8811-ff15628e0741";
const PERSONAL = "81b1d0ec-943f-4416-adee-955b54103b39";
const TEAM = "a735fb76-3dbc-42dc-a7a5-e8d7664c639a";

test("listed conversation stays in the current scope", () => {
  eq(
    conversationScopeDecision({
      conversationId: CHAT,
      currentProjectId: PERSONAL,
      chatsLoaded: true,
      listed: true,
    }),
    { kind: "stay" },
    "listed",
  );
});

test("waits until the ledger finishes loading", () => {
  eq(
    conversationScopeDecision({
      conversationId: CHAT,
      currentProjectId: PERSONAL,
      chatsLoaded: false,
      listed: false,
    }),
    { kind: "wait" },
    "ledger pending",
  );
});

test("looks up the session when the ledger loaded without the id", () => {
  eq(
    conversationScopeDecision({
      conversationId: CHAT,
      currentProjectId: PERSONAL,
      chatsLoaded: true,
      listed: false,
    }),
    { kind: "lookup" },
    "session pending",
  );
});

test("reloads into the owning project when the URL named the wrong scope", () => {
  eq(
    conversationScopeDecision({
      conversationId: CHAT,
      currentProjectId: PERSONAL,
      chatsLoaded: true,
      listed: false,
      sessionProjectId: TEAM,
    }),
    { kind: "reload", href: conversationHref(CHAT, TEAM) },
    "wrong scope",
  );
  eq(
    conversationHref(CHAT, TEAM),
    `/chat/${CHAT}?project=${TEAM}`,
    "href carries project",
  );
});

test("stays when the session belongs to the current project but the list lagged", () => {
  eq(
    conversationScopeDecision({
      conversationId: CHAT,
      currentProjectId: TEAM,
      chatsLoaded: true,
      listed: false,
      sessionProjectId: TEAM,
    }),
    { kind: "stay" },
    "same project",
  );
});

test("missing when the membership-scoped session read refuses", () => {
  eq(
    conversationScopeDecision({
      conversationId: CHAT,
      currentProjectId: PERSONAL,
      chatsLoaded: true,
      listed: false,
      sessionProjectId: null,
    }),
    { kind: "missing" },
    "404",
  );
});

let failed = 0;
for (const item of cases) {
  try {
    item.run();
    console.log(`ok  ${item.name}`);
  } catch (error) {
    failed += 1;
    console.log(`fail  ${item.name}`);
    console.log(error instanceof Error ? error.message : String(error));
  }
}
if (failed > 0) {
  console.log(`${String(failed)} failed`);
} else {
  console.log(`${String(cases.length)} passed`);
}
const host = globalThis as { process?: { exitCode?: number } };
if (host.process !== undefined && failed > 0) host.process.exitCode = 1;
