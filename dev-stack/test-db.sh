#!/usr/bin/env bash
# Runs one focused cargo test invocation against the local development
# PostgreSQL without printing or persisting any credential value. The URL is
# forwarded to the child process through its environment only.
#
# Usage: dev-stack/test-db.sh <cargo test arguments...>
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
env_file="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/voie-dev-cloud/env"
if [[ -z "${VOIE_TEST_DATABASE_URL:-}" ]]; then
  if [[ ! -r "$env_file" ]]; then
    printf 'dev-stack/test-db.sh: local stack env is unavailable; start dev-stack/up.sh first\n' >&2
    exit 2
  fi
  stack_db="$(sed -n 's/^export VOIE_DATABASE_URL=//p' "$env_file" | head -1 | tr -d '\r')"
  if [[ -z "$stack_db" ]]; then
    printf 'dev-stack/test-db.sh: stack env has no VOIE_DATABASE_URL entry\n' >&2
    exit 2
  fi
  export VOIE_TEST_DATABASE_URL="$stack_db"
fi
cd "$root"
exec cargo test -p voie-cloud "$@"