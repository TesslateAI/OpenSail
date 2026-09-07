/**
 * Document-scoped `voie-new-chat` listener.
 *
 * The DSH graph stays mounted across New chat. Each boot's `apply()` must
 * not stack another `window` listener: one dispatch would then call
 * `startSession` once per prior mount. This module keeps exactly one
 * listener and points it at the currently mounted plugin's starter.
 */
import { getVoieDshHostContext } from "./host-context.ts";
import { lastWorkspace } from "./last-workspace.ts";

export const VOIE_NEW_CHAT_EVENT = "voie-new-chat";

export type NewChatStarter = (workspaceId?: string) => void;

let starter: NewChatStarter | null = null;
let listening = false;

function workspaceIdFromEvent(event: Event): string {
  const detail = (event as CustomEvent<{ workspaceId?: unknown }>).detail;
  if (typeof detail?.workspaceId !== "string") return "";
  return detail.workspaceId.trim();
}

function resolveWorkspaceId(event: Event): string {
  const fromEvent = workspaceIdFromEvent(event);
  if (fromEvent !== "") return fromEvent;
  const ctx = getVoieDshHostContext();
  const fromStorage = lastWorkspace(ctx.projectId);
  if (fromStorage !== "") return fromStorage;
  return ctx.workspaceId?.trim() ?? "";
}

function onNewChat(event: Event): void {
  const startSession = starter;
  if (startSession === null) return;
  // Resolve at dispatch, then run on a macrotask: connect/create is async,
  // but a synchronous throw must not freeze the New-chat click / CDP evaluate.
  const workspaceId = resolveWorkspaceId(event);
  window.setTimeout(() => {
    if (starter === null) return;
    try {
      if (workspaceId !== "") starter(workspaceId);
      else starter();
    } catch {
      // DSH already logs connect failures; a missing service must not
      // freeze the New-chat surface.
    }
  }, 0);
}

/** Point the single document listener at this plugin boot's starter. */
export function bindVoieNewChatListener(startSession: NewChatStarter): void {
  starter = startSession;
  if (listening) return;
  window.addEventListener(VOIE_NEW_CHAT_EVENT, onNewChat);
  listening = true;
}

/** Drop the document listener when the DSH graph is disposed. */
export function unbindVoieNewChatListener(): void {
  if (!listening) return;
  window.removeEventListener(VOIE_NEW_CHAT_EVENT, onNewChat);
  listening = false;
  starter = null;
}
