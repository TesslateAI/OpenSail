#!/usr/bin/env bash
# Minimal declarative local cloud for development: real PostgreSQL and real
# Azure Blob HTTP semantics (SharedKey).
#
# Default Blob boundary: Floci-AZ, the documented floci/floci-az:latest
# rootless Podman image (https://github.com/floci-io/floci-az). It serves
# the Azure Blob REST protocol with path-style account addressing on one
# unified port. Set VOIE_DEV_CLOUD_EMULATOR=azurite explicitly to fall back
# to the pinned nixpkgs Azurite; any other value fails closed. The launcher
# never silently downgrades from Floci.
#
# The Floci image is provisioned explicitly and exactly once via
# `just dev-cloud-provision` (host Podman check, then one image pull).
# Neither `up` nor the wider dev stack ever pulls or launches Podman
# implicitly: they fail closed pointing at that recipe when Podman or the
# image is missing.
#
# All state is ephemeral under XDG_RUNTIME_DIR; credentials are generated per
# `up` and only ever written to the env file outside Git. No remote hosts.
#
# Usage: local-stack.sh {up|down|env|check|provision}
set -euo pipefail

LC_ALL=C
export LC_ALL

API_VERSION="2021-08-06"
ACCOUNT="voiedevlocal"
CONTAINER="voie-session-events"
DATABASE="voie_dev"
PG_USER="voiedev"
FLOCI_IMAGE="docker.io/floci/floci-az:latest"
FLOCI_CONTAINER="voie-dev-floci"
FLOCI_INTERNAL_PORT=4577
scope_prefix="${VOIE_DEV_STACK_SCOPE:-voie-dev-stack}"
repo_path="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck disable=SC1091
source "$repo_path/dev-stack/pid-guard.sh"

# Emulator selection happens once, up front, and fails closed on unknown
# values before any tool check or state mutation.
EMULATOR="${VOIE_DEV_CLOUD_EMULATOR:-floci-az}"
case "$EMULATOR" in
	floci-az | azurite) ;;
	*)
		printf 'dev-cloud: VOIE_DEV_CLOUD_EMULATOR must be "floci-az" or "azurite" (got: %s)\n' "$EMULATOR" >&2
		exit 2
		;;
esac

usage() {
	printf 'usage: %s {up|down|env|check|provision}\n' "$0" >&2
	exit 2
}

repo_root() {
	local dir
	dir=$(cd "$(dirname "$0")/.." && pwd)
	printf '%s' "$dir"
}

runtime_root() {
	local base="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
	printf '%s/voie-dev-cloud' "$base"
}

# Cloud children (PostgreSQL, the Podman/Floci-AZ boundary, or Azurite) must
# live inside the same fail-closed systemd resource domain as the rest of
# the dev stack: the shared capped voie-dev-stack.slice, either under a
# per-operation child scope or (legacy layout) inside the old single fixed
# scope. Direct invocation outside both always refuses.
require_scope() {
	local scope_name="${VOIE_DEV_STACK_SCOPE:-voie-dev-stack}"
	local cgroup_path
	[[ "$scope_name" =~ ^voie-dev-stack(-[A-Za-z0-9]+)?$ ]] ||
		die "invalid VOIE_DEV_STACK_SCOPE; refusing cloud operation"
	cgroup_path="$(sed -n 's/^0:://p' /proc/self/cgroup)"
	case "$cgroup_path" in
		*/"$scope_name".scope)
			return 0 ;; # legacy single fixed scope, same ceilings
		*/"$scope_name".slice/*)
			return 0 ;; # per-operation child scope of the shared capped slice
	esac
	die "refusing to run outside ${scope_name}.slice; run 'just dev-cloud-up' or 'just dev-cloud-check'"
}

die() {
	printf 'dev-cloud: %s\n' "$1" >&2
	exit 1
}

# Re-run inside the pinned Nix dev shell when service binaries are absent so a
# fresh checkout needs no manual repair steps. Azurite is a dev-shell package;
# Podman deliberately is not: the Floci-AZ boundary is resolved from the host
# environment (require_podman) and is never satisfied by `nix develop`.
ensure_stack_tools() {
	local missing=()
	local name
	for name in initdb pg_ctl pg_isready psql openssl python3 curl; do
		command -v "$name" >/dev/null 2>&1 || missing+=("$name")
	done
	if [[ "$EMULATOR" == azurite ]]; then
		command -v azurite-blob >/dev/null 2>&1 || command -v azurite >/dev/null 2>&1 || missing+=("azurite")
	fi
	if ((${#missing[@]} == 0)); then
		return
	fi
	if [[ -n "${VOIE_DEV_CLOUD_NESTED:-}" ]] || ! command -v nix >/dev/null 2>&1; then
		die "missing required tools (${missing[*]}); install them or run through 'nix develop'"
	fi
	export VOIE_DEV_CLOUD_NESTED=1
	exec nix develop "$(repo_root)" -c bash "$0" "$@"
}

require_podman() {
	command -v podman >/dev/null 2>&1 ||
		die "podman not found in PATH; the default Floci-AZ boundary needs host Podman — run 'just dev-cloud-provision' to check prerequisites"
}

# Fail closed when the image was never provisioned. `up` never pulls: the
# only code path that fetches floci/floci-az:latest is cmd_provision.
require_floci_image() {
	require_podman
	podman image exists "$FLOCI_IMAGE" ||
		die "image $FLOCI_IMAGE is absent; provision it once with 'just dev-cloud-provision' (up never pulls)"
}

floci_container_running() {
	podman inspect --format '{{.State.Running}}' "$FLOCI_CONTAINER" 2>/dev/null | grep -qx true
}

floci_container_owned() {
	local state_dir="$1" expected actual
	[[ -s "$state_dir/floci-container-id" ]] || return 1
	expected="$(tr -d '[:space:]' <"$state_dir/floci-container-id")"
	actual="$(podman inspect --format '{{.Id}}' "$FLOCI_CONTAINER" 2>/dev/null)" || return 1
	[[ -n "$expected" && "$expected" == "$actual" ]]
}

# Emulator recorded by the running/paused stack, for mismatch detection.
# State written before emulator tracking existed had none but always ran
# Azurite, so an azurite pid line identifies it.
recorded_emulator() {
	local state_dir="$1" found=""
	if [[ -f "$state_dir/pids" ]]; then
		found=$(sed -n 's/^emulator=//p' "$state_dir/pids")
		if [[ -z "$found" ]] && grep -q '^azurite=' "$state_dir/pids"; then
			found=azurite
		fi
	fi
	printf '%s' "${found:-$EMULATOR}"
}

azurite_binary() {
	if command -v azurite-blob >/dev/null 2>&1; then
		printf '%s' azurite-blob
	else
		printf '%s' azurite
	fi
}

port_busy() {
	if (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; then
		return 0
	fi
	return 1
}

pid_process_alive() {
	[[ -n "${1:-}" ]] && kill -0 "$1" 2>/dev/null
}

http_date() {
	date -u '+%a, %d %b %Y %H:%M:%S GMT'
}

# HMAC-SHA256 SharedKey signature: key (base64) as $1, string-to-sign on stdin.
shared_key_signature() {
	python3 -c '
import base64, hashlib, hmac, sys
key = base64.b64decode(sys.argv[1])
print(base64.b64encode(hmac.new(key, sys.stdin.buffer.read(), hashlib.sha256).digest()).decode())
' "$1"
}

# Signed Azure Blob request body: prints HTTP status of PUT container.
create_container_status() {
	local host_port="$1" account="$2" container="$3" key_b64="$4"
	local date signature status url
	date=$(http_date)
	signature=$(printf 'PUT\n\n\n\n\n\n\n\n\n\n\n\nx-ms-date:%s\nx-ms-version:%s\n/%s/%s\nrestype:container' \
		"$date" "$API_VERSION" "$account" "$container" | shared_key_signature "$key_b64")
	url="http://${host_port}/${container}?restype=container"
	status=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 5 -X PUT \
		-H "Authorization: SharedKey ${account}:${signature}" \
		-H "x-ms-date: ${date}" \
		-H "x-ms-version: ${API_VERSION}" \
		-H 'Content-Length:' \
		"$url")
	printf '%s' "$status"
}

stack_running() {
	local state_dir="$1"
	[[ -f "$state_dir/env" && -f "$state_dir/pids" ]] || return 1
	local blob_emulator
	pid_guard_validate "$state_dir/postgres.pid" "$scope_prefix" || return 1
	blob_emulator=$(recorded_emulator "$state_dir")
	case "$blob_emulator" in
		floci-az)
			floci_container_owned "$state_dir/runtime" &&
				floci_container_running || return 1
			;;
		*)
			pid_guard_validate "$state_dir/azurite.pid" "$scope_prefix" || return 1
			;;
	esac
	return 0
}

# Start the pinned-nixpkgs Azurite fallback and wait until a SharedKey-signed
# PUT container succeeds against its path-style endpoint. Prints the Azurite
# pid on success.
start_azurite() {
	local runtime_dir="$1" blob_port="$2" blob_key="$3"
	local azurite_bin azurite_pid ready status
	azurite_bin=$(azurite_binary)
	AZURITE_ACCOUNTS="$ACCOUNT:$blob_key" nohup "$azurite_bin" \
		--blobHost 127.0.0.1 --blobPort "$blob_port" \
		--location "$runtime_dir/blob" \
		--silent --disableTelemetry >"$runtime_dir/azurite.log" 2>&1 &
	azurite_pid=$!

	ready=0
	for _ in $(seq 1 30); do
		status=$(create_container_status "${ACCOUNT}.blob.localhost:${blob_port}" "$ACCOUNT" "$CONTAINER" "$blob_key" || true)
		case "$status" in
			201 | 409)
				ready=1
				break
				;;
		esac
		pid_process_alive "$azurite_pid" || break
		sleep 1
	done
	if ((ready != 1)); then
		die "Azure Blob emulator did not become ready (last status: ${status:-none}); see $runtime_dir/azurite.log"
	fi
	printf '%s' "$azurite_pid"
}

# Start the default Floci-AZ boundary: one ephemeral rootless Podman
# container published on loopback only, memory-only storage, Azure Functions
# disabled (no Docker socket), documented strict auth mode requested, and
# cgroupfs management so the container stays inside the caller's systemd
# user scope instead of escaping into its own unit. Readiness is proven
# twice: GET /health answers, then a SharedKey-signed PUT container succeeds
# against the path-style account endpoint (201, or 409 when already there).
start_floci() {
	local runtime_dir="$1" blob_port="$2" blob_key="$3"
	local ready status
	require_floci_image
	if podman inspect "$FLOCI_CONTAINER" >/dev/null 2>&1; then
		if ! floci_container_owned "$runtime_dir"; then
			die "container name $FLOCI_CONTAINER is occupied by an unowned container; refusing to remove it"
		fi
		podman rm --force --ignore "$FLOCI_CONTAINER" >/dev/null 2>&1 || true
	fi
	if ! podman --cgroup-manager=cgroupfs run --detach --rm \
		--name "$FLOCI_CONTAINER" \
		--publish "127.0.0.1:${blob_port}:${FLOCI_INTERNAL_PORT}" \
		--env FLOCI_AZ_STORAGE_MODE=memory \
		--env FLOCI_AZ_SERVICES_FUNCTIONS_ENABLED=false \
		--env FLOCI_AZ_AUTH_MODE=strict \
		"$FLOCI_IMAGE" >"$runtime_dir/floci-container-id.tmp" 2>"$runtime_dir/floci.log"; then
		die "podman run failed for $FLOCI_IMAGE; see $runtime_dir/floci.log"
	fi
	if ! podman inspect --format '{{.Id}}' "$FLOCI_CONTAINER" >"$runtime_dir/floci-container-id"; then
		die "could not record Floci container ownership; refusing to continue"
	fi
	rm -f "$runtime_dir/floci-container-id.tmp"

	ready=0
	for _ in $(seq 1 30); do
		if curl --fail --silent "http://127.0.0.1:${blob_port}/health" >/dev/null 2>&1; then
			ready=1
			break
		fi
		floci_container_running || break
		sleep 1
	done
	if ((ready != 1)); then
		podman logs --tail 50 "$FLOCI_CONTAINER" >"$runtime_dir/floci.log" 2>&1 || true
		if floci_container_owned "$runtime_dir"; then
			podman rm --force --ignore "$FLOCI_CONTAINER" >/dev/null 2>&1 || true
		fi
		die "Floci-AZ did not become healthy; see $runtime_dir/floci.log"
	fi

	ready=0
	for _ in $(seq 1 30); do
		status=$(create_container_status "127.0.0.1:${blob_port}/${ACCOUNT}" "$ACCOUNT" "$CONTAINER" "$blob_key" || true)
		case "$status" in
			201 | 409)
				ready=1
				break
				;;
		esac
		floci_container_running || break
		sleep 1
	done
	if ((ready != 1)); then
		podman logs --tail 50 "$FLOCI_CONTAINER" >"$runtime_dir/floci.log" 2>&1 || true
		if floci_container_owned "$runtime_dir"; then
			podman rm --force --ignore "$FLOCI_CONTAINER" >/dev/null 2>&1 || true
		fi
		die "Floci-AZ rejected the signed container creation (last status: ${status:-none}); see $runtime_dir/floci.log"
	fi
}

cmd_up() {
	require_scope
	local state_dir runtime_dir pg_port blob_port
	state_dir=$(runtime_root)
	runtime_dir="$state_dir/runtime"
	pg_port="${VOIE_DEV_PG_PORT:-15432}"
	blob_port="${VOIE_DEV_BLOB_PORT:-11000}"

	if stack_running "$state_dir"; then
		local running_emulator
		running_emulator=$(recorded_emulator "$state_dir")
		if [[ "$running_emulator" != "$EMULATOR" ]]; then
			die "running cloud uses emulator '$running_emulator'; run 'just dev-cloud-down' before switching to '$EMULATOR'"
		fi
		printf 'dev-cloud already up\nenv file: %s\n' "$state_dir/env"
		return 0
	fi

	# Heal partial state from an interrupted run before starting fresh.
	cmd_down_quiet

	mkdir -p "$runtime_dir/pgdata" "$runtime_dir/blob"
	chmod 700 "$state_dir" "$runtime_dir"

	if port_busy "$pg_port"; then
		die "port $pg_port is in use by another process; set VOIE_DEV_PG_PORT to a free port"
	fi
	if port_busy "$blob_port"; then
		die "port $blob_port is in use by another process; set VOIE_DEV_BLOB_PORT to a free port"
	fi

	local pg_password blob_key
	pg_password=$(openssl rand -base64 24 | tr '+/' '-_')
	# Single-line base64: a wrapped key corrupts both the Azurite
	# AZURITE_ACCOUNTS parsing and SharedKey signing consumers.
	blob_key=$(openssl rand -base64 64 | tr -d '\n')
	# Azurite authenticates the documented emulator account on IPv4
	# path-style URLs. A random AZURITE_ACCOUNTS entry parses but every
	# SharedKey request against it returns AuthorizationFailure.
	if [[ "$EMULATOR" == azurite ]]; then
		ACCOUNT="devstoreaccount1"
		blob_key="Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw=="
	fi

	# --- PostgreSQL -------------------------------------------------------
	local pw_file="$runtime_dir/pg-pw"
	printf '%s' "$pg_password" >"$pw_file"
	chmod 600 "$pw_file"
	initdb --pgdata="$runtime_dir/pgdata" --username="$PG_USER" --pwfile="$pw_file" \
		--auth-local=trust --auth-host=scram-sha-256 --encoding=UTF8 --locale=C \
		--no-instructions >/dev/null
	rm -f "$pw_file"

	pg_ctl --pgdata="$runtime_dir/pgdata" --log="$runtime_dir/postgres.log" \
		--options="-c listen_addresses=127.0.0.1 -p $pg_port -k $runtime_dir" \
		--wait --timeout=30 start >/dev/null
	local postgres_pid
	postgres_pid=$(head -n 1 "$runtime_dir/pgdata/postmaster.pid")
	pid_guard_record "$postgres_pid" "$state_dir/postgres.pid" "$scope_prefix" || {
		die "could not record PostgreSQL ownership; refusing to continue"
	}
	printf 'postgres=%s\n' "$postgres_pid" >"$state_dir/pids"
	chmod 600 "$state_dir/pids"

	local ready=0
	for _ in $(seq 1 30); do
		if pg_isready --host=127.0.0.1 --port="$pg_port" --username="$PG_USER" --timeout=1 >/dev/null 2>&1; then
			ready=1
			break
		fi
		sleep 1
	done
	((ready == 1)) || die "PostgreSQL did not become ready; see $runtime_dir/postgres.log"

	PGPASSWORD="$pg_password" psql --host=127.0.0.1 --port="$pg_port" \
		--username="$PG_USER" --dbname=postgres --set=ON_ERROR_STOP=1 \
		--command="create database $DATABASE" >/dev/null

	# --- Azure Blob boundary ---------------------------------------------
	local azurite_pid=""
	if [[ "$EMULATOR" == floci-az ]]; then
		start_floci "$runtime_dir" "$blob_port" "$blob_key"
	else
		azurite_pid=$(start_azurite "$runtime_dir" "$blob_port" "$blob_key")
		pid_guard_record "$azurite_pid" "$state_dir/azurite.pid" "$scope_prefix" || {
			die "could not record Azurite ownership; refusing to continue"
		}
	fi

	# --- Env file outside Git --------------------------------------------
	# Floci-AZ addresses accounts as path segments on IPv4. Azurite uses
	# product-style Host `ACCOUNT.blob.localhost` (add that name to
	# /etc/hosts pointing at 127.0.0.1 when nss does not map it).
	local blob_endpoint
	blob_endpoint="http://127.0.0.1:${blob_port}/${ACCOUNT}"
	if [[ "$EMULATOR" == azurite ]]; then
		blob_endpoint="http://${ACCOUNT}.blob.localhost:${blob_port}"
	fi
	cat >"$state_dir/env" <<ENVEOF
# Ephemeral local dev cloud settings. Generated by dev-cloud/local-stack.sh.
export VOIE_DATABASE_URL=postgres://$PG_USER:$pg_password@127.0.0.1:$pg_port/$DATABASE
export VOIE_AZURE_BLOB_ACCOUNT=$ACCOUNT
export VOIE_AZURE_BLOB_KEY=$blob_key
export VOIE_AZURE_BLOB_CONTAINER=$CONTAINER
export VOIE_AZURE_BLOB_ENDPOINT=$blob_endpoint
export VOIE_DEV_CLOUD_EMULATOR=$EMULATOR
ENVEOF
	chmod 600 "$state_dir/env"
	{
		printf 'emulator=%s\n' "$EMULATOR"
		printf 'postgres=%s\n' "$postgres_pid"
		if [[ -n "$azurite_pid" ]]; then
			printf 'azurite=%s\n' "$azurite_pid"
		fi
		printf 'pg_port=%s\n' "$pg_port"
		printf 'blob_port=%s\n' "$blob_port"
	} >"$state_dir/pids"
	chmod 600 "$state_dir/pids"

	printf 'local dev cloud up (%s emulator, PostgreSQL :%s, Blob :%s)\nenv file: %s\n' \
		"$EMULATOR" "$pg_port" "$blob_port" "$state_dir/env"
}

stop_stack() {
	local state_dir="$1"
	pid_guard_stop "$state_dir/azurite.pid" "$scope_prefix"
	pid_guard_stop "$state_dir/postgres.pid" "$scope_prefix"
	# The Floci container is removed only when its recorded container ID still
	# owns the fixed name; a stale name or missing record is never force-removed.
	if command -v podman >/dev/null 2>&1; then
		if floci_container_owned "$state_dir/runtime"; then
			podman rm --force --ignore "$FLOCI_CONTAINER" >/dev/null 2>&1 || true
		else
			rm -f "$state_dir/runtime/floci-container-id"
		fi
	fi
	return 0
}

cmd_down_quiet() {
	stop_stack "$(runtime_root)"
	rm -rf "$(runtime_root)"
}

cmd_down() {
	local state_dir
	state_dir=$(runtime_root)
	if [[ ! -e "$state_dir" ]]; then
		printf 'local dev cloud already down\n'
		return 0
	fi
	stop_stack "$state_dir"
	rm -rf "$state_dir"
	printf 'local dev cloud down\n'
}

cmd_env() {
	local state_dir
	state_dir=$(runtime_root)
	[[ -f "$state_dir/env" ]] || die "no running stack; run 'just dev-cloud-up' first"
	printf '%s\n' "$state_dir/env"
}

cmd_check() {
	require_scope
	local state_dir
	state_dir=$(runtime_root)
	[[ -f "$state_dir/env" ]] || die "no running stack; run 'just dev-cloud-up' first"
	# shellcheck disable=SC1090
	source "$state_dir/env"
	cd "$(repo_root)"
	cargo test -p voie-cloud --test dev_local_stack -- --ignored --nocapture
}

# Explicit, idempotent provisioning of the default Floci-AZ boundary: verify
# host Podman support, then fetch the image exactly once. Nothing is started,
# no stack state is created, and this is the ONLY code path that pulls; `up`
# and the wider dev stack consume the result and fail closed without it.
cmd_provision() {
	require_podman
	if ! podman info >/dev/null 2>&1; then
		die "podman is present but not usable (rootless storage initialization failed); resolve Podman support first"
	fi
	if podman image exists "$FLOCI_IMAGE"; then
		printf 'floci image already provisioned: %s\n' "$FLOCI_IMAGE"
	else
		printf 'pulling %s (single explicit provisioning pull)\n' "$FLOCI_IMAGE"
		podman pull "$FLOCI_IMAGE"
	fi
	podman image exists "$FLOCI_IMAGE" || die "image pull did not yield $FLOCI_IMAGE"
	printf 'floci image %s ready (%s); nothing was started\n' \
		"$FLOCI_IMAGE" "$(podman image inspect --format '{{.ID}}' "$FLOCI_IMAGE")"
}

main() {
	(($# == 1)) || usage
	case "$1" in
		up | check)
			# Host Podman is checked before any Nix shell round-trip: the dev
			# shell never provides it, so a missing Podman must fail fast with
			# the provisioning pointer instead of repairing PostgreSQL tools.
			if [[ "$EMULATOR" == floci-az ]]; then
				require_podman
			fi
			ensure_stack_tools "$@"
			;;
	esac
	case "$1" in
		up) cmd_up ;;
		down) cmd_down ;;
		env) cmd_env ;;
		check) cmd_check ;;
		provision) cmd_provision ;;
		*) usage ;;
	esac
}

if [[ -z "${VOIE_DEV_CLOUD_LIB:-}" ]]; then
	main "$@"
fi
