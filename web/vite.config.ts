import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";

const root = dirname(fileURLToPath(import.meta.url));

/** The vendored loader whose top-level evaluator this build defers. */
const LOADER_PACKAGE = "@deepseek-ai/cordis-plugin-loader";
const LOADER_VERSION = "1.0.2";
/** The loader's published entry, matched on the resolved (symlink-followed) path. */
const LOADER_ENTRY = /[\\/]@deepseek-ai[\\/]cordis-plugin-loader[\\/]lib[\\/]index\.js$/;
/**
 * The loader's top-level evaluator construction at 1.0.2, spelled as one
 * exact string so the match is an identity rather than an interpretation.
 */
const LOADER_EVALUATOR = 'const evaluate = new Function("ctx", "expr", `\n  with (ctx) {\n    return eval(expr)\n  }\n`);';
/** The export list must keep naming `evaluate` for the substitution to reach consumers. */
const LOADER_EVALUATOR_EXPORT = /^export \{[^}]*\bevaluate\b[^}]*\};$/m;
/** The virtual specifier answered by the stand-in module below. */
const JS_EXPR_SPECIFIER = "voie:loader-js-expr";
const JS_EXPR_STANDIN = join(root, "src/loader-js-expr.ts");

function voieLazyJsExpr(): Plugin {
  return {
    name: "voie-web-lazy-js-expr",
    enforce: "pre",
    resolveId(specifier) {
      if (specifier === JS_EXPR_SPECIFIER) return JS_EXPR_STANDIN;
      return null;
    },
    async load(id) {
      if (!LOADER_ENTRY.test(id)) return null;
      // Line endings are normalized before matching so a repacked tarball
      // that only changed them fails nothing; every other difference fails.
      const source = (await readFile(id, "utf8")).replaceAll("\r\n", "\n");
      const manifestPath = join(dirname(id), "..", "package.json");
      const manifest = JSON.parse(await readFile(manifestPath, "utf8")) as { version?: unknown };
      if (manifest.version !== LOADER_VERSION) {
        throw new Error(
          `web build: ${LOADER_PACKAGE} is ${String(manifest.version)}, but the lazy \`!js\` evaluator `
            + `rewrite is pinned to ${LOADER_VERSION}; re-read lib/index.js and update LOADER_VERSION and `
            + "LOADER_EVALUATOR together",
        );
      }
      const occurrences = source.split(LOADER_EVALUATOR).length - 1;
      if (occurrences !== 1) {
        throw new Error(
          `web build: ${LOADER_PACKAGE}@${LOADER_VERSION} lib/index.js contains ${String(occurrences)} copies `
            + "of the top-level `!js` evaluator this build defers, expected exactly 1; the vendored text changed "
            + "shape and LOADER_EVALUATOR must be re-anchored",
        );
      }
      if (!LOADER_EVALUATOR_EXPORT.test(source)) {
        throw new Error(
          `web build: ${LOADER_PACKAGE}@${LOADER_VERSION} lib/index.js no longer exports \`evaluate\`; the `
            + "substituted binding would not reach its consumers and the rewrite must be re-anchored",
        );
      }
      return {
        code: source.replace(LOADER_EVALUATOR, `import { evaluate } from ${JSON.stringify(JS_EXPR_SPECIFIER)};`),
        map: null,
      };
    },
  };
}

function stripSourceMappingUrl(): Plugin {
  return {
    name: "voie-web-strip-source-mapping-url",
    generateBundle(_options, bundle) {
      for (const item of Object.values(bundle)) {
        if (item.type === "chunk") {
          item.code = item.code.replace(/\/\/[#@]\s*sourceMappingURL=\S+/g, "");
        }
        if (item.type === "asset" && typeof item.source === "string") {
          item.source = item.source.replace(/\/\*[#@]\s*sourceMappingURL=\S+\s*\*\//g, "");
        }
      }
    },
  };
}

// Native VOIE Console build. Inputs are the static import graph under `src/`
// only: no glob imports or generated boot graph. Output is
// plain static files for the `voie-cloud` web asset server (`VOIE_WEB_ROOT`,
// default `web/dist`) under a `script-src 'self'` CSP.
export default defineConfig({
  plugins: [react(), stripSourceMappingUrl(), voieLazyJsExpr()],
  resolve: {
    dedupe: ["react", "react-dom"],
    alias: [
      { find: /^node:module$/, replacement: join(root, "src/node-module-stub.ts") },
    ],
  },
  define: {
    "process.versions.node": '"0.0.0"',
    "process.execArgv": "[]",
    "process.env.CORDIS_SHARED": "undefined",
  },
  esbuild: {
    legalComments: "none",
  },
  build: {
    target: "es2022",
    sourcemap: false,
    outDir: "dist",
    emptyOutDir: true,
    minify: false,
    cssMinify: false,
    modulePreload: false,
    rollupOptions: {
      output: {
        compact: false,
      },
    },
  },
  server: {
    port: 5173,
    strictPort: true,
    // Dev convenience only: forward same-origin API/auth paths to a local
    // `voie-cloud` process (default bind port 8080). Production serves
    // everything from one origin.
    proxy: {
      "/api": "http://127.0.0.1:8080",
      "/login": "http://127.0.0.1:8080",
      "/logout": "http://127.0.0.1:8080",
      "/oidc": "http://127.0.0.1:8080",
    },
  },
});
