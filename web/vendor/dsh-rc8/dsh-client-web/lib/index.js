import * as Cordis from "@deepseek-ai/cordis";
import { Context } from "@deepseek-ai/cordis";
import Loader from "@deepseek-ai/cordis-plugin-loader";
import css from "./boot-page.module.css";
import * as React from "react";
import * as ReactJsxRuntime from "react/jsx-runtime";
import * as ReactDom from "react-dom";
import * as ReactDomClient from "react-dom/client";
import * as UiSlots from "@deepseek-ai/dsh-client-ui-slots";
import * as UiPrimitives from "@deepseek-ai/dsh-client-ui-primitives";
import "./base.css";
//#region lib/types/boot-page.js
/** Create a div with one module class and optional text. */
function div(className, text) {
	const el = document.createElement("div");
	el.className = className ?? "";
	if (text !== void 0) el.textContent = text;
	return el;
}
/** Kernel-owned page mounted below the application's root element. */
var BootPage = class {
	root;
	card;
	wordmark;
	spinner;
	hint;
	states = /* @__PURE__ */ new Map();
	active = /* @__PURE__ */ new Set();
	total = 0;
	failure;
	/**
	* Build and attach the boot page.
	* @param container - Application mount point.
	*/
	constructor(container) {
		this.root = div(css.boot);
		this.root.dataset.dshBoot = "";
		this.card = div(css.card);
		this.wordmark = div(css.wordmark, "HARNESS");
		this.spinner = div(css.spinner);
		this.spinner.dataset.dshBootSpinner = "";
		this.hint = div(css.hint, "Loading plugins…");
		this.card.append(this.wordmark, this.spinner, this.hint);
		this.root.append(this.card);
		container.append(this.root);
		this.updateProgress();
	}
	/**
	* Set the number of loader entries represented by the progress arc.
	* @param total - Complete boot roster size.
	*/
	setTotal(total) {
		this.total = total;
		this.updateProgress();
	}
	/**
	* Project one loader entry's fiber state.
	* @param id - Loader entry name.
	* @param state - Projected fiber state.
	*/
	setState(id, state) {
		this.states.set(id, state);
		if (state === "active") this.active.add(id);
		this.updateProgress();
		this.render();
	}
	/**
	* Display the boot failure report.
	* @param message - Failure report text.
	*/
	fail(message) {
		this.failure = message;
		this.render();
	}
	/** Detach the page before or after the UI renderer takes the mount point. */
	dispose() {
		this.root.remove();
	}
	/** Redraw the state-dependent content below the wordmark. */
	render() {
		const failed = [...this.states].filter(([, state]) => state === "failed").map(([id]) => id);
		if (this.failure === void 0 && failed.length === 0) {
			if (this.spinner.parentElement !== this.card) this.card.replaceChildren(this.wordmark, this.spinner, this.hint);
			return;
		}
		const report = div(css.failed);
		report.append(div(css.failedTitle, "Failed to load plugins"));
		for (const id of failed) report.append(div(css.failedItem, id));
		if (this.failure !== void 0) report.append(div(css.failedItem, this.failure));
		this.card.replaceChildren(this.wordmark, report);
	}
	/** Grow the rotating arc monotonically as loader entries activate. */
	updateProgress() {
		const ratio = this.total === 0 ? 0 : Math.min(this.active.size / this.total, 1);
		this.spinner.style.setProperty("--dsh-boot-arc", `${String(Math.round(72 + ratio * 216))}deg`);
	}
};
//#endregion
//#region lib/types/seed.js
/**
* Platform-singleton module-table. These are the ONLY entities the shell
* shares into the frozen module table — fetch bundles resolve their externals
* against exactly this set through the loader's require. Keys come from the
* platform constant module ({@link ./platform.ts}, the single source
* of truth with the tsdown client externals); values stay shell-static
* imports so every bundle sees the same instance.
*/
/**
* Build the static table handed to the module loader at boot.
* @returns module specifier → exported entity (one entry per platform word).
*/
function getStaticModules() {
	return {
		"react": React,
		"react/jsx-runtime": ReactJsxRuntime,
		"react-dom": ReactDom,
		"react-dom/client": ReactDomClient,
		"@deepseek-ai/cordis": Cordis,
		"@deepseek-ai/dsh-client-ui-slots": UiSlots,
		"@deepseek-ai/dsh-client-ui-primitives": UiPrimitives
	};
}
//#endregion
//#region lib/types/loader-status.js
/**
* Value mirror of cordis's `FiberState` const enum: a const enum has no
* runtime object to import (and esbuild-based pipelines cannot inline it
* across modules), so these values mirror the pinned vendored definition
* while retaining its type (same rationale as dsh-tool-cordis's mirror).
*/
const FIBER_STATE = {
	PENDING: 0,
	LOADING: 1,
	ACTIVE: 2,
	FAILED: 3,
	DISPOSED: 4,
	UNLOADING: 5
};
/** Label for each fiber state, keyed by member (inlining-safe — no reverse mapping). */
const STATE_LABELS = {
	[FIBER_STATE.PENDING]: "pending",
	[FIBER_STATE.LOADING]: "loading",
	[FIBER_STATE.ACTIVE]: "active",
	[FIBER_STATE.FAILED]: "failed",
	[FIBER_STATE.DISPOSED]: "disposed",
	[FIBER_STATE.UNLOADING]: "unloading"
};
//#endregion
//#region lib/types/boot.js
/**
* Web boot kernel. It owns only the module system, Cordis loader, and a
* framework-free boot page. The dynamic UI renderer receives the mount
* point after every client entry activates.
* @module @deepseek-ai/dsh-client-web/src/boot
*/
/** Browser boot entry consumed by `apps/web`. */
var AppWebEntry = class {
	container;
	seams;
	page;
	ctx;
	modules;
	manifest;
	/**
	* Draw the boot page; {@link run} starts the loader.
	* @param container - Application mount point.
	* @param seams - Optional module transport replacement.
	*/
	constructor(container, seams) {
		this.container = container;
		this.seams = seams;
		this.page = new BootPage(container);
	}
	/**
	* Load and activate every client entry, then hand the mount point to the
	* UI renderer. Plugin failures remain visible on the boot page.
	* @returns Resolves after application mount or failure rendering.
	*/
	async run() {
		try {
			const win = globalThis;
			const moduleLoader = win.__ModuleLoader__;
			if (moduleLoader === void 0) throw new Error("web boot: window.__ModuleLoader__ bootstrap facade is missing");
			this.modules = moduleLoader.create({
				boot: win.__DSH_BOOT__,
				staticModules: getStaticModules(),
				...this.seams
			});
			this.manifest = this.modules.manifest;
			const prefetching = this.prefetchImmediateTier();
			const ctx = new Context();
			this.ctx = ctx;
			await this.runPluginBoot(ctx, prefetching);
			await this.mountApp(ctx);
		} catch (reason) {
			console.error(reason);
			this.page.fail(reason instanceof Error ? reason.message : String(reason));
		}
	}
	/** Dispose the client plugin tree and whichever page owns the mount point. */
	async dispose() {
		const ctx = this.ctx;
		this.ctx = void 0;
		if (ctx !== void 0) await ctx.fiber.dispose();
		this.page.dispose();
	}
	/** Mount through a dependency fiber so replacing uiRenderer remounts the application. */
	async mountApp(ctx) {
		await ctx.inject(["uiRenderer"], (scope) => {
			scope.effect(() => scope.uiRenderer.mount(this.container), "web boot: application mount");
		});
	}
	/** Prefetch stage-one bundles; their import path owns any eventual failure. */
	async prefetchImmediateTier() {
		await Promise.all(this.manifest.plugins.filter((row) => row.immediately).map((row) => this.modules.prefetch(row.id).catch((_prefetchError) => {})));
	}
	/** Mount the Loader, create all graph entries, await quiescence, and audit activation. */
	async runPluginBoot(ctx, prefetching) {
		await ctx.plugin(Loader);
		const loader = ctx.loader;
		loader.internal = this.modules;
		ctx.on("internal/status", (fiber) => {
			const entry = fiber.entry;
			if (entry === void 0 || entry.fiber === void 0) return;
			this.page.setState(entry.options.name, STATE_LABELS[entry.fiber.state]);
		});
		const rows = this.manifest.plugins.map((row) => row.id);
		this.page.setTotal(rows.length);
		await prefetching;
		await Promise.all(rows.map(async (name) => {
			this.page.setState(name, "loading");
			const id = await loader.create({ name });
			if (loader.resolve(id).fiber === void 0) this.page.setState(name, "failed");
		}));
		await loader.await();
		this.assertEntriesActive(ctx);
	}
	/** Reject entries that failed import/apply or still wait on missing services. */
	assertEntriesActive(ctx) {
		const failures = [];
		for (const entry of ctx.loader.entries()) {
			const name = entry.options.name;
			if (entry.fiber === void 0) {
				failures.push(`${name}: import failed (see console for the import error)`);
				continue;
			}
			const state = STATE_LABELS[entry.fiber.state];
			if (state === "active") continue;
			if (state === "pending") {
				const missing = Object.keys(entry.fiber.inject).filter((service) => ctx.get(service) === void 0);
				failures.push(`${name}: pending (waiting for service${missing.length === 1 ? "" : "s"}: ${missing.join(", ") || "unknown"})`);
			} else failures.push(`${name}: ${state}`);
		}
		if (failures.length > 0) throw new Error(`web boot: ${String(failures.length)} entr${failures.length === 1 ? "y" : "ies"} did not activate\n${failures.join("\n")}`);
	}
};
//#endregion
//#region lib/types/platform.js
/**
* Shared browser platform modules. Seeding, bundling externals, and Vite
* aliases consume this list so their module identities cannot drift.
* @module @deepseek-ai/dsh-client-web/src/platform
*/
/** The module specifiers the shell shares into the frozen module table. */
const PLATFORM_MODULES = [
	"react",
	"react/jsx-runtime",
	"react-dom",
	"react-dom/client",
	"@deepseek-ai/cordis",
	"@deepseek-ai/dsh-client-ui-slots",
	"@deepseek-ai/dsh-client-ui-primitives"
];
/** Client-bundle specifiers whose factories the parser preloads before the shell starts. */
const PRELOADED_CLIENT_EXTERNALS = ["@deepseek-ai/dsh-client-runtime/client"];
//#endregion
export { AppWebEntry, PLATFORM_MODULES, PRELOADED_CLIENT_EXTERNALS, getStaticModules };

//# sourceMappingURL=index.js.map