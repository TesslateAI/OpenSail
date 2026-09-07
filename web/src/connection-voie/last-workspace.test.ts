/**
 * Last-Workspace memory: New Chat must bind the Workspace just created.
 *
 *   cd web && node --experimental-strip-types src/connection-voie/last-workspace.test.ts
 */
import {
  forgetWorkspacesForTests,
  lastWorkspace,
  rememberWorkspace,
} from "./last-workspace.ts";

type Case = { name: string; run: () => void };

const cases: Case[] = [];

function test(name: string, run: () => void): void {
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

function installStorage(): void {
  const make = (): Storage => {
    const data = new Map<string, string>();
    return {
      getItem: (key: string) => data.get(key) ?? null,
      setItem: (key: string, value: string) => {
        data.set(key, value);
      },
      removeItem: (key: string) => {
        data.delete(key);
      },
      clear: () => {
        data.clear();
      },
      key: (index: number) => [...data.keys()][index] ?? null,
      get length() {
        return data.size;
      },
    };
  };
  const host = globalThis as { sessionStorage?: Storage; localStorage?: Storage };
  host.sessionStorage = make();
  host.localStorage = make();
}

test("remembered Workspace is readable in the same document", () => {
  installStorage();
  forgetWorkspacesForTests();
  rememberWorkspace("proj", "ws-new");
  eq(lastWorkspace("proj"), "ws-new", "in-memory");
});

test("storage survives forgetting in-memory", () => {
  installStorage();
  forgetWorkspacesForTests();
  rememberWorkspace("proj", "ws-stored");
  forgetWorkspacesForTests();
  eq(lastWorkspace("proj"), "ws-stored", "sessionStorage");
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
console.log(
  failures === 0
    ? `all ${String(cases.length)} last-workspace cases passed`
    : `${String(cases.length - failures)}/${String(cases.length)} passed, ${String(failures)} failed`,
);
const host = globalThis as { process?: { exitCode?: number } };
if (host.process !== undefined && failures > 0) host.process.exitCode = 1;
