/**
 * Fiber-state projection vocabulary for the framework-free boot page. The
 * boot chain subscribes to `internal/status` and projects the owning loader
 * entry's current state.
 * @module @deepseek-ai/dsh-client-web/src/loader-status
 */
import type { FiberState } from '@deepseek-ai/cordis';
/**
 * Value mirror of cordis's `FiberState` const enum: a const enum has no
 * runtime object to import (and esbuild-based pipelines cannot inline it
 * across modules), so these values mirror the pinned vendored definition
 * while retaining its type (same rationale as dsh-tool-cordis's mirror).
 */
export declare const FIBER_STATE: {
    readonly PENDING: FiberState.PENDING;
    readonly LOADING: FiberState.LOADING;
    readonly ACTIVE: FiberState.ACTIVE;
    readonly FAILED: FiberState.FAILED;
    readonly DISPOSED: FiberState.DISPOSED;
    readonly UNLOADING: FiberState.UNLOADING;
};
/** One entry's projected state label (lower-case face of {@link FiberState}). */
export type LoaderEntryState = 'pending' | 'loading' | 'active' | 'failed' | 'disposed' | 'unloading';
/** Label for each fiber state, keyed by member (inlining-safe — no reverse mapping). */
export declare const STATE_LABELS: Record<FiberState, LoaderEntryState>;
//# sourceMappingURL=loader-status.d.ts.map