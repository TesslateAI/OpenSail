#!/usr/bin/env bash
# Read-only live preflight. Prints READY or a complete list of missing
# prerequisites. Does not mutate product state.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=tests/live/common.sh
source "${ROOT}/tests/live/common.sh"

MISSING=()
note() { MISSING+=("$*"); }

host="${1:-${VOIE_FABRIC_SSH:-baremetal-1-cs}}"
control_ssh="${VOIE_CONTROL_SSH:-}"

load_local_stack_env || true

ssh_ok() {
  ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" "$1" >/dev/null 2>&1
}

if ! ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" 'true' >/dev/null 2>&1; then
  note "operator SSH to Fabric host $host"
else
  ssh_ok 'test -w /dev/kvm' || note "writable KVM on $host"
  ssh_ok 'systemctl is-active --quiet k3s' || note "k3s on $host"
  ssh_ok 'systemctl is-active --quiet voie-fabricd' || note "voie-fabricd on $host"
  ssh_ok 'grep -Fq VOIE_FABRICD_STAGE_MODE=none /etc/voie/fabric.env' \
    || note "fabric.env STAGE_MODE=none on $host"
  ssh_ok 'grep -Fq VOIE_STORAGE_STAGING=0 /etc/voie/fabric.env' \
    || note "fabric.env STORAGE_STAGING=0 on $host"
  if ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
    'lvs voie-ws/stage >/dev/null 2>&1'; then
    note "retired staging LV voie-ws/stage on $host"
  fi
  if ssh_ok 'test -w /dev/kvm'; then
    handler="$(ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
      'k3s kubectl get runtimeclass voie-firecracker -o jsonpath={.handler}' 2>/dev/null || true)"
    [ "$handler" = "kata-fc-rs-voie" ] || note "RuntimeClass voie-firecracker / kata-fc-rs-voie on $host"
  fi
fi

if [ -n "$control_ssh" ]; then
  if ! ssh -o BatchMode=yes -o ConnectTimeout=8 "$control_ssh" 'true' >/dev/null 2>&1; then
    note "operator SSH to Control host $control_ssh"
  else
    ssh -o BatchMode=yes -o ConnectTimeout=8 "$control_ssh" 'ss -ltn | grep -q ":22 "' \
      >/dev/null 2>&1 || note "Control TCP/22 is not listening"
  fi
fi

origin="${VOIE_PUBLIC_ORIGIN:-${VOIE_CONTROL_URL:-}}"
if [ -n "$origin" ]; then
  origin="${origin%/}"
  code="$(curl -sk -o /dev/null -w '%{http_code}' --connect-timeout 5 "$origin/healthz" || true)"
  [ "$code" = "200" ] || note "Control /healthz at $origin (HTTP ${code:-none})"
  ready="$(curl -sk -o /dev/null -w '%{http_code}' --connect-timeout 5 "$origin/readyz" || true)"
  [ "$ready" = "200" ] || note "Control /readyz at $origin (HTTP ${ready:-none})"
else
  note "VOIE_PUBLIC_ORIGIN or VOIE_CONTROL_URL for Control HTTPS"
fi

[ -n "${VOIE_FABRIC_ENDPOINT:-}" ] || note "VOIE_FABRIC_ENDPOINT"
[ -n "${VOIE_FABRIC_CA_CERT_PATH:-}" ] || note "VOIE_FABRIC_CA_CERT_PATH"
[ -n "${VOIE_FABRIC_CLIENT_CERT_PATH:-}" ] || note "VOIE_FABRIC_CLIENT_CERT_PATH"
[ -n "${VOIE_FABRIC_CLIENT_KEY_PATH:-}" ] || note "VOIE_FABRIC_CLIENT_KEY_PATH"

if [ -n "${VOIE_FABRIC_ENDPOINT:-}" ] && [ -n "${VOIE_FABRIC_CA_CERT_PATH:-}" ] \
  && [ -n "${VOIE_FABRIC_CLIENT_CERT_PATH:-}" ] && [ -n "${VOIE_FABRIC_CLIENT_KEY_PATH:-}" ]; then
  mtls="$(curl -sk -o /dev/null -w '%{http_code}' --connect-timeout 5 \
    --cacert "$VOIE_FABRIC_CA_CERT_PATH" \
    --cert "$VOIE_FABRIC_CLIENT_CERT_PATH" \
    --key "$VOIE_FABRIC_CLIENT_KEY_PATH" \
    "${VOIE_FABRIC_ENDPOINT%/}/v1/health" || true)"
  [ "$mtls" = "200" ] || note "Fabric mTLS ${VOIE_FABRIC_ENDPOINT}/v1/health (HTTP ${mtls:-none})"
fi

[ -n "${VOIE_DATABASE_URL:-}" ] || note "VOIE_DATABASE_URL"
[ -n "${VOIE_MODEL_BASE_URL:-}" ] || note "VOIE_MODEL_BASE_URL"
[ -n "${VOIE_MODEL_NAME:-}" ] || note "VOIE_MODEL_NAME"
if [ -z "${VOIE_MODEL_API_KEY:-}" ] && [ -z "${VOIE_MODEL_API_KEY_FILE:-}" ]; then
  note "model credentials (VOIE_MODEL_API_KEY or VOIE_MODEL_API_KEY_FILE)"
fi

if ! command -v tofu >/dev/null 2>&1 && ! command -v terraform >/dev/null 2>&1; then
  note "OpenTofu/terraform on PATH"
fi
if [ -z "${ARM_CLIENT_ID:-}" ] && [ -z "${AZURE_CLIENT_ID:-}" ]; then
  note "Azure credentials (ARM_CLIENT_ID or AZURE_CLIENT_ID)"
fi

if [ -n "${VOIE_FABRIC_ENDPOINT:-}" ]; then
  :
fi

if [ "${#MISSING[@]}" -eq 0 ]; then
  printf 'READY\n'
  exit 0
fi

printf 'live-doctor missing prerequisites:\n' >&2
printf '  %s\n' "${MISSING[@]}" >&2
exit 2
