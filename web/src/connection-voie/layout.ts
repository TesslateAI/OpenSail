/**
 * VOIE layout face for the conversation graph.
 *
 * Stock DSH `ui-layout` paints a three-column AppFrame (sidebar | chat |
 * details). VOIE already owns the product sidebar, so that package is not
 * composed. This module still provides `ctx.layout` (conversation opens the
 * details column through it) and a store the root frame reads.
 */
import { defineStore } from "@deepseek-ai/dsh-client-runtime/client";

export type VoieLayoutState = {
  details: number;
};

export type VoieLayoutActions = {
  openDetails: () => void;
  closeDetails: () => void;
  toggleSidebar: () => void;
};

/** Panel store seated on the VOIE `root` registration. */
export function createVoieLayoutStore() {
  return defineStore({
    init: (): VoieLayoutState => ({ details: 0 }),
    actions: {
      openDetails: (draft: VoieLayoutState) => {
        if (draft.details === 0) draft.details = 380;
      },
      closeDetails: (draft: VoieLayoutState) => {
        draft.details = 0;
      },
      toggleSidebar: () => {
        // Product navigation lives in PortalShell. There is no DSH sidebar.
      },
    },
  });
}

/** Cross-plugin panel face conversation expects as `ctx.layout`. */
export class VoieLayoutController {
  #panels: VoieLayoutActions | undefined;

  attachPanels(actions: VoieLayoutActions): void {
    this.#panels = actions;
  }

  toggleSidebar(): void {
    this.#require().toggleSidebar();
  }

  openDetails(): void {
    this.#require().openDetails();
  }

  closeDetails(): void {
    this.#require().closeDetails();
  }

  #require(): VoieLayoutActions {
    if (this.#panels === undefined) {
      throw new Error("layout: panel actions not wired (root entry not mounted)");
    }
    return this.#panels;
  }
}
