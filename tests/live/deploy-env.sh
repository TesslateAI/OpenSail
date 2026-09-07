#!/usr/bin/env bash
# Shared, data-only deployment input helpers for live C7/C8 recipes.
# This file is sourced after the caller enables strict shell mode.

load_voie_deploy_env() {
  local file="$1" line key value
  while IFS= read -r line || [[ -n "$line" ]]; do
    case "$line" in
      ""|\#*) continue ;;
      export\ *) line="${line#export }" ;;
    esac
    [[ "$line" == *=* ]] || continue
    key="${line%%=*}"
    value="${line#*=}"
    case "$key" in
      ACME_EMAIL|AZURE_CLIENT_ID|AZURE_CLIENT_SECRET|AZURE_SUBSCRIPTION_ID|AZURE_TENANT_ID|\
      BAREMETAL_SSH_HOST|BAREMETAL_SSH_USER|\
      CLOUDFLARE_API_TOKEN|CLOUDFLARE_ZONE_ID|\
      VOIE_ACME_EMAIL|VOIE_ADMIN_SSH_PUBLIC_KEY|VOIE_BASE_DOMAIN|\
      VOIE_CONTROL_IMAGE_ID|VOIE_CONTROL_IMAGE_VHD_PATH|VOIE_FABRIC_BOOTSTRAP_HOST|\
      VOIE_LOCATION|VOIE_MANAGEMENT_CIDRS|VOIE_PUBLIC_HOSTNAME|VOIE_SUBSCRIPTION_ID|\
      VOIE_TENANT_ID|VOIE_TF_BACKEND_RESOURCE_GROUP|VOIE_TF_BACKEND_STORAGE_ACCOUNT|\
      VOIE_TF_BACKEND_CONTAINER|VOIE_WORKSPACE_PV_DEVICE|VOIE_FABRIC_UUID|\
      ARM_ACCESS_TOKEN|ARM_SUBSCRIPTION_ID|\
      VOIE_TF_BACKEND_HCL|\
      ARM_CLIENT_ID|ARM_CLIENT_SECRET|ARM_TENANT_ID|VOIE_BOOTSTRAP_ADMIN_USERNAME|\
      VOIE_BOOTSTRAP_ADMIN_PASSWORD|\
      VOIE_MODEL_BASE_URL|VOIE_MODEL_NAME|VOIE_MODEL_API_KEY|VOIE_MODEL_API_KEY_FILE|\
      VOIE_OIDC_PROVISION|VOIE_WIPE_VOIE_WS) ;;
      *) continue ;;
    esac
    case "$value" in
      \"*\") value="${value:1:${#value}-2}" ;;
      \'*\') value="${value:1:${#value}-2}" ;;
    esac
    printf -v "$key" '%s' "$value"
    export "$key"
  done < "$file"
}

normalize_voie_deploy_env() {
  : "${VOIE_SUBSCRIPTION_ID:=${AZURE_SUBSCRIPTION_ID:-}}"
  : "${VOIE_TENANT_ID:=${AZURE_TENANT_ID:-}}"
: "${VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE:=}"
  : "${VOIE_ACME_EMAIL:=${ACME_EMAIL:-}}"
  # Estate rule: the Fabric deployment target is always the literal SSH
  # alias `baremetal-1-cs`; no underlying host discovery happens here.
  VOIE_FABRIC_BOOTSTRAP_HOST="baremetal-1-cs"
  : "${VOIE_SSH_USER:=${BAREMETAL_SSH_USER:-}}"
  : "${ARM_CLIENT_ID:=${AZURE_CLIENT_ID:-}}"
  : "${ARM_CLIENT_SECRET:=${AZURE_CLIENT_SECRET:-}}"
  : "${ARM_TENANT_ID:=${AZURE_TENANT_ID:-}}"
  : "${ARM_SUBSCRIPTION_ID:=${AZURE_SUBSCRIPTION_ID:-}}"
# Native deployment needs the Azure location; the estate default lives in
# the env file as VOIE_AZURE_LOCATION or AZURE_LOCATION.
: "${VOIE_LOCATION:=${AZURE_LOCATION:-}}"
  export ARM_CLIENT_ID ARM_CLIENT_SECRET ARM_TENANT_ID ARM_SUBSCRIPTION_ID
}

load_voie_backend_cache() {
  local cache="$1"
  VOIE_BACKEND_FROM_CACHE=0
  if [[ -r "$cache" && -z "${VOIE_TF_BACKEND_RESOURCE_GROUP:-}${VOIE_TF_BACKEND_STORAGE_ACCOUNT:-}${VOIE_TF_BACKEND_CONTAINER:-}" ]]; then
    VOIE_TF_BACKEND_RESOURCE_GROUP="$(jq -r '.backend.config.resource_group_name // empty' "$cache")"
    VOIE_TF_BACKEND_STORAGE_ACCOUNT="$(jq -r '.backend.config.storage_account_name // empty' "$cache")"
    VOIE_TF_BACKEND_CONTAINER="$(jq -r '.backend.config.container_name // empty' "$cache")"
    VOIE_BACKEND_FROM_CACHE=1
  fi
}

discover_voie_workspace_pv() {
  local pv_json
  if [[ -n "${VOIE_WORKSPACE_PV_DEVICE:-}" ]]; then
    return 0
  fi
  pv_json="$(ssh -o BatchMode=yes -o ConnectTimeout=15 baremetal-1-cs "pvs --reportformat json --select 'vg_name=voie-ws' -o pv_name" 2>/dev/null || true)"
  if [[ -n "$pv_json" ]]; then
    local -a candidates=()
    mapfile -t candidates < <(jq -r '.report[0].pv[]?.pv_name // empty' <<<"$pv_json")
    if ((${#candidates[@]} == 1)); then
      VOIE_WORKSPACE_PV_DEVICE="${candidates[0]}"
    fi
  fi
}

# Existing Control estate: require exactly one valid baremetal-1 Fabric UUID.
# First estate (no control VM in managed state): use VOIE_FABRIC_UUID or mint once.
resolve_voie_fabric_uuid() {
  local prior_state="$1"
  local control_ssh="${VOIE_CONTROL_SSH:-control}"
  local control_count out rc=0
  local -a ids=()
  control_count="$(jq -r '[.values.root_module.resources[]? | select(.type == "azurerm_linux_virtual_machine" and .name == "control")] | length' "$prior_state" 2>/dev/null || printf '0')"
  if [[ "$control_count" != "0" ]]; then
    out="$(ssh -o BatchMode=yes -o ConnectTimeout=8 "$control_ssh" \
      "PGSERVICEFILE=/etc/voie/secrets/postgres-service.conf psql service=voie -Atc \"select id from fabrics where name = 'baremetal-1'\"" )" || rc=$?
    mapfile -t ids < <(printf '%s\n' "$out" | sed '/^[[:space:]]*$/d')
    if [[ "$rc" != 0 || "${#ids[@]}" -ne 1 ]]; then
      # Empty/wiped PostgreSQL has no fabrics row. Ansible control.yml
      # inserts VOIE_FABRIC_UUID after migrate; the extra-var must still
      # carry the enrolled identity through DESTROY redeploy.
      if [[ "${VOIE_FABRIC_UUID:-}" =~ ^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$ ]]; then
        printf 'just live-c7: live fabrics lookup unavailable; using VOIE_FABRIC_UUID\n' >&2
        export VOIE_FABRIC_UUID
        return 0
      fi
      if [[ "$rc" != 0 ]]; then
        printf 'just live-c7: existing estate Fabric identity lookup failed (SSH or PostgreSQL)\n' >&2
        return 2
      fi
      printf 'just live-c7: existing estate must have exactly one baremetal-1 Fabric UUID (found %s)\n' "${#ids[@]}" >&2
      return 2
    fi
    if [[ ! "${ids[0]}" =~ ^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$ ]]; then
      printf 'just live-c7: registered Fabric identity is not a UUID\n' >&2
      return 2
    fi
    VOIE_FABRIC_UUID="${ids[0]}"
    export VOIE_FABRIC_UUID
    return 0
  fi
  if [[ -n "${VOIE_FABRIC_UUID:-}" ]]; then
    if [[ ! "$VOIE_FABRIC_UUID" =~ ^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$ ]]; then
      printf 'just live-c7: VOIE_FABRIC_UUID is not a UUID\n' >&2
      return 2
    fi
    export VOIE_FABRIC_UUID
    return 0
  fi
  VOIE_FABRIC_UUID="$(python3 -c 'import uuid; print(uuid.uuid4())')"
  export VOIE_FABRIC_UUID
}
