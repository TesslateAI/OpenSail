#!/usr/bin/env bash
# Entry point for run-scoped.sh's "cloud" mode: forwards to the declarative
# local cloud launcher so PostgreSQL and the Floci-AZ (or Azurite) boundary
# are started as children of the bounded stack systemd user scope.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if test $# -lt 1; then
  printf 'cloud.sh: missing command (up|down|env|check|provision)\n' >&2
  exit 2
fi
exec bash "$here/../dev-cloud/local-stack.sh" "$@"
