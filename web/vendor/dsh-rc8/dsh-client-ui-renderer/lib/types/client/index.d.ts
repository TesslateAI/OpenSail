import type { Context } from '@deepseek-ai/cordis';
import type { SnapshotSelectorHook } from '@deepseek-ai/dsh-client-ui-slots';
/** Selector hook over a session's conversation snapshot. */
export type UseSession<Snap extends object = object> = SnapshotSelectorHook<Snap>;
export type { ChainRenderOpts, HostObservable, RenderOpts, SessionProvideInfo, SnapshotSelectorHook, SlotRenderer, SlotRendererHost, StoreInstanceLike, } from '@deepseek-ai/dsh-client-ui-slots';
export type { SessionProviderProps } from './session-provider.tsx';
/** Mount operation exposed to the framework-free boot kernel. */
export interface UiRendererService {
    /**
     * Mount the assembled application into the supplied element.
     * @param container - Application mount point.
     * @returns Disposer that unmounts the React root.
     */
    mount: (container: HTMLElement) => () => void;
}
declare module '@deepseek-ai/cordis' {
    interface Context {
        /** Mount face provided after the UI renderer activates. */
        uiRenderer: UiRendererService;
    }
}
/** Services required before application assembly. */
export declare const inject: string[];
/**
 * Install the slot renderer and provide the application mount face.
 * @param ctx - Plugin context.
 */
export declare function apply(ctx: Context): void;
//# sourceMappingURL=index.d.ts.map