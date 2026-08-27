#!/usr/bin/env bash
# C4 (integration-1): a disposable activation performs model -> remote Bash
# through the control's Fabric client, and the host-local Bash fallback is
# refused.
#
# Proves, with no substitutes:
#   1. a create-mode run starts through POST /api/sessions/{id}/runs and
#      reaches terminal;
#   2. the activation child executed the Bash tool through product mTLS
#      Fabric: the canonical events carry a real bash result containing this
#      run's marker;
#   3. the host canary stayed clean: VOIE_ACTIVATION_PATH shadows bash with
#      a recording shim, and nothing wrote through it — Workspace exec must
#      never fall back to the control host.
#
# Owns its process lifecycle like live-c3: builds voie-cloud, spawns it over
# REAL PostgreSQL/Blob/mTLS-Fabric/model boundaries with the ephemeral OIDC
# issuer, and drives everything through native REST.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
# shellcheck source=tests/live/common.sh
source "${ROOT}/tests/live/common.sh"

refuse_fixture_model live-c4
require_env VOIE_DATABASE_URL \
  VOIE_AZURE_BLOB_ACCOUNT VOIE_AZURE_BLOB_CONTAINER \
  VOIE_FABRIC_ENDPOINT VOIE_FABRIC_CA_CERT_PATH \
  VOIE_FABRIC_CLIENT_CERT_PATH VOIE_FABRIC_CLIENT_KEY_PATH \
  VOIE_MODEL_BASE_URL VOIE_MODEL_NAME >/dev/null || {
  printf '  (live-c4 drives the real model -> mTLS Fabric exec path)\n' >&2
  exit 2
}
if [ -z "${VOIE_AZURE_BLOB_KEY:-}" ] && [ -z "${VOIE_AZURE_BLOB_KEY_FILE:-}" ]; then
  edge "Azure Blob credential (VOIE_AZURE_BLOB_KEY or VOIE_AZURE_BLOB_KEY_FILE)"
fi
if [ -z "${VOIE_MODEL_API_KEY:-}" ] && [ -z "${VOIE_MODEL_API_KEY_FILE:-}" ]; then
  edge "model provider credential (VOIE_MODEL_API_KEY or VOIE_MODEL_API_KEY_FILE)"
fi
[ -f "${ROOT}/activation/dist/index.js" ] ||
  edge "built activation entry (activation/dist/index.js); run just activation-dist"

command -v cargo >/dev/null || edge "Rust toolchain (cargo)"
command -v curl >/dev/null || edge "curl"
command -v node >/dev/null || edge "Node (activation and OIDC issuer child)"
command -v python3 >/dev/null || edge "python3"

RUNTIME="${XDG_RUNTIME_DIR:-/tmp}/voie-live-c4"
rm -rf "$RUNTIME"
install -d -m 700 "$RUNTIME"

ISSUER_PORT="${VOIE_LIVE_ISSUER_PORT:-18098}"
BIND="${VOIE_BIND:-localhost:18084}"
ORIGIN="http://${BIND}"
export VOIE_BIND="$BIND"
export VOIE_PUBLIC_ORIGIN="$ORIGIN"
export VOIE_OIDC_ISSUER="http://localhost:${ISSUER_PORT}"
export VOIE_OIDC_ISSUER_URL="$VOIE_OIDC_ISSUER"
export VOIE_OIDC_CLIENT_ID="${VOIE_OIDC_CLIENT_ID:-voie-dev}"
printf 'dev-only\n' >"${RUNTIME}/oidc-client-secret"
export VOIE_OIDC_CLIENT_SECRET_FILE="${RUNTIME}/oidc-client-secret"
export VOIE_OIDC_REDIRECT_URL="${ORIGIN}/oidc/callback"
export VOIE_TEST_ISSUER_LOGIN="${VOIE_TEST_ISSUER_LOGIN:-voie-dev}"
export VOIE_TEST_ISSUER_PASSWORD="${VOIE_TEST_ISSUER_PASSWORD:-voie-dev-pass}"
export VOIE_ALLOW_ISSUER_QUERY_LOGIN=yes # script-owned loopback issuer

# Host-local canary: first PATH entry shadows bash with a recorder. The
# control exports VOIE_ACTIVATION_PATH to the activation child, so any
# host-side Bash execution betrays itself here.
CANARY_DIR="${RUNTIME}/canary"
install_host_canary "$CANARY_DIR"
export VOIE_ACTIVATION_PATH="${CANARY_DIR}:/usr/bin:/bin"

CLOUD_PID=""
stop_cloud() {
  if [ -n "$CLOUD_PID" ]; then
    kill "$CLOUD_PID" 2>/dev/null || true
    wait "$CLOUD_PID" 2>/dev/null || true
    CLOUD_PID=""
  fi
}
cleanup() {
  stop_cloud
  if [ -n "${ISSUER_PID:-}" ]; then
    kill "$ISSUER_PID" 2>/dev/null || true
    wait "$ISSUER_PID" 2>/dev/null || true
  fi
  [ -n "${WORKSPACE_ID:-}" ] && scratch_workspace_close
}
trap cleanup EXIT

node "${ROOT}/dev-stack/oidc-issuer.mjs" "$ISSUER_PORT" >"${RUNTIME}/oidc.log" 2>&1 &
ISSUER_PID=$!
await_issuer_ready "${RUNTIME}/oidc.log"

cargo build -p voie-cloud --locked || edge "cargo build -p voie-cloud"

await_cloud_ready "${RUNTIME}/cloud.log"

export VOIE_CONTROL_URL="$ORIGIN"
JAR="${RUNTIME}/cookies.txt"
oidc_login_boot "$ORIGIN" "$JAR"

OUT="${RUNTIME}/body.json"
scratch_workspace_open "$OUT"
SESSION_ID="$(rest_provision_session "$JAR" "$OUT")"

MARKER="c4-exec-ok-$(date +%s)-$$"
RUN_PROMPT="Run echo ${MARKER} in bash and then reply with done."
RUN_MODE="create"
RUN_ID="$(uuid4)"
if ! await_run_terminal "$JAR" "$RUN_ID" "$OUT"; then
  fail "run ${RUN_ID} did not reach terminal: $(cat "$OUT")"
fi

EVENTS="${RUNTIME}/events.json"
CODE="$(api_read "$JAR" "${ORIGIN}/api/sessions/${SESSION_ID}/events" "$EVENTS")"
[ "$CODE" = "200" ] || fail "session events HTTP ${CODE}: $(cat "$EVENTS")"
canonical_bash_output_has_marker "$EVENTS" "$MARKER" ||
  fail "canonical events carry no bash result with ${MARKER}: the model -> remote Bash path did not execute"

if [ -f "${CANARY_DIR}/executed" ]; then
  fail "host-local bash ran; Workspace exec must not fall back to the control host"
fi

scratch_workspace_close
WORKSPACE_ID=""

echo "live-c4 pass: run ${RUN_ID} terminal through Fabric exec; events carry ${MARKER}; host canary clean"
