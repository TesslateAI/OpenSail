import type { ReadBlockProps } from '@deepseek-ai/dsh-client-ui-primitives';
import { type ToolCallBlock } from './tool-call-model.ts';
/**
 * Content lines the chat row's resident read body shows before collapsing the
 * middle — half the primitive's own default, which the details panel keeps. A
 * chat row is a summary surface inside the message flow: the flow must stay
 * scannable across many calls, while the details panel is the single-call
 * reading surface. A design constant of this UI's row geometry, not a
 * deployment choice, so it is fixed here rather than a plugin Config field. The
 * same split [`CHAT_TERMINAL_MAX_LINES`](./terminal-card-model.ts) draws for
 * terminal output.
 */
export declare const CHAT_READ_MAX_LINES = 8;
/**
 * The {@link ReadBlock} props this derivation owns. Picked off the primitive's
 * props so the two stay in step; `maxLines`/`className` belong to each render
 * site.
 */
export type ReadCardModel = Pick<ReadBlockProps, 'label' | 'lines' | 'totalLines' | 'lang'>;
/**
 * Derive the read-card props for a tool call, or null when this call is not a
 * read card and belongs on the generic path.
 *
 * The read card is result-side only, so only a settled call whose result view
 * declares `card:'read'` produces one. Every other case is null — the
 * documented generic-card default:
 *
 * - A running call: it has no result view yet, and a read carries no content at
 *   call time.
 * - A settled call whose result view is not a read card — including a `card`
 *   value this UI version does not know, which arrives over the wire and cannot
 *   be trusted to be one of the compiled variants, and the read tool's own
 *   generic fallback for an error result or a non-envelope body.
 *
 * The label is the read view's `title` when the tool supplied one (the
 * presentation contract's replacement-title rule), otherwise the file path
 * shortened the same way the row summary is: workspace-relative first, then
 * POSIX `~` for a leftover host-home path.
 * @param block - RunningToolCall or ToolResultNode off the snapshot caches.
 * @param sessionCwd - the session workspace root; a workspace-rooted absolute
 *   path label displays relative to it. Absent leaves the path as authored.
 * @param home - host account home; a leftover POSIX home path displays as `~`.
 * @returns the read-card props, or null for the generic path.
 */
export declare function readCardModel(block: ToolCallBlock, sessionCwd?: string, home?: string): ReadCardModel | null;
//# sourceMappingURL=read-card-model.d.ts.map