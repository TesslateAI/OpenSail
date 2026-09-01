#!/usr/bin/env bash
# P1-C4: agent publishes the exact preview Release; prod artifact hash equals
# preview hash; unhealthy candidate receives no production traffic; rollback
# restores the previous Release.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=tests/live/p1-common.sh
source "${ROOT}/tests/live/p1-common.sh"

host="${1:-baremetal-1-cs}"
p1_require_fabric_host "$host"
p1_require_guest_images "$host"
p1_require_control
p1_require_model
require_env VOIE_CONSOLE_HOST >/dev/null || edge "console host for *.prod wildcard publication"

CANARY="${TMPDIR:-/tmp}/voie-p1-c4-canary"
install_host_canary "$CANARY"
export PATH="$CANARY:$PATH"

p1_boot_session
p1_ready_unbound_workspace
SLUG="p1c4$(python3 -c 'import uuid; print(uuid.uuid4().hex[:8])')"
p1_agent_create_and_test "$SLUG" "P1 C4 tracker" postgres
p1_require_workspace_guest_image
p1_guest_test
p1_agent_create_databases
[ "$DEV_DB_ID" != "$PROD_DB_ID" ] || fail "dev and prod databases must be distinct"
p1_agent_build_release
FIRST_RELEASE_ID="$RELEASE_ID"
PREVIEW_HASH="$RELEASE_HASH"

p1_agent_deploy_dev
p1_wait_healthy "$DEPLOYMENT_ID"
p1_activate_healthy "$DEPLOYMENT_ID"

p1_agent_publish_prod
FIRST_PROD_DEPLOY="$DEPLOYMENT_ID"

state="$(p1_deployment_state "$FIRST_PROD_DEPLOY")"
if [ "$state" != "healthy" ] && [ "$state" != "active" ]; then
  act="$(api_mutate "$JAR" POST "${ORIGIN}/api/deployments/${FIRST_PROD_DEPLOY}/activate" "{}" "$OUT")"
  [ "$act" = "409" ] || fail "activate of ${state} Deployment returned HTTP ${act}, expected 409"
fi
p1_wait_healthy "$FIRST_PROD_DEPLOY"
p1_activate_healthy "$FIRST_PROD_DEPLOY"

code="$(api_read "$JAR" "${ORIGIN}/api/applications/${APPLICATION_ID}/releases" "$OUT")"
[ "$code" = "200" ] || fail "releases list HTTP ${code}"
prod_hash="$(python3 - "$OUT" "$FIRST_RELEASE_ID" <<'PY'
import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
want=sys.argv[2]
for item in data.get("items") or []:
    if item.get("id")==want:
        print(item.get("artifactHash") or "")
        break
PY
)"
[ "$prod_hash" = "$PREVIEW_HASH" ] || fail "prod artifact hash ${prod_hash} != preview ${PREVIEW_HASH}"
p1_wait_public_body "https://${PROD_HOST}/" "tracker"

# Workspace mutation without a new Release must not change production.
mutate_idle="$(python3 -c 'import json; print("python3 -c " + json.dumps("open(\"/workspace/marker.txt\",\"w\").write(\"mutated-prod\\n\")"))')"
call_id="$(uuid4)"
payload="$(python3 -c 'import json,sys; print(json.dumps({"call_id":sys.argv[1],"command":sys.argv[2]}))' "$call_id" "$mutate_idle")"
fabric_rpc POST "/v1/workspaces/${WORKSPACE_ID}/exec" "$payload" "$OUT" >/dev/null
code="$(api_read "$JAR" "${ORIGIN}/api/applications/${APPLICATION_ID}/releases" "$OUT")"
[ "$code" = "200" ] || fail "releases re-list after workspace mutation HTTP ${code}"
still="$(python3 - "$OUT" "$FIRST_RELEASE_ID" <<'PY'
import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
want=sys.argv[2]
for item in data.get("items") or []:
    if item.get("id")==want:
        print(item.get("artifactHash") or "")
        break
PY
)"
[ "$still" = "$PREVIEW_HASH" ] || fail "Workspace mutation changed the published Release hash"
p1_wait_public_body "https://${PROD_HOST}/" "tracker"

# Second Release, then a candidate that must not take traffic until healthy.
# marker.txt is packed into the artifact; voie.toml must change too so the
# Release request hash (generation + manifest bytes) is a new pack identity.
mutate_cmd="$(python3 -c 'import json; print("python3 -c " + json.dumps("open(\"/workspace/marker.txt\",\"w\").write(\"candidate\\n\"); p=\"/workspace/voie.toml\"; t=open(p).read(); open(p,\"w\").write(t if t.endswith(\"# candidate\\n\") else t.rstrip()+\"\\n# candidate\\n\")"))')"
call_id="$(uuid4)"
payload="$(python3 -c 'import json,sys; print(json.dumps({"call_id":sys.argv[1],"command":sys.argv[2]}))' "$call_id" "$mutate_cmd")"
fabric_rpc POST "/v1/workspaces/${WORKSPACE_ID}/exec" "$payload" "$OUT" >/dev/null
p1_agent_build_release
SECOND_RELEASE_ID="$RELEASE_ID"
[ "$SECOND_RELEASE_ID" != "$FIRST_RELEASE_ID" ] || fail "second pack reused the first Release"
# Candidate is created over HTTP so this proof owns activate. The agent path
# already published the first Release; an in-run activate would hide 409.
p1_deploy "$PROD_ENV_ID"
code="$P1_HTTP_CODE"
if [ "$code" = "409" ]; then
  approval_id="$(p1_json_field "$OUT" approvalId)"
  p1_accept_approval "$approval_id"
  p1_deploy "$PROD_ENV_ID" "$approval_id"
  code="$P1_HTTP_CODE"
fi
[ "$code" = "202" ] || [ "$code" = "200" ] || fail "second prod candidate HTTP ${code}: $(cat "$OUT")"
SECOND_PROD_DEPLOY="$DEPLOYMENT_ID"
[ -n "$SECOND_PROD_DEPLOY" ] || fail "second prod candidate missing deployment id"
[ "$SECOND_PROD_DEPLOY" != "$FIRST_PROD_DEPLOY" ] || fail "second prod reused the first Deployment"
# Candidate Pod exists but must not own the Environment Service yet.
# A single 404 during realize is not cutover; serving "candidate" is.
prod_code="000"
prod_body=""
kept=0
for _ in $(seq 1 45); do
  prod_code="$(curl -sS -o "${RUNTIME}/prod-candidate.body" -w '%{http_code}' --max-time 20 \
    "https://${PROD_HOST}/" || true)"
  prod_body="$(p1_strip_body < "${RUNTIME}/prod-candidate.body" 2>/dev/null || true)"
  case "$prod_code" in
    200)
      [ "$prod_body" = "candidate" ] && fail "unhealthy candidate served production traffic"
      if [ "$prod_body" = "tracker" ]; then
        kept=1
        break
      fi
      ;;
    000) edge "wildcard prod edge for ${PROD_HOST}" ;;
  esac
  sleep 2
done
[ "$kept" = "1" ] || fail "production did not keep previous tracker while candidate was unhealthy HTTP ${prod_code} body '${prod_body}'"
state="$(p1_deployment_state "$SECOND_PROD_DEPLOY")"
if [ "$state" != "healthy" ] && [ "$state" != "active" ]; then
  act="$(api_mutate "$JAR" POST "${ORIGIN}/api/deployments/${SECOND_PROD_DEPLOY}/activate" "{}" "$OUT")"
  [ "$act" = "409" ] || fail "unhealthy candidate activate HTTP ${act}, expected 409"
fi
p1_wait_healthy "$SECOND_PROD_DEPLOY"
p1_activate_healthy "$SECOND_PROD_DEPLOY"
p1_wait_public_body "https://${PROD_HOST}/" "candidate"

p1_rollback "$SECOND_PROD_DEPLOY"
code="$P1_HTTP_CODE"
if [ "$code" = "409" ]; then
  approval_id="$(p1_json_field "$OUT" approvalId)"
  p1_accept_approval "$approval_id"
  p1_rollback "$SECOND_PROD_DEPLOY" "$approval_id"
  code="$P1_HTTP_CODE"
fi
[ "$code" = "202" ] || [ "$code" = "200" ] || fail "rollback HTTP ${code}: $(cat "$OUT")"
p1_wait_healthy "$DEPLOYMENT_ID"
p1_activate_healthy "$DEPLOYMENT_ID"
rolled="$(p1_json_field "$OUT" deployment releaseId)"
[ "$rolled" = "$FIRST_RELEASE_ID" ] || fail "rollback restored ${rolled}, want ${FIRST_RELEASE_ID}"
p1_wait_public_body "https://${PROD_HOST}/" "tracker"

p1_assert_canary_quiet "$CANARY"
printf 'p1-c4 exact hash %s; 409 until healthy; rollback restored Release %s\n' \
  "$PREVIEW_HASH" "$FIRST_RELEASE_ID"
