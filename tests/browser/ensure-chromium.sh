#!/usr/bin/env bash
# Resolve the Nix-pinned chrome-headless-shell used by just browser-smoke.
# Acceptance never downloads a browser. Operators may override with
# VOIE_SMOKE_EXECUTABLE pointing at the same pinned binary.
set -euo pipefail

if [[ -n "${VOIE_SMOKE_EXECUTABLE:-}" ]]; then
  if [[ -x "${VOIE_SMOKE_EXECUTABLE}" ]]; then
    printf '%s\n' "${VOIE_SMOKE_EXECUTABLE}"
    exit 0
  fi
  printf 'ensure-chromium: VOIE_SMOKE_EXECUTABLE is not executable\n' >&2
  exit 2
fi

if command -v chrome-headless-shell >/dev/null 2>&1; then
  command -v chrome-headless-shell
  exit 0
fi

printf 'ensure-chromium: nix-pinned chrome-headless-shell is not on PATH; use nix develop\n' >&2
exit 2
