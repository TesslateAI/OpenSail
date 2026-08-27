import type { Context } from '@deepseek-ai/cordis';
import type { PropsLocale } from '@deepseek-ai/dsh-client-ui-slots';
import type { ToolCallViewProps } from '../../contract/slots.ts';
/** Todo row props: the toolview runtime share plus the standard locale seat. */
type TodoRowProps = ToolCallViewProps & PropsLocale<'conversation'>;
/** One-line plan update row (the whole row toggles the call's Input/Output
 *  sections, ToolRow's unified expand). Non-ok execution states keep the
 *  shared row's dot semantics — a cancelled call wrote no todo/write, so it
 *  must not read as a completed update. */
export declare function TodoRow({ toolName, block, inspect, t }: TodoRowProps): import("react").JSX.Element;
/**
 * The todo row as a plain registrant plugin following the atomic Tool-view
 * declaration across independent activation and reload lifetimes.
 */
export declare const todoToolview: {
    name: string;
    inject: string[];
    /**
     * Register the todo row into the Tool-owned keyed view slot.
     * @param ctx - registrant context (disposal rides ctx.effect inside slots.register).
     */
    apply(ctx: Context): void;
};
export {};
//# sourceMappingURL=todo-row.d.ts.map