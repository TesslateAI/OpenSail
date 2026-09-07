# DSH rc.8 browser graph — provenance

The browser conversation surface is the approved pinned DeepSeek Harness
(DSH) rc.8 client graph, copied verbatim from the pinned Whaled/DSH source
tree and adapted through a VOIE-owned same-origin carrier.

## Source

- Source tree: `/home/user/allgood/whaled/web` (pinned Whaled/DSH rc.8
  deployment, `pnpm-lock.yaml` lockfileVersion 9.0)
- Upstream repository: `https://github.com/deepseek-ai/deepseek-harness`
  (package `repository` fields), packages under `packages/client/*`
- Pinned versions: all `0.1.0-rc.8`; `@deepseek-ai/cordis` 4.0.1,
  `@deepseek-ai/cordis-plugin-loader` 1.0.2
- License: MIT (each vendored package carries its `LICENSE` file)

## Copied packages (16)

`vendor/dsh-rc8/<name>/` contains `lib/` (verbatim `client.js` bundles and
support files), `package.json`, and `LICENSE`:

- cordis, cordis-plugin-loader
- dsh-client-connection (replaced at compose time by the VOIE adapter)
- dsh-client-locale, dsh-client-modules, dsh-client-runtime
- dsh-client-ui-conversation, dsh-client-ui-layout, dsh-client-ui-primitives,
  dsh-client-ui-renderer, dsh-client-ui-sidebar (session-seat only),
  dsh-client-ui-slots, dsh-client-ui-tool, dsh-client-ui-trajectory
- dsh-client-web, dsh-typert-registry

## Excluded (per product model)

- `dsh-api-gateway` and `dsh-api-remotes` — no DSH gateway or remote package
  is declared, copied, or composed into the browser graph.
- `dsh-client-ui-settings` and `dsh-client-ui-theme` — no DSH settings or
  official branding/theme package is declared, copied, or composed.
- Whaled bearer/infrastructure/session bindings, separate web process, DSH
  provider settings/plugins, and DSH branding. VOIE owns the scope picker,
  management links, connection-voie seam, and the conversation root frame
  (`ui-layout` / `ui-sidebar` stay vendored for types but are dropped from
  the composed boot graph).
- `dsh-invariants` and every host, persistence, Agent, LLM, tools, credentials,
  and plugin-inventory package are outside the browser graph.

## VOIE-owned files

- `src/carrier/types.ts` — canonical carrier contract
- `src/carrier/voie.ts` — same-origin carrier over VOIE resources
- `src/connection-voie/**` — DSH connection-voie face (composed in place
  of the stock `dsh-client-connection` bundle)
- `src/shell/Shell.tsx` — VOIE-branded shell mount (scope switcher,
  management links)
- `scripts/compose-graph.mjs` — graph composer
- `public/boot-loader.js` — module-loader facade (VOIE-authored, same
  protocol as the DSH boot kernel expects)

## Rebuild

`pnpm compose` regenerates `public/plugins/**` and `public/boot-graph.js`
from `vendor/dsh-rc8/**` plus the VOIE connection plugin. The generated
`public/plugins/**` are build artifacts; the vendored `vendor/dsh-rc8/**`
are the recorded provenance.
