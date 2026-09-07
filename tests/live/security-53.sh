#!/usr/bin/env bash
# #53 live proofs on the working-branch estate. Exit 2 = missing estate;
# exit 1 = assertion failure. Never prints secrets. Does not PASS when a
# required live invariant was not exercised.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=tests/live/common.sh
source "${ROOT}/tests/live/common.sh"

for envfile in /tmp/voie-runtime/voie-c7/env.sh /tmp/voie-runtime/voie-p1/env.sh; do
  if [ -r "$envfile" ]; then
    set -a
    # shellcheck disable=SC1090
    source "$envfile"
    set +a
  fi
done

host="${1:-baremetal-1-cs}"
CONTROL_SSH="${VOIE_CONTROL_SSH:-control}"
command -v ssh >/dev/null || edge "ssh"
ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" 'true' >/dev/null 2>&1 ||
  edge "KVM Fabric host $host"

ssh_fabric() {
  ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" "$1"
}
ssh_control() {
  ssh -o BatchMode=yes -o ConnectTimeout=8 "$CONTROL_SSH" "$1"
}

ssh_fabric 'systemctl is-active --quiet voie-fabricd' || edge "voie-fabricd on $host"
ssh_fabric 'systemctl is-enabled --quiet dropbear-rescue.service' ||
  fail "managed dropbear-rescue is not enabled"
ssh_fabric 'systemctl is-active --quiet dropbear-rescue.service' ||
  fail "managed dropbear-rescue is not running"

if ssh_fabric 'test -e /etc/systemd/system-generators/voie-iso-rescue \
  -o -e /etc/systemd/system/voie-iso-rescue.service \
  -o -e /var/lib/voie-iso-rescue \
  -o -e /etc/systemd/system/local-fs-pre.target.wants/voie-iso-rescue.service \
  -o -e /etc/systemd/system/sysinit.target.wants/voie-iso-rescue.service'; then
  fail "legacy ISO-rescue implant still present after managed converge"
fi
if ssh_fabric 'iptables -S INPUT 2>/dev/null | grep -E "^-A INPUT -p tcp .* --dport 22 -j ACCEPT"'; then
  fail "legacy INPUT ACCEPT 22 insert still present"
fi

control_ip="$(ssh_fabric 'sed -n "s/^VOIE_GATEWAY_CONTROL_IP=//p" /etc/voie/fabric.env' | tr -d '[:space:]')"
[[ "$control_ip" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
  fail "VOIE_GATEWAY_CONTROL_IP is not an IPv4 in fabric.env"

# Direct host-edge: loopback is not the control Tailscale IPv4, so the
# helper must close before nsenter.
if ! ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" python3 - <<'PY'
import socket, sys
s = socket.socket()
s.settimeout(3)
try:
    s.connect(("127.0.0.1", 8082))
except Exception:
    sys.exit(0)
try:
    s.sendall(b"GET / HTTP/1.1\r\nHost: example.dev\r\n\r\n")
    data = s.recv(64)
except Exception:
    data = b""
s.close()
sys.exit(0 if not data else 1)
PY
then
  fail "Fabric :8082 accepted a non-control loopback source"
fi

fabric_ts="$(ssh_fabric 'tailscale ip -4' | awk 'NR==1{print; exit}')"
[[ "$fabric_ts" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
  fail "Fabric Tailscale IPv4 is missing"

# Control Tailscale source must be admitted.
if ! ssh -o BatchMode=yes -o ConnectTimeout=8 "$CONTROL_SSH" python3 - "$fabric_ts" <<'PY'
import socket, sys
target = sys.argv[1]
s = socket.socket()
s.settimeout(5)
s.connect((target, 8082))
s.sendall(b"GET / HTTP/1.1\r\nHost: example.dev\r\n\r\n")
try:
    data = s.recv(64)
except Exception:
    data = b""
s.close()
sys.exit(0 if data else 1)
PY
then
  fail "Fabric :8082 rejected the Control Tailscale source"
fi

# Non-control tailnet peer must be rejected. Prefer this host when it is
# enrolled; otherwise bind from the Fabric node itself (the only other
# tailnet peer). Do not skip the probe.
peer_probe_ok=0
if command -v tailscale >/dev/null 2>&1; then
  peer_ip="$(tailscale ip -4 2>/dev/null | awk 'NR==1{print; exit}')"
  if [[ "$peer_ip" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] && [ "$peer_ip" != "$control_ip" ]; then
    python3 - "$fabric_ts" <<'PY' || fail "Fabric :8082 accepted a non-control tailnet peer"
import socket, sys
target = sys.argv[1]
s = socket.socket()
s.settimeout(5)
try:
    s.connect((target, 8082))
except Exception:
    sys.exit(0)
try:
    s.sendall(b"GET / HTTP/1.1\r\nHost: example.dev\r\n\r\n")
    data = s.recv(64)
except Exception:
    data = b""
s.close()
sys.exit(0 if not data else 1)
PY
    peer_probe_ok=1
  fi
fi
if [ "$peer_probe_ok" != 1 ]; then
  if ! ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" python3 - "$fabric_ts" <<'PY'
import socket, sys
target = sys.argv[1]
s = socket.socket()
s.settimeout(5)
try:
    s.bind((target, 0))
except OSError:
    pass
try:
    s.connect((target, 8082))
except Exception:
    sys.exit(0)
try:
    s.sendall(b"GET / HTTP/1.1\r\nHost: example.dev\r\n\r\n")
    data = s.recv(64)
except Exception:
    data = b""
s.close()
sys.exit(0 if not data else 1)
PY
  then
    fail "Fabric :8082 accepted a non-control tailnet peer"
  fi
fi

count_edge() {
  ssh_fabric "ps -eo args= | grep -c '[n]senter -t .* python3'" || true
}

baseline_nsenter="$(count_edge)"
ssh -o BatchMode=yes -o ConnectTimeout=8 "$CONTROL_SSH" python3 - "$fabric_ts" <<'PY' &
import socket, sys, time
target = sys.argv[1]
conns = []
for _ in range(80):
    s = socket.socket()
    s.settimeout(2)
    try:
        s.connect((target, 8082))
        conns.append(s)
    except Exception:
        pass
time.sleep(8)
for s in conns:
    try:
        s.close()
    except Exception:
        pass
PY
fan_pid=$!
sleep 3
peak_nsenter="$(count_edge)"
if [ "${peak_nsenter:-0}" -gt 64 ]; then
  fail "gateway nsenter children ${peak_nsenter} exceeded splice ceiling 64"
fi
wait "$fan_pid" || fail "gateway fan-out probe failed"
sleep 3
after_nsenter="$(count_edge)"
[ "${after_nsenter:-0}" -le $((baseline_nsenter + 2)) ] ||
  fail "gateway nsenter count did not return to baseline (${after_nsenter} vs ${baseline_nsenter})"
ssh_fabric 'systemctl is-active --quiet voie-fabricd' || fail "fabricd unhealthy after fan-out"
ssh -o BatchMode=yes -o ConnectTimeout=8 "$CONTROL_SSH" python3 - "$fabric_ts" <<'PY' || fail "normal gateway routing failed after fan-out"
import socket, sys
s = socket.socket(); s.settimeout(5)
s.connect((sys.argv[1], 8082))
s.sendall(b"GET /healthz HTTP/1.1\r\nHost: example.dev\r\n\r\n")
data = b""
try:
    data = s.recv(64)
except Exception:
    pass
s.close()
sys.exit(0 if data else 1)
PY

ORIGIN="${VOIE_CONTROL_URL:-${VOIE_C7_ORIGIN:-${VOIE_PUBLIC_ORIGIN:-}}}"
case "$ORIGIN" in
  https://*) ;;
  *) fail "VOIE_CONTROL_URL is required for the Database census" ;;
esac
ORIGIN="${ORIGIN%/}"
export VOIE_CONTROL_URL="$ORIGIN"
bootstrap_admin_env_ready || fail "bootstrap admin credentials are required for the Database census"

RUNTIME="${XDG_RUNTIME_DIR:-/tmp}/voie-live-security-53"
install -d -m 700 "$RUNTIME"
JAR="${RUNTIME}/cookies.txt"
OUT="${RUNTIME}/body.json"
bootstrap_admin_login "$ORIGIN" "$JAR"
code="$(api_read "$JAR" "${ORIGIN}/api/admin/health" "$OUT")"
[ "$code" = "200" ] || fail "admin health HTTP ${code}: $(cat "$OUT")"

mapfile -t DB_ROWS < <(python3 - "$OUT" <<'PY'
import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
items=(data.get("databases") or {}).get("items") or []
for item in items:
    print(f"{item.get('id')}\t{item.get('state')}\t{item.get('securityProfile')}")
PY
)
if [ "${#DB_ROWS[@]}" -eq 0 ]; then
  fail "estate contains zero non-deleted Databases; postgres census not exercised"
fi
insecure="$(python3 - "$OUT" <<'PY'
import json,sys
print((json.load(open(sys.argv[1], encoding="utf-8")).get("databases") or {}).get("insecure", 0))
PY
)"
[ "$insecure" = "0" ] || fail "${insecure} non-deleted Database(s) still have security_profile < 2"
for row in "${DB_ROWS[@]}"; do
    db_id="${row%%$'\t'*}"
    rest="${row#*$'\t'}"
    state="${rest%%$'\t'*}"
    gen="${rest##*$'\t'}"
    [ "$gen" = "2" ] || fail "Database ${db_id} securityProfile=${gen} state=${state}"
    pod=""
    for _ in $(seq 1 90); do
      pod="$(ssh_fabric "k3s kubectl get pod -A -l io.voie/database=${db_id} -o json" \
        | python3 -c '
import json, sys
items = json.load(sys.stdin).get("items") or []
rst_ready = []
rst_pending = False
create_ready = []
for item in items:
    meta = item.get("metadata") or {}
    ns = str(meta.get("namespace") or "")
    name = str(meta.get("name") or "")
    if not ns or not name:
        continue
    phase = str((item.get("status") or {}).get("phase") or "")
    if phase in ("Succeeded", "Failed"):
        continue
    conds = (item.get("status") or {}).get("conditions") or []
    ready = any(c.get("type") == "Ready" and c.get("status") == "True" for c in conds)
    if name.startswith("voie-pg-rst-"):
        if ready:
            rst_ready.append((ns, name))
        else:
            rst_pending = True
    elif ready:
        create_ready.append((ns, name))
if rst_ready:
    print("%s/%s" % rst_ready[0])
elif rst_pending:
    pass
elif create_ready:
    print("%s/%s" % create_ready[0])
' || true)"
      if [[ "$pod" == */* && "$pod" != "/" ]]; then
        break
      fi
      sleep 2
    done
    [[ "$pod" == */* && "$pod" != "/" ]] || fail "Database ${db_id} has no Ready postgres Pod"
    ns="${pod%%/*}"
    name="${pod#*/}"
    ssh_fabric "k3s kubectl get pod -n ${ns} ${name} -o jsonpath='{.status.conditions[?(@.type==\"Ready\")].status}'" | grep -qx True ||
      fail "postgres Pod for ${db_id} is not Ready"
    flags="$(ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
      "k3s kubectl exec -i -n $(printf '%q' "$ns") $(printf '%q' "$name") -c postgres --request-timeout 45s -- /bin/sh" \
      <<'EOS' | tr -d '[:space:]'
set -eu
PGPASSWORD=$(cat /run/voie/postgres-password)
export PGPASSWORD
exec /bin/psql -U app -h 127.0.0.1 -d app -Atc "SELECT CASE WHEN rolsuper THEN 't' ELSE 'f' END||','||CASE WHEN rolcreatedb THEN 't' ELSE 'f' END||','||CASE WHEN rolcreaterole THEN 't' ELSE 'f' END||','||CASE WHEN rolreplication THEN 't' ELSE 'f' END||','||CASE WHEN rolbypassrls THEN 't' ELSE 'f' END FROM pg_roles WHERE rolname='app'"
EOS
)"
    [ "$flags" = "f,f,f,f,f" ] || fail "Database ${db_id} app flags ${flags}"
    platform="$(ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
      "k3s kubectl exec -i -n $(printf '%q' "$ns") $(printf '%q' "$name") -c postgres --request-timeout 45s -- /bin/sh" \
      <<'EOS' | tr -d '[:space:]'
set -eu
PGPASSWORD=$(cat /run/voie/postgres-password)
export PGPASSWORD
exec /bin/psql -U app -h 127.0.0.1 -d app -Atc "SELECT rolcanlogin FROM pg_roles WHERE rolname='voie_platform'"
EOS
)"
    [ "$platform" = "f" ] || fail "Database ${db_id} voie_platform.rolcanlogin=${platform}"
    copy_rc=0
    ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
      "k3s kubectl exec -i -n $(printf '%q' "$ns") $(printf '%q' "$name") -c postgres --request-timeout 45s -- /bin/sh" \
      <<'EOS' >/dev/null 2>&1 || copy_rc=$?
set -eu
PGPASSWORD=$(cat /run/voie/postgres-password)
export PGPASSWORD
exec /bin/psql -U app -h 127.0.0.1 -d app -c "COPY (SELECT 1) TO PROGRAM 'true'"
EOS
    [ "$copy_rc" -ne 0 ] || fail "COPY PROGRAM succeeded for Database ${db_id}"
    ssh_fabric "k3s kubectl exec -n ${ns} ${name} -c postgres -- test ! -e /tmp/voie-postgres-password" ||
      fail "/tmp/voie-postgres-password present in ${db_id}"
done

# Two real Application sources: per-source ceiling fairness + special dests.
app_pods="$(ssh_fabric "k3s kubectl get pod -A -l io.voie/kind=application --field-selector=status.phase=Running -o jsonpath='{range .items[*]}{.metadata.namespace}/{.metadata.name}{\"\\n\"}{end}'")"
mapfile -t APP_PODS <<<"$app_pods"
APP_PODS=("${APP_PODS[@]// }")
real_apps=()
for pod in "${APP_PODS[@]}"; do
  [[ "$pod" == */* && "$pod" != "/" ]] && real_apps+=("$pod")
done
if [ "${#real_apps[@]}" -lt 2 ]; then
  fail "egress fairness needs two running Application Pods (found ${#real_apps[@]})"
fi
pod_a="${real_apps[0]}"
pod_b="${real_apps[1]}"
ns_a="${pod_a%%/*}"; name_a="${pod_a#*/}"
ns_b="${pod_b%%/*}"; name_b="${pod_b#*/}"
egress_ip="$(ssh_fabric "k3s kubectl -n voie-workspace get svc voie-egress -o jsonpath='{.spec.clusterIP}'")"
[[ "$egress_ip" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
  fail "voie-egress ClusterIP is missing"

# Hold 32 TCP slots from A (MAX_PER_SOURCE), then prove an extra A HTTP
# request is closed and B can still speak to the proxy.
ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
  "k3s kubectl exec -n $(printf '%q' "$ns_a") $(printf '%q' "$name_a") --request-timeout 50s -- /bin/busybox sh -c 'i=0; while [ \$i -lt 32 ]; do nc ${egress_ip} 8080 >/dev/null 2>&1 & i=\$((i+1)); done; sleep 8; wait'" \
  >/dev/null 2>&1 &
hold_pid=$!
sleep 3
a_extra_rc=0
ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
  "k3s kubectl exec -n $(printf '%q' "$ns_a") $(printf '%q' "$name_a") --request-timeout 20s -- /bin/busybox sh -c $(printf '%q' "NO_PROXY= HTTP_PROXY=http://${egress_ip}:8080 HTTPS_PROXY=http://${egress_ip}:8080 wget -q -O /dev/null --timeout=4 http://example.com/")" \
  >/dev/null 2>&1 || a_extra_rc=$?
[ "$a_extra_rc" -ne 0 ] || fail "Application A exceeded per-source egress ceiling"
# Fairness: B still obtains proxy service while A holds slots. A legitimate
# proxy response is enough to prove B got a worker.
ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
  "k3s kubectl exec -n $(printf '%q' "$ns_b") $(printf '%q' "$name_b") --request-timeout 20s -- /bin/busybox sh -c $(printf '%q' "printf \"CONNECT example.com:80 HTTP/1.1\\r\\nHost: example.com:80\\r\\n\\r\\n\" | nc -w 4 ${egress_ip} 8080 | head -c 64")" \
  | grep -Eiq 'HTTP/1\.[01] ' || fail "Application B could not reach voie-egress while A held slots"
wait "$hold_pid" || true

# Special destinations through the real proxy (NO_PROXY cleared).
specials='127.0.0.1 ::1 169.254.1.1 fe80::1 0.0.0.0 :: 224.0.0.1 ff02::1'
for dest in $specials; do
  url="http://${dest}/"
  case "$dest" in
    *:*) url="http://[${dest}]/" ;;
  esac
  rc=0
  ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
    "k3s kubectl exec -n $(printf '%q' "$ns_a") $(printf '%q' "$name_a") --request-timeout 20s -- /bin/busybox sh -c $(printf '%q' "NO_PROXY= HTTP_PROXY=http://${egress_ip}:8080 HTTPS_PROXY=http://${egress_ip}:8080 wget -q -O /dev/null --timeout=4 '$url'")" \
    >/dev/null 2>&1 || rc=$?
  [ "$rc" -ne 0 ] || fail "egress allowed special destination ${dest}"
done

# Approved ordinary egress: CONNECT must establish and the upstream request
# must succeed. 403/502/timeout/EOF/reset fail this assertion. The guest
# ash has no /dev/tcp; nc is the TCP client in voie-app:v1.
ordinary="$(ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
  "k3s kubectl exec -n $(printf '%q' "$ns_b") $(printf '%q' "$name_b") --request-timeout 30s -- /bin/busybox sh -c $(printf '%q' "printf \"CONNECT example.com:80 HTTP/1.1\\r\\nHost: example.com:80\\r\\n\\r\\nGET / HTTP/1.1\\r\\nHost: example.com\\r\\nConnection: close\\r\\n\\r\\n\" | nc -w 12 ${egress_ip} 8080 | head -c 400")" \
  2>/dev/null || true)"
printf '%s\n' "$ordinary" | grep -Fq 'HTTP/1.1 200 Connection Established' ||
  fail "ordinary egress CONNECT did not establish: ${ordinary}"
printf '%s\n' "$ordinary" | grep -Eiq 'HTTP/1\.[01] 200' ||
  fail "ordinary egress upstream request failed: ${ordinary}"

printf 'live-security-53 pass: census, control-only :8082, fan-out bound, rescue disarmed, egress specials\n'
