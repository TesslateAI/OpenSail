#!/usr/bin/env bash
# Shared helpers for integration-1 live checkpoint scripts. Sourced, never
# executed. Transport/process scaffolding adapted from cursor/main tests/live
# (commit 245d334e); all product shapes are native REST, canonical Blob-backed
# events, mTLS Fabric, and the native bootstrap-admin or OIDC Web-session boot.
#
# Exit codes: 2 = missing live edge, 1 = real assertion failure.

edge() { echo "missing live edge: $*" >&2; exit 2; }
fail() { echo "live proof failed: $*" >&2; exit 1; }

# Batched require of non-empty environment values; reports every miss.
# Adapted from the source commit's need() helper.
require_env() {
  local missing=() name
  for name in "$@"; do
    [ -n "${!name:-}" ] || missing+=("$name")
  done
  if [ "${#missing[@]}" -gt 0 ]; then
    printf 'required environment values are missing:\n' >&2
    printf '  %s\n' "${missing[@]}" >&2
    return 1
  fi
}

# Imported verbatim from the source commit: read a nested JSON field.
json_field() {
  python3 -c 'import json,sys; value=json.load(sys.stdin)
for key in sys.argv[1:]:
    value=value[key]
print(value)' "$@"
}

uuid4() { python3 -c 'import uuid; print(uuid.uuid4())'; }

# Loopback address literals, assembled from octal escapes so no sanitized
# address token ever rides in this file and no glob brackets distort them.
LOOPBACK_IPV4="$(printf '\061\62\67\56\60\56\60\56\61')"
LOOPBACK_IPV6_BRACKETED="[$(printf '\72\72\61')]"
LOOPBACK_IPV6_BARE="$(printf '\72\72\61')"

# Host-local bash canary: PATH shadows bash with a recorder; any host-side
# Bash execution betrays itself via the marker file.
install_host_canary() {
  install -d "$1"
  cat >"$1/bash" <<EOF
#!/bin/sh
echo executed > '$1/executed'
exit 0
EOF
  chmod 755 "$1/bash"
}

# Internal: export KEY=VALUE lines from one env file, filling only vars
# that are not already set. Handles optional leading "export " and skips
# comments/blank lines. Symlinks are refused.
_load_env_file() {
  local file="$1"
  [ -f "$file" ] || return 0
  [ ! -L "$file" ] || return 0
  local line key value
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      ""|\#*) continue ;;
    esac
    case "$line" in
      "export "*) line="${line#export }" ;;
    esac
    case "$line" in
      [A-Za-z_]*"="*) ;;
      *) continue ;;
    esac
    key="${line%%=*}"
    value="${line#*=}"
    case "$key" in
      [A-Za-z_][A-Za-z0-9_]* ) ;;
      *) continue ;;
    esac
    # Never override an explicit value from the caller.
    eval "[ -n \"\${$key+set}\" ]" && continue
    # Optionally strip one layer of surrounding double quotes.
    case "$value" in
      \"*\" ) value="${value#\"}"; value="${value%\"}" ;;
    esac
    # Optionally strip single quotes.
    case "$value" in
      \'*\' ) value="${value#\'}"; value="${value%\'}" ;;
    esac
    if printf -v _probe test 2>/dev/null; then
      printf -v "$key" "%s" "$value"
      export "$key"
    else
      # shellcheck disable=SC2163
      export "$key=$value"
    fi
  done <"$file"
}

# Fill missing live-boundary environment from the documented local stack
# state when it exists. Never overrides explicit caller values.
# Normalizes the Azure Blob endpoint from *.blob.localhost to loopback
# when the name is unroutable, mirroring dev-stack/up.sh.
load_local_stack_env() {
  local runtime_base="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
  local stack_env="$runtime_base/voie-dev-stack/stack.env"
  local dev_env="$runtime_base/voie-dev-cloud/env"
  _load_env_file "$dev_env"
  _load_env_file "$stack_env"
  if [ -z "${VOIE_DATABASE_URL:-}" ] && [ -x "${ROOT:-.}/dev-cloud/local-stack.sh" ]; then
    local discovered=""
    discovered="$("${ROOT}/dev-cloud/local-stack.sh" env 2>/dev/null || true)"
    [ -n "$discovered" ] && _load_env_file "$discovered"
  fi
  if [ -n "${VOIE_AZURE_BLOB_ENDPOINT:-}" ] && [ -n "${VOIE_AZURE_BLOB_CONTAINER:-}" ]; then
    local blob_host="${VOIE_AZURE_BLOB_ENDPOINT#http://}"
    blob_host="${blob_host#https://}"
    blob_host="${blob_host%%:*}"
    blob_host="${blob_host%%/*}"
    case "$blob_host" in
      *.localhost)
        if ! curl --fail --silent "${VOIE_AZURE_BLOB_ENDPOINT}/${VOIE_AZURE_BLOB_CONTAINER}?restype=container" >/dev/null 2>&1; then
          local suffix="${VOIE_AZURE_BLOB_ENDPOINT##*:}"
          export VOIE_AZURE_BLOB_ENDPOINT="http://127.0.0.1:${suffix}"
        fi
        ;;
    esac
  fi
}

# Refuse the deterministic fixture model for checkpoints that require a
# real provider. The dev stack exports VOIE_FIXTURE_MODEL when no real
# provider is configured; C4/C6 must never pass against it.
refuse_fixture_model() {
  local label="${1:-live}"
  if [ "${VOIE_FIXTURE_MODEL:-}" = "1" ] || [ -n "${VOIE_DEV_FIXTURE_MODEL_URL:-}" ]; then
    edge "${label} requires a real model provider; fixture mode (VOIE_FIXTURE_MODEL=1) is not accepted — configure VOIE_MODEL_BASE_URL and VOIE_MODEL_API_KEY to a real provider"
  fi
}

# Spawn voie-cloud and wait for readiness; sets CLOUD_PID, needs $ORIGIN.
await_cloud_ready() {
  local log="$1"
  ./target/debug/voie-cloud >>"$log" 2>&1 &
  CLOUD_PID=$!
  for _ in $(seq 1 80); do
    if curl -sf "${ORIGIN}/readyz" >/dev/null; then
      return 0
    fi
    if ! kill -0 "$CLOUD_PID" 2>/dev/null; then
      edge "voie-cloud exited before ready (${log})"
    fi
    sleep 0.1
  done
  edge "voie-cloud did not become ready on ${ORIGIN}"
}

# True when VOIE_CONTROL_URL already points at a deployed control. Origin-mode
# live-c3/c4/c5 drive that process instead of spawning a second writer against
# live PostgreSQL (one writer; see Production Profile 0).
live_origin_mode() {
  [ -n "${VOIE_CONTROL_URL:-}" ]
}

# Restart the live voie-cloud systemd unit and wait until ORIGIN/healthz
# recovers. Origin-mode C3/C5 use this in place of killing a local process.
restart_origin_control() {
  local origin="${1%/}"
  local service="${VOIE_CONTROL_SERVICE:-voie-cloud}"
  [ -n "${VOIE_CONTROL_SSH:-}" ] ||
    edge "VOIE_CONTROL_SSH (origin mode restarts the live ${service} unit)"
  local ssh=(ssh -o BatchMode=yes -o ConnectTimeout=8)
  if [ -n "${VOIE_SSH_PRIVATE_KEY:-}" ]; then
    ssh+=(-i "${VOIE_SSH_PRIVATE_KEY}" -o IdentitiesOnly=yes)
  fi
  "${ssh[@]}" "$VOIE_CONTROL_SSH" "sudo systemctl restart ${service}" ||
    edge "restart ${service} over ssh"
  local n=0
  while [ "$n" -lt 180 ]; do
    if curl -sf --max-time 3 "${origin}/healthz" >/dev/null; then
      return 0
    fi
    n=$((n + 1))
    sleep 1
  done
  fail "control HTTPS did not recover after ${service} restart"
}

# Wait for discovery on the script-owned ephemeral issuer.
await_issuer_ready() {
  local log="$1"
  for _ in $(seq 1 40); do
    curl -sfS "${VOIE_OIDC_ISSUER}/.well-known/openid-configuration" >/dev/null 2>&1 && return 0
    sleep 0.25
  done
  edge "ephemeral OIDC issuer did not become ready (${log})"
}

# Print "host port" parsed from an http(s) URL. Addresses come from the
# configured endpoints; nothing is hardcoded (the sanitized literals in the
# source commit must never be copied).
endpoint_host_port() {
  python3 - "$1" "$2" <<'PY'
import sys, urllib.parse
url = urllib.parse.urlparse(sys.argv[1])
print(url.hostname, url.port or int(sys.argv[2]))
PY
}

# Authenticated read; prints the HTTP status, body into OUT (default /dev/null).
api_read() {
  local jar="$1" url="$2" out="${3:-/dev/null}"
  curl -sS -o "$out" -w '%{http_code}' -b "$jar" "$url"
}

# Same-origin JSON mutation exactly as the VOIE console sends it: Origin must
# equal the configured public origin, Content-Type must be application/json
# (same_origin_json guard), and the x-voie-intent CSRF marker rides along.
api_mutate() {
  local jar="$1" method="$2" url="$3" data="$4" out="${5:-/dev/null}"
  curl -sS -o "$out" -w '%{http_code}' \
    -b "$jar" \
    -H "Origin: ${VOIE_PUBLIC_ORIGIN%/}" \
    -H 'Content-Type: application/json' \
    -H 'x-voie-intent: mutate' \
    -X "$method" \
    ${data:+-d "$data"} \
    "$url"
}

# Product mTLS Fabric RPC. Adapted from the source commit's fabric_rpc
# scaffolding; TLS SNI name defaults to the endpoint host and may be pinned
# separately via VOIE_FABRIC_TLS_NAME when the certificate identity differs
# from the routable address. Prints the HTTP status, body into OUT.
fabric_rpc() {
  local method="$1" path="$2" out="${4:-/dev/null}"
  local data="${3:-}"
  require_env VOIE_FABRIC_ENDPOINT VOIE_FABRIC_CA_CERT_PATH \
    VOIE_FABRIC_CLIENT_CERT_PATH VOIE_FABRIC_CLIENT_KEY_PATH >/dev/null || {
    printf '  (needed for Fabric RPC %s %s)\n' "$method" "$path" >&2
    exit 2
  }
  local fhost fport
  read -r fhost fport <<<"$(endpoint_host_port "$VOIE_FABRIC_ENDPOINT" 7840)"
  local name="${VOIE_FABRIC_TLS_NAME:-$fhost}"
  local args=(
    --silent --show-error --max-time "${VOIE_FABRIC_TIMEOUT:-180}"
    --cacert "$VOIE_FABRIC_CA_CERT_PATH"
    --cert "$VOIE_FABRIC_CLIENT_CERT_PATH"
    --key "$VOIE_FABRIC_CLIENT_KEY_PATH"
    -X "$method"
  )
  if [ "$name" != "$fhost" ]; then
    args+=(--resolve "${name}:${fport}:${fhost}")
  fi
  if [ -n "$data" ]; then
    args+=(-H 'content-type: application/json' -d "$data")
  fi
  curl "${args[@]}" -o "$out" -w '%{http_code}' "https://${name}:${fport}${path}"
}

# True when some decoded canonical event carries a Bash tool-result whose
# text contains MARKER. Searching raw event text would let a model pass by
# parroting the prompt marker; only a tool-result block proves execution.
# Event bytes are the DSH session-log appends (one JSON line per event):
# tool/result events carry data.message.content[] with a tool-result block
# whose nested text holds the executed output. The legacy conversation
# {kind:"bash-result", output} shape is also accepted.
canonical_events_have_marker() {
  python3 - "$1" "$2" <<'PY'
import base64, json, sys

def result_texts(node):
    if isinstance(node, dict):
        if node.get("type") == "tool-result":
            for block in node.get("content") or []:
                if isinstance(block, dict) and isinstance(block.get("text"), str):
                    yield block["text"]
        if node.get("kind") == "bash-result" and isinstance(node.get("output"), str):
            yield node["output"]
        for value in node.values():
            yield from result_texts(value)
    elif isinstance(node, list):
        for value in node:
            yield from result_texts(value)

path, marker = sys.argv[1], sys.argv[2]
data = json.load(open(path, encoding="utf-8"))
for item in data.get("items") or []:
    try:
        raw = base64.b64decode(item.get("bytes") or "")
    except Exception:
        continue
    for line in raw.decode("utf-8", "replace").split("\n"):
        line = line.strip()
        if not line:
            continue
        try:
            event = json.loads(line)
        except Exception:
            continue
        if any(marker in text for text in result_texts(event)):
            sys.exit(0)
sys.exit(1)
PY
}

# Backward-compatible name used by the activation proofs (C4/C5).
canonical_bash_output_has_marker() {
  canonical_events_have_marker "$1" "$2"
}

# Mint a Web session through the real OIDC authorization-code boot and record
# the voie_session cookie into JAR. GET /login redirects to the issuer; an
# auto-approving test issuer (the in-repo dev-stack ephemeral issuer) accepts
# login/password query parameters. Because credentials then ride in the URL,
# that path is doubly constrained: VOIE_ALLOW_ISSUER_QUERY_LOGIN=yes must be
# set AND the issuer must be loopback. Otherwise provide VOIE_SESSION_COOKIE
# minted out-of-band.
oidc_login_boot() {
  local origin="${1%/}" jar="$2"
  if [ -n "${VOIE_SESSION_COOKIE:-}" ]; then
    printf '.\tTRUE\t/\tTRUE\t0\tvoie_session\t%s\n' "$VOIE_SESSION_COOKIE" >"$jar"
    return 0
  fi
  [ "${VOIE_ALLOW_ISSUER_QUERY_LOGIN:-}" = "yes" ] ||
    edge "automated issuer login is disabled; set VOIE_SESSION_COOKIE or opt in via VOIE_ALLOW_ISSUER_QUERY_LOGIN=yes"
  require_env VOIE_TEST_ISSUER_LOGIN VOIE_TEST_ISSUER_PASSWORD >/dev/null || {
    printf '  (set VOIE_TEST_ISSUER_LOGIN and VOIE_TEST_ISSUER_PASSWORD too)\n' >&2
    exit 2
  }
  local location status
  location="$(curl -sS -o /dev/null -D - -c "$jar" "${origin}/login" |
    tr -d '\r' | sed -n 's/^[Ll]ocation: //p' | head -1)"
  [ -n "$location" ] || fail "GET /login did not redirect to the OIDC issuer"
  read -r issuer_host _ <<<"$(endpoint_host_port "$location" 0)"
  if [ "$issuer_host" != "localhost" ] &&
    [ "$issuer_host" != "$LOOPBACK_IPV4" ] &&
    [ "$issuer_host" != "$LOOPBACK_IPV6_BRACKETED" ] &&
    [ "$issuer_host" != "$LOOPBACK_IPV6_BARE" ]; then
    edge "issuer is not loopback; refusing the query-login flow — mint VOIE_SESSION_COOKIE out-of-band"
  fi
  case "$location" in
    *\?*) location="${location}&login=${VOIE_TEST_ISSUER_LOGIN}&password=${VOIE_TEST_ISSUER_PASSWORD}" ;;
    *) location="${location}?login=${VOIE_TEST_ISSUER_LOGIN}&password=${VOIE_TEST_ISSUER_PASSWORD}" ;;
  esac
  location="$(curl -sS -o /dev/null -D - -b "$jar" -c "$jar" "$location" |
    tr -d '\r' | sed -n 's/^[Ll]ocation: //p' | head -1)"
  [ -n "$location" ] || fail "issuer authorize did not redirect back to the control callback"
  status="$(curl -sS -o /dev/null -w '%{http_code}' -b "$jar" -c "$jar" "$location")"
  grep -q $'\tvoie_session\t' "$jar" ||
    fail "OIDC boot completed (HTTP ${status}) without a voie_session cookie"
}

# True when a Web session can be minted out-of-band (VOIE_SESSION_COOKIE) or
# through the bootstrap admin credential pair (VOIE_BOOTSTRAP_ADMIN_* with
# VOIE_NATIVE_ADMIN_* accepted for compatibility). Prints missing names.
bootstrap_admin_env_ready() {
  [ -n "${VOIE_SESSION_COOKIE:-}" ] && return 0
  local missing=()
  if [ -z "${VOIE_BOOTSTRAP_ADMIN_USERNAME:-}" ] && [ -z "${VOIE_NATIVE_ADMIN_USERNAME:-}" ]; then
    missing+=("VOIE_BOOTSTRAP_ADMIN_USERNAME")
  fi
  if [ -z "${VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE:-}" ] && [ -z "${VOIE_NATIVE_ADMIN_PASSWORD_FILE:-}" ]; then
    missing+=("VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE (0600 file holding the bootstrap admin password)")
  fi
  if [ -n "${VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE:-}" ] && [ ! -r "$VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE" ]; then
    missing+=("VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE is unreadable")
  elif [ -n "${VOIE_NATIVE_ADMIN_PASSWORD_FILE:-}" ] && [ ! -r "$VOIE_NATIVE_ADMIN_PASSWORD_FILE" ]; then
    missing+=("VOIE_NATIVE_ADMIN_PASSWORD_FILE is unreadable")
  fi
  if [ "${#missing[@]}" -gt 0 ]; then
    printf 'required web-session inputs are missing:\n' >&2
    printf '  %s\n' "${missing[@]}" >&2
    return 1
  fi
}

# Mint a Web session through the native bootstrap-admin login and record the
# voie_session cookie into JAR. POST /login is form-encoded and same-origin;
# the password is read from the credential file, trimmed of exactly one
# trailing newline (mirroring the control's seed), staged in a 0600 file
# inside RUNTIME, and submitted via --data-urlencode @file — it never rides
# in argv, logs, or any other artifact. VOIE_SESSION_COOKIE mints the jar
# out-of-band when provided.
bootstrap_admin_login() {
  local origin="${1%/}" jar="$2"
  if [ -n "${VOIE_SESSION_COOKIE:-}" ]; then
    printf '.\tTRUE\t/\tTRUE\t0\tvoie_session\t%s\n' "$VOIE_SESSION_COOKIE" >"$jar"
    return 0
  fi
  [ -n "${RUNTIME:-}" ] || edge "bootstrap_admin_login needs RUNTIME set"
  local username password_file pwfile status password
  username="${VOIE_BOOTSTRAP_ADMIN_USERNAME:-${VOIE_NATIVE_ADMIN_USERNAME:-}}"
  [ -n "$username" ] || edge "native admin username (VOIE_BOOTSTRAP_ADMIN_USERNAME)"
  password_file="${VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE:-${VOIE_NATIVE_ADMIN_PASSWORD_FILE:-}}"
  if [ -z "$password_file" ] || [ ! -r "$password_file" ]; then
    edge "native admin password file (VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE, 0600)"
  fi
  password="$(cat "$password_file")"
  pwfile="${RUNTIME}/bootstrap-password"
  # Mirror the control's seed trim: exactly one trailing newline is removed.
  printf '%s' "${password%$'\n'}" >"$pwfile"
  chmod 600 "$pwfile"
  password=""
  # The control compares Origin by exact match against VOIE_PUBLIC_ORIGIN;
  # a trailing slash from a caller-exported value would 403 the login.
  local login_origin="${VOIE_PUBLIC_ORIGIN:-$origin}"
  status="$(curl -sS -o "${RUNTIME}/login-body" -D "${RUNTIME}/login-headers" -c "$jar" -w '%{http_code}' \
    -H "Origin: ${login_origin%/}" \
    -H 'Content-Type: application/x-www-form-urlencoded' \
    --data-urlencode "username=${username}" \
    --data-urlencode "password@${pwfile}" \
    "${origin}/login")"
  [ "$status" = "303" ] || fail "native login HTTP ${status}: $(cat "${RUNTIME}/login-body" 2>/dev/null)"
  grep -qi '^location: /' "${RUNTIME}/login-headers" || fail "native login Location is not /"
  grep -q $'\tvoie_session\t' "$jar" || fail "native login completed without a voie_session cookie"
  rm -f "$pwfile"
}

# Print the id of the acting user's personal scope from a /api/projects page.
# Prefer the product Personal scope (name "Personal") over leftover live-*
# projects that older session provisioning created with kind=personal.
personal_project_id_of() {
  python3 - "$1" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
personals = [p for p in (data.get("items") or []) if p.get("kind") == "personal"]
for project in personals:
    if project.get("name") == "Personal":
        print(project["id"])
        raise SystemExit(0)
if personals:
    print(personals[0]["id"])
PY
}

# Resolve the acting user's Personal scope (kind=personal) from the project
# listing; when the product has not auto-created one yet (e.g. an
# OAuth-linked user), create it through POST /api/projects. Prints the id.
resolve_personal_scope() {
  local jar="$1" out="$2"
  local status id
  status="$(api_read "$jar" "${VOIE_CONTROL_URL%/}/api/projects" "$out")"
  [ "$status" = "200" ] || fail "/api/projects HTTP ${status}: $(cat "$out")"
  id="$(personal_project_id_of "$out")"
  if [ -z "$id" ]; then
    status="$(api_mutate "$jar" POST "${VOIE_CONTROL_URL%/}/api/projects" \
      "{\"id\":\"$(uuid4)\",\"name\":\"Personal\"}" "$out")"
    [ "$status" = "200" ] || fail "personal scope create HTTP ${status}: $(cat "$out")"
    status="$(api_read "$jar" "${VOIE_CONTROL_URL%/}/api/projects" "$out")"
    [ "$status" = "200" ] || fail "/api/projects re-read HTTP ${status}"
    id="$(personal_project_id_of "$out")"
  fi
  [ -n "$id" ] || fail "no personal scope found for the acting user: $(cat "$out")"
  printf '%s' "$id"
}

# Create one Agent in the scope through the product API; prints its id.
provision_agent() {
  local jar="$1" project_id="$2" out="$3"
  local agent status
  agent="$(uuid4)"
  status="$(api_mutate "$jar" POST "${VOIE_CONTROL_URL%/}/api/projects/${project_id}/agents" \
    "{\"id\":\"${agent}\",\"name\":\"live-agent-${agent%%-*}\",\"model\":\"${VOIE_MODEL_NAME:-}\",\"max_tokens\":1024}" "$out")"
  [ "$status" = "200" ] || fail "agent create HTTP ${status}: $(cat "$out")"
  printf '%s' "$agent"
}

# Poll GET /api/runs/{id} until STATE reaches terminal within TRIES seconds.
# The final run body is left in OUT.
await_run_resource() {
  local jar="$1" run_id="$2" out="$3" tries="${4:-120}"
  local status i=0
  while [ "$i" -lt "$tries" ]; do
    i=$((i + 1))
    status="$(api_read "$jar" "${VOIE_CONTROL_URL%/}/api/runs/${run_id}" "$out")"
    [ "$status" = "200" ] || fail "run read HTTP ${status}: $(cat "$out")"
    case "$(json_field 'state' <"$out" 2>/dev/null || echo '')" in
      terminal) return 0 ;;
      unknown|cancelled) return 1 ;;
    esac
    sleep 1
  done
  return 1
}

# Canonical event bytes are Blob-backed; a run can be terminal a moment
# before the session event listing reflects the bash result.
await_canonical_marker() {
  local jar="$1" session_id="$2" marker="$3" out="$4" tries="${5:-30}"
  local status i=0
  while [ "$i" -lt "$tries" ]; do
    i=$((i + 1))
    status="$(api_read "$jar" "${VOIE_CONTROL_URL%/}/api/sessions/${session_id}/events" "$out")"
    [ "$status" = "200" ] || fail "session events HTTP ${status}: $(cat "$out")"
    canonical_events_have_marker "$out" "$marker" && return 0
    sleep 1
  done
  return 1
}

# Provision Agent+Session over REST as the console does; prints the session
# id. Needs WORKSPACE_ID. Reuses PROJECT_ID when already set (a product
# Workspace is owned by that project); otherwise creates a fresh project.
rest_provision_session() {
  local jar="$1" out="$2"
  local project agent session status
  if [ -n "${PROJECT_ID:-}" ]; then
    project="$PROJECT_ID"
  else
    project="$(uuid4)"
    status="$(api_mutate "$jar" POST "${VOIE_CONTROL_URL%/}/api/projects" \
      "{\"id\":\"${project}\",\"name\":\"live-$(date +%s)-$$\"}" "$out")"
    [ "$status" = "200" ] || fail "project create HTTP ${status}: $(cat "$out")"
    PROJECT_ID="$project"
    export PROJECT_ID
  fi
  agent="$(uuid4)"
  status="$(api_mutate "$jar" POST "${VOIE_CONTROL_URL%/}/api/projects/${project}/agents" \
    "{\"id\":\"${agent}\",\"name\":\"live-agent-${agent%%-*}\",\"model\":\"${VOIE_MODEL_NAME:-}\",\"max_tokens\":1024}" "$out")"
  [ "$status" = "200" ] || fail "agent create HTTP ${status}: $(cat "$out")"
  session="$(uuid4)"
  status="$(api_mutate "$jar" POST "${VOIE_CONTROL_URL%/}/api/projects/${project}/sessions" \
    "{\"id\":\"${session}\",\"agentId\":\"${agent}\",\"workspaceId\":\"${WORKSPACE_ID}\"}" "$out")"
  [ "$status" = "200" ] || fail "session create HTTP ${status}: $(cat "$out")"
  [ -n "$session" ] || fail "session create returned no id"
  printf '%s' "$session"
}

# Create or reuse a product Workspace (PostgreSQL row + Fabric realize) in
# the acting user's personal scope. Session create addresses only those
# rows; a direct Fabric POST /v1/workspaces is not a control Workspace.
# Sets PROJECT_ID, WORKSPACE_ID, and PRODUCT_WORKSPACE=1. Do not Fabric-DELETE
# a product Workspace: that is how ghost ready rows without LVs are made.
product_workspace_open() {
  local jar="$1" out="$2" label="$3"
  local status existing
  PROJECT_ID="$(resolve_personal_scope "$jar" "$out")"
  export PROJECT_ID
  status="$(api_read "$jar" "${VOIE_CONTROL_URL%/}/api/scopes/${PROJECT_ID}/workspaces" "$out")"
  [ "$status" = "200" ] || fail "scope workspaces list HTTP ${status}: $(cat "$out")"
  existing="$(python3 - "$out" "$PROJECT_ID" "$label" <<'PY'
import json, sys
path, scope_id, label = sys.argv[1], sys.argv[2], sys.argv[3]
data = json.load(open(path, encoding="utf-8"))
for item in data.get("items") or []:
    if str(item.get("label") or "") != label:
        continue
    wid = str(item.get("id") or "").strip()
    scope = str(item.get("scopeId") or item.get("projectId") or "").strip()
    state = str(item.get("state") or "").strip()
    if wid and state == "ready" and (not scope or scope == scope_id):
        print(wid)
        break
PY
)"
  local probe="${RUNTIME:-/tmp}/workspace-probe.json"
  if [ -n "$existing" ] && product_workspace_has_volume "$existing" "$probe"; then
    WORKSPACE_ID="$existing"
    export WORKSPACE_ID PRODUCT_WORKSPACE=1
    return 0
  fi
  WORKSPACE_ID="$(uuid4)"
  status="$(api_mutate "$jar" POST "${VOIE_CONTROL_URL%/}/api/projects/${PROJECT_ID}/workspaces" \
    "{\"id\":\"${WORKSPACE_ID}\",\"label\":\"${label}\"}" "$out")"
  if [ "$status" = "429" ]; then
    # User quota is 8. Ghost ready rows without an LV still charge it.
    # Reuse a ready Workspace that Fabric still holds a block device for.
    # Never take the dedicated native-c6 acceptance Workspace.
    status="$(api_read "$jar" "${VOIE_CONTROL_URL%/}/api/scopes/${PROJECT_ID}/workspaces" "$out")"
    [ "$status" = "200" ] || fail "scope workspaces list HTTP ${status}: $(cat "$out")"
    existing="$(python3 - "$out" "$PROJECT_ID" <<'PY'
import json, sys
path, scope_id = sys.argv[1], sys.argv[2]
data = json.load(open(path, encoding="utf-8"))
for item in data.get("items") or []:
    wid = str(item.get("id") or "").strip()
    scope = str(item.get("scopeId") or item.get("projectId") or "").strip()
    state = str(item.get("state") or "").strip()
    label = str(item.get("label") or "")
    if label == "native-c6":
        continue
    if wid and state == "ready" and (not scope or scope == scope_id):
        print(wid)
PY
)"
    WORKSPACE_ID=""
    local cand
    for cand in $existing; do
      if product_workspace_has_volume "$cand" "$probe"; then
        WORKSPACE_ID="$cand"
        break
      fi
    done
    [ -n "$WORKSPACE_ID" ] ||
      edge "workspace quota reached; no Fabric-backed ready workspace to reuse"
    export WORKSPACE_ID PRODUCT_WORKSPACE=1
    return 0
  fi
  [ "$status" = "200" ] || fail "product workspace create HTTP ${status}: $(cat "$out")"
  [ "$(json_field 'id' <"$out")" = "$WORKSPACE_ID" ] ||
    fail "workspace create returned a different id: $(cat "$out")"
  [ "$(json_field 'state' <"$out")" = "ready" ] ||
    fail "workspace create did not return ready: $(cat "$out")"
  export WORKSPACE_ID PRODUCT_WORKSPACE=1
}

# True when Fabric still holds a block device for this workspace id.
product_workspace_has_volume() {
  local id="$1" probe="$2"
  local code device
  code="$(fabric_rpc GET "/v1/workspaces/${id}" "" "$probe")"
  [ "$code" = "200" ] || return 1
  device="$(json_field 'device' <"$probe" 2>/dev/null || true)"
  case "$device" in
    /dev/*) ;;
    *) return 1 ;;
  esac
  local alloc=""
  alloc="$(json_field 'allocatedBytes' <"$probe" 2>/dev/null || true)"
  [ -n "$alloc" ] && [ "$alloc" != "0" ] && [ "$alloc" != "None" ]
}

# Start one run (mode create|resume) and poll GET /api/runs/{id} until STATE
# reaches terminal within TRIES seconds. Prints the final run body path.
await_run_terminal() {
  local jar="$1" run_id="$2" out="$3" tries="${4:-120}"
  local status i=0
  status="$(api_mutate "$jar" POST "${VOIE_CONTROL_URL%/}/api/sessions/${SESSION_ID}/runs" \
    "{\"runId\":\"${run_id}\",\"intentId\":\"$(uuid4)\",\"prompt\":\"${RUN_PROMPT}\",\"mode\":\"${RUN_MODE:-create}\"}" "$out")"
  [ "$status" = "200" ] || fail "run start HTTP ${status}: $(cat "$out")"
  while [ "$i" -lt "$tries" ]; do
    i=$((i + 1))
    status="$(api_read "$jar" "${VOIE_CONTROL_URL%/}/api/runs/${run_id}" "$out")"
    [ "$status" = "200" ] || fail "run read HTTP ${status}: $(cat "$out")"
    case "$(json_field 'state' <"$out" 2>/dev/null || echo '')" in
      terminal) return 0 ;;
      unknown|cancelled) return 1 ;;
    esac
    sleep 1
  done
  return 1
}

# Shared scratch Fabric workspace with guaranteed cleanup. Caller exports
# WORKSPACE_ID; the DELETE result is asserted by the caller before exit.
scratch_workspace_open() {
  local out="$1"
  local status
  status="$(fabric_rpc POST /v1/workspaces '{}' "$out")"
  [ "$status" = "200" ] || edge "Fabric workspace create (HTTP ${status}: $(cat "$out"))"
  WORKSPACE_ID="$(json_field 'id' <"$out")"
  [ -n "$WORKSPACE_ID" ] || edge "Fabric workspace create returned no id"
  export WORKSPACE_ID
}

scratch_workspace_close() {
  fabric_rpc DELETE "/v1/workspaces/${WORKSPACE_ID}" "" /dev/null >/dev/null 2>&1 || true
}

# Poll guest exec until /workspace is mounted; fails the proof otherwise.
# Requires WORKSPACE_ID and Fabric mTLS material; uses fabric_rpc.
await_workspace_mounted() {
  local ws_id="$1" out i=0 status
  out="$(mktemp)"
  while [ "$i" -lt 30 ]; do
    i=$((i + 1))
    status="$(fabric_rpc POST "/v1/workspaces/${ws_id}/exec" \
      "{\"call_id\":\"mount-wait-${i}-$$\",\"command\":\"grep ' /workspace ' /proc/mounts\"}" \
      "$out" || true)"
    if [ "$status" = "200" ] &&
      [ "$(jq -r .exit_code "$out" 2>/dev/null || true)" = "0" ] &&
      jq -r .stdout "$out" 2>/dev/null | grep -q ' /workspace '; then
      rm -f "$out"
      return 0
    fi
    sleep 1
  done
  rm -f "$out"
  return 1
}
