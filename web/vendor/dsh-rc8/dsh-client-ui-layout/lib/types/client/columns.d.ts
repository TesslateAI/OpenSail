/**
 * Pure concession-chain column solver for the three-column AppFrame.
 * Chain order is fixed by contract: keep center >= CENTER_MIN by shrinking
 * details, then auto-closing it (derived zero width — preferred width
 * preferences are never rewritten, so widening the window restores them).
 * The sidebar never concedes: its rendered width is always the drag
 * preference (or the collapsed rail), and center absorbs any remaining
 * deficit as the last resort. Inputs are the layout store's plain width
 * preferences (0 = closed); a closed sidebar resolves to the fixed
 * SIDEBAR_COLLAPSED control rail while closed details resolve to zero width.
 * The SIDEBAR_AUTO_COLLAPSE breakpoint is consumed by AppFrame, which decides
 * the effective sidebar preference before solving; the solver itself stays
 * breakpoint-free.
 */
/** Resolved widths for one frame; center may drop below CENTER_MIN only at the final fallback. */
export interface Columns {
    sidebar: number;
    center: number;
    details: number;
}
/** Center column floor; only the final fallback may go below it. */
export declare const CENTER_MIN = 640;
/** Sidebar drag clamp floor. */
export declare const SIDEBAR_MIN = 264;
/** Sidebar drag clamp ceiling. */
export declare const SIDEBAR_MAX = 420;
/** Sidebar width before any user drag. */
export declare const SIDEBAR_DEFAULT = 280;
/** Closed-sidebar rail: a 24px icon column between 16px horizontal paddings. */
export declare const SIDEBAR_COLLAPSED = 56;
/** Viewport width below which the sidebar auto-collapses to the rail (deepsuite
 * LG breakpoint); a manual toggle below it re-expands over the squeezed center
 * (stores.ts narrowExpanded). */
export declare const SIDEBAR_AUTO_COLLAPSE = 1024;
/** Details drag clamp floor. */
export declare const DETAILS_MIN = 300;
/** Details drag clamp ceiling. */
export declare const DETAILS_MAX = 520;
/** Details width before any user drag. */
export declare const DETAILS_DEFAULT = 360;
/**
 * Clamp a panel width into its contract range.
 * @param px - requested width.
 * @param min - range lower bound.
 * @param max - range upper bound.
 * @returns the clamped width.
 */
export declare function clampWidth(px: number, min: number, max: number): number;
/**
 * Solve the three column widths for one viewport frame. Pure: no hysteresis —
 * the output is a function of (viewport, preferences) only, so recovery on
 * re-widening is automatic. Preferences re-clamp here because they cross the
 * store boundary and callers may still supply stale ranges.
 * @param viewport - available frame width in px.
 * @param sidebar - sidebar width preference in px (0 = closed).
 * @param details - details width preference in px (0 = closed).
 * @returns resolved widths; details 0 means visually closed (never unmounted), while a closed sidebar keeps its compact rail.
 */
export declare function computeColumns(viewport: number, sidebar: number, details: number): Columns;
//# sourceMappingURL=columns.d.ts.map