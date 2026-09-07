//! OIDC login, Web session, and Project authorization contract.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use chrono::{Duration as ChronoDuration, Utc};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{HeaderValue, CONTENT_TYPE, LOCATION};
use hyper::{Method, Request, Response, StatusCode};
use openidconnect::core::{
    CoreIdToken, CoreIdTokenClaims, CoreJsonWebKeySet, CoreJwsSigningAlgorithm,
    CoreRsaPrivateSigningKey,
};
use openidconnect::{
    Audience, EmptyAdditionalClaims, IssuerUrl, JsonWebKeyId, Nonce, PrivateSigningKey,
    StandardClaims, SubjectIdentifier,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;
use voie_cloud::auth::{authorize, Action, Auth, AuthConfig, AuthError, Role};
use voie_cloud::web_session::{self, COOKIE_NAME, CSRF_HEADER, CSRF_MARKER, OIDC_STATE_COOKIE};
use voie_cloud::{Config, Kernel};

const CLIENT_ID: &str = "voie-cloud-test";
const CLIENT_SECRET: &str = "voie-cloud-test-secret";
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

struct IssuedCode {
    nonce: String,
    redirect_uri: String,
}

struct TestIssuer {
    issuer_url: String,
    signing_key: CoreRsaPrivateSigningKey,
    jwks: String,
    codes: Mutex<HashMap<String, IssuedCode>>,
}

struct HttpCall {
    status: u16,
    headers: Vec<(String, String)>,
    _body: Vec<u8>,
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
        _body: body.as_bytes().to_vec(),
    }
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
            signing_key.as_verification_key()
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
        let mut nonce = params.get("nonce").cloned().unwrap_or_default();
        if params.get("mutate_nonce").map(String::as_str) == Some("1") {
            nonce = "mutated-nonce".to_string();
        }
        let code = Uuid::new_v4().to_string();
        self.codes.lock().expect("issuer codes").insert(
            code.clone(),
            IssuedCode {
                nonce,
                redirect_uri: redirect_uri.clone(),
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
        let Some(code) = params.get("code").cloned() else {
            return json_response(
                StatusCode::BAD_REQUEST,
                r#"{"error":"invalid_grant"}"#.into(),
            );
        };
        let issued = self.codes.lock().expect("issuer codes").remove(&code);
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
            StandardClaims::new(SubjectIdentifier::new("alice".to_string())),
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

async fn get(port: u16, path: &str, extra_headers: &str) -> HttpCall {
    http_exchange(
        "127.0.0.1",
        port,
        &format!("GET {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\nconnection: close\r\n{extra_headers}\r\n"),
    )
    .await
}

#[tokio::test]
async fn auth_flow_contract() {
    let database_url = std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database");
    let kernel = Kernel::connect(&Config::database_url(database_url))
        .await
        .expect("PostgreSQL connection succeeds");
    kernel.migrate().await.expect("migration succeeds");
    sqlx::query("truncate table users cascade")
        .execute(kernel.pool())
        .await
        .expect("test tables start empty");

    let issuer_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("issuer binds");
    let issuer_port = issuer_listener.local_addr().expect("issuer addr").port();
    let issuer_url = format!("http://127.0.0.1:{issuer_port}");
    let issuer = Arc::new(TestIssuer::new(issuer_url.clone()));
    let issuer_task = tokio::spawn(serve_issuer(issuer_listener, issuer));

    let auth_listener = TcpListener::bind("127.0.0.1:0").await.expect("auth binds");
    let auth_port = auth_listener.local_addr().expect("auth addr").port();
    let public_origin = format!("http://127.0.0.1:{auth_port}");
    let redirect_url = format!("{public_origin}/oidc/callback");
    let auth = Auth::connect(
        AuthConfig::new(
            issuer_url,
            CLIENT_ID,
            CLIENT_SECRET,
            redirect_url,
            public_origin.clone(),
        ),
        kernel.pool().clone(),
    )
    .await
    .expect("OIDC discovery succeeds");
    let auth = Arc::new(auth);
    let auth_task = tokio::spawn(voie_cloud::auth::serve(auth_listener, auth.clone()));

    let login = get(auth_port, "/login/oidc", "").await;
    assert_eq!(login.status, 302);
    let oidc_cookie = login
        .cookie_value(OIDC_STATE_COOKIE)
        .expect("OIDC state cookie");
    let authorize_url = url::Url::parse(login.header("location").expect("authorize location"))
        .expect("authorize URL");
    let authorize_query = authorize_url.query().expect("authorize query");
    let authorize_params = query_map(authorize_query);
    let expected_state = authorize_params.get("state").cloned().expect("state");

    let bad_state = get(
        auth_port,
        &format!("/oidc/callback?code=not-a-code&state=forged-state"),
        &format!("cookie: {OIDC_STATE_COOKIE}={oidc_cookie}\r\n"),
    )
    .await;
    assert_eq!(bad_state.status, 400);

    let mutated = format!("{authorize_query}&mutate_nonce=1");
    let issuer_redirect = get(issuer_port, &format!("/authorize?{mutated}"), "").await;
    assert_eq!(issuer_redirect.status, 302);
    let mutated_callback = url::Url::parse(
        issuer_redirect
            .header("location")
            .expect("mutated callback"),
    )
    .expect("callback URL");
    let mutated_path = format!(
        "{}?{}",
        mutated_callback.path(),
        mutated_callback.query().unwrap_or("")
    );
    let bad_nonce = get(
        auth_port,
        &mutated_path,
        &format!("cookie: {OIDC_STATE_COOKIE}={oidc_cookie}\r\n"),
    )
    .await;
    assert_eq!(bad_nonce.status, 400);

    let users_before: i64 = sqlx::query_scalar("select count(*) from users")
        .fetch_one(kernel.pool())
        .await
        .expect("user count");
    let sessions_before: i64 = sqlx::query_scalar("select count(*) from web_sessions")
        .fetch_one(kernel.pool())
        .await
        .expect("session count");
    assert_eq!(users_before, 0);
    assert_eq!(sessions_before, 0);

    let login = get(auth_port, "/login/oidc", "").await;
    let oidc_cookie = login
        .cookie_value(OIDC_STATE_COOKIE)
        .expect("OIDC state cookie");
    let authorize_url = url::Url::parse(login.header("location").expect("authorize location"))
        .expect("authorize URL");
    let _ = expected_state;
    let issuer_redirect = get(
        issuer_port,
        &format!("/authorize?{}", authorize_url.query().expect("query")),
        "",
    )
    .await;
    assert_eq!(issuer_redirect.status, 302);
    let callback = url::Url::parse(issuer_redirect.header("location").expect("callback"))
        .expect("callback URL");
    let callback_path = format!("{}?{}", callback.path(), callback.query().unwrap_or(""));
    let logged_in = get(
        auth_port,
        &callback_path,
        &format!("cookie: {OIDC_STATE_COOKIE}={oidc_cookie}\r\n"),
    )
    .await;
    assert_eq!(logged_in.status, 303);
    let session_cookie_header = logged_in
        .set_cookies()
        .into_iter()
        .find(|cookie| cookie.starts_with(&format!("{COOKIE_NAME}=")))
        .expect("session Set-Cookie");
    assert!(
        session_cookie_header.contains("HttpOnly"),
        "session cookie is HttpOnly: {session_cookie_header}"
    );
    assert!(
        session_cookie_header.contains("Secure"),
        "session cookie is Secure: {session_cookie_header}"
    );
    assert!(
        session_cookie_header.contains("SameSite=Lax"),
        "session cookie is SameSite: {session_cookie_header}"
    );
    let session_token = logged_in
        .cookie_value(COOKIE_NAME)
        .expect("session cookie value");
    let session = web_session::lookup(kernel.pool(), &session_token, auth.config().session_ttl())
        .await
        .expect("session lookup")
        .expect("session exists");
    let users: i64 = sqlx::query_scalar("select count(*) from users")
        .fetch_one(kernel.pool())
        .await
        .expect("user count");
    let sessions: i64 = sqlx::query_scalar("select count(*) from web_sessions")
        .fetch_one(kernel.pool())
        .await
        .expect("session count");
    assert_eq!(users, 1);
    assert_eq!(sessions, 1);

    let alice = session.user_id;
    let bob = Uuid::new_v4();
    sqlx::query("insert into users (id, issuer, subject) values ($1, $2, $3)")
        .bind(bob)
        .bind("http://foreign-issuer")
        .bind("bob")
        .execute(kernel.pool())
        .await
        .expect("bob user");
    let foreign = kernel
        .create_project(Uuid::new_v4(), bob, "bob-project", "personal")
        .await
        .expect("foreign project");
    let bob_owner: i64 = sqlx::query_scalar(
        "select count(*) from project_members where project_id = $1 and user_id = $2 and role = 'owner'",
    )
    .bind(foreign.id)
    .bind(bob)
    .fetch_one(kernel.pool())
    .await
    .expect("owner membership is atomic with the project row");
    assert_eq!(bob_owner, 1);
    let denied = authorize(kernel.pool(), alice, foreign.id, Action::ReadProject).await;
    assert!(matches!(denied, Err(AuthError::Denied)));

    let member_project = kernel
        .create_project(Uuid::new_v4(), bob, "shared-operate", "personal")
        .await
        .expect("shared project");
    sqlx::query(
        "insert into project_members (project_id, user_id, role) values ($1, $2, 'member')",
    )
    .bind(member_project.id)
    .bind(alice)
    .execute(kernel.pool())
    .await
    .expect("alice member");
    let member_role = authorize(
        kernel.pool(),
        alice,
        member_project.id,
        Action::OperateSession,
    )
    .await
    .expect("member can operate");
    assert_eq!(member_role, Role::Member);

    let viewer_project = kernel
        .create_project(Uuid::new_v4(), bob, "shared-view", "personal")
        .await
        .expect("viewer project");
    sqlx::query(
        "insert into project_members (project_id, user_id, role) values ($1, $2, 'viewer')",
    )
    .bind(viewer_project.id)
    .bind(alice)
    .execute(kernel.pool())
    .await
    .expect("alice viewer");
    assert!(matches!(
        authorize(kernel.pool(), alice, viewer_project.id, Action::ReadProject).await,
        Ok(Role::Viewer)
    ));
    assert!(matches!(
        authorize(
            kernel.pool(),
            alice,
            viewer_project.id,
            Action::OperateSession
        )
        .await,
        Err(AuthError::MissingAction(Action::OperateSession))
    ));
    assert!(matches!(
        authorize(
            kernel.pool(),
            alice,
            viewer_project.id,
            Action::ManageMembership
        )
        .await,
        Err(AuthError::MissingAction(Action::ManageMembership))
    ));

    let logout_denied = http_exchange(
        "127.0.0.1",
        auth_port,
        &format!(
            "POST /logout HTTP/1.1\r\nhost: 127.0.0.1:{auth_port}\r\nconnection: close\r\ncookie: {COOKIE_NAME}={session_token}\r\ncontent-length: 0\r\n\r\n"
        ),
    )
    .await;
    assert_eq!(logout_denied.status, 403);

    let logout = http_exchange(
        "127.0.0.1",
        auth_port,
        &format!(
            "POST /logout HTTP/1.1\r\nhost: 127.0.0.1:{auth_port}\r\nconnection: close\r\norigin: {public_origin}\r\n{CSRF_HEADER}: {CSRF_MARKER}\r\ncookie: {COOKIE_NAME}={session_token}\r\ncontent-length: 0\r\n\r\n"
        ),
    )
    .await;
    assert_eq!(logout.status, 204);
    let after = web_session::lookup(kernel.pool(), &session_token, auth.config().session_ttl())
        .await
        .expect("lookup after logout");
    assert!(after.is_none(), "logout deletes the server-side session");
    let sessions_after: i64 = sqlx::query_scalar("select count(*) from web_sessions")
        .fetch_one(kernel.pool())
        .await
        .expect("session count after logout");
    assert_eq!(sessions_after, 0);

    issuer_task.abort();
    auth_task.abort();
    let _ = issuer_task.await;
    let _ = auth_task.await;
}
