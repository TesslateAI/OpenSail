#!/usr/bin/env bash
# Profile 1 guests must expose the binaries Fabric and the agent invoke by
# absolute /bin paths or PATH=/bin. dockerTools store paths are not enough
# when an image sets PATH=/bin:/usr/bin.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"

workspace="${root}/nix/runtime/voie-workspace-image.nix"
grep -q 'ln -sfn ${python3}/bin/python3 bin/python3' "$workspace" || {
  printf 'voie-workspace:v1 must pin /bin/python3\n' >&2
  exit 1
}
grep -q 'ln -sfn ${pack}/bin/voie-pack bin/voie-pack' "$workspace" || {
  printf 'voie-workspace:v1 must pin /bin/voie-pack\n' >&2
  exit 1
}
grep -q 'ln -sfn busybox bin/cat' "$workspace" || {
  printf 'voie-workspace:v1 must pin /bin/cat for pack copy-out\n' >&2
  exit 1
}

app="${root}/nix/runtime/voie-app-image.nix"
grep -q 'ln -sfn ${python}/bin/python3 bin/python3' "$app" || {
  printf 'voie-app:v1 must pin /bin/python3 with psycopg\n' >&2
  exit 1
}
grep -q 'ln -sfn busybox bin/wget' "$app" || {
  printf 'voie-app:v1 must pin /bin/wget\n' >&2
  exit 1
}
grep -q 'libvoie-bind-any.so' "$app" || {
  printf 'voie-app:v1 must pin /lib/libvoie-bind-any.so\n' >&2
  exit 1
}

gateway="${root}/nix/runtime/voie-gateway-image.nix"
grep -q 'ln -sfn ${caddy}/bin/caddy bin/caddy' "$gateway" || {
  printf 'voie-gateway:v1 must pin /bin/caddy\n' >&2
  exit 1
}

postgres="${root}/nix/runtime/voie-postgres-image.nix"
grep -q 'ln -sfn busybox bin/cat' "$postgres" || {
  printf 'voie-postgres:v1 must pin /bin/cat for backup copy-out\n' >&2
  exit 1
}

realize="${root}/crates/voie-fabricd/src"
if grep -RFq '["cp"' "$realize"; then
  printf 'Firecracker guests must not use kubectl cp\n' >&2
  exit 1
fi
if ! grep -Rq '/bin/cat' "$realize"; then
  printf 'guest copy-out must stream through kubectl exec /bin/cat\n' >&2
  exit 1
fi

printf 'guest image /bin pins hold\n'
