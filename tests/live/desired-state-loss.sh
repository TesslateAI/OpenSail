#!/usr/bin/env bash
# Disposable desired-state loss/recovery proof on the live estate.
# Never touches keep-list Workspace/Application identities.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=tests/live/p1-common.sh
source "${ROOT}/tests/live/p1-common.sh"

host="${1:-baremetal-1-cs}"
KEEP_WORKSPACES="0a6b8637-8c9a-42dd-bb5a-e5f899d86258 2f5abbb3-184f-4572-bba9-1c86481411d4 58492022-7cef-4172-95ff-3aa5b53ac43c"
KEEP_APPS="09ca064b-e8ca-49e2-b3bc-95ec9f89e0e5 92ae7c37-b093-4f2e-af5c-adf2f69d033d"

p1_require_fabric_host "$host"
p1_require_guest_images "$host"
p1_require_control

compact_id() { tr -d '-' <<<"$1"; }

refuse_keep_workspace() {
  local id="$1"
  case " $KEEP_WORKSPACES " in
    *" $id "*) fail "refusing keep-list Workspace ${id}" ;;
  esac
}

refuse_keep_app() {
  local id="$1"
  case " $KEEP_APPS " in
    *" $id "*) fail "refusing keep-list Application ${id}" ;;
  esac
}

wait_row() {
  local sql="$1" want="$2" seconds="${3:-90}"
  local i got
  for i in $(seq 1 "$seconds"); do
    got="$(control_sql "$sql" | tr -d '[:space:]')"
    if [ "$got" = "$want" ]; then
      return 0
    fi
    sleep 1
  done
  fail "timed out waiting for ${want} (last ${got:-empty}): ${sql}"
}

fabric_lv_hint() {
  local kind="$1" id="$2" path code probe
  probe="${RUNTIME}/drop-probe.json"
  case "$kind" in
    workspace) path="/v1/workspaces/${id}" ;;
    database) path="/v1/databases/${id}" ;;
    deployment) path="/v1/deployments/${id}" ;;
    *) return 0 ;;
  esac
  code="$(fabric_rpc GET "$path" "" "$probe" || true)"
  [ "$code" = "200" ] || return 0
  python3 - "$probe" <<'PY'
import json, os, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
device = str(data.get("device") or "")
pv = str(data.get("pv_name") or data.get("pvName") or "")
base = os.path.basename(device)
lv = ""
if base.startswith("voie-crypt-"):
    rest = base[len("voie-crypt-"):]
    lv = rest if rest.startswith("voie-") else f"voie-{rest}"
elif base:
    lv = base
print(lv)
print(pv)
PY
}

fabric_drop_lv() {
  local kind="$1" id="$2"
  local compact needle hint_lv="" hint_pv=""
  refuse_keep_workspace "$id"
  compact="$(compact_id "$id")"
  case "$kind" in
    workspace) needle="ws${compact}" ;;
    database) needle="pg${compact}" ;;
    deployment) needle="dep${compact}" ;;
    *) fail "unknown volume kind ${kind}" ;;
  esac
  local hints
  hints="$(fabric_lv_hint "$kind" "$id" || true)"
  hint_lv="$(printf '%s\n' "$hints" | sed -n '1p')"
  hint_pv="$(printf '%s\n' "$hints" | sed -n '2p')"
  ssh -o BatchMode=yes -o ConnectTimeout=12 "$host" "bash -s" <<EOS
set -euo pipefail
id=$(printf '%q' "$id")
kind=$(printf '%q' "$kind")
needle=$(printf '%q' "$needle")
compact=$(printf '%q' "$compact")
hint_lv=$(printf '%q' "$hint_lv")
hint_pv=$(printf '%q' "$hint_pv")
keep_a='0a6b86378c9a42ddbb5ae5f899d86258'
keep_b='2f5abbb3184f4572bba91c86481411d4'
keep_c='584920227cef417295ff3aa5b53ac43c'
if [ "\$compact" = "\$keep_a" ] || [ "\$compact" = "\$keep_b" ] || [ "\$compact" = "\$keep_c" ]; then
  echo "refusing keep-list LV" >&2
  exit 1
fi
label=""
case "\$kind" in
  workspace) label="io.voie/workspace=\${id}" ;;
  database) label="io.voie/database=\${id}" ;;
  deployment) label="io.voie/deployment=\${id}" ;;
esac
# Release the block device before lvremove. Desired-state Fabric will recreate
# the Pod while the LV still exists; Kubernetes holding the PV is not Lost.
k3s kubectl delete pod,pvc -n voie-workspace -l "\$label" --wait=true --timeout=90s || true
k3s kubectl delete pv -l "\$label" --wait=true --timeout=90s || true
k3s kubectl delete pv "voie-db-\${id}" "voie-dep-\${id}" "voie-ws-\${id}" --ignore-not-found --wait=true --timeout=60s || true
if [ -n "\$hint_pv" ]; then
  k3s kubectl delete pv "\$hint_pv" --ignore-not-found --wait=true --timeout=60s || true
fi
sleep 1
names="\$(lvs --noheadings -o lv_name voie-ws | awk '{print \$1}')"
match=""
for name in \$names; do
  case "\$name" in
    workspace|runtime|stage) continue ;;
    "\$hint_lv"|"\$needle"|voie-ws-\$id|voie-db-\$id|voie-dep-\$id|voie-rst-\$id) match="\$name" ;;
  esac
  case "\$name" in
    *\$id*|*\$compact*)
      case "\$name" in
        workspace|runtime|stage) ;;
        *) match="\$name" ;;
      esac
      ;;
  esac
done
if [ -z "\$match" ] && [ -n "\$hint_lv" ] && lvs "voie-ws/\${hint_lv}" >/dev/null 2>&1; then
  match="\$hint_lv"
fi
if [ -z "\$match" ]; then
  echo "no LV matched \${needle} / \${id}" >&2
  echo "\$names" >&2
  exit 1
fi
case "\$match" in
  workspace|runtime|stage) echo "refusing pool LV \$match" >&2; exit 1 ;;
esac
close_mappers() {
  local cand
  [ -n "\$match" ] || return 0
  for cand in \\
    "/dev/mapper/voie-crypt-\${match}" \\
    "/dev/mapper/voie-crypt-db-\${id}" \\
    "/dev/mapper/voie-crypt-pg\${compact}" \\
    "/dev/mapper/voie-crypt-ws-\${id}" \\
    "/dev/mapper/voie-crypt-ws\${compact}" \\
    "/dev/mapper/voie-crypt-dep-\${id}" \\
    "/dev/mapper/voie-crypt-dep\${compact}" \\
    "/dev/mapper/voie-crypt-rst-\${id}"; do
    if [ -e "\$cand" ]; then
      cryptsetup close "\$(basename "\$cand")" || true
    fi
  done
  if [ -n "\$hint_lv" ]; then
    cand="/dev/mapper/voie-crypt-\${hint_lv#voie-}"
    if [ -e "\$cand" ]; then
      cryptsetup close "\$(basename "\$cand")" || true
    fi
    cand="/dev/mapper/voie-crypt-\${hint_lv}"
    if [ -e "\$cand" ]; then
      cryptsetup close "\$(basename "\$cand")" || true
    fi
  fi
}
removed=0
attempt=0
while [ "\$attempt" -lt 20 ]; do
  attempt=\$((attempt + 1))
  k3s kubectl delete pod,pvc -n voie-workspace -l "\$label" --wait=true --timeout=20s >/dev/null 2>&1 || true
  k3s kubectl delete pv -l "\$label" --ignore-not-found --wait=true --timeout=20s >/dev/null 2>&1 || true
  close_mappers
  lvchange -an "voie-ws/\${match}" >/dev/null 2>&1 || true
  if lvremove -y "voie-ws/\${match}" >/dev/null 2>&1; then
    removed=1
    break
  fi
  sleep 1
done
if [ "\$removed" != 1 ]; then
  echo "LV still in use after releasing Pod/PV: voie-ws/\${match}" >&2
  lvs "voie-ws/\${match}" >&2 || true
  exit 1
fi
echo "removed voie-ws/\${match}"
if lvs "voie-ws/\${match}" >/dev/null 2>&1; then
  echo "LV still present after lvremove" >&2
  exit 1
fi
EOS
}

workspace_exec() {
  local command="$1" call
  call="$(uuid4)"
  local payload
  payload="$(python3 -c 'import json,sys; print(json.dumps({"call_id":sys.argv[1],"command":sys.argv[2]}))' "$call" "$command")"
  local code
  code="$(fabric_rpc POST "/v1/workspaces/${WORKSPACE_ID}/exec" "$payload" "$OUT")"
  [ "$code" = "200" ] || fail "workspace exec HTTP ${code}: $(cat "$OUT")"
  [ "$(p1_json_field "$OUT" exit_code 2>/dev/null || p1_json_field "$OUT" exitCode 2>/dev/null || echo x)" = "0" ] ||
    fail "workspace exec failed: $(cat "$OUT")"
}

snapshot_workspace() {
  local code
  code="$(api_mutate "$JAR" POST "${ORIGIN}/api/workspaces/${WORKSPACE_ID}/snapshots" '{}' "$OUT")"
  if [ "$code" = "200" ]; then
    p1_json_field "$OUT" snapshotId
    return 0
  fi
  printf 'snapshot REST HTTP %s; using workspace.snapshot tool\n' "$code" >&2
  p1_require_model
  p1_provision_agent
  local session intent run
  session="$(uuid4)"
  intent="$(uuid4)"
  SESSION_ID="$session"
  export SESSION_ID
  local payload
  payload="$(python3 - "$session" "$PROJECT_ID" "$AGENT_ID" "$WORKSPACE_ID" <<'PY'
import json, sys
session, project, agent, workspace = sys.argv[1:5]
print(json.dumps({
    "conversationId": session,
    "projectId": project,
    "agentId": agent,
    "workspaceId": workspace,
}))
PY
)"
  code="$(api_mutate "$JAR" POST "${ORIGIN}/api/conversations" "$payload" "$OUT")"
  [ "$code" = "200" ] || fail "snapshot conversation HTTP ${code}: $(cat "$OUT")"
  payload="$(python3 - "$intent" <<'PY'
import json, sys
print(json.dumps({
    "intentId": sys.argv[1],
    "prompt": (
        "Call workspace.snapshot once and stop. Do not write files, pack, "
        "deploy, or call another product tool. Never print credentials."
    ),
}))
PY
)"
  code="$(api_mutate "$JAR" POST "${ORIGIN}/api/conversations/${session}/messages" "$payload" "$OUT")"
  [ "$code" = "200" ] || fail "snapshot message HTTP ${code}: $(cat "$OUT")"
  run="$(p1_json_field "$OUT" runId)"
  await_run_resource "$JAR" "$run" "$OUT" 600 || fail "snapshot run ${run} did not finish"
  control_sql "select id::text from workspace_snapshots where workspace_id = '${WORKSPACE_ID}' order by created_at desc limit 1"
}

install_control_sql() {
  cat > /tmp/voie-control-sql.py <<'PY'
#!/usr/bin/env python3
import re, subprocess, sys
text = open("/etc/voie/control.env", encoding="utf-8").read()
match = re.search(r"^VOIE_DATABASE_URL_FILE=(.*)$", text, re.M)
if not match:
    raise SystemExit("VOIE_DATABASE_URL_FILE missing")
path = match.group(1).strip().strip("\"'")
dsn = open(path, encoding="utf-8").read().strip()
sql = sys.argv[1]
proc = subprocess.run(
    ["psql", dsn, "-At", "-F", "|", "-c", sql],
    check=False,
    capture_output=True,
    text=True,
)
sys.stdout.write(proc.stdout)
if proc.returncode != 0:
    sys.stderr.write(proc.stderr.replace(dsn, "***"))
    raise SystemExit(proc.returncode)
PY
  scp -o BatchMode=yes /tmp/voie-control-sql.py control:/tmp/voie-control-sql.py
  ssh -o BatchMode=yes control 'chmod 700 /tmp/voie-control-sql.py'
}

control_sql() {
  local sql="$1"
  ssh -o BatchMode=yes -o ConnectTimeout=12 control \
    "python3 /tmp/voie-control-sql.py $(printf '%q' "$sql")"
}

p1_boot_session
install_control_sql
p1_ready_unbound_workspace
refuse_keep_workspace "$WORKSPACE_ID"
export WORKSPACE_ID P1_FABRIC_HOST="$host"
code="$(api_read "$JAR" "${ORIGIN}/api/workspaces/${WORKSPACE_ID}" "$OUT")"
[ "$code" = "200" ] || fail "disposable workspace GET HTTP ${code}: $(cat "$OUT")"
code="$(api_mutate "$JAR" PATCH "${ORIGIN}/api/workspaces/${WORKSPACE_ID}" \
  '{"label":"live-ds-loss"}' "$OUT")"
[ "$code" = "200" ] || fail "relabel disposable workspace HTTP ${code}: $(cat "$OUT")"

SLUG="dsloss$(python3 -c 'import uuid; print(uuid.uuid4().hex[:8])')"
p1_create_application "$SLUG" "DS loss demo"
refuse_keep_app "$APPLICATION_ID"
p1_require_workspace_guest_image

MARKER="ds-loss-$(python3 -c 'import uuid; print(uuid.uuid4().hex)')"
workspace_exec "printf '%s' $(printf '%q' "$MARKER") > /workspace/voie-ds-loss.marker"
WS_DESIRED="$(control_sql "select desired_revision::text from workspaces where id = '${WORKSPACE_ID}'")"
SNAPSHOT_ID="$(snapshot_workspace | tr -d '[:space:]')"
[ -n "$SNAPSHOT_ID" ] || fail "workspace snapshot returned no id"

printf 'workspace snapshot %s at desired_revision %s\n' "$SNAPSHOT_ID" "$WS_DESIRED"
fabric_drop_lv workspace "$WORKSPACE_ID"
wait_row "select observed_state from workspaces where id = '${WORKSPACE_ID}'" "lost" 90
AFTER_DESIRED="$(control_sql "select desired_revision::text from workspaces where id = '${WORKSPACE_ID}'")"
[ "$AFTER_DESIRED" = "$WS_DESIRED" ] ||
  fail "Lost observation bumped desired_revision ${WS_DESIRED} -> ${AFTER_DESIRED}"
ERR="$(control_sql "select coalesce(last_error_code,'') from workspaces where id = '${WORKSPACE_ID}'")"
[ "$ERR" = "durable_volume_missing" ] || fail "workspace last_error_code is ${ERR}"
NEW_LV="$(ssh -o BatchMode=yes "$host" "lvs --noheadings -o lv_name voie-ws | awk '{print \$1}'" | grep -E "${WORKSPACE_ID}|$(compact_id "$WORKSPACE_ID")" || true)"
[ -z "$NEW_LV" ] || fail "empty Workspace LV was reminted: ${NEW_LV}"

code="$(api_mutate "$JAR" POST "${ORIGIN}/api/workspaces/${WORKSPACE_ID}/restores" \
  "$(python3 -c 'import json,sys; print(json.dumps({"snapshotId":sys.argv[1]}))' "$SNAPSHOT_ID")" \
  "$OUT")"
[ "$code" = "200" ] || fail "workspace restore HTTP ${code}: $(cat "$OUT")"
wait_row "select observed_state from workspaces where id = '${WORKSPACE_ID}'" "ready" 180
await_workspace_mounted "$WORKSPACE_ID" "$OUT" || fail "restored workspace is not mounted"
workspace_exec "cat /workspace/voie-ds-loss.marker"
GOT="$(p1_json_field "$OUT" stdout 2>/dev/null || true)"
[ "$GOT" = "$MARKER" ] || fail "workspace marker after restore is '${GOT}', want ${MARKER}"
printf 'workspace Lost restore recovered marker %s\n' "$MARKER"

p1_guest_write_tracker postgres
p1_guest_test
DEV_DB_ID="$(p1_create_database "$DEV_ENV_ID")"
[ -n "$DEV_DB_ID" ] || fail "database create returned no id"
p1_wait_database_ready "$DEV_DB_ID"
DB_ROW="ds-db-$(python3 -c 'import uuid; print(uuid.uuid4().hex[:16])')"
p1_pg_at "$DEV_DB_ID" "CREATE TABLE IF NOT EXISTS voie_ds_loss(id int PRIMARY KEY, note text); INSERT INTO voie_ds_loss VALUES (1, '${DB_ROW}') ON CONFLICT (id) DO UPDATE SET note = EXCLUDED.note;" >/dev/null
[ "$(p1_pg_at "$DEV_DB_ID" "SELECT note FROM voie_ds_loss WHERE id=1" | tr -d '[:space:]')" = "$DB_ROW" ] ||
  fail "failed to write database marker"
BACKUP_ID="$(p1_backup_database "$DEV_DB_ID")"
[ -n "$BACKUP_ID" ] || fail "database backup returned no id"
DB_DESIRED="$(control_sql "select desired_revision::text from application_databases where id = '${DEV_DB_ID}'")"
fabric_drop_lv database "$DEV_DB_ID"
wait_row "select observed_state from application_databases where id = '${DEV_DB_ID}'" "lost" 90
AFTER_DB_DESIRED="$(control_sql "select desired_revision::text from application_databases where id = '${DEV_DB_ID}'")"
[ "$AFTER_DB_DESIRED" = "$DB_DESIRED" ] ||
  fail "Database Lost observation bumped desired_revision ${DB_DESIRED} -> ${AFTER_DB_DESIRED}"
ERR="$(control_sql "select coalesce(last_error_code,'') from application_databases where id = '${DEV_DB_ID}'")"
[ "$ERR" = "durable_volume_missing" ] || fail "database last_error_code is ${ERR}"
NEW_LV="$(ssh -o BatchMode=yes "$host" "lvs --noheadings -o lv_name voie-ws | awk '{print \$1}'" | grep -E "pg$(compact_id "$DEV_DB_ID")|voie-db-${DEV_DB_ID}" || true)"
[ -z "$NEW_LV" ] || fail "empty Database LV was reminted: ${NEW_LV}"
p1_restore_database "$DEV_DB_ID" "$BACKUP_ID"
[ "$(p1_pg_at "$DEV_DB_ID" "SELECT note FROM voie_ds_loss WHERE id=1" | tr -d '[:space:]')" = "$DB_ROW" ] ||
  fail "database marker did not survive Lost restore"
p1_assert_tenant_postgres_role "$DEV_DB_ID"
code="$(api_read "$JAR" "${ORIGIN}/api/databases/${DEV_DB_ID}" "$OUT")"
[ "$code" = "200" ] || fail "database get after restore HTTP ${code}"
[ "$(p1_json_field "$OUT" database securityProfile)" = "2" ] ||
  fail "restored Database securityProfile is not 2"
printf 'database Lost restore recovered row %s\n' "$DB_ROW"

p1_build_release
[ -n "${RELEASE_HASH:-}" ] || fail "Release artifact hash is missing"
p1_deploy "$DEV_ENV_ID"
[ "$P1_HTTP_CODE" = "202" ] || [ "$P1_HTTP_CODE" = "200" ] ||
  fail "dev deploy HTTP ${P1_HTTP_CODE}: $(cat "$OUT")"
p1_wait_healthy "$DEPLOYMENT_ID"
BEFORE_HASH="$RELEASE_HASH"
fabric_drop_lv deployment "$DEPLOYMENT_ID"
wait_row "select observed_state from application_deployments where id = '${DEPLOYMENT_ID}'" "needs_release_stream" 90
p1_wait_healthy "$DEPLOYMENT_ID"
code="$(api_read "$JAR" "${ORIGIN}/api/applications/${APPLICATION_ID}/releases" "$OUT")"
[ "$code" = "200" ] || fail "releases list after rematerialize HTTP ${code}"
AFTER_HASH="$(python3 - "$OUT" "$RELEASE_ID" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
want = sys.argv[2]
for item in data.get("items") or []:
    if item.get("id") == want:
        print(item.get("artifactHash") or "")
        break
PY
)"
[ "$AFTER_HASH" = "$BEFORE_HASH" ] ||
  fail "Release hash changed across rematerialize ${BEFORE_HASH} -> ${AFTER_HASH}"
printf 'deployment rematerialized same Release hash %s\n' "$AFTER_HASH"

p1_delete_application
printf 'desired-state loss/recovery pass: workspace %s application %s database %s deployment %s\n' \
  "$WORKSPACE_ID" "$APPLICATION_ID" "$DEV_DB_ID" "$DEPLOYMENT_ID"
