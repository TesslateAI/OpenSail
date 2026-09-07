/**
 * Document-scoped New-chat listener: remounting DSH must not stack
 * `voie-new-chat` handlers.
 *
 *   cd web && nix develop -c bash -c \
 *     "node --experimental-strip-types src/connection-voie/new-chat.test.ts"
 *
 * No wall-clock sleeps: production `setTimeout(0)` is replaced with a
 * synchronous schedule for the duration of each case.
 */
import {
  bindVoieNewChatListener,
  unbindVoieNewChatListener,
  VOIE_NEW_CHAT_EVENT,
  type NewChatStarter,
} from "./new-chat.ts";
import { setVoieDshHostContext } from "./host-context.ts";
import { forgetWorkspacesForTests, rememberWorkspace } from "./last-workspace.ts";

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

function installWindow(): {
  dispatch(): void;
  listenerCount(): number;
} {
  const target = new EventTarget();
  const add = target.addEventListener.bind(target);
  const remove = target.removeEventListener.bind(target);
  let listeners = 0;
  const addEventListener: EventTarget["addEventListener"] = (type, listener, options) => {
    if (type === VOIE_NEW_CHAT_EVENT) listeners += 1;
    add(type, listener, options);
  };
  const removeEventListener: EventTarget["removeEventListener"] = (type, listener, options) => {
    if (type === VOIE_NEW_CHAT_EVENT) listeners = Math.max(0, listeners - 1);
    remove(type, listener, options);
  };
  const setTimeout = (run: () => void): number => {
    run();
    return 0;
  };
  (globalThis as { window?: EventTarget & { setTimeout: typeof setTimeout } }).window = Object.assign(
    target,
    { addEventListener, removeEventListener, setTimeout },
  );
  return {
    dispatch: (workspaceId?: string) => {
      if (workspaceId === undefined) {
        target.dispatchEvent(new Event(VOIE_NEW_CHAT_EVENT));
        return;
      }
      target.dispatchEvent(
        new CustomEvent(VOIE_NEW_CHAT_EVENT, { detail: { workspaceId } }),
      );
    },
    listenerCount: () => listeners,
  };
}

function starter(calls: string[]): NewChatStarter {
  return (workspaceId?: string) => {
    calls.push(workspaceId === undefined ? "default" : workspaceId);
  };
}

test("conversation A -> New chat -> conversation B -> New chat -> one startSession", () => {
  unbindVoieNewChatListener();
  forgetWorkspacesForTests();
  const win = installWindow();
  setVoieDshHostContext({ projectId: "scope", workspaceId: "ws-b" });
  const callsA: string[] = [];
  const callsB: string[] = [];
  bindVoieNewChatListener(starter(callsA));
  // Established conversation remounts ChatHost without the old listener
  // being removed by AppWebEntry.dispose — the previous leak.
  bindVoieNewChatListener(starter(callsB));
  eq(win.listenerCount(), 1, "still one document listener after the second boot");
  win.dispatch();
  eq(callsA.length, 0, "disposed boot A must not start a session");
  eq(callsB, ["ws-b"], "New chat after conversation B starts exactly once");
});

test("unbind drops the listener so a later boot is the only handler", () => {
  unbindVoieNewChatListener();
  forgetWorkspacesForTests();
  const win = installWindow();
  setVoieDshHostContext({ projectId: "scope" });
  const first: string[] = [];
  const second: string[] = [];
  bindVoieNewChatListener(starter(first));
  unbindVoieNewChatListener();
  eq(win.listenerCount(), 0, "dispose removes the document listener");
  bindVoieNewChatListener(starter(second));
  eq(win.listenerCount(), 1, "rebind installs one listener");
  win.dispatch();
  eq(first.length, 0, "unbound starter is not called");
  eq(second, ["default"], "current boot starts the session once");
});

test("a dispatch after dispose does not start a session", () => {
  unbindVoieNewChatListener();
  const win = installWindow();
  setVoieDshHostContext({ projectId: "scope", workspaceId: "ws-a" });
  const calls: string[] = [];
  bindVoieNewChatListener(starter(calls));
  unbindVoieNewChatListener();
  win.dispatch();
  eq(calls.length, 0, "no starter after DSH disposal");
  eq(win.listenerCount(), 0, "no leftover listener");
});

test("event workspaceId beats a stale host context", () => {
  unbindVoieNewChatListener();
  forgetWorkspacesForTests();
  const win = installWindow();
  setVoieDshHostContext({ projectId: "scope", workspaceId: "ws-old" });
  const calls: string[] = [];
  bindVoieNewChatListener(starter(calls));
  win.dispatch("ws-created");
  eq(calls, ["ws-created"], "New chat uses the Workspace from the event");
});

test("remembered Workspace beats a stale host context", () => {
  unbindVoieNewChatListener();
  forgetWorkspacesForTests();
  const win = installWindow();
  setVoieDshHostContext({ projectId: "scope", workspaceId: "ws-old" });
  rememberWorkspace("scope", "ws-created");
  const calls: string[] = [];
  bindVoieNewChatListener(starter(calls));
  win.dispatch();
  eq(calls, ["ws-created"], "New chat uses the Workspace just created");
  forgetWorkspacesForTests();
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
unbindVoieNewChatListener();
console.log(
  failures === 0
    ? `all ${String(cases.length)} new-chat listener cases passed`
    : `${String(cases.length - failures)}/${String(cases.length)} passed, ${String(failures)} failed`,
);
const host = globalThis as { process?: { exitCode?: number } };
if (host.process !== undefined && failures > 0) host.process.exitCode = 1;
