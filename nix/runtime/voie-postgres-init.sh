# Body of /bin/voie-postgres-init. Shebang and PATH are prefixed at image
# build so initdb and postgres resolve without depending on dockerTools PATH.
PGDATA="${PGDATA:-/var/lib/postgresql/data}"
PASSWORD_FILE=/run/voie/postgres-password
if [ ! -s "$PASSWORD_FILE" ]; then
  echo 'voie-postgres-init: password file missing' >&2
  exit 1
fi
# Privileged mount of /dev/pgdata runs as root. initdb refuses uid 0.
if ! id postgres >/dev/null 2>&1; then
  echo 'voie-postgres-init: postgres user missing' >&2
  exit 1
fi
# Fresh ext4 has lost+found. initdb refuses a non-empty data directory.
if [ ! -f "$PGDATA/PG_VERSION" ]; then
  rm -rf "$PGDATA/lost+found"
fi
chown -R postgres "$PGDATA"
PW=/tmp/voie-postgres-password
cp "$PASSWORD_FILE" "$PW"
chown postgres "$PW"
chmod 400 "$PW"
if [ ! -f "$PGDATA/PG_VERSION" ]; then
  su postgres -s /bin/sh -c \
    "initdb -D '$PGDATA' --username=app --pwfile='$PW' --auth-host=scram-sha-256 --auth-local=scram-sha-256"
fi
# ClusterIP peers are not localhost. Isolation is the Application NetworkPolicy.
if ! grep -q 'voie-cluster-listen' "$PGDATA/postgresql.conf" 2>/dev/null; then
  printf '%s\n' '# voie-cluster-listen' "listen_addresses = '*'" >> "$PGDATA/postgresql.conf"
fi
if ! grep -q 'voie-cluster-hba' "$PGDATA/pg_hba.conf" 2>/dev/null; then
  printf '%s\n' '# voie-cluster-hba' 'host all all 0.0.0.0/0 scram-sha-256' 'host all all ::/0 scram-sha-256' >> "$PGDATA/pg_hba.conf"
fi
chown postgres "$PGDATA/postgresql.conf" "$PGDATA/pg_hba.conf" 2>/dev/null || true
# initdb creates database postgres. The Application URL uses /app.
if [ ! -f "$PGDATA/voie-app-db" ]; then
  su postgres -s /bin/sh -c "postgres --single -D '$PGDATA' postgres" <<'SQL' || true
CREATE DATABASE app;
SQL
  touch "$PGDATA/voie-app-db"
  chown postgres "$PGDATA/voie-app-db"
fi
# Nix postgresql defaults unix_socket_directories to /run/postgresql.
mkdir -p /run/postgresql
chown postgres /run/postgresql
exec su postgres -s /bin/sh -c "exec postgres -D '$PGDATA' -c listen_addresses='*'"
