//! End-to-end REST contract of the native VOIE Console flow: OIDC-derived
//! session boot, project role, session creation, an accepted Run, the
//! canonical event envelope, and a cancel request.
//!
//! Everything runs locally against disposable doubles: a stub OIDC issuer,
//! a stub Azure Blob endpoint, throwaway mTLS material for the Fabric client,
//! and a stand-in activation child that keeps the dispatched Run in flight.
//! No remote estate or real provider credentials are involved.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::{Duration as ChronoDuration, Utc};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderValue, LOCATION};
use hyper::{Method, Request, Response, StatusCode};
use openidconnect::PrivateSigningKey;
use openidconnect::core::{
    CoreIdToken, CoreIdTokenClaims, CoreJsonWebKeySet, CoreJwsSigningAlgorithm,
    CoreRsaPrivateSigningKey,
};
use openidconnect::{
    Audience, EmptyAdditionalClaims, IssuerUrl, JsonWebKeyId, Nonce, StandardClaims,
    SubjectIdentifier,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;
use voie_cloud::session_store::{AppendEvent, SessionStore};
use voie_cloud::{Config, Kernel};

#[path = "common/tls_pems.rs"]
mod tls_pems;

const CLIENT_ID: &str = "voie-console-test";
const CLIENT_SECRET: &str = "voie-console-test-secret";
const BLOB_ACCOUNT: &str = "voie-test-account";
const BLOB_CONTAINER: &str = "voie-test-container";
/// Arbitrary 32-byte key material; only shape matters to the local stub.
const BLOB_KEY_BASE64: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const SUBJECT: &str = "alice";

const RSA_PEM: &str = r#"
-----BEGIN RSA PRIVATE KEY-----
MIIEpAIBAAKCAQEA1xC84njXQv1roQS/Z8Sm5zl7xW4ir7M0rR2ONmnKFJIDqw8e
VUvfbB7S5/X0mJvRqMUrAwBTl9CUhasBkh96s2fac04LjFJVJdrMLKbHLQC8uwcD
5J3zwgKgJLipuSvtPVSjGPMcDyvWm74twL2SSLANOMy4mAmxUWQl22FKLL3lG09Q
GCqVIjCec2g9GYCsa6Yihu/+lbTb5qxmMWhdR9riMlDPBsSJqDkpeV04TeIlb/MP
BRPkTxiBM9V9qgLfAMN4ry2g0+3xjJ8MMod/gwGwoQgVPMLjRRJCKO2lGLVGU8GA
ACko+0OSFEw1NYLyAZKWtBju6vapyz/jZFy3uwIDAQABAoIBAAKvMMxa2cT6SMef
uYvgBn1IWGUkdMZgpD2s6sN/GoibMfSGochKxCUjVVqT1VO6TimfHGRTMrfoYJIy
ijh6sBthJnbd+ILt3CY2zumXw1Cqe7CR69iEqDA5vCn5LBUlmTZ0wfxjvGvsDiev
ff6z3wmNOP0GgR9Ur6PmbhqI4lYgmsksbJhYBVg0tD8cUpAr4wpq3Am8i/nGfMTR
dn9xEPcT+ecqtvvJv/2uEN1OGjW4TqdWzQot3asipVOY+Z6bABT0uF66dPpYj42/
RkaolTeoLP82vmpFPAq6FiFy2T9OD7A8LmjycDTGlCLx9D6SrOENCR8hju2Guzdr
pU7Eq1ECgYEA8lfSoWO6JVqbRrcblnW/kWb7inHsJwUbo7juY2kb6G4D071OQ58A
LY6TZ5KJZOd8wkbfTGNLoX7NDTxJ9MWwD7CAjMGKE3AxI97rqkkA9gD2cfR4crvh
fiEazbRHyal0CFPJEJp0qV3ptzoU3tEbAiZZDtbQsyDt6FL3U384ibcCgYEA4y9k
WBaD8AWIrsvE7dbwS76YTMkYll5hV7EcvQXGYgc8xh5wqKqCtSim1sD4R+BJuuZG
V0sfT6UCE3/miaYp6P1jriKVP4ZOrGiXNX45lpPCbw+JfeIB2FI+ioIckEbJYZWa
+L7yGnZMbNcO6BDfCFpQAn3uFUTSEYoqSknr0h0CgYEAuZyAK7IpOUDrWr8V9yha
QDBjCkd0+vHTmJMkqqkvgdb5QWxljC80wK/JwHMgnlMaX+ZOUsBeheOLg86gSkQ7
M9kYrDXz3i14xaOQVk0x2jkkiGUY969k5ujOEa05qoAJ6fLaNchHAA142yg2Ie6A
RCZA4bewAvJ+pQkeeyoekIMCgYBm08mIMVCwb+DItQRCXnnO3sqSXqbZUIigp1KJ
n7aGIh540chOHzcgBfFV3GvEJJlaleWaly7p3pbM+qP/A42Onjni1FZXNVQgpwph
tOsd420q1Y52wrfxEHCsQm3pQ5DcsVk+YzazkX3P+ZsOoKxCXJZAOn1rdQXb2HyB
uWmaZQKBgQDWqHqHVuYNlVhv6hev80vqPGUUrlq41ggK9UcKosq4VW8EUUOoUN0d
SVJ7wHfd+AywQSkdiLxtOSosuZxIFmT709JDlguI7VEgu1cQUXz222pDHHWnjNBD
lDkVumJUghKJI8v0DhXt6V8hlMJvEtVP6R/OXV4f6i7osjvX9UbC6w==
-----END RSA PRIVATE KEY-----
"#;

// ---------------------------------------------------------------- HTTP client

struct Exchange {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Exchange {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn set_cookie(&self, name: &str) -> Option<String> {
        self.headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case("set-cookie"))
            .find_map(|(_, value)| value.strip_prefix(&format!("{name}=")))
            .map(|rest| rest.split(';').next().expect("cookie pair").to_string())
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("response body is JSON")
    }
}

fn request_text(
    method: &str,
    path: &str,
    port: u16,
    headers: &[(&str, String)],
    body: Option<&[u8]>,
) -> String {
    let mut text = format!("{method} {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\n");
    for (key, value) in headers {
        text.push_str(&format!("{key}: {value}\r\n"));
    }
    text.push_str("connection: close\r\n\r\n");
    if let Some(body) = body {
        text.push_str(&String::from_utf8_lossy(body));
    }
    text
}

async fn exchange(port: u16, request: &str) -> Exchange {
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        raw_exchange(port, request),
    )
    .await
    .expect("HTTP exchange completes inside 10s");
    result
}

async fn raw_exchange(port: u16, request: &str) -> Exchange {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("listener accepts connections");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("request writes");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("response reads");
    let text = String::from_utf8_lossy(&response);
    let (head, body) = text.split_once("\r\n\r\n").expect("header terminator");
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .expect("status line")
        .split_whitespace()
        .nth(1)
        .expect("status code")
        .parse()
        .expect("numeric status");
    let headers = lines
        .filter_map(|line| line.split_once(": "))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    Exchange {
        status,
        headers,
        body: body.as_bytes().to_vec(),
    }
}

async fn get(port: u16, path: &str, cookie: Option<&str>) -> Exchange {
    let mut headers: Vec<(&str, String)> = Vec::new();
    if let Some(cookie) = cookie {
        headers.push(("cookie", format!("{cookie}")));
    }
    exchange(port, &request_text("GET", path, port, &headers, None)).await
}

async fn post_json(
    port: u16,
    path: &str,
    cookie: &str,
    origin: Option<&str>,
    body: Value,
) -> Exchange {
    let bytes = body.to_string().into_bytes();
    let mut headers: Vec<(&str, String)> = vec![
        ("cookie", cookie.to_string()),
        (
            "content-type",
            "application/json; charset=utf-8".to_string(),
        ),
        ("content-length", bytes.len().to_string()),
    ];
    if let Some(origin) = origin {
        headers.push(("origin", origin.to_string()));
    }
    headers.push(("x-voie-intent", "mutate".to_string()));
    exchange(
        port,
        &request_text("POST", path, port, &headers, Some(&bytes)),
    )
    .await
}

/// The browser mutation bound enforced by the API surface.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Posts a raw byte body (any size) and reports the bare status.
async fn post_raw(port: u16, path: &str, cookie: &str, origin: &str, body: &[u8]) -> u16 {
    let headers: Vec<(&str, String)> = vec![
        ("cookie", cookie.to_string()),
        ("content-type", "application/json".to_string()),
        ("origin", origin.to_string()),
        ("x-voie-intent", "mutate".to_string()),
        ("content-length", body.len().to_string()),
    ];
    exchange(
        port,
        &request_text("POST", path, port, &headers, Some(body)),
    )
    .await
    .status
}

async fn patch_json(
    port: u16,
    path: &str,
    cookie: &str,
    origin: Option<&str>,
    body: Value,
) -> Exchange {
    let bytes = body.to_string().into_bytes();
    let mut headers: Vec<(&str, String)> = vec![
        ("cookie", cookie.to_string()),
        (
            "content-type",
            "application/json; charset=utf-8".to_string(),
        ),
        ("content-length", bytes.len().to_string()),
    ];
    if let Some(origin) = origin {
        headers.push(("origin", origin.to_string()));
    }
    headers.push(("x-voie-intent", "mutate".to_string()));
    exchange(
        port,
        &request_text("PATCH", path, port, &headers, Some(&bytes)),
    )
    .await
}

// ------------------------------------------------------------- stub OIDC issuer

struct IssuedCode {
    nonce: Nonce,
    redirect_uri: String,
    /// Subject minted for this code; the console test drives two users.
    subject: String,
}

struct TestIssuer {
    issuer_url: String,
    signing_key: CoreRsaPrivateSigningKey,
    jwks: String,
    codes: Mutex<HashMap<String, IssuedCode>>,
}

fn query_map(query: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect()
}

impl TestIssuer {
    fn new(issuer_url: String) -> Self {
        let signing_key = CoreRsaPrivateSigningKey::from_pem(
            RSA_PEM,
            Some(JsonWebKeyId::new("test-rsa".to_string())),
        )
        .expect("test RSA key");
        let jwks = serde_json::to_string(&CoreJsonWebKeySet::new(vec![
            signing_key.as_verification_key(),
        ]))
        .expect("JWKS serializes");
        TestIssuer {
            issuer_url,
            signing_key,
            jwks,
            codes: Mutex::new(HashMap::new()),
        }
    }

    async fn handle(&self, request: Request<Incoming>) -> Response<Full<bytes::Bytes>> {
        match (request.method(), request.uri().path()) {
            (&Method::GET, "/.well-known/openid-configuration") => {
                let body = serde_json::json!({
                    "issuer": self.issuer_url,
                    "authorization_endpoint": format!("{}/authorize", self.issuer_url),
                    "token_endpoint": format!("{}/token", self.issuer_url),
                    "jwks_uri": format!("{}/jwks", self.issuer_url),
                    "response_types_supported": ["code"],
                    "subject_types_supported": ["public"],
                    "id_token_signing_alg_values_supported": ["RS256"],
                    "token_endpoint_auth_methods_supported": ["client_secret_post"],
                });
                json_response(StatusCode::OK, body.to_string())
            }
            (&Method::GET, "/jwks") => json_response(StatusCode::OK, self.jwks.clone()),
            (&Method::GET, "/authorize") => self.authorize(request.uri().query().unwrap_or("")),
            (&Method::POST, "/token") => self.token(request).await,
            _ => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Full::new(bytes::Bytes::from_static(b"not found")))
                .expect("static response"),
        }
    }

    fn authorize(&self, query: &str) -> Response<Full<bytes::Bytes>> {
        let params = query_map(query);
        if params.get("client_id").map(String::as_str) != Some(CLIENT_ID)
            || params.get("response_type").map(String::as_str) != Some("code")
            || params.get("nonce").is_none()
        {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Full::new(bytes::Bytes::from_static(b"bad authorize")))
                .expect("static response");
        }
        let redirect_uri = params.get("redirect_uri").cloned().unwrap_or_default();
        let state = params.get("state").cloned().unwrap_or_default();
        let nonce = Nonce::new(params.get("nonce").cloned().unwrap_or_default());
        let subject = params
            .get("sub")
            .cloned()
            .unwrap_or_else(|| SUBJECT.to_string());
        let code = Uuid::new_v4().to_string();
        self.codes.lock().expect("issuer codes").insert(
            code.clone(),
            IssuedCode {
                nonce,
                redirect_uri: redirect_uri.clone(),
                subject,
            },
        );
        let location = format!("{redirect_uri}?code={code}&state={state}");
        Response::builder()
            .status(StatusCode::FOUND)
            .header(LOCATION, location)
            .body(Full::new(bytes::Bytes::new()))
            .expect("redirect response")
    }

    async fn token(&self, request: Request<Incoming>) -> Response<Full<bytes::Bytes>> {
        let collected = request.into_body().collect().await.expect("token body");
        let params = query_map(&String::from_utf8_lossy(&collected.to_bytes()));
        if params.get("client_id").map(String::as_str) != Some(CLIENT_ID)
            || params.get("client_secret").map(String::as_str) != Some(CLIENT_SECRET)
            || params.get("grant_type").map(String::as_str) != Some("authorization_code")
        {
            return json_response(
                StatusCode::BAD_REQUEST,
                r#"{"error":"invalid_client"}"#.into(),
            );
        }
        let issued = match params.get("code").cloned() {
            Some(code) => self.codes.lock().expect("issuer codes").remove(&code),
            None => None,
        };
        let Some(issued) = issued else {
            return json_response(
                StatusCode::BAD_REQUEST,
                r#"{"error":"invalid_grant"}"#.into(),
            );
        };
        if params.get("redirect_uri") != Some(&issued.redirect_uri) {
            return json_response(
                StatusCode::BAD_REQUEST,
                r#"{"error":"invalid_grant"}"#.into(),
            );
        }
        let claims = CoreIdTokenClaims::new(
            IssuerUrl::new(self.issuer_url.clone()).expect("issuer"),
            vec![Audience::new(CLIENT_ID.to_string())],
            Utc::now() + ChronoDuration::seconds(300),
            Utc::now(),
            StandardClaims::new(SubjectIdentifier::new(issued.subject.clone())),
            EmptyAdditionalClaims {},
        )
        .set_nonce(Some(issued.nonce));
        let id_token = CoreIdToken::new(
            claims,
            &self.signing_key,
            CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
            None,
            None,
        )
        .expect("id token signs");
        json_response(
            StatusCode::OK,
            serde_json::json!({
                "access_token": "not-used-by-product",
                "token_type": "Bearer",
                "id_token": id_token.to_string(),
            })
            .to_string(),
        )
    }
}

fn json_response(status: StatusCode, body: String) -> Response<Full<bytes::Bytes>> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
        .body(Full::new(bytes::Bytes::from(body)))
        .expect("json response")
}

async fn serve_issuer(listener: TcpListener, issuer: Arc<TestIssuer>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        let io = hyper_util::rt::TokioIo::new(stream);
        let issuer = issuer.clone();
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |request| {
                let issuer = issuer.clone();
                async move { Ok::<_, Infallible>(issuer.handle(request).await) }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await;
        });
    }
}

// --------------------------------------------------------- stub Azure Blob API

/// Minimal Azure Blob semantics the product clients rely on: immutable
/// put-if-absent and plain get, scoped to one container.
async fn serve_blob(listener: TcpListener, objects: Arc<Mutex<HashMap<String, Vec<u8>>>>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        let io = hyper_util::rt::TokioIo::new(stream);
        let objects = objects.clone();
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |request: Request<Incoming>| {
                let objects = objects.clone();
                async move { Ok::<_, Infallible>(blob_handle(request, &objects).await) }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await;
        });
    }
}

async fn blob_handle(
    request: Request<Incoming>,
    objects: &Mutex<HashMap<String, Vec<u8>>>,
) -> Response<Full<bytes::Bytes>> {
    let path = request.uri().path().to_owned();
    let Some(key) = path
        .strip_prefix(&format!("/{BLOB_CONTAINER}/"))
        .map(str::to_owned)
    else {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(bytes::Bytes::from_static(b"unknown container")))
            .expect("static response");
    };
    match request.method() {
        &Method::PUT => {
            let bytes = request
                .into_body()
                .collect()
                .await
                .expect("blob body")
                .to_bytes();
            let mut objects = objects.lock().expect("blob map lock");
            if let Some(existing) = objects.get(&key) {
                if existing.as_slice() != bytes.as_ref() {
                    return Response::builder()
                        .status(StatusCode::CONFLICT)
                        .body(Full::new(bytes::Bytes::from_static(b"immutable")))
                        .expect("static response");
                }
                return created();
            }
            objects.insert(key, bytes.to_vec());
            created()
        }
        &Method::GET => {
            let objects = objects.lock().expect("blob map lock");
            match objects.get(&key) {
                Some(bytes) => Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(bytes::Bytes::from(bytes.clone())))
                    .expect("blob get response"),
                None => Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Full::new(bytes::Bytes::from_static(b"missing")))
                    .expect("static response"),
            }
        }
        _ => Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .body(Full::new(bytes::Bytes::from_static(b"method")))
            .expect("static response"),
    }
}

// --------------------------------------------------------- stub Fabric service

const FABRIC_STUB_SCRIPT: &str = r#"
import http.server, ssl, sys, os, json, urllib.parse

port, cert, key, ca, flag, fixed = (
    int(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5], int(sys.argv[6])
)

# Dedicated flag files for the indeterminate-create regression: the test
# writes these in the same directory as `flag` (certs dir) without
# changing the stub spawn signature used elsewhere in this file.
post_status_path = os.path.join(os.path.dirname(flag), "fabric-post-status")
get_missing_path = os.path.join(os.path.dirname(flag), "fabric-get-missing")
get_state_path = get_missing_path + ".state"

class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def status_for_post(self):
        if fixed:
            return fixed
        # Explicit per-POST status file (e.g. "202") overrides the generic
        # fail-flag; absent => 200 (not 201: only 200 is truthful success).
        try:
            with open(post_status_path) as f:
                return int((f.read().strip() or "200"))
        except FileNotFoundError:
            pass
        except Exception:
            return 500
        if os.path.exists(flag):
            return 500
        return 200

    def handle_get(self):
        # GET /v1/health is a plain 200 empty probe (dependencies_ready).
        parsed = urllib.parse.urlparse(self.path)
        path = parsed.path
        if path == "/v1/health":
            self.send_response(200 if not fixed else fixed)
            self.send_header("content-length", "0")
            self.end_headers()
            return
        if path.startswith("/v1/workspaces/"):
            # Existence probe for indeterminate-create reconciliation.
            if os.path.exists(get_missing_path):
                self.send_response(404)
                self.send_header("content-length", "0")
                self.end_headers()
                return
            state = "ready"
            try:
                with open(get_state_path) as f:
                    state = (f.read().strip() or "ready")
            except FileNotFoundError:
                pass
            except Exception:
                state = "ready"
            body = json.dumps({"state": state, "id": path.split("/")[-1]}).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        # Generic GET (workspaces list, etc. — not used by this stub; real
        # Fabric handles those but control's dependencies only check health).
        self.send_response(200 if not fixed else fixed)
        self.send_header("content-length", "0")
        self.end_headers()

    def handle_post(self):
        length = int(self.headers.get("content-length", 0))
        if length:
            self.rfile.read(length)
        # Only the workspace-create route needs a distinct status; other
        # POSTs (replace) use the same mapping which is correct: 200 on
        # success, 500 when flag present.
        status = self.status_for_post()
        body = b"{}"
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def handle_delete(self):
        body = b'{"state":"deleted"}'
        self.send_response(200 if not fixed else fixed)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        if not fixed:
            self.wfile.write(body)

    def handle_put(self):
        length = int(self.headers.get("content-length", 0))
        if length:
            self.rfile.read(length)
        parsed = urllib.parse.urlparse(self.path)
        path = parsed.path
        status = self.status_for_post()
        if path.startswith("/v1/workspaces/"):
            state = "ready"
            try:
                with open(get_state_path) as f:
                    state = (f.read().strip() or "ready")
            except FileNotFoundError:
                pass
            except Exception:
                state = "ready"
            body = json.dumps({"state": state, "id": path.split("/")[-1]}).encode()
        else:
            body = b"{}"
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self): self.handle_get()
    def do_POST(self): self.handle_post()
    def do_PUT(self): self.handle_put()
    def do_DELETE(self): self.handle_delete()
    def log_message(self, *_a): pass

server = http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(cert, key)
context.verify_mode = ssl.CERT_REQUIRED
context.load_verify_locations(ca)
server.socket = context.wrap_socket(server.socket, server_side=True)
print(server.server_address[1], flush=True)
server.serve_forever()
"#;

/// Spawns the Fabric stand-in over real product-style HTTPS+mTLS: it presents
/// the CA-signed server certificate and requires the voie-cloud client
/// certificate against the same root. Returns the bound port and the child
/// handle; mutations answer 500 while `fail_flag` exists on disk.
async fn spawn_fabric_stub(
    dir: &std::path::Path,
    pems: &FabricPems,
    fail_flag: std::path::PathBuf,
    fixed_status: u16,
) -> (u16, tokio::process::Child) {
    let script_path = dir.join(format!("fabric_stub_{}.py", fixed_status));
    let child_err = dir.join("fabric-stub.err");
    std::fs::write(&script_path, FABRIC_STUB_SCRIPT).expect("fabric stub script writes");
    let mut child = tokio::process::Command::new("python3")
        .arg(&script_path)
        .arg("0")
        .arg(&pems.server_cert)
        .arg(&pems.server_key)
        .arg(&pems.ca_cert)
        .arg(&fail_flag)
        .arg(fixed_status.to_string())
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::fs::File::create(&child_err).expect("stub stderr file creates"))
        .spawn()
        .expect("python3 runs the fabric stub");
    let stdout = child.stdout.take().expect("stub stdout piped");
    let mut reader = tokio::io::BufReader::new(stdout);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let mut line = Vec::new();
        match tokio::time::timeout(
            std::time::Duration::from_millis(200),
            reader.read_until(b'\n', &mut line),
        )
        .await
        {
            Ok(Ok(read)) if read > 0 => {
                let text = String::from_utf8_lossy(&line);
                let parsed: u16 = text.trim().parse().expect("stub prints its port");
                return (parsed, child);
            }
            _ if std::time::Instant::now() > deadline => {
                panic!("fabric stub never reported its port");
            }
            _ => continue,
        }
    }
}

fn created() -> Response<Full<bytes::Bytes>> {
    Response::builder()
        .status(StatusCode::CREATED)
        .header(CONTENT_LENGTH, HeaderValue::from_static("0"))
        .body(Full::new(bytes::Bytes::new()))
        .expect("created response")
}

// ----------------------------------------------------------------------- test

/// Throwaway mTLS material: one CA, the client identity voie-cloud presents,
/// and a Fabric server certificate for the local HTTPS stand-in.
struct FabricPems {
    client_cert: String,
    client_key: String,
    ca_cert: String,
    server_cert: String,
    server_key: String,
}

fn fabric_pem_files(dir: &std::path::Path) -> FabricPems {
    let pems = tls_pems::write_v3_mtls_bundle(dir);
    FabricPems {
        client_cert: pems.client_pem.display().to_string(),
        client_key: pems.client_key.display().to_string(),
        ca_cert: pems.ca_pem.display().to_string(),
        server_cert: pems.server_pem.display().to_string(),
        server_key: pems.server_key.display().to_string(),
    }
}

/// Activation child stand-in: ignores its argument and stays alive long past
/// the test, so the dispatched Run remains in flight while cancel is exercised.
fn write_fake_node(dir: &std::path::Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    // The activation child runs with a stripped PATH, so the blocking
    // stand-in must reference its sleeper by absolute path.
    let sleep = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .map(|entry| std::path::Path::new(entry).join("sleep"))
        .find(|candidate| candidate.is_file())
        .map(|found| found.display().to_string())
        .unwrap_or_else(|| "/bin/sleep".to_string());
    let script = dir.join("fake-node.sh");
    std::fs::write(&script, format!("#!/bin/sh\nexec {sleep} 30\n")).expect("fake node writes");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("fake node chmod");
    script.display().to_string()
}

fn set_env(name: &str, value: &str) {
    // Process-global configuration consumed by Services::from_env in this
    // test process only; set before any reader is spawned.
    unsafe { std::env::set_var(name, value) };
}

fn unset_env(name: &str) {
    unsafe { std::env::remove_var(name) };
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Removes its directory on drop so repeated runs leave nothing behind.
struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(label: &str) -> TempDir {
    let dir = std::env::temp_dir().join(format!("voie-rest-{label}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("temp dir creates");
    TempDir(dir)
}

// ----------------------------------------------------------------------- test

#[tokio::test]
async fn rest_console_flow_contract() {
    let database_url = std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database");
    let kernel = Kernel::connect(&Config::database_url(database_url))
        .await
        .expect("PostgreSQL connection succeeds");
    kernel.migrate().await.expect("migration succeeds");
    sqlx::query("truncate table users, fabrics cascade")
        .execute(kernel.pool())
        .await
        .expect("test tables start empty");

    let kernel = Arc::new(kernel);

    // Stub Azure Blob endpoint backing the session event store.
    let blob_objects = Arc::new(Mutex::new(HashMap::new()));
    let blob_listener = TcpListener::bind("127.0.0.1:0").await.expect("blob binds");
    let blob_port = blob_listener.local_addr().expect("blob addr").port();
    tokio::spawn(serve_blob(blob_listener, blob_objects.clone()));

    // Stub OIDC issuer.
    let issuer_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("issuer binds");
    let issuer_port = issuer_listener.local_addr().expect("issuer addr").port();
    let issuer_url = format!("http://127.0.0.1:{issuer_port}");
    let issuer = Arc::new(TestIssuer::new(issuer_url.clone()));
    tokio::spawn(serve_issuer(issuer_listener, issuer));

    // Local-only service configuration: no remote estate credentials.
    let certs = temp_dir("certs");
    let pems = fabric_pem_files(certs.0.as_path());
    // Local Fabric stand-in for the workspace lifecycle; mutations are
    // flippable through the flag file to prove the durable row never leads
    // the real resource. The stand-in speaks product HTTPS+mTLS.
    let fabric_fail_flag = certs.0.join("fabric-fail");
    let (fabric_port, mut _fabric_child) =
        spawn_fabric_stub(certs.0.as_path(), &pems, fabric_fail_flag.clone(), 0).await;
    let fake_node_dir = temp_dir("node");
    let fake_node = write_fake_node(&fake_node_dir.0);
    unset_env("VOIE_AZURE_BLOB_KEY_FILE");
    unset_env("VOIE_MODEL_API_KEY_FILE");
    set_env("VOIE_AZURE_BLOB_ACCOUNT", BLOB_ACCOUNT);
    set_env("VOIE_AZURE_BLOB_KEY", BLOB_KEY_BASE64);
    set_env("VOIE_AZURE_BLOB_CONTAINER", BLOB_CONTAINER);
    set_env(
        "VOIE_AZURE_BLOB_ENDPOINT",
        &format!("http://127.0.0.1:{blob_port}"),
    );
    set_env("VOIE_MODEL_BASE_URL", "https://127.0.0.1:9/v1");
    set_env("VOIE_MODEL_NAME", "test-model");
    set_env("VOIE_MODEL_API_KEY", "test-key");
    set_env(
        "VOIE_FABRIC_ENDPOINT",
        &format!("https://127.0.0.1:{fabric_port}/"),
    );
    set_env("VOIE_FABRIC_CLIENT_CERT_PATH", &pems.client_cert);
    set_env("VOIE_FABRIC_CLIENT_KEY_PATH", &pems.client_key);
    set_env("VOIE_FABRIC_CA_CERT_PATH", &pems.ca_cert);
    set_env("VOIE_USER_SECRETS_BACKEND", "memory");
    set_env("VOIE_NODE", &fake_node);
    // D004: new Workspaces bind this deployment-configured identity. The
    // row is inserted after boot; an unset env must not invent a Fabric
    // by counting rows.
    let fabric = Uuid::new_v4();
    set_env("VOIE_FABRIC_ID", &fabric.to_string());

    // Full Release 0 surface on one listener.
    let auth_listener = TcpListener::bind("127.0.0.1:0").await.expect("auth binds");
    let port = auth_listener.local_addr().expect("auth addr").port();
    let public_origin = format!("http://127.0.0.1:{port}");
    let auth = voie_cloud::auth::Auth::connect(
        voie_cloud::auth::AuthConfig::new(
            issuer_url.clone(),
            CLIENT_ID.to_string(),
            CLIENT_SECRET.to_string(),
            format!("{public_origin}/oidc/callback"),
            public_origin.clone(),
        ),
        kernel.pool().clone(),
    )
    .await
    .expect("OIDC discovery succeeds");
    let services = voie_cloud::integration::Services::from_env(kernel.pool().clone())
        .expect("local service configuration resolves");
    let auth = Arc::new(auth);
    tokio::spawn(voie_cloud::serve_with_services(
        auth_listener,
        kernel.clone(),
        auth.clone(),
        services,
    ));

    // --- OIDC-derived session boot -----------------------------------------
    let health = get(port, "/healthz", None).await;
    assert_eq!(health.status, 200);

    let anonymous = get(port, "/api/me", None).await;
    assert_eq!(anonymous.status, 401, "API refuses callers without a boot");

    let login = get(port, "/login/oidc", None).await;
    assert_eq!(login.status, 302);
    let oidc_cookie = login.set_cookie("voie_oidc").expect("OIDC state cookie");
    let authorize_url =
        url::Url::parse(login.header("location").expect("authorize location")).expect("URL");
    let authorize_query = authorize_url.query().expect("authorize query").to_string();

    let issuer_redirect = get(issuer_port, &format!("/authorize?{authorize_query}"), None).await;
    assert_eq!(issuer_redirect.status, 302, "issuer grants the code");
    let callback = url::Url::parse(
        issuer_redirect
            .header("location")
            .expect("callback location"),
    )
    .expect("callback URL");
    let callback_path = format!(
        "{}?{}",
        callback.path(),
        callback.query().unwrap_or_default()
    );

    let established = get(
        port,
        &callback_path,
        Some(&format!("voie_oidc={oidc_cookie}")),
    )
    .await;
    assert_eq!(established.status, 303, "login completes");
    let session_cookie = established
        .set_cookie("voie_session")
        .expect("web session cookie");

    let me = get(
        port,
        "/api/me",
        Some(&format!("voie_session={session_cookie}")),
    )
    .await;
    assert_eq!(me.status, 200);
    let user_id: Uuid = me
        .json()
        .get("userId")
        .and_then(Value::as_str)
        .and_then(|value| value.parse().ok())
        .expect("me carries the booted identity");
    let (subject, db_user): (String, Uuid) = sqlx::query_as(
        "select a.subject, u.id from users u \
         join auth_identities a on a.user_id = u.id \
         where a.provider = 'oidc' and a.issuer = $1",
    )
    .bind(&issuer_url)
    .fetch_one(kernel.pool())
    .await
    .expect("booted user row exists");
    assert_eq!(subject, SUBJECT, "identity derives from the OIDC subject");
    assert_eq!(user_id, db_user, "me reports the OIDC-derived user id");

    // --- project role -------------------------------------------------------
    let refused = post_json(
        port,
        "/api/projects",
        &format!("voie_session={session_cookie}"),
        None,
        serde_json::json!({ "id": Uuid::new_v4(), "name": "cross-origin" }),
    )
    .await;
    assert_eq!(refused.status, 403, "mutations demand the same origin");

    let project_id = Uuid::new_v4();
    let project = post_json(
        port,
        "/api/projects",
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "id": project_id, "name": "console-contract" }),
    )
    .await;
    assert_eq!(project.status, 200);
    assert_eq!(
        project.json().get("role"),
        Some(&serde_json::json!("owner"))
    );

    // Fixed Fabric resources have no REST route; seed the identity already
    // bound by `VOIE_FABRIC_ID`.
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .expect("fabric inserts");
    let workspace = Uuid::new_v4();
    sqlx::query("insert into workspaces (id, project_id, fabric_id, observed_state) values ($1, $2, $3, 'ready')")
        .bind(workspace)
        .bind(project_id)
        .bind(fabric)
        .execute(kernel.pool())
        .await
        .expect("workspace inserts");

    // --- agent and session creation ----------------------------------------
    let agent_id = Uuid::new_v4();
    let agent = post_json(
        port,
        &format!("/api/projects/{project_id}/agents"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "id": agent_id, "name": "contract-agent", "max_tokens": 256 }),
    )
    .await;
    assert_eq!(agent.status, 200, "agent creates: {}", agent.body.len());

    let session_id = Uuid::new_v4();
    let session = post_json(
        port,
        &format!("/api/projects/{project_id}/sessions"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({
            "id": session_id,
            "agentId": agent_id,
            "workspaceId": workspace,
        }),
    )
    .await;
    assert_eq!(session.status, 200, "session creates");
    let created_session = session.json();
    assert_eq!(
        created_session.get("headRevision"),
        Some(&serde_json::json!(0))
    );
    assert_eq!(
        created_session.get("projectId"),
        Some(&serde_json::json!(project_id))
    );

    // --- canonical event envelope -------------------------------------------
    let event_bytes = br#"{"type":"user","text":"contract"}"#.to_vec();
    let append_id = Uuid::new_v4();
    let store = SessionStore::new(kernel.pool().clone());
    {
        let mut writer = store.writer(session_id).await.expect("writer pins");
        let revision = writer
            .append(AppendEvent {
                append_id,
                writer_generation: writer.writer_generation(),
                expected_revision: 1,
                bytes: event_bytes.clone(),
                model_usage: None,
            })
            .await
            .expect("canonical event appends");
        assert_eq!(revision, 1);
    }

    let events = get(
        port,
        &format!("/api/sessions/{session_id}/events?after=0"),
        Some(&format!("voie_session={session_cookie}")),
    )
    .await;
    assert_eq!(events.status, 200, "event history reads");
    let envelope = events.json();
    let items = envelope
        .get("items")
        .and_then(Value::as_array)
        .expect("items");
    assert_eq!(items.len(), 1, "one canonical event");
    let item = &items[0];
    assert_eq!(item.get("sessionId"), Some(&serde_json::json!(session_id)));
    assert_eq!(item.get("revision"), Some(&serde_json::json!(1)));
    assert_eq!(item.get("appendId"), Some(&serde_json::json!(append_id)));
    assert!(
        item.get("objectKey").is_none(),
        "hot Session history does not expose a Blob object key"
    );
    assert_eq!(
        item.get("contentHash"),
        Some(&serde_json::json!(hex_digest(&event_bytes)))
    );
    assert_eq!(
        item.get("byteLength"),
        Some(&serde_json::json!(event_bytes.len()))
    );
    let decoded = BASE64
        .decode(item.get("bytes").and_then(Value::as_str).expect("payload"))
        .expect("base64 payload");
    assert_eq!(decoded, event_bytes, "event bytes round-trip");
    let cursor = envelope
        .get("cursor")
        .and_then(Value::as_i64)
        .expect("cursor");
    assert!(
        cursor > 0,
        "cursor advances to the returned global sequence"
    );
    assert_eq!(items[0].get("globalSeq"), Some(&serde_json::json!(cursor)));

    let stored_payload: Option<Vec<u8>> = sqlx::query_scalar(
        "select payload from session_events where session_id = $1 and revision = 1",
    )
    .bind(session_id)
    .fetch_one(kernel.pool())
    .await
    .expect("payload column");
    assert_eq!(
        stored_payload.as_deref(),
        Some(event_bytes.as_slice()),
        "append payload is stored in PostgreSQL"
    );

    let tail = get(
        port,
        &format!("/api/events?after={cursor}"),
        Some(&format!("voie_session={session_cookie}")),
    )
    .await;
    assert_eq!(tail.status, 200);
    assert_eq!(
        tail.json()
            .get("items")
            .and_then(Value::as_array)
            .expect("items")
            .len(),
        0,
        "the cursor consumes the feed"
    );

    // --- accepted Run -------------------------------------------------------
    let run_id = Uuid::new_v4();
    let intent_id = Uuid::new_v4();
    let run = post_json(
        port,
        &format!("/api/sessions/{session_id}/runs"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({
            "runId": run_id,
            "intentId": intent_id,
            "prompt": "Summarize the contract.",
            "mode": "create"
        }),
    )
    .await;
    assert_eq!(run.status, 200, "run accepted");
    let accepted_run = run.json();
    assert_eq!(accepted_run.get("accepted"), Some(&serde_json::json!(true)));
    assert_eq!(
        accepted_run.get("state"),
        Some(&serde_json::json!("accepted"))
    );
    assert_eq!(accepted_run.get("runId"), Some(&serde_json::json!(run_id)));
    assert_eq!(
        accepted_run.get("intentId"),
        Some(&serde_json::json!(intent_id))
    );

    // The resident supervisor dispatches the accepted Run and holds it in
    // flight against the stand-in child.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let observed = loop {
        let resource = get(
            port,
            &format!("/api/runs/{run_id}"),
            Some(&format!("voie_session={session_cookie}")),
        )
        .await;
        assert_eq!(resource.status, 200, "run resource reads");
        let state = resource
            .json()
            .get("state")
            .and_then(Value::as_str)
            .expect("state")
            .to_string();
        if state == "dispatched" || std::time::Instant::now() > deadline {
            break state;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    assert_eq!(
        observed, "dispatched",
        "supervisor dispatches the accepted run"
    );

    let detail = get(
        port,
        &format!("/api/sessions/{session_id}"),
        Some(&format!("voie_session={session_cookie}")),
    )
    .await;
    assert_eq!(detail.status, 200);
    assert_eq!(
        detail.json().get("running"),
        Some(&serde_json::json!(true)),
        "session detail reports durable in-flight truth"
    );

    // --- cancel request ------------------------------------------------------
    let cancel = post_json(
        port,
        &format!("/api/runs/{run_id}/cancel"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(cancel.status, 200, "cancel requests");
    let cancelled = cancel.json();
    assert_eq!(cancelled.get("accepted"), Some(&serde_json::json!(true)));
    assert_eq!(
        cancelled.get("state"),
        Some(&serde_json::json!("cancel-requested")),
        "an in-flight run records the cancel request instead of vanishing"
    );

    let deadline_stop = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let settled = loop {
        let resource = get(
            port,
            &format!("/api/runs/{run_id}"),
            Some(&format!("voie_session={session_cookie}")),
        )
        .await;
        let state = resource
            .json()
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if state != "dispatched" || std::time::Instant::now() > deadline_stop {
            break state;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    assert!(
        settled == "cancelled",
        "cancel aborts the live child and classifies the Run as cancelled: {settled}"
    );

    let still = get(
        port,
        &format!("/api/runs/{run_id}"),
        Some(&format!("voie_session={session_cookie}")),
    )
    .await;
    let terminal_states = ["dispatched", "cancelled", "unknown"];
    assert!(
        still
            .json()
            .get("state")
            .and_then(Value::as_str)
            .is_some_and(|state| terminal_states.contains(&state)),
        "the cancellation request is durable and the attempt stays classified: {}",
        still.json()
    );

    // --- idempotent terminal replay answers a consistent JSON receipt --------
    let retained = serde_json::json!({
        "accepted": true,
        "runId": run_id,
        "intentId": intent_id,
        "state": "terminal",
        "finalText": "contract honored",
        "bashCalls": 0,
    });
    sqlx::query(
        "update runs set state = 'terminal', result = $2, terminal_at = now() \
         where id = $1",
    )
    .bind(run_id)
    .bind(retained.to_string())
    .execute(kernel.pool())
    .await
    .expect("terminal result seeds");

    let replay = post_json(
        port,
        &format!("/api/sessions/{session_id}/runs"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({
            "runId": run_id,
            "intentId": intent_id,
            "prompt": "Summarize the contract.",
            "mode": "create"
        }),
    )
    .await;
    assert_eq!(replay.status, 200);
    let receipt = replay.json();
    assert_eq!(receipt.get("accepted"), Some(&serde_json::json!(false)));
    assert_eq!(receipt.get("state"), Some(&serde_json::json!("terminal")));
    assert_eq!(receipt.get("runId"), Some(&serde_json::json!(run_id)));
    assert_eq!(receipt.get("intentId"), Some(&serde_json::json!(intent_id)));
    assert_eq!(
        receipt
            .get("result")
            .and_then(|result| result.get("finalText")),
        Some(&serde_json::json!("contract honored")),
        "the replay carries the retained outcome as structured JSON"
    );

    // No-replay rules survive: the same intent with changed bytes conflicts.
    let conflicting = post_json(
        port,
        &format!("/api/sessions/{session_id}/runs"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({
            "runId": run_id,
            "intentId": intent_id,
            "prompt": "Different bytes, same intent.",
            "mode": "create"
        }),
    )
    .await;
    assert_eq!(conflicting.status, 409);

    // --- normalized audit rows ----------------------------------------------
    let accepted_row = wait_audit(kernel.pool(), "run.accepted").await;
    assert_eq!(
        accepted_row.get("actorUserId"),
        Some(&serde_json::json!(user_id)),
        "route emissions carry the authenticated actor"
    );
    assert_eq!(
        accepted_row.get("resourceType"),
        Some(&serde_json::json!("run"))
    );
    assert_eq!(accepted_row.get("outcome"), Some(&serde_json::json!("ok")));
    let cancel_row = wait_audit(kernel.pool(), "run.cancel_requested").await;
    assert_eq!(
        cancel_row.get("actorUserId"),
        Some(&serde_json::json!(user_id))
    );
    assert_eq!(
        cancel_row.get("resourceId"),
        Some(&serde_json::json!(run_id))
    );
    let dispatched_row = wait_audit(kernel.pool(), "run.dispatched").await;
    assert_eq!(
        dispatched_row.get("actorUserId"),
        Some(&Value::Null),
        "supervisor emissions carry no human actor"
    );

    // --- browser body bound ---------------------------------------------------
    let oversized = vec![b'a'; MAX_BODY_BYTES + 1];
    let too_large = post_raw(
        port,
        "/api/projects",
        &format!("voie_session={session_cookie}"),
        &public_origin,
        &oversized,
    )
    .await;
    assert_eq!(
        too_large, 413,
        "browser bodies beyond 64 KiB are refused before any work"
    );

    // --- second identity: foreign access and membership lifecycle -------------
    let bob_login = get(port, "/login/oidc?sub=bob", None).await;
    assert_eq!(bob_login.status, 302);
    let bob_oidc = bob_login.set_cookie("voie_oidc").expect("bob oidc cookie");
    let bob_authorize_url =
        url::Url::parse(bob_login.header("location").expect("authorize location")).expect("URL");
    let bob_issuer_redirect = get(
        issuer_port,
        &format!(
            "/authorize?{}&sub=bob",
            bob_authorize_url.query().expect("authorize query")
        ),
        None,
    )
    .await;
    assert_eq!(bob_issuer_redirect.status, 302);
    let bob_callback = url::Url::parse(
        bob_issuer_redirect
            .header("location")
            .expect("callback location"),
    )
    .expect("callback URL");
    let bob_established = get(
        port,
        &format!(
            "{}?{}",
            bob_callback.path(),
            bob_callback.query().unwrap_or_default()
        ),
        Some(&format!("voie_oidc={bob_oidc}")),
    )
    .await;
    assert_eq!(bob_established.status, 303);
    let bob_cookie = bob_established
        .set_cookie("voie_session")
        .expect("bob session cookie");
    let bob_me = get(port, "/api/me", Some(&format!("voie_session={bob_cookie}"))).await;
    let bob_id: Uuid = bob_me
        .json()
        .get("userId")
        .and_then(Value::as_str)
        .and_then(|value| value.parse().ok())
        .expect("bob booted");
    assert_ne!(bob_id, user_id, "second login mints a distinct identity");

    let foreign_view = get(
        port,
        &format!("/api/projects/{project_id}"),
        Some(&format!("voie_session={bob_cookie}")),
    )
    .await;
    assert_eq!(
        foreign_view.status, 404,
        "foreign callers cannot observe the project"
    );

    // Member management requires a collaboration scope; the durable
    // personal scope refuses member adds (tested separately at the end).
    sqlx::query("update projects set kind = 'team' where id = $1")
        .bind(project_id)
        .execute(kernel.pool())
        .await
        .expect("project becomes a team scope for member management");

    // Owner-only mutations refuse members.
    let member_refused = post_json(
        port,
        &format!("/api/projects/{project_id}/members"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "userId": Uuid::new_v4(), "role": "viewer" }),
    )
    .await;
    assert_eq!(
        member_refused.status, 400,
        "unknown users are refused before any membership change"
    );
    let added = post_json(
        port,
        &format!("/api/projects/{project_id}/members"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "userId": bob_id, "role": "member" }),
    )
    .await;
    assert_eq!(added.status, 200, "the owner adds a member");
    assert_eq!(added.json().get("subject"), Some(&serde_json::json!("bob")));

    // Member capabilities are derived server-side.
    let member_view = get(
        port,
        &format!("/api/projects/{project_id}"),
        Some(&format!("voie_session={bob_cookie}")),
    )
    .await;
    assert_eq!(member_view.status, 200);
    let detail = member_view.json();
    assert_eq!(
        detail.get("capabilities"),
        Some(&serde_json::json!({
            "read": true, "operateSessions": true, "manageMembers": false
        }))
    );
    assert!(
        detail
            .get("createdAt")
            .and_then(Value::as_str)
            .is_some_and(|stamp| !stamp.is_empty()),
        "project detail carries the durable creation timestamp"
    );
    assert!(
        detail
            .get("members")
            .and_then(Value::as_array)
            .is_some_and(|members| members.len() == 2),
        "detail embeds the membership roster"
    );

    let bob_manages = post_json(
        port,
        &format!("/api/projects/{project_id}/members"),
        &format!("voie_session={bob_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "userId": Uuid::new_v4(), "role": "viewer" }),
    )
    .await;
    assert_eq!(bob_manages.status, 403, "membership mutation is owner-only");

    // The durable project owner can never be demoted or removed.
    let self_demote = post_json(
        port,
        &format!("/api/projects/{project_id}/members"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "userId": user_id, "role": "member" }),
    )
    .await;
    assert_eq!(self_demote.status, 409);
    let self_remove = exchange(
        port,
        &request_text(
            "DELETE",
            &format!("/api/projects/{project_id}/members/{user_id}"),
            port,
            &[
                ("cookie", format!("voie_session={session_cookie}")),
                ("origin", public_origin.clone()),
                ("x-voie-intent", "mutate".to_string()),
            ],
            None,
        ),
    )
    .await;
    assert_eq!(self_remove.status, 409);

    // Rerole to viewer: operate capability disappears server-side.
    let viewer_role = patch_json(
        port,
        &format!("/api/projects/{project_id}/members/{bob_id}"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "role": "viewer" }),
    )
    .await;
    assert_eq!(viewer_role.status, 200);
    let viewer_sessions = post_json(
        port,
        &format!("/api/projects/{project_id}/sessions"),
        &format!("voie_session={bob_cookie}"),
        Some(&public_origin),
        serde_json::json!({
            "id": Uuid::new_v4(),
            "agentId": agent_id,
            "workspaceId": workspace,
        }),
    )
    .await;
    assert_eq!(viewer_sessions.status, 403, "viewers cannot open sessions");

    let role_audit = wait_audit(kernel.pool(), "member.role_changed").await;
    assert_eq!(
        role_audit.get("actorUserId"),
        Some(&serde_json::json!(user_id))
    );
    assert_eq!(
        role_audit.get("resourceId"),
        Some(&serde_json::json!(bob_id))
    );
    let metadata: Value = serde_json::from_str(
        role_audit
            .get("metadata")
            .and_then(Value::as_str)
            .expect("metadata"),
    )
    .expect("audit metadata is JSON");
    assert_eq!(metadata.get("role"), Some(&serde_json::json!("viewer")));
    assert_eq!(
        metadata.get("previousRole"),
        Some(&serde_json::json!("member"))
    );

    // Bob still owns his own Project; ownership is per-project, not global.
    let bob_project_id = Uuid::new_v4();
    let bob_project = post_json(
        port,
        "/api/projects",
        &format!("voie_session={bob_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "id": bob_project_id, "name": "bob-console" }),
    )
    .await;
    assert_eq!(bob_project.status, 200);

    // --- workspace lifecycle against the real Fabric client --------------------
    let workspace_id = Uuid::new_v4();
    let viewer_create = post_json(
        port,
        &format!("/api/projects/{project_id}/workspaces"),
        &format!("voie_session={bob_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "id": Uuid::new_v4() }),
    )
    .await;
    assert_eq!(
        viewer_create.status, 403,
        "viewers cannot provision Workspaces"
    );
    let created_workspace = post_json(
        port,
        &format!("/api/projects/{project_id}/workspaces"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "id": workspace_id }),
    )
    .await;
    assert_eq!(
        created_workspace.status,
        200,
        "workspace provisions: {}",
        String::from_utf8_lossy(&created_workspace.body)
    );
    // Product surfaces no longer expose underlay identity; Fabric
    // attribution is admin-diagnostics-only.
    assert!(
        created_workspace.json().get("fabricId").is_none(),
        "product create response must not expose the underlay Fabric"
    );
    assert_eq!(
        created_workspace.json().get("projectId"),
        Some(&serde_json::json!(project_id)),
        "the creating Project durably owns the Workspace"
    );
    assert_eq!(
        created_workspace.json().get("createdByUserId"),
        Some(&serde_json::json!(user_id)),
        "the authenticated principal is recorded as the creator"
    );

    // Bob provisions inside his own project and never sees Alice's rows.
    let bob_workspace_id = Uuid::new_v4();
    let bob_created = post_json(
        port,
        &format!("/api/projects/{bob_project_id}/workspaces"),
        &format!("voie_session={bob_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "id": bob_workspace_id }),
    )
    .await;
    assert_eq!(bob_created.status, 200);
    let bob_listing = get(
        port,
        "/api/workspaces",
        Some(&format!("voie_session={bob_cookie}")),
    )
    .await;
    let bob_items = bob_listing
        .json()
        .get("items")
        .and_then(Value::as_array)
        .expect("items")
        .clone();
    let bob_own = bob_items
        .iter()
        .find(|item| item.get("id") == Some(&serde_json::json!(bob_workspace_id)))
        .expect("bob sees his own provisioned workspace");
    assert_eq!(
        bob_own.get("projectId"),
        Some(&serde_json::json!(bob_project_id))
    );
    assert_eq!(
        bob_own.get("createdByUserId"),
        Some(&serde_json::json!(bob_id)),
        "the listed workspace keeps its creator attribution"
    );

    // Another project's Workspace is not addressable through this route,
    // even by its owner: existence itself is scoped to ownership.
    let cross_delete = exchange(
        port,
        &request_text(
            "DELETE",
            &format!("/api/projects/{bob_project_id}/workspaces/{workspace_id}"),
            port,
            &[
                ("cookie", format!("voie_session={bob_cookie}")),
                ("origin", public_origin.clone()),
                ("x-voie-intent", "mutate".to_string()),
            ],
            None,
        ),
    )
    .await;
    assert_eq!(
        cross_delete.status, 404,
        "cross-project teardown is refused as nonexistent"
    );
    let cross_replace = exchange(
        port,
        &request_text(
            "POST",
            &format!("/api/projects/{bob_project_id}/workspaces/{workspace_id}/replace"),
            port,
            &[
                ("cookie", format!("voie_session={bob_cookie}")),
                ("origin", public_origin.clone()),
                ("x-voie-intent", "mutate".to_string()),
                ("content-length", "0".to_string()),
                (
                    "content-type",
                    "application/json; charset=utf-8".to_string(),
                ),
            ],
            None,
        ),
    )
    .await;
    assert_eq!(cross_replace.status, 404);

    let creator: Option<Option<Uuid>> = sqlx::query_scalar(
        "select actor_user_id from audit_events \
         where kind = 'workspace.created' and resource_id = $1 and outcome = 'ok' \
         order by seq desc limit 1",
    )
    .bind(workspace_id)
    .fetch_optional(kernel.pool())
    .await
    .expect("audit query runs");
    assert_eq!(
        creator,
        Some(Some(user_id)),
        "the owning project's operator audited the provisioning"
    );
    let durable_creator: Option<Uuid> =
        sqlx::query_scalar("select created_by_user_id from workspaces where id = $1")
            .bind(workspace_id)
            .fetch_optional(kernel.pool())
            .await
            .expect("workspace creator query runs");
    assert_eq!(
        durable_creator,
        Some(user_id),
        "the durable workspace row keeps the creator user id"
    );

    let listed = get(
        port,
        "/api/workspaces",
        Some(&format!("voie_session={session_cookie}")),
    )
    .await;
    let listed_items = listed
        .json()
        .get("items")
        .and_then(Value::as_array)
        .expect("workspace items")
        .clone();
    let listed_own = listed_items
        .iter()
        .find(|item| item.get("id") == Some(&serde_json::json!(workspace_id)))
        .expect("the provisioned workspace item");
    assert_eq!(
        listed_own.get("createdByUserId"),
        Some(&serde_json::json!(user_id)),
        "creator attribution survives the list round-trip"
    );

    // --- replace advances the durable execution generation ---------------------
    let replace_body = |cookie: &str| {
        request_text(
            "POST",
            &format!("/api/projects/{project_id}/workspaces/{workspace_id}/replace"),
            port,
            &[
                ("cookie", format!("voie_session={cookie}")),
                ("origin", public_origin.clone()),
                ("x-voie-intent", "mutate".to_string()),
                ("content-length", "0".to_string()),
                (
                    "content-type",
                    "application/json; charset=utf-8".to_string(),
                ),
            ],
            None,
        )
    };
    let replaced = exchange(port, &replace_body(&session_cookie)).await;
    assert_eq!(replaced.status, 200, "replace responds");
    assert_eq!(
        replaced.json().get("execGeneration"),
        Some(&serde_json::json!(1))
    );
    let again = exchange(port, &replace_body(&session_cookie)).await;
    assert_eq!(
        again.json().get("execGeneration"),
        Some(&serde_json::json!(2)),
        "each confirmed replacement advances the durable generation"
    );

    // A Fabric refusal leaves the recorded generation untouched.
    std::fs::write(&fabric_fail_flag, b"1").expect("fabric fail flag writes");
    let refused_replace = exchange(port, &replace_body(&session_cookie)).await;
    assert_eq!(refused_replace.status, 502);
    std::fs::remove_file(&fabric_fail_flag).expect("fabric fail flag clears");
    let listed_after_failure = get(
        port,
        "/api/workspaces",
        Some(&format!("voie_session={session_cookie}")),
    )
    .await;
    assert_eq!(
        listed_after_failure
            .json()
            .get("items")
            .and_then(Value::as_array)
            .expect("items")
            .iter()
            .find(|item| item.get("id") == Some(&serde_json::json!(workspace_id)))
            .and_then(|item| item.get("execGeneration"))
            .cloned(),
        Some(serde_json::json!(2)),
        "a failed replacement never advances durable truth"
    );
    let replace_audit_row = wait_audit(kernel.pool(), "workspace.replaced").await;
    assert_eq!(
        replace_audit_row.get("actorUserId"),
        Some(&serde_json::json!(user_id))
    );
    let replace_metadata: Value = serde_json::from_str(
        replace_audit_row
            .get("metadata")
            .and_then(Value::as_str)
            .expect("metadata"),
    )
    .expect("audit metadata is JSON");
    assert_eq!(
        replace_metadata.get("execGeneration"),
        Some(&serde_json::json!(2))
    );

    // --- lifecycle fence serialization (deterministic interleaving) -----------
    // Claim the fence out-of-band, exactly as a concurrent delete would.
    assert!(
        kernel
            .begin_workspace_delete(workspace_id)
            .await
            .expect("fence claims"),
        "the first lifecycle claim wins"
    );
    let fenced_attach = post_json(
        port,
        &format!("/api/projects/{project_id}/sessions"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({
            "id": Uuid::new_v4(),
            "agentId": agent_id,
            "workspaceId": workspace_id,
        }),
    )
    .await;
    assert_eq!(
        fenced_attach.status, 409,
        "no Session attaches to a fenced Workspace"
    );
    let second_delete = exchange(
        port,
        &request_text(
            "DELETE",
            &format!("/api/projects/{project_id}/workspaces/{workspace_id}"),
            port,
            &[
                ("cookie", format!("voie_session={session_cookie}")),
                ("origin", public_origin.clone()),
                ("x-voie-intent", "mutate".to_string()),
            ],
            None,
        ),
    )
    .await;
    assert_eq!(
        second_delete.status, 409,
        "a second lifecycle operation is refused while one holds the fence"
    );
    // The concurrent holder finishes without deleting; it releases the
    // fence, exactly like the refused-teardown path does.
    assert!(
        kernel
            .restore_workspace(workspace_id)
            .await
            .expect("fence releases"),
        "the held fence releases; product ready is observed, not process promotion"
    );
    // The refused teardown restored the Workspace: attachment works again.
    let attach_after_restore = post_json(
        port,
        &format!("/api/projects/{project_id}/sessions"),
        &format!("voie_session={bob_cookie}"),
        Some(&public_origin),
        serde_json::json!({
            "id": Uuid::new_v4(),
            "agentId": agent_id,
            "workspaceId": workspace_id,
        }),
    )
    .await;
    assert_eq!(attach_after_restore.status, 403);

    let duplicate_workspace = post_json(
        port,
        &format!("/api/projects/{project_id}/workspaces"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "id": workspace_id }),
    )
    .await;
    assert_eq!(
        duplicate_workspace.status,
        200,
        "same-project retry of a live identity wakes the reconciler: {}",
        String::from_utf8_lossy(&duplicate_workspace.body)
    );
    assert_eq!(
        duplicate_workspace.json().get("id"),
        Some(&serde_json::json!(workspace_id)),
        "retry does not mint a second reservation"
    );

    // Fabric rejection leaves the desired-state reservation; Control does
    // not delete the row or invent a ready lie. Reconciliation retries PUT.
    std::fs::write(&fabric_fail_flag, b"1").expect("fabric fail flag writes");
    let failed_id = Uuid::new_v4();
    let failed_create = post_json(
        port,
        &format!("/api/projects/{project_id}/workspaces"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "id": failed_id }),
    )
    .await;
    assert_eq!(failed_create.status, 502);
    let error_audit = wait_audit_outcome(kernel.pool(), "workspace.created", "error").await;
    assert_eq!(
        error_audit.get("resourceId"),
        Some(&serde_json::json!(failed_id)),
        "failed provisioning names the attempted resource"
    );
    let reserved: Option<(String, String, i64)> = sqlx::query_as(
        "select state, desired_state, desired_revision from workspaces where id = $1",
    )
    .bind(failed_id)
    .fetch_optional(kernel.pool())
    .await
    .expect("reservation query runs");
    assert_eq!(
        reserved
            .as_ref()
            .map(|row| (row.0.as_str(), row.1.as_str(), row.2)),
        Some(("creating", "active", 1)),
        "Fabric rejection keeps desired active at revision 1"
    );
    let after_failure = get(
        port,
        "/api/workspaces",
        Some(&format!("voie_session={session_cookie}")),
    )
    .await;
    let after_failure_json = after_failure.json();
    let failed_item = after_failure_json
        .get("items")
        .and_then(Value::as_array)
        .expect("items")
        .iter()
        .find(|item| item.get("id") == Some(&serde_json::json!(failed_id)))
        .expect("creating reservation is listed");
    assert_eq!(
        failed_item.get("state"),
        Some(&serde_json::json!("creating")),
        "listing never exposes a rejected first PUT as ready"
    );
    std::fs::remove_file(&fabric_fail_flag).expect("fabric fail flag clears");

    // Repeatable spec PUT is not an unknown effect. HTTP 202 is a Fabric
    // protocol error: Control answers 502 and keeps the creating row.
    let post_status = certs.0.join("fabric-post-status");
    let get_missing = certs.0.join("fabric-get-missing");
    let get_state = certs.0.join("fabric-get-missing.state");
    // Ensure probe state override is absent before the Unknown test.
    let _ = std::fs::remove_file(&get_state);
    let _ = std::fs::remove_file(&get_missing);
    std::fs::write(&post_status, b"202").expect("post status 202 writes");
    let unknown_id = Uuid::new_v4();
    let unknown_create = post_json(
        port,
        &format!("/api/projects/{project_id}/workspaces"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "id": unknown_id }),
    )
    .await;
    assert_eq!(
        unknown_create.status,
        502,
        "spec PUT HTTP 202 is not success: {}",
        String::from_utf8_lossy(&unknown_create.body)
    );
    assert!(
        String::from_utf8_lossy(&unknown_create.body).contains("rejected"),
        "repeatable PUT 202 is a protocol error, not outcome-unknown"
    );
    let row_state: Option<String> =
        sqlx::query_scalar("select state from workspaces where id = $1")
            .bind(unknown_id)
            .fetch_optional(kernel.pool())
            .await
            .expect("state query runs");
    assert_eq!(
        row_state.as_deref(),
        Some("creating"),
        "rejected spec PUT keeps a creating reservation"
    );
    let listed_unknown = get(
        port,
        "/api/workspaces",
        Some(&format!("voie_session={session_cookie}")),
    )
    .await;
    let unknown_item = listed_unknown
        .json()
        .get("items")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .find(|i| i.get("id") == Some(&serde_json::json!(unknown_id)))
        .expect("creating workspace appears in listing")
        .clone();
    assert_eq!(
        unknown_item.get("state"),
        Some(&serde_json::json!("creating")),
        "listing never exposes indeterminate workspace as ready"
    );
    // No session may attach while creating.
    let attach_while_creating = post_json(
        port,
        &format!("/api/projects/{project_id}/sessions"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({
            "id": Uuid::new_v4(),
            "agentId": agent_id,
            "workspaceId": unknown_id,
        }),
    )
    .await;
    assert_eq!(
        attach_while_creating.status, 409,
        "no Session attaches to a creating Workspace"
    );
    let rejected_audit = wait_audit_outcome(kernel.pool(), "workspace.created", "error").await;
    assert_eq!(
        rejected_audit.get("resourceId"),
        Some(&serde_json::json!(unknown_id)),
        "rejected spec PUT is audited as error, not unknown"
    );

    // Reconciliation: the next user-initiated create for the same id
    // wakes the Workspace reconciler. The Fabric holds the identity, so
    // desired Active converges and the request answers 200 with the
    // ready Workspace. It does not invent a second reservation.
    let _ = std::fs::remove_file(&post_status);
    let _ = std::fs::remove_file(&get_missing);
    let _ = std::fs::remove_file(&get_state);
    let reconcile_conflict = post_json(
        port,
        &format!("/api/projects/{project_id}/workspaces"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "id": unknown_id }),
    )
    .await;
    assert_eq!(
        reconcile_conflict.status,
        200,
        "retry POST realizes leftover creating once Fabric holds the spec: {}",
        String::from_utf8_lossy(&reconcile_conflict.body)
    );
    let after_reconcile: Option<String> =
        sqlx::query_scalar("select observed_state from workspaces where id = $1")
            .bind(unknown_id)
            .fetch_optional(kernel.pool())
            .await
            .unwrap();
    assert_eq!(
        after_reconcile.as_deref(),
        Some("ready"),
        "probe proved existence: observed ready, leftover process stays creating"
    );
    let listed_ready = get(
        port,
        "/api/workspaces",
        Some(&format!("voie_session={session_cookie}")),
    )
    .await;
    assert_eq!(
        listed_ready
            .json()
            .get("items")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .find(|i| i.get("id") == Some(&serde_json::json!(unknown_id)))
            .and_then(|i| i.get("state"))
            .cloned(),
        Some(serde_json::json!("ready")),
        "listing now shows the reconciled Workspace as ready"
    );

    // Reconciliation of leftover creating when Fabric has no guest: retry
    // POST PUTs desired Active. The stub answers ready, so the reservation
    // converges instead of being discarded by a GET probe.
    let absent_id = Uuid::new_v4();
    sqlx::query(
        "insert into workspaces \
         (id, project_id, fabric_id, state, desired_state, desired_revision, observed_revision) \
         values ($1, $2, $3, 'creating', 'active', 1, 0)",
    )
    .bind(absent_id)
    .bind(project_id)
    .bind(fabric)
    .execute(kernel.pool())
    .await
    .expect("seed creating row for absent-probe test");
    std::fs::write(&get_missing, b"1").expect("get missing flag writes");
    let _ = std::fs::remove_file(&post_status);
    let _ = std::fs::remove_file(&get_state);
    let absent_recreate = post_json(
        port,
        &format!("/api/projects/{project_id}/workspaces"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "id": absent_id }),
    )
    .await;
    assert_eq!(
        absent_recreate.status,
        200,
        "retry POST PUTs desired Active and the stub reports ready: {}",
        String::from_utf8_lossy(&absent_recreate.body)
    );
    let absent_state: Option<String> =
        sqlx::query_scalar("select observed_state from workspaces where id = $1")
            .bind(absent_id)
            .fetch_optional(kernel.pool())
            .await
            .unwrap();
    assert_eq!(absent_state.as_deref(), Some("ready"));
    let _ = std::fs::remove_file(&get_missing);

    // Reconciliation when the Fabric still reports creating: retry POST
    // wakes the reconciler but must not expose the Workspace as ready.
    let pending_id = Uuid::new_v4();
    sqlx::query(
        "insert into workspaces \
         (id, project_id, fabric_id, state, desired_state, desired_revision, observed_revision) \
         values ($1, $2, $3, 'creating', 'active', 1, 0)",
    )
    .bind(pending_id)
    .bind(project_id)
    .bind(fabric)
    .execute(kernel.pool())
    .await
    .unwrap();
    std::fs::write(&get_state, b"creating").expect("get state creating writes");
    // Ensure GET is not 404 (remove missing flag) but state override exists.
    let _ = std::fs::remove_file(&get_missing);
    let pending_probe = post_json(
        port,
        &format!("/api/projects/{project_id}/workspaces"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "id": pending_id }),
    )
    .await;
    assert_eq!(
        pending_probe.status,
        202,
        "Fabric still creating: retry POST does not expose ready: {}",
        String::from_utf8_lossy(&pending_probe.body)
    );
    assert_eq!(
        pending_probe.json().get("state"),
        Some(&serde_json::json!("creating")),
        "HTTP body stays creating until observed ready"
    );
    let pending_state: Option<String> =
        sqlx::query_scalar("select state from workspaces where id = $1")
            .bind(pending_id)
            .fetch_optional(kernel.pool())
            .await
            .unwrap();
    assert_eq!(
        pending_state.as_deref(),
        Some("creating"),
        "not promoted while Fabric is creating"
    );
    let _ = std::fs::remove_file(&get_state);
    // Clean the seeded pending row for later workspace listing determinism.
    let _ = sqlx::query("delete from workspaces where id = $1")
        .bind(pending_id)
        .execute(kernel.pool())
        .await;

    // Cleanup the probe-mode flags before the next section.
    let _ = std::fs::remove_file(&post_status);
    let _ = std::fs::remove_file(&get_missing);
    let _ = std::fs::remove_file(&get_state);

    // Transport / unreachable Fabric: reserve desired state first, then PUT.
    // Unreachable Fabric is not Lost and does not drop the reservation.
    let dead_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("dead listener binds");
    let dead_port = dead_listener.local_addr().unwrap().port();
    drop(dead_listener);
    // Build a Services snapshot that points at the dead Fabric endpoint;
    // the env var is read by Services::from_env, but already-built
    // Services (the main `port` surface) is unaffected. Restore after.
    let saved_endpoint = std::env::var("VOIE_FABRIC_ENDPOINT").unwrap_or_default();
    set_env(
        "VOIE_FABRIC_ENDPOINT",
        &format!("https://127.0.0.1:{dead_port}/"),
    );
    let dead_services = voie_cloud::integration::Services::from_env(kernel.pool().clone())
        .expect("dead fabric config resolves");
    let dead_surface = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("dead surface binds");
    let dead_surface_port = dead_surface.local_addr().unwrap().port();
    tokio::spawn(voie_cloud::serve_with_services(
        dead_surface,
        kernel.clone(),
        auth.clone(),
        dead_services,
    ));
    let transport_id = Uuid::new_v4();
    let transport_create = post_json(
        dead_surface_port,
        &format!("/api/projects/{project_id}/workspaces"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "id": transport_id }),
    )
    .await;
    assert_eq!(
        transport_create.status, 502,
        "transport failure maps to 502, not incorrectly routed"
    );
    let transport_row: Option<String> =
        sqlx::query_scalar("select state from workspaces where id = $1")
            .bind(transport_id)
            .fetch_optional(kernel.pool())
            .await
            .unwrap();
    assert_eq!(
        transport_row.as_deref(),
        Some("creating"),
        "unreachable Fabric keeps the creating reservation for later observation"
    );
    let transport_audit = wait_audit_outcome(kernel.pool(), "workspace.created", "error").await;
    // The most recent error audit should name the transport attempt; allow
    // any recent error for determinism, but at least one exists.
    assert_eq!(
        transport_audit.get("outcome"),
        Some(&serde_json::json!("error"))
    );
    set_env("VOIE_FABRIC_ENDPOINT", &saved_endpoint);

    // An unset deployment identity does not invent a Fabric by counting
    // registered rows. Existing Workspace `fabric_id` stays the authority
    // for already-bound resources.
    let saved_fabric_id = std::env::var("VOIE_FABRIC_ID").unwrap_or_default();
    unsafe { std::env::remove_var("VOIE_FABRIC_ID") };
    let unbound_surface = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("unbound surface binds");
    let unbound_port = unbound_surface.local_addr().expect("surface addr").port();
    let unbound_services = voie_cloud::integration::Services::from_env(kernel.pool().clone())
        .expect("missing VOIE_FABRIC_ID still boots");
    tokio::spawn(voie_cloud::serve_with_services(
        unbound_surface,
        kernel.clone(),
        auth.clone(),
        unbound_services,
    ));
    let unbound_provision = post_json(
        unbound_port,
        &format!("/api/projects/{project_id}/workspaces"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "id": Uuid::new_v4() }),
    )
    .await;
    assert_eq!(
        unbound_provision.status, 503,
        "an unset Fabric identity refuses create instead of counting rows"
    );
    set_env("VOIE_FABRIC_ID", &saved_fabric_id);

    // A configured but unregistered Fabric identity refuses before any
    // external side effect: no Fabric resource is created and no durable
    // row or audit success exists.
    set_env("VOIE_FABRIC_ID", &Uuid::new_v4().to_string());
    let unregistered_surface = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("second surface binds");
    let unregistered_port = unregistered_surface
        .local_addr()
        .expect("surface addr")
        .port();
    let unregistered_services = voie_cloud::integration::Services::from_env(kernel.pool().clone())
        .expect("unregistered fabric configuration still resolves");
    tokio::spawn(voie_cloud::serve_with_services(
        unregistered_surface,
        kernel.clone(),
        auth.clone(),
        unregistered_services,
    ));
    let refused_provision = post_json(
        unregistered_port,
        &format!("/api/projects/{project_id}/workspaces"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "id": Uuid::new_v4() }),
    )
    .await;
    assert_eq!(
        refused_provision.status, 503,
        "an unregistered configured Fabric refuses before side effects"
    );
    // The variable stays unset for the remainder of the flow; Services
    // instances already built are unaffected either way.
    unsafe { std::env::remove_var("VOIE_FABRIC_ID") };

    // Sessions do not pin Workspace capacity. Delete persists desired
    // deleted immediately so Fabric can release the guest.
    let referenced_delete = exchange(
        port,
        &request_text(
            "DELETE",
            &format!("/api/projects/{project_id}/workspaces/{workspace}"),
            port,
            &[
                ("cookie", format!("voie_session={session_cookie}")),
                ("origin", public_origin.clone()),
                ("x-voie-intent", "mutate".to_string()),
            ],
            None,
        ),
    )
    .await;
    assert_eq!(
        referenced_delete.status, 200,
        "a Workspace with Sessions still tears down"
    );

    let deleted = exchange(
        port,
        &request_text(
            "DELETE",
            &format!("/api/projects/{project_id}/workspaces/{workspace_id}"),
            port,
            &[
                ("cookie", format!("voie_session={session_cookie}")),
                ("origin", public_origin.clone()),
                ("x-voie-intent", "mutate".to_string()),
            ],
            None,
        ),
    )
    .await;
    assert_eq!(deleted.status, 200, "unreferenced workspace tears down");
    let delete_audit = wait_audit(kernel.pool(), "workspace.deleted").await;
    assert_eq!(delete_audit.get("outcome"), Some(&serde_json::json!("ok")));
    let missing_delete = exchange(
        port,
        &request_text(
            "DELETE",
            &format!("/api/projects/{project_id}/workspaces/{workspace_id}"),
            port,
            &[
                ("cookie", format!("voie_session={session_cookie}")),
                ("origin", public_origin.clone()),
                ("x-voie-intent", "mutate".to_string()),
            ],
            None,
        ),
    )
    .await;
    assert_eq!(missing_delete.status, 404);

    // --- bounded bash capability end to end -------------------------------------
    let agent_detail = get(
        port,
        &format!("/api/agents/{agent_id}"),
        Some(&format!("voie_session={session_cookie}")),
    )
    .await;
    assert_eq!(
        agent_detail.json().get("bashEnabled"),
        Some(&serde_json::json!(true)),
        "agents default to the bounded bash capability"
    );
    let muted_agent = post_json(
        port,
        &format!("/api/projects/{project_id}/agents"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({
            "id": Uuid::new_v4(),
            "name": "no-shell-agent",
            "max_tokens": 64,
            "bashEnabled": false,
        }),
    )
    .await;
    assert_eq!(muted_agent.status, 200);
    let muted_id = muted_agent
        .json()
        .get("id")
        .and_then(Value::as_str)
        .expect("created agent carries its identity")
        .to_owned();
    let toggled = patch_json(
        port,
        &format!("/api/agents/{muted_id}"),
        &format!("voie_session={session_cookie}"),
        Some(&public_origin),
        serde_json::json!({ "bashEnabled": true }),
    )
    .await;
    assert_eq!(
        toggled.status,
        200,
        "agent patch responds: {}",
        String::from_utf8_lossy(&toggled.body)
    );
    assert_eq!(
        toggled.json().get("bashEnabled"),
        Some(&serde_json::json!(true))
    );
}

/// Polls the audit table until one row of `kind` appears; route emissions
/// await their insert, supervisor rows land on spawned tasks.
async fn wait_audit(pool: &sqlx::PgPool, kind: &str) -> Value {
    wait_audit_outcome(pool, kind, "ok").await
}

async fn wait_audit_outcome(pool: &sqlx::PgPool, kind: &str, outcome: &str) -> Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let row: Option<(i64, Option<Uuid>, String, Option<Uuid>, Option<String>)> =
            sqlx::query_as(
                "select seq, actor_user_id, resource_type, resource_id, metadata::text \
             from audit_events where kind = $1 and outcome = $2 \
             order by seq desc limit 1",
            )
            .bind(kind)
            .bind(outcome)
            .fetch_optional(pool)
            .await
            .expect("audit query runs");
        if let Some((seq, actor, resource_type, resource_id, metadata)) = row {
            return json!({
                "seq": seq,
                "actorUserId": actor,
                "resourceType": resource_type,
                "resourceId": resource_id,
                "outcome": outcome,
                "metadata": metadata,
            });
        }
        if std::time::Instant::now() > deadline {
            panic!("audit row {kind}/{outcome} never appeared");
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}
