import type { Context } from '@deepseek-ai/cordis';
import type { PropsLocale } from '@deepseek-ai/dsh-client-ui-slots';
import type { ToolCallViewProps } from '../../contract/slots.ts';
/** Bash row props: the toolview runtime share plus the standard locale seat. */
type BashRowProps = ToolCallViewProps & PropsLocale<'conversation'>;
/**
 * Bash row: icon + Bash · {description} in the shared ToolRow chrome, the
 * whole row toggling the command's terminal or generic error card (ToolRow's unified
 * expand interaction, replicated locally per the registrant posture).
 */
export declare function BashRow({ toolName, block, sessionId, useSessions, inspect, t }: BashRowProps): import("react").JSX.Element;
/**
 * The sample as a plain registrant plugin. Slot injection follows the chat
 * toolview declaration across independent activation and reload lifetimes.
 */
export declare const bashToolviewSample: {
    name: string;
    inject: string[];
    /**
     * Register the bash row into the Tool-owned keyed view slot.
     * @param ctx - registrant context (disposal rides ctx.effect inside slots.register).
     */
    apply(ctx: Context): void;
};
export {};
//# sourceMappingURL=bash-sample.d.ts.map