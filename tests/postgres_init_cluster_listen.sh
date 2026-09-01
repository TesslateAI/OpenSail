#!/usr/bin/env bash
# Dedicated PostgreSQL must accept ClusterIP peers. initdb defaults to
# localhost listen and 127.0.0.1 pg_hba; Application migrate runs from
# another Pod. Isolation stays on the Application NetworkPolicy.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
src="${root}/nix/runtime/voie-postgres-image.nix"
init="${root}/nix/runtime/voie-postgres-init.sh"
grep -q 'su postgres' "$init" || {
  printf 'postgres init must drop root before initdb\n' >&2
  exit 1
}
grep -Fq 'lost+found' "$init" || {
  printf 'postgres init must clear ext4 lost+found before initdb\n' >&2
  exit 1
}
grep -Fq '/run/postgresql' "$init" || {
  printf 'postgres init must create /run/postgresql for the lock file\n' >&2
  exit 1
}
grep -Fq 'CREATE DATABASE app' "$init" || {
  printf 'postgres init must create database app for DATABASE_URL\n' >&2
  exit 1
}
grep -q 'postgres:x:70:70' "$src" || {
  printf 'postgres image must ship uid 70\n' >&2
  exit 1
}
grep -q 'voie-cluster-listen' "$init" || {
  printf 'postgres init must persist listen_addresses=*\n' >&2
  exit 1
}
grep -q 'host all all 0.0.0.0/0 scram-sha-256' "$init" || {
  printf 'postgres init must allow scram from ClusterIP peers\n' >&2
  exit 1
}
grep -Fq "listen_addresses='*'" "$init" || {
  printf 'postgres exec must override listen_addresses\n' >&2
  exit 1
}
grep -q 'export PATH=' "$src" || {
  printf 'postgres init must put postgresql bin on PATH\n' >&2
  exit 1
}
grep -q 'cat ${./voie-postgres-init.sh}' "$src" || {
  printf 'postgres image must embed nix/runtime/voie-postgres-init.sh\n' >&2
  exit 1
}
grep -q 'ln -sfn ${postgresql_17}/bin/pg_isready bin/pg_isready' "$src" || {
  printf 'postgres image must pin /bin/pg_isready\n' >&2
  exit 1
}
pod="${root}/crates/voie-fabricd/src/product_realize.rs"
grep -Fq '/bin/pg_isready' "$pod" || {
  printf 'postgres Ready probe must call /bin/pg_isready\n' >&2
  exit 1
}
grep -Fq '127.0.0.1' "$pod" || {
  printf 'postgres Ready probe must use TCP localhost\n' >&2
  exit 1
}
grep -Fq 'devicePath: /dev/pgdata' "$pod" || {
  printf 'postgres Pod must attach a Firecracker block device\n' >&2
  exit 1
}
printf 'postgres init cluster listen invariants hold\n'
