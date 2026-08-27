/**
 * Framework-free boot page and failure report. It remains available when a
 * client plugin fails because React arrives only with the UI renderer.
 * @module @deepseek-ai/dsh-client-web/src/boot-page
 */
import type { LoaderEntryState } from './loader-status.ts';
/** Kernel-owned page mounted below the application's root element. */
export declare class BootPage {
    private readonly root;
    private readonly card;
    private readonly wordmark;
    private readonly spinner;
    private readonly hint;
    private readonly states;
    private readonly active;
    private total;
    private failure;
    /**
     * Build and attach the boot page.
     * @param container - Application mount point.
     */
    constructor(container: HTMLElement);
    /**
     * Set the number of loader entries represented by the progress arc.
     * @param total - Complete boot roster size.
     */
    setTotal(total: number): void;
    /**
     * Project one loader entry's fiber state.
     * @param id - Loader entry name.
     * @param state - Projected fiber state.
     */
    setState(id: string, state: LoaderEntryState): void;
    /**
     * Display the boot failure report.
     * @param message - Failure report text.
     */
    fail(message: string): void;
    /** Detach the page before or after the UI renderer takes the mount point. */
    dispose(): void;
    /** Redraw the state-dependent content below the wordmark. */
    private render;
    /** Grow the rotating arc monotonically as loader entries activate. */
    private updateProgress;
}
//# sourceMappingURL=boot-page.d.ts.map