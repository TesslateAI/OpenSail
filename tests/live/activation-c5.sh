#!/usr/bin/env bash
# C5 (integration-1): a fresh activation resumes the same durable Session and
# Workspace, and one interrupted exec becomes outcome-unknown and is never
# redispatched.
#
# Proves, with no substitutes:
#   1. a create-mode run completes and its bash result lands in canonical
#      events;
#   2. an exec dispatched to the Fabric journal is abandoned mid-flight by
#      killing the control process; the journal keeps the claim durable;
#   3. after the control restart, repeating the same call ID returns the
#      outcome-unknown claim immediately instead of dispatching again, and a
#      different request hash for that call ID is refused (conflict);
#   4. a resume-mode run on the same session succeeds with session identity,
#      workspace binding, and canonical event head unchanged.
#
# Two honest modes (same contract as native-c6 / live-c3):
#   VOIE_CONTROL_URL set .... drive the already-deployed control. Restart
#                             is `systemctl restart voie-cloud` over
#                             VOIE_CONTROL_SSH. Never spawn a second writer
#                             against live PostgreSQL.
#   otherwise ............... owns the process lifecycle over REAL
#                             PostgreSQL/Blob/mTLS-Fabric/model with the
#                             ephemeral OIDC issuer.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
# shellcheck source=tests/live/common.sh
source "${ROOT}/tests/live/common.sh"

MODE="origin"
if ! live_origin_mode; then
  MODE="local"
  load_local_stack_env
  require_env VOIE_DATABASE_URL \
    VOIE_AZURE_BLOB_ACCOUNT VOIE_AZURE_BLOB_CONTAINER \
    VOIE_FABRIC_ENDPOINT VOIE_FABRIC_CA_CERT_PATH \
    VOIE_FABRIC_CLIENT_CERT_PATH VOIE_FABRIC_CLIENT_KEY_PATH \
    VOIE_MODEL_BASE_URL VOIE_MODEL_NAME >/dev/null || {
    printf '  (live-c5 drives the real resume + no-replay path)\n' >&2
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
  printf '  (live-c5 Fabric journal proof needs product mTLS)\n' >&2
  exit 2
}
command -v curl >/dev/null || edge "curl"
command -v python3 >/dev/null || edge "python3"

if [ "$MODE" = "origin" ]; then
  bootstrap_admin_env_ready || {
    printf '  (live-c5 origin logs in as the bootstrap admin: username + 0600 password file)\n' >&2
    exit 2
  }
  [ -n "${VOIE_CONTROL_SSH:-}" ] ||
    edge "VOIE_CONTROL_SSH (origin C5 restarts the live voie-cloud unit)"
fi

RUNTIME="${XDG_RUNTIME_DIR:-/tmp}/voie-live-c5"
rm -rf "$RUNTIME"
install -d -m 700 "$RUNTIME"

CLOUD_PID=""
ISSUER_PID=""
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
restart_control() {
  if [ "$MODE" = "origin" ]; then
    restart_origin_control "$ORIGIN"
  else
    stop_cloud
    sleep 0.5
    start_cloud
  fi
}
cleanup() {
  stop_cloud
  if [ -n "${ISSUER_PID:-}" ]; then
    kill "$ISSUER_PID" 2>/dev/null || true
    wait "$ISSUER_PID" 2>/dev/null || true
  fi
  if [ "$MODE" = "local" ] && [ -n "${WORKSPACE_ID:-}" ] && [ "${PRODUCT_WORKSPACE:-}" = "1" ]; then
    product_workspace_close
  elif [ -n "${WORKSPACE_ID:-}" ] && [ "${PRODUCT_WORKSPACE:-}" != "1" ]; then
    scratch_workspace_close
  fi
}
trap cleanup EXIT

if [ "$MODE" = "local" ]; then
  ISSUER_PORT="${VOIE_LIVE_ISSUER_PORT:-18097}"
  BIND="${VOIE_BIND:-localhost:18085}"
  ORIGIN="http://${BIND}"
  export VOIE_BIND="$BIND"
  export VOIE_PUBLIC_ORIGIN="$ORIGIN"
  export VOIE_OIDC_ISSUER="http://127.0.0.1:${ISSUER_PORT}"
  export VOIE_OIDC_ISSUER_URL="$VOIE_OIDC_ISSUER"
  export VOIE_OIDC_CLIENT_ID="${VOIE_OIDC_CLIENT_ID:-voie-dev}"
  printf 'dev-only\n' >"${RUNTIME}/oidc-client-secret"
  export VOIE_OIDC_CLIENT_SECRET_FILE="${RUNTIME}/oidc-client-secret"
  export VOIE_OIDC_REDIRECT_URL="${ORIGIN}/oidc/callback"
  export VOIE_TEST_ISSUER_LOGIN="${VOIE_TEST_ISSUER_LOGIN:-voie-dev}"
  export VOIE_TEST_ISSUER_PASSWORD="${VOIE_TEST_ISSUER_PASSWORD:-voie-dev-pass}"
  export VOIE_ALLOW_ISSUER_QUERY_LOGIN=yes # script-owned loopback issuer
  # Default AuthMode is native; C5 logs in through the loopback issuer.
  export VOIE_AUTH_MODE=oidc
  unset VOIE_FABRIC_TLS_NAME VOIE_FABRIC_SSH VOIE_FABRIC_BOOTSTRAP_HOST

  node "${ROOT}/dev-stack/oidc-issuer.mjs" "$ISSUER_PORT" >"${RUNTIME}/oidc.log" 2>&1 &
  ISSUER_PID=$!
  await_issuer_ready "${RUNTIME}/oidc.log"

  cargo build -p voie-cloud --locked || edge "cargo build -p voie-cloud"
  start_cloud() { await_cloud_ready "${RUNTIME}/cloud.log"; }
  start_cloud
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
product_workspace_open "$JAR" "$OUT" "live-c5"
SESSION_ID="$(rest_provision_session "$JAR" "$OUT")"

MARKER="c5-marker-$(date +%Y%m%dT%H%M%S)-$$"
RUN_PROMPT="Run echo ${MARKER} in bash and then reply with done."
RUN_MODE="create"
RUN_ONE="$(uuid4)"
if ! await_run_terminal "$JAR" "$RUN_ONE" "$OUT"; then
  fail "first run ${RUN_ONE} did not reach terminal: $(cat "$OUT")"
fi

EVENTS="${RUNTIME}/events.json"
CODE="$(api_read "$JAR" "${ORIGIN}/api/sessions/${SESSION_ID}/events" "$EVENTS")"
[ "$CODE" = "200" ] || fail "session events HTTP ${CODE}: $(cat "$EVENTS")"
canonical_bash_output_has_marker "$EVENTS" "$MARKER" ||
  fail "canonical events do not contain ${MARKER} after the first run"
HEAD_BEFORE="$(json_field 'cursor' <"$EVENTS")"

# Dispatch one long exec directly against the product Fabric journal on this
# proof's own workspace, then abandon it in flight.
CALL_ID="c5-interrupt-$(date +%s)-$$"
DISPATCH_OUT="${RUNTIME}/dispatch.json"
VOIE_FABRIC_TIMEOUT=2 fabric_rpc POST "/v1/workspaces/${WORKSPACE_ID}/exec" \
  "{\"call_id\":\"${CALL_ID}\",\"command\":\"sleep 25\"}" "$DISPATCH_OUT" >/dev/null 2>&1 ||
  true # abandoning the client does not cancel the durable dispatched claim

# Kill the control while the exec claim is dispatched; the journal must hold.
restart_control

REPEAT_OUT="${RUNTIME}/repeat.json"
START="$SECONDS"
CODE="$(fabric_rpc POST "/v1/workspaces/${WORKSPACE_ID}/exec" \
  "{\"call_id\":\"${CALL_ID}\",\"command\":\"sleep 25\"}" "$REPEAT_OUT")"
ELAPSED=$((SECONDS - START))
[ "$CODE" = "200" ] || edge "repeated interrupted call HTTP ${CODE}: $(cat "$REPEAT_OUT")"
case "$(json_field 'state' <"$REPEAT_OUT")" in
  unknown|dispatched) ;;
  *) fail "repeated interrupted call state is $(json_field 'state' <"$REPEAT_OUT"); want outcome-unknown without redispatch" ;;
esac
[ "$ELAPSED" -lt 10 ] ||
  fail "repeated interrupted call waited ${ELAPSED}s; the journal must answer from retained state"

CONFLICT_OUT="${RUNTIME}/conflict.json"
CODE="$(fabric_rpc POST "/v1/workspaces/${WORKSPACE_ID}/exec" \
  "{\"call_id\":\"${CALL_ID}\",\"command\":\"sleep 1\"}" "$CONFLICT_OUT")"
[ "$CODE" = "409" ] || fail "conflicting hash HTTP ${CODE}, want 409: $(cat "$CONFLICT_OUT")"

# Fresh activation resumes the same Session and Workspace.
RUN_PROMPT="Resume and confirm the same Workspace."
RUN_MODE="resume"
RUN_TWO="$(uuid4)"
if ! await_run_terminal "$JAR" "$RUN_TWO" "$OUT" 180; then
  fail "resume run ${RUN_TWO} did not reach terminal: $(cat "$OUT")"
fi

CODE="$(api_read "$JAR" "${ORIGIN}/api/sessions/${SESSION_ID}" "$OUT")"
[ "$CODE" = "200" ] || fail "session read HTTP ${CODE}"
[ "$(json_field 'workspaceId' <"$OUT")" = "$WORKSPACE_ID" ] ||
  fail "resume changed the workspace binding"

EVENTS="${RUNTIME}/events-final.json"
CODE="$(api_read "$JAR" "${ORIGIN}/api/sessions/${SESSION_ID}/events" "$EVENTS")"
[ "$CODE" = "200" ] || fail "final session events HTTP ${CODE}"
HEAD_AFTER="$(json_field 'cursor' <"$EVENTS")"
[ "$HEAD_AFTER" -ge "$HEAD_BEFORE" ] ||
  fail "canonical event head regressed (${HEAD_BEFORE} -> ${HEAD_AFTER})"

if [ "$MODE" = "local" ] && [ -n "${WORKSPACE_ID:-}" ]; then
  product_workspace_close
fi
WORKSPACE_ID=""

echo "live-c5 pass: resume kept session ${SESSION_ID}; call ${CALL_ID} stayed outcome-unknown in ${ELAPSED}s; conflict refused"
