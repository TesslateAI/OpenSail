import type { ReactNode } from 'react';
export interface TrajectoryTurnProps {
    /** 1-based turn index for the sticky header. */
    turn: number;
    /** Message / Step headers and TrajectoryCell rows. */
    children?: ReactNode;
}
/**
 * Render one turn section (sticky header + body).
 * @param props - turn index and body children.
 * @returns the turn section element.
 */
export declare function TrajectoryTurn({ turn, children }: TrajectoryTurnProps): import("react").JSX.Element;
//# sourceMappingURL=TrajectoryTurn.d.ts.map