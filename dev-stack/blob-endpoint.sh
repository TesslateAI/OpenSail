# Source-only. Azurite product-style URLs take the account from the Host
# header (`http://ACCOUNT.blob.localhost:PORT/container/blob`). A previous
# rewrite to `http://127.0.0.1:PORT` made Host `127.0.0.1`, which is not a
# valid account name. Restore the vanity hostname for Azurite. Floci-AZ
# path-style `http://127.0.0.1:PORT/ACCOUNT` is left alone. Real Azure
# hostnames are left alone. Safe to call more than once.
voie_normalize_local_blob_endpoint() {
  local ep="${VOIE_AZURE_BLOB_ENDPOINT:-}"
  local acct="${VOIE_AZURE_BLOB_ACCOUNT:-}"
  local emu="${VOIE_DEV_CLOUD_EMULATOR:-}"
  local host port_and_rest port
  [[ -n "$ep" && -n "$acct" ]] || return 0
  case "$ep" in
    http://* | https://*) ;;
    *) return 0 ;;
  esac
  host="${ep#http://}"
  host="${host#https://}"
  host="${host%%:*}"
  host="${host%%/*}"
  port_and_rest="${ep##*:}"
  port="${port_and_rest%%/*}"
  [[ "$port" =~ ^[0-9]+$ ]] || return 0
  case "$host" in
    127.0.0.1)
      case "$emu" in
        azurite | "") ;;
        *) return 0 ;;
      esac
      case "$acct" in
        devstoreaccount1 | voiedevlocal) ;;
        *)
          [[ "$emu" == azurite ]] || return 0
          ;;
      esac
      export VOIE_AZURE_BLOB_ENDPOINT="http://${acct}.blob.localhost:${port}"
      ;;
  esac
}
