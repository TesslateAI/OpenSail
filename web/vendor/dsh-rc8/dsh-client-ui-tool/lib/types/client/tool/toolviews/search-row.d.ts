import type { Context } from '@deepseek-ai/cordis';
import type { PropsLocale } from '@deepseek-ai/dsh-client-ui-slots';
import type { ToolCallViewProps } from '../../contract/slots.ts';
/** Full row props: the toolview runtime share plus the standard locale seat. */
type SearchRowProps = ToolCallViewProps & PropsLocale<'conversation'>;
/**
 * Search row: icon + Grep/Glob · {summary} in the shared ToolRow chrome, with the
 * completed search's card as the row's collapsed-by-default card body (a capped
 * search's recovery footer rides below it, inside ToolRow). Registered under
 * both `grep` and `glob`; the derived model's `kind` decides the card shape. A
 * settled call with no search card surfaces its model-facing text through
 * ToolRow's Output section, since the keyed SearchRow owns this render slot.
 */
export declare function SearchRow({ toolName, block, inspect, t }: SearchRowProps): import("react").JSX.Element;
/**
 * The search view follows the atomic Tool-view declaration across activation
 * and reload. One component registers under both keys because `grep` and
 * `glob` are the same visual object discriminated by the result view's `kind`.
 */
export declare const searchToolview: {
    name: string;
    inject: string[];
    /**
     * Register the search row into the Tool-owned keyed view slot under both
     * the `grep` and `glob` tool names.
     * @param ctx - registrant context (disposal rides ctx.effect inside slots.register).
     */
    apply(ctx: Context): void;
};
export {};
//# sourceMappingURL=search-row.d.ts.map