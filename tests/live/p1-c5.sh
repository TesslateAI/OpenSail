#!/usr/bin/env bash
# P1-C5: unknown build, migration, or deployment effects are not replayed;
# deletion removes routes, Pods, Services, volumes, bindings, and Fabric
# journal rows.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=tests/live/p1-common.sh
source "${ROOT}/tests/live/p1-common.sh"

host="${1:-baremetal-1-cs}"
p1_require_fabric_host "$host"
p1_require_guest_images "$host"
p1_require_control
p1_require_model

CANARY="${TMPDIR:-/tmp}/voie-p1-c5-canary"
install_host_canary "$CANARY"
export PATH="$CANARY:$PATH"

p1_boot_session
p1_ready_unbound_workspace
SLUG="p1c5$(python3 -c 'import uuid; print(uuid.uuid4().hex[:8])')"
p1_agent_create_and_test "$SLUG" "P1 C5 tracker" postgres
p1_require_workspace_guest_image
p1_guest_test
p1_agent_create_dev_database
p1_agent_build_release
generation="$(p1_exec_generation)"
escaped="$(p1_read_guest_manifest)"
[ -n "${BUILD_INTENT_ID:-}" ] || fail "ready Release missing buildIntentId"

# Same build intent is not dispatched again.
code="$(api_mutate "$JAR" POST "${ORIGIN}/api/applications/${APPLICATION_ID}/releases" \
  "$(python3 -c 'import json,sys; print(json.dumps({"build_intent_id":sys.argv[1],"workspace_id":sys.argv[2],"source_exec_generation":int(sys.argv[3]),"manifest":json.loads(sys.argv[4])}))' "$BUILD_INTENT_ID" "$WORKSPACE_ID" "$generation" "$escaped")" \
  "$OUT")"
case "$code" in
  200|202) ;;
  409) ;;
  *) fail "replayed build intent HTTP ${code}: $(cat "$OUT")" ;;
esac
code="$(api_read "$JAR" "${ORIGIN}/api/applications/${APPLICATION_ID}/releases" "$OUT")"
count="$(python3 - "$OUT" "$BUILD_INTENT_ID" <<'PY'
import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
want=sys.argv[2]
print(sum(1 for item in (data.get("items") or []) if item.get("buildIntentId")==want))
PY
)"
[ "$count" = "1" ] || fail "build intent was replayed into ${count} Release rows"

# In-flight unknown: a second POST of a fresh intent must not start a second
# dispatch while the first is still dispatched.
fresh="$(uuid4)"
body="$(python3 -c 'import json,sys; print(json.dumps({"build_intent_id":sys.argv[1],"workspace_id":sys.argv[2],"source_exec_generation":int(sys.argv[3]),"manifest":json.loads(sys.argv[4])}))' "$fresh" "$WORKSPACE_ID" "$generation" "$escaped")"
first="$(api_mutate "$JAR" POST "${ORIGIN}/api/applications/${APPLICATION_ID}/releases" "$body" "$OUT")"
second="$(api_mutate "$JAR" POST "${ORIGIN}/api/applications/${APPLICATION_ID}/releases" "$body" "$OUT")"
if [ "$first" = "202" ] && [ "$second" != "409" ] && [ "$second" != "200" ]; then
  fail "fresh intent replay HTTP ${second}, want 409 unknown or 200 ready"
fi
if [ "$first" = "202" ] && [ "$second" = "202" ]; then
  fail "unknown build intent was dispatched a second time"
fi

p1_agent_deploy_dev
export RELEASE_ID DEPLOY_INTENT_ID
[ -n "${DEPLOY_INTENT_ID:-}" ] || fail "agent deploy missing deploymentIntentId"
replay="$(api_mutate "$JAR" POST "${ORIGIN}/api/environments/${DEV_ENV_ID}/deployments" \
  "$(python3 -c 'import json,os; print(json.dumps({"release_id":os.environ["RELEASE_ID"],"deployment_intent_id":os.environ["DEPLOY_INTENT_ID"]}))')" \
  "$OUT")"
case "$replay" in
  200|409) ;;
  202) fail "deployment intent was dispatched a second time" ;;
  *) fail "replayed deployment intent HTTP ${replay}: $(cat "$OUT")" ;;
esac

p1_wait_healthy "$DEPLOYMENT_ID"
p1_assert_migrate_not_replayed "$DEPLOYMENT_ID" "$DEV_ENV_ID" "$RELEASE_ID" "dev"
p1_activate_healthy "$DEPLOYMENT_ID"

code="$(api_read "$JAR" "${ORIGIN}/api/deployments/${DEPLOYMENT_ID}/logs" "$OUT")"
[ "$code" = "200" ] || fail "logs HTTP ${code}: $(cat "$OUT")"
p1_assert_no_secrets
code="$(api_read "$JAR" "${ORIGIN}/api/applications/${APPLICATION_ID}/metrics" "$OUT")"
[ "$code" = "200" ] || fail "metrics HTTP ${code}: $(cat "$OUT")"

if ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
  "k3s kubectl get networkpolicy -A -l io.voie/slug=${SLUG} -o yaml 2>/dev/null | grep -q ipBlock"; then
  fail "Application NetworkPolicy still carries ipBlock"
fi

p1_delete_application
dep_gone="000"
db_gone="000"
for _ in $(seq 1 45); do
  dep_gone="$(fabric_rpc GET "/v1/deployments/${DEPLOYMENT_ID}" "" "$OUT" || true)"
  db_gone="$(fabric_rpc GET "/v1/databases/${DEV_DB_ID}" "" "$OUT" || true)"
  [ "$dep_gone" = "404" ] && [ "$db_gone" = "404" ] && break
  sleep 2
done
[ "$dep_gone" = "404" ] || fail "Fabric journal still has deployment ${DEPLOYMENT_ID}: HTTP ${dep_gone} $(cat "$OUT")"
[ "$db_gone" = "404" ] || fail "Fabric journal still has database ${DEV_DB_ID}: HTTP ${db_gone} $(cat "$OUT")"
code="$(api_read "$JAR" "${ORIGIN}/api/environments/${DEV_ENV_ID}/secret-bindings" "$OUT")"
[ "$code" = "200" ] || fail "bindings list HTTP ${code}: $(cat "$OUT")"
bind_count="$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1])).get("items") or []))' "$OUT")"
[ "$bind_count" = "0" ] || fail "secret bindings remain after delete: ${bind_count}"
remain="1"
for _ in $(seq 1 45); do
  slug_remain="$(ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
    "k3s kubectl get pods,svc,pvc,secret,networkpolicy -A -l io.voie/slug=${SLUG} --no-headers 2>/dev/null | grep -v '^$' | wc -l" || true)"
  rel_remain="$(ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
    "k3s kubectl get pvc -A -l io.voie/release=${RELEASE_ID} --no-headers 2>/dev/null | grep -v '^$' | wc -l" || true)"
  db_remain="$(ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
    "k3s kubectl get pvc -A -l io.voie/database=${DEV_DB_ID} --no-headers 2>/dev/null | grep -v '^$' | wc -l" || true)"
  rel_pv="$(ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
    "k3s kubectl get pv -l io.voie/release=${RELEASE_ID} --no-headers 2>/dev/null | grep -v '^$' | wc -l" || true)"
  db_pv="$(ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
    "k3s kubectl get pv -l io.voie/database=${DEV_DB_ID} --no-headers 2>/dev/null | grep -v '^$' | wc -l" || true)"
  dep_sec="$(ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
    "k3s kubectl get secret -A -l io.voie/deployment=${DEPLOYMENT_ID} --no-headers 2>/dev/null | grep -v '^$' | wc -l" || true)"
  db_sec="$(ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
    "k3s kubectl get secret -A -l io.voie/database=${DEV_DB_ID} --no-headers 2>/dev/null | grep -v '^$' | wc -l" || true)"
  slug_remain="${slug_remain// /}"
  rel_remain="${rel_remain// /}"
  db_remain="${db_remain// /}"
  rel_pv="${rel_pv// /}"
  db_pv="${db_pv// /}"
  dep_sec="${dep_sec// /}"
  db_sec="${db_sec// /}"
  remain=$(( ${slug_remain:-0} + ${rel_remain:-0} + ${db_remain:-0} + ${rel_pv:-0} + ${db_pv:-0} + ${dep_sec:-0} + ${db_sec:-0} ))
  [ "${remain}" = "0" ] && break
  sleep 2
done
[ "${remain}" = "0" ] || fail "Application objects remain after delete: slug=${slug_remain:-?} release=${rel_remain:-?} database=${db_remain:-?} release-pv=${rel_pv:-?} database-pv=${db_pv:-?} deploy-secret=${dep_sec:-?} database-secret=${db_sec:-?}"
route_code="$(fabric_rpc GET "/v1/routes" "" "$OUT" || true)"
[ "$route_code" = "200" ] || fail "Fabric routes HTTP ${route_code}: $(cat "$OUT")"
python3 - "$OUT" "$SLUG" <<'PY' || fail "Fabric gateway still has a route for the deleted Application"
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
slug = sys.argv[2]
items = data.get("items") or []
left = [item for item in items if item.get("slug") == slug]
assert not left, left
PY

p1_assert_canary_quiet "$CANARY"
printf 'p1-c5 no-replay of intent %s; Application %s deleted (slug leftover=%s)\n' \
  "$BUILD_INTENT_ID" "$SLUG" "${remain:-0}"
