#!/usr/bin/env bash
# P1-C2: one Release is packed; private dev URL requires authentication;
# Workspace mutation does not change the active preview.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=tests/live/p1-common.sh
source "${ROOT}/tests/live/p1-common.sh"

host="${1:-baremetal-1-cs}"
p1_require_fabric_host "$host"
p1_require_guest_images "$host"
p1_require_control
p1_require_model
require_env VOIE_CONSOLE_HOST >/dev/null || edge "console host for *.dev wildcard preview"

CANARY="${TMPDIR:-/tmp}/voie-p1-c2-canary"
install_host_canary "$CANARY"
export PATH="$CANARY:$PATH"

p1_boot_session
p1_ready_unbound_workspace
SLUG="p1c2$(python3 -c 'import uuid; print(uuid.uuid4().hex[:8])')"
p1_agent_create_and_test "$SLUG" "P1 C2 tracker"
p1_require_workspace_guest_image
p1_agent_build_release
[ -n "$RELEASE_HASH" ] || fail "Release artifact hash from live pack"
p1_agent_deploy_dev
p1_wait_healthy "$DEPLOYMENT_ID"
p1_activate_healthy "$DEPLOYMENT_ID"

# Unauthenticated preview must not serve the Application.
preview_code="$(curl -sS -o "${RUNTIME}/preview.body" -w '%{http_code}' --max-time 20 \
  -H "Host: ${DEV_HOST}" "https://${DEV_HOST}/" || true)"
case "$preview_code" in
  401|403|302|307|308) ;;
  000) edge "wildcard preview edge for ${DEV_HOST}" ;;
  *) fail "private preview without auth HTTP ${preview_code}, want 401/403/redirect" ;;
esac
p1_authenticated_preview "$DEV_HOST" "$DEV_ENV_ID" "tracker"

# Workspace mutation after pack must not change the packed preview or hash.
mutate_cmd="$(python3 -c 'import json; print("python3 -c " + json.dumps("open(\"/workspace/marker.txt\",\"w\").write(\"mutated\\n\")"))')"
call_id="$(uuid4)"
payload="$(python3 -c 'import json,sys; print(json.dumps({"call_id":sys.argv[1],"command":sys.argv[2]}))' "$call_id" "$mutate_cmd")"
fabric_rpc POST "/v1/workspaces/${WORKSPACE_ID}/exec" "$payload" "$OUT" >/dev/null
code="$(api_read "$JAR" "${ORIGIN}/api/applications/${APPLICATION_ID}/releases" "$OUT")"
[ "$code" = "200" ] || fail "releases re-list HTTP ${code}"
still="$(python3 - "$OUT" "$RELEASE_ID" <<'PY'
import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
want=sys.argv[2]
for item in data.get("items") or []:
    if item.get("id")==want:
        print(item.get("artifactHash") or "")
        break
PY
)"
[ "$still" = "$RELEASE_HASH" ] || fail "Workspace mutation changed the packed Release hash"
p1_authenticated_preview "$DEV_HOST" "$DEV_ENV_ID" "tracker"
p1_assert_canary_quiet "$CANARY"
printf 'p1-c2 Release %s hash %s; private preview refused unauthenticated access\n' "$RELEASE_ID" "$RELEASE_HASH"
