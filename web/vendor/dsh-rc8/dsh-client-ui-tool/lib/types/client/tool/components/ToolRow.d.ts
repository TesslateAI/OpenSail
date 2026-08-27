import { type ReactNode } from 'react';
import type { WebBlockProps } from '@deepseek-ai/dsh-client-ui-primitives';
import type { TranslateNS } from '@deepseek-ai/dsh-client-ui-slots';
import { type DiffCardModel } from '../models/diff-card-model.ts';
import { type ReadCardModel } from '../models/read-card-model.ts';
import { type SearchCardModel } from '../models/search-card-model.ts';
import { type TerminalCardModel } from '../models/terminal-card-model.ts';
import type { ToolRowState, ToolRowVariant } from '../models/tool-call-model.ts';
export interface ToolRowProps {
    /** The render site's conversation locale seat (terminal/code body copy). */
    t: TranslateNS<'conversation'>;
    variant: ToolRowVariant;
    /** Wire tool name for tool-owned styling layered over the generic variant. */
    toolName?: string | undefined;
    /** Leading 16px tool icon, shown while collapsed and not running/failed. */
    icon: ReactNode;
    title: string;
    summary: string;
    /**
     * Trailing summary fragment rendered outside the ellipsized summary text, so
     * a narrow row clips the summary before this. For a fragment whose whole
     * value is surviving that clip — the todo row's parallel-active count.
     * null/absent = the summary is the whole collapsed content. Dropped on an
     * error row, whose collapsed summary is the failure line instead.
     */
    summarySuffix?: string | null | undefined;
    /** Expanded-body input text; null = no input section. */
    body: string | null;
    /** Flattened result text for the expanded Output section; null/absent = no output section. */
    output?: string | null | undefined;
    /** Error first line shown as the collapsed summary on an error row; null/absent = keep `summary`. */
    errorSummary?: string | null | undefined;
    /**
     * Terminal-card material for a call whose render intent is a terminal card
     * (derived by `terminalCardModel`); it replaces the text sections when
     * present. A call carries at most one card kind, so the card props below are
     * mutually exclusive.
     */
    terminal?: TerminalCardModel | null | undefined;
    /**
     * Diff-card material for a call whose render intent is a diff card (derived by
     * `diffCardModel`); it replaces the text body when present, the same way
     * `terminal` does.
     */
    diff?: DiffCardModel | null | undefined;
    /**
     * Read-card material for a call whose render intent is a read card (derived by
     * `readCardModel`); it replaces the text body with the file's line-numbered,
     * syntax-highlighted window when present.
     */
    read?: ReadCardModel | null | undefined;
    /**
     * Search-card material for a call whose render intent is a search card
     * (derived by `searchCardModel`); it replaces the text body with grouped
     * matches or a path list when present.
     */
    search?: SearchCardModel | null | undefined;
    /**
     * Web-card material for a call whose render intent is a web card (derived by
     * `webCardModel`); it replaces the text body with the retrieval's citation
     * list or fetched-source card when present.
     */
    web?: WebBlockProps | null | undefined;
    state: ToolRowState;
    /**
     * Filesystem path from tool args; when set with onOpenFile, the summary
     * renders as a hover-underline link that opens the host default app.
     */
    filePath?: string | undefined;
    /** Open the path with the host OS default application (already cwd-resolved). */
    onOpenFile?: ((path: string) => void) | undefined;
    /**
     * Jump to this call in the trajectory view: a hover-revealed Inspect pill
     * over the expanded body. Absent = no affordance.
     */
    inspect?: (() => void) | undefined;
}
export declare function ToolRow({ t, variant, toolName, icon, title, summary, summarySuffix, body, output, errorSummary, terminal, diff, read, search, web, state, filePath, onOpenFile, inspect, }: ToolRowProps): import("react").JSX.Element;
//# sourceMappingURL=ToolRow.d.ts.map