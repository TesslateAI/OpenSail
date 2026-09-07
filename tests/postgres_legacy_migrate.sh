#!/usr/bin/env bash
# Executable regression: legacy PGDATA (bootstrap role app, local SCRAM)
# must migrate under the current initializer without appending local trust.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
init="${root}/nix/runtime/voie-postgres-init.sh"
command -v initdb >/dev/null || {
  printf 'postgres_legacy_migrate: initdb is missing from PATH\n' >&2
  exit 1
}
command -v pg_ctl >/dev/null || {
  printf 'postgres_legacy_migrate: pg_ctl is missing from PATH\n' >&2
  exit 1
}
command -v psql >/dev/null || {
  printf 'postgres_legacy_migrate: psql is missing from PATH\n' >&2
  exit 1
}
command -v pg_isready >/dev/null || {
  printf 'postgres_legacy_migrate: pg_isready is missing from PATH\n' >&2
  exit 1
}

export VOIE_SECURITY_PROFILE=2
# Host TCP 5432 may already be in use. This regression only needs the Unix socket.
export VOIE_PG_LISTEN_ADDRESSES=

work="$(mktemp -d "${TMPDIR:-/tmp}/voie-pg-legacy-XXXXXX")"
cleanup() {
  if [ -n "${PGDATA:-}" ] && [ -d "$PGDATA" ]; then
    pg_ctl -D "$PGDATA" -m fast -w stop >/dev/null 2>&1 || true
  fi
  rm -f /tmp/voie-postgres-password
  rm -rf "$work"
}
trap cleanup EXIT

PGDATA="$work/pgdata"
SOCKET_DIR="$work/run"
PASSWORD_FILE="$work/tenant-password"
LEGACY_PW='tenant-legacy-credential'
MARKER='voie-legacy-marker-a1b2c3'
mkdir -p "$PGDATA" "$SOCKET_DIR"
printf '%s\n' "$LEGACY_PW" >"$PASSWORD_FILE"
chmod 600 "$PASSWORD_FILE"
printf '%s\n' "$LEGACY_PW" >"$work/initdb-pw"

initdb -D "$PGDATA" --username=app --auth-host=scram-sha-256 --auth-local=scram-sha-256 \
  --pwfile="$work/initdb-pw" >/dev/null
# Legacy clusters keep a first-match local SCRAM rule. The initializer must
# not depend on an appended trust line.
if ! grep -E '^local[[:space:]]+all[[:space:]]+all[[:space:]]+scram-sha-256' "$PGDATA/pg_hba.conf" >/dev/null; then
  printf 'legacy pg_hba.conf lost local SCRAM\n' >&2
  exit 1
fi

pg_ctl -D "$PGDATA" -o "-c listen_addresses= -c unix_socket_directories=$SOCKET_DIR" -w start >/dev/null
export PGPASSWORD="$LEGACY_PW"
super="$(psql -U app -d postgres -h "$SOCKET_DIR" -Atc "SELECT rolsuper FROM pg_roles WHERE rolname='app'")"
[ "$super" = "t" ] || {
  printf 'legacy app is not superuser (%s)\n' "$super" >&2
  exit 1
}
psql -U app -d postgres -h "$SOCKET_DIR" -v ON_ERROR_STOP=1 \
  -c "CREATE DATABASE app OWNER app;" >/dev/null
psql -U app -d app -h "$SOCKET_DIR" -v ON_ERROR_STOP=1 \
  -c "CREATE TABLE tenant_marker(id int PRIMARY KEY, note text); INSERT INTO tenant_marker VALUES (1, '$MARKER');" >/dev/null
psql -U app -d app -h "$SOCKET_DIR" -v ON_ERROR_STOP=1 \
  -c "CREATE SCHEMA tenant_extra; CREATE TABLE tenant_extra.items(id serial PRIMARY KEY, note text); INSERT INTO tenant_extra.items(note) VALUES ('$MARKER-extra');" >/dev/null
unset PGPASSWORD
pg_ctl -D "$PGDATA" -m fast -w stop >/dev/null

export PGDATA PASSWORD_FILE
export VOIE_PG_SOCKET_DIR="$SOCKET_DIR"
rm -f /tmp/voie-postgres-password
bash "$init" >"$work/init-1.log" 2>&1 &
init_pid=$!
ready=0
for _ in $(seq 1 120); do
  if [ -f "$PGDATA/voie-security-generation-2" ] &&
    kill -0 "$init_pid" 2>/dev/null &&
    pg_isready -h "$SOCKET_DIR" -q &&
    PGPASSWORD="$LEGACY_PW" psql -U app -d app -h "$SOCKET_DIR" -Atc "SELECT 1" 2>/dev/null | grep -qx 1; then
    ready=1
    break
  fi
  if [ -f "$PGDATA/voie-security-generation-2" ] && ! kill -0 "$init_pid" 2>/dev/null; then
    break
  fi
  sleep 0.25
done
if [ "$ready" != 1 ]; then
  printf 'initializer did not start postgres after legacy migrate\n' >&2
  cat "$work/init-1.log" >&2 || true
  exit 1
fi

export PGPASSWORD="$LEGACY_PW"
note="$(psql -U app -d app -h "$SOCKET_DIR" -Atc "SELECT note FROM tenant_marker WHERE id=1")"
[ "$note" = "$MARKER" ] || {
  printf 'tenant marker did not survive migrate (%s)\n' "$note" >&2
  exit 1
}
extra="$(psql -U app -d app -h "$SOCKET_DIR" -Atc "SELECT note FROM tenant_extra.items ORDER BY id LIMIT 1")"
[ "$extra" = "$MARKER-extra" ] || {
  printf 'second schema did not survive migrate (%s)\n' "$extra" >&2
  exit 1
}
seq_ok="$(psql -U app -d app -h "$SOCKET_DIR" -Atc "SELECT pg_get_serial_sequence('tenant_extra.items','id') IS NOT NULL")"
[ "$seq_ok" = "t" ] || {
  printf 'second-schema sequence did not survive migrate\n' >&2
  exit 1
}
psql -U app -d app -h "$SOCKET_DIR" -v ON_ERROR_STOP=1 \
  -c "CREATE TABLE tenant_extra.after_migrate(id int);" >/dev/null
psql -U app -d app -h "$SOCKET_DIR" -v ON_ERROR_STOP=1 \
  -c "INSERT INTO tenant_extra.after_migrate VALUES (7);" >/dev/null
created="$(psql -U app -d app -h "$SOCKET_DIR" -Atc "SELECT id FROM tenant_extra.after_migrate")"
[ "$created" = "7" ] || {
  printf 'tenant CREATE/INSERT after migrate failed (%s)\n' "$created" >&2
  exit 1
}
flags="$(psql -U app -d app -h "$SOCKET_DIR" -Atc "SELECT CASE WHEN rolsuper THEN 't' ELSE 'f' END||','||CASE WHEN rolcreatedb THEN 't' ELSE 'f' END||','||CASE WHEN rolcreaterole THEN 't' ELSE 'f' END||','||CASE WHEN rolreplication THEN 't' ELSE 'f' END||','||CASE WHEN rolbypassrls THEN 't' ELSE 'f' END FROM pg_roles WHERE rolname='app'")"
[ "$flags" = "f,f,f,f,f" ] || {
  printf 'app privileged flags after migrate: %s\n' "$flags" >&2
  exit 1
}
platform="$(psql -U app -d app -h "$SOCKET_DIR" -Atc "SELECT CASE WHEN rolcanlogin THEN 't' ELSE 'f' END FROM pg_roles WHERE rolname='voie_platform'")"
[ "$platform" = "f" ] || {
  printf 'voie_platform.rolcanlogin is %s\n' "$platform" >&2
  exit 1
}
copy_rc=0
psql -U app -d app -h "$SOCKET_DIR" -c "COPY (SELECT 1) TO PROGRAM 'true'" >/dev/null 2>&1 || copy_rc=$?
[ "$copy_rc" -ne 0 ] || {
  printf 'COPY ... PROGRAM succeeded as tenant app\n' >&2
  exit 1
}
unset PGPASSWORD
if [ -e /tmp/voie-postgres-password ]; then
  printf '/tmp/voie-postgres-password still present after migrate\n' >&2
  exit 1
fi
if [ ! -f "$PGDATA/voie-security-generation-2" ]; then
  printf 'security generation marker missing after migrate\n' >&2
  exit 1
fi
if [ ! -f "$PGDATA/voie-platform-contract" ]; then
  printf 'platform-contract marker missing after migrate with VOIE_SECURITY_PROFILE=2\n' >&2
  exit 1
fi

kill "$init_pid" >/dev/null 2>&1 || true
pg_ctl -D "$PGDATA" -m fast -w stop >/dev/null 2>&1 || true
wait "$init_pid" 2>/dev/null || true

bash "$init" >"$work/init-2.log" 2>&1 &
init_pid=$!
ready=0
for _ in $(seq 1 80); do
  if pg_isready -h "$SOCKET_DIR" -q; then
    ready=1
    break
  fi
  if ! kill -0 "$init_pid" 2>/dev/null; then
    break
  fi
  sleep 0.25
done
if [ "$ready" != 1 ]; then
  printf 'second startup after migrate failed\n' >&2
  cat "$work/init-2.log" >&2 || true
  exit 1
fi
export PGPASSWORD="$LEGACY_PW"
note="$(psql -U app -d app -h "$SOCKET_DIR" -Atc "SELECT note FROM tenant_marker WHERE id=1")"
[ "$note" = "$MARKER" ] || {
  printf 'tenant marker missing after second startup\n' >&2
  exit 1
}
unset PGPASSWORD
kill "$init_pid" >/dev/null 2>&1 || true
pg_ctl -D "$PGDATA" -m fast -w stop >/dev/null 2>&1 || true
wait "$init_pid" 2>/dev/null || true

# Interrupted replacement: volume root has the migrate lock and voie-old
# (legacy PGDATA), no PG_VERSION. Init must unstash and migrate, not initdb.
RESUME="$work/resume"
mkdir -p "$RESUME/pgdata" "$RESUME/run"
printf '%s\n' "$LEGACY_PW" >"$RESUME/tenant-password"
chmod 600 "$RESUME/tenant-password"
printf '%s\n' "$LEGACY_PW" >"$RESUME/initdb-pw"
PGDATA="$RESUME/pgdata"
SOCKET_DIR="$RESUME/run"
PASSWORD_FILE="$RESUME/tenant-password"
initdb -D "$PGDATA" --username=app --auth-host=scram-sha-256 --auth-local=scram-sha-256 \
  --pwfile="$RESUME/initdb-pw" >/dev/null
pg_ctl -D "$PGDATA" -o "-c listen_addresses= -c unix_socket_directories=$SOCKET_DIR" -w start >/dev/null
export PGPASSWORD="$LEGACY_PW"
psql -U app -d postgres -h "$SOCKET_DIR" -v ON_ERROR_STOP=1 \
  -c "CREATE DATABASE app OWNER app;" >/dev/null
psql -U app -d app -h "$SOCKET_DIR" -v ON_ERROR_STOP=1 \
  -c "CREATE TABLE tenant_marker(id int PRIMARY KEY, note text); INSERT INTO tenant_marker VALUES (1, '$MARKER');" >/dev/null
unset PGPASSWORD
pg_ctl -D "$PGDATA" -m fast -w stop >/dev/null
mkdir -p "$PGDATA/voie-old"
for x in "$PGDATA"/*; do
  [ -e "$x" ] || continue
  base=$(basename "$x")
  case "$base" in
    voie-old|lost+found) continue ;;
  esac
  mv "$x" "$PGDATA/voie-old/"
done
printf '1\n' >"$PGDATA/voie-security-migrate-in-progress"
export PGDATA PASSWORD_FILE
export VOIE_PG_SOCKET_DIR="$SOCKET_DIR"
rm -f /tmp/voie-postgres-password
bash "$init" >"$RESUME/init.log" 2>&1 &
init_pid=$!
ready=0
for _ in $(seq 1 120); do
  if [ -f "$PGDATA/voie-security-generation-2" ] &&
    kill -0 "$init_pid" 2>/dev/null &&
    pg_isready -h "$SOCKET_DIR" -q &&
    PGPASSWORD="$LEGACY_PW" psql -U app -d app -h "$SOCKET_DIR" -Atc "SELECT 1" 2>/dev/null | grep -qx 1; then
    ready=1
    break
  fi
  if [ -f "$PGDATA/voie-security-generation-2" ] && ! kill -0 "$init_pid" 2>/dev/null; then
    break
  fi
  sleep 0.25
done
if [ "$ready" != 1 ]; then
  printf 'initializer did not resume postgres from voie-old\n' >&2
  cat "$RESUME/init.log" >&2 || true
  exit 1
fi
export PGPASSWORD="$LEGACY_PW"
note="$(psql -U app -d app -h "$SOCKET_DIR" -Atc "SELECT note FROM tenant_marker WHERE id=1")"
[ "$note" = "$MARKER" ] || {
  printf 'tenant marker did not survive voie-old resume (%s)\n' "$note" >&2
  exit 1
}
unset PGPASSWORD
if [ -d "$PGDATA/voie-old" ]; then
  printf 'voie-old still present after resume migrate\n' >&2
  exit 1
fi
kill "$init_pid" >/dev/null 2>&1 || true
pg_ctl -D "$PGDATA" -m fast -w stop >/dev/null 2>&1 || true
wait "$init_pid" 2>/dev/null || true

printf 'postgres_legacy_migrate: ok\n'
