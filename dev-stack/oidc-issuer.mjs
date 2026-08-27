#!/usr/bin/env node
/**
 * Minimal standard OIDC provider for the local dev stack only.
 *
 * Implements discovery, authorization-code flow with PKCE-free state,
 * token issuance (RS256 ID tokens), and JWKS for exactly one fixed test
 * account. Keys and codes live in process memory; nothing is persisted.
 * Binds to loopback only; this is a development fixture, never a product
 * component.
 */
import { createServer } from "node:http";
import { createHash, generateKeyPairSync, randomUUID, createSign } from "node:crypto";

// Loopback host, constructed here rather than copied from any environment:
const HOST = [127, 0, 0, 1].join(".");

const port = Number(process.argv[2] ?? "18081");
const issuerUrl = `http://${HOST}:${port}`;
const CLIENT_ID = "voie-dev";
const REDIRECT_URI = `http://${HOST}:18080/oidc/callback`;
const ACCOUNT = { login: "voie-dev", password: "voie-dev-pass", subject: "voie-dev-subject" };

const { publicKey, privateKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });
const jwk = publicKey.export({ format: "jwk" });
const KID = "dev-key-1";

/** code -> { expires } */
const codes = new Map();

function b64url(input) {
  return Buffer.from(input).toString("base64url");
}

function signJwt(claims) {
  const header = b64url(JSON.stringify({ alg: "RS256", typ: "JWT", kid: KID }));
  const body = b64url(JSON.stringify(claims));
  const signer = createSign("RSA-SHA256");
  signer.update(`${header}.${body}`);
  const signature = signer.sign(privateKey.export({ type: "pkcs1", format: "pem" }));
  return `${header}.${body}.${signature.toString("base64url")}`;
}

function idToken(nonce) {
  const now = Math.floor(Date.now() / 1000);
  return signJwt({
    iss: issuerUrl,
    sub: ACCOUNT.subject,
    aud: CLIENT_ID,
    iat: now,
    exp: now + 600,
    nonce,
  });
}

const server = createServer((request, response) => {
  const url = new URL(request.url ?? "/", issuerUrl);
  const json = (status, body) => {
    response.writeHead(status, { "content-type": "application/json" });
    response.end(JSON.stringify(body));
  };

  if (url.pathname === "/.well-known/openid-configuration") {
    return json(200, {
      issuer: issuerUrl,
      authorization_endpoint: `${issuerUrl}/authorize`,
      token_endpoint: `${issuerUrl}/token`,
      jwks_uri: `${issuerUrl}/jwks`,
      id_token_signing_alg_values_supported: ["RS256"],
      response_types_supported: ["code"],
      subject_types_supported: ["public"],
    });
  }

  if (url.pathname === "/jwks") {
    return json(200, { keys: [{ ...jwk, kid: KID, alg: "RS256", use: "sig" }] });
  }

  // Authorization endpoint: auto-approve the single test account. Credentials
  // may arrive as query parameters from the driver; wrong values are refused.
  if (url.pathname === "/authorize") {
    if (
      url.searchParams.get("login") !== ACCOUNT.login ||
      url.searchParams.get("password") !== ACCOUNT.password
    ) {
      return json(401, { error: "invalid_credentials" });
    }
    const code = randomUUID();
    codes.set(code, { nonce: url.searchParams.get("nonce") ?? "", expires: Date.now() + 60_000 });
    const redirect = new URL(url.searchParams.get("redirect_uri") ?? REDIRECT_URI);
    redirect.searchParams.set("code", code);
    redirect.searchParams.set("state", url.searchParams.get("state") ?? "");
    response.writeHead(302, { location: redirect.toString() });
    return response.end();
  }

  if (url.pathname === "/token" && request.method === "POST") {
    let raw = "";
    request.on("data", (chunk) => (raw += chunk));
    request.on("end", () => {
      const params = new URLSearchParams(raw);
      const entry = codes.get(params.get("code") ?? "");
      codes.delete(params.get("code") ?? "");
      if (entry === undefined || entry.expires < Date.now()) {
        return json(400, { error: "invalid_grant" });
      }
      return json(200, {
        access_token: "dev-access-token",
        token_type: "Bearer",
        id_token: idToken(entry.nonce),
      });
    });
    return;
  }

  json(404, { error: "not_found" });
});

server.listen(port, HOST, () => {
  console.log(issuerUrl);
});
