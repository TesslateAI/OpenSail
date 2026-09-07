#!/usr/bin/env bash
# Checkpoint C2 live proof for integration-1. Runs on this machine, drives
# one Fabric host over ssh for qualification, and every workspace operation
# goes through the product voie-fabricd over mTLS.
#
# Failures are real: nothing here substitutes a host-local command for guest
# exec. The lab sidecar path of the source commit is gone — integration-1
# speaks product mTLS only.
#
# Proves:
#   1. the qualified Firecracker runtime identity (KVM, k3s,
#      kata-fc-rs-voie handler, RuntimeClass voie-firecracker);
#   2. a jailed Firecracker VMM under a non-root per-sandbox identity;
#   3. the workspace block device is never mounted on the host;
#   4. E1 writes a marker inside the guest; E1 is replaced by E2 with a new
#      pod identity while device and PV survive;
#   5. E2 reads the marker back and a repeated call ID returns the retained
#      terminal result (no replay);
#   6. DELETE positively removes pod, reservation, jail, VMM, and children.
set -euo pipefail

host="${1:-baremetal-1-cs}"
LEAK_TAG="c2-$(date +%s)-$$"
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$ROOT"
# shellcheck source=tests/live/common.sh
source "${ROOT}/tests/live/common.sh"

# This proof mutates the shared Fabric estate: it creates and deletes one
# workspace. Opt in explicitly; anything else fails closed.
if [ "${VOIE_LIVE_C2_CONFIRM:-}" != "yes" ]; then
  printf 'live-c2 is an unsafe shared-estate mutation.\n' >&2
  printf 'Re-run with VOIE_LIVE_C2_CONFIRM=yes to execute it for real.\n' >&2
  exit 2
fi

require_env VOIE_FABRIC_ENDPOINT VOIE_FABRIC_CA_CERT_PATH \
  VOIE_FABRIC_CLIENT_CERT_PATH VOIE_FABRIC_CLIENT_KEY_PATH >/dev/null || {
  printf '  (live-c2 drives the product voie-fabricd over mTLS only)\n' >&2
  exit 2
}
command -v jq >/dev/null || edge "jq"
command -v python3 >/dev/null || edge "python3"

ssh_ok() {
  ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" "$1"
}

if ! ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" 'true' >/dev/null 2>&1; then
  printf 'live-c2: remaining live dependency: ssh %s (host down, rescue, or unprovisioned)\n' "$host" >&2
  exit 2
fi
if ! ssh_ok 'test -w /dev/kvm && systemctl is-active --quiet k3s'; then
  printf 'live-c2: remaining live dependency: %s K3s/KVM fabric (rescue or unprovisioned)\n' "$host" >&2
  exit 2
fi

# Exact Firecracker runtime identity (same checks as C1).
ssh_ok 'test -w /dev/kvm'
ssh_ok 'systemctl is-active --quiet k3s'
ssh_ok 'grep -q "kata-fc-rs-voie" /var/lib/rancher/k3s/agent/etc/containerd/config-v3.toml.d/voie-kata-fc-rs.toml'
ssh_ok 'test "$(k3s kubectl get runtimeclass voie-firecracker -o jsonpath={.handler})" = "kata-fc-rs-voie"'

OUT="$(mktemp)"
WS_ID=""
cleanup() {
  if [ -n "$WS_ID" ]; then
    fabric_rpc DELETE "/v1/workspaces/${WS_ID}" "" "$OUT" >/dev/null 2>&1 || true
    WS_ID=""
  fi
  rm -f "$OUT"
}
trap cleanup EXIT

STATUS="$(fabric_rpc GET /v1/health "" "$OUT")"
[ "$STATUS" = "200" ] || { printf 'live-c2: remaining live dependency: fabricd mTLS health (HTTP %s)\n' "$STATUS" >&2; exit 2; }

scratch_workspace_open "$OUT"
WS_ID="$WORKSPACE_ID"
STATUS="$(fabric_rpc GET "/v1/workspaces/${WS_ID}" "" "$OUT")"
[ "$STATUS" = "200" ] || { printf 'live-c2: remaining live dependency: workspace GET (HTTP %s)\n' "$STATUS" >&2; exit 2; }
echo "live-c2 create: $(cat "$OUT")"
DEVICE="$(json_field 'device' <"$OUT")"
PV_NAME="$(json_field 'pv_name' <"$OUT" 2>/dev/null || echo '')"
[ -n "$PV_NAME" ] || fail "workspace create returned no pv_name; replacement retention is unprovable"
POD1="$(json_field 'pod_name' <"$OUT")"
UID1="$(json_field 'pod_uid' <"$OUT")"
[ "$(json_field 'runtime_class' <"$OUT")" = "voie-firecracker" ]
[ "$(json_field 'state' <"$OUT")" = "ready" ]
[ -n "$UID1" ] && [ -n "$DEVICE" ]

# Guest is a jailed Firecracker VMM bound to THIS workspace's pod: select the
# firecracker process whose cgroup carries this pod's UID, then require the
# non-root per-sandbox identity. A first-match pgrep could hit another
# tenant's sandbox on a shared estate.
ssh_ok "set -e; cg_uid=\$(printf '%s' '${UID1}' | tr '-' '_'); for i in \$(seq 1 30); do p=''; for cand in \$(pgrep -x firecracker || true); do grep -q \"\$cg_uid\" /proc/\$cand/cgroup 2>/dev/null && { p=\$cand; break; }; done; [ -n \"\$p\" ] && break; sleep 1; done; test -n \"\$p\"; uid=\$(stat -c %u /proc/\$p); test \$uid -ge 100000; echo \"jailed firecracker pid=\$p uid=\$uid pod=${UID1}\""


# The block device must not be mounted on the host: bytes live in the guest.
if ssh_ok "findmnt -n -S '${DEVICE}'" >/dev/null 2>&1; then
  printf 'live-c2: workspace device %s is mounted on the host; refusing host-local path\n' "$DEVICE" >&2
  exit 1
fi
ssh_ok "test ! -e '/workspace/.${LEAK_TAG}'"

await_workspace_mounted "$WS_ID" ||
  { printf 'live-c2: /workspace was not mounted in the guest\n' >&2; exit 1; }

STATUS="$(fabric_rpc POST "/v1/workspaces/${WS_ID}/exec" \
  "{\"call_id\":\"e1-write\",\"command\":\"printf ${LEAK_TAG} > /workspace/.${LEAK_TAG} && sync && cat /workspace/.${LEAK_TAG}\"}" "$OUT")"
echo "live-c2 e1-write: $(cat "$OUT")"
[ "$(jq -r .state "$OUT")" = "terminal" ]
[ "$(jq -r .exit_code "$OUT")" = "0" ]
[ "$(jq -r .stdout "$OUT")" = "${LEAK_TAG}" ]

# Host still has no such file: the write went through voie-runner in the guest,
# and the bytes stayed bound to this sandbox's block device.
if ssh_ok "test -e '/workspace/.${LEAK_TAG}'"; then
  printf 'live-c2: guest file .%s appeared on the fabric host\n' "$LEAK_TAG" >&2
  exit 1
fi

STATUS="$(fabric_rpc POST "/v1/workspaces/${WS_ID}/replace" "" "$OUT")"
echo "live-c2 replace: $(cat "$OUT")"
UID2="$(json_field 'pod_uid' <"$OUT")"
POD2="$(json_field 'pod_name' <"$OUT")"
DEVICE2="$(json_field 'device' <"$OUT")"
PV2="$(json_field 'pv_name' <"$OUT" 2>/dev/null || echo '')"
[ -n "$PV2" ] || fail "replacement returned no pv_name"
[ -n "$UID2" ]
[ "$UID1" != "$UID2" ]
[ "$POD1" != "$POD2" ]
[ "$DEVICE" = "$DEVICE2" ]
[ "$PV_NAME" = "$PV2" ]
[ "$(json_field 'generation' <"$OUT")" = "2" ]

await_workspace_mounted "$WS_ID" ||
  { printf 'live-c2: /workspace was not mounted in the guest\n' >&2; exit 1; }

STATUS="$(fabric_rpc POST "/v1/workspaces/${WS_ID}/exec" \
  "{\"call_id\":\"e2-read\",\"command\":\"cat /workspace/.${LEAK_TAG}\"}" "$OUT")"
echo "live-c2 e2-read: $(cat "$OUT")"
[ "$(jq -r .state "$OUT")" = "terminal" ]
[ "$(jq -r .stdout "$OUT")" = "${LEAK_TAG}" ]

# No-replay: the same call id returns the retained terminal result.
STATUS="$(fabric_rpc POST "/v1/workspaces/${WS_ID}/exec" \
  "{\"call_id\":\"e2-read\",\"command\":\"cat /workspace/.${LEAK_TAG}\"}" "$OUT")"
[ "$(jq -r .state "$OUT")" = "terminal" ]
[ "$(jq -r .stdout "$OUT")" = "${LEAK_TAG}" ]

STATUS="$(fabric_rpc DELETE "/v1/workspaces/${WS_ID}" "" "$OUT")"
echo "live-c2 delete: $(cat "$OUT")"
for key in pod reservation jail vmm children; do
  [ "$(jq -r ".cleaned.${key}" "$OUT")" = "true" ]
done

ssh_ok "k3s kubectl get pod '${POD2}' -n voie-workspace" >/dev/null 2>&1 && {
  printf 'live-c2: pod %s still present after delete\n' "$POD2" >&2
  exit 1
} || true
WS_ID=""

echo "live-c2: marker survived E1 -> E2 on ${DEVICE} (pod ${UID1} -> ${UID2})"
