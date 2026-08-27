/**
 * The root entry's transient layout store: panel geometry as plain widths in
 * px (0 = closed). Module level exports the factory only — a module-level
 * handle would pin the store's identity in the module
 * cache (a de-facto singleton surviving plugin reloads). register() receives
 * the factory (exclusive use: the framework instantiates per entry), AppFrame
 * derives its PropsStore share from the return type, and the service face
 * receives the bound actions through the registration's inject hook.
 */
import { type EngineStoreHandle } from '@deepseek-ai/dsh-client-runtime/client';
/**
 * Layout store state: panel width preferences in px (0 = closed), plus the
 * narrow-viewport pair — `narrow` mirrors AppFrame's breakpoint reading
 * (viewport < SIDEBAR_AUTO_COLLAPSE) so toggleSidebar can pick semantics, and
 * `narrowExpanded` is the manual override that re-expands the auto-collapsed
 * sidebar over the squeezed center without rewriting the width preference.
 */
type LayoutState = {
    sidebar: number;
    details: number;
    narrow: boolean;
    narrowExpanded: boolean;
};
/**
 * Annotation twin of the actions literal below (the export needs a declared
 * return type); drift fails assignability at the defineStore call.
 */
type LayoutActions = {
    setSidebar: (draft: LayoutState, px: number) => void;
    setDetails: (draft: LayoutState, px: number) => void;
    toggleSidebar: (draft: LayoutState) => void;
    setNarrow: (draft: LayoutState, narrow: boolean) => void;
    openDetails: (draft: LayoutState) => void;
    closeDetails: (draft: LayoutState) => void;
};
/**
 * Create the layout panel store handle. The preference IS the width, so
 * closing a panel forgets its drag width — reopening restores the contract
 * default. Actions are the complete write set: drag writes clamp
 * into the panel's contract range and never cross the open/closed line;
 * open/close transitions write 0 / the default explicitly. Below the
 * auto-collapse breakpoint (AppFrame feeds setNarrow) the sidebar toggle
 * flips the narrowExpanded override instead of the preference.
 * @returns the store handle (spec + type + identity + factory in one).
 */
export declare function createLayoutStore(): EngineStoreHandle<LayoutState, LayoutActions>;
export {};
//# sourceMappingURL=stores.d.ts.map