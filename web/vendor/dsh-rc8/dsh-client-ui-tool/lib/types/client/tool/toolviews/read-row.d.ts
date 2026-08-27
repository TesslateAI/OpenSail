import type { Context } from '@deepseek-ai/cordis';
import type { PropsLocale } from '@deepseek-ai/dsh-client-ui-slots';
import type { ToolCallViewProps } from '../../contract/slots.ts';
/** Full row props: the toolview runtime share plus the standard locale seat. */
type ReadRowProps = ToolCallViewProps & PropsLocale<'conversation'>;
/**
 * Read row: icon + Read · {path} in the shared ToolRow chrome, with the file's
 * read card as the row's collapsed-by-default card body. The summary path is an
 * openable host link when the row names a single file.
 */
export declare function ReadRow({ toolName, block, cwd, home, openFile, inspect, t }: ReadRowProps): import("react").JSX.Element;
/**
 * The read row as a plain registrant plugin following the atomic Tool-view
 * declaration across independent activation and reload lifetimes.
 */
export declare const readToolview: {
    name: string;
    inject: string[];
    /**
     * Register the read row into the Tool-owned keyed view slot.
     * @param ctx - registrant context (disposal rides ctx.effect inside slots.register).
     */
    apply(ctx: Context): void;
};
export {};
//# sourceMappingURL=read-row.d.ts.map