/**
 * Node half of the client module system (`dsh.client` dual-face package): scans
 * the host Loader's entries for packages declaring `dsh.client`, composes the
 * `window.__DSH_BOOT__` entry graph (wire single source: {@link WebBootEntry}
 * in `./client/manifest.ts`) in module-graph order, serves
 * `/plugins/<id>/client.js` and its source map, taps the index render to
 * inject the boot manifest plus the parser-blocking bootstrap preloads, and
 * provides the `clientModuleHost` service (the HMR node half's
 * registration/notification face).
 *
 * Scanning is incremental per package — there is no full-rescan code path.
 * Every cordis `internal/plugin` emission (fiber construction/disposal) marks
 * the fiber's entry name dirty; a microtask flush reconciles each dirty name
 * against the live loader entries. The activation pass seeds the same dirty
 * set with all current entries and flushes synchronously, so first scan and
 * steady state share one implementation. Package metadata (including the
 * negative "not a client package" verdict) is cached per name and never
 * expires — plugin-set changes take effect on restart; bundle content
 * changes reach the graph only through
 * {@link ClientModuleRegistry.rebuilt}.
 * @module @deepseek-ai/dsh-client-modules
 */
import { Service } from '@deepseek-ai/cordis';
import type { Context } from '@deepseek-ai/cordis';
import type { WebBootEntry, WebBootGraph } from './client/manifest.ts';
export { stripClientSuffix } from './client/manifest.ts';
export type { BootManifest, BootModuleRow, BootPluginRow, WebBootEntry, WebBootGraph, } from './client/manifest.ts';
declare module '@deepseek-ai/cordis' {
    interface Context {
        /** The web plugin table (provided by the client-modules node half). */
        clientModules: ClientModuleRegistry;
    }
}
/**
 * Order composed rows so every requested dynamic package precedes its
 * consumers. An `external` specifier is either the package row it names
 * (`<pkg>/client` aliases the bare package) or a static-table name that adds no
 * graph edge.
 * @param entries - composed rows in scan order.
 * @returns the same rows reordered; scan order breaks every tie.
 * @throws {Error} when a row requests itself or when the module graph has a
 * cycle; the message lists the packages on it.
 */
export declare function orderByModuleGraph(entries: readonly WebBootEntry[]): WebBootEntry[];
/**
 * Inject the boot protocol into index.html. The inline registration queue precedes
 * blocking classic scripts for modules' and runtime's ordinary
 * `lib/client.js` artifacts. Its `create()` method materializes the modules
 * bundle, delegates construction to that bundle, and leaves the same facade
 * in live-registration mode. The graph script follows before the shell reads
 * it. `<` is escaped in JSON so a plugin-controlled string cannot break out
 * of the script element.
 * @param html - the index.html source.
 * @param graph - the composed entry graph.
 * @returns the html with the graph script injected.
 */
export declare function injectBootManifest(html: string, graph: WebBootGraph): string;
/**
 * The web plugin table service: incremental `dsh.client` scan + wire composition
 * + bundle route + index tap. Construction runs the activation scan
 * synchronously — a malformed declaration or missing bundle among the
 * already-loaded entries aggregates into one loud throw (FAILED fiber; the
 * boot activation audit reports it).
 */
export declare class ClientModuleRegistry extends Service {
    static inject: string[];
    private readonly table;
    private readonly pkgMeta;
    private readonly rebuildListeners;
    private readonly graphListeners;
    private readonly dirty;
    private readonly resolvePkgJson;
    private flushQueued;
    private composed;
    /**
     * Build the service: subscribe, seed, and run the activation flush.
     * @param ctx - plugin context carrying webServer and loader.
     */
    constructor(ctx: Context);
    /**
     * Current composed entry graph (stable object between changes).
     * @returns the graph served as `window.__DSH_BOOT__`.
     */
    graph(): WebBootGraph;
    /**
     * Absolute path of an entry's client bundle.
     * @param id - entry id (package name).
     * @returns the path, or undefined for an unknown id.
     */
    clientPath(id: string): string | undefined;
    /**
     * Re-hash one bundle (the HMR watch's registration hook — the only entry
     * point through which bundle content changes reach the graph).
     * @param id - entry id (package name).
     * @returns the new rev, or undefined for an unknown id.
     */
    rebuilt(id: string): string | undefined;
    /**
     * Subscribe to bundle rebuilds; fires only when the re-hash changed the rev.
     * @param listener - receives the entry id and its new bundle rev.
     * @returns the unsubscriber.
     */
    onRebuilt(listener: (id: string, rev: string) => void): () => void;
    /**
     * Fires after any flush that recomposed the graph (row added/removed, or a
     * rebuilt rev change). Pull model: listeners re-read {@link graph}.
     * @param listener - notified with no payload.
     * @returns the unsubscriber.
     */
    onGraphChanged(listener: () => void): () => void;
    private compose;
    private notifyGraphChanged;
    private resolveMeta;
    /**
     * Read the activation-time bundle revision.
     * @param pkgName - package that declares the client bundle.
     * @param clientPath - absolute path of the built client artifact.
     * @returns the bundle content's short hash for use as its revision.
     * @throws {MissingClientBundleError} when the read fails with `ENOENT`; other filesystem errors are rethrown unchanged.
     */
    private initialBundleRevision;
    /** Reconcile one entry name against the live loader entries. @returns whether the table changed. */
    private processOne;
    private flush;
    private readonly serveBundle;
}
export default ClientModuleRegistry;
//# sourceMappingURL=index.d.ts.map