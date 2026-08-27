#!/usr/bin/env bash
# Local acceptance gates, run in order; the first blocked prerequisite fails
# with its exact edge message. No gate degrades into a fixture pass.
#
# `--report` prints the current resource-scope state (memory/swap limits,
# member PIDs) and exits without touching any service or starting a child.
set -euo pipefail

runtime_base="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
runtime_root="$runtime_base/voie-dev-stack"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
scope_prefix="${VOIE_DEV_STACK_SCOPE:-voie-dev-stack}"
slice_name="$scope_prefix.slice"
if [[ ! "$scope_prefix" =~ ^voie-dev-stack(-[A-Za-z0-9]+)?$ ]]; then
  printf 'dev-stack-check: invalid VOIE_DEV_STACK_SCOPE; refusing inspection\n' >&2
  exit 2
fi
# shellcheck disable=SC1091
source "$root/dev-stack/pid-guard.sh"

just() { command just --justfile "$root/justfile" "$@"; }

# Read-only view of the capped slice the stack lives in. Prints limits,
# member operation scopes, and PIDs only; never environment values,
# credentials, or endpoints with secrets.
report_scope() {
  local control_group cgroup_dir procs prop unit
  if ! command -v systemctl >/dev/null 2>&1; then
    printf 'dev-stack-check: scope report unavailable (no systemctl)\n'
    return 0
  fi
  control_group="$(systemctl --user show -P ControlGroup "$slice_name" 2>/dev/null || true)"
  if [[ -z "$control_group" ]] || ! test -d "/sys/fs/cgroup/$control_group"; then
    printf 'dev-stack-check: resource slice %s is not loaded (stack down)\n' "$slice_name"
    return 0
  fi
  cgroup_dir="/sys/fs/cgroup/$control_group"
  printf 'dev-stack-check: slice %s at %s\n' "$slice_name" "$control_group"
  for prop in memory.max memory.swap.max memory.current memory.swap.current pids.max cpu.max; do
    if test -r "$cgroup_dir/$prop"; then
      printf '  %-20s %s\n' "$prop" "$(cat "$cgroup_dir/$prop")"
    fi
  done
  while IFS= read -r unit; do
    printf '  operation scope:     %s\n' "$unit"
  done < <(systemctl --user list-units --all --plain --no-legend "$scope_prefix-*.scope" 2>/dev/null | awk '{print $1}')
  procs="$(find "$cgroup_dir" -name cgroup.procs -exec cat {} + 2>/dev/null || true)"
  if [[ -n "$procs" ]]; then
    printf '  member pids:         %s\n' "$(printf '%s\n' "$procs" | tr '\n' ' ')"
  else
    printf '  member pids:         (none)\n'
  fi
}

if [[ "${1:-}" == "--report" ]]; then
  report_scope
  exit 0
fi

if ! pid_guard_validate "$runtime_root/cloud.pid" "$scope_prefix"; then
  pid_guard_discard "$runtime_root/cloud.pid"
  printf 'dev-stack-check: blocked at cloud edge — voie-cloud is not running; run just dev-stack-up\n' >&2
  exit 2
fi
control_url="$(sed -n 's/^VOIE_CONTROL_URL=//p' "$runtime_root/stack.env")"

# C1/C2 substrate: one local KVM VM executing the runner and keeping a
# workspace marker across execution replacement (fabricd API over
# HTTPS+mTLS with the dev client identity).
just dev-fabric-up >/dev/null
tls_dir="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/voie-dev-stack/tls"
cfa() { curl --fail --silent --connect-timeout 2 --max-time 5 --show-error --cacert "$tls_dir/ca-bundle.pem" --cert "$tls_dir/client-cert.pem" --key "$tls_dir/client-key.pem" "$@"; }
base="https://127.0.0.1:17840"
create="$(cfa -X POST "$base/v1/workspaces" -H 'content-type: application/json' -d '{}')"
workspace_id="$(printf '%s' "$create" | jq -er .id)"
write="$(cfa -X POST "$base/v1/workspaces/$workspace_id/exec" \
  -H 'content-type: application/json' \
  -d "$(jq -cn --arg c 'printf marker > /workspace/marker' '{call_id:"dev-c1",command:$c}')")"
test "$(printf '%s' "$write" | jq -r .state)" = terminal
cfa -X POST "$base/v1/workspaces/$workspace_id/replace" >/dev/null
read_result="$(cfa -X POST "$base/v1/workspaces/$workspace_id/exec" \
  -H 'content-type: application/json' \
  -d "$(jq -cn '{call_id:"dev-c2",command:"cat /workspace/marker"}')")"
test "$(printf '%s' "$read_result" | jq -r .stdout)" = marker
printf 'dev-stack-check: C1/C2 substrate proved on the local VM\n'

# Load live boundary env from BOTH stack.env and dev-cloud/env without
# overriding explicit caller values, and normalize the Blob endpoint.
# Mirrors tests/live/common.sh:load_local_stack_env and dev-stack/up.sh's
# Blob normalization so live-c3/c5 can run without manual env assembly.
_load_env_file() {
  local file="$1"
  [ -f "$file" ] || return 0
  [ ! -L "$file" ] || return 0
  local line key value
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in ""|\#*) continue ;; esac
    case "$line" in "export "*) line="${line#export }" ;; esac
    case "$line" in [A-Za-z_]*"="*) ;; *) continue ;; esac
    key="${line%%=*}"
    value="${line#*=}"
    case "$key" in [A-Za-z_][A-Za-z0-9_]* ) ;; *) continue ;; esac
    eval "[ -n \"\${$key+set}\" ]" && continue
    case "$value" in \"*\" ) value="${value#\"}"; value="${value%\"}" ;; esac
    case "$value" in \'*\' ) value="${value#\'}"; value="${value%\'}" ;; esac
    if printf -v _probe test 2>/dev/null; then
      printf -v "$key" "%s" "$value"
      export "$key"
    else
      export "$key=$value"
    fi
  done <"$file"
}

stack_env="$runtime_root/stack.env"
dev_env="$runtime_base/voie-dev-cloud/env"
_load_env_file "$dev_env"
_load_env_file "$stack_env"
if [ -z "${VOIE_DATABASE_URL:-}" ] && [ -x "$root/dev-cloud/local-stack.sh" ]; then
  discovered="$("$root/dev-cloud/local-stack.sh" env 2>/dev/null || true)"
  [ -n "$discovered" ] && _load_env_file "$discovered"
fi
if [ -n "${VOIE_AZURE_BLOB_ENDPOINT:-}" ] && [ -n "${VOIE_AZURE_BLOB_CONTAINER:-}" ]; then
  blob_host="${VOIE_AZURE_BLOB_ENDPOINT#http://}"
  blob_host="${blob_host#https://}"
  blob_host="${blob_host%%:*}"
  blob_host="${blob_host%%/*}"
  case "$blob_host" in
    *.localhost)
      if ! curl --fail --silent "${VOIE_AZURE_BLOB_ENDPOINT}/${VOIE_AZURE_BLOB_CONTAINER}?restype=container" >/dev/null 2>&1; then
        suffix="${VOIE_AZURE_BLOB_ENDPOINT##*:}"
        export VOIE_AZURE_BLOB_ENDPOINT="http://127.0.0.1:${suffix}"
      fi
      ;;
  esac
fi

cargo_jobs="${VOIE_DEV_CARGO_JOBS:-2}"
case "$cargo_jobs" in
  '' | *[!0-9]*) cargo_jobs=2 ;;
esac
if (( cargo_jobs > 2 )); then cargo_jobs=2; fi
VOIE_DATABASE_URL="${VOIE_DATABASE_URL:?}" cargo test -p voie-cloud --test backend_vertical \
  --locked --jobs "$cargo_jobs" -- --ignored --nocapture live_c3 >/dev/null || {
  printf 'dev-stack-check: blocked at C3 edge — backend vertical failed against the local stack\n' >&2
  exit 3
}
printf 'dev-stack-check: C3 session/Blob/journal path proved against the local stack\n'

# C4/C5/C6 require a model provider. The dev stack exports VOIE_FIXTURE_MODEL=1
# when no real provider is configured (consumed by tests/live/common.sh and
# this gate). C4/C6 must refuse the deterministic fixture; C3 and C5 can
# prove with the emulator after the pickMarker fix (echo variants).
if [ "${VOIE_FIXTURE_MODEL:-}" = "1" ] || [ -n "${VOIE_DEV_FIXTURE_MODEL_URL:-}" ]; then
  printf 'dev-stack-check: C4/C6 fixture model active — real provider required, skipping C4/C6 (fixture mode)\n' >&2
  # C5 can still prove with the fixture emulator: it drives the real
  # PostgreSQL/Blob/mTLS Fabric chain plus the deterministic model that
  # correctly echoes the Run-echo marker (fixed in model-emulator.mjs).
  # Use the same BIND defaults as tests/live/activation-c5.sh (18085) which
  # does not collide with the fixture model port 18083 or the dev control
  # 18080, and load both env files via the helper above so no manual
  # assembly is needed.
  if [ -x "$root/tests/live/activation-c5.sh" ]; then
    # Run live-c5 as a child of this scope; its own load_local_stack_env
    # will pick up the same normalized env we just prepared.
    if bash "$root/tests/live/activation-c5.sh" >/tmp/dev-stack-check-c5.log 2>&1; then
      printf 'dev-stack-check: C5 resume/no-replay proved against the local stack (fixture model)\n'
    else
      rc=$?
      printf 'dev-stack-check: blocked at C5 edge — live-c5 failed against the local stack\n' >&2
      cat /tmp/dev-stack-check-c5.log >&2 || true
      exit 5
    fi
  else
    printf 'dev-stack-check: C5 script not found, skipping\n' >&2
  fi
  printf 'dev-stack-check: blocked at C4 edge — fixture model active; configure VOIE_MODEL_BASE_URL and VOIE_MODEL_API_KEY for a real provider to prove C4/C6\n' >&2
  exit 4
fi

if [ -z "${VOIE_MODEL_BASE_URL:-}" ]; then
  printf 'dev-stack-check: blocked at C4 edge — no local model provider configured (VOIE_MODEL_BASE_URL)\n' >&2
  exit 4
fi

# Real provider present: attempt C5 (and C4 if desired) with the same
# env handling. C5 is the cheaper resume gate; C4 would need a real model.
if [ -x "$root/tests/live/activation-c5.sh" ]; then
  if bash "$root/tests/live/activation-c5.sh" >/tmp/dev-stack-check-c5.log 2>&1; then
    printf 'dev-stack-check: C5 resume/no-replay proved against the local stack (real model)\n'
  else
    printf 'dev-stack-check: blocked at C5 edge — live-c5 failed\n' >&2
    cat /tmp/dev-stack-check-c5.log >&2 || true
    exit 5
  fi
fi

printf 'dev-stack-check: blocked at C4 edge — no local model provider configured (VOIE_MODEL_BASE_URL)\n' >&2
exit 4
