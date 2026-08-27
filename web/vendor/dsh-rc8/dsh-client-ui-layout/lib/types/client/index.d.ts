/**
 * Layout plugin, browser half: one register() call contributes AppFrame into
 * the runtime's built-in 'root' slot and, in the same breath, declares the
 * four child slots (declaration = exclusive render authority), seats the
 * layout store (panel geometry), and wires the panel-action service face.
 * ctx.layout is the cross-plugin panel-action contract; navigation state lives
 * with the runtime sessions service. A second effect seats the theme
 * presenter, which projects ctx.theme snapshots onto document.body.
 */
import type { ClientContext } from '@deepseek-ai/dsh-client-runtime/client';
export { LayoutController } from './service.ts';
export type { ILayout } from './service.ts';
declare module '@deepseek-ai/cordis' {
    interface Context {
        /** The outward face only; the concrete service stays inside this plugin. */
        layout: import('./service.ts').ILayout;
    }
}
declare module '@deepseek-ai/dsh-client-ui-slots' {
    interface SlotMap {
        /**
         * The whole left column. OCCUPIED by ui-sidebar's SidebarRoot, which
         * declares the workspace and settings seats inside it — registering here
         * replaces the navigation column outright rather than adding to it, and
         * the seats it declares disappear with it. To add something to the
         * sidebar, register into one of those inner seats instead.
         *
         * The occupant receives the frame's live column state (collapsed, width)
         * and is expected to render the compact control rail while collapsed.
         */
        'sidebar': {
            kind: 'single';
            scope: 'root';
            owner: SidebarOwnerProps;
        };
        /**
         * The whole center column, across both the no-session hero and a live
         * conversation. OCCUPIED by ui-conversation's ConversationRoot, which
         * declares the session body, composer, and input seats inside it —
         * registering here replaces the entire conversation surface (and removes
         * every seat it declares) rather than adding to it.
         *
         * Current-session-optional: the occupant owns both states without
         * changing its React identity, so it keeps its own state across a session
         * switch. It receives no owner props; session facts arrive through the
         * framework hooks of the `session-maybe` scope.
         */
        'conversation': {
            kind: 'single';
            scope: 'session-maybe';
            owner: ConvOwnerProps;
        };
        /**
         * The right details column, shown when the layout opens it. OCCUPIED by
         * ui-conversation's DetailsPanel, which declares the tool-details seat
         * inside it — registering here replaces the column and takes that seat
         * with it. Absent an occupant the column renders nothing.
         *
         * No owner props: the framework injects the session id and hooks for the
         * `session` scope, and `ctx.layout` owns whether the column is open.
         */
        'details': {
            kind: 'single';
            scope: 'session';
            owner: DetailsOwnerProps;
        };
        /**
         * Frame-wide floating layer, above every column and outside their scroll
         * containers. Deliberately generic and unowned by any feature: a badge, a
         * toast stack or a status pill all belong here, and entries order among
         * themselves. The layer itself is click-through — entries opt back into
         * pointer events — so an occupant never blocks the app underneath.
         *
         * This is the additive seat for a frame-wide surface of your own: a fresh
         * `id` is added beside the shipped entries instead of replacing them.
         */
        'shell.overlay': {
            kind: 'list';
            scope: 'root';
        };
    }
}
/** Sidebar owner share: live column state from the frame's concession solve. */
export interface SidebarOwnerProps {
    /** True when the sidebar is closed (the column renders the compact control rail). */
    collapsed: boolean;
    /** Rendered column width in px (SIDEBAR_COLLAPSED when collapsed). */
    width: number;
}
/** Conversation owner share: business state and actions belong to the registrant. */
export interface ConvOwnerProps {
}
/** Details owner share: empty — sessionId arrives as a framework-standard prop. */
export interface DetailsOwnerProps {
}
/** Required services (cordis fiber inject — the loader passes all module exports as an object plugin). */
export declare const inject: string[];
/**
 * Client plugin body: provide ctx.layout, then one register() call — AppFrame
 * into 'root' with the four child-slot declarations, the layout store seat,
 * and the inject hook that hands the store's bound actions to the service.
 * @param ctx - client root context.
 */
export declare function apply(ctx: ClientContext): void;
//# sourceMappingURL=index.d.ts.map