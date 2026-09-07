#!/usr/bin/env bash
# C6 (integration-1): the native console path performs bootstrap-admin login
# -> Personal scope -> product Workspace -> first chat message -> visible
# tool/answer -> durable follow-up -> refresh/reconstruct, through native
# REST only.
#
# Auth-path contract: this is the default acceptance path for every estate.
# The legacy OIDC login path is kept only when the deployment opted in via
# var.oidc_provision=true (tofu r0) — i.e. VOIE_OIDC_* rendered in
# control.env and VOIE_AUTH_MODE=oidc|both — and is then exercised by
# tests/live/oauth-c6.sh (just live-c6-oauth), never by this script.
# Two honest modes:
#   VOIE_CONTROL_URL set .... drive a deployed HTTPS control directly. The
#                             bootstrap admin credential pair must be
#                             supplied (VOIE_BOOTSTRAP_ADMIN_USERNAME +
#                             VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE, a 0600 file;
#                             VOIE_NATIVE_ADMIN_* is accepted for
#                             compatibility). The control seeds the admin at
#                             first boot; this script only logs in.
#   otherwise ............... spawn one local control over REAL
#                             PostgreSQL/Blob/mTLS-Fabric/model boundaries
#                             with VOIE_AUTH_MODE=native and a script-owned
#                             bootstrap admin, then do the same.
#
# Proves, with no substitutes:
#   1. the console entry serves HTML and gates every API behind the Web
#      session (401 before login);
#   2. the native bootstrap-admin login mints the voie_session cookie
#      (303, no OIDC round trip) without persisting any cookie artifact;
#   3. /api/me and the Personal scope (kind=personal) resolve;
#   4. one dedicated `native-c6` Workspace is reused (created through the
#      product API when absent, verified ready in the same Personal scope
#      when present);
#   5. the first chat message (POST /api/conversations) executes remote Bash
#      and its bash result becomes visible in canonical events;
#   6. a durable follow-up (POST /api/conversations/{id}/messages) queues a
#      resume run on the same conversation and settles;
#   7. refresh/reconstruct reads the same Session and its event head with
#      monotonic cursors, and stale cursors are discarded server-side.
#
# The password never rides in argv, logs, or files: it is read from the
# credential file, trimmed of exactly one trailing newline (mirroring the
# control's seed), staged in a 0600 file, and submitted via
# --data-urlencode @file. Cookie jars live in a 0700 runtime dir removed on
# exit. No secret value is printed.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
# shellcheck source=tests/live/common.sh
source "${ROOT}/tests/live/common.sh"

refuse_fixture_model live-c6

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
    printf '  (live-c6 drives the real console chain end to end)\n' >&2
    exit 2
  }
  if [ -z "${VOIE_AZURE_BLOB_KEY:-}" ] && [ -z "${VOIE_AZURE_BLOB_KEY_FILE:-}" ]; then
    edge "Azure Blob credential (VOIE_AZURE_BLOB_KEY or VOIE_AZURE_BLOB_KEY_FILE)"
  fi
  if [ -z "${VOIE_MODEL_API_KEY:-}" ] && [ -z "${VOIE_MODEL_API_KEY_FILE:-}" ]; then
    edge "model provider credential (VOIE_MODEL_API_KEY or VOIE_MODEL_API_KEY_FILE)"
  fi
  command -v cargo >/dev/null || edge "Rust toolchain (cargo)"
  [ -f "${ROOT}/activation/dist/index.js" ] ||
    edge "built activation entry (activation/dist/index.js); run just activation-dist"
fi

# Local mode seeds its own script-owned bootstrap admin below; only origin
# mode (a deployed control that already seeded from its credential file)
# requires the caller-supplied credential pair.
if [ "$MODE" = "origin" ]; then
  bootstrap_admin_env_ready || {
    printf '  (live-c6 logs in as the bootstrap admin: username + 0600 password file)\n' >&2
    exit 2
  }
fi

RUNTIME="${XDG_RUNTIME_DIR:-/tmp}/voie-live-c6"
rm -rf "$RUNTIME"
install -d -m 700 "$RUNTIME"

CLOUD_PID=""
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
  rm -rf "$RUNTIME"
}
trap cleanup EXIT

if [ "$MODE" = "local" ]; then
  BIND="${VOIE_BIND:-localhost:18086}"
  ORIGIN="http://${BIND}"
  export VOIE_BIND="$BIND"
  export VOIE_PUBLIC_ORIGIN="${VOIE_PUBLIC_ORIGIN:-$ORIGIN}"
  # Native-only control: the ephemeral OIDC issuer is not spawned. The
  # bootstrap admin is seeded at startup from the script-owned pair.
  export VOIE_AUTH_MODE=native
  unset VOIE_FABRIC_TLS_NAME VOIE_FABRIC_SSH VOIE_FABRIC_BOOTSTRAP_HOST
  # Shared stack PostgreSQL already has platform admin `voie` from the first
  # local native seed. bootstrap_native_admin is a no-op once any admin
  # exists, so a second username never gets credentials. Log in as `voie`
  # with the same password the original local seed used.
  export VOIE_BOOTSTRAP_ADMIN_USERNAME="${VOIE_BOOTSTRAP_ADMIN_USERNAME:-voie}"
  printf 'voie\n' >"${RUNTIME}/bootstrap-admin-password"
  chmod 600 "${RUNTIME}/bootstrap-admin-password"
  export VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE="${RUNTIME}/bootstrap-admin-password"

  CANARY_DIR="${RUNTIME}/canary"
  install_host_canary "$CANARY_DIR"
  export VOIE_ACTIVATION_PATH="${CANARY_DIR}:/usr/bin:/bin"

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
# Exercise the served artifact itself: the built console references a
# bundled JS asset; that exact asset must be served by the same origin.
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

# Native bootstrap-admin Web-session boot. The jar stays inside RUNTIME and
# is removed on exit; no cookie artifact is persisted anywhere.
JAR="${RUNTIME}/cookies.txt"
bootstrap_admin_login "$ORIGIN" "$JAR"

CODE="$(api_read "$JAR" "${ORIGIN}/api/me" "$OUT")"
[ "$CODE" = "200" ] || fail "/api/me after login HTTP ${CODE}: $(cat "$OUT")"
# userId is the stable identity claim; username/displayName/platformRole are
# newer profile fields still landing on the wire contract — assert them only
# when present, never fail on absence.
python3 - "$OUT" <<'PY' || fail "/api/me profile fields malformed: $(cat "$OUT")"
import json, sys
me = json.load(open(sys.argv[1], encoding="utf-8"))
assert str(me.get("userId", "")).strip(), "userId missing"
for name in ("username", "displayName", "platformRole"):
    if name in me:
        assert isinstance(me[name], str) and me[name].strip(), f"{name} present but empty"
print("me:", ",".join(k for k in ("username", "displayName", "platformRole") if k in me) or "core-only")
PY

# Personal scope: the bootstrap admin owns exactly one kind=personal scope.
PROJECT_ID="$(resolve_personal_scope "$JAR" "$OUT")"
[ -n "$PROJECT_ID" ] || fail "personal scope resolution returned no id"

# Agent contract: the integrated product treats agentId on
# POST /api/conversations as OPTIONAL, so the default create omits it.
# C6_AGENT_ID=<uuid> pins an explicit agent reference (also exercised below);
# C6_AGENT_ID= (empty) keeps the omission strict — no legacy retry.
if [ -z "${C6_AGENT_ID+x}" ] || [ -z "${C6_AGENT_ID}" ]; then
  AGENT_ID=""
else
  AGENT_ID="${C6_AGENT_ID}"
fi

# One dedicated acceptance Workspace for this Personal scope. Creating a
# fresh Workspace every run would consume the per-scope quota and make C6
# non-repeatable; the conversation still pins the Workspace (DELETE 409),
# so the same labeled ready Workspace is reused instead.
C6_WORKSPACE_LABEL="native-c6"
CODE="$(api_read "$JAR" "${ORIGIN}/api/projects/${PROJECT_ID}/workspaces" "$OUT")"
[ "$CODE" = "200" ] || fail "scope workspaces list HTTP ${CODE}: $(cat "$OUT")"
set +e
WORKSPACE_ID="$(python3 - "$OUT" "$PROJECT_ID" "$C6_WORKSPACE_LABEL" <<'PY'
import json, sys
path, scope_id, label = sys.argv[1], sys.argv[2], sys.argv[3]
data = json.load(open(path, encoding="utf-8"))
items = data.get("items") or []
found = None
for item in items:
    if item.get("label") != label:
        continue
    found = item
    break
if found is None:
    raise SystemExit(0)
wid = str(found.get("id") or "").strip()
scope = str(found.get("scopeId") or found.get("projectId") or "").strip()
state = str(found.get("state") or "").strip()
if not wid:
    sys.stderr.write("native-c6 workspace row has no id\n")
    raise SystemExit(2)
if scope != scope_id:
    sys.stderr.write(f"native-c6 workspace {wid} scope {scope} != {scope_id}\n")
    raise SystemExit(2)
if state != "ready":
    sys.stderr.write(f"native-c6 workspace {wid} state is {state}, want ready\n")
    raise SystemExit(2)
print(wid)
PY
)"
lookup_rc=$?
set -e
if [ "$lookup_rc" -eq 2 ]; then
  fail "dedicated native-c6 workspace is present but not reusable: $(cat "$OUT")"
fi
[ "$lookup_rc" -eq 0 ] || fail "dedicated native-c6 workspace lookup failed"
PROBE="${RUNTIME}/workspace-probe.json"
if [ -n "$WORKSPACE_ID" ] && ! product_workspace_has_volume "$WORKSPACE_ID" "$PROBE"; then
  printf '  (dedicated native-c6 %s has no Fabric volume; recreating)\n' "$WORKSPACE_ID" >&2
  product_workspace_close "$JAR" "$PROJECT_ID" "$WORKSPACE_ID"
  WORKSPACE_ID=""
fi
if [ -z "$WORKSPACE_ID" ]; then
  LIST_OUT="${RUNTIME}/workspaces.json"
  cp "$OUT" "$LIST_OUT"
  WORKSPACE_ID="$(uuid4)"
  CODE="$(api_mutate "$JAR" POST "${ORIGIN}/api/projects/${PROJECT_ID}/workspaces" \
    "{\"id\":\"${WORKSPACE_ID}\",\"label\":\"${C6_WORKSPACE_LABEL}\"}" "$OUT")"
  if [ "$CODE" = "429" ]; then
    # Quota is full: reuse only an already acceptance-owned ready Workspace.
    # Never PATCH/relabel an arbitrary product Workspace.
    WORKSPACE_ID="$(python3 - "$LIST_OUT" "$PROJECT_ID" "$C6_WORKSPACE_LABEL" <<'PY'
import json, sys
path, scope_id, dedicated = sys.argv[1], sys.argv[2], sys.argv[3]
data = json.load(open(path, encoding="utf-8"))
smoke = None
for item in data.get("items") or []:
    scope = str(item.get("scopeId") or item.get("projectId") or "").strip()
    state = str(item.get("state") or "").strip()
    wid = str(item.get("id") or "").strip()
    label = str(item.get("label") or "")
    if scope != scope_id or state != "ready" or not wid:
        continue
    if label == dedicated:
        print(wid)
        raise SystemExit(0)
    if smoke is None and label.startswith("Smoke"):
        smoke = wid
if smoke:
    print(smoke)
    raise SystemExit(0)
raise SystemExit(1)
PY
)" || fail "workspace quota reached and no acceptance-owned ready workspace (label ${C6_WORKSPACE_LABEL} or Smoke*) to reuse"
    CODE="$(api_read "$JAR" "${ORIGIN}/api/workspaces/${WORKSPACE_ID}" "$OUT")"
    [ "$CODE" = "200" ] || fail "quota-reuse workspace detail HTTP ${CODE}: $(cat "$OUT")"
    [ "$(json_field 'state' <"$OUT")" = "ready" ] ||
      fail "quota-reuse workspace is not ready: $(cat "$OUT")"
    printf '  (reusing acceptance workspace %s after quota; label left unchanged)\n' "$WORKSPACE_ID" >&2
  else
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
  fi
else
  CODE="$(api_read "$JAR" "${ORIGIN}/api/workspaces/${WORKSPACE_ID}" "$OUT")"
  [ "$CODE" = "200" ] || fail "reuse workspace detail HTTP ${CODE}: $(cat "$OUT")"
  [ "$(json_field 'id' <"$OUT")" = "$WORKSPACE_ID" ] ||
    fail "workspace detail returned a different id: $(cat "$OUT")"
  [ "$(json_field 'state' <"$OUT")" = "ready" ] ||
    fail "reused workspace is not ready: $(cat "$OUT")"
  DETAIL_SCOPE="$(json_field 'scopeId' <"$OUT" 2>/dev/null || json_field 'projectId' <"$OUT")"
  [ "$DETAIL_SCOPE" = "$PROJECT_ID" ] ||
    fail "reused workspace scope ${DETAIL_SCOPE} != personal ${PROJECT_ID}"
  printf '  (reusing dedicated native-c6 workspace %s)\n' "$WORKSPACE_ID" >&2
fi

# After C7 restarts fabricd, guest exec can lag Ready. Wait for a real
# /workspace mount before the first conversation; a completed Run with no
# bash result is not a C6 pass.
if ! await_workspace_mounted "$WORKSPACE_ID"; then
  fail "native-c6 workspace ${WORKSPACE_ID} guest is not mounted for exec"
fi

# First chat: durable empty Session, then the first prompt as Run #1.
# Control mints the Session id; a client-supplied conversationId is ignored.
MARKER="c6-exec-ok-$(date +%Y%m%dT%H%M%S)-$$"
FIRST_INTENT="$(uuid4)"
open_payload() {
  if [ -n "$AGENT_ID" ]; then
    printf '{"projectId":"%s","agentId":"%s","workspaceId":"%s"}' \
      "$PROJECT_ID" "$AGENT_ID" "$WORKSPACE_ID"
  else
    printf '{"projectId":"%s","workspaceId":"%s"}' \
      "$PROJECT_ID" "$WORKSPACE_ID"
  fi
}
CODE="$(api_mutate "$JAR" POST "${ORIGIN}/api/conversations" "$(open_payload)" "$OUT")"
if [ "$CODE" = "400" ] && [ -z "${C6_AGENT_ID+x}" ]; then
  AGENT_ID="$(provision_agent "$JAR" "$PROJECT_ID" "$OUT")"
  printf 'native-c6: control requires agentId (pre-optional contract); retried with provisioned agent\n' >&2
  CODE="$(api_mutate "$JAR" POST "${ORIGIN}/api/conversations" "$(open_payload)" "$OUT")"
fi
[ "$CODE" = "200" ] || fail "conversation create HTTP ${CODE}: $(cat "$OUT")"
SESSION_ID="$(json_field 'conversationId' <"$OUT")"
[ -n "$SESSION_ID" ] || fail "conversation create returned no conversationId: $(cat "$OUT")"
CODE="$(api_read "$JAR" "${ORIGIN}/api/conversations" "$OUT")"
[ "$CODE" = "200" ] || fail "conversation list HTTP ${CODE}: $(cat "$OUT")"
python3 - "$OUT" "$SESSION_ID" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
want = sys.argv[2]
ids = [str(item.get("id") or "") for item in data.get("items") or []]
if want not in ids:
    raise SystemExit(f"empty Session {want} missing from list: {ids}")
PY
CODE="$(api_mutate "$JAR" POST "${ORIGIN}/api/conversations/${SESSION_ID}/messages" \
  "$(printf '{"intentId":"%s","prompt":"Run echo %s in bash and then reply with done."}' "$FIRST_INTENT" "$MARKER")" \
  "$OUT")"
[ "$CODE" = "200" ] || fail "conversation message HTTP ${CODE}: $(cat "$OUT")"
FIRST_RUN="$(json_field 'runId' <"$OUT")"
[ -n "$FIRST_RUN" ] || fail "conversation message returned no runId: $(cat "$OUT")"

if ! await_run_resource "$JAR" "$FIRST_RUN" "$OUT"; then
  fail "first run ${FIRST_RUN} did not reach terminal: $(cat "$OUT")"
fi

EVENTS="${RUNTIME}/events.json"
if ! await_canonical_marker "$JAR" "$SESSION_ID" "$MARKER" "$EVENTS"; then
  fail "canonical events carry no bash result with ${MARKER}: the first chat message -> tool -> answer path failed"
fi
CURSOR="$(json_field 'cursor' <"$EVENTS")"
[ -n "$CURSOR" ] || fail "canonical events returned no cursor"

# Durable follow-up on the same conversation: always a resume-mode run that
# queues behind its predecessor and settles.
FOLLOWUP="c6-followup-ok-$(date +%Y%m%dT%H%M%S)-$$"
FOLLOW_INTENT="$(uuid4)"
CODE="$(api_mutate "$JAR" POST "${ORIGIN}/api/conversations/${SESSION_ID}/messages" \
  "{\"intentId\":\"${FOLLOW_INTENT}\",\"prompt\":\"Reply with the exact text: ${FOLLOWUP}\"}" "$OUT")"
[ "$CODE" = "200" ] || fail "conversation follow-up HTTP ${CODE}: $(cat "$OUT")"
FOLLOW_RUN="$(json_field 'runId' <"$OUT")"
[ -n "$FOLLOW_RUN" ] || fail "conversation follow-up returned no runId: $(cat "$OUT")"

if ! await_run_resource "$JAR" "$FOLLOW_RUN" "$OUT"; then
  fail "follow-up run ${FOLLOW_RUN} did not reach terminal: $(cat "$OUT")"
fi

# Refresh/reconstruct the same chat: session identity is unchanged and the
# event head advanced past the first cursor; the follow-up prompt is visible
# in the canonical stream.
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

# Cleanup: tear the Workspace down through the product API. The product
# refuses (409) while the conversation still references it; that guard is a
# durable contract, so a 409 with the sessions reason is a pass and the
# dedicated Workspace stays reusable for the next C6 run. Anything else is
# a real failure.
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

echo "live-c6 pass: bootstrap-admin login, personal scope ${PROJECT_ID}, product workspace ${WORKSPACE_ID}, runs ${FIRST_RUN}/${FOLLOW_RUN} terminal, events carry ${MARKER}, cursor monotonic"
