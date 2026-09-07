#!/usr/bin/env bash
# C6 (integration-1), OAuth variant: the console path performs the external
# identity-provider login -> Personal scope -> product Workspace -> first
# chat message -> visible tool/answer -> durable follow-up ->
# refresh/reconstruct, through native REST only. This is an OPT-IN variant:
# the default acceptance path is the native bootstrap admin
# (tests/live/native-c6.sh); OIDC must be enabled on the control
# (VOIE_AUTH_MODE=oidc or both) and provider credentials supplied.
#
# Two honest modes:
#   VOIE_CONTROL_URL set .... drive a deployed HTTPS control directly.
#   otherwise ............... spawn one local control over REAL
#                             PostgreSQL/Blob/mTLS-Fabric/model boundaries
#                             with the ephemeral OIDC issuer (loopback-only
#                             query login) and VOIE_AUTH_MODE=oidc.
#
# The OIDC flow mirrors the legacy browser boot: GET /login redirects to the
# issuer and the callback mints the voie_session cookie. Provider
# credentials may be passed via VOIE_SESSION_COOKIE (out-of-band) or
# VOIE_TEST_ISSUER_LOGIN/VOIE_TEST_ISSUER_PASSWORD for the script-owned
# loopback issuer only.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
# shellcheck source=tests/live/common.sh
source "${ROOT}/tests/live/common.sh"

refuse_fixture_model live-c6-oauth

command -v curl >/dev/null || edge "curl"
command -v python3 >/dev/null || edge "python3"

MODE="origin"
if [ -z "${VOIE_CONTROL_URL:-}" ]; then
  MODE="local"
  load_local_stack_env
  require_env VOIE_DATABASE_URL \
    VOIE_AZURE_BLOB_ACCOUNT VOIE_AZURE_BLOB_CONTAINER \
    VOIE_FABRIC_ENDPOINT VOIE_FABRIC_CA_CERT_PATH \
    VOIE_FABRIC_CLIENT_CERT_PATH VOIE_FABRIC_CLIENT_KEY_PATH \
    VOIE_MODEL_BASE_URL VOIE_MODEL_NAME >/dev/null || {
    printf '  (live-c6-oauth drives the real console chain end to end)\n' >&2
    exit 2
  }
  if [ -z "${VOIE_AZURE_BLOB_KEY:-}" ] && [ -z "${VOIE_AZURE_BLOB_KEY_FILE:-}" ]; then
    edge "Azure Blob credential (VOIE_AZURE_BLOB_KEY or VOIE_AZURE_BLOB_KEY_FILE)"
  fi
  if [ -z "${VOIE_MODEL_API_KEY:-}" ] && [ -z "${VOIE_MODEL_API_KEY_FILE:-}" ]; then
    edge "model provider credential (VOIE_MODEL_API_KEY or VOIE_MODEL_API_KEY_FILE)"
  fi
  command -v cargo >/dev/null || edge "Rust toolchain (cargo)"
  command -v node >/dev/null || edge "Node (activation and OIDC issuer child)"
  [ -f "${ROOT}/activation/dist/index.js" ] ||
    edge "built activation entry (activation/dist/index.js); run just activation-dist"
  WEB_ROOT="${VOIE_WEB_ROOT:-${ROOT}/web/dist}"
  export VOIE_WEB_ROOT="$WEB_ROOT"
  [ -f "${WEB_ROOT}/index.html" ] ||
    edge "built Web artifact (${WEB_ROOT}/index.html); run just web-smoke"
else
  if [ -z "${VOIE_SESSION_COOKIE:-}" ]; then
    require_env VOIE_TEST_ISSUER_LOGIN VOIE_TEST_ISSUER_PASSWORD >/dev/null || {
      printf '  (origin mode: set VOIE_SESSION_COOKIE or the test-issuer credentials)\n' >&2
      exit 2
    }
  fi
fi

RUNTIME="${XDG_RUNTIME_DIR:-/tmp}/voie-live-c6-oauth"
rm -rf "$RUNTIME"
install -d -m 700 "$RUNTIME"

CLOUD_PID=""
ISSUER_PID=""
CANARY_DIR=""
stop_cloud() {
  if [ -n "$CLOUD_PID" ]; then
    kill "$CLOUD_PID" 2>/dev/null || true
    wait "$CLOUD_PID" 2>/dev/null || true
    CLOUD_PID=""
  fi
}
cleanup() {
  stop_cloud
  if [ -n "$ISSUER_PID" ]; then
    kill "$ISSUER_PID" 2>/dev/null || true
    wait "$ISSUER_PID" 2>/dev/null || true
  fi
  rm -rf "$RUNTIME"
}
trap cleanup EXIT

if [ "$MODE" = "local" ]; then
  ISSUER_PORT="${VOIE_LIVE_ISSUER_PORT:-18096}"
  BIND="${VOIE_BIND:-localhost:18086}"
  ORIGIN="http://${BIND}"
  export VOIE_BIND="$BIND"
  export VOIE_PUBLIC_ORIGIN="${VOIE_PUBLIC_ORIGIN:-$ORIGIN}"
  export VOIE_AUTH_MODE="${VOIE_AUTH_MODE:-oidc}"
  export VOIE_OIDC_ISSUER="http://127.0.0.1:${ISSUER_PORT}"
  export VOIE_OIDC_ISSUER_URL="$VOIE_OIDC_ISSUER"
  export VOIE_OIDC_CLIENT_ID="${VOIE_OIDC_CLIENT_ID:-voie-dev}"
  printf 'dev-only\n' >"${RUNTIME}/oidc-client-secret"
  export VOIE_OIDC_CLIENT_SECRET_FILE="${RUNTIME}/oidc-client-secret"
  export VOIE_OIDC_REDIRECT_URL="${VOIE_PUBLIC_ORIGIN}/oidc/callback"
  export VOIE_TEST_ISSUER_LOGIN="${VOIE_TEST_ISSUER_LOGIN:-voie-dev}"
  export VOIE_TEST_ISSUER_PASSWORD="${VOIE_TEST_ISSUER_PASSWORD:-voie-dev-pass}"
  export VOIE_ALLOW_ISSUER_QUERY_LOGIN=yes # script-owned loopback issuer

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
  case "$VOIE_PUBLIC_ORIGIN" in
    http://*|https://*) ;;
    *) edge "public origin must be an http(s) URL (VOIE_PUBLIC_ORIGIN)" ;;
  esac
fi

OUT="${RUNTIME}/body.json"

# Console entry and API gate.
CODE="$(curl -sS -o "$OUT" -w '%{http_code}' "${ORIGIN}/")"
[ "$CODE" = "200" ] || fail "console entry HTTP ${CODE}: $(cat "$OUT")"
grep -qi '<html' "$OUT" || fail "console entry did not serve HTML"
ASSET="$(python3 -c 'import re,sys
html = open(sys.argv[1], encoding="utf-8").read()
match = re.search(r"src=\"(/assets/[^\"]+\.js)\"", html)
if not match:
    match = re.search(r"src=\"(assets/[^\"]+\.js)\"", html)
sys.stdout.write(match.group(1) if match else "")' "$OUT")"
case "$ASSET" in
  /*) ASSET_URL="${ORIGIN}${ASSET}" ;;
  "") edge "built console index.html references no bundled JS asset" ;;
  *) ASSET_URL="${ORIGIN}/${ASSET}" ;;
esac
CONTENT_TYPE="$(curl -sS -o /dev/null -w '%{content_type}' "${ASSET_URL}")"
CONTENT_TYPE="${CONTENT_TYPE%%;*}"
CODE="$(curl -sS -o /dev/null -w '%{http_code}' "${ASSET_URL}")"
[ "$CODE" = "200" ] || fail "bundled console asset ${ASSET} HTTP ${CODE}"
case "$CONTENT_TYPE" in
  javascript/*|application/javascript|text/javascript) ;;
  *) fail "bundled console asset served as ${CONTENT_TYPE}, not JavaScript" ;;
esac
CODE="$(curl -sS -o "$OUT" -w '%{http_code}' "${ORIGIN}/api/me")"
[ "$CODE" = "401" ] || fail "unauthenticated /api/me HTTP ${CODE}, want 401"

# Real Web-session boot through the provider.
JAR="${RUNTIME}/cookies.txt"
oidc_login_boot "$ORIGIN" "$JAR"

CODE="$(api_read "$JAR" "${ORIGIN}/api/me" "$OUT")"
[ "$CODE" = "200" ] || fail "/api/me after login HTTP ${CODE}: $(cat "$OUT")"
[ -n "$(json_field 'userId' <"$OUT" 2>/dev/null || true)" ] ||
  fail "/api/me returned no userId: $(cat "$OUT")"

# Personal scope: resolve (kind=personal) or create for the provider-linked
# user.
PROJECT_ID="$(resolve_personal_scope "$JAR" "$OUT")"
[ -n "$PROJECT_ID" ] || fail "personal scope resolution returned no id"

AGENT_ID="$(provision_agent "$JAR" "$PROJECT_ID" "$OUT")"

WORKSPACE_ID="$(uuid4)"
CODE="$(api_mutate "$JAR" POST "${ORIGIN}/api/projects/${PROJECT_ID}/workspaces" \
  "{\"id\":\"${WORKSPACE_ID}\"}" "$OUT")"
case "$CODE" in
  200 | 202) ;;
  *) fail "product workspace create HTTP ${CODE}: $(cat "$OUT")" ;;
esac
[ "$(json_field 'id' <"$OUT")" = "$WORKSPACE_ID" ] ||
  fail "workspace create returned a different id: $(cat "$OUT")"
if [ "$CODE" != "200" ] || [ "$(json_field 'state' <"$OUT")" != "ready" ]; then
  await_product_workspace_ready "$JAR" "$WORKSPACE_ID" "$OUT" ||
    fail "workspace create did not become ready: $(cat "$OUT")"
fi

# First chat message through the product conversation API.
SESSION_ID="$(uuid4)"
MARKER="c6-oauth-ok-$(date +%s)-$$"
FIRST_INTENT="$(uuid4)"
CODE="$(api_mutate "$JAR" POST "${ORIGIN}/api/conversations" \
  "{\"conversationId\":\"${SESSION_ID}\",\"projectId\":\"${PROJECT_ID}\",\"agentId\":\"${AGENT_ID}\",\"workspaceId\":\"${WORKSPACE_ID}\"}" "$OUT")"
[ "$CODE" = "200" ] || fail "conversation create HTTP ${CODE}: $(cat "$OUT")"
CODE="$(api_mutate "$JAR" POST "${ORIGIN}/api/conversations/${SESSION_ID}/messages" \
  "{\"intentId\":\"${FIRST_INTENT}\",\"prompt\":\"Run echo ${MARKER} in bash and then reply with done.\"}" "$OUT")"
[ "$CODE" = "200" ] || fail "conversation message HTTP ${CODE}: $(cat "$OUT")"
FIRST_RUN="$(json_field 'runId' <"$OUT")"
[ -n "$FIRST_RUN" ] || fail "conversation message returned no runId: $(cat "$OUT")"

if ! await_run_resource "$JAR" "$FIRST_RUN" "$OUT"; then
  fail "first run ${FIRST_RUN} did not reach terminal: $(cat "$OUT")"
fi

EVENTS="${RUNTIME}/events.json"
CODE="$(api_read "$JAR" "${ORIGIN}/api/sessions/${SESSION_ID}/events" "$EVENTS")"
[ "$CODE" = "200" ] || fail "session events HTTP ${CODE}: $(cat "$EVENTS")"
canonical_events_have_marker "$EVENTS" "$MARKER" ||
  fail "canonical events carry no bash result with ${MARKER}: the chat message -> tool -> answer path failed"
CURSOR="$(json_field 'cursor' <"$EVENTS")"
[ -n "$CURSOR" ] || fail "canonical events returned no cursor"

# Durable follow-up on the same conversation.
FOLLOWUP="c6-oauth-followup-$(date +%s)-$$"
FOLLOW_INTENT="$(uuid4)"
CODE="$(api_mutate "$JAR" POST "${ORIGIN}/api/conversations/${SESSION_ID}/messages" \
  "{\"intentId\":\"${FOLLOW_INTENT}\",\"prompt\":\"Reply with the exact text: ${FOLLOWUP}\"}" "$OUT")"
[ "$CODE" = "200" ] || fail "conversation follow-up HTTP ${CODE}: $(cat "$OUT")"
FOLLOW_RUN="$(json_field 'runId' <"$OUT")"
[ -n "$FOLLOW_RUN" ] || fail "conversation follow-up returned no runId: $(cat "$OUT")"

if ! await_run_resource "$JAR" "$FOLLOW_RUN" "$OUT"; then
  fail "follow-up run ${FOLLOW_RUN} did not reach terminal: $(cat "$OUT")"
fi

# Refresh/reconstruct the same chat.
CODE="$(api_read "$JAR" "${ORIGIN}/api/sessions/${SESSION_ID}" "$OUT")"
[ "$CODE" = "200" ] || fail "session read HTTP ${CODE}"
[ "$(json_field 'workspaceId' <"$OUT")" = "$WORKSPACE_ID" ] ||
  fail "conversation refresh changed the workspace binding"

EVENTS2="${RUNTIME}/events-follow.json"
CODE="$(api_read "$JAR" "${ORIGIN}/api/sessions/${SESSION_ID}/events?after=${CURSOR}" "$EVENTS2")"
[ "$CODE" = "200" ] || fail "events poll at cursor HTTP ${CODE}"
[ "$(json_field 'cursor' <"$EVENTS2")" -ge "$CURSOR" ] ||
  fail "event cursor regressed at head (${CURSOR})"
if ! python3 - "$EVENTS2" "$FOLLOWUP" <<'PY'
import base64, json, sys
path, followup = sys.argv[1], sys.argv[2]
data = json.load(open(path, encoding="utf-8"))
for item in data.get("items") or []:
    try:
        raw = base64.b64decode(item.get("bytes") or "")
    except Exception:
        continue
    for line in raw.decode("utf-8", "replace").split("\n"):
        line = line.strip()
        if not line:
            continue
        try:
            event = json.loads(line)
        except Exception:
            continue
        if event.get("type") != "user/message":
            continue
        payload = event.get("data") or {}
        blocks = payload.get("content") or ((payload.get("message") or {}).get("content") or [])
        for block in blocks:
            if isinstance(block, dict) and block.get("type") == "text" and followup in block.get("text", ""):
                sys.exit(0)
sys.exit(1)
PY
then
  fail "follow-up prompt is not visible in the reconstructed conversation"
fi
CODE="$(api_read "$JAR" "${ORIGIN}/api/events?cursor=stale-garbage" "$OUT")"
[ "$CODE" = "200" ] || fail "garbage cursor poll HTTP ${CODE}, want stale discard (200)"
[ "$(json_field 'after' <"$OUT")" = "0" ] ||
  fail "garbage cursor was not discarded to 0: $(cat "$OUT")"

if [ -n "$CANARY_DIR" ] && [ -f "${CANARY_DIR}/executed" ]; then
  fail "host-local bash ran; Workspace exec must not fall back to the control host"
fi

CODE="$(api_mutate "$JAR" DELETE "${ORIGIN}/api/projects/${PROJECT_ID}/workspaces/${WORKSPACE_ID}" "" "$OUT")"
case "$CODE" in
  200) ;;
  409)
    grep -q 'sessions' "$OUT" ||
      fail "workspace delete HTTP 409 without the session-reference guard: $(cat "$OUT")"
    printf '  (workspace %s retained: teardown is refused while its conversation references it)\n' "$WORKSPACE_ID" >&2
    ;;
  *) fail "workspace delete HTTP ${CODE}: $(cat "$OUT")" ;;
esac

echo "live-c6-oauth pass: provider login, personal scope ${PROJECT_ID}, product workspace ${WORKSPACE_ID}, runs ${FIRST_RUN}/${FOLLOW_RUN} terminal, events carry ${MARKER}, cursor monotonic"
