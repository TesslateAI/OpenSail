# Third-party notices — `@voie/web`

This browser island redistributes the approved DeepSeek Harness browser
surface at `dsh-v0.1.0-rc.8` (`141eb6fef83422698aef7a981029e843e8161534`).
The packages below are pinned in this island's `pnpm-lock.yaml`; the copied
DSH packages also retain their upstream `LICENSE` files under
`vendor/dsh-rc8/`. The DSH source is MIT-licensed and comes from
`https://github.com/deepseek-ai/deepseek-harness`.

The browser graph contains only the packages listed below. The VOIE
`connection-voie` adapter replaces the stock connection client bundle at
compose time. No provider credential, host, persistence, Agent, or official
DSH branding package is served by this graph.

| Package | Version | License | Upstream |
| --- | --- | --- | --- |
| `@deepseek-ai/cordis` | 4.0.1 | MIT | https://github.com/deepseek-ai/deepseek-harness |
| `@deepseek-ai/cordis-plugin-loader` | 1.0.2 | MIT | https://github.com/deepseek-ai/deepseek-harness |
| `@deepseek-ai/dsh-client-connection` | 0.1.0-rc.8 | MIT | https://github.com/deepseek-ai/deepseek-harness |
| `@deepseek-ai/dsh-client-locale` | 0.1.0-rc.8 | MIT | https://github.com/deepseek-ai/deepseek-harness |
| `@deepseek-ai/dsh-client-modules` | 0.1.0-rc.8 | MIT | https://github.com/deepseek-ai/deepseek-harness |
| `@deepseek-ai/dsh-client-runtime` | 0.1.0-rc.8 | MIT | https://github.com/deepseek-ai/deepseek-harness |
| `@deepseek-ai/dsh-client-ui-conversation` | 0.1.0-rc.8 | MIT | https://github.com/deepseek-ai/deepseek-harness |
| `@deepseek-ai/dsh-client-ui-layout` | 0.1.0-rc.8 | MIT | https://github.com/deepseek-ai/deepseek-harness |
| `@deepseek-ai/dsh-client-ui-primitives` | 0.1.0-rc.8 | MIT | https://github.com/deepseek-ai/deepseek-harness |
| `@deepseek-ai/dsh-client-ui-renderer` | 0.1.0-rc.8 | MIT | https://github.com/deepseek-ai/deepseek-harness |
| `@deepseek-ai/dsh-client-ui-sidebar` | 0.1.0-rc.8 | MIT | https://github.com/deepseek-ai/deepseek-harness |
| `@deepseek-ai/dsh-client-ui-slots` | 0.1.0-rc.8 | MIT | https://github.com/deepseek-ai/deepseek-harness |
| `@deepseek-ai/dsh-client-ui-tool` | 0.1.0-rc.8 | MIT | https://github.com/deepseek-ai/deepseek-harness |
| `@deepseek-ai/dsh-client-ui-trajectory` | 0.1.0-rc.8 | MIT | https://github.com/deepseek-ai/deepseek-harness |
| `@deepseek-ai/dsh-client-web` | 0.1.0-rc.8 | MIT | https://github.com/deepseek-ai/deepseek-harness |
| `@deepseek-ai/dsh-typert-registry` | 0.1.0-rc.8 | MIT | https://github.com/deepseek-ai/deepseek-harness |

The graph's ordinary third-party closure preserves each dependency's own
notice through the pinned install. The principal browser-facing licenses are:

- React and React DOM 18.3.1 — MIT.
- `@deepseek-ai/cosmokit`, `@deepseek-ai/schemastery`, and
  `@standard-schema/spec` — MIT.
- Immer, Zustand, clsx, anser, KaTeX, Shiki, the mdast/micromark stack,
  `@tanstack/react-virtual`, `@tanstack/virtual-core`,
  `use-sync-external-store`, zod, and ws — MIT.
- `diff` 9.0.0 — BSD-3-Clause.
- `@ungap/structured-clone` — ISC; `argparse` — Python-2.0.
- Vite, esbuild, and `@vitejs/plugin-react` — MIT; TypeScript 5.8.3 —
  Apache-2.0.

MIT, BSD-3-Clause, ISC, and Python-2.0 require the applicable copyright and
permission notices to travel with redistributed copies or substantial bundled
copies. This file and the per-package vendored `LICENSE` files preserve that
record for the static artifact served by `voie-cloud`.
