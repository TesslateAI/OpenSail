#!/usr/bin/env bash
# P1-C3: agent creates postgres Application and Databases; dev and prod
# databases are distinct; Application persists across Pod restart; prod
# credential never enters Workspace or conversation log.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=tests/live/p1-common.sh
source "${ROOT}/tests/live/p1-common.sh"

host="${1:-baremetal-1-cs}"
p1_require_fabric_host "$host"
p1_require_guest_images "$host"
p1_require_control
p1_require_model

CANARY="${TMPDIR:-/tmp}/voie-p1-c3-canary"
install_host_canary "$CANARY"
export PATH="$CANARY:$PATH"

p1_boot_session
p1_ready_unbound_workspace
SLUG="p1c3$(python3 -c 'import uuid; print(uuid.uuid4().hex[:8])')"
p1_agent_create_and_test "$SLUG" "P1 C3 tracker" postgres
p1_require_workspace_guest_image
p1_guest_test
p1_agent_create_databases
[ "$DEV_DB_ID" != "$PROD_DB_ID" ] || fail "dev and prod databases must be distinct"

MARKER="voie-p1c3-$(python3 -c 'import uuid; print(uuid.uuid4().hex[:12])')"
p1_bind_prod_secret "$MARKER"
export P1_SECRET_NEEDLES="$MARKER"

code="$(api_read "$JAR" "${ORIGIN}/api/databases/${PROD_DB_ID}" "$OUT")"
[ "$code" = "200" ] || fail "prod database get HTTP ${code}"
p1_assert_no_secrets

code="$(api_read "$JAR" "${ORIGIN}/api/applications/${APPLICATION_ID}" "$OUT")"
[ "$code" = "200" ] || fail "application get HTTP ${code}"
p1_assert_no_secrets
printf '%s' "$(cat "$OUT")" | grep -F -q "$MARKER" && fail "application metadata contained Environment secret material"

p1_agent_build_release
p1_agent_deploy_dev
p1_wait_healthy "$DEPLOYMENT_ID"
p1_activate_healthy "$DEPLOYMENT_ID"

code="$(api_mutate "$JAR" POST "${ORIGIN}/api/deployments/${DEPLOYMENT_ID}/restart" '{}' "$OUT")"
[ "$code" = "200" ] || fail "restart HTTP ${code}: $(cat "$OUT")"
# Restart 200 is Fabric apply. Kubelet Ready and /healthz are observational.
p1_wait_healthy "$DEPLOYMENT_ID"
p1_authenticated_preview "$DEV_HOST" "$DEV_ENV_ID" "tracker"

p1_guest_scan_no_secrets
p1_scan_conversations_no_secrets
p1_assert_canary_quiet "$CANARY"
printf 'p1-c3 distinct databases %s / %s; restart held; Workspace and conversation have no credentials\n' \
  "$DEV_DB_ID" "$PROD_DB_ID"
