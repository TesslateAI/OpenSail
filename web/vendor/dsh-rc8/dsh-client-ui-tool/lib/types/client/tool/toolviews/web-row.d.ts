import type { Context } from '@deepseek-ai/cordis';
import type { PropsLocale } from '@deepseek-ai/dsh-client-ui-slots';
import type { ToolCallViewProps } from '../../contract/slots.ts';
/** Full row props: the toolview runtime share plus the standard locale seat. */
type WebRowProps = ToolCallViewProps & PropsLocale<'conversation'>;
/**
 * Web row: icon + Search/Fetch · {summary} in the shared ToolRow chrome, with
 * the completed retrieval's web card as the row's collapsed-by-default card
 * body. The row discriminates on `toolName` only to pick its icon and title.
 */
export declare function WebRow({ toolName, block, inspect, t }: WebRowProps): import("react").JSX.Element;
/**
 * The web rows follow the atomic Tool-view declaration across activation and
 * reload. One WebRow component registers under both web tool names.
 */
export declare const webToolview: {
    name: string;
    inject: string[];
    /**
     * Register the web row under both web tool names' keyed toolview holes.
     * @param ctx - registrant context (disposal rides ctx.effect inside slots.register).
     */
    apply(ctx: Context): void;
};
export {};
//# sourceMappingURL=web-row.d.ts.map