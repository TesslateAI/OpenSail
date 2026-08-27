import type { PropsRenderSlots, PropsRuntime, PropsStore } from '@deepseek-ai/dsh-client-ui-slots';
import type { createLayoutStore } from './stores.ts';
/** Full composed props: runtime share + child-slot render share + store share. */
export type AppFrameProps = PropsRuntime<'root'> & PropsRenderSlots<'sidebar' | 'conversation' | 'details' | 'shell.overlay'> & PropsStore<ReturnType<typeof createLayoutStore>>;
/** The three-column frame (see module doc). */
export declare function AppFrame({ useStore, useSessions, actions, renderSlot, }: AppFrameProps): import("react").JSX.Element;
//# sourceMappingURL=AppFrame.d.ts.map