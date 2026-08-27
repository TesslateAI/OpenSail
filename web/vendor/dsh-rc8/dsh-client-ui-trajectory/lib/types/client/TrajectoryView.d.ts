/** Trajectory view: compact summary over a turn-aware event ledger. */
import type { ConvViewProps } from '@deepseek-ai/dsh-client-ui-conversation/client';
import type { InjectFace, PropsLocale } from '@deepseek-ai/dsh-client-ui-slots';
import type { SnapshotStore } from '@deepseek-ai/dsh-client-runtime/client';
/** Session-bound controls not already supplied by the conversation view slot. */
export interface TrajectoryViewInjected {
    hooks: {
        duration: SnapshotStore<boolean>;
    };
    loadOlder: () => Promise<boolean>;
    setActualDuration: (actualDuration: boolean) => void;
}
export declare function TrajectoryView({ useSession, useDuration, loadOlder, setActualDuration, inspect, onInspectDone, t, }: ConvViewProps & InjectFace<TrajectoryViewInjected> & PropsLocale<'trajectory'>): import("react").JSX.Element;
//# sourceMappingURL=TrajectoryView.d.ts.map