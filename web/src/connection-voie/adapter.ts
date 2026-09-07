/**
 * One DSH adapter boundary for the native console.
 *
 * Portal and ChatHost talk only to this module. Carrier internals, DOM
 * datasets, and document-global DSH events stay inside `connection-voie`.
 */
export { mountDshApp, unmountDshApp } from "../dsh-lifecycle.ts";
export {
  setVoieDshHostContext,
  type VoieDshHostContext,
} from "./host-context.ts";
export { requestVoieOpenConversation } from "./session-nav.ts";
export { VOIE_NEW_CHAT_EVENT } from "./new-chat.ts";

import { VOIE_NEW_CHAT_EVENT as NEW_CHAT } from "./new-chat.ts";

/** Start a durable empty Session on the already-mounted graph. */
export function requestVoieNewChat(workspaceId?: string): void {
  const id = workspaceId?.trim() ?? "";
  window.dispatchEvent(
    id === ""
      ? new Event(NEW_CHAT)
      : new CustomEvent(NEW_CHAT, { detail: { workspaceId: id } }),
  );
}
