#!/usr/bin/env bash
# Security invariant: the public Application edge asks voie-cloud to
# authorize private preview, and strips platform cookies before the app.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FILE="${ROOT}/ansible/templates/Caddyfile.j2"

grep -q 'forward_auth 127.0.0.1:8080' "$FILE" || {
  echo "Caddyfile missing forward_auth to voie-cloud" >&2
  exit 1
}
grep -q 'uri /internal/preview/authorize' "$FILE" || {
  echo "Caddyfile missing preview authorize URI" >&2
  exit 1
}
if grep -q 'Cookie {http.request.header.Cookie.replaceregex' "$FILE"; then
  echo "Cookie-strip placeholder must be double-quoted; Caddyfile splits on the space in (?:^|; )" >&2
  exit 1
fi
grep -F -q 'header_up Cookie "{http.request.header.Cookie.replaceregex' "$FILE" || {
  echo "Caddyfile does not strip platform cookies before the Application" >&2
  exit 1
}
grep -F -q '__Host-voie-preview' "$FILE" || {
  echo "Caddyfile does not strip the exact-host preview cookie" >&2
  exit 1
}
grep -F -q 'voie_session' "$FILE" || {
  echo "Caddyfile does not strip the console session cookie" >&2
  exit 1
}
if grep -q 'request_header {' "$FILE"; then
  echo "Caddyfile must not use block-form request_header (invalid on Caddy 2.11.4)" >&2
  exit 1
fi
grep -q 'header_up -X-Voie-Preview-Host' "$FILE" || {
  echo "Caddyfile must strip internal routing headers" >&2
  exit 1
}
if grep -q 'header_up X-Voie-Preview-Host {host}' "$FILE"; then
  echo "Caddyfile must not inject X-Voie-Preview-Host toward the app" >&2
  exit 1
fi
grep -q 'http://baremetal-1:8082' "$FILE" || {
  echo "Caddyfile must reverse-proxy Application hosts to the Fabric gateway" >&2
  exit 1
}
grep -q 'tls {{ wildcard_dev_cert }} {{ wildcard_dev_key }}' "$FILE" || {
  echo "Caddyfile must pin deployment-owned wildcard development cert files" >&2
  exit 1
}
grep -q 'tls {{ wildcard_prod_cert }} {{ wildcard_prod_key }}' "$FILE" || {
  echo "Caddyfile must pin deployment-owned wildcard production cert files" >&2
  exit 1
}
grep -q 'admin off' "$FILE" || {
  echo "public Caddyfile must disable the admin endpoint" >&2
  exit 1
}
if grep -q 'caddy-admin.sock' "$FILE"; then
  echo "public Caddyfile must not expose the Fabric gateway admin socket" >&2
  exit 1
fi
if grep -qiE 'on_demand' "$FILE"; then
  echo "Caddyfile must not use on-demand TLS for Application hosts" >&2
  exit 1
fi
if grep -qiE 'yaml|helm|kubectl|LoadBalancer' "$FILE"; then
  echo "Caddyfile contains user infrastructure fragments" >&2
  exit 1
fi
CONTROL="${ROOT}/ansible/control.yml"
grep -q -- '--dns' "$CONTROL" && grep -q cloudflare "$CONTROL" || {
  echo "control play must issue Application wildcards with Cloudflare DNS-01" >&2
  exit 1
}
grep -q 'lego' "$CONTROL" || {
  echo "control play must invoke lego for Application wildcard certificates" >&2
  exit 1
}
grep -q 'CLOUDFLARE_DNS_API_TOKEN' "$CONTROL" || {
  echo "control play must pass the Cloudflare DNS token only as a process environment" >&2
  exit 1
}
VERIFY="${ROOT}/ansible/verify.yml"
grep -q '/etc/voie/certs/wildcard-dev.crt' "$VERIFY" || {
  echo "verify play must require the issued wildcard development certificate" >&2
  exit 1
}

command -v caddy >/dev/null || {
  echo "caddy is required to adapt the Cookie-strip placeholder" >&2
  exit 1
}
ADAPT="$(mktemp -d)"
trap 'rm -rf "$ADAPT"' EXIT
cat >"$ADAPT/Caddyfile" <<'EOF'
{
	admin off
}
https://example.test {
	reverse_proxy 127.0.0.1:8080 {
		header_up Cookie "{http.request.header.Cookie.replaceregex((?i)(?:^|; )(?:voie_session|__Host-voie-preview)=[^;]*)}"
	}
}
EOF
caddy adapt --config "$ADAPT/Caddyfile" --adapter caddyfile >/dev/null || {
  echo "caddy rejected the quoted Cookie-strip placeholder" >&2
  exit 1
}

echo "caddyfile preview edge invariants hold"
