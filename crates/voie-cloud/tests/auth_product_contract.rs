//! Native and external authentication contracts through the complete
//! `serve_with_services` product surface. These tests deliberately avoid the
//! standalone `auth::serve` listener so routing regressions stay visible.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use chrono::{Duration as ChronoDuration, Utc};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{CONTENT_TYPE, HeaderValue, LOCATION};
use hyper::{Method, Request, Response, StatusCode};
use openidconnect::core::{
    CoreIdToken, CoreIdTokenClaims, CoreJsonWebKeySet, CoreJwsSigningAlgorithm,
    CoreRsaPrivateSigningKey,
};
use openidconnect::{
    Audience, EmptyAdditionalClaims, IssuerUrl, JsonWebKeyId, Nonce, PrivateSigningKey,
    StandardClaims, SubjectIdentifier,
};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;
use voie_cloud::auth::{Auth, AuthConfig};
use voie_cloud::web_session::{self, COOKIE_NAME, OIDC_STATE_COOKIE};
use voie_cloud::{Config, Kernel, serve_with_services};

const CLIENT_ID: &str = "voie-cloud-auth-product-test";
const CLIENT_SECRET: &str = "voie-cloud-auth-product-test-secret";
const ADMIN_USERNAME: &str = "root-admin";
const ADMIN_PASSWORD: &str = "correct-horse-battery-staple";
const LATER_ADMIN_USERNAME: &str = "later-admin";
const LATER_ADMIN_PASSWORD: &str = "later-bootstrap-password";
const RSA_PEM: &str = r#"-----BEGIN RSA PRIVATE KEY-----
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

struct HttpCall {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpCall {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn set_cookies(&self) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, value)| value.as_str())
            .collect()
    }

    fn cookie_value(&self, name: &str) -> Option<String> {
        self.set_cookies().into_iter().find_map(|cookie| {
            let (head, _) = cookie.split_once(';').unwrap_or((cookie, ""));
            let (key, value) = head.split_once('=')?;
            (key == name).then(|| value.to_string())
        })
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("response body is JSON")
    }
}

async fn http_exchange(host: &str, port: u16, request: &str) -> HttpCall {
    let mut stream = TcpStream::connect((host, port))
        .await
        .expect("HTTP listener accepts connections");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("HTTP request writes");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("HTTP response reads");
    let text = String::from_utf8_lossy(&response);
    let (head, body) = text.split_once("\r\n\r\n").expect("HTTP header terminator");
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
    HttpCall {
        status,
        headers,
        body: body.as_bytes().to_vec(),
    }
}

async fn get(port: u16, path: &str, extra_headers: &str) -> HttpCall {
    http_exchange(
        "127.0.0.1",
        port,
        &format!(
            "GET {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\nconnection: close\r\n{extra_headers}\r\n"
        ),
    )
    .await
}

async fn post_login(port: u16, origin: Option<&str>, username: &str, password: &str) -> HttpCall {
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("username", username)
        .append_pair("password", password)
        .finish();
    let origin_header = origin
        .map(|value| format!("origin: {value}\r\n"))
        .unwrap_or_default();
    http_exchange(
        "127.0.0.1",
        port,
        &format!(
            "POST /login HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\n{origin_header}content-type: application/x-www-form-urlencoded\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{form}",
            form.len()
        ),
    )
    .await
}

struct IssuedCode {
    nonce: String,
    redirect_uri: String,
    subject: String,
}

struct TestIssuer {
    issuer_url: String,
    signing_key: CoreRsaPrivateSigningKey,
    jwks: String,
    codes: Mutex<HashMap<String, IssuedCode>>,
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
        {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Full::new(bytes::Bytes::from_static(b"bad authorize")))
                .expect("static response");
        }
        let redirect_uri = params.get("redirect_uri").cloned().unwrap_or_default();
        let state = params.get("state").cloned().unwrap_or_default();
        let nonce = params.get("nonce").cloned().unwrap_or_default();
        let subject = params
            .get("subject")
            .cloned()
            .unwrap_or_else(|| "alice".to_string());
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
                r#"{"error":"invalid_client"}"#.to_string(),
            );
        }
        let Some(code) = params.get("code").cloned() else {
            return json_response(
                StatusCode::BAD_REQUEST,
                r#"{"error":"invalid_grant"}"#.to_string(),
            );
        };
        let issued = self.codes.lock().expect("issuer codes").remove(&code);
        let Some(issued) = issued else {
            return json_response(
                StatusCode::BAD_REQUEST,
                r#"{"error":"invalid_grant"}"#.to_string(),
            );
        };
        if params.get("redirect_uri") != Some(&issued.redirect_uri) {
            return json_response(
                StatusCode::BAD_REQUEST,
                r#"{"error":"invalid_grant"}"#.to_string(),
            );
        }
        let claims = CoreIdTokenClaims::new(
            IssuerUrl::new(self.issuer_url.clone()).expect("issuer"),
            vec![Audience::new(CLIENT_ID.to_string())],
            Utc::now() + ChronoDuration::seconds(300),
            Utc::now(),
            StandardClaims::new(SubjectIdentifier::new(issued.subject)),
            EmptyAdditionalClaims {},
        )
        .set_nonce(Some(Nonce::new(issued.nonce)));
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

fn query_map(query: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect()
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

async fn complete_oidc_login(product_port: u16, issuer_port: u16, subject: &str) -> HttpCall {
    let start = get(product_port, "/login/oidc", "").await;
    assert_eq!(
        start.status, 302,
        "OIDC login starts through product routing"
    );
    let state_cookie = start
        .cookie_value(OIDC_STATE_COOKIE)
        .expect("OIDC state cookie");
    let authorize_url = url::Url::parse(start.header("location").expect("authorize location"))
        .expect("authorize URL");
    let query = authorize_url.query().expect("authorize query");
    let encoded_subject: String =
        url::form_urlencoded::byte_serialize(subject.as_bytes()).collect();
    let issuer_redirect = get(
        issuer_port,
        &format!("/authorize?{query}&subject={encoded_subject}"),
        "",
    )
    .await;
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
    get(
        product_port,
        &callback_path,
        &format!("cookie: {OIDC_STATE_COOKIE}={state_cookie}\r\n"),
    )
    .await
}

fn database_url() -> String {
    std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database")
}

fn set_env(name: &str, value: &str) {
    unsafe { std::env::set_var(name, value) };
}

fn unset_env(name: &str) {
    unsafe { std::env::remove_var(name) };
}

struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(label: &str) -> TempDir {
    let dir = std::env::temp_dir().join(format!("voie-auth-{label}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("temp dir creates");
    TempDir(dir)
}

/// Throwaway mTLS material; `FabricClient::from_env` loads it eagerly even
/// though this contract never calls Fabric.
fn fabric_pem_files(dir: &std::path::Path) -> (String, String, String) {
    fn openssl(args: &[&str]) {
        let done = std::process::Command::new("openssl")
            .args(args)
            .output()
            .expect("openssl runs");
        assert!(
            done.status.success(),
            "openssl failed: {}",
            String::from_utf8_lossy(&done.stderr)
        );
    }
    let ca_key = dir.join("ca.key");
    let ca_pem = dir.join("ca.pem");
    let client_key = dir.join("client.key");
    let client_csr = dir.join("client.csr");
    let client_pem = dir.join("client.pem");
    openssl(&[
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-keyout",
        ca_key.to_str().expect("ca key path"),
        "-out",
        ca_pem.to_str().expect("ca pem path"),
        "-days",
        "2",
        "-nodes",
        "-subj",
        "/CN=voie-auth-product-test-ca",
    ]);
    openssl(&[
        "req",
        "-newkey",
        "rsa:2048",
        "-keyout",
        client_key.to_str().expect("client key path"),
        "-out",
        client_csr.to_str().expect("client csr path"),
        "-nodes",
        "-subj",
        "/CN=voie-auth-product-test-client",
    ]);
    openssl(&[
        "x509",
        "-req",
        "-in",
        client_csr.to_str().expect("client csr path"),
        "-CA",
        ca_pem.to_str().expect("ca pem path"),
        "-CAkey",
        ca_key.to_str().expect("ca key path"),
        "-out",
        client_pem.to_str().expect("client pem path"),
        "-days",
        "2",
    ]);
    let path = |value: &std::path::Path| value.display().to_string();
    (path(&client_pem), path(&client_key), path(&ca_pem))
}

fn configure_services() -> TempDir {
    for name in [
        "VOIE_AZURE_BLOB_KEY_FILE",
        "VOIE_MODEL_API_KEY_FILE",
        "VOIE_FABRIC_ID",
    ] {
        unset_env(name);
    }
    set_env("VOIE_AZURE_BLOB_ACCOUNT", "voie-auth-product-test-account");
    set_env(
        "VOIE_AZURE_BLOB_KEY",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    set_env(
        "VOIE_AZURE_BLOB_CONTAINER",
        "voie-auth-product-test-container",
    );
    set_env(
        "VOIE_AZURE_BLOB_ENDPOINT",
        "http://127.0.0.1:9/unreached-blob",
    );
    set_env("VOIE_MODEL_BASE_URL", "http://127.0.0.1:9/v1");
    set_env("VOIE_MODEL_NAME", "auth-product-test-model");
    set_env("VOIE_MODEL_API_KEY", "auth-product-test-key");
    set_env("VOIE_FABRIC_ENDPOINT", "https://127.0.0.1:9/");
    let certs = temp_dir("certs");
    let (client_cert, client_key, ca_cert) = fabric_pem_files(certs.0.as_path());
    set_env("VOIE_FABRIC_CLIENT_CERT_PATH", &client_cert);
    set_env("VOIE_FABRIC_CLIENT_KEY_PATH", &client_key);
    set_env("VOIE_FABRIC_CA_CERT_PATH", &ca_cert);
    certs
}

#[tokio::test]
async fn auth_product_routing_contract() {
    let kernel = Arc::new(
        Kernel::connect(&Config::database_url(database_url()))
            .await
            .expect("PostgreSQL connection succeeds"),
    );
    kernel.migrate().await.expect("migration succeeds");
    sqlx::query("truncate table users cascade")
        .execute(kernel.pool())
        .await
        .expect("auth test tables start empty");

    let _certs = configure_services();
    let services = voie_cloud::integration::Services::from_env(kernel.pool().clone())
        .expect("local service configuration resolves");

    // Native mode: bootstrap, capability discovery, and native form login all
    // use one product listener rather than auth::serve.
    let native_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("native product listener binds");
    let native_port = native_listener
        .local_addr()
        .expect("native listener address")
        .port();
    let native_origin = format!("http://127.0.0.1:{native_port}");
    for name in [
        "VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE",
        "VOIE_NATIVE_ADMIN_USERNAME",
        "VOIE_NATIVE_ADMIN_PASSWORD",
        "VOIE_NATIVE_ADMIN_PASSWORD_FILE",
        "VOIE_OIDC_CLIENT_SECRET_FILE",
    ] {
        unset_env(name);
    }
    set_env("VOIE_AUTH_MODE", "native");
    set_env("VOIE_PUBLIC_ORIGIN", &native_origin);
    set_env("VOIE_BOOTSTRAP_ADMIN_USERNAME", ADMIN_USERNAME);
    set_env("VOIE_BOOTSTRAP_ADMIN_PASSWORD", ADMIN_PASSWORD);
    let native_auth = Arc::new(
        Auth::connect(
            AuthConfig::from_env().expect("native auth configuration resolves"),
            kernel.pool().clone(),
        )
        .await
        .expect("native auth connects without OIDC discovery"),
    );
    native_auth
        .bootstrap_native_admin(&kernel)
        .await
        .expect("native admin bootstrap succeeds");

    let admin = kernel
        .find_user_by_username(ADMIN_USERNAME)
        .await
        .expect("bootstrapped admin lookup succeeds")
        .expect("bootstrapped admin exists");
    assert_eq!(admin.platform_role, "admin");
    assert_eq!(admin.status, "active");
    let admin_count: i64 = sqlx::query_scalar(
        "select count(*) from users where username = $1 and platform_role = 'admin'",
    )
    .bind(ADMIN_USERNAME)
    .fetch_one(kernel.pool())
    .await
    .expect("admin count query succeeds");
    assert_eq!(admin_count, 1, "bootstrap creates one named platform admin");
    let personal_scopes: i64 = sqlx::query_scalar(
        "select count(*) from projects where owner_user_id = $1 and kind = 'personal'",
    )
    .bind(admin.id)
    .fetch_one(kernel.pool())
    .await
    .expect("personal scope count succeeds");
    assert_eq!(personal_scopes, 1, "bootstrap creates one personal scope");
    let owner_memberships: i64 = sqlx::query_scalar(
        "select count(*) from project_members m join projects p on p.id = m.project_id \
         where p.owner_user_id = $1 and p.kind = 'personal' and m.user_id = $1 and m.role = 'owner'",
    )
    .bind(admin.id)
    .fetch_one(kernel.pool())
    .await
    .expect("personal owner membership count succeeds");
    assert_eq!(
        owner_memberships, 1,
        "personal scope has one owner membership"
    );
    let native_credentials: i64 =
        sqlx::query_scalar("select count(*) from native_credentials where user_id = $1")
            .bind(admin.id)
            .fetch_one(kernel.pool())
            .await
            .expect("native credential count succeeds");
    assert_eq!(native_credentials, 1);
    let native_identity: i64 = sqlx::query_scalar(
        "select count(*) from auth_identities where provider = 'native' and issuer = 'native' \
         and subject = $1 and user_id = $2",
    )
    .bind(ADMIN_USERNAME)
    .bind(admin.id)
    .fetch_one(kernel.pool())
    .await
    .expect("native identity count succeeds");
    assert_eq!(native_identity, 1);

    // A second startup with different bootstrap values is a no-op once the
    // first platform admin exists.
    native_auth
        .bootstrap_native_admin(&kernel)
        .await
        .expect("repeated bootstrap is idempotent");
    set_env("VOIE_BOOTSTRAP_ADMIN_USERNAME", LATER_ADMIN_USERNAME);
    set_env("VOIE_BOOTSTRAP_ADMIN_PASSWORD", LATER_ADMIN_PASSWORD);
    let later_auth = Auth::connect(
        AuthConfig::from_env().expect("later native auth configuration resolves"),
        kernel.pool().clone(),
    )
    .await
    .expect("later native auth connects");
    later_auth
        .bootstrap_native_admin(&kernel)
        .await
        .expect("later bootstrap configuration is ignored");
    let all_users: i64 = sqlx::query_scalar("select count(*) from users")
        .fetch_one(kernel.pool())
        .await
        .expect("user count after later bootstrap succeeds");
    assert_eq!(all_users, 1, "later bootstrap does not create another User");
    assert!(
        kernel
            .find_user_by_username(LATER_ADMIN_USERNAME)
            .await
            .expect("later username lookup succeeds")
            .is_none(),
        "later bootstrap username is ignored"
    );
    let personal_scopes_after: i64 = sqlx::query_scalar(
        "select count(*) from projects where owner_user_id = $1 and kind = 'personal'",
    )
    .bind(admin.id)
    .fetch_one(kernel.pool())
    .await
    .expect("personal scope count after later bootstrap succeeds");
    assert_eq!(personal_scopes_after, 1);

    let native_task = tokio::spawn(serve_with_services(
        native_listener,
        kernel.clone(),
        native_auth.clone(),
        services.clone(),
    ));
    let native_caps = get(native_port, "/api/auth/capabilities", "").await;
    assert_eq!(
        native_caps.status, 200,
        "pre-session capabilities are public through product routing"
    );
    let native_caps_json = native_caps.json();
    assert_eq!(
        native_caps_json.get("native").and_then(Value::as_bool),
        Some(true)
    );
    let native_external = native_caps_json
        .get("external")
        .and_then(Value::as_array)
        .expect("native capabilities carry an external array");
    assert!(
        native_external.is_empty(),
        "native mode has no external provider"
    );

    // Web-less fixture servers serve no login page at all; the deployed
    // product mounts the VOIE static app for GET /login.
    let native_page = get(native_port, "/login", "").await;
    assert_eq!(native_page.status, 404, "no bare HTML login UI exists");
    let oidc_hidden = get(native_port, "/login/oidc", "").await;
    assert_eq!(oidc_hidden.status, 404, "native-only mode hides OIDC route");

    let anonymous_api = get(native_port, "/api/me", "").await;
    assert_eq!(anonymous_api.status, 401);
    let wrong_origin = post_login(
        native_port,
        Some("http://evil.invalid"),
        ADMIN_USERNAME,
        ADMIN_PASSWORD,
    )
    .await;
    assert_eq!(wrong_origin.status, 403, "native login is same-origin only");
    let wrong_password = post_login(
        native_port,
        Some(&native_origin),
        ADMIN_USERNAME,
        "incorrect-password",
    )
    .await;
    assert_eq!(
        wrong_password.status, 401,
        "invalid native credentials are refused"
    );
    let unknown_user = post_login(
        native_port,
        Some(&native_origin),
        "missing-user",
        ADMIN_PASSWORD,
    )
    .await;
    assert_eq!(
        unknown_user.status, 401,
        "unknown native credentials are refused"
    );

    let native_login = post_login(
        native_port,
        Some(&native_origin),
        ADMIN_USERNAME,
        ADMIN_PASSWORD,
    )
    .await;
    assert_eq!(native_login.status, 303);
    assert_eq!(native_login.header("location"), Some("/"));
    let native_cookie_header = native_login
        .set_cookies()
        .into_iter()
        .find(|cookie| cookie.starts_with(&format!("{COOKIE_NAME}=")))
        .expect("native login sets the opaque session cookie");
    assert!(native_cookie_header.contains("HttpOnly"));
    assert!(native_cookie_header.contains("Secure"));
    assert!(native_cookie_header.contains("SameSite=Lax"));
    let native_token = native_login
        .cookie_value(COOKIE_NAME)
        .expect("native session cookie value");
    assert!(native_token.len() >= 32);
    assert!(
        !native_token.contains('.'),
        "session value is opaque, not a JWT"
    );
    let native_session = web_session::lookup(
        kernel.pool(),
        &native_token,
        native_auth.config().session_ttl(),
    )
    .await
    .expect("native session lookup succeeds")
    .expect("native session row exists");
    assert_eq!(native_session.user_id, admin.id);
    let me = get(
        native_port,
        "/api/me",
        &format!("cookie: {COOKIE_NAME}={native_token}\r\n"),
    )
    .await;
    assert_eq!(me.status, 200);
    let me_json = me.json();
    let expected_admin_id = admin.id.to_string();
    assert_eq!(
        me_json.get("userId").and_then(Value::as_str),
        Some(expected_admin_id.as_str())
    );

    // A valid native credential is refused after its canonical User is
    // disabled, including when the request arrives through product routing.
    let admin_hash: String =
        sqlx::query_scalar("select password_hash from native_credentials where user_id = $1")
            .bind(admin.id)
            .fetch_one(kernel.pool())
            .await
            .expect("admin password hash lookup succeeds");
    let disabled_id = Uuid::new_v4();
    kernel
        .create_native_user(disabled_id, "disabled-user", &admin_hash, "user")
        .await
        .expect("disabled test user creates");
    sqlx::query("update users set status = 'disabled' where id = $1")
        .bind(disabled_id)
        .execute(kernel.pool())
        .await
        .expect("disabled test user updates");
    let disabled_login = post_login(
        native_port,
        Some(&native_origin),
        "disabled-user",
        ADMIN_PASSWORD,
    )
    .await;
    assert_eq!(
        disabled_login.status, 401,
        "disabled Users lose native access"
    );
    assert!(disabled_login.cookie_value(COOKIE_NAME).is_none());

    native_task.abort();
    let _ = native_task.await;

    // Both mode: the chooser and pre-session capabilities advertise the
    // native form and an explicit /login/oidc action together.
    let issuer_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("OIDC issuer binds");
    let issuer_port = issuer_listener
        .local_addr()
        .expect("OIDC issuer address")
        .port();
    let issuer_url = format!("http://127.0.0.1:{issuer_port}");
    let issuer = Arc::new(TestIssuer::new(issuer_url.clone()));
    let issuer_task = tokio::spawn(serve_issuer(issuer_listener, issuer));

    for name in [
        "VOIE_BOOTSTRAP_ADMIN_USERNAME",
        "VOIE_BOOTSTRAP_ADMIN_PASSWORD",
        "VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE",
        "VOIE_NATIVE_ADMIN_USERNAME",
        "VOIE_NATIVE_ADMIN_PASSWORD",
        "VOIE_NATIVE_ADMIN_PASSWORD_FILE",
    ] {
        unset_env(name);
    }
    let both_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("both-mode product listener binds");
    let both_port = both_listener
        .local_addr()
        .expect("both-mode listener address")
        .port();
    let both_origin = format!("http://127.0.0.1:{both_port}");
    set_env("VOIE_AUTH_MODE", "both");
    set_env("VOIE_PUBLIC_ORIGIN", &both_origin);
    set_env("VOIE_OIDC_ISSUER", &issuer_url);
    set_env("VOIE_OIDC_CLIENT_ID", CLIENT_ID);
    set_env("VOIE_OIDC_CLIENT_SECRET", CLIENT_SECRET);
    set_env(
        "VOIE_OIDC_REDIRECT_URL",
        &format!("{both_origin}/oidc/callback"),
    );
    let both_auth = Arc::new(
        Auth::connect(
            AuthConfig::from_env().expect("both-mode auth configuration resolves"),
            kernel.pool().clone(),
        )
        .await
        .expect("both-mode OIDC discovery succeeds"),
    );

    // This existing provider link intentionally carries a non-default role;
    // OIDC authenticates the link but never becomes the role source.
    let linked_user_id = Uuid::new_v4();
    sqlx::query(
        "insert into users (id, issuer, subject, status, platform_role) \
         values ($1, $2, $3, 'active', 'admin')",
    )
    .bind(linked_user_id)
    .bind(&issuer_url)
    .bind("alice")
    .execute(kernel.pool())
    .await
    .expect("linked User inserts");
    sqlx::query(
        "insert into auth_identities (provider, issuer, subject, user_id) \
         values ('oidc', $1, 'alice', $2)",
    )
    .bind(&issuer_url)
    .bind(linked_user_id)
    .execute(kernel.pool())
    .await
    .expect("OIDC identity link inserts");
    let users_before_oidc: i64 = sqlx::query_scalar("select count(*) from users")
        .fetch_one(kernel.pool())
        .await
        .expect("pre-OIDC user count succeeds");

    let both_task = tokio::spawn(serve_with_services(
        both_listener,
        kernel.clone(),
        both_auth.clone(),
        services,
    ));
    let both_caps = get(both_port, "/api/auth/capabilities", "").await;
    assert_eq!(both_caps.status, 200);
    let both_caps_json = both_caps.json();
    assert_eq!(
        both_caps_json.get("native").and_then(Value::as_bool),
        Some(true)
    );
    let external = both_caps_json
        .get("external")
        .and_then(Value::as_array)
        .expect("both capabilities carry an external provider list");
    assert_eq!(
        external.len(),
        1,
        "both mode exposes the configured OIDC provider"
    );
    assert_eq!(external[0].get("id").and_then(Value::as_str), Some("oidc"));
    assert_eq!(
        external[0].get("href").and_then(Value::as_str),
        Some("/login/oidc")
    );
    assert!(
        external[0]
            .get("label")
            .and_then(Value::as_str)
            .is_some_and(|label| !label.trim().is_empty()),
        "external capability carries a server-provided label"
    );
    // Web-less fixture servers expose no login page; both-mode still keeps
    // the explicit OIDC chooser route reachable.
    let both_page = get(both_port, "/login", "").await;
    assert_eq!(both_page.status, 404, "no bare HTML login UI exists");
    let chooser_start = get(both_port, "/login/oidc", "").await;
    assert_eq!(chooser_start.status, 302);
    assert!(
        chooser_start
            .header("location")
            .expect("OIDC chooser location")
            .starts_with(&issuer_url),
        "OIDC chooser redirects to the configured issuer"
    );

    // Native remains available in both mode.
    let both_native_login = post_login(
        both_port,
        Some(&both_origin),
        ADMIN_USERNAME,
        ADMIN_PASSWORD,
    )
    .await;
    assert_eq!(both_native_login.status, 303);
    assert_eq!(both_native_login.header("location"), Some("/"));

    // An existing OIDC identity link retains its canonical User id and role.
    let first_oidc = complete_oidc_login(both_port, issuer_port, "alice").await;
    assert_eq!(first_oidc.status, 303);
    let first_oidc_token = first_oidc
        .cookie_value(COOKIE_NAME)
        .expect("first OIDC session cookie");
    let first_session = web_session::lookup(
        kernel.pool(),
        &first_oidc_token,
        both_auth.config().session_ttl(),
    )
    .await
    .expect("first OIDC session lookup succeeds")
    .expect("first OIDC session exists");
    assert_eq!(first_session.user_id, linked_user_id);
    let users_after_first_oidc: i64 = sqlx::query_scalar("select count(*) from users")
        .fetch_one(kernel.pool())
        .await
        .expect("post-first-OIDC user count succeeds");
    assert_eq!(users_after_first_oidc, users_before_oidc);

    let second_oidc = complete_oidc_login(both_port, issuer_port, "alice").await;
    assert_eq!(second_oidc.status, 303);
    let second_oidc_token = second_oidc
        .cookie_value(COOKIE_NAME)
        .expect("second OIDC session cookie");
    let second_session = web_session::lookup(
        kernel.pool(),
        &second_oidc_token,
        both_auth.config().session_ttl(),
    )
    .await
    .expect("second OIDC session lookup succeeds")
    .expect("second OIDC session exists");
    assert_eq!(second_session.user_id, linked_user_id);
    let linked_identity_count: i64 = sqlx::query_scalar(
        "select count(*) from auth_identities where provider = 'oidc' and issuer = $1 \
         and subject = 'alice' and user_id = $2",
    )
    .bind(&issuer_url)
    .bind(linked_user_id)
    .fetch_one(kernel.pool())
    .await
    .expect("linked identity count succeeds");
    assert_eq!(
        linked_identity_count, 1,
        "repeated OIDC login does not duplicate links"
    );
    let (linked_status, linked_role): (String, String) =
        sqlx::query_as("select status, platform_role from users where id = $1")
            .bind(linked_user_id)
            .fetch_one(kernel.pool())
            .await
            .expect("linked User status and role lookup succeeds");
    assert_eq!(linked_status, "active");
    assert_eq!(
        linked_role, "admin",
        "OIDC claims do not become the role source"
    );

    // The same link now belongs to a disabled User. The identity and User id
    // remain intact, but the callback cannot mint another Web session.
    let users_before_disabled_oidc: i64 = sqlx::query_scalar("select count(*) from users")
        .fetch_one(kernel.pool())
        .await
        .expect("pre-disabled-OIDC user count succeeds");
    let sessions_before_disabled_oidc: i64 =
        sqlx::query_scalar("select count(*) from web_sessions")
            .fetch_one(kernel.pool())
            .await
            .expect("pre-disabled-OIDC session count succeeds");
    sqlx::query("update users set status = 'disabled' where id = $1")
        .bind(linked_user_id)
        .execute(kernel.pool())
        .await
        .expect("linked User disables");
    let disabled_oidc = complete_oidc_login(both_port, issuer_port, "alice").await;
    assert!(
        disabled_oidc.status >= 400,
        "disabled OIDC-linked User is refused rather than redirected"
    );
    assert!(disabled_oidc.cookie_value(COOKIE_NAME).is_none());
    let users_after_disabled_oidc: i64 = sqlx::query_scalar("select count(*) from users")
        .fetch_one(kernel.pool())
        .await
        .expect("post-disabled-OIDC user count succeeds");
    assert_eq!(users_after_disabled_oidc, users_before_disabled_oidc);
    let sessions_after_disabled_oidc: i64 = sqlx::query_scalar("select count(*) from web_sessions")
        .fetch_one(kernel.pool())
        .await
        .expect("post-disabled-OIDC session count succeeds");
    assert_eq!(sessions_after_disabled_oidc, sessions_before_disabled_oidc);
    let disabled_status: String = sqlx::query_scalar("select status from users where id = $1")
        .bind(linked_user_id)
        .fetch_one(kernel.pool())
        .await
        .expect("disabled linked User status lookup succeeds");
    assert_eq!(disabled_status, "disabled");

    // Existing sessions are checked against current User status by the same
    // product API and are revoked on first use after disablement.
    let disabled_me = get(
        both_port,
        "/api/me",
        &format!("cookie: {COOKIE_NAME}={second_oidc_token}\r\n"),
    )
    .await;
    assert_eq!(disabled_me.status, 401);

    both_task.abort();
    let _ = both_task.await;
    issuer_task.abort();
    let _ = issuer_task.await;
}
