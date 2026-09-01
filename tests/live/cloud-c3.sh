#!/usr/bin/env bash
# C3 (integration-1): one real exec result through PostgreSQL control rows,
# Blob-backed canonical event bytes, and the product mTLS Fabric journal.
#
# Proves, with no substitutes:
#   1. a run's bash result lands in canonical events (Blob bytes) and is still
#      served byte-identically after the control process is killed and
#      restarted (PostgreSQL references + immutable Blob objects survive);
#   2. a repeated terminal call ID returns the retained result and is not
#      redispatched by the Fabric exec journal;
#   3. reusing a call ID with different content is refused (conflict).
#
# Two honest modes (same contract as native-c6):
#   VOIE_CONTROL_URL set .... drive the already-deployed control. Restart
#                             is `systemctl restart voie-cloud` over
#                             VOIE_CONTROL_SSH. Never spawn a second writer
#                             against live PostgreSQL.
#   otherwise ............... this script owns the process lifecycle: it
#                             builds voie-cloud, spawns it against REAL
#                             PostgreSQL/Azure-Blob/mTLS-Fabric/model
#                             boundaries with the in-repo ephemeral OIDC
#                             issuer, restarts that process mid-proof, and
#                             tears everything down.
#
# Missing boundaries fail closed; nothing degrades to a mock.
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
    printf '  (live-c3 drives the real PostgreSQL + Blob + mTLS Fabric chain)\n' >&2
    exit 2
  }
  if [ -z "${VOIE_AZURE_BLOB_KEY:-}" ] && [ -z "${VOIE_AZURE_BLOB_KEY_FILE:-}" ]; then
    edge "Azure Blob credential (VOIE_AZURE_BLOB_KEY or VOIE_AZURE_BLOB_KEY_FILE)"
  fi
  if [ -z "${VOIE_MODEL_API_KEY:-}" ] && [ -z "${VOIE_MODEL_API_KEY_FILE:-}" ]; then
    edge "model provider credential (VOIE_MODEL_API_KEY or VOIE_MODEL_API_KEY_FILE)"
  fi
  command -v cargo >/dev/null || edge "Rust toolchain (cargo)"
  command -v node >/dev/null || edge "Node (ephemeral OIDC issuer child)"
fi

require_env VOIE_FABRIC_ENDPOINT VOIE_FABRIC_CA_CERT_PATH \
  VOIE_FABRIC_CLIENT_CERT_PATH VOIE_FABRIC_CLIENT_KEY_PATH >/dev/null || {
  printf '  (live-c3 Fabric journal proof needs product mTLS)\n' >&2
  exit 2
}
command -v curl >/dev/null || edge "curl"
command -v python3 >/dev/null || edge "python3"

if [ "$MODE" = "origin" ]; then
  bootstrap_admin_env_ready || {
    printf '  (live-c3 origin logs in as the bootstrap admin: username + 0600 password file)\n' >&2
    exit 2
  }
  [ -n "${VOIE_CONTROL_SSH:-}" ] ||
    edge "VOIE_CONTROL_SSH (origin C3 restarts the live voie-cloud unit)"
fi

RUNTIME="${XDG_RUNTIME_DIR:-/tmp}/voie-live-c3"
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
  if [ -n "${WORKSPACE_ID:-}" ] && [ "${PRODUCT_WORKSPACE:-}" != "1" ]; then
    scratch_workspace_close
  fi
}
trap cleanup EXIT

if [ "$MODE" = "local" ]; then
  ISSUER_PORT="${VOIE_LIVE_ISSUER_PORT:-18099}"
  BIND="${VOIE_BIND:-localhost:18087}"
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

  node "${ROOT}/dev-stack/oidc-issuer.mjs" "$ISSUER_PORT" >"${RUNTIME}/oidc.log" 2>&1 &
  ISSUER_PID=$!
  await_issuer_ready "${RUNTIME}/oidc.log"

  cargo build -p voie-cloud --locked || edge "cargo build -p voie-cloud"
  start_cloud() { await_cloud_ready "${RUNTIME}/cloud.log"; }
  export VOIE_CONTROL_URL="$ORIGIN"
  start_cloud
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
product_workspace_open "$JAR" "$OUT" "live-c3"
SESSION_ID="$(rest_provision_session "$JAR" "$OUT")"

MARKER="c3-event-$(date +%Y%m%dT%H%M%S)-$$"
RUN_PROMPT="Run echo ${MARKER} in bash and then reply with done."
RUN_MODE="create"
RUN_ID="$(uuid4)"
if ! await_run_terminal "$JAR" "$RUN_ID" "$OUT"; then
  fail "run ${RUN_ID} did not reach terminal: $(cat "$OUT")"
fi

EVENTS_BEFORE="${RUNTIME}/events-before.json"
CODE="$(api_read "$JAR" "${ORIGIN}/api/sessions/${SESSION_ID}/events" "$EVENTS_BEFORE")"
[ "$CODE" = "200" ] || fail "session events HTTP ${CODE}: $(cat "$EVENTS_BEFORE")"
canonical_bash_output_has_marker "$EVENTS_BEFORE" "$MARKER" ||
  fail "canonical events do not contain the bash marker ${MARKER}"

# Fabric journal semantics on this proof's own scratch workspace.
EXEC_OUT="${RUNTIME}/exec.json"
CALL_ID="c3-call-$(date +%s)-$$"
CODE="$(fabric_rpc POST "/v1/workspaces/${WORKSPACE_ID}/exec" \
  "{\"call_id\":\"${CALL_ID}\",\"command\":\"echo c3-journal-ok\"}" "$EXEC_OUT")"
[ "$CODE" = "200" ] || edge "Fabric exec HTTP ${CODE}: $(cat "$EXEC_OUT")"
[ "$(json_field 'state' <"$EXEC_OUT")" = "terminal" ] || fail "first exec state: $(cat "$EXEC_OUT")"
[ "$(json_field 'stdout' <"$EXEC_OUT")" = "c3-journal-ok" ] || fail "first exec stdout: $(cat "$EXEC_OUT")"

REPEAT_OUT="${RUNTIME}/repeat.json"
CODE="$(fabric_rpc POST "/v1/workspaces/${WORKSPACE_ID}/exec" \
  "{\"call_id\":\"${CALL_ID}\",\"command\":\"echo c3-journal-ok\"}" "$REPEAT_OUT")"
[ "$CODE" = "200" ] || fail "repeated call ID HTTP ${CODE}: $(cat "$REPEAT_OUT")"
[ "$(json_field 'state' <"$REPEAT_OUT")" = "terminal" ] || fail "repeated call state: $(cat "$REPEAT_OUT")"
[ "$(json_field 'stdout' <"$REPEAT_OUT")" = "c3-journal-ok" ] ||
  fail "repeated call ID was redispatched (result changed): $(cat "$REPEAT_OUT")"

CONFLICT_OUT="${RUNTIME}/conflict.json"
CODE="$(fabric_rpc POST "/v1/workspaces/${WORKSPACE_ID}/exec" \
  "{\"call_id\":\"${CALL_ID}\",\"command\":\"echo c3-conflict\"}" "$CONFLICT_OUT")"
[ "$CODE" = "409" ] || fail "conflicting hash HTTP ${CODE}, want 409: $(cat "$CONFLICT_OUT")"

# Kill and restart the control process; everything durable must survive.
restart_control

CODE="$(api_read "$JAR" "${ORIGIN}/api/sessions/${SESSION_ID}" "$OUT")"
[ "$CODE" = "200" ] || fail "session row lost across control restart (HTTP ${CODE})"
[ "$(json_field 'workspaceId' <"$OUT")" = "$WORKSPACE_ID" ] ||
  fail "session workspace changed across control restart"

EVENTS_AFTER="${RUNTIME}/events-after.json"
CODE="$(api_read "$JAR" "${ORIGIN}/api/sessions/${SESSION_ID}/events" "$EVENTS_AFTER")"
[ "$CODE" = "200" ] || fail "session events after restart HTTP ${CODE}"
BEFORE_SEQ="$(json_field 'cursor' <"$EVENTS_BEFORE")"
AFTER_SEQ="$(json_field 'cursor' <"$EVENTS_AFTER")"
[ "$BEFORE_SEQ" = "$AFTER_SEQ" ] ||
  fail "canonical event head moved without writes (before ${BEFORE_SEQ}, after ${AFTER_SEQ})"
canonical_bash_output_has_marker "$EVENTS_AFTER" "$MARKER" ||
  fail "canonical events lost the bash marker across control restart"

WORKSPACE_ID=""

echo "live-c3 pass: events through control restart (head ${AFTER_SEQ}); call ${CALL_ID} terminal without redispatch; conflict refused"
