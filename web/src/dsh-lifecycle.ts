import { AppWebEntry } from "@deepseek-ai/dsh-client-web";

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
  if (entry !== undefined) await entry.dispose();
}
