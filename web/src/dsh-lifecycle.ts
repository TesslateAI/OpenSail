import { AppWebEntry } from "@deepseek-ai/dsh-client-web";
import { unbindVoieNewChatListener } from "./connection-voie/new-chat.ts";
import { unbindVoieSessionNav } from "./connection-voie/session-nav.ts";

const DSH_MOUNT_ID = "voie-dsh-root";
let dshEntry: AppWebEntry | undefined;
let dshRun: Promise<void> | undefined;

/** Boot the pinned DSH graph into the already-mounted VOIE chat seat. */
export function mountDshApp(): Promise<void> {
  if (dshEntry !== undefined) return dshRun ?? Promise.resolve();
  const host = document.getElementById(DSH_MOUNT_ID);
  if (host === null) {
    throw new Error(`voie console: missing #${DSH_MOUNT_ID}`);
  }
  dshEntry = new AppWebEntry(host);
  dshRun = dshEntry.run();
  return dshRun;
}

/** Dispose the DSH graph and release the chat seat. */
export async function unmountDshApp(): Promise<void> {
  const entry = dshEntry;
  dshEntry = undefined;
  dshRun = undefined;
  unbindVoieNewChatListener();
  unbindVoieSessionNav();
  if (entry !== undefined) await entry.dispose();
}
