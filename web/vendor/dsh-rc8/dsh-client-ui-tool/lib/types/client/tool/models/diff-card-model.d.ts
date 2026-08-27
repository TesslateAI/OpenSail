/**
 * Pure derivation of the diff-card props from a frozen call slice: the
 * `card:'diff'` render intent the write/edit tools declare arrives on the
 * snapshot as `callView`/`resultView`, and this is the one place that turns
 * that pair into what {@link DiffBlock} draws. Both conversation render sites
 * (the chat tool row's expanded body and the details panel's Output section)
 * call this, so the hunks they show are derived once.
 * @module
 */
import type { DiffBlockProps } from '@deepseek-ai/dsh-client-ui-primitives';
import type { ToolCallBlock } from './tool-call-model.ts';
/**
 * Diff-body lines the chat row shows before collapsing the middle — half the
 * primitive's own default, which the details panel keeps. A chat row is a
 * summary surface inside the message flow: the flow must stay scannable across
 * many calls, while the details panel is the single-call reading surface. The
 * same split {@link CHAT_TERMINAL_MAX_LINES} draws for a terminal card, so the
 * two card kinds cap a long body at the same place in the flow. A design
 * constant of this UI's row geometry, not a deployment choice.
 */
export declare const CHAT_DIFF_MAX_LINES = 8;
/**
 * The {@link DiffBlock} props this derivation owns. Picked off the primitive's
 * props so the two stay in step; `maxLines`/`className` belong to each render
 * site.
 */
export interface DiffCardModel {
    /**
     * The props {@link DiffBlock} draws. Held as a nested object so a render site
     * spreads exactly the primitive's own surface and can never leak a
     * neighbouring field into it.
     */
    card: Pick<DiffBlockProps, 'diffs'>;
}
/**
 * Derive the diff-card props for a tool call, or null when this call is not a
 * diff card and belongs on the generic path.
 *
 * The result side is authoritative once the call settles: the write/edit tools
 * return the applied contextual hunks there (an edit's real before/after, a
 * create's whole-file diff), which replace the call-time diff derived from the
 * arguments alone. While the call is still running only the call side exists,
 * so a running write/edit shows its intended change. Null is the documented
 * generic-card default and covers every non-diff card — including a `card`
 * value this UI version does not know, which arrives over the wire and cannot
 * be trusted to be one of the compiled variants — and a settled call whose
 * result view is generic (how write/edit keep their execution errors on the
 * generic path).
 *
 * This derivation consumes only `diffs`; the render intent's `title` field is
 * deliberately dropped. The row supplies its own title (`Edit`/`Write · path`
 * from the args), which outranks the view's `title`. A tool that names its own
 * diff header therefore does not surface that text on the Web row.
 * @param block - RunningToolCall or ToolResultNode off the snapshot caches.
 * @returns the diff-card props, or null for the generic path.
 */
export declare function diffCardModel(block: ToolCallBlock): DiffCardModel | null;
//# sourceMappingURL=diff-card-model.d.ts.map