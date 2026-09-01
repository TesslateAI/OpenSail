#!/usr/bin/env bash
# C8 (integration-1): isolation, unknown/no-replay, recovery, restore, and
# cleanup on the live estate. This is an UNSAFE, OPT-IN proof: it restarts
# live control and fabric services and mutates estate state.
#
# Set VOIE_C8_CONFIRM=yes to run. Anything else exits 2 with the exact list
# of unmet preconditions — never a simulated pass.
#
# After this script passes, just live-c8 still has to close public
# management TCP/22. Use just live-c8-preclose to keep SSH open.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
# shellcheck source=tests/live/common.sh
source "${ROOT}/tests/live/common.sh"

if [ "${VOIE_C8_CONFIRM:-}" != "yes" ]; then
  printf 'live-c8 is an unsafe live-estate proof (service restarts, state mutation).\n' >&2
  printf 'Re-run with VOIE_C8_CONFIRM=yes to execute it for real.\n' >&2
  exit 2
fi

command -v curl >/dev/null || edge "curl"
RUN_TAG="$(date +%s)-$$"
command -v python3 >/dev/null || edge "python3"
command -v jq >/dev/null || edge "jq"

RUNTIME="${XDG_RUNTIME_DIR:-/tmp}/voie-live-c8"
rm -rf "$RUNTIME"
install -d -m 700 "$RUNTIME"
cleanup_runtime() {
  rm -rf "$RUNTIME"
}
trap cleanup_runtime EXIT

ORIGIN="${VOIE_CONTROL_URL:-${VOIE_C8_ORIGIN:-}}"
case "$ORIGIN" in
  https://*) ;;
  *) edge "HTTPS control origin (VOIE_CONTROL_URL or VOIE_C8_ORIGIN must be https://...)" ;;
esac
ORIGIN="${ORIGIN%/}"
export VOIE_PUBLIC_ORIGIN="${VOIE_PUBLIC_ORIGIN:-$ORIGIN}"
export VOIE_CONTROL_URL="$ORIGIN"

require_env VOIE_FABRIC_ENDPOINT VOIE_FABRIC_CA_CERT_PATH \
  VOIE_FABRIC_CLIENT_CERT_PATH VOIE_FABRIC_CLIENT_KEY_PATH VOIE_CONTROL_SSH >/dev/null || {
  printf '  (live-c8 needs the mTLS Fabric boundary and control SSH)\n' >&2
  exit 2
}
if [ -z "${VOIE_SESSION_COOKIE:-}" ]; then
  bootstrap_admin_env_ready || {
    printf '  (web session: set VOIE_SESSION_COOKIE or the bootstrap admin username + password file)\n' >&2
    exit 2
  }
fi

FABRIC_SSH="${VOIE_FABRIC_SSH:-baremetal-1-cs}"
CONTROL_SERVICE="${VOIE_CONTROL_SERVICE:-voie-cloud}"
FABRIC_SERVICE="${VOIE_FABRIC_SERVICE:-voie-fabricd}"

ssh_try() {
  local tries=5 n=0
  while [ "$n" -lt "$tries" ]; do
    if "$@"; then return 0; fi
    n=$((n + 1))
    sleep 2
  done
  return 1
}
ssh_fabric() {
  ssh_try ssh -o BatchMode=yes -o ConnectTimeout=8 "$FABRIC_SSH" "$1"
}
CONTROL_SSH=(ssh -o BatchMode=yes -o ConnectTimeout=8)
if [ -n "${VOIE_SSH_PRIVATE_KEY:-}" ]; then
  CONTROL_SSH+=(-i "${VOIE_SSH_PRIVATE_KEY}" -o IdentitiesOnly=yes)
fi
ssh_control() {
  ssh_try "${CONTROL_SSH[@]}" "$VOIE_CONTROL_SSH" "$1"
}

# Reachability before anything else; failures name the exact missing host.
ssh_fabric 'true' >/dev/null 2>&1 ||
  edge "ssh ${FABRIC_SSH} (host down, rescue, or unprovisioned)"
ssh_control 'true' >/dev/null 2>&1 ||
  edge "ssh control via VOIE_CONTROL_SSH"

OUT="$(mktemp)"
WS_ID=""
cleanup_ws() {
  if [ -n "$WS_ID" ]; then
    fabric_rpc DELETE "/v1/workspaces/${WS_ID}" "" "$OUT" >/dev/null 2>&1 || true
    WS_ID=""
  fi
  rm -f "$OUT"
  cleanup_runtime
}
trap cleanup_ws EXIT

# --- isolation ---
CODE="$(curl -sS -o "$OUT" -w '%{http_code}' "${ORIGIN}/api/me")"
[ "$CODE" = "401" ] || fail "unauthenticated /api/me HTTP ${CODE}, want 401"

JAR="${RUNTIME}/cookies.txt"
bootstrap_admin_login "$ORIGIN" "$JAR"

FOREIGN_PROJECT="$(uuid4)"
CODE="$(api_mutate "$JAR" POST "${ORIGIN}/api/projects/${FOREIGN_PROJECT}/sessions" \
  "{\"id\":\"$(uuid4)\",\"agentId\":\"$(uuid4)\",\"workspaceId\":\"$(uuid4)\"}" "$OUT")"
[ "$CODE" = "403" ] || fail "foreign-project session create HTTP ${CODE}, want 403: $(cat "$OUT")"

# --- Fabric direct: qualified runtime and guest isolation ---
STATUS="$(fabric_rpc GET /v1/health "" "$OUT")"
[ "$STATUS" = "200" ] || edge "voie-fabricd mTLS health HTTP ${STATUS}"

STATUS="$(fabric_rpc POST /v1/workspaces '{}' "$OUT")"
[ "$STATUS" = "200" ] || edge "workspace create HTTP ${STATUS}: $(cat "$OUT")"
WS_ID="$(json_field 'id' <"$OUT")"
[ -n "$WS_ID" ] || edge "workspace create returned no id"
DEVICE="$(json_field 'device' <"$OUT" 2>/dev/null || echo '')"
POD="$(json_field 'pod_name' <"$OUT" 2>/dev/null || echo '')"
[ -n "$DEVICE" ] || fail "workspace create returned no device: $(cat "$OUT")"
[ -n "$POD" ] || fail "workspace create returned no pod_name: $(cat "$OUT")"
[ "$(json_field 'state' <"$OUT")" = "ready" ] || fail "workspace not ready: $(cat "$OUT")"
[ "$(json_field 'runtime_class' <"$OUT")" = "voie-firecracker" ] ||
  fail "runtime is not voie-firecracker: $(cat "$OUT")"

if [ -n "$DEVICE" ] && ssh_fabric "findmnt -n -S '${DEVICE}'" >/dev/null 2>&1; then
  fail "workspace device ${DEVICE} is mounted on the fabric host"
fi
await_workspace_mounted "$WS_ID" ||
  fail "/workspace was not mounted in the guest"

# Real, unique host-isolation canary: this sandbox writes its own per-run
# file inside the guest; that exact file must never appear on the fabric
# host. A fixed name would prove nothing on a shared estate.
LEAK_TAG="c8-leak-${RUN_TAG}"
STATUS="$(fabric_rpc POST "/v1/workspaces/${WS_ID}/exec" \
  "{\"call_id\":\"c8-canary\",\"command\":\"printf ${LEAK_TAG} > /workspace/.${LEAK_TAG} && sync\"}" "$OUT")"
[ "$(jq -r .state "$OUT")" = "terminal" ] || fail "leak canary write failed: $(cat "$OUT")"
if ssh_fabric "test -e '/workspace/.${LEAK_TAG}'"; then
  fail "guest file .${LEAK_TAG} appeared on the fabric host: Workspace bytes leaked"
fi

STATUS="$(fabric_rpc POST "/v1/workspaces/${WS_ID}/exec" \
  '{"call_id":"c8-env","command":"printenv"}' "$OUT")"
[ "$(jq -r .state "$OUT")" = "terminal" ] || fail "printenv not terminal: $(cat "$OUT")"
if jq -r .stdout "$OUT" | grep -Eiq 'AZURE_|VOIE_DATABASE|VOIE_MODEL|POSTGRES|CLIENT_SECRET|API_KEY|KEY_VAULT'; then
  fail "guest environ leaked a protected credential"
fi
STATUS="$(fabric_rpc POST "/v1/workspaces/${WS_ID}/exec" \
  '{"call_id":"c8-host-env","command":"test ! -e /etc/voie/control.env && test ! -e /etc/voie/fabric.env && echo isolated"}' "$OUT")"
[ "$(jq -r .stdout "$OUT")" = "isolated" ] || fail "guest saw host control/fabric env files: $(cat "$OUT")"

# --- unknown / no-replay across a real dual-service restart ---
CALL_ID="c8-unknown-$(date +%s)-$$"
VOIE_FABRIC_TIMEOUT=6 fabric_rpc POST "/v1/workspaces/${WS_ID}/exec" \
  "{\"call_id\":\"${CALL_ID}\",\"command\":\"sleep 120\"}" "$OUT" >/dev/null 2>&1 || true

ssh_control "sudo systemctl restart ${CONTROL_SERVICE}" ||
  edge "restart ${CONTROL_SERVICE} over ssh"
ssh_fabric "sudo systemctl restart ${FABRIC_SERVICE}" ||
  edge "restart ${FABRIC_SERVICE} over ssh"
for _ in $(seq 1 180); do
  if curl -sf --max-time 3 "${ORIGIN}/healthz" >/dev/null &&
    [ "$(fabric_rpc GET /v1/health "" /dev/null)" = "200" ]; then
    break
  fi
  sleep 1
done
curl -sf "${ORIGIN}/healthz" >/dev/null || fail "control HTTPS did not recover after restart"
[ "$(fabric_rpc GET /v1/health "" "$OUT")" = "200" ] || fail "fabricd did not recover after restart"

START="$SECONDS"
STATUS="$(fabric_rpc POST "/v1/workspaces/${WS_ID}/exec" \
  "{\"call_id\":\"${CALL_ID}\",\"command\":\"sleep 120\"}" "$OUT")"
ELAPSED=$((SECONDS - START))
[ "$STATUS" = "200" ] || edge "repeated interrupted call HTTP ${STATUS}: $(cat "$OUT")"
[ "$(jq -r .state "$OUT")" = "unknown" ] ||
  fail "repeated interrupted call state $(jq -r .state "$OUT"), want unknown without redispatch"
[ "$ELAPSED" -lt 10 ] ||
  fail "repeated interrupted call waited ${ELAPSED}s; journal must answer from retained state"
STATUS="$(fabric_rpc POST "/v1/workspaces/${WS_ID}/exec" \
  "{\"call_id\":\"${CALL_ID}\",\"command\":\"sleep 1\"}" "$OUT")"
[ "$STATUS" = "409" ] || fail "conflicting hash HTTP ${STATUS}, want 409: $(cat "$OUT")"

STATUS="$(fabric_rpc GET "/v1/workspaces/${WS_ID}" "" "$OUT")"
[ "$STATUS" = "200" ] || fail "workspace read after restart HTTP ${STATUS}"
[ "$(jq -r .state "$OUT")" = "ready" ] || fail "workspace did not restore: $(cat "$OUT")"
[ "$(jq -r .pod_name "$OUT")" = "$POD" ] ||
  fail "restore changed pod identity ($(jq -r .pod_name "$OUT"))"

if ssh_fabric "ls /run/systemd/system/voie-fabricd.service.d 2>/dev/null | grep -q ."; then
  fail "fabric still has a legacy /run override after service restart"
fi
if ssh_control "ls /run/systemd/system/voie-cloud.service.d /run/systemd/system/voie-activation-broker@.service.d 2>/dev/null | grep -q ."; then
  fail "control still has a legacy /run override after service restart"
fi

# --- machine reboot recovery (service restart is not this proof) ---
REBOOT_TAG="c8-reboot-${RUN_TAG}"
STATUS="$(fabric_rpc POST "/v1/workspaces/${WS_ID}/exec" \
  "{\"call_id\":\"c8-reboot-mark\",\"command\":\"printf ${REBOOT_TAG} > /workspace/.${REBOOT_TAG} && sync\"}" "$OUT")"
[ "$(jq -r .state "$OUT")" = "terminal" ] || fail "reboot marker write failed: $(cat "$OUT")"

ssh_fabric "sudo systemctl reboot" >/dev/null 2>&1 || true
sleep 8
FABRIC_BACK=0
for _ in $(seq 1 180); do
  if ssh_fabric "true" >/dev/null 2>&1 &&
    [ "$(fabric_rpc GET /v1/health "" /dev/null)" = "200" ]; then
    FABRIC_BACK=1
    break
  fi
  sleep 2
done
[ "$FABRIC_BACK" = "1" ] || fail "fabric machine did not return after reboot"
if ssh_fabric "ls /run/systemd/system/voie-fabricd.service.d 2>/dev/null | grep -q ."; then
  fail "fabric legacy /run override returned after machine reboot"
fi
ssh_fabric "systemctl is-active --quiet ${FABRIC_SERVICE}" ||
  fail "voie-fabricd is not active after fabric reboot"
ssh_fabric "test -e /dev/voie-ws/runtime -a -e /dev/voie-ws/stage" ||
  fail "runtime or stage LV did not activate after fabric reboot"
ssh_fabric "systemctl is-active --quiet voie-fabric-lvm" ||
  fail "voie-fabric-lvm did not remain active after fabric reboot"
ssh_fabric "findmnt -n -o SOURCE /var/lib/voie-fabricd/stage | grep -q voie" ||
  fail "stage LV did not remount from Fabric VG after reboot"
STATUS="$(fabric_rpc GET "/v1/workspaces/${WS_ID}" "" "$OUT")"
[ "$STATUS" = "200" ] || fail "workspace read after fabric reboot HTTP ${STATUS}"
[ "$(jq -r .state "$OUT")" = "ready" ] || fail "workspace did not survive fabric reboot: $(cat "$OUT")"
STATUS="$(fabric_rpc POST "/v1/workspaces/${WS_ID}/exec" \
  "{\"call_id\":\"c8-reboot-read\",\"command\":\"cat /workspace/.${REBOOT_TAG}\"}" "$OUT")"
[ "$(jq -r .stdout "$OUT")" = "$REBOOT_TAG" ] ||
  fail "workspace marker did not survive fabric reboot: $(cat "$OUT")"

ssh_control "sudo systemctl reboot" >/dev/null 2>&1 || true
sleep 8
CONTROL_BACK=0
for _ in $(seq 1 180); do
  if curl -sf --max-time 3 "${ORIGIN}/healthz" >/dev/null; then
    CONTROL_BACK=1
    break
  fi
  sleep 2
done
[ "$CONTROL_BACK" = "1" ] || fail "control machine did not return after reboot"
if ssh_control "ls /run/systemd/system/voie-cloud.service.d /run/systemd/system/voie-activation-broker@.service.d 2>/dev/null | grep -q ."; then
  fail "control legacy /run override returned after machine reboot"
fi
ssh_control "systemctl is-active --quiet ${CONTROL_SERVICE}" ||
  fail "voie-cloud is not active after control reboot"
[ "$(fabric_rpc GET /v1/health "" "$OUT")" = "200" ] || fail "fabric mTLS failed after control reboot"
JAR3="${RUNTIME}/cookies-reboot.txt"
bootstrap_admin_login "$ORIGIN" "$JAR3"
CODE="$(api_read "$JAR3" "${ORIGIN}/api/me" "$OUT")"
[ "$CODE" = "200" ] || fail "fresh login after control reboot HTTP ${CODE}"
STATUS="$(fabric_rpc GET "/v1/workspaces/${WS_ID}" "" "$OUT")"
[ "$(jq -r .state "$OUT")" = "ready" ] || fail "workspace lost after control reboot"

JAR2="${RUNTIME}/cookies-restore.txt"
bootstrap_admin_login "$ORIGIN" "$JAR2"
CODE="$(api_read "$JAR2" "${ORIGIN}/api/me" "$OUT")"
[ "$CODE" = "200" ] || fail "fresh login after control restart HTTP ${CODE}"

# --- cleanup ---
STATUS="$(fabric_rpc DELETE "/v1/workspaces/${WS_ID}" "" "$OUT")"
[ "$STATUS" = "200" ] || fail "workspace delete HTTP ${STATUS}: $(cat "$OUT")"
for key in pod reservation jail vmm children; do
  [ "$(printf '%s' "$(jq -r ".cleaned.${key}" "$OUT")")" = "true" ] ||
    fail "cleanup ${key} is $(jq -r ".cleaned.${key}" "$OUT"): $(cat "$OUT")"
done
if [ -n "$POD" ] && ssh_fabric "k3s kubectl get pod '${POD}' -n voie-workspace" >/dev/null 2>&1; then
  fail "pod ${POD} still present after delete"
fi
WS_ID=""

echo "live-c8 pass: isolation, guest credential isolation, unknown/no-replay (${ELAPSED}s), process restart, fabric reboot, control reboot, restore, cleanup all proven"
