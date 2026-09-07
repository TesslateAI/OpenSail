/**
 * Identity the portal passes into the DSH connection plugin.
 *
 * Scope, workspace, and conversation ids live here — not on DOM datasets
 * or document-global custom events consumed outside this adapter.
 */
export type VoieDshHostContext = {
  projectId: string;
  workspaceId?: string;
  conversationId?: string;
};

let context: VoieDshHostContext = { projectId: "" };

/** Portal ChatHost writes the current seat identity before the graph boots. */
export function setVoieDshHostContext(next: VoieDshHostContext): void {
  const workspaceId = next.workspaceId?.trim();
  const conversationId = next.conversationId?.trim();
  context = {
    projectId: next.projectId,
    ...(workspaceId === undefined || workspaceId === ""
      ? {}
      : { workspaceId }),
    ...(conversationId === undefined || conversationId === ""
      ? {}
      : { conversationId }),
  };
}

export function getVoieDshHostContext(): VoieDshHostContext {
  return context;
}
