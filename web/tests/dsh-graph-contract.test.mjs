import assert from "node:assert/strict";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const WEB_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (relativePath) => readFileSync(join(WEB_ROOT, relativePath), "utf8");

const EXCLUDED_PACKAGES = [
  "@deepseek-ai/dsh-api-gateway",
  "@deepseek-ai/dsh-api-remotes",
  "@deepseek-ai/dsh-client-ui-settings",
  "@deepseek-ai/dsh-client-ui-theme",
];
const CONNECTION_PACKAGE = "@deepseek-ai/dsh-client-connection";

function packageNamePattern(name) {
  const escaped = [...name]
    .map((character) => "\\^$.*+?()[]{}|".includes(character) ? `\\${character}` : character)
    .join("");
  return new RegExp(escaped);
}

function importerKeys(lockfile) {
  const start = lockfile.indexOf("importers:\n");
  const end = lockfile.indexOf("\npackages:\n", start);
  assert.notEqual(start, -1, "pnpm lockfile must contain an importer section");
  const importer = lockfile.slice(start, end === -1 ? undefined : end);
  return [...importer.matchAll(/^ {6}(?:'([^']+)'|"([^"]+)"|([^\s:#]+)):\s*$/gm)].map(
    (match) => match[1] ?? match[2] ?? match[3],
  );
}

test("the web importer and vendor graph exclude BFF/settings/theme packages", () => {
  const packageJson = JSON.parse(read("package.json"));
  const declared = new Set([
    ...Object.keys(packageJson.dependencies ?? {}),
    ...Object.keys(packageJson.devDependencies ?? {}),
  ]);
  const lockKeys = new Set(importerKeys(read("pnpm-lock.yaml")));
  const vendorRoot = join(WEB_ROOT, "vendor", "dsh-rc8");
  assert.ok(existsSync(vendorRoot), "the pinned DSH vendor root must exist");
  const vendored = new Set(
    readdirSync(vendorRoot, { withFileTypes: true })
      .filter((entry) => entry.isDirectory())
      .map((entry) => `@deepseek-ai/${entry.name}`),
  );

  for (const excluded of EXCLUDED_PACKAGES) {
    assert.equal(declared.has(excluded), false, `${excluded} must not be a direct web dependency`);
    assert.equal(lockKeys.has(excluded), false, `${excluded} must not be an importer dependency`);
    assert.equal(vendored.has(excluded), false, `${excluded} must not be copied into vendor/dsh-rc8`);
  }
});

test("the boot manifest contains only resolvable approved graph entries", () => {
  const graph = JSON.parse(read("public/boot-graph.json"));
  assert.ok(Array.isArray(graph.entries), "boot graph entries must be an array");
  assert.ok(graph.entries.length > 0, "boot graph must not be empty");

  const ids = new Set();
  for (const entry of graph.entries) {
    assert.equal(typeof entry.id, "string", "every graph entry needs an id");
    assert.equal(ids.has(entry.id), false, `duplicate graph entry: ${entry.id}`);
    ids.add(entry.id);
    assert.match(entry.url, /^\/plugins\/.+\/client\.js\?rev=[0-9a-f]+$/);
    for (const excluded of EXCLUDED_PACKAGES) {
      assert.doesNotMatch(JSON.stringify(entry), packageNamePattern(excluded));
    }
  }

  assert.ok(ids.has(CONNECTION_PACKAGE), "the DSH connection seam must remain in the boot graph");
  for (const entry of graph.entries) {
    for (const injected of entry.inject ?? []) {
      assert.equal(ids.has(injected), true, `${entry.id} injects an absent graph entry ${injected}`);
    }
  }

  const bootScript = read("public/boot-graph.js");
  assert.match(bootScript, /window\.__DSH_BOOT__\s*=/);
  for (const excluded of EXCLUDED_PACKAGES) {
    assert.doesNotMatch(bootScript, packageNamePattern(excluded));
  }
});

test("connection-voie is the required replacement for the stock connection plugin", () => {
  const pluginPath = join(WEB_ROOT, "src", "connection-voie", "plugin.ts");
  assert.ok(existsSync(pluginPath), "src/connection-voie/plugin.ts must exist");
  const plugin = read("src/connection-voie/plugin.ts");

  for (const exported of [
    "VoieCarrier",
    "createConnectionHandle",
    "createCarrierApi",
    "inject",
    "apply",
  ]) {
    assert.match(
      plugin,
      new RegExp(`export\\s*\\{[^}]*\\b${exported}\\b`, "s"),
      `connection-voie must export ${exported}`,
    );
  }
  assert.match(plugin, /from\s+["']\.\.\/carrier\/(?:voie|types)\.ts["']/);
  for (const excluded of EXCLUDED_PACKAGES) {
    assert.doesNotMatch(plugin, packageNamePattern(excluded));
  }
});

test("the DSH mount seam exposes an idempotent mount and an explicit unmount", () => {
  const lifecycle = read("src/dsh-lifecycle.ts");
  assert.match(lifecycle, /export\s+function\s+mountDshApp\s*\(\)\s*:\s*Promise<void>/);
  assert.match(lifecycle, /export\s+async\s+function\s+unmountDshApp\s*\(\)\s*:\s*Promise<void>/);
  assert.match(lifecycle, /getElementById\(["']voie-dsh-root["']\)/);
  assert.match(lifecycle, /new\s+AppWebEntry\s*\(/);
  assert.match(lifecycle, /entry\.dispose\s*\(\)/);
  assert.match(lifecycle, /dshEntry\s*!==\s*undefined/);
});
