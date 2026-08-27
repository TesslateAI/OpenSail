#!/usr/bin/env bash
# Runtime-generated local PKI for the bounded dev stack. One throwaway dev
# CA signs exactly three short-lived certificates:
#
#   ca.pem                  dev root CA (self-signed, CA:TRUE)
#   control-cert.pem        HTTPS control endpoint Caddy serves (server auth;
#                           SANs localhost / 127.0.0.1 / ::1)
#   fabric-server-cert.pem  HTTPS Fabric endpoint Caddy serves with mTLS
#                           (server auth; SAN 127.0.0.1)
#   client-cert.pem         the single Fabric client identity the product
#                           FabricClient presents (client auth)
#   ca-bundle.pem           runtime CA bundle for product env consumers
#
# Every key and certificate lives under XDG_RUNTIME_DIR/voie-dev-stack/tls
# with private permissions; nothing here is ever committed to the checkout.
#   gen [--force]  create or regenerate the material (idempotent)
#   verify         no-child verification: chain, validity window, purpose,
#                  SAN, key match, CA flag, file permissions
#
# Only short-lived openssl invocations run here; no listener, daemon, VM, or
# build is started, and every byte stays inside the runtime directory.
set -euo pipefail

runtime_base="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
case "$runtime_base" in
  /*) ;;
  *) printf 'dev-stack-tls: XDG_RUNTIME_DIR must be absolute\n' >&2; exit 2 ;;
esac
runtime_root="$runtime_base/voie-dev-stack"
tls_dir="$runtime_root/tls"

days="${VOIE_DEV_TLS_DAYS:-7}"
umask 077

fail() {
  printf 'dev-stack-tls: %s\n' "$*" >&2
  exit 1
}

ec_key() {
  openssl ecparam -name prime256v1 -genkey -noout -out "$1" 2>/dev/null ||
    fail "cannot generate EC key $1"
}

# One leaf certificate signed by the dev CA: name, subject, SAN list, EKU.
issue_leaf() {
  local name="$1" subject="$2" san="$3" eku="$4"
  local ext="$tls_dir/$name.ext" csr="$tls_dir/$name.csr"
  ec_key "$tls_dir/$name-key.pem"
  openssl req -new -key "$tls_dir/$name-key.pem" -subj "$subject" -out "$csr" 2>/dev/null ||
    fail "cannot build CSR for $name"
  {
    printf 'basicConstraints=critical,CA:FALSE\n'
    printf 'keyUsage=critical,digitalSignature,keyAgreement\n'
    printf 'extendedKeyUsage=%s\n' "$eku"
    printf 'subjectAltName=%s\n' "$san"
  } >"$ext"
  openssl x509 -req -in "$csr" \
    -CA "$tls_dir/ca.pem" -CAkey "$tls_dir/ca-key.pem" \
    -set_serial "0x$(openssl rand -hex 16)" \
    -days "$days" -sha256 -extfile "$ext" -out "$tls_dir/$name-cert.pem" 2>/dev/null ||
    fail "cannot issue $name certificate"
  rm -f "$csr" "$ext"
}

material_complete() {
  test -s "$tls_dir/ca.pem" && test -s "$tls_dir/ca-key.pem" &&
    test -s "$tls_dir/control-cert.pem" && test -s "$tls_dir/control-key.pem" &&
    test -s "$tls_dir/fabric-server-cert.pem" && test -s "$tls_dir/fabric-server-key.pem" &&
    test -s "$tls_dir/client-cert.pem" && test -s "$tls_dir/client-key.pem" &&
    test -s "$tls_dir/ca-bundle.pem"
}

gen() {
  if [[ "${1:-}" == "--force" ]]; then
    rm -rf "$tls_dir"
  elif material_complete; then
    printf 'dev-stack-tls: reusing existing material in %s\n' "$tls_dir"
    return 0
  fi
  install -d -m 700 "$tls_dir"

  ec_key "$tls_dir/ca-key.pem"
  openssl req -x509 -new -key "$tls_dir/ca-key.pem" -sha256 -days "$days" \
    -subj "/CN=voie-dev-stack root/O=voie-dev" \
    -addext "basicConstraints=critical,CA:TRUE,pathlen:0" \
    -addext "keyUsage=critical,keyCertSign,cRLSign" \
    -out "$tls_dir/ca.pem" 2>/dev/null ||
    fail "cannot generate the dev CA"

  issue_leaf control "/CN=localhost/O=voie-dev" \
    "DNS:localhost,IP:127.0.0.1,IP:0:0:0:0:0:0:0:1" serverAuth
  issue_leaf fabric-server "/CN=fabric.localhost/O=voie-dev" \
    "DNS:fabric.localhost,IP:127.0.0.1" serverAuth
  issue_leaf client "/CN=voie-dev-fabric-client/O=voie-dev" \
    "DNS:voie-dev-fabric-client" clientAuth

  chmod 600 "$tls_dir"/*-key.pem
  chmod 644 "$tls_dir"/ca.pem "$tls_dir"/*-cert.pem
  cat "$tls_dir/ca.pem" > "$tls_dir/ca-bundle.pem"
  chmod 644 "$tls_dir/ca-bundle.pem"
  verify
  printf 'dev-stack-tls: generated %s (CA + control + fabric server + fabric client + bundle, %s days)\n' \
    "$tls_dir" "$days"
}

require_text() {
  local cert="$1" label="$2" pattern="$3"
  openssl x509 -in "$cert" -noout -text 2>/dev/null | grep -q "$pattern" ||
    fail "$label is missing $pattern"
}

verify() {
  material_complete || fail "no complete material in $tls_dir; run dev-stack/tls.sh gen"

  openssl verify -CAfile "$tls_dir/ca.pem" \
    "$tls_dir/control-cert.pem" "$tls_dir/fabric-server-cert.pem" \
    "$tls_dir/client-cert.pem" >/dev/null ||
    fail "certificate chain does not verify against the dev CA"

  for cert in ca control-cert fabric-server-cert client-cert; do
    openssl x509 -in "$tls_dir/$cert.pem" -checkend 3600 -noout ||
      fail "$cert.pem is expired or expires within one hour; regenerate with --force"
  done

  require_text "$tls_dir/ca.pem" "dev CA" "CA:TRUE"
  require_text "$tls_dir/control-cert.pem" "control cert" "TLS Web Server Authentication"
  require_text "$tls_dir/control-cert.pem" "control cert" "DNS:localhost"
  require_text "$tls_dir/fabric-server-cert.pem" "fabric server cert" "TLS Web Server Authentication"
  require_text "$tls_dir/fabric-server-cert.pem" "fabric server cert" "IP Address:127.0.0.1"
  require_text "$tls_dir/client-cert.pem" "client cert" "TLS Web Client Authentication"

  check_pair() {
    local base="$1"
    diff <(openssl x509 -in "$tls_dir/$base-cert.pem" -pubkey -noout) \
      <(openssl pkey -in "$tls_dir/$base-key.pem" -pubout) >/dev/null ||
      fail "$base key does not match its certificate"
  }
  check_pair control
  check_pair fabric-server
  check_pair client

  local mode
  mode="$(stat -c %a "$tls_dir")"
  [[ "$mode" == 700 ]] || fail "$tls_dir must be mode 700, got $mode"
  for key in "$tls_dir"/*-key.pem; do
    mode="$(stat -c %a "$key")"
    [[ "$mode" == 600 ]] || fail "$key must be mode 600, got $mode"
  done
  test -s "$tls_dir/ca-bundle.pem" || fail "ca-bundle.pem is missing"
  diff "$tls_dir/ca.pem" "$tls_dir/ca-bundle.pem" >/dev/null ||
    fail "ca-bundle.pem must contain the dev CA"

  printf 'dev-stack-tls: verified chain, purposes, SANs, key pairs, and permissions\n'
}

case "${1:-}" in
  gen) shift || true; gen "$@" ;;
  verify) verify ;;
  *)
    printf 'usage: dev-stack/tls.sh {gen [--force]|verify}\n' >&2
    exit 2
    ;;
esac
