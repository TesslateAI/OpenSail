import type { ClientModuleCreateOptions } from '@deepseek-ai/dsh-client-modules/client';
import './base.css';
/** Module transport hook replaced by jsdom tests. */
export type BootSeams = Pick<ClientModuleCreateOptions, 'loadBundle'>;
/** Browser boot entry consumed by `apps/web`. */
export declare class AppWebEntry {
    private readonly container;
    private readonly seams;
    private readonly page;
    private ctx;
    private modules;
    private manifest;
    /**
     * Draw the boot page; {@link run} starts the loader.
     * @param container - Application mount point.
     * @param seams - Optional module transport replacement.
     */
    constructor(container: HTMLElement, seams?: BootSeams);
    /**
     * Load and activate every client entry, then hand the mount point to the
     * UI renderer. Plugin failures remain visible on the boot page.
     * @returns Resolves after application mount or failure rendering.
     */
    run(): Promise<void>;
    /** Dispose the client plugin tree and whichever page owns the mount point. */
    dispose(): Promise<void>;
    /** Mount through a dependency fiber so replacing uiRenderer remounts the application. */
    private mountApp;
    /** Prefetch stage-one bundles; their import path owns any eventual failure. */
    private prefetchImmediateTier;
    /** Mount the Loader, create all graph entries, await quiescence, and audit activation. */
    private runPluginBoot;
    /** Reject entries that failed import/apply or still wait on missing services. */
    private assertEntriesActive;
}
//# sourceMappingURL=boot.d.ts.map