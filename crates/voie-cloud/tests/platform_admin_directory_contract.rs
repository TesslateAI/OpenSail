//! Focused contracts for platform administration and human-readable members.
//!
//! Platform administration is an underlay/user boundary, separate from
//! project membership. A platform admin can list and update canonical Users,
//! while an ordinary project owner cannot use those routes and a platform
//! admin with no project membership cannot read a project. Project member
//! rows expose username/display name labels in addition to the durable UUID;
//! a raw UUID (or provider subject) alone is not an acceptable directory UX.

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

/// Builds PEM files accepted by FabricClient. The admin route never reaches
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

#[tokio::test]
async fn platform_admin_user_capabilities_are_separate_from_project_membership() {
    let _environment_lock = ENV_LOCK.lock().expect("environment fixture lock");
    let fixture = TempDir::new("platform-admin");
    let (cert, key, ca) = fabric_pem_fixture(&fixture);
    let mut environment = EnvironmentRestore::new();
    environment.set("VOIE_AZURE_BLOB_ACCOUNT", "scope-test-account");
    environment.set(
        "VOIE_AZURE_BLOB_KEY",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    environment.set("VOIE_AZURE_BLOB_CONTAINER", "scope-test-container");
    environment.set("VOIE_AZURE_BLOB_ENDPOINT", "http://127.0.0.1:1");
    environment.set("VOIE_MODEL_BASE_URL", "http://127.0.0.1:1");
    environment.set("VOIE_MODEL_NAME", "scope-test-model");
    environment.set("VOIE_MODEL_API_KEY", "scope-test-key");
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

    let platform_admin = Uuid::new_v4();
    let project_owner = Uuid::new_v4();
    let project_member = Uuid::new_v4();
    let project = Uuid::new_v4();
    for (user_id, subject, username, display_name, role) in [
        (
            platform_admin,
            "platform-admin-subject",
            "platform-admin",
            "Platform Administrator",
            "admin",
        ),
        (
            project_owner,
            "project-owner-subject",
            "owner",
            "Project Owner",
            "user",
        ),
        (
            project_member,
            "project-member-subject",
            "member",
            "Project Member",
            "user",
        ),
    ] {
        sqlx::query(
            "insert into users \
             (id, issuer, subject, username, display_name, email, platform_role, status) \
             values ($1, $2, $3, $4, $5, $6, $7, 'active') on conflict (username) where username is not null do update set platform_role = excluded.platform_role, status = 'active'",
        )
        .bind(user_id)
        .bind(&format!("scope-contract-{}", Uuid::new_v4()))
        .bind(subject)
        .bind(&format!("{username}-{}", user_id))
        .bind(display_name)
        .bind(format!("{username}@example.test"))
        .bind(role)
        .execute(kernel.pool())
        .await
        .expect("canonical user inserts");
    }
    sqlx::query(
        "insert into projects (id, owner_user_id, name, kind) \
         values ($1, $2, 'directory-contract', 'team')",
    )
    .bind(project)
    .bind(project_owner)
    .execute(kernel.pool())
    .await
    .expect("team project inserts");
    sqlx::query(
        "insert into project_members (project_id, user_id, role) \
         values ($1, $2, 'owner'), ($1, $3, 'member')",
    )
    .bind(project)
    .bind(project_owner)
    .bind(project_member)
    .execute(kernel.pool())
    .await
    .expect("project memberships insert");

    let admin_auth = std::sync::Arc::new(
        Auth::connect(
            AuthConfig::native("http://scope-contract.test"),
            kernel.pool().clone(),
        )
        .await
        .expect("native auth connects"),
    );
    let admin_session = web_session::create(
        kernel.pool(),
        platform_admin,
        admin_auth.config().session_ttl(),
    )
    .await
    .expect("platform-admin session creates");
    let owner_session = web_session::create(
        kernel.pool(),
        project_owner,
        admin_auth.config().session_ttl(),
    )
    .await
    .expect("project-owner session creates");

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
        admin_auth.clone(),
        services,
    ));
    let public_origin = "http://scope-contract.test";

    let listed = get(port, "/api/admin/users", &admin_session.1).await;
    assert_eq!(listed.status, 200, "active platform admin can list users");
    let listed_body = String::from_utf8_lossy(&listed.body);
    for label in [
        "platform-admin",
        "Platform Administrator",
        "owner",
        "Project Owner",
        "member",
        "Project Member",
        "platformRole",
        "status",
    ] {
        assert!(
            listed_body.contains(label),
            "admin directory retains human-readable field {label}: {listed_body}"
        );
    }
    assert!(
        listed_body.contains(&platform_admin.to_string()),
        "admin directory still carries durable identity alongside labels"
    );

    let ordinary_admin_route = get(port, "/api/admin/users", &owner_session.1).await;
    assert_eq!(
        ordinary_admin_route.status, 403,
        "project membership does not grant platform user-management access"
    );
    let platform_admin_project =
        get(port, &format!("/api/projects/{project}"), &admin_session.1).await;
    assert_eq!(
        platform_admin_project.status, 404,
        "platform admin role does not silently grant project membership"
    );

    let project_detail = get(port, &format!("/api/projects/{project}"), &owner_session.1).await;
    assert_eq!(
        project_detail.status, 200,
        "project owner can read project detail"
    );
    let detail_body = String::from_utf8_lossy(&project_detail.body);
    assert!(
        detail_body.contains(&format!("\"username\":\"member-{project_member}\"")),
        "project member lookup exposes username, not only a UUID"
    );
    assert!(
        detail_body.contains("\"displayName\":\"Project Member\""),
        "project member lookup exposes display name, not only a provider subject"
    );
    assert!(
        detail_body.contains(&project_member.to_string()),
        "member detail retains the durable User identity"
    );

    let role_change = patch_json(
        port,
        &format!("/api/admin/users/{project_member}/role"),
        &admin_session.1,
        public_origin,
        r#"{"platformRole":"admin"}"#,
    )
    .await;
    assert_eq!(
        role_change.status, 200,
        "platform admin can change platform role"
    );
    assert!(
        String::from_utf8_lossy(&role_change.body).contains("\"updated\":true"),
        "role mutation returns an explicit update result"
    );
    let stored_platform_role: String =
        sqlx::query_scalar("select platform_role from users where id = $1")
            .bind(project_member)
            .fetch_one(kernel.pool())
            .await
            .expect("platform role query succeeds");
    assert_eq!(stored_platform_role, "admin");

    let status_change = patch_json(
        port,
        &format!("/api/admin/users/{project_member}/status"),
        &admin_session.1,
        public_origin,
        r#"{"status":"disabled"}"#,
    )
    .await;
    assert_eq!(
        status_change.status, 200,
        "platform admin can change user status"
    );
    let stored_status: String = sqlx::query_scalar("select status from users where id = $1")
        .bind(project_member)
        .fetch_one(kernel.pool())
        .await
        .expect("user status query succeeds");
    assert_eq!(stored_status, "disabled");

    server.abort();
    let _ = server.await;
}
