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
#   3. in local mode, the host canary stayed clean: VOIE_ACTIVATION_PATH
#      shadows bash with a recording shim, and nothing wrote through it —
#      Workspace exec must never fall back to the control host. Origin mode
#      does not own the deployed control's PATH, so it does not install a
#      shim; the marker in canonical events is the Fabric-exec proof.
#
# Two honest modes (same contract as native-c6 / live-c3):
#   VOIE_CONTROL_URL set .... drive the already-deployed control. Never
#                             spawn a second writer against live PostgreSQL.
#   otherwise ............... owns the process lifecycle: builds voie-cloud,
#                             spawns it over REAL PostgreSQL/Blob/mTLS-
#                             Fabric/model with the ephemeral OIDC issuer.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
# shellcheck source=tests/live/common.sh
source "${ROOT}/tests/live/common.sh"

refuse_fixture_model live-c4

MODE="origin"
if ! live_origin_mode; then
  MODE="local"
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
  command -v node >/dev/null || edge "Node (activation and OIDC issuer child)"
fi

require_env VOIE_FABRIC_ENDPOINT VOIE_FABRIC_CA_CERT_PATH \
  VOIE_FABRIC_CLIENT_CERT_PATH VOIE_FABRIC_CLIENT_KEY_PATH >/dev/null || {
  printf '  (live-c4 needs product mTLS to open a scratch Workspace)\n' >&2
  exit 2
}
command -v curl >/dev/null || edge "curl"
command -v python3 >/dev/null || edge "python3"

if [ "$MODE" = "origin" ]; then
  bootstrap_admin_env_ready || {
    printf '  (live-c4 origin logs in as the bootstrap admin: username + 0600 password file)\n' >&2
    exit 2
  }
fi

RUNTIME="${XDG_RUNTIME_DIR:-/tmp}/voie-live-c4"
rm -rf "$RUNTIME"
install -d -m 700 "$RUNTIME"

CLOUD_PID=""
ISSUER_PID=""
CANARY_DIR=""
stop_cloud() {
  if [ "$MODE" = "origin" ]; then
    return 0
  fi
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
  if [ -n "${WORKSPACE_ID:-}" ] && [ "${PRODUCT_WORKSPACE:-}" != "1" ]; then
    scratch_workspace_close
  fi
}
trap cleanup EXIT

if [ "$MODE" = "local" ]; then
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

  node "${ROOT}/dev-stack/oidc-issuer.mjs" "$ISSUER_PORT" >"${RUNTIME}/oidc.log" 2>&1 &
  ISSUER_PID=$!
  await_issuer_ready "${RUNTIME}/oidc.log"

  cargo build -p voie-cloud --locked || edge "cargo build -p voie-cloud"
  await_cloud_ready "${RUNTIME}/cloud.log"
  export VOIE_CONTROL_URL="$ORIGIN"
else
  ORIGIN="${VOIE_CONTROL_URL%/}"
  export VOIE_PUBLIC_ORIGIN="${VOIE_PUBLIC_ORIGIN:-$ORIGIN}"
  export VOIE_CONTROL_URL="$ORIGIN"
fi

JAR="${RUNTIME}/cookies.txt"
if [ "$MODE" = "origin" ]; then
  bootstrap_admin_login "$ORIGIN" "$JAR"
else
  oidc_login_boot "$ORIGIN" "$JAR"
fi

OUT="${RUNTIME}/body.json"
product_workspace_open "$JAR" "$OUT" "live-c4"
SESSION_ID="$(rest_provision_session "$JAR" "$OUT")"

MARKER="c4-exec-ok-$(date +%Y%m%dT%H%M%S)-$$"
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

if [ "$MODE" = "local" ] && [ -f "${CANARY_DIR}/executed" ]; then
  fail "host-local bash ran; Workspace exec must not fall back to the control host"
fi

WORKSPACE_ID=""

echo "live-c4 pass: run ${RUN_ID} terminal through Fabric exec; events carry ${MARKER}; host canary clean"
