import type { ToolCallBlock, ToolResultNode } from '@deepseek-ai/dsh-client-runtime/client';
export type { ToolCallBlock } from '@deepseek-ai/dsh-client-runtime/client';
/** Tool-call row variants selected by the generic atomic renderer. */
export type ToolRowVariant = 'search' | 'read' | 'bash' | 'write' | 'edit' | 'code' | 'others';
/** Row state semantic; colors self-supplied via StateDot (design gives none). */
export type ToolRowState = 'running' | 'ok' | 'error' | 'stopped';
/** Figma row titles per variant (design literals, not translatable copy). */
export declare const VARIANT_TITLES: Record<ToolRowVariant, string>;
/**
 * Classify a tool name into its row variant.
 * @param toolName - wire tool name.
 * @returns matching variant, others when unknown.
 */
export declare function classifyTool(toolName: string): ToolRowVariant;
/** Everything ToolRow needs, derived once from the frozen slice. */
export interface ToolRowModel {
    variant: ToolRowVariant;
    title: string;
    summary: string;
    /**
     * Filesystem path from args (`path` / `file_path`) when the row is a file
     * tool; absent for URL reads and non-file tools. The chat view resolves
     * relative values against the session cwd before opening.
     */
    filePath: string | undefined;
    /** Expanded-body input text (pretty args); null = no input section. */
    body: string | null;
    /** Flattened result text ({@link resultText}); null while running or when the result carries no text. */
    output: string | null;
    /** First line of the result text on an error row; null for every other state. */
    errorSummary: string | null;
    state: ToolRowState;
}
/**
 * Flatten a settled result's content blocks to display text: text blocks
 * verbatim, other block shapes as pretty JSON. Empty content on a failed call
 * falls back to the structured error's `name: code` line.
 * @param node - the settled result node.
 * @returns the flattened result text (may be empty).
 */
export declare function resultText(node: ToolResultNode): string;
/**
 * Strip the workspace root from a workspace-rooted absolute path (display only).
 * @param text - the path to shorten.
 * @param cwd - session workspace root; absent or empty leaves the path unchanged.
 * @returns the path relative to the workspace root, or unchanged when it is not rooted there.
 */
export declare function relativizeToCwd(text: string, cwd: string | undefined): string;
/**
 * Derive the full row model from a frozen call slice.
 * @param toolName - wire tool name (dispatch-supplied; survives windowless results).
 * @param block - RunningToolCall or ToolResultNode off the snapshot caches.
 * @param cwd - session workspace root; workspace-rooted path summaries display relative to it.
 * @param home - host account home; a leftover POSIX home path displays as `~`.
 * @returns the row model.
 */
export declare function toolRowModel(toolName: string, block: ToolCallBlock, cwd?: string, home?: string): ToolRowModel;
//# sourceMappingURL=tool-call-model.d.ts.map