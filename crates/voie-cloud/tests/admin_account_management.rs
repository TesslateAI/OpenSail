//! Focused contracts for platform-admin account management.
//!
//! A platform admin can create canonical Users with native credentials,
//! reset a User's password, and inspect or revoke a User's live Web
//! sessions. Disabling a User revokes its sessions immediately. An
//! ordinary project owner cannot use any of these routes.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Mutex;

use sqlx::Row;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;
use voie_cloud::auth::{Auth, AuthConfig};
use voie_cloud::integration::Services;
use voie_cloud::web_session;
use voie_cloud::{Config, Kernel, serve_with_services};

#[path = "common/tls_pems.rs"]
mod tls_pems;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvironmentRestore {
    previous: Vec<(&'static str, Option<OsString>)>,
}

impl EnvironmentRestore {
    fn new() -> Self {
        Self {
            previous: Vec::new(),
        }
    }

    fn set(&mut self, name: &'static str, value: impl AsRef<std::ffi::OsStr>) {
        self.previous.push((name, std::env::var_os(name)));
        // Rust 2024 marks process-wide environment mutation unsafe. The test
        // holds ENV_LOCK, so no sibling contract test observes a half-set
        // fixture.
        unsafe { std::env::set_var(name, value) };
    }
}

impl Drop for EnvironmentRestore {
    fn drop(&mut self) {
        for (name, value) in self.previous.drain(..).rev() {
            match value {
                Some(value) => unsafe { std::env::set_var(name, value) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("voie-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("temporary fixture directory creates");
        Self(path)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Builds PEM files accepted by FabricClient. The admin routes never reach
/// the endpoint, but Services still validates its mTLS material at startup.
fn fabric_pem_fixture(dir: &TempDir) -> (PathBuf, PathBuf, PathBuf) {
    let pems = tls_pems::write_v3_ca_and_client(&dir.0);
    (pems.client_pem, pems.client_key, pems.ca_pem)
}

struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

async fn exchange(port: u16, request: String) -> HttpResponse {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("API listener accepts connections");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("request writes");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("response reads");
    let (head, body) = response
        .split_once_bytes(b"\r\n\r\n")
        .expect("HTTP response has a header terminator");
    let status = String::from_utf8_lossy(head)
        .lines()
        .next()
        .expect("HTTP status line exists")
        .split_whitespace()
        .nth(1)
        .expect("HTTP status code exists")
        .parse()
        .expect("HTTP status is numeric");
    HttpResponse {
        status,
        body: body.to_vec(),
    }
}

trait SplitOnceBytes {
    fn split_once_bytes(&self, needle: &[u8]) -> Option<(&[u8], &[u8])>;
}

impl SplitOnceBytes for [u8] {
    fn split_once_bytes(&self, needle: &[u8]) -> Option<(&[u8], &[u8])> {
        let offset = self
            .windows(needle.len())
            .position(|window| window == needle)?;
        Some((&self[..offset], &self[offset + needle.len()..]))
    }
}

async fn get(port: u16, path: &str, token: &str) -> HttpResponse {
    exchange(
        port,
        format!(
            "GET {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\nconnection: close\r\ncookie: voie_session={token}\r\n\r\n"
        ),
    )
    .await
}

async fn post_json(port: u16, path: &str, token: &str, origin: &str, body: &str) -> HttpResponse {
    exchange(
        port,
        format!(
            "POST {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\nconnection: close\r\ncookie: voie_session={token}\r\norigin: {origin}\r\ncontent-type: application/json\r\nx-voie-intent: mutate\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        ),
    )
    .await
}

async fn delete(port: u16, path: &str, token: &str, origin: &str) -> HttpResponse {
    exchange(
        port,
        format!(
            "DELETE {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\nconnection: close\r\ncookie: voie_session={token}\r\norigin: {origin}\r\nx-voie-intent: mutate\r\n\r\n"
        ),
    )
    .await
}

async fn post_login(port: u16, origin: &str, username: &str, password: &str) -> HttpResponse {
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("username", username)
        .append_pair("password", password)
        .finish();
    exchange(
        port,
        format!(
            "POST /login HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\norigin: {origin}\r\ncontent-type: application/x-www-form-urlencoded\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{form}",
            form.len()
        ),
    )
    .await
}
async fn patch_json(port: u16, path: &str, token: &str, origin: &str, body: &str) -> HttpResponse {
    exchange(
        port,
        format!(
            "PATCH {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\nconnection: close\r\ncookie: voie_session={token}\r\norigin: {origin}\r\ncontent-type: application/json\r\nx-voie-intent: mutate\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        ),
    )
    .await
}

fn database_url() -> String {
    std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database")
}

/// One native-mode product listener with a bootstrapped platform admin and
/// one ordinary user, mirroring the directory-contract fixture.
struct Fixture {
    kernel: std::sync::Arc<Kernel>,
    port: u16,
    admin_token: String,
    ordinary_token: String,
    public_origin: String,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
    _environment: EnvironmentRestore,
    _fixture_dir: TempDir,
}

async fn spawn_fixture(label: &str) -> Fixture {
    let _environment_lock = ENV_LOCK.lock().expect("environment fixture lock");
    let fixture_dir = TempDir::new(label);
    let (cert, key, ca) = fabric_pem_fixture(&fixture_dir);
    let mut environment = EnvironmentRestore::new();
    environment.set("VOIE_AZURE_BLOB_ACCOUNT", "admin-account-test-account");
    environment.set(
        "VOIE_AZURE_BLOB_KEY",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    environment.set("VOIE_AZURE_BLOB_CONTAINER", "admin-account-test-container");
    environment.set("VOIE_AZURE_BLOB_ENDPOINT", "http://127.0.0.1:1");
    environment.set("VOIE_MODEL_BASE_URL", "http://127.0.0.1:1");
    environment.set("VOIE_MODEL_NAME", "admin-account-test-model");
    environment.set("VOIE_MODEL_API_KEY", "admin-account-test-key");
    environment.set("VOIE_FABRIC_ENDPOINT", "https://127.0.0.1:1");
    environment.set("VOIE_FABRIC_CLIENT_CERT_PATH", &cert);
    environment.set("VOIE_FABRIC_CLIENT_KEY_PATH", &key);
    environment.set("VOIE_FABRIC_CA_CERT_PATH", &ca);
    environment.set("VOIE_USER_SECRETS_BACKEND", "memory");

    let kernel = std::sync::Arc::new(
        Kernel::connect(&Config::database_url(database_url()))
            .await
            .expect("PostgreSQL connection succeeds"),
    );
    kernel.migrate().await.expect("latest migration applies");

    let admin_id = Uuid::new_v4();
    let ordinary_id = Uuid::new_v4();
    for (user_id, subject, username, display_name, role) in [
        (
            admin_id,
            "admin-account-admin-subject",
            "admin-account-admin",
            "Admin Account Administrator",
            "admin",
        ),
        (
            ordinary_id,
            "admin-account-owner-subject",
            "admin-account-owner",
            "Admin Account Owner",
            "user",
        ),
    ] {
        sqlx::query(
            "insert into users \
             (id, issuer, subject, username, display_name, email, platform_role, status) \
             values ($1, $2, $3, $4, $5, $6, $7, 'active') on conflict (username) where username is not null do update set platform_role = excluded.platform_role, status = 'active'",
        )
        .bind(user_id)
        .bind(&format!("admin-account-{}", Uuid::new_v4()))
        .bind(subject)
        .bind(&format!("{username}-{}", Uuid::new_v4()))
        .bind(display_name)
        .bind(format!("{username}@example.test"))
        .bind(role)
        .execute(kernel.pool())
        .await
        .expect("canonical user inserts");
    }

    let auth = std::sync::Arc::new(
        Auth::connect(
            AuthConfig::native("http://admin-account.test"),
            kernel.pool().clone(),
        )
        .await
        .expect("native auth connects"),
    );
    let admin_session = web_session::create(kernel.pool(), admin_id, auth.config().session_ttl())
        .await
        .expect("platform-admin session creates");
    let ordinary_session =
        web_session::create(kernel.pool(), ordinary_id, auth.config().session_ttl())
            .await
            .expect("ordinary session creates");

    let services = Services::from_env(kernel.pool().clone()).expect("service seams configure");
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("API listener binds");
    let port = listener
        .local_addr()
        .expect("API listener address exists")
        .port();
    let server = tokio::spawn(serve_with_services(
        listener,
        kernel.clone(),
        auth.clone(),
        services,
    ));

    Fixture {
        kernel,
        port,
        admin_token: admin_session.1,
        ordinary_token: ordinary_session.1,
        public_origin: "http://admin-account.test".to_string(),
        server,
        _environment: environment,
        _fixture_dir: fixture_dir,
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[tokio::test]
async fn admin_account_management_contract() {
    let fixture = spawn_fixture("admin-account").await;
    let port = fixture.port;
    let public_origin = &fixture.public_origin;
    let created_username = format!("created-user-{}", Uuid::new_v4());
    let created_password = "first-passphrase-123";

    // 1. Admin creates a User with a native credential.
    let create_body = serde_json::json!({
        "username": created_username,
        "displayName": "Created User",
        "email": "created-user@example.test",
        "platformRole": "user",
        "password": created_password,
    });
    let created = post_json(
        port,
        "/api/admin/users",
        &fixture.admin_token,
        public_origin,
        &create_body.to_string(),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "admin creates one user: {}",
        String::from_utf8_lossy(&created.body)
    );
    let created_json: serde_json::Value =
        serde_json::from_slice(&created.body).expect("create response is JSON");
    let created_id = created_json
        .get("id")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .expect("create response carries the durable id");
    let stored: (String, String) =
        sqlx::query_as("select status, platform_role from users where id = $1")
            .bind(created_id)
            .fetch_one(fixture.kernel.pool())
            .await
            .expect("created user row exists");
    assert_eq!(stored.0, "active");
    assert_eq!(stored.1, "user");

    // 2. A duplicate username is rejected.
    let duplicate = post_json(
        port,
        "/api/admin/users",
        &fixture.admin_token,
        public_origin,
        &create_body.to_string(),
    )
    .await;
    assert_eq!(
        duplicate.status,
        409,
        "duplicate username is a conflict: {}",
        String::from_utf8_lossy(&duplicate.body)
    );

    // The created User can log in with the seeded password.
    let login = post_login(port, public_origin, &created_username, created_password).await;
    assert_eq!(login.status, 303, "seeded native credential logs in");

    // 3. Reset-password overwrites the hash; the old password fails and the
    //    new one logs in.
    let sessions_before = web_session::create(
        fixture.kernel.pool(),
        created_id,
        std::time::Duration::from_secs(600),
    )
    .await
    .expect("pre-reset session exists");

    let new_password = "second-passphrase-456";
    let reset = post_json(
        port,
        &format!("/api/admin/users/{created_id}/reset-password"),
        &fixture.admin_token,
        public_origin,
        &serde_json::json!({ "password": new_password }).to_string(),
    )
    .await;
    assert_eq!(
        reset.status,
        200,
        "admin resets the password: {}",
        String::from_utf8_lossy(&reset.body)
    );
    let old_login = post_login(port, public_origin, &created_username, created_password).await;
    assert_eq!(old_login.status, 401, "old password no longer logs in");
    let new_login = post_login(port, public_origin, &created_username, new_password).await;
    assert_eq!(new_login.status, 303, "new password logs in");

    let audit_kinds: Vec<String> = sqlx::query(
        "select kind from audit_events where resource_id = $1 and kind in ('user.created', 'user.password_reset') order by kind",
    )
    .bind(created_id)
    .fetch_all(fixture.kernel.pool())
    .await
    .expect("audit query succeeds")
    .into_iter()
    .map(|row| row.get::<String, _>("kind"))
    .collect();
    assert!(
        audit_kinds.contains(&"user.created".to_string()),
        "create is audited: {audit_kinds:?}"
    );
    assert!(
        audit_kinds.contains(&"user.password_reset".to_string()),
        "password reset is audited: {audit_kinds:?}"
    );

    // 4. Reset-password revoked every live Web session immediately, so the
    //    pre-reset session row is already gone from the admin listing.
    let listed = get(
        port,
        &format!("/api/admin/users/{created_id}/sessions"),
        &fixture.admin_token,
    )
    .await;
    assert_eq!(
        listed.status,
        200,
        "admin lists sessions: {}",
        String::from_utf8_lossy(&listed.body)
    );
    let listed_body = String::from_utf8_lossy(&listed.body);
    assert!(
        !listed_body.contains(&sessions_before.0.id.to_string()),
        "reset-password revoked the pre-reset session: {listed_body}"
    );
    let fresh_session = web_session::create(
        fixture.kernel.pool(),
        created_id,
        std::time::Duration::from_secs(600),
    )
    .await
    .expect("post-reset session exists");
    let relisted = get(
        port,
        &format!("/api/admin/users/{created_id}/sessions"),
        &fixture.admin_token,
    )
    .await;
    assert_eq!(
        relisted.status,
        200,
        "admin lists sessions: {}",
        String::from_utf8_lossy(&relisted.body)
    );
    let relisted_body = String::from_utf8_lossy(&relisted.body);
    assert!(
        relisted_body.contains(&fresh_session.0.id.to_string()),
        "post-reset session is listed before explicit revoke: {relisted_body}"
    );

    let revoked = delete(
        port,
        &format!("/api/admin/users/{created_id}/sessions"),
        &fixture.admin_token,
        public_origin,
    )
    .await;
    assert_eq!(
        revoked.status,
        200,
        "admin revokes sessions: {}",
        String::from_utf8_lossy(&revoked.body)
    );
    let remaining: i64 = sqlx::query_scalar("select count(*) from web_sessions where user_id = $1")
        .bind(created_id)
        .fetch_one(fixture.kernel.pool())
        .await
        .expect("session count query succeeds");
    assert_eq!(remaining, 0, "revocation deletes every session row");

    let listed_after = get(
        port,
        &format!("/api/admin/users/{created_id}/sessions"),
        &fixture.admin_token,
    )
    .await;
    assert_eq!(listed_after.status, 200);
    let after_json: serde_json::Value =
        serde_json::from_slice(&listed_after.body).expect("session list is JSON");
    assert!(
        after_json
            .get("items")
            .and_then(serde_json::Value::as_array)
            .expect("session list carries items")
            .is_empty(),
        "no sessions remain after revoke: {after_json}"
    );

    // 5. Disabling a User revokes its sessions.
    let relogin = post_login(port, public_origin, &created_username, new_password).await;
    assert_eq!(relogin.status, 303, "user logs in before disable");
    let disable = patch_json(
        port,
        &format!("/api/admin/users/{created_id}/status"),
        &fixture.admin_token,
        public_origin,
        r#"{"status":"disabled"}"#,
    )
    .await;
    assert_eq!(disable.status, 200, "admin disables the user");
    let remaining_after_disable: i64 =
        sqlx::query_scalar("select count(*) from web_sessions where user_id = $1")
            .bind(created_id)
            .fetch_one(fixture.kernel.pool())
            .await
            .expect("session count after disable succeeds");
    assert_eq!(remaining_after_disable, 0, "disable revokes every session");

    // 6. Non-admin callers get 403 on every new route.
    let ordinary_create = post_json(
        port,
        "/api/admin/users",
        &fixture.ordinary_token,
        public_origin,
        &serde_json::json!({
            "username": "nope",
            "displayName": "Nope",
            "password": "irrelevant-password-1",
        })
        .to_string(),
    )
    .await;
    assert_eq!(ordinary_create.status, 403, "non-admin cannot create users");
    let ordinary_reset = post_json(
        port,
        &format!("/api/admin/users/{created_id}/reset-password"),
        &fixture.ordinary_token,
        public_origin,
        r#"{"password":"irrelevant-password-2"}"#,
    )
    .await;
    assert_eq!(
        ordinary_reset.status, 403,
        "non-admin cannot reset passwords"
    );
    let ordinary_list = get(
        port,
        &format!("/api/admin/users/{created_id}/sessions"),
        &fixture.ordinary_token,
    )
    .await;
    assert_eq!(ordinary_list.status, 403, "non-admin cannot list sessions");
    let ordinary_revoke = delete(
        port,
        &format!("/api/admin/users/{created_id}/sessions"),
        &fixture.ordinary_token,
        public_origin,
    )
    .await;
    assert_eq!(
        ordinary_revoke.status, 403,
        "non-admin cannot revoke sessions"
    );
}
