#!/usr/bin/env bash
# One-time developer provisioning for the local dev stack: package installs
# and frontend builds that stack startup deliberately avoids repeating.
# Produces exactly the artifacts `up` consumes:
#   web/dist/index.html          served product bundle
#   activation/dist/index.js     guest activation entry point
# Idempotent: finished artifacts short-circuit their build steps.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

scope_name="${VOIE_DEV_STACK_SCOPE:-voie-dev-stack}"
if [[ ! "$scope_name" =~ ^voie-dev-stack(-[A-Za-z0-9]+)?$ ]]; then
  printf 'dev-stack-provision: invalid VOIE_DEV_STACK_SCOPE; refusing provisioning\n' >&2
  exit 2
fi
cgroup_path="$(sed -n 's/^0:://p' /proc/self/cgroup)"
case "$cgroup_path" in
  */"$scope_name".scope | */"$scope_name".slice/*) ;;
  *) printf 'dev-stack-provision: refusing to run outside %s.slice; run just dev-stack-provision\n' "$scope_name" >&2; exit 2 ;;
esac

if [[ ! -f "$root/web/dist/index.html" ]]; then
  pnpm --dir "$root/web" install --frozen-lockfile
  pnpm --dir "$root/web" build
fi
test -f "$root/web/dist/index.html"

if [[ ! -f "$root/activation/dist/index.js" ]]; then
  pnpm --dir "$root/activation" install --frozen-lockfile
  pnpm --dir "$root/activation" run build
fi
test -f "$root/activation/dist/index.js"

printf 'dev-stack provision complete: web/dist and activation/dist ready\n'
