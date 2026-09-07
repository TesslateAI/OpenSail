#!/usr/bin/env bash
# C7 waits until every non-deleted Database has security_profile 2.
# Polls platform-admin health; does not sleep blindly.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=tests/live/common.sh
source "${ROOT}/tests/live/common.sh"

ORIGIN="${VOIE_CONTROL_URL:-${VOIE_C7_ORIGIN:-${VOIE_PUBLIC_ORIGIN:-}}}"
case "$ORIGIN" in
  https://*|http://127.0.0.1*|http://localhost*) ;;
  *)
    printf 'wait-database-security: set VOIE_CONTROL_URL to the deployed origin\n' >&2
    exit 2
    ;;
esac
ORIGIN="${ORIGIN%/}"
export VOIE_CONTROL_URL="$ORIGIN"
export VOIE_PUBLIC_ORIGIN="${VOIE_PUBLIC_ORIGIN:-$ORIGIN}"

bootstrap_admin_env_ready || {
  printf 'wait-database-security: bootstrap admin credentials are required\n' >&2
  exit 2
}

RUNTIME="${XDG_RUNTIME_DIR:-/tmp}/voie-wait-db-sec"
install -d -m 700 "$RUNTIME"
JAR="${RUNTIME}/cookies.txt"
OUT="${RUNTIME}/health.json"

ready=0
for _ in $(seq 1 60); do
  if curl -sf --max-time 3 "${ORIGIN}/healthz" >/dev/null; then
    ready=1
    break
  fi
  sleep 2
done
[ "$ready" = 1 ] || {
  printf 'wait-database-security: control /healthz did not return\n' >&2
  exit 1
}

bootstrap_admin_login "$ORIGIN" "$JAR"

deadline=$((SECONDS + 900))
last_insecure=""
last_live=""
while [ "$SECONDS" -lt "$deadline" ]; do
  code="$(api_read "$JAR" "${ORIGIN}/api/admin/health" "$OUT")"
  [ "$code" = "200" ] || {
    sleep 4
    continue
  }
  live="$(python3 - "$OUT" <<'PY'
import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
print((data.get("databases") or {}).get("live", 0))
PY
)"
  insecure="$(python3 - "$OUT" <<'PY'
import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
print((data.get("databases") or {}).get("insecure", 0))
PY
)"
  last_insecure="$insecure"
  last_live="$live"
  if [ "$insecure" = "0" ]; then
    printf 'wait-database-security: %s live Database(s), zero with security_profile < 2\n' "$live"
    exit 0
  fi
  python3 - "$OUT" <<'PY' | while IFS= read -r db_id; do
import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
for item in (data.get("databases") or {}).get("items") or []:
    gen=item.get("securityProfile")
    db_id=item.get("id")
    if db_id and (gen is None or int(gen) < 2):
        print(db_id)
PY
    [ -n "$db_id" ] || continue
    api_mutate "$JAR" POST "${ORIGIN}/api/databases/${db_id}/security-profile" \
      '{"securityProfile":2}' "${RUNTIME}/security-profile.json" >/dev/null || true
  done
  sleep 4
done

printf 'wait-database-security: timed out with %s live Database(s), %s still security_profile < 2\n' \
  "${last_live:-unknown}" "${last_insecure:-unknown}" >&2
python3 - "$OUT" <<'PY' >&2 || true
import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
items=(data.get("databases") or {}).get("items") or []
for item in items:
    gen=item.get("securityProfile")
    if gen is None or int(gen) < 2:
        print(f"  {item.get('id')} state={item.get('state')} securityProfile={gen}")
PY
exit 1
