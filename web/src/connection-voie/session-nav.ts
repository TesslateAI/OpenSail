/**
 * Drive the live DSH session selection from the portal URL.
 *
 * ChatHost must not remount the graph when the conversation id changes:
 * the module loader can boot only once. The portal writes the id onto
 * `#voie-dsh-root` and this binder calls `sessions.open` once that id
 * appears in the session list.
 */
import { getVoieDshHostContext } from "./host-context.ts";

export const VOIE_OPEN_CONVERSATION_EVENT = "voie-open-conversation";

export type SessionListMirror = {
  getSnapshot: () => { byId: Record<string, unknown>; current: string | undefined };
  subscribe: (listener: () => void) => () => void;
};

export type VoieSessionNav = {
  open: (id: string) => void;
  clear?: () => void;
  list: SessionListMirror;
};

let nav: VoieSessionNav | null = null;
let listening = false;
let pending: (() => void) | undefined;
let wanted: string | undefined;
let opening = false;

function conversationIdFromHost(): string | undefined {
  return getVoieDshHostContext().conversationId;
}

function conversationIdFromEvent(event: Event): string | undefined {
  const detail = (event as CustomEvent<{ conversationId?: unknown }>).detail;
  if (typeof detail?.conversationId !== "string") return undefined;
  const id = detail.conversationId.trim();
  return id === "" ? undefined : id;
}

function cancelPending(): void {
  pending?.();
  pending = undefined;
}

function openWhenListed(sessions: VoieSessionNav, id: string): void {
  wanted = id;
  cancelPending();
  const tryOpen = (): boolean => {
    if (wanted !== id) return true;
    const snap = sessions.list.getSnapshot();
    if (snap.byId[id] === undefined) return false;
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

function onOpen(event: Event): void {
  const sessions = nav;
  const id = conversationIdFromEvent(event);
  if (sessions === null || id === undefined) return;
  openWhenListed(sessions, id);
}

function syncFromHost(): void {
  const sessions = nav;
  if (sessions === null) return;
  const id = conversationIdFromHost();
  if (id === undefined) {
    cancelPending();
    wanted = undefined;
    return;
  }
  openWhenListed(sessions, id);
}

/** Point the live graph at the conversation the portal URL currently names. */
export function bindVoieSessionNav(sessions: VoieSessionNav): void {
  nav = sessions;
  if (!listening) {
    window.addEventListener(VOIE_OPEN_CONVERSATION_EVENT, onOpen);
    listening = true;
  }
  syncFromHost();
}

/** Drop the document listener and in-flight open when the graph is disposed. */
export function unbindVoieSessionNav(): void {
  if (listening) {
    window.removeEventListener(VOIE_OPEN_CONVERSATION_EVENT, onOpen);
    listening = false;
  }
  cancelPending();
  nav = null;
  wanted = undefined;
  opening = false;
}

/** Portal URL changed to an established conversation; open it on the live graph. */
export function requestVoieOpenConversation(conversationId: string): void {
  window.dispatchEvent(
    new CustomEvent(VOIE_OPEN_CONVERSATION_EVENT, {
      detail: { conversationId },
    }),
  );
}
