import type { TerminalBlockLabels, TerminalBlockProps } from '@deepseek-ai/dsh-client-ui-primitives';
import type { TranslateNS } from '@deepseek-ai/dsh-client-ui-slots';
import type { ToolCallBlock } from './tool-call-model.ts';
/**
 * Build the TerminalBlock display copy from the conversation locale seat —
 * the one place the primitive's label surface pairs with this package's
 * dictionary, shared by every terminal render site (chat row, bash row,
 * details panel).
 * @param t - the render site's conversation locale seat.
 * @returns the full label set for {@link TerminalBlockProps}'s `labels`.
 */
export declare function terminalBlockLabels(t: TranslateNS<'conversation'>): TerminalBlockLabels;
/**
 * The {@link TerminalBlock} props this derivation owns. Picked off the
 * primitive's props so the two stay in step; `home` is absent because the web
 * client has no home path for the session host (a cwd renders as its last
 * path segment), and `maxLines`/`className` belong to each render site.
 */
export interface TerminalCardModel {
    /**
     * The props {@link TerminalBlock} draws. Held as a nested object so a render
     * site spreads exactly the primitive's own surface and can never leak a
     * neighbouring field into it.
     */
    card: Pick<TerminalBlockProps, 'command' | 'cwd' | 'output' | 'exitCode' | 'signal' | 'running'>;
    /**
     * The call view's model-authored description, which the contract defines as
     * rendering ABOVE the card (the card itself has no description slot). Absent
     * when the presenter supplied none, or when the window dropped the call side;
     * a row then keeps its args-derived summary.
     */
    description: string | undefined;
}
/**
 * True when a settled terminal card reports a failing exit — a non-zero code
 * or a terminating signal. The bash tool settles a failing command as a
 * completed call (`isError` stays false: the exit status is result data), so
 * this is the collapsed row's only failure signal; without it the red exit
 * pill would be visible only after expanding the card.
 * @param model - a derived terminal card.
 * @returns whether the card's exit status is a failure.
 */
export declare function terminalFailed(model: TerminalCardModel): boolean;
/**
 * Derive the terminal-card props for a tool call, or null when this call is
 * not a terminal card and belongs on the generic path.
 *
 * The call side supplies the command and its working directory; the result
 * side supplies the captured output and exit status. Three cases produce
 * null, all of them the documented generic-card default:
 *
 * - Neither side declares `card:'terminal'` — including a `card` value this
 *   UI version does not know, which arrives over the wire and therefore
 *   cannot be trusted to be one of the compiled variants.
 * - A settled call whose result view is not a terminal card: the result
 *   presentation decides how the settled call renders, and the bash tool
 *   returns a generic fenced card for an execution error or a background
 *   start, whose text and error styling the generic path preserves.
 *
 * Window truncation can drop the call head from a settled result (see
 * `ToolResultNode.call`/`callView` in dsh-client-runtime), leaving a terminal
 * result with no call side. That still renders: the command falls back to the
 * result view's replacement title, then to an empty command (the prompt line
 * draws bare), and the prompt shows no cwd.
 * @param block - RunningToolCall or ToolResultNode off the snapshot caches.
 * @param sessionCwd - the session workspace root, which resolves an omitted or
 *   relative view cwd (see {@link resolveTerminalCwd}); absent leaves both unresolved.
 * @returns the terminal-card props, or null for the generic path.
 */
export declare function terminalCardModel(block: ToolCallBlock, sessionCwd?: string): TerminalCardModel | null;
//# sourceMappingURL=terminal-card-model.d.ts.map