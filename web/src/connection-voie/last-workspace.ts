/**
 * Last Workspace this browser chose in a Project. New Chat reads it.
 *
 * In-memory first: Create Workspace and New Chat share one SPA document, and
 * storage can be partitioned or unavailable. sessionStorage/localStorage
 * survive a same-origin reload of this tab.
 */

const memory = new Map<string, string>();

function key(projectId: string): string {
  return `voie:lastWorkspace:${projectId}`;
}

function browserStore(name: "sessionStorage" | "localStorage"): Storage | null {
  try {
    const value = (globalThis as { sessionStorage?: Storage; localStorage?: Storage })[name];
    return value ?? null;
  } catch {
    return null;
  }
}

function readStore(store: Storage, projectId: string): string {
  try {
    return store.getItem(key(projectId))?.trim() ?? "";
  } catch {
    return "";
  }
}

function writeStore(store: Storage, projectId: string, workspaceId: string): void {
  try {
    store.setItem(key(projectId), workspaceId);
  } catch {
    // Private mode or quota; in-memory still serves this document.
  }
}

export function rememberWorkspace(projectId: string, workspaceId: string): void {
  const scope = projectId.trim();
  const id = workspaceId.trim();
  if (scope === "" || id === "") return;
  memory.set(scope, id);
  const session = browserStore("sessionStorage");
  if (session !== null) writeStore(session, scope, id);
  const local = browserStore("localStorage");
  if (local !== null) writeStore(local, scope, id);
}

export function lastWorkspace(projectId: string): string {
  const scope = projectId.trim();
  if (scope === "") return "";
  const fromMemory = memory.get(scope);
  if (fromMemory !== undefined && fromMemory !== "") return fromMemory;
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

/** Test hook: drop process-local memory. Storage is left for the caller. */
export function forgetWorkspacesForTests(): void {
  memory.clear();
}
