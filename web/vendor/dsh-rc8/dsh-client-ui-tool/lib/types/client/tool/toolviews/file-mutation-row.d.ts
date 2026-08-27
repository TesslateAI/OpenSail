import type { Context } from '@deepseek-ai/cordis';
import type { PropsLocale } from '@deepseek-ai/dsh-client-ui-slots';
import type { ToolCallViewProps } from '../../contract/slots.ts';
/** Full row props: the toolview runtime share plus the standard locale seat. */
type FileMutationRowProps = ToolCallViewProps & PropsLocale<'conversation'>;
/**
 * File-mutation row: icon + {Edit,Write} · {path} in the shared ToolRow chrome,
 * with the applied diff as the row's collapsed-by-default card body. The
 * summary is a path link (a file tool's interaction); the host's `openFile`
 * resolves it against the session cwd, so this passes the tool's own path
 * verbatim. An errored mutation has no diff card, so ToolRow surfaces the
 * model-facing error text through its Output section and its first line in the
 * collapsed summary instead.
 */
export declare function FileMutationRow({ toolName, block, cwd, home, openFile, inspect, t }: FileMutationRowProps): import("react").JSX.Element;
/**
 * The file-mutation rows as a plain registrant plugin following the chat
 * toolview declaration across independent activation and reload lifetimes.
 */
export declare const fileMutationToolview: {
    name: string;
    inject: string[];
    /**
     * Register the file-mutation row into the Tool-owned keyed view slot
     * under both mutation tool names.
     * @param ctx - registrant context (disposal rides ctx.effect inside slots.register).
     */
    apply(ctx: Context): void;
};
export {};
//# sourceMappingURL=file-mutation-row.d.ts.map