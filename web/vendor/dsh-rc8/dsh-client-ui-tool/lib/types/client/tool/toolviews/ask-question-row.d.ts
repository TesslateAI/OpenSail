import type { Context } from '@deepseek-ai/cordis';
import type { PropsLocale } from '@deepseek-ai/dsh-client-ui-slots';
import type { ToolCallViewProps } from '../../contract/slots.ts';
/** Full row props: the toolview runtime share plus the standard locale seat. */
type AskQuestionRowProps = ToolCallViewProps & PropsLocale<'conversation'>;
/** One-line question-interaction row (the whole row toggles the call's
 *  Input/Output sections, ToolRow's unified expand). */
export declare function AskQuestionRow({ toolName, block, inspect, t }: AskQuestionRowProps): import("react").JSX.Element;
/**
 * The ask-question row as a plain registrant plugin following the chat
 * toolview declaration across independent activation and reload lifetimes.
 */
export declare const askQuestionToolview: {
    name: string;
    inject: string[];
    /**
     * Register the ask-question row into the Tool-owned keyed view slot.
     * @param ctx - registrant context (disposal rides ctx.effect inside slots.register).
     */
    apply(ctx: Context): void;
};
export {};
//# sourceMappingURL=ask-question-row.d.ts.map