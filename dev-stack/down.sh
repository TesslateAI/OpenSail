#!/usr/bin/env bash
# Tear down everything the dev stack owns, in dependency order:
#
#   1. every transient per-operation child scope (up/check/provision/cloud)
#      under the shared capped slice;
#   2. the parent slice itself — its stop SIGTERMs whatever still lives
#      inside the aggregate cap (builds, QEMU, cloud services, Caddy,
#      OIDC issuer, model emulator);
#   3. any pidfile survivors started with VOIE_DEV_STACK_ALLOW_UNSCOPED=1,
#      the local KVM VM, and the cloud data plane.
#
# Removes only the stack's own XDG_RUNTIME_DIR state plus the generated
# slice unit file, so no stale limits can leak into later sessions.
#
# Runs OUTSIDE the resource domain on purpose: it must be able to stop it
# from the outside.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
runtime_base="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
runtime_root="$runtime_base/voie-dev-stack"
scope_prefix="${VOIE_DEV_STACK_SCOPE:-voie-dev-stack}"
legacy_scope="$scope_prefix.scope"
slice_name="$scope_prefix.slice"
if [[ ! "$scope_prefix" =~ ^voie-dev-stack(-[A-Za-z0-9]+)?$ ]]; then
  printf 'dev-stack-down: invalid VOIE_DEV_STACK_SCOPE; refusing teardown\n' >&2
  exit 2
fi
# shellcheck disable=SC1091
source "$root/dev-stack/pid-guard.sh"
# Verified teardown core shared with up.sh: unit stops and pid-guard signals
# only, success proven against the live cgroup tree.
# shellcheck disable=SC1091
source "$root/dev-stack/teardown.sh"

# `command` bypasses this same-named wrapper: plain `just` here would
# resolve to the function itself and recurse forever instead of ever
# reaching the fallback teardown steps.
just() { command just --justfile "$root/justfile" "$@"; }

if ! stack_teardown "$scope_prefix" "$runtime_root" \
    "$runtime_base/voie-fabric-dev" ""; then
  printf 'dev-stack-down: owned processes survived teardown; refusing to report the stack as down\n' >&2
  exit 1
fi

# The cap holder is now verifiably empty; deactivate it and drop the
# generated unit so a later session cannot inherit stale limits.
if command -v systemctl >/dev/null 2>&1; then
  systemctl --user stop "$slice_name" 2>/dev/null || true
  if test -f "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$slice_name"; then
    rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$slice_name"
    systemctl --user daemon-reload
  fi
fi

just dev-fabric-down >/dev/null 2>&1 || true
bash "$root/dev-cloud/local-stack.sh" down >/dev/null 2>&1 || true

rm -rf "$runtime_root"
printf 'local dev stack is down\n'
