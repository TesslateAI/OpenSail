#!/usr/bin/env bash
# Local zero-session-on-open probe for the browser smoke harness.
#
# Counts sessions-table rows in the shared dev PostgreSQL via
# VOIE_DATABASE_URL (read from the stack env file). Values are never
# echoed: the URL is consumed only through env-var indirection inside
# psql, and only the resulting count is printed.
#
# Usage: local-probe.sh sessions
set -euo pipefail

psql_bin="$(command -v psql || true)"
if [[ -z "$psql_bin" ]]; then
  for candidate in /nix/store/*-postgresql-*/bin/psql; do
    if [[ -x "$candidate" ]]; then
      psql_bin="$candidate"
      break
    fi
  done
fi
if [[ -z "$psql_bin" ]]; then
  printf 'local-probe: psql not found\n' >&2
  exit 1
fi

env_file="${VOIE_DEV_CLOUD_ENV:-/run/user/1000/voie-dev-cloud/env}"
# shellcheck disable=SC1091
source "$env_file"

if [[ -z "${VOIE_DATABASE_URL:-}" ]]; then
  printf 'local-probe: VOIE_DATABASE_URL not set after loading stack env\n' >&2
  exit 1
fi

case "${1:-}" in
  sessions)
    exec "$psql_bin" "$VOIE_DATABASE_URL" -tAc "select count(*) from sessions"
    ;;
  *)
    printf 'usage: local-probe.sh sessions\n' >&2
    exit 2
    ;;
esac