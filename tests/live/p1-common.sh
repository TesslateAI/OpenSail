#!/usr/bin/env bash
# Shared helpers for Profile 1 live checkpoints. Sourced, never executed.
# Exit 2 = missing live estate; exit 1 = assertion failure.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=tests/live/common.sh
source "${ROOT}/tests/live/common.sh"

p1_require_fabric_host() {
  local host="${1:-baremetal-1-cs}"
  P1_FABRIC_HOST="$host"
  export P1_FABRIC_HOST
  ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" 'true' >/dev/null 2>&1 ||
    edge "KVM Fabric host $host"
  ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" 'test -w /dev/kvm' ||
    edge "writable KVM on $host"
  ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" 'systemctl is-active --quiet k3s' ||
    edge "k3s on $host"
  ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
    'test "$(k3s kubectl get runtimeclass voie-firecracker -o jsonpath={.handler})" = "kata-fc-rs-voie"' ||
    edge "RuntimeClass voie-firecracker / kata-fc-rs-voie on $host"
}

p1_require_guest_images() {
  local host="${1:-baremetal-1-cs}"
  local listing
  listing="$(ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" 'k3s ctr -n k8s.io images ls' 2>/dev/null)" ||
    edge "containerd image list on $host"
  # Here-string avoids SIGPIPE from `grep -q` closing a pipe early under
  # `set -o pipefail` (a 45KiB `ctr images ls` exceeds the pipe buffer).
  grep -Fq 'voie-workspace:v1' <<<"$listing" || edge "voie-workspace:v1 on $host"
  grep -Fq 'voie-app:v1' <<<"$listing" || edge "voie-app:v1 on $host"
  grep -Fq 'voie-postgres:v1' <<<"$listing" || edge "voie-postgres:v1 on $host"
  grep -Fq 'voie-gateway:v1' <<<"$listing" || edge "voie-gateway:v1 on $host"
}

p1_require_control() {
  load_local_stack_env || true
  require_env VOIE_PUBLIC_ORIGIN VOIE_DATABASE_URL VOIE_FABRIC_ENDPOINT \
    VOIE_FABRIC_CA_CERT_PATH VOIE_FABRIC_CLIENT_CERT_PATH VOIE_FABRIC_CLIENT_KEY_PATH \
    >/dev/null || {
    printf '  (live-p1 drives Application/Release/Deployment through real Fabric)\n' >&2
    edge "voie-cloud + mTLS Fabric control plane"
  }
}

# P1-C1 is the agent path. Fixture models and a missing provider are a
# missing live edge, not a skipped assertion.
p1_require_model() {
  refuse_fixture_model live-p1
  require_env VOIE_MODEL_BASE_URL VOIE_MODEL_NAME >/dev/null || {
    printf '  (live-p1 drives Application create, pack, and deploy through a real model tool loop)\n' >&2
    edge "real model provider (VOIE_MODEL_BASE_URL, VOIE_MODEL_NAME)"
  }
  if [ -z "${VOIE_MODEL_API_KEY:-}" ] && [ -z "${VOIE_MODEL_API_KEY_FILE:-}" ]; then
    edge "model provider credential (VOIE_MODEL_API_KEY or VOIE_MODEL_API_KEY_FILE)"
  fi
}

# Console session + Personal scope. Missing bootstrap credentials are a
# missing live edge, not a skipped assertion.
p1_boot_session() {
  ORIGIN="${VOIE_PUBLIC_ORIGIN%/}"
  export VOIE_CONTROL_URL="${VOIE_CONTROL_URL:-$ORIGIN}"
  export VOIE_PUBLIC_ORIGIN="$ORIGIN"
  RUNTIME="${XDG_RUNTIME_DIR:-/tmp}/voie-live-p1"
  install -d -m 700 "$RUNTIME"
  JAR="${RUNTIME}/cookies.txt"
  OUT="${RUNTIME}/body.json"
  bootstrap_admin_login "$ORIGIN" "$JAR"
  PROJECT_ID="$(resolve_personal_scope "$JAR" "$OUT")"
  [ -n "$PROJECT_ID" ] || fail "personal scope resolution returned no id"
}

p1_ready_workspace() {
  if [ -n "${VOIE_LIVE_WORKSPACE_ID:-}" ]; then
    WORKSPACE_ID="${VOIE_LIVE_WORKSPACE_ID}"
    return 0
  fi
  local code
  code="$(api_read "$JAR" "${ORIGIN}/api/projects/${PROJECT_ID}/workspaces" "$OUT")"
  [ "$code" = "200" ] || fail "scope workspaces list HTTP ${code}: $(cat "$OUT")"
  WORKSPACE_ID="$(python3 - "$OUT" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
for item in data.get("items") or []:
    state = str(item.get("state") or "")
    wid = str(item.get("id") or "").strip()
    if wid and state in ("ready", "running"):
        print(wid)
        break
PY
)"
  [ -n "$WORKSPACE_ID" ] || edge "ready Workspace on the live Project for Application create"
}

# Application.create attaches to one Workspace. Starting on a Workspace
# that already has an Application either hands the guest off (quota
# permitting) or returns "application quota reached" (workspace quota 8).
# Opening at quota reuses an occupied Workspace; reclaim leftover P1
# trackers instead. Keep-list slugs (default the C2 live app) stay.
p1_ready_unbound_workspace() {
  if [ -n "${VOIE_LIVE_WORKSPACE_ID:-}" ]; then
    WORKSPACE_ID="${VOIE_LIVE_WORKSPACE_ID}"
    export WORKSPACE_ID
    return 0
  fi
  local attempt bound ws_count
  for attempt in $(seq 1 10); do
    p1_select_unbound_workspace
    if [ -z "$WORKSPACE_ID" ]; then
      ws_count="$(p1_live_workspace_count)"
      # Leave a Workspace slot before create. Leftover C3/C4/C5 Firecracker
      # guests also starve Database create into unknown.
      if [ "${ws_count:-0}" -ge 7 ]; then
        p1_reclaim_one_disposable_tracker ||
          fail "workspace quota full (${ws_count}); no disposable P1 tracker to reclaim"
        continue
      fi
      product_workspace_open "$JAR" "$OUT" "live-p1-$(uuid4 | cut -c1-8)"
    fi
    [ -n "$WORKSPACE_ID" ] || continue
    export WORKSPACE_ID
    bound="$(p1_application_on_workspace "$WORKSPACE_ID")"
    if [ -z "$bound" ]; then
      return 0
    fi
    # Occupied, including 429 reuse of a keep-list Workspace. Never
    # continue into application.create on that guest.
    p1_reclaim_one_disposable_tracker ||
      fail "Workspace ${WORKSPACE_ID} already has Application ${bound}"
    WORKSPACE_ID=""
  done
  fail "no unbound Workspace after reclaiming leftover P1 trackers"
}

p1_live_workspace_count() {
  python3 - "$OUT" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
print(sum(1 for item in (data.get("items") or []) if str(item.get("state") or "") != "deleted"))
PY
}

p1_select_unbound_workspace() {
  local code apps_file probe cand
  code="$(api_read "$JAR" "${ORIGIN}/api/projects/${PROJECT_ID}/workspaces" "$OUT")"
  [ "$code" = "200" ] || fail "scope workspaces list HTTP ${code}: $(cat "$OUT")"
  apps_file="${RUNTIME}/p1-apps.json"
  probe="${RUNTIME}/workspace-probe.json"
  code="$(api_read "$JAR" "${ORIGIN}/api/projects/${PROJECT_ID}/applications" "$apps_file")"
  [ "$code" = "200" ] || fail "project applications list HTTP ${code}: $(cat "$apps_file")"
  WORKSPACE_ID=""
  while read -r cand; do
    [ -n "$cand" ] || continue
    if product_workspace_has_volume "$cand" "$probe"; then
      WORKSPACE_ID="$cand"
      break
    fi
  done <<EOF
$(python3 - "$OUT" "$apps_file" <<'PY'
import json, sys
workspaces = json.load(open(sys.argv[1], encoding="utf-8"))
apps = json.load(open(sys.argv[2], encoding="utf-8"))
bound = {
    str(item.get("workspaceId") or item.get("workspace_id") or "")
    for item in apps.get("items") or []
    if item.get("workspaceId") or item.get("workspace_id")
}
for item in workspaces.get("items") or []:
    wid = str(item.get("id") or "").strip()
    state = str(item.get("state") or "")
    label = str(item.get("label") or "")
    if label == "native-c6":
        continue
    if wid and state in ("ready", "running") and wid not in bound:
        print(wid)
PY
)
EOF
}

p1_application_on_workspace() {
  local workspace_id="$1" apps_file code
  apps_file="${RUNTIME}/p1-apps.json"
  code="$(api_read "$JAR" "${ORIGIN}/api/projects/${PROJECT_ID}/applications" "$apps_file")"
  [ "$code" = "200" ] || fail "project applications list HTTP ${code}: $(cat "$apps_file")"
  python3 - "$apps_file" "$workspace_id" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
want = sys.argv[2]
for item in data.get("items") or []:
    if str(item.get("workspaceId") or item.get("workspace_id") or "") == want:
        print(item.get("id") or "")
        break
PY
}

p1_require_workspace_guest_image() {
  local host="${P1_FABRIC_HOST:-baremetal-1-cs}"
  local images
  images="$(ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
    "k3s kubectl get pod -A -l io.voie/workspace=${WORKSPACE_ID} --no-headers -o custom-columns=IMAGE:.spec.containers[0].image" 2>/dev/null || true)"
  printf '%s' "$images" | grep -q 'voie-workspace:v1' ||
    edge "Workspace ${WORKSPACE_ID} Firecracker image voie-workspace:v1 (got ${images:-none})"
}

p1_json_field() {
  python3 -c 'import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
cur=data
for key in sys.argv[2:]:
    if isinstance(cur, dict):
        cur=cur.get(key)
    else:
        cur=None
        break
if cur is None:
    sys.exit(1)
if isinstance(cur, (dict, list)):
    print(json.dumps(cur))
else:
    print(cur)' "$@"
}

# Empty Agent prompt so the Profile 1 preamble is injected. max_tokens is
# the kernel ceiling (1024).
p1_provision_agent() {
  local agent code
  agent="$(uuid4)"
  # (project_id, name) is unique; a fixed name makes a second live proof 500.
  code="$(api_mutate "$JAR" POST "${ORIGIN}/api/projects/${PROJECT_ID}/agents" \
    "$(python3 -c 'import json,os,sys; print(json.dumps({"id":sys.argv[1],"name":"p1-agent-"+sys.argv[1][:8],"model":os.environ.get("VOIE_MODEL_NAME") or "","systemPrompt":"","bashEnabled":True,"max_tokens":1024}))' "$agent")" \
    "$OUT")"
  [ "$code" = "200" ] || fail "agent create HTTP ${code}: $(cat "$OUT")"
  AGENT_ID="$agent"
  export AGENT_ID
}

p1_load_application() {
  local application_id="$1"
  local code bound
  APPLICATION_ID="$application_id"
  code="$(api_read "$JAR" "${ORIGIN}/api/applications/${APPLICATION_ID}" "$OUT")"
  [ "$code" = "200" ] || fail "application get HTTP ${code}: $(cat "$OUT")"
  SLUG="$(p1_json_field "$OUT" application slug)"
  bound="$(python3 - "$OUT" <<'PY'
import json, sys
print((json.load(open(sys.argv[1], encoding="utf-8")).get("application") or {}).get("workspaceId") or "")
PY
)"
  if [ -n "$bound" ]; then
    WORKSPACE_ID="$bound"
    export WORKSPACE_ID
  fi
  code="$(api_read "$JAR" "${ORIGIN}/api/applications/${APPLICATION_ID}/environments" "$OUT")"
  [ "$code" = "200" ] || fail "environments list HTTP ${code}: $(cat "$OUT")"
  DEV_ENV_ID="$(python3 - "$OUT" <<'PY'
import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
for item in data.get("items") or []:
    if item.get("kind")=="dev":
        print(item["id"]); break
PY
)"
  PROD_ENV_ID="$(python3 - "$OUT" <<'PY'
import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
for item in data.get("items") or []:
    if item.get("kind")=="prod":
        print(item["id"]); break
PY
)"
  DEV_HOST="$(python3 - "$OUT" <<'PY'
import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
for item in data.get("items") or []:
    if item.get("kind")=="dev":
        print(item.get("hostname") or ""); break
PY
)"
  PROD_HOST="$(python3 - "$OUT" <<'PY'
import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
for item in data.get("items") or []:
    if item.get("kind")=="prod":
        print(item.get("hostname") or ""); break
PY
)"
  [ -n "$APPLICATION_ID" ] && [ -n "$DEV_ENV_ID" ] && [ -n "$PROD_ENV_ID" ] ||
    fail "application ${APPLICATION_ID} missing Environments"
}

p1_bind_application_by_slug() {
  local slug="$1" code id
  code="$(api_read "$JAR" "${ORIGIN}/api/projects/${PROJECT_ID}/applications" "$OUT")"
  [ "$code" = "200" ] || fail "project applications list HTTP ${code}: $(cat "$OUT")"
  id="$(python3 - "$OUT" "$slug" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
want = sys.argv[2]
for item in data.get("items") or []:
    if item.get("slug") == want:
        print(item.get("id") or "")
        break
PY
)"
  [ -n "$id" ] || fail "agent run did not create Application slug ${slug}"
  p1_load_application "$id"
}

p1_events_mention() {
  local session_id="$1" needle="$2" events="$3"
  python3 - "$events" "$needle" <<'PY'
import base64, json, sys
path, needle = sys.argv[1], sys.argv[2]
data = json.load(open(path, encoding="utf-8"))
for item in data.get("items") or []:
    try:
        raw = base64.b64decode(item.get("bytes") or "")
    except Exception:
        continue
    if needle.encode() in raw:
        raise SystemExit(0)
    if needle in raw.decode("utf-8", "replace"):
        raise SystemExit(0)
raise SystemExit(1)
PY
}

p1_voie_toml() {
  local postgres="${1:-}"
  python3 - "$postgres" <<'PY'
import sys
postgres = sys.argv[1] == "postgres"
toml = """version = 1
[application]
runtime = "universal-v1"
[build]
command = ["python3", "-c", "print('ok')"]
output = "."
[test]
command = ["python3", "-m", "py_compile", "server.py"]
[run]
command = ["python3", "server.py"]
port = 3000
health_path = "/healthz"
"""
if postgres:
    toml += """
[database]
postgres = true
migration_command = ["python3", "server.py", "migrate"]
"""
sys.stdout.write(toml)
PY
}

# Conversation create -> model tool loop. One tool per turn and the 1024
# token ceiling cannot emit application.create plus a full bash write in
# one model output. Create first; later follow-ups write toml, then
# server.py, then py_compile. The live script does not write guest files.
p1_agent_create_and_test() {
  local slug="$1"
  local name="${2:-P1 C1 tracker}"
  local postgres="${3:-}"
  local session intent run payload code events toml_prompt py_prompt
  p1_provision_agent
  session="$(uuid4)"
  intent="$(uuid4)"
  SESSION_ID="$session"
  export SESSION_ID
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
  [ "$code" = "200" ] || fail "conversation create HTTP ${code}: $(cat "$OUT")"
  prompt_payload="$(python3 - "$intent" "$slug" "$name" <<'PY'
import json, sys
intent, slug, name = sys.argv[1:4]
prompt = (
    "Create an Application on this Workspace with application.create. "
    f"Use slug {slug} and name {name}. Do not write files, pack, deploy, "
    "or call another product tool in this turn. Never print credentials, "
    "DATABASE_URL, or postgres URLs. Do not use Kubernetes, Dockerfiles, "
    "GitHub Actions, or another Project."
)
print(json.dumps({"intentId": intent, "prompt": prompt}))
PY
)"
  code="$(api_mutate "$JAR" POST "${ORIGIN}/api/conversations/${session}/messages" "$prompt_payload" "$OUT")"
  [ "$code" = "200" ] || fail "conversation message HTTP ${code}: $(cat "$OUT")"
  run="$(p1_json_field "$OUT" runId)"
  [ -n "$run" ] || fail "conversation message returned no runId: $(cat "$OUT")"
  if ! await_run_resource "$JAR" "$run" "$OUT" 600; then
    fail "agent run ${run} did not reach terminal: $(cat "$OUT")"
  fi
  events="${RUNTIME}/p1-c1-events.json"
  code="$(api_read "$JAR" "${ORIGIN}/api/sessions/${session}/events" "$events")"
  [ "$code" = "200" ] || fail "session events HTTP ${code}: $(cat "$events")"
  p1_events_mention "$session" "application.create" "$events" ||
    fail "canonical events have no application.create tool call"
  if p1_events_mention "$session" "postgres://" "$events" ||
     p1_events_mention "$session" "DATABASE_URL=" "$events"; then
    fail "canonical events contain a database credential"
  fi
  p1_bind_application_by_slug "$slug"
  toml_prompt="$(P1_TOML="$(p1_voie_toml "$postgres")" python3 - <<'PY'
import os
toml = os.environ["P1_TOML"]
print(
    "With bash write only /workspace/voie.toml and /workspace/marker.txt. "
    "Do not write server.py in this turn. voie.toml must be exactly:\n"
    f"{toml}\n"
    "marker.txt must contain the single word tracker and a newline. "
    "Never print credentials, DATABASE_URL, or postgres URLs. "
    "Do not pack, deploy, test, or use Kubernetes, Dockerfiles, GitHub Actions, "
    "or another Project."
)
PY
)"
  p1_agent_followup "$toml_prompt" "bash"
  py_prompt="$(python3 - "${ROOT}/tests/live/p1-tracker.py" "$postgres" <<'PY'
import sys
tracker = open(sys.argv[1], encoding="utf-8").read()
postgres = sys.argv[2] == "postgres"
db = ""
if postgres:
    db = (
        "DATABASE_URL may be read with psycopg when set; never print it. "
        "python3 server.py migrate must exit 0 without printing credentials. "
    )
print(
    "With bash write only /workspace/server.py from this exact file. "
    "Packed runtime files are under /app; open marker.txt next to server.py, "
    "not /workspace/marker.txt. "
    f"{db}"
    "Do not write voie.toml or marker.txt again. Do not py_compile yet.\n"
    f"{tracker}"
)
PY
)"
  p1_agent_followup "$py_prompt" "bash"
  p1_agent_followup \
    "With bash run python3 -m py_compile /workspace/server.py and stop. Do not pack or deploy. Never print credentials, DATABASE_URL, or postgres URLs." \
    "bash"
  p1_assert_guest_voie_toml "$postgres"
}

p1_agent_followup() {
  local prompt="$1"
  local needle="${2:-}"
  local intent run payload code events
  [ -n "${SESSION_ID:-}" ] || fail "agent follow-up needs SESSION_ID"
  intent="$(uuid4)"
  payload="$(python3 - "$SESSION_ID" "$intent" "$prompt" <<'PY'
import json, sys
print(json.dumps({
    "intentId": sys.argv[2],
    "prompt": sys.argv[3],
}))
PY
)"
  code="$(api_mutate "$JAR" POST "${ORIGIN}/api/conversations/${SESSION_ID}/messages" "$payload" "$OUT")"
  [ "$code" = "200" ] || fail "conversation follow-up HTTP ${code}: $(cat "$OUT")"
  run="$(p1_json_field "$OUT" runId)"
  [ -n "$run" ] || fail "conversation follow-up returned no runId: $(cat "$OUT")"
  if ! await_run_resource "$JAR" "$run" "$OUT" 600; then
    fail "agent follow-up run ${run} did not reach terminal: $(cat "$OUT")"
  fi
  events="${RUNTIME}/p1-followup-events.json"
  code="$(api_read "$JAR" "${ORIGIN}/api/sessions/${SESSION_ID}/events" "$events")"
  [ "$code" = "200" ] || fail "session events HTTP ${code}: $(cat "$events")"
  if [ -n "$needle" ]; then
    p1_events_mention "$SESSION_ID" "$needle" "$events" ||
      fail "canonical events have no ${needle} tool call"
  fi
  if p1_events_mention "$SESSION_ID" "postgres://" "$events" ||
     p1_events_mention "$SESSION_ID" "DATABASE_URL=" "$events"; then
    fail "canonical events contain a database credential"
  fi
}

p1_bind_ready_release() {
  local i code failed previous="${1:-}"
  [ -n "${APPLICATION_ID:-}" ] || fail "ready Release bind needs APPLICATION_ID"
  for i in $(seq 1 300); do
    code="$(api_read "$JAR" "${ORIGIN}/api/applications/${APPLICATION_ID}/releases" "$OUT")"
    [ "$code" = "200" ] || fail "releases list HTTP ${code}: $(cat "$OUT")"
    RELEASE_ID="$(python3 - "$OUT" "$previous" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
previous = sys.argv[2]
for item in reversed(data.get("items") or []):
    if previous and item.get("id") == previous:
        continue
    if item.get("state") == "ready" and item.get("id"):
        print(item["id"])
        break
PY
)"
    RELEASE_HASH="$(python3 - "$OUT" "$previous" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
previous = sys.argv[2]
for item in reversed(data.get("items") or []):
    if previous and item.get("id") == previous:
        continue
    if item.get("state") == "ready" and item.get("id"):
        print(item.get("artifactHash") or "")
        break
PY
)"
    BUILD_INTENT_ID="$(python3 - "$OUT" "$previous" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
previous = sys.argv[2]
for item in reversed(data.get("items") or []):
    if previous and item.get("id") == previous:
        continue
    if item.get("state") == "ready" and item.get("id"):
        print(item.get("buildIntentId") or "")
        break
PY
)"
    failed="$(python3 - "$OUT" "$previous" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
previous = sys.argv[2]
for item in reversed(data.get("items") or []):
    if previous and item.get("id") == previous:
        continue
    state = str(item.get("state") or "")
    if state in ("failed", "unknown"):
        print(state)
        break
PY
)"
    if [ -n "$failed" ]; then
      fail "Release pack became ${failed}: $(cat "$OUT")"
    fi
    if [ -n "$RELEASE_ID" ] && [ -n "$RELEASE_HASH" ]; then
      export BUILD_INTENT_ID
      return 0
    fi
    sleep 2
  done
  fail "Release did not become ready on the live Fabric pack path"
}

p1_agent_build_release() {
  local previous="${RELEASE_ID:-}"
  p1_agent_followup \
    "Pack an immutable Release with release.build from the guest voie.toml. Call it once and stop; do not poll in this turn. Never print credentials, DATABASE_URL, or postgres URLs." \
    "release.build"
  p1_bind_ready_release "$previous"
}

p1_bind_latest_deployment() {
  local env_id="${1:-$DEV_ENV_ID}"
  local i code failed
  [ -n "$env_id" ] || fail "deployment bind needs an Environment id"
  for i in $(seq 1 300); do
    code="$(api_read "$JAR" "${ORIGIN}/api/environments/${env_id}/deployments" "$OUT")"
    [ "$code" = "200" ] || fail "deployments list HTTP ${code}: $(cat "$OUT")"
    DEPLOYMENT_ID="$(python3 - "$OUT" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
items = data.get("items") or []
if not items:
    raise SystemExit(0)
print(items[-1].get("id") or "")
PY
)"
    DEPLOY_INTENT_ID="$(python3 - "$OUT" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
items = data.get("items") or []
if not items:
    raise SystemExit(0)
print(items[-1].get("deploymentIntentId") or "")
PY
)"
    failed="$(python3 - "$OUT" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
items = data.get("items") or []
if not items:
    raise SystemExit(0)
state = str(items[-1].get("state") or "")
if state in ("failed", "unknown"):
    print(state)
PY
)"
    if [ -n "$failed" ]; then
      fail "Deployment became ${failed}: $(cat "$OUT")"
    fi
    if [ -n "$DEPLOYMENT_ID" ]; then
      export DEPLOY_INTENT_ID
      return 0
    fi
    sleep 2
  done
  fail "agent deploy did not create a Deployment"
}

p1_agent_deploy_dev() {
  p1_agent_followup \
    "Materialize the ready Release in private dev with environment.deploy_dev once and stop. Do not call deployment.activate; the human activates after healthy. Do not print credentials, DATABASE_URL, or postgres URLs." \
    "environment.deploy_dev"
  p1_bind_latest_deployment "$DEV_ENV_ID"
}

p1_assert_guest_voie_toml() {
  local postgres="${1:-}"
  local escaped
  escaped="$(p1_read_guest_manifest)"
  python3 - "$escaped" "$postgres" <<'PY' || fail "guest voie.toml is not a packable Application manifest"
import json, sys
raw = json.loads(sys.argv[1])
postgres = sys.argv[2] == "postgres"
text = raw if isinstance(raw, str) else str(raw)
lower = text.lower()
assert "version = 1" in lower, text
assert 'runtime = "universal-v1"' in lower or "runtime = 'universal-v1'" in lower, text
assert "port = 3000" in lower, text
if postgres:
    assert "postgres = true" in lower, text
PY
}

p1_bind_environment_database() {
  local env_id="$1"
  local i code state seen=""
  [ -n "$env_id" ] || fail "environment Database bind needs an Environment id"
  for i in $(seq 1 300); do
    code="$(api_read "$JAR" "${ORIGIN}/api/environments/${env_id}/database" "$OUT")"
    if [ "$code" = "200" ]; then
      seen=1
      p1_assert_no_secrets
      DATABASE_ID="$(p1_json_field "$OUT" database id)"
      state="$(p1_json_field "$OUT" database state)"
      gen="$(p1_json_field "$OUT" database securityProfile)"
      [ -n "$DATABASE_ID" ] || fail "environment Database missing id: $(cat "$OUT")"
      case "$state" in
        ready)
          # Profile 2 is the live postgres role/init contract. Ready at
          # profile 1 still has no tenant Pod to exec.
          [ "$gen" = "2" ] && return 0
          ;;
        failed|unknown|deleted) fail "database ${DATABASE_ID} became ${state}" ;;
      esac
    elif [ "$code" != "404" ]; then
      fail "environment Database get HTTP ${code}: $(cat "$OUT")"
    fi
    sleep 2
  done
  if [ -z "$seen" ]; then
    fail "agent did not create a Database for environment ${env_id}"
  fi
  fail "Database for environment ${env_id} did not become ready on the live Fabric"
}

p1_agent_create_databases() {
  p1_agent_create_dev_database
  p1_agent_followup \
    "Call database.create once with kind=prod and stop. Do not create another Database. Never print credentials, DATABASE_URL, or postgres URLs." \
    "database.create"
  p1_bind_environment_database "$PROD_ENV_ID"
  PROD_DB_ID="$DATABASE_ID"
  [ -n "$DEV_DB_ID" ] && [ -n "$PROD_DB_ID" ] || fail "agent did not create distinct Environment Databases"
}

p1_agent_create_dev_database() {
  p1_agent_followup \
    "Call database.create once with kind=dev and stop. Do not create a prod Database in this turn. Never print credentials, DATABASE_URL, or postgres URLs." \
    "database.create"
  p1_bind_environment_database "$DEV_ENV_ID"
  DEV_DB_ID="$DATABASE_ID"
  [ -n "$DEV_DB_ID" ] || fail "agent did not create the dev Database"
}

p1_environment_deployment_count() {
  local env_id="$1" code
  code="$(api_read "$JAR" "${ORIGIN}/api/environments/${env_id}/deployments" "$OUT")"
  [ "$code" = "200" ] || fail "deployments list HTTP ${code}: $(cat "$OUT")"
  python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1])).get("items") or []))' "$OUT"
}

p1_latest_pending_approval() {
  local kind="$1" code
  code="$(api_read "$JAR" "${ORIGIN}/api/applications/${APPLICATION_ID}/approvals" "$OUT")"
  [ "$code" = "200" ] || fail "approvals list HTTP ${code}: $(cat "$OUT")"
  python3 - "$OUT" "$kind" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
want = sys.argv[2]
for item in data.get("items") or []:
    if item.get("kind") == want and item.get("state") == "pending" and item.get("id"):
        print(item["id"])
        break
PY
}

p1_agent_publish_prod() {
  local before after approval="" code
  [ -n "${RELEASE_ID:-}" ] || fail "publish_prod needs RELEASE_ID"
  before="$(p1_environment_deployment_count "$PROD_ENV_ID")"
  p1_agent_followup \
    "Call environment.publish_prod for ready Release ${RELEASE_ID}. Pass release_id. If approval is required, stop after that call and do not retry or invent an approval. Do not call deployment.activate; the human activates after healthy. Never print credentials, DATABASE_URL, or postgres URLs." \
    "environment.publish_prod"
  after="$(p1_environment_deployment_count "$PROD_ENV_ID")"
  if [ "$after" -le "$before" ]; then
    approval="$(p1_latest_pending_approval publish_production)"
    [ -n "$approval" ] || fail "environment.publish_prod did not request publish_production approval"
    p1_accept_approval "$approval"
    p1_agent_followup \
      "Human accepted approval ${approval}. Call environment.publish_prod again with approval_id ${approval} and release_id ${RELEASE_ID}. Do not call deployment.activate; the human activates after healthy. Never print credentials, DATABASE_URL, or postgres URLs." \
      "environment.publish_prod"
    after="$(p1_environment_deployment_count "$PROD_ENV_ID")"
  fi
  if [ "$after" -le "$before" ]; then
    [ -n "$approval" ] || fail "production Deployment missing after publish_prod"
    p1_deploy "$PROD_ENV_ID" "$approval"
    code="$P1_HTTP_CODE"
    [ "$code" = "202" ] || [ "$code" = "200" ] || fail "approved prod publish HTTP ${code}: $(cat "$OUT")"
  fi
  p1_bind_latest_deployment "$PROD_ENV_ID"
}

p1_create_application() {
  local slug="${1:-p1live}"
  local name="${2:-P1 live}"
  local code
  SLUG="$slug"
  code="$(api_mutate "$JAR" POST "${ORIGIN}/api/projects/${PROJECT_ID}/applications" \
    "$(python3 -c 'import json,sys; print(json.dumps({"name":sys.argv[1],"slug":sys.argv[2],"workspace_id":sys.argv[3],"root_path":"."}))' "$name" "$slug" "$WORKSPACE_ID")" \
    "$OUT")"
  [ "$code" = "201" ] || fail "application.create HTTP ${code}: $(cat "$OUT")"
  APPLICATION_ID="$(p1_json_field "$OUT" application id)"
  DEV_ENV_ID="$(python3 - "$OUT" <<'PY'
import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
for item in data.get("environments") or []:
    if item.get("kind")=="dev":
        print(item["id"]); break
PY
)"
  PROD_ENV_ID="$(python3 - "$OUT" <<'PY'
import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
for item in data.get("environments") or []:
    if item.get("kind")=="prod":
        print(item["id"]); break
PY
)"
  DEV_HOST="$(python3 - "$OUT" <<'PY'
import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
for item in data.get("environments") or []:
    if item.get("kind")=="dev":
        print(item.get("hostname") or ""); break
PY
)"
  PROD_HOST="$(python3 - "$OUT" <<'PY'
import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
for item in data.get("environments") or []:
    if item.get("kind")=="prod":
        print(item.get("hostname") or ""); break
PY
)"
  [ -n "$APPLICATION_ID" ] && [ -n "$DEV_ENV_ID" ] && [ -n "$PROD_ENV_ID" ] ||
    fail "application.create did not return Application and Environments"
  local handoff bound
  handoff="$(python3 - "$OUT" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
value = data.get("workspaceHandoff")
if value:
    print(value)
PY
)"
  if [ -n "$handoff" ]; then
    WORKSPACE_ID="$handoff"
    export WORKSPACE_ID
  else
    bound="$(python3 - "$OUT" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
print((data.get("application") or {}).get("workspaceId") or "")
PY
)"
    if [ -n "$bound" ]; then
      WORKSPACE_ID="$bound"
      export WORKSPACE_ID
    fi
  fi
  p1_require_workspace_guest_image
}

p1_guest_write_tracker() {
  local postgres="${1:-}"
  local command payload code call_id tracker toml
  tracker="$(cat "${ROOT}/tests/live/p1-tracker.py")"
  toml="$(p1_voie_toml "$postgres")"
  command="$(python3 - "$toml" "$tracker" <<'PY'
import json, sys
toml = sys.argv[1]
tracker = sys.argv[2]
inner = (
    "open('/workspace/voie.toml','w').write(%r); "
    "open('/workspace/server.py','w').write(%r); "
    "open('/workspace/marker.txt','w').write('tracker\\n')"
) % (toml, tracker)
print("python3 -c " + json.dumps(inner))
PY
)"
  call_id="$(uuid4)"
  payload="$(python3 -c 'import json,sys; print(json.dumps({"call_id":sys.argv[1],"command":sys.argv[2]}))' "$call_id" "$command")"
  code="$(fabric_rpc POST "/v1/workspaces/${WORKSPACE_ID}/exec" "$payload" "$OUT")"
  [ "$code" = "200" ] || fail "workspace guest write HTTP ${code}: $(cat "$OUT")"
}

p1_guest_test() {
  local payload code op
  op="$(uuid4)"
  payload="$(python3 -c 'import json,sys; print(json.dumps({"operation_id":sys.argv[1],"request_hash":"p1-test-"+sys.argv[1],"relative_root":".","run_argv":["python3","-m","py_compile","server.py"]}))' "$op")"
  code="$(fabric_rpc POST "/v1/workspaces/${WORKSPACE_ID}/guest-run" "$payload" "$OUT")"
  [ "$code" = "200" ] || fail "workspace guest test HTTP ${code}: $(cat "$OUT")"
  [ "$(p1_json_field "$OUT" exitCode)" = "0" ] || fail "workspace guest test failed: $(cat "$OUT")"
}

p1_exec_generation() {
  local code
  code="$(api_read "$JAR" "${ORIGIN}/api/workspaces/${WORKSPACE_ID}" "$OUT")"
  [ "$code" = "200" ] || fail "workspace detail HTTP ${code}: $(cat "$OUT")"
  python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("execGeneration") or 0)' "$OUT"
}

p1_read_guest_manifest() {
  local call_id payload code
  call_id="$(uuid4)"
  payload="$(python3 -c 'import json,sys; print(json.dumps({"call_id":sys.argv[1],"command":"python3 -c " + json.dumps("print(open(\"/workspace/voie.toml\").read(), end=\"\")")}))' "$call_id")"
  code="$(fabric_rpc POST "/v1/workspaces/${WORKSPACE_ID}/exec" "$payload" "$OUT")"
  [ "$code" = "200" ] || fail "guest voie.toml read HTTP ${code}: $(cat "$OUT")"
  python3 - "$OUT" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
text = data.get("stdout") or ""
if not text.strip():
    raise SystemExit("guest voie.toml stdout empty")
print(json.dumps(text))
PY
}

p1_build_release() {
  local intent escaped code generation terminal
  generation="$(p1_exec_generation)"
  [ -n "$generation" ] || fail "workspace execGeneration is missing"
  intent="$(uuid4)"
  escaped="$(p1_read_guest_manifest)"
  [ -n "$escaped" ] || fail "guest voie.toml is missing"
  code="$(api_mutate "$JAR" POST "${ORIGIN}/api/applications/${APPLICATION_ID}/releases" \
    "$(python3 -c 'import json,sys; print(json.dumps({"build_intent_id":sys.argv[1],"workspace_id":sys.argv[2],"source_exec_generation":int(sys.argv[3]),"manifest":json.loads(sys.argv[4])}))' "$intent" "$WORKSPACE_ID" "$generation" "$escaped")" \
    "$OUT")"
  [ "$code" = "202" ] || [ "$code" = "200" ] || fail "release.build HTTP ${code}: $(cat "$OUT")"
  BUILD_INTENT_ID="$intent"
  local i
  for i in $(seq 1 300); do
    code="$(api_read "$JAR" "${ORIGIN}/api/applications/${APPLICATION_ID}/releases" "$OUT")"
    [ "$code" = "200" ] || fail "releases list HTTP ${code}"
    RELEASE_ID="$(python3 - "$OUT" "$intent" <<'PY'
import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
want=sys.argv[2]
for item in data.get("items") or []:
    if item.get("buildIntentId")==want and item.get("state")=="ready":
        print(item["id"]); break
PY
)"
    RELEASE_HASH="$(python3 - "$OUT" "$intent" <<'PY'
import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
want=sys.argv[2]
for item in data.get("items") or []:
    if item.get("buildIntentId")==want and item.get("state")=="ready":
        print(item.get("artifactHash") or ""); break
PY
)"
    terminal="$(python3 - "$OUT" "$intent" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
want = sys.argv[2]
for item in data.get("items") or []:
    if item.get("buildIntentId") == want:
        state = str(item.get("state") or "")
        if state in ("failed", "unknown"):
            print(state)
        break
PY
)"
    if [ -n "$terminal" ]; then
      fail "Release ${intent} became ${terminal}: $(cat "$OUT")"
    fi
    if [ -n "$RELEASE_ID" ]; then
      return 0
    fi
    sleep 2
  done
  fail "Release did not become ready on the live Fabric pack path"
}

p1_deploy() {
  local env_id="$1" approval="${2:-}"
  local intent code body
  intent="$(uuid4)"
  if [ -n "$approval" ]; then
    body="$(python3 -c 'import json,sys; print(json.dumps({"release_id":sys.argv[1],"deployment_intent_id":sys.argv[2],"approval_id":sys.argv[3]}))' "$RELEASE_ID" "$intent" "$approval")"
  else
    body="$(python3 -c 'import json,sys; print(json.dumps({"release_id":sys.argv[1],"deployment_intent_id":sys.argv[2]}))' "$RELEASE_ID" "$intent")"
  fi
  code="$(api_mutate "$JAR" POST "${ORIGIN}/api/environments/${env_id}/deployments" "$body" "$OUT")"
  DEPLOY_INTENT_ID="$intent"
  DEPLOYMENT_ID="$(p1_json_field "$OUT" deploymentId 2>/dev/null || true)"
  P1_HTTP_CODE="$code"
}

p1_deployment_state() {
  local id="$1" code
  code="$(api_read "$JAR" "${ORIGIN}/api/deployments/${id}" "$OUT")"
  [ "$code" = "200" ] || fail "deployment get HTTP ${code}: $(cat "$OUT")"
  p1_json_field "$OUT" deployment state
}

p1_create_database() {
  local env_id="$1" op code
  op="$(uuid4)"
  code="$(api_mutate "$JAR" POST "${ORIGIN}/api/environments/${env_id}/database" \
    "$(python3 -c 'import json,sys; print(json.dumps({"operation_id":sys.argv[1]}))' "$op")" "$OUT")"
  [ "$code" = "202" ] || fail "database create HTTP ${code}: $(cat "$OUT")"
  p1_assert_no_secrets
  p1_json_field "$OUT" database id
}

p1_wait_database_ready() {
  local id="$1"
  local i state gen code
  for i in $(seq 1 300); do
    code="$(api_read "$JAR" "${ORIGIN}/api/databases/${id}" "$OUT")"
    [ "$code" = "200" ] || fail "database get HTTP ${code}: $(cat "$OUT")"
    state="$(p1_json_field "$OUT" database state)"
    gen="$(p1_json_field "$OUT" database securityProfile)"
    case "$state" in
      ready)
        [ "$gen" = "2" ] && return 0
        ;;
      failed|unknown|deleted) fail "database ${id} became ${state}" ;;
    esac
    sleep 2
  done
  fail "Database ${id} did not become ready at securityProfile 2 on the live Fabric"
}

p1_strip_body() {
  python3 -c 'import sys; sys.stdout.write(sys.stdin.read().strip())'
}

p1_fetch_app_body() {
  local url="$1"
  shift
  curl -sS --max-time 20 "$@" "$url" 2>/dev/null | p1_strip_body || true
}

p1_authenticated_preview() {
  local host="${1:-$DEV_HOST}"
  local env_id="${2:-$DEV_ENV_ID}"
  local expected="${3:-tracker}"
  local code redirect cookie body
  [ -n "$host" ] || fail "preview host is empty"
  code="$(api_read "$JAR" \
    "${ORIGIN}/api/preview/login?applicationId=${APPLICATION_ID}&environmentId=${env_id}" "$OUT")"
  [ "$code" = "200" ] || fail "preview login HTTP ${code}: $(cat "$OUT")"
  redirect="$(p1_json_field "$OUT" redirect)"
  [ -n "$redirect" ] || fail "preview login missing redirect"
  cookie="$(curl -sS -D - -o /dev/null --max-time 20 "$redirect" \
    | python3 -c 'import sys
for line in sys.stdin:
    if line.lower().startswith("set-cookie:"):
        value=line.split(":",1)[1].strip().split(";",1)[0]
        if value.startswith("__Host-voie-preview="):
            print(value)
            break
')"
  [ -n "$cookie" ] || edge "preview callback cookie for ${host}"
  body=""
  for _ in $(seq 1 45); do
    body="$(p1_fetch_app_body "https://${host}/" -H "Cookie: ${cookie}")"
    [ "$body" = "$expected" ] && return 0
    sleep 2
  done
  if [ -z "$body" ]; then
    fail "authenticated preview body from ${host} was empty"
  fi
  [ "$body" = "$expected" ] || fail "authenticated preview body '${body}', want '${expected}'"
}

p1_guest_scan_no_secrets() {
  local command payload code call_id stdout
  command="$(python3 - "${P1_SECRET_NEEDLES:-}" <<'PY'
import base64, json, sys
extra = [part for part in sys.argv[1].split(",") if part]
needles = ["postgres://", "DATABASE_URL=", "POSTGRES_PASSWORD"] + extra
script = """
import os, pathlib
needles = %s
hits = []
for path in pathlib.Path("/workspace").rglob("*"):
    if not path.is_file():
        continue
    try:
        text = path.read_text(errors="ignore")
    except Exception:
        continue
    for needle in needles:
        if needle in text:
            hits.append(str(path))
            break
env = " ".join("%%s=%%s" %% (k, os.environ.get(k, "")) for k in os.environ)
for needle in needles:
    if needle.strip("=") in env or needle in env:
        hits.append("env")
        break
print("clean" if not hits else "hit:" + ",".join(hits[:8]))
""" % (json.dumps(needles),)
payload = base64.b64encode(script.encode()).decode()
print("python3 -c " + json.dumps("import base64; exec(base64.b64decode('%s'))" % payload))
PY
)"
  call_id="$(uuid4)"
  payload="$(python3 -c 'import json,sys; print(json.dumps({"call_id":sys.argv[1],"command":sys.argv[2]}))' "$call_id" "$command")"
  code="$(fabric_rpc POST "/v1/workspaces/${WORKSPACE_ID}/exec" "$payload" "$OUT")"
  [ "$code" = "200" ] || fail "workspace secret scan HTTP ${code}: $(cat "$OUT")"
  stdout="$(p1_json_field "$OUT" stdout 2>/dev/null || true)"
  case "$stdout" in
    clean) ;;
    hit:*) fail "prod credential entered the Workspace: ${stdout}" ;;
    *) fail "workspace secret scan did not complete: $(cat "$OUT")" ;;
  esac
}

p1_scan_conversations_no_secrets() {
  local code
  code="$(api_read "$JAR" "${ORIGIN}/api/workspaces/${WORKSPACE_ID}/conversations" "$OUT" || true)"
  case "$code" in
    200) ;;
    404) return 0 ;;
    *) fail "conversation index HTTP ${code}: $(cat "$OUT")" ;;
  esac
  if grep -qiE 'postgres://|DATABASE_URL=|POSTGRES_PASSWORD' "$OUT"; then
    fail "conversation index contained database credentials"
  fi
  if [ -n "${P1_SECRET_NEEDLES:-}" ] && grep -F -q "$P1_SECRET_NEEDLES" "$OUT"; then
    fail "conversation index contained Environment secret material"
  fi
}

p1_bind_prod_secret() {
  local marker="$1"
  local code secret_id approval_id
  [ -n "$marker" ] || fail "prod secret marker is empty"
  code="$(api_mutate "$JAR" POST "${ORIGIN}/api/projects/${PROJECT_ID}/secrets" \
    "$(python3 -c 'import json,sys,uuid; print(json.dumps({"name":"p1-prod-marker-"+uuid.uuid4().hex[:8],"value":sys.argv[1]}))' "$marker")" \
    "$OUT")"
  [ "$code" = "200" ] || fail "secret create HTTP ${code}: $(cat "$OUT")"
  printf '%s' "$(cat "$OUT")" | grep -F -q "$marker" && fail "secret create echoed material"
  secret_id="$(p1_json_field "$OUT" secret id)"
  [ -n "$secret_id" ] || fail "secret create missing id"
  code="$(api_mutate "$JAR" PUT "${ORIGIN}/api/environments/${PROD_ENV_ID}/secret-bindings/P1_C3_MARKER" \
    "$(python3 -c 'import json,sys; print(json.dumps({"secret_id":sys.argv[1]}))' "$secret_id")" \
    "$OUT")"
  [ "$code" = "409" ] || fail "prod binding without approval HTTP ${code}: $(cat "$OUT")"
  approval_id="$(p1_json_field "$OUT" approvalId)"
  p1_accept_approval "$approval_id"
  code="$(api_mutate "$JAR" PUT "${ORIGIN}/api/environments/${PROD_ENV_ID}/secret-bindings/P1_C3_MARKER" \
    "$(python3 -c 'import json,sys; print(json.dumps({"secret_id":sys.argv[1],"approval_id":sys.argv[2]}))' "$secret_id" "$approval_id")" \
    "$OUT")"
  [ "$code" = "200" ] || fail "prod binding after approval HTTP ${code}: $(cat "$OUT")"
  printf '%s' "$(cat "$OUT")" | grep -F -q "$marker" && fail "binding response contained material"
  code="$(api_read "$JAR" "${ORIGIN}/api/environments/${PROD_ENV_ID}/secret-bindings" "$OUT")"
  [ "$code" = "200" ] || fail "prod bindings list HTTP ${code}: $(cat "$OUT")"
  printf '%s' "$(cat "$OUT")" | grep -F -q "$marker" && fail "prod bindings list contained material"
  python3 - "$OUT" <<'PY' || fail "prod bindings list missing P1_C3_MARKER metadata"
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
items = data.get("items") or []
assert len(items) == 1, items
assert items[0].get("name") == "P1_C3_MARKER", items
assert "value" not in items[0], items
PY
  code="$(api_read "$JAR" "${ORIGIN}/api/environments/${DEV_ENV_ID}/secret-bindings" "$OUT")"
  [ "$code" = "200" ] || fail "dev bindings list HTTP ${code}: $(cat "$OUT")"
  python3 - "$OUT" <<'PY' || fail "prod secret leaked onto the dev Environment"
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
assert len(data.get("items") or []) == 0, data
PY
}

p1_wait_healthy() {
  local id="${1:-$DEPLOYMENT_ID}"
  local i state observed code
  for i in $(seq 1 300); do
    code="$(api_read "$JAR" "${ORIGIN}/api/deployments/${id}" "$OUT")"
    [ "$code" = "200" ] || fail "deployment get HTTP ${code}: $(cat "$OUT")"
    state="$(p1_json_field "$OUT" deployment state)"
    observed="$(p1_json_field "$OUT" deployment observedState 2>/dev/null || true)"
    case "$observed" in
      lost | failed) fail "deployment ${id} observed ${observed}" ;;
      needs_release_stream)
        sleep 2
        continue
        ;;
    esac
    case "$state" in
      healthy | active)
        case "$observed" in
          "" | healthy | active | running) return 0 ;;
        esac
        ;;
      failed | unknown | stopped) fail "deployment ${id} became ${state}" ;;
    esac
    sleep 2
  done
  fail "Deployment ${id} did not become healthy on the live Fabric"
}

# Activate after p1_wait_healthy. 409 is retryable (voie-gateway or app
# Pod not Ready yet, or public Caddy/DNS/TLS lag). Other statuses fail closed.
# Already-active is HTTP 200. Do not use this when the proof expects 409.
p1_activate_healthy() {
  local deployment_id="${1:-$DEPLOYMENT_ID}"
  local i http
  [ -n "$deployment_id" ] || fail "activate needs a Deployment id"
  for i in $(seq 1 6); do
    http="$(api_mutate "$JAR" POST "${ORIGIN}/api/deployments/${deployment_id}/activate" "{}" "$OUT")"
    case "$http" in
      200) return 0 ;;
      409)
        sleep 5
        ;;
      *)
        fail "activate healthy HTTP ${http}: $(cat "$OUT")"
        ;;
    esac
  done
  fail "activate stayed 409 for ${deployment_id}: $(cat "$OUT")"
}

p1_wait_public_body() {
  local url="$1" expected="$2"
  local body="" i
  for i in $(seq 1 45); do
    body="$(p1_fetch_app_body "$url")"
    [ "$body" = "$expected" ] && return 0
    sleep 2
  done
  if [ -z "$body" ]; then
    fail "body from ${url} was empty"
  fi
  fail "body from ${url} was '${body}', want '${expected}'"
}

p1_rollback() {
  local deployment_id="$1" approval="${2:-}"
  local intent code body
  intent="$(uuid4)"
  if [ -n "$approval" ]; then
    body="$(python3 -c 'import json,sys; print(json.dumps({"deployment_intent_id":sys.argv[1],"approval_id":sys.argv[2]}))' "$intent" "$approval")"
  else
    body="$(python3 -c 'import json,sys; print(json.dumps({"deployment_intent_id":sys.argv[1]}))' "$intent")"
  fi
  code="$(api_mutate "$JAR" POST "${ORIGIN}/api/deployments/${deployment_id}/rollback" "$body" "$OUT")"
  DEPLOYMENT_ID="$(p1_json_field "$OUT" deploymentId 2>/dev/null || true)"
  P1_HTTP_CODE="$code"
}

p1_delete_application() {
  local code approval_id
  # plan_delete refuses while a prod Environment still points at an active
  # Deployment (WorkspaceBusy). Suspend stops Deployments and clears that
  # pointer first. It does not delete Databases, Releases, or the Workspace.
  code="$(api_mutate "$JAR" PATCH "${ORIGIN}/api/applications/${APPLICATION_ID}" \
    '{"state":"suspended"}' "$OUT")"
  [ "$code" = "200" ] || fail "suspend before delete HTTP ${code}: $(cat "$OUT")"
  code="$(api_mutate "$JAR" DELETE "${ORIGIN}/api/applications/${APPLICATION_ID}" "{}" "$OUT")"
  [ "$code" = "409" ] || fail "delete without approval HTTP ${code}: $(cat "$OUT")"
  approval_id="$(p1_json_field "$OUT" approvalId)"
  p1_accept_approval "$approval_id"
  code="$(api_mutate "$JAR" DELETE "${ORIGIN}/api/applications/${APPLICATION_ID}" \
    "$(python3 -c 'import json,sys; print(json.dumps({"approvalId":sys.argv[1]}))' "$approval_id")" "$OUT")"
  [ "$code" = "204" ] || fail "delete after approval HTTP ${code}: $(cat "$OUT")"
}

# Oldest leftover P1 C1–C5 tracker that is not a keep-list slug. Native-c6
# has no Application. Returns 1 when nothing reclaimable remains.
p1_reclaim_one_disposable_tracker() {
  local apps_file code id saved
  apps_file="${RUNTIME}/p1-apps.json"
  code="$(api_read "$JAR" "${ORIGIN}/api/projects/${PROJECT_ID}/applications" "$apps_file")"
  [ "$code" = "200" ] || fail "project applications list HTTP ${code}: $(cat "$apps_file")"
  id="$(VOIE_P1_KEEP_SLUGS="${VOIE_P1_KEEP_SLUGS:-p1c2f1f0e4e7}" python3 - "$apps_file" <<'PY'
import json, os, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
keep = {part.strip() for part in os.environ.get("VOIE_P1_KEEP_SLUGS", "").split(",") if part.strip()}
candidates = []
for item in data.get("items") or []:
    slug = str(item.get("slug") or "")
    name = str(item.get("name") or "")
    if slug in keep:
        continue
    is_p1 = name.startswith("P1 C") and name.endswith(" tracker")
    is_ds_loss = name.startswith("DS loss") or slug.startswith("dsloss")
    if not is_p1 and not is_ds_loss:
        continue
    # Extra C3/C4/C5 guests and leftover loss-demo apps first; keep C1/C2
    # for two-Application egress.
    group = 0 if (
        name.startswith(("P1 C3 ", "P1 C4 ", "P1 C5 ")) or is_ds_loss
    ) else 1
    candidates.append((group, str(item.get("createdAt") or ""), str(item.get("id") or "")))
candidates.sort()
if candidates and candidates[0][2]:
    print(candidates[0][2])
PY
)"
  [ -n "$id" ] || return 1
  printf 'p1: reclaiming leftover Application %s to free Workspace quota\n' "$id"
  saved="${APPLICATION_ID:-}"
  APPLICATION_ID="$id"
  p1_delete_application
  APPLICATION_ID="$saved"
}

p1_assert_migrate_not_replayed() {
  local deployment_id="$1" environment_id="$2" release_id="$3" kind="$4"
  local op hash revision payload code state intent
  code="$(api_read "$JAR" "${ORIGIN}/api/deployments/${deployment_id}" "$OUT")"
  [ "$code" = "200" ] || fail "deployment get for migrate replay HTTP ${code}: $(cat "$OUT")"
  revision="$(p1_json_field "$OUT" deployment desiredRevision)"
  intent="$(p1_json_field "$OUT" deployment deploymentIntentId)"
  [ -n "$intent" ] || fail "deployment missing deploymentIntentId for migrate replay"
  op="$(python3 - "$deployment_id" <<'PY'
import hashlib, sys, uuid
dep = uuid.UUID(sys.argv[1])
digest = hashlib.sha256(b"voie-migrate:" + dep.bytes).digest()
print(uuid.UUID(bytes=digest[:16]))
PY
)"
  hash="$(python3 - "$environment_id" "$release_id" "$kind" "$intent" <<'PY'
import hashlib, sys, uuid
env = uuid.UUID(sys.argv[1])
rel = uuid.UUID(sys.argv[2])
kind = sys.argv[3].encode()
intent = uuid.UUID(sys.argv[4])
digest = hashlib.sha256()
digest.update(env.bytes)
digest.update(rel.bytes)
digest.update(kind)
digest.update(intent.bytes)
print("migrate:" + digest.hexdigest())
PY
)"
  payload="$(python3 -c 'import json,sys; print(json.dumps({"operation_id":sys.argv[1],"request_hash":sys.argv[2],"desired_revision":int(sys.argv[3]),"run_argv":["true"],"migrate_argv":["python3","server.py","migrate"]}))' "$op" "$hash" "$revision")"
  code="$(fabric_rpc POST "/v1/deployments/${deployment_id}/migrate" "$payload" "$OUT")"
  state="$(p1_json_field "$OUT" state 2>/dev/null || true)"
  case "$code" in
    200)
      [ "$state" != "dispatched" ] || fail "migrate replay dispatched a second run: $(cat "$OUT")"
      ;;
    409) fail "migrate replay hash conflict; journaled request hash must match: $(cat "$OUT")" ;;
    202) fail "unknown migrate was dispatched a second time: $(cat "$OUT")" ;;
    *) fail "migrate replay HTTP ${code}: $(cat "$OUT")" ;;
  esac
}

p1_accept_approval() {
  local approval_id="$1" code
  code="$(api_mutate "$JAR" POST "${ORIGIN}/api/approvals/${approval_id}/accept" "{}" "$OUT")"
  [ "$code" = "200" ] || fail "approval accept HTTP ${code}: $(cat "$OUT")"
}

p1_assert_no_secrets() {
  local text
  text="$(cat "$OUT")"
  # `grep && fail` as the last command of a function returns 1 when the
  # body is clean, and `set -e` then aborts the caller before bind/wait.
  if printf '%s' "$text" | grep -qi 'postgres://'; then
    fail "response contained a postgres URL"
  fi
  if printf '%s' "$text" | grep -qi 'password'; then
    fail "response contained password material"
  fi
  if printf '%s' "$text" | grep -q 'DATABASE_URL'; then
    fail "response contained DATABASE_URL"
  fi
  return 0
}

p1_assert_canary_quiet() {
  local canary="$1"
  if [ -f "${canary}/executed" ]; then
    fail "host bash canary fired; a project command ran on the control or Fabric client host"
  fi
}

p1_guest_psql_file() {
  local ns="$1" name="$2" remote="$3"
  local host="${P1_FABRIC_HOST:-baremetal-1-cs}"
  ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
    "k3s kubectl exec -i -n $(printf '%q' "$ns") $(printf '%q' "$name") -c postgres --request-timeout 45s -- /bin/sh" \
    <"$remote"
}

p1_postgres_pod() {
  local db_id="$1"
  local host="${P1_FABRIC_HOST:-baremetal-1-cs}"
  local i pod
  # After restore the live Pod is voie-pg-rst-{op}, not voie-pg-{id}.
  # A Ready create Pod can still exist while restore is Pending; wait for
  # the restore Pod and never exec the replaced volume.
  for i in $(seq 1 180); do
    pod="$(ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
      "k3s kubectl get pod -A -l io.voie/database=${db_id} -o json" \
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
')"
    if [[ "$pod" == */* && "$pod" != "/" ]]; then
      printf '%s\n' "$pod"
      return 0
    fi
    sleep 2
  done
  fail "postgres pod for ${db_id} is missing"
}

p1_wait_restore_pod() {
  local db_id="$1"
  local op="$2"
  local host="${P1_FABRIC_HOST:-baremetal-1-cs}"
  local compact i pod
  compact="$(printf '%s' "$op" | tr -d '-')"
  [ -n "$compact" ] || fail "restore operation id is missing"
  for i in $(seq 1 180); do
    pod="$(ssh -o BatchMode=yes -o ConnectTimeout=8 "$host" \
      "k3s kubectl get pod -A -l io.voie/database=${db_id} -o json" \
      | python3 -c '
import json, sys
want = "voie-pg-rst-" + sys.argv[1]
items = json.load(sys.stdin).get("items") or []
for item in items:
    meta = item.get("metadata") or {}
    ns = str(meta.get("namespace") or "")
    name = str(meta.get("name") or "")
    if name != want or not ns:
        continue
    conds = (item.get("status") or {}).get("conditions") or []
    if any(c.get("type") == "Ready" and c.get("status") == "True" for c in conds):
        print("%s/%s" % (ns, name))
        break
' "$compact")"
    if [[ "$pod" == */* && "$pod" != "/" ]]; then
      printf '%s\n' "$pod"
      return 0
    fi
    sleep 2
  done
  fail "restore postgres pod voie-pg-rst-${compact} for ${db_id} did not become Ready"
}

p1_assert_tenant_postgres_role() {
  local db_id="$1"
  local pod ns name flags platform copy_rc
  local role_sql platform_sql copy_sql tenant_sql
  pod="$(p1_postgres_pod "$db_id")"
  ns="${pod%%/*}"
  name="${pod#*/}"
  role_sql="${RUNTIME}/p1-pg-role.sh"
  platform_sql="${RUNTIME}/p1-pg-platform.sh"
  copy_sql="${RUNTIME}/p1-pg-copy.sh"
  tenant_sql="${RUNTIME}/p1-pg-tenant.sh"
  cat >"$role_sql" <<'EOS'
set -eu
PGPASSWORD=$(cat /run/voie/postgres-password)
export PGPASSWORD
exec /bin/psql -U app -h 127.0.0.1 -d app -Atc "SELECT CASE WHEN rolsuper THEN 't' ELSE 'f' END||','||CASE WHEN rolcreatedb THEN 't' ELSE 'f' END||','||CASE WHEN rolcreaterole THEN 't' ELSE 'f' END||','||CASE WHEN rolreplication THEN 't' ELSE 'f' END||','||CASE WHEN rolbypassrls THEN 't' ELSE 'f' END FROM pg_roles WHERE rolname='app'"
EOS
  cat >"$platform_sql" <<'EOS'
set -eu
PGPASSWORD=$(cat /run/voie/postgres-password)
export PGPASSWORD
exec /bin/psql -U app -h 127.0.0.1 -d app -Atc "SELECT CASE WHEN rolcanlogin THEN 't' ELSE 'f' END FROM pg_roles WHERE rolname='voie_platform'"
EOS
  cat >"$copy_sql" <<'EOS'
set -eu
PGPASSWORD=$(cat /run/voie/postgres-password)
export PGPASSWORD
exec /bin/psql -U app -h 127.0.0.1 -d app -c "COPY (SELECT 1) TO PROGRAM 'true'"
EOS
  cat >"$tenant_sql" <<'EOS'
set -eu
PGPASSWORD=$(cat /run/voie/postgres-password)
export PGPASSWORD
exec /bin/psql -U app -h 127.0.0.1 -d app -Atc "SELECT 1"
EOS
  flags="$(p1_guest_psql_file "$ns" "$name" "$role_sql" | tr -d '[:space:]')"
  [ "$flags" = "f,f,f,f,f" ] || fail "tenant app role flags for ${db_id} are ${flags}, want f,f,f,f,f"
  platform="$(p1_guest_psql_file "$ns" "$name" "$platform_sql" | tr -d '[:space:]')"
  [ "$platform" = "f" ] || fail "voie_platform.rolcanlogin for ${db_id} is ${platform}"
  copy_rc=0
  p1_guest_psql_file "$ns" "$name" "$copy_sql" >/dev/null 2>&1 || copy_rc=$?
  [ "$copy_rc" -ne 0 ] || fail "COPY ... PROGRAM succeeded for tenant app on ${db_id}"
  [ "$(p1_guest_psql_file "$ns" "$name" "$tenant_sql" | tr -d '[:space:]')" = "1" ] ||
    fail "tenant SQL failed for ${db_id}"
  ssh -o BatchMode=yes -o ConnectTimeout=8 "${P1_FABRIC_HOST:-baremetal-1-cs}" \
    "k3s kubectl exec -n ${ns} ${name} -c postgres -- test ! -e /tmp/voie-postgres-password" ||
    fail "/tmp/voie-postgres-password still present in ${db_id}"
}

p1_pg_at() {
  local db_id="$1" sql="$2"
  local pod ns name script
  pod="$(p1_postgres_pod "$db_id")"
  ns="${pod%%/*}"
  name="${pod#*/}"
  script="${RUNTIME}/p1-pg-at.sh"
  cat >"$script" <<EOS
set -eu
PGPASSWORD=\$(cat /run/voie/postgres-password)
export PGPASSWORD
exec /bin/psql -U app -h 127.0.0.1 -d app -Atc $(printf '%q' "$sql")
EOS
  p1_guest_psql_file "$ns" "$name" "$script"
}

p1_wait_backup() {
  local db_id="$1" before="$2"
  local i code count
  for i in $(seq 1 180); do
    code="$(api_read "$JAR" "${ORIGIN}/api/databases/${db_id}/backups" "$OUT")"
    [ "$code" = "200" ] || fail "backup list HTTP ${code}: $(cat "$OUT")"
    count="$(python3 - "$OUT" <<'PY'
import json,sys
print(len(json.load(open(sys.argv[1], encoding="utf-8")).get("items") or []))
PY
)"
    if [ "$count" -gt "$before" ]; then
      python3 - "$OUT" <<'PY'
import json,sys
items=json.load(open(sys.argv[1], encoding="utf-8")).get("items") or []
print(items[0]["id"])
PY
      return 0
    fi
    sleep 2
  done
  fail "database ${db_id} backup did not appear"
}

p1_backup_database() {
  local db_id="$1"
  local before code
  code="$(api_read "$JAR" "${ORIGIN}/api/databases/${db_id}/backups" "$OUT")"
  [ "$code" = "200" ] || fail "backup list HTTP ${code}: $(cat "$OUT")"
  before="$(python3 - "$OUT" <<'PY'
import json,sys
print(len(json.load(open(sys.argv[1], encoding="utf-8")).get("items") or []))
PY
)"
  code="$(api_mutate "$JAR" POST "${ORIGIN}/api/databases/${db_id}/backups" '{}' "$OUT")"
  [ "$code" = "202" ] || fail "database backup HTTP ${code}: $(cat "$OUT")"
  p1_assert_no_secrets
  p1_wait_backup "$db_id" "$before"
}

p1_restore_database() {
  local db_id="$1" backup_id="$2"
  local op code approval
  op="$(uuid4)"
  code="$(api_mutate "$JAR" POST "${ORIGIN}/api/databases/${db_id}/restores" \
    "$(python3 -c 'import json,sys; print(json.dumps({"backup_id":sys.argv[1],"operation_id":sys.argv[2]}))' "$backup_id" "$op")" \
    "$OUT")"
  if [ "$code" = "409" ] || [ "$code" = "403" ] || [ "$code" = "401" ]; then
    approval="$(python3 - "$OUT" <<'PY'
import json,sys
data=json.load(open(sys.argv[1], encoding="utf-8"))
print(data.get("approvalId") or "")
PY
)"
    [ -n "$approval" ] || fail "restore did not return approvalId: $(cat "$OUT")"
    p1_accept_approval "$approval"
    op="$(uuid4)"
    code="$(api_mutate "$JAR" POST "${ORIGIN}/api/databases/${db_id}/restores" \
      "$(python3 -c 'import json,sys; print(json.dumps({"backup_id":sys.argv[1],"operation_id":sys.argv[2],"approval_id":sys.argv[3]}))' "$backup_id" "$op" "$approval")" \
      "$OUT")"
  fi
  [ "$code" = "202" ] || fail "database restore HTTP ${code}: $(cat "$OUT")"
  p1_wait_restore_pod "$db_id" "$op"
  p1_wait_database_ready "$db_id"
}

