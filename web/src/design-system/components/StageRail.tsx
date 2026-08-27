/**
 * StageRail — lifecycle ladder that reads as progress, never a locked door.
 * Source: mock surfaces.css § .stages/.stage* ; mock ui.js § StageRail.
 *
 * The stage vocabulary is passed in (parity supplies the VOIE stages);
 * `compact` keeps only the current stage's label so the rail fits a card,
 * collapsing the rest to pips instead of wrapping.
 */
import type { ReactNode } from "react";
import { cx } from "../cx";
import type { StageState } from "../variants";

export interface StageNode {
  id: string;
  label: string;
  /** Tooltip hint shown on hover. */
  hint?: string;
  /** When true the pip shows a done glyph instead of the index. */
  completeMark?: ReactNode;
}

export interface StageRailProps {
  stages: ReadonlyArray<StageNode>;
  /** Index of the current stage (0-based). */
  currentIndex: number;
  /** Waiting/hold stage index, if any (renders warn). */
  waitingIndex?: number;
  compact?: boolean;
}

export function StageRail({ stages, currentIndex, waitingIndex, compact }: StageRailProps): ReactNode {
  return (
    <div className="kds-stages">
      {stages.map((s, i) => {
        const state: StageState =
          i < currentIndex ? "done"
          : i === currentIndex ? "current"
          : waitingIndex !== undefined && i === waitingIndex ? "waiting"
          : "upcoming";
        const showLabel = !compact || i === currentIndex;
        return (
          <div key={s.id} className={cx("kds-stage", `kds-${state}`)} title={s.hint ?? undefined}>
            {i > 0 ? <div className={cx("kds-stage-link", i <= currentIndex && "kds-done")} /> : null}
            <div
              className="kds-stage-node"
              style={showLabel ? undefined : { padding: 4, borderRadius: 999 }}
            >
              <span className="kds-pip">
                {i < currentIndex && s.completeMark ? s.completeMark : i + 1}
              </span>
              {showLabel ? s.label : null}
            </div>
          </div>
        );
      })}
    </div>
  );
}
