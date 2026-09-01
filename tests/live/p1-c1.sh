#!/usr/bin/env bash
# P1-C1: agent creates an Application and tests a normal web project in the
# Workspace guest; no project command runs on control or Fabric host.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=tests/live/p1-common.sh
source "${ROOT}/tests/live/p1-common.sh"

host="${1:-baremetal-1-cs}"
p1_require_fabric_host "$host"
p1_require_guest_images "$host"
p1_require_control
p1_require_model

ssh_ok() { ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" "$1"; }

CANARY="${TMPDIR:-/tmp}/voie-p1-c1-canary"
install_host_canary "$CANARY"
export PATH="$CANARY:$PATH"

ssh_ok 'k3s kubectl get runtimeclass voie-firecracker >/dev/null'
listing="$(ssh_ok 'k3s ctr -n k8s.io images ls')"
echo "$listing" | grep -q 'voie-workspace:v1' || fail "workspace guest image missing after estate check"

p1_boot_session
p1_ready_unbound_workspace
SLUG="p1c1$(python3 -c 'import uuid; print(uuid.uuid4().hex[:8])')"
p1_agent_create_and_test "$SLUG" "P1 C1 tracker"
p1_require_workspace_guest_image
p1_read_guest_manifest >/dev/null
p1_guest_test
p1_assert_canary_quiet "$CANARY"

printf 'p1-c1 Application %s created by the agent; guest wrote voie.toml and passed py_compile in the Workspace\n' "$APPLICATION_ID"
