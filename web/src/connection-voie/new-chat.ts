/**
 * Document-scoped `voie-new-chat` listener.
 *
 * ChatHost remounts the DSH graph when leaving an established conversation
 * for New chat. Each boot's `apply()` must not stack another `window`
 * listener: one dispatch would then call `startSession` once per prior
 * mount. This module keeps exactly one listener and points it at the
 * currently mounted plugin's starter.
 */
const DSH_MOUNT_ID = "voie-dsh-root";

export const VOIE_NEW_CHAT_EVENT = "voie-new-chat";

export type NewChatStarter = (workspaceId?: string) => void;

let starter: NewChatStarter | null = null;
let listening = false;

function onNewChat(): void {
  const startSession = starter;
  if (startSession === null) return;
  const raw = document.getElementById(DSH_MOUNT_ID)?.dataset.voieWorkspaceId?.trim();
  const workspaceId = raw === undefined || raw === "" ? undefined : raw;
  // Do not run startSession inside the New-chat click / CDP evaluate:
  // connectWorkspace is async, but a synchronous throw (unknown
  // workspace) or a busy React flush can leave Runtime.evaluate hanging.
  window.setTimeout(() => {
    if (starter === null) return;
    try {
      if (workspaceId !== undefined) starter(workspaceId);
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
