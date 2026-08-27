import { createRequire } from "node:module";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { Service } from "@deepseek-ai/cordis";
//#region lib/types/client/manifest.js
/**
* Client module system: the browser peer of Node's internal ESM loader, built
* as a lazy CJS table. The vendored cordis Loader consumes this object
* through its `internal` contract (the only call site is `EntryTree.import` →
* `internal.import`), which keeps entry governance (fiber lifecycle, inject
* waiting, update/refresh) entirely on the vendored side while this package
* owns code arrival.
*
* Lazy CJS model: executing a plugin bundle only REGISTERS its
* factory (`window.__ModuleLoader__.load({id, factory})`); every module body
* side effect — including CSS injection — lives inside the factory closure
* and runs at materialization, not at script execution. Materialization
* (factory(require) → exports) happens on first import/require and is
* memoized in {@link ClientModuleLoader.loadCache}; a factory that requires
* another registered-but-unmaterialized module materializes it recursively,
* so load order needs no external sequencing.
*
* Resolution branch order (import): seed word → shell instance; memoized
* record → exports; graph row → register its dependency factories and own
* factory; registered factory → materialize; anything else → throw (loud —
* the runtime mirror of the build-time bundle purity gate).
* The synchronous `require` handed to factories walks the same order minus
* the load branch. Loading is async, so a requested dynamic package must have
* registered its factory before a consumer materializes.
*
* This file is the browser-safe contract face (zero node imports): the
* `__DSH_BOOT__` wire types, the boot-manifest parser, and the boundaries around
* {@link ClientModuleSystem}. The package root is the host-side service that
* composes the wire.
*/
/**
* Validate an optional string-array field read from a `dsh.client` declaration
* or from the boot wire.
* @param subject - diagnostic prefix naming the package or the wire row.
* @param field - field name as it appears in the diagnostic.
* @param value - the raw field value.
* @returns the validated array, or undefined when the field is absent.
* @throws {Error} when the value is present but is not an array of strings.
*/
function optionalStringArray(subject, field, value) {
	if (value === void 0) return void 0;
	if (!Array.isArray(value) || value.some((item) => typeof item !== "string")) throw new Error(`client-modules: ${subject} ${field} must be a string array`);
	return value;
}
/**
* Normalize a module specifier onto the graph row that owns it: a plugin bundle
* IS its package's client half, so `<id>/client` (the exports subpath external
* bundles emit) and the bare package name resolve to the same exports. Both the
* require path and graph composition normalize here, which is what lets each
* importing package request the subpath its own code imports.
* @param spec - module specifier as a bundle requires it or a declaration spells it.
* @returns the specifier with a trailing `/client` removed.
*/
function stripClientSuffix(spec) {
	return spec.endsWith("/client") ? spec.slice(0, -7) : spec;
}
//#endregion
//#region lib/types/index.js
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
/** Recovery instruction shared by grouped startup and steady-state bundle diagnostics. */
const CLIENT_BUNDLE_BUILD_INSTRUCTION = "run `pnpm run build` before launch";
/** Missing built client export, retained as structured data for activation-error grouping. */
var MissingClientBundleError = class extends Error {
	packageName;
	clientPath;
	constructor(packageName, clientPath, cause) {
		super([
			`client-modules: client bundle not found; ${CLIENT_BUNDLE_BUILD_INSTRUCTION}:`,
			`  package: ${packageName}`,
			`  path: ${clientPath}`
		].join("\n"), { cause });
		this.packageName = packageName;
		this.clientPath = clientPath;
	}
};
/** Activation failures grouped by actionable package-build errors and unrelated failures. */
var ClientPackageCompositionError = class extends AggregateError {
	constructor(failures) {
		const missingBundles = failures.filter((error) => error instanceof MissingClientBundleError);
		const otherFailures = failures.filter((error) => !(error instanceof MissingClientBundleError));
		const packageNoun = failures.length === 1 ? "package" : "packages";
		const lines = [`client-modules: ${String(failures.length)} client ${packageNoun} failed to compose:`];
		if (missingBundles.length > 0) {
			lines.push(`  client bundles not found; ${CLIENT_BUNDLE_BUILD_INSTRUCTION}:`);
			for (const error of missingBundles) lines.push(`    - package: ${error.packageName}`, `      path: ${error.clientPath}`);
		}
		if (otherFailures.length > 0) lines.push("  other failures:", ...otherFailures.map((error) => `    - ${error.message}`));
		super(failures, lines.join("\n"));
	}
};
/** Narrow an unknown parsed JSON value to the `dsh.client` declaration, throwing on malformed fields. */
function parseDshClient(pkgName, value) {
	if (value === void 0) return void 0;
	if (typeof value !== "object" || value === null) throw new Error(`client-modules: ${pkgName} has a non-object dsh.client declaration`);
	const decl = value;
	if (typeof decl.platform !== "string") throw new Error(`client-modules: ${pkgName} dsh.client.platform must be a string`);
	const inject = optionalStringArray(pkgName, "dsh.client.inject", decl.inject);
	const external = optionalStringArray(pkgName, "dsh.client.external", decl.external);
	if (decl.immediately !== void 0 && typeof decl.immediately !== "boolean") throw new Error(`client-modules: ${pkgName} dsh.client.immediately must be a boolean`);
	return {
		platform: decl.platform,
		...inject !== void 0 ? { inject } : {},
		...external !== void 0 ? { external } : {},
		...decl.immediately !== void 0 ? { immediately: decl.immediately } : {}
	};
}
/** Resolve `exports["./client"]` to a relative path, accepting the string and one-level conditional forms. */
function clientExportOf(pkgName, exportsField) {
	if (typeof exportsField !== "object" || exportsField === null) return void 0;
	const client = exportsField["./client"];
	if (client === void 0) return void 0;
	if (typeof client === "string") return client;
	if (typeof client === "object" && client !== null) {
		const fallback = client.default;
		if (typeof fallback === "string") return fallback;
	}
	throw new Error(`client-modules: ${pkgName} exports["./client"] must be a string or an object with a string default`);
}
/** sha1 content hash shortened to 12 hex chars (bundle rev / graph rev). */
function shortHash(input) {
	return createHash("sha1").update(input).digest("hex").slice(0, 12);
}
/** Graph row for one bundle rev (url carries the rev as its cache-busting query). */
function graphRow(id, rev, fields) {
	return {
		id,
		url: `/plugins/${id}/client.js?rev=${rev}`,
		rev,
		...fields.inject !== void 0 ? { inject: fields.inject } : {},
		...fields.immediately ? { immediately: true } : {},
		...fields.external.length > 0 ? { external: fields.external } : {}
	};
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
function orderByModuleGraph(entries) {
	const rowsById = /* @__PURE__ */ new Map();
	for (const entry of entries) rowsById.set(entry.id, entry);
	const ordered = [];
	const placed = /* @__PURE__ */ new Set();
	const open = [];
	const visit = (entry) => {
		if (placed.has(entry.id)) return;
		const cycleStart = open.indexOf(entry.id);
		if (cycleStart !== -1) throw new Error(`client-modules: module graph cycle ${[...open.slice(cycleStart), entry.id].join(" -> ")} — a requested package row must precede its consumers, and factory-form CJS cannot deliver partial exports`);
		open.push(entry.id);
		for (const name of entry.external ?? []) {
			const dependency = rowsById.get(name) ?? rowsById.get(stripClientSuffix(name));
			if (dependency === entry) throw new Error(`client-modules: "${entry.id}" requests module "${name}" that it answers itself — a row must not declare its own package in dsh.client.external`);
			if (dependency !== void 0) visit(dependency);
		}
		open.pop();
		placed.add(entry.id);
		ordered.push(entry);
	};
	for (const entry of entries) visit(entry);
	return ordered;
}
/** Bootstrap package whose ordinary client bundle supplies the module-system implementation. */
const CLIENT_MODULES_ID = "@deepseek-ai/dsh-client-modules";
/** Ordinary dynamic bundles the HTML parser executes before the Vite shell. */
const PARSER_PRELOAD_IDS = [CLIENT_MODULES_ID, "@deepseek-ai/dsh-client-runtime"];
/** Escape a graph URL before placing it in a quoted HTML attribute. */
function escapeHtmlAttribute(value) {
	return value.replaceAll("&", "&amp;").replaceAll("\"", "&quot;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");
}
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
function injectBootManifest(html, graph) {
	const json = JSON.stringify(graph).replaceAll("<", "\\u003c");
	const script = `${`<script>(()=>{
const pendingQueue=[]
window.__ModuleLoader__={
  mode:"queue",
  pendingQueue,
  load(registration){pendingQueue.push(registration)},
  create(options){
    if(this.mode!=="queue")throw new Error("client-modules: window.__ModuleLoader__.create called after module-system boot")
    const index=pendingQueue.findIndex(registration=>registration.id===${JSON.stringify(CLIENT_MODULES_ID)})
    const registration=pendingQueue[index]
    if(registration===undefined)throw new Error("client-modules: HTML did not preload ${CLIENT_MODULES_ID}/client.js")
    pendingQueue.splice(index,1)
    const exports=registration.factory(specifier=>{
      throw new Error('client-modules: ${CLIENT_MODULES_ID}/client.js requested external "'+specifier+'" before the module system existed')
    })
    if(typeof exports!=="object"||exports===null||typeof exports.createClientModuleSystem!=="function"||typeof exports.apply!=="function"){
      throw new Error("client-modules: ${CLIENT_MODULES_ID}/client.js did not export the bootstrap module face")
    }
    return exports.createClientModuleSystem(this,{id:registration.id,exports},options)
  }
}
})()<\/script>`}${PARSER_PRELOAD_IDS.map((id) => graph.entries.find((entry) => entry.id === id)).filter((entry) => entry !== void 0).map((entry) => `<script src="${escapeHtmlAttribute(entry.url)}"><\/script>`).join("")}<script>window.__DSH_BOOT__ = ${json}<\/script>`;
	const head = html.indexOf("<head>");
	if (head !== -1) return `${html.slice(0, head + 6)}${script}${html.slice(head + 6)}`;
	return `${script}${html}`;
}
/**
* The web plugin table service: incremental `dsh.client` scan + wire composition
* + bundle route + index tap. Construction runs the activation scan
* synchronously — a malformed declaration or missing bundle among the
* already-loaded entries aggregates into one loud throw (FAILED fiber; the
* boot activation audit reports it).
*/
var ClientModuleRegistry = class extends Service {
	static inject = ["webServer", "loader"];
	table = /* @__PURE__ */ new Map();
	pkgMeta = /* @__PURE__ */ new Map();
	rebuildListeners = /* @__PURE__ */ new Set();
	graphListeners = /* @__PURE__ */ new Set();
	dirty = /* @__PURE__ */ new Set();
	resolvePkgJson;
	flushQueued = false;
	composed;
	/**
	* Build the service: subscribe, seed, and run the activation flush.
	* @param ctx - plugin context carrying webServer and loader.
	*/
	constructor(ctx) {
		super(ctx, "clientModules");
		if (ctx.baseUrl === void 0) throw new Error("client-modules: ctx.baseUrl is unset — the node half needs the config-tree anchor to resolve plugin packages");
		const require = createRequire(ctx.baseUrl);
		this.resolvePkgJson = (spec) => require.resolve(`${spec}/package.json`);
		ctx.on("internal/plugin", (fiber) => {
			const entryName = fiber.entry?.options.name;
			if (entryName === void 0) return;
			this.dirty.add(entryName);
			if (this.flushQueued) return;
			this.flushQueued = true;
			queueMicrotask(() => {
				this.flushQueued = false;
				this.flush((err) => {
					ctx.logger.warn(err);
				});
			});
		});
		for (const entry of ctx.loader.entries()) this.dirty.add(entry.options.name);
		this.composed = this.compose();
		const failures = [];
		this.flush((err) => failures.push(err));
		if (failures.length > 0) throw new ClientPackageCompositionError(failures);
		ctx.effect(() => ctx.webServer.register({
			kind: "prefix",
			path: "/plugins",
			handler: this.serveBundle
		}), "client-modules: bundle route");
		ctx.effect(() => ctx.webServer.tapIndex((html) => injectBootManifest(html, this.composed)), "client-modules: boot manifest injection");
	}
	/**
	* Current composed entry graph (stable object between changes).
	* @returns the graph served as `window.__DSH_BOOT__`.
	*/
	graph() {
		return this.composed;
	}
	/**
	* Absolute path of an entry's client bundle.
	* @param id - entry id (package name).
	* @returns the path, or undefined for an unknown id.
	*/
	clientPath(id) {
		return this.table.get(id)?.meta.clientPath;
	}
	/**
	* Re-hash one bundle (the HMR watch's registration hook — the only entry
	* point through which bundle content changes reach the graph).
	* @param id - entry id (package name).
	* @returns the new rev, or undefined for an unknown id.
	*/
	rebuilt(id) {
		const record = this.table.get(id);
		if (record === void 0) return void 0;
		const rev = shortHash(readFileSync(record.meta.clientPath));
		if (rev === record.entry.rev) return rev;
		record.entry = graphRow(id, rev, record.meta);
		this.composed = this.compose();
		for (const notify of this.rebuildListeners) try {
			notify(id, rev);
		} catch (error) {
			this.ctx.logger.error(error);
		}
		this.notifyGraphChanged();
		return rev;
	}
	/**
	* Subscribe to bundle rebuilds; fires only when the re-hash changed the rev.
	* @param listener - receives the entry id and its new bundle rev.
	* @returns the unsubscriber.
	*/
	onRebuilt(listener) {
		this.rebuildListeners.add(listener);
		return () => {
			this.rebuildListeners.delete(listener);
		};
	}
	/**
	* Fires after any flush that recomposed the graph (row added/removed, or a
	* rebuilt rev change). Pull model: listeners re-read {@link graph}.
	* @param listener - notified with no payload.
	* @returns the unsubscriber.
	*/
	onGraphChanged(listener) {
		this.graphListeners.add(listener);
		return () => {
			this.graphListeners.delete(listener);
		};
	}
	compose() {
		const entries = orderByModuleGraph([...this.table.values()].map((record) => record.entry));
		return {
			rev: shortHash(JSON.stringify(entries)),
			entries
		};
	}
	notifyGraphChanged() {
		for (const listener of this.graphListeners) try {
			listener();
		} catch (error) {
			this.ctx.logger.error(error);
		}
	}
	resolveMeta(pkgName) {
		const cached = this.pkgMeta.get(pkgName);
		if (cached !== void 0) return cached;
		let pkgPath;
		try {
			pkgPath = this.resolvePkgJson(pkgName);
		} catch {
			this.pkgMeta.set(pkgName, null);
			return null;
		}
		const pkg = JSON.parse(readFileSync(pkgPath, "utf8"));
		const dsh = pkg.dsh;
		const decl = parseDshClient(pkgName, dsh !== null && typeof dsh === "object" ? dsh.client : void 0);
		if (decl === void 0 || decl.platform !== "web") {
			this.pkgMeta.set(pkgName, null);
			return null;
		}
		const clientRel = clientExportOf(pkgName, pkg.exports);
		if (clientRel === void 0) throw new Error(`client-modules: ${pkgName} declares dsh.client but exports no "./client" bundle`);
		const meta = {
			clientPath: join(dirname(pkgPath), clientRel),
			...decl.inject !== void 0 ? { inject: decl.inject } : {},
			external: decl.external ?? [],
			immediately: decl.immediately === true
		};
		this.pkgMeta.set(pkgName, meta);
		return meta;
	}
	/**
	* Read the activation-time bundle revision.
	* @param pkgName - package that declares the client bundle.
	* @param clientPath - absolute path of the built client artifact.
	* @returns the bundle content's short hash for use as its revision.
	* @throws {MissingClientBundleError} when the read fails with `ENOENT`; other filesystem errors are rethrown unchanged.
	*/
	initialBundleRevision(pkgName, clientPath) {
		try {
			return shortHash(readFileSync(clientPath));
		} catch (error) {
			if (error.code !== "ENOENT") throw error;
			throw new MissingClientBundleError(pkgName, clientPath, error);
		}
	}
	/** Reconcile one entry name against the live loader entries. @returns whether the table changed. */
	processOne(entryName) {
		let qualifies = false;
		for (const entry of this.ctx.loader.entries()) if (entry.options.name === entryName && entry.fiber !== void 0 && !entry.disabled) {
			qualifies = true;
			break;
		}
		if (!qualifies) return this.table.delete(entryName);
		if (this.table.has(entryName)) return false;
		const meta = this.resolveMeta(entryName);
		if (meta === null) return false;
		const rev = this.initialBundleRevision(entryName, meta.clientPath);
		this.table.set(entryName, {
			entry: graphRow(entryName, rev, meta),
			meta
		});
		return true;
	}
	flush(onError) {
		let changed = false;
		for (const entryName of [...this.dirty]) {
			this.dirty.delete(entryName);
			try {
				if (this.processOne(entryName)) changed = true;
			} catch (error) {
				onError(error instanceof Error ? error : new Error(String(error)));
			}
		}
		if (!changed) return;
		let composed;
		try {
			composed = this.compose();
		} catch (error) {
			onError(error);
			return;
		}
		this.composed = composed;
		this.notifyGraphChanged();
	}
	serveBundle = async (req, res) => {
		if (req.method !== "GET" && req.method !== "HEAD") {
			res.writeHead(405);
			res.end();
			return;
		}
		/* v8 ignore next -- `?? '/'` arm: node:http always sets url on server requests. */
		const pathname = decodeURIComponent(new URL(req.url ?? "/", "http://x").pathname);
		const prefix = "/plugins/";
		const mapSuffix = "/client.js.map";
		const bundleSuffix = "/client.js";
		const isSourceMap = pathname.startsWith(prefix) && pathname.endsWith(mapSuffix);
		const suffix = isSourceMap ? mapSuffix : bundleSuffix;
		const clientPath = pathname.startsWith(prefix) && pathname.endsWith(suffix) ? this.clientPath(pathname.slice(9, -suffix.length)) : void 0;
		const path = clientPath === void 0 ? void 0 : `${clientPath}${isSourceMap ? ".map" : ""}`;
		if (path === void 0) {
			res.writeHead(404);
			res.end();
			return;
		}
		try {
			const body = await readFile(path);
			res.writeHead(200, {
				"content-type": isSourceMap ? "application/json; charset=utf-8" : "text/javascript; charset=utf-8",
				"cache-control": "no-cache"
			});
			res.end(body);
		} catch {
			res.writeHead(404);
			res.end();
		}
	};
};
//#endregion
export { ClientModuleRegistry, ClientModuleRegistry as default, injectBootManifest, orderByModuleGraph, stripClientSuffix };
