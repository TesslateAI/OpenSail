#!/usr/bin/env node
/**
 * Compose the DSH rc.8 client plugin graph from the vendored pinned
 * packages under `vendor/dsh-rc8`, copy `lib/client.js` bundles, and
 * replace the connection plugin with the canonical `connection-voie`
 * carrier-backed adapter.
 *
 * Provenance: every bundle is copied verbatim from the pinned Whaled/DSH
 * rc.8 package graph (see vendor/dsh-rc8/PROVENANCE.md); the only
 * VOIE-authored plugin is the required connection-voie face.
 */
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, rmSync, writeFileSync, existsSync, realpathSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(join(root, "package.json"));
// esbuild is a transitive dependency of vite; resolve it through vite's own
// package when it is not linked at the web root.
let esbuild;
try {
  esbuild = require("esbuild");
} catch {
  // The web-root vite entry is a pnpm symlink; resolve through its real
  // package directory so createRequire walks the store layout.
  const viteReal = realpathSync(join(root, "node_modules", "vite", "package.json"));
  const viteRequire = createRequire(viteReal);
  esbuild = viteRequire("esbuild");
}

const outFlag = process.argv.indexOf("--out");
const outDir = join(root, outFlag === -1 ? "public" : process.argv[outFlag + 1] ?? "public");

const CONNECTION_ID = "@deepseek-ai/dsh-client-connection";
const CONNECTION_ENTRY = "src/connection-voie/plugin.ts";
const VENDOR_ROOT = join(root, "vendor", "dsh-rc8");

const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
// Stock DSH product chrome the VOIE shell does not serve: the global sidebar
// package registers product-window chrome around the conversation surface.
// Its dependency row stays declared for editor/type resolution, but it is
// dropped from the composed boot graph; dangling inject/external edges are
// pruned below — exactly how ui-settings/ui-theme already ship omitted.
const CHROME_DROP_IDS = new Set([
  "@deepseek-ai/dsh-client-ui-sidebar",
]);
const names = Object.keys(pkg.dependencies ?? {})
  .filter((name) => name.startsWith("@deepseek-ai/"))
  .filter((name) => !CHROME_DROP_IDS.has(name));
const EXCLUDED_IDS = new Set([
  "@deepseek-ai/dsh-api-gateway",
  "@deepseek-ai/dsh-api-remotes",
  "@deepseek-ai/dsh-client-ui-settings",
  "@deepseek-ai/dsh-client-ui-theme",
]);
const forbiddenNames = names.filter((name) => EXCLUDED_IDS.has(name));
if (forbiddenNames.length > 0) {
  throw new Error(
    `compose-graph: excluded packages declared in web/package.json: ${forbiddenNames.join(", ")}`,
  );
}

const rows = [];
for (const name of names) {
  // Vendored directories are named without the `@deepseek-ai/` scope prefix.
  const dirName = name.slice("@deepseek-ai/".length);
  const manifestPath = join(VENDOR_ROOT, dirName, "package.json");
  if (!existsSync(manifestPath)) {
    throw new Error(`compose-graph: ${name} is not vendored under vendor/dsh-rc8`);
  }
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  const client = manifest.dsh?.client;
  if (client === undefined) continue;
  if (client.platform !== "web") continue;
  const clientPath = join(VENDOR_ROOT, dirName, "lib", "client.js");
  if (!existsSync(clientPath)) {
    throw new Error(`compose-graph: ${name} has no lib/client.js`);
  }
  rows.push({
    id: name,
    clientPath,
    inject: Array.isArray(client.inject) ? client.inject : [],
    immediately: client.immediately === true,
    external: Array.isArray(client.external) ? client.external : [],
  });
}

const byId = new Map(rows.map((row) => [row.id, row]));
const graphIdOf = (spec) => spec.endsWith("/client") ? spec.slice(0, -"/client".length) : spec;
// The pinned package declarations may mention packages deliberately omitted
// from VOIE's browser graph. Keep the wire self-contained: package graph
// edges name only rows that are actually composed. Non-DSH externals remain
// intact because they may be static platform seeds.
for (const row of rows) {
  row.inject = row.inject.filter((spec) => byId.has(graphIdOf(spec)));
  row.external = row.external.filter(
    (spec) => !spec.startsWith("@deepseek-ai/") || byId.has(graphIdOf(spec)),
  );
}
const ordered = [];
const placed = new Set();
const visiting = [];
const visit = (row) => {
  if (placed.has(row.id)) return;
  const cycle = visiting.indexOf(row.id);
  if (cycle !== -1) {
    throw new Error(`compose-graph: cycle ${[...visiting.slice(cycle), row.id].join(" -> ")}`);
  }
  visiting.push(row.id);
  for (const spec of row.external) {
    const depName = spec.endsWith("/client") ? spec.slice(0, -"/client".length) : spec;
    const dep = byId.get(depName);
    if (dep !== undefined) visit(dep);
  }
  visiting.pop();
  placed.add(row.id);
  ordered.push(row);
};
for (const row of rows) visit(row);

if (!byId.has(CONNECTION_ID)) {
  throw new Error("compose-graph: @deepseek-ai/dsh-client-connection is not in the graph");
}
const connectionEntryPath = join(root, CONNECTION_ENTRY);
if (!existsSync(connectionEntryPath)) {
  throw new Error(`compose-graph: required ${CONNECTION_ENTRY} seam is missing`);
}


const pluginRoot = join(outDir, "plugins");
rmSync(pluginRoot, { recursive: true, force: true });
mkdirSync(pluginRoot, { recursive: true });

const connectionOut = join(pluginRoot, CONNECTION_ID, "client.js");
mkdirSync(dirname(connectionOut), { recursive: true });
await esbuild.build({
  absWorkingDir: root,
  entryPoints: [CONNECTION_ENTRY],
  outfile: connectionOut,
  bundle: true,
  format: "cjs",
  platform: "browser",
  sourcemap: false,
  minify: false,
  banner: {
    js: `window.__ModuleLoader__.load({ id: ${JSON.stringify(CONNECTION_ID)}, factory: (require) => {\nconst module = { exports: {} };\nconst exports = module.exports;\n`,
  },
  footer: {
    js: `\nreturn module.exports;\n}});\n`,
  },
});

const entries = [];
for (const row of ordered) {
  const dest = join(pluginRoot, row.id, "client.js");
  if (row.id !== CONNECTION_ID) {
    mkdirSync(dirname(dest), { recursive: true });
    const source = readFileSync(row.clientPath, "utf8").replace(/\/\/# sourceMappingURL=.*$/gm, "");
    writeFileSync(dest, source);
  }
  const bytes = readFileSync(dest);
  const rev = createHash("sha1").update(bytes).digest("hex").slice(0, 12);
  entries.push({
    id: row.id,
    url: `/plugins/${row.id}/client.js?rev=${rev}`,
    rev,
    ...(row.inject.length > 0 ? { inject: row.inject } : {}),
    ...(row.immediately ? { immediately: true } : {}),
    ...(row.external.length > 0 ? { external: row.external } : {}),
  });
}

const graphRev = createHash("sha1")
  .update(entries.map((entry) => `${entry.id}:${entry.rev}`).join("|"))
  .digest("hex")
  .slice(0, 12);

const graph = { rev: graphRev, entries };
const graphJs = `window.__DSH_BOOT__ = ${JSON.stringify(graph).replaceAll("<", "\\u003c")};\n`;
writeFileSync(join(outDir, "boot-graph.js"), graphJs);
writeFileSync(join(outDir, "boot-graph.json"), `${JSON.stringify(graph, null, 2)}\n`);
console.log(`compose-graph: ${String(entries.length)} plugins -> ${outDir}`);
