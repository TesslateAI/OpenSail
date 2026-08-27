/** `trajectory` namespace dictionaries (view tab label + toolbar strings). */
/** Dictionary namespace owned by this plugin. */
export declare const NS = "trajectory";
/** The trajectory dictionary key set (the source of truth for both locales). */
export type TrajectoryKey = 'view.trajectory' | 'toolbar.aria' | 'toolbar.duration' | 'toolbar.useActualDuration' | 'toolbar.useEqualWidth' | 'toolbar.actualTime' | 'toolbar.turns' | 'toolbar.expandTurns' | 'toolbar.collapseTurns' | 'toolbar.calls' | 'toolbar.expandCalls' | 'toolbar.collapseCalls' | 'toolbar.search' | 'toolbar.searchPlaceholder';
declare module '@deepseek-ai/dsh-client-ui-slots' {
    interface LocaleNamespaceMap {
        /** The trajectory view tab label and toolbar strings. */
        'trajectory': TrajectoryKey;
    }
}
/** Simplified Chinese dictionary (the key-set source of truth). */
export declare const zh: Record<TrajectoryKey, string>;
/** English dictionary. */
export declare const en: Record<TrajectoryKey, string>;
//# sourceMappingURL=locales.d.ts.map