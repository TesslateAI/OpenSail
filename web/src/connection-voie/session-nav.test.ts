/**
 * Live-graph conversation switching: the portal URL opens a session
 * without remounting DSH.
 *
 *   cd web && nix develop -c bash -c \
 *     "node --experimental-strip-types src/connection-voie/session-nav.test.ts"
 */
import {
  bindVoieSessionNav,
  requestVoieOpenConversation,
  unbindVoieSessionNav,
  type VoieSessionNav,
} from "./session-nav.ts";
import { setVoieDshHostContext } from "./host-context.ts";

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
    return false;
  };
  if (!same(actual, expected)) {
    fail(`${label}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

function installWindow(): void {
  const target = new EventTarget();
  (globalThis as { window?: EventTarget }).window = target;
}

function fakeSessions(listed: string[]): VoieSessionNav & { opened: string[] } {
  const byId: Record<string, unknown> = {};
  for (const id of listed) byId[id] = { id };
  let current: string | undefined;
  const listeners: Array<() => void> = [];
  const opened: string[] = [];
  return {
    opened,
    open: (id: string) => {
      if (byId[id] === undefined) fail(`open of unlisted session ${id}`);
      current = id;
      opened.push(id);
    },
    list: {
      getSnapshot: () => ({ byId, current }),
      subscribe: (listener: () => void) => {
        listeners.push(listener);
        return () => {
          const index = listeners.indexOf(listener);
          if (index !== -1) listeners.splice(index, 1);
        };
      },
    },
  };
}

test("host context id at bind opens the listed conversation", () => {
  unbindVoieSessionNav();
  installWindow();
  setVoieDshHostContext({ projectId: "scope", conversationId: "conv-a" });
  const sessions = fakeSessions(["conv-a", "conv-b"]);
  bindVoieSessionNav(sessions);
  eq(sessions.opened, ["conv-a"], "opens the host conversation");
});

test("unknown id waits for the list then opens once", () => {
  unbindVoieSessionNav();
  installWindow();
  setVoieDshHostContext({ projectId: "scope", conversationId: "conv-late" });
  const byId: Record<string, unknown> = {};
  let current: string | undefined;
  const listeners: Array<() => void> = [];
  const opened: string[] = [];
  const sessions: VoieSessionNav = {
    open: (id: string) => {
      current = id;
      opened.push(id);
      for (const listener of [...listeners]) listener();
    },
    list: {
      getSnapshot: () => ({ byId, current }),
      subscribe: (listener: () => void) => {
        listeners.push(listener);
        return () => {
          const index = listeners.indexOf(listener);
          if (index !== -1) listeners.splice(index, 1);
        };
      },
    },
  };
  bindVoieSessionNav(sessions);
  eq(opened.length, 0, "does not open before the row exists");
  byId["conv-late"] = { id: "conv-late" };
  for (const listener of [...listeners]) listener();
  eq(opened, ["conv-late"], "opens once the list gains the row");
  for (const listener of [...listeners]) listener();
  eq(opened, ["conv-late"], "list updates after open do not reopen");
});

test("event switches to another listed conversation", () => {
  unbindVoieSessionNav();
  installWindow();
  setVoieDshHostContext({ projectId: "scope", conversationId: "conv-a" });
  const sessions = fakeSessions(["conv-a", "conv-b"]);
  bindVoieSessionNav(sessions);
  requestVoieOpenConversation("conv-b");
  eq(sessions.opened, ["conv-a", "conv-b"], "second URL opens the new id");
});

test("unbind drops the listener so a later event does not open", () => {
  unbindVoieSessionNav();
  installWindow();
  setVoieDshHostContext({ projectId: "scope", conversationId: "conv-a" });
  const sessions = fakeSessions(["conv-a", "conv-b"]);
  bindVoieSessionNav(sessions);
  unbindVoieSessionNav();
  requestVoieOpenConversation("conv-b");
  eq(sessions.opened, ["conv-a"], "disposed binder ignores later events");
});

let failures = 0;
for (const current of cases) {
  try {
    current.run();
    console.log(`ok - ${current.name}`);
  } catch (reason) {
    failures += 1;
    const detail = reason instanceof Error ? reason.message : String(reason);
    console.log(`not ok - ${current.name}`);
    console.log(`  ${detail.split("\n").join("\n  ")}`);
  }
}
unbindVoieSessionNav();
console.log(
  failures === 0
    ? `all ${String(cases.length)} session-nav cases passed`
    : `${String(cases.length - failures)}/${String(cases.length)} passed, ${String(failures)} failed`,
);
const host = globalThis as { process?: { exitCode?: number } };
if (host.process !== undefined && failures > 0) host.process.exitCode = 1;
