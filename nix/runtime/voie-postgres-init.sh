# Body of /bin/voie-postgres-init. Shebang and PATH are prefixed at image
# build so initdb and postgres resolve without depending on dockerTools PATH.
PGDATA="${PGDATA:-/var/lib/postgresql/data}"
PASSWORD_FILE="${PASSWORD_FILE:-/run/voie/postgres-password}"
SOCKET_DIR="${VOIE_PG_SOCKET_DIR:-/run/postgresql}"
MARKER="$PGDATA/voie-security-generation-2"
MIGRATE_LOCK="$PGDATA/voie-security-migrate-in-progress"
PW=/tmp/voie-postgres-password
SQL=/tmp/voie-postgres-setup.sql

cleanup_secrets() {
  rm -f "$PW" "$PW.platform" "$PW.migrate" "$SQL" /tmp/voie-postgres-migrate.sql
}
trap cleanup_secrets EXIT INT TERM

if [ ! -s "$PASSWORD_FILE" ]; then
  echo 'voie-postgres-init: password file missing' >&2
  exit 1
fi

pg_user() {
  if [ "$(id -u)" -eq 0 ] && id postgres >/dev/null 2>&1; then
    printf '%s' postgres
    return 0
  fi
  id -un
}

PGUSER_NAME="$(pg_user)"

pg_run() {
  if [ "$(id -u)" -eq 0 ] && [ "$PGUSER_NAME" = postgres ]; then
    su postgres -s /bin/sh -c "$1"
  else
    sh -c "$1"
  fi
}

if [ "$(id -u)" -eq 0 ] && [ "$PGUSER_NAME" = postgres ]; then
  if [ ! -f "$PGDATA/PG_VERSION" ]; then
    rm -rf "$PGDATA/lost+found"
  fi
  chown -R postgres "$PGDATA"
fi
mkdir -p "$SOCKET_DIR"
if [ "$(id -u)" -eq 0 ] && [ "$PGUSER_NAME" = postgres ]; then
  chown postgres "$SOCKET_DIR"
fi

sql_escape() {
  printf "%s" "$1" | sed "s/'/''/g"
}

# Keep the migrate lock and stash directory at the volume root so a retried
# database/secure can see an in-progress replacement instead of a blank LV.
pgdata_keep() {
  case "$1" in
    voie-old | lost+found | voie-security-migrate-in-progress) return 0 ;;
  esac
  return 1
}

unstash_voie_old() {
  stop_unix_only
  for x in "$PGDATA"/*; do
    [ -e "$x" ] || continue
    base=$(basename "$x")
    pgdata_keep "$base" && continue
    rm -rf "$x"
  done
  for x in "$PGDATA/voie-old"/*; do
    [ -e "$x" ] || continue
    mv "$x" "$PGDATA/"
  done
  rmdir "$PGDATA/voie-old" 2>/dev/null || true
}

write_listen_and_hba() {
  if ! grep -q 'voie-cluster-listen' "$PGDATA/postgresql.conf" 2>/dev/null; then
    printf '%s\n' '# voie-cluster-listen' "listen_addresses = '*'" >> "$PGDATA/postgresql.conf"
  fi
  if ! grep -q 'voie-cluster-hba' "$PGDATA/pg_hba.conf" 2>/dev/null; then
    printf '%s\n' '# voie-cluster-hba' 'host all all 0.0.0.0/0 scram-sha-256' 'host all all ::/0 scram-sha-256' >> "$PGDATA/pg_hba.conf"
  fi
  if [ "$(id -u)" -eq 0 ] && [ "$PGUSER_NAME" = postgres ]; then
    chown postgres "$PGDATA/postgresql.conf" "$PGDATA/pg_hba.conf" 2>/dev/null || true
  fi
}

start_unix_only() {
  pg_run "pg_ctl -D '$PGDATA' -o \"-c listen_addresses= -c unix_socket_directories=$SOCKET_DIR\" -w start"
}

stop_unix_only() {
  pg_run "pg_ctl -D '$PGDATA' -w stop" || true
}

copy_password() {
  cp "$PASSWORD_FILE" "$PW"
  if [ "$(id -u)" -eq 0 ] && [ "$PGUSER_NAME" = postgres ]; then
    chown postgres "$PW"
  fi
  chmod 400 "$PW"
}

# Authenticate as tenant app with the mounted password. Never append local
# trust: legacy clusters keep a first-match SCRAM local rule.
psql_app() {
  pg_run "PGPASSWORD=\$(cat '$PW') psql -U app -d ${1:-postgres} -h '$SOCKET_DIR' -v ON_ERROR_STOP=1 ${2:-}"
}

psql_app_at() {
  pg_run "PGPASSWORD=\$(cat '$PW') psql -U app -d postgres -h '$SOCKET_DIR' -Atc \"$1\""
}

psql_platform_trust() {
  pg_run "psql -U voie_platform -d postgres -h '$SOCKET_DIR' -v ON_ERROR_STOP=1"
}

# Helper superuser used only to rename the bootstrap role (session user
# cannot be renamed). Dropped before the listener starts.
psql_migrate() {
  pg_run "PGPASSWORD=\$(cat '$PW.migrate') psql -U voie_migrate -d ${1:-postgres} -h '$SOCKET_DIR' -v ON_ERROR_STOP=1"
}

psql_platform_login() {
  pg_run "PGPASSWORD=\$(cat '$PW') psql -U voie_platform -d postgres -h '$SOCKET_DIR' -v ON_ERROR_STOP=1"
}

verify_roles() {
  psql_app_at "SELECT CASE WHEN rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls THEN 0 ELSE 1 END FROM pg_roles WHERE rolname = 'app'" | grep -qx 1
  psql_app_at "SELECT CASE WHEN rolcanlogin THEN 0 ELSE 1 END FROM pg_roles WHERE rolname = 'voie_platform'" | grep -qx 1
}

# A killed replacement leaves tenant PGDATA in voie-old and no PG_VERSION at
# the volume root. Unstash and restart migrate; never initdb a blank cluster
# over the stash.
if [ -d "$PGDATA/voie-old" ] && [ ! -f "$MARKER" ]; then
  echo 'voie-postgres-init: resuming interrupted legacy migrate' >&2
  unstash_voie_old
fi

if [ ! -f "$PGDATA/PG_VERSION" ]; then
  copy_password
  pg_run "initdb -D '$PGDATA' --username=voie_platform --auth-host=scram-sha-256 --auth-local=trust"
  write_listen_and_hba
  start_unix_only
  APP_PW_SQL=$(sql_escape "$(cat "$PW")")
  psql_platform_trust <<SQL
CREATE ROLE app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD '$APP_PW_SQL';
CREATE DATABASE app OWNER app;
ALTER ROLE voie_platform NOLOGIN;
SQL
  if ! verify_roles; then
    echo 'voie-postgres-init: fresh role verification failed' >&2
    stop_unix_only
    exit 1
  fi
  printf '2\n' > "$MARKER"
  if [ "$(id -u)" -eq 0 ] && [ "$PGUSER_NAME" = postgres ]; then
    chown postgres "$MARKER"
  fi
  stop_unix_only
  cleanup_secrets
elif [ ! -f "$MARKER" ]; then
  # In-place privilege migration of the existing tenant cluster.
  # Never dump, never initdb a replacement, never move PGDATA.
  copy_password
  write_listen_and_hba
  start_unix_only
  if ! psql_app_at "SELECT 1 FROM pg_database WHERE datname = 'app'" | grep -qx '1'; then
    echo 'voie-postgres-init: legacy database app is missing' >&2
    stop_unix_only
    exit 1
  fi
  APP_PW_SQL=$(sql_escape "$(cat "$PW")")
  MIGRATE_PW=$(openssl rand -hex 24)
  printf '%s\n' "$MIGRATE_PW" > "$PW.migrate"
  if [ "$(id -u)" -eq 0 ] && [ "$PGUSER_NAME" = postgres ]; then
    chown postgres "$PW.migrate"
  fi
  chmod 400 "$PW.migrate"
  MIGRATE_PW_SQL=$(sql_escape "$MIGRATE_PW")
  unset MIGRATE_PW
  # PostgreSQL forbids removing SUPERUSER from the bootstrap role and
  # forbids renaming the session user. Create a helper superuser, rename
  # bootstrap app -> voie_platform, then mint tenant app. No dump, no new PGDATA.
  if ! psql_app postgres <<SQL
DO \$\$ BEGIN
  PERFORM pg_terminate_backend(pid) FROM pg_stat_activity
    WHERE usename = 'app' AND pid <> pg_backend_pid();
END \$\$;
DROP ROLE IF EXISTS voie_migrate;
CREATE ROLE voie_migrate SUPERUSER LOGIN PASSWORD '$MIGRATE_PW_SQL';
SQL
  then
    echo 'voie-postgres-init: migrate helper role failed' >&2
    stop_unix_only
    exit 1
  fi
  if ! psql_migrate postgres <<SQL
DROP ROLE IF EXISTS voie_platform;
ALTER ROLE app RENAME TO voie_platform;
CREATE ROLE app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD '$APP_PW_SQL';
ALTER DATABASE app OWNER TO app;
SQL
  then
    echo 'voie-postgres-init: bootstrap rename failed' >&2
    stop_unix_only
    exit 1
  fi
  if ! psql_migrate app <<'SQL'
DO $$
DECLARE obj record;
BEGIN
  FOR obj IN
    SELECT n.nspname AS nsp, c.relname AS rel,
           CASE c.relkind
             WHEN 'r' THEN 'TABLE'
             WHEN 'p' THEN 'TABLE'
             WHEN 'S' THEN 'SEQUENCE'
             WHEN 'v' THEN 'VIEW'
             WHEN 'm' THEN 'MATERIALIZED VIEW'
             WHEN 'f' THEN 'FOREIGN TABLE'
           END AS kind
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE c.relowner = 'voie_platform'::regrole
      AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
      AND c.relkind IN ('r', 'v', 'm', 'p', 'f')
  LOOP
    EXECUTE format('ALTER %s %I.%I OWNER TO app', obj.kind, obj.nsp, obj.rel);
  END LOOP;
  FOR obj IN
    SELECT n.nspname AS nsp, c.relname AS rel
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE c.relowner = 'voie_platform'::regrole
      AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
      AND c.relkind = 'S'
  LOOP
    EXECUTE format('ALTER SEQUENCE %I.%I OWNER TO app', obj.nsp, obj.rel);
  END LOOP;
  FOR obj IN
    SELECT n.nspname AS nsp
    FROM pg_namespace n
    WHERE n.nspowner = 'voie_platform'::regrole
      AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
  LOOP
    EXECUTE format('ALTER SCHEMA %I OWNER TO app', obj.nsp);
  END LOOP;
  FOR obj IN
    SELECT p.oid::regprocedure::text AS fn
    FROM pg_proc p
    JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE p.proowner = 'voie_platform'::regrole
      AND n.nspname NOT IN ('pg_catalog', 'information_schema')
  LOOP
    EXECUTE format('ALTER FUNCTION %s OWNER TO app', obj.fn);
  END LOOP;
END $$;
SQL
  then
    echo 'voie-postgres-init: tenant ownership reassign failed' >&2
    stop_unix_only
    exit 1
  fi
  if ! psql_platform_login <<SQL
DROP ROLE voie_migrate;
ALTER ROLE voie_platform WITH SUPERUSER NOLOGIN;
SQL
  then
    echo 'voie-postgres-init: platform NOLOGIN failed' >&2
    stop_unix_only
    exit 1
  fi
  rm -f "$PW.migrate"
  if ! verify_roles; then
    echo 'voie-postgres-init: migrated role verification failed' >&2
    stop_unix_only
    exit 1
  fi
  printf '2\n' > "$MARKER"
  if [ "$(id -u)" -eq 0 ] && [ "$PGUSER_NAME" = postgres ]; then
    chown postgres "$MARKER"
  fi
  rm -f "$MIGRATE_LOCK"
  stop_unix_only
  cleanup_secrets
else
  write_listen_and_hba
  copy_password
  start_unix_only
  APP_PW_SQL=$(sql_escape "$(cat "$PW")")
  psql_app postgres <<SQL
ALTER ROLE app PASSWORD '$APP_PW_SQL';
SQL
  stop_unix_only
  cleanup_secrets
fi

rm -f "$PW" "$SQL"
# In-place platform contract already ran through psql as app-before-demotion
# or voie_platform-before-NOLOGIN. Do not use postgres --single here: the
# OS user is not a database role, and --single would require a host-specific
# superuser. Fabric observes live roles; this marker is not product truth.
WANTED="${VOIE_SECURITY_PROFILE:-1}"
if [ "$WANTED" -ge 2 ] 2>/dev/null; then
  printf '2\n' > "$PGDATA/voie-platform-contract"
  if [ "$(id -u)" -eq 0 ] && [ "$PGUSER_NAME" = postgres ]; then
    chown postgres "$PGDATA/voie-platform-contract"
  fi
fi
mkdir -p "$SOCKET_DIR"
if [ "$(id -u)" -eq 0 ] && [ "$PGUSER_NAME" = postgres ]; then
  chown postgres "$SOCKET_DIR"
fi
if [ "${VOIE_PG_LISTEN_ADDRESSES+set}" = set ]; then
  PG_CMD="exec postgres -D '$PGDATA' -c listen_addresses='$VOIE_PG_LISTEN_ADDRESSES' -c unix_socket_directories='$SOCKET_DIR'"
else
  PG_CMD="exec postgres -D '$PGDATA' -c listen_addresses='*' -c unix_socket_directories='$SOCKET_DIR'"
fi
if [ "$(id -u)" -eq 0 ] && [ "$PGUSER_NAME" = postgres ]; then
  exec su postgres -s /bin/sh -c "$PG_CMD"
fi
exec sh -c "$PG_CMD"
