//! Behavioral contracts for the Release 0 product scope surface: same-origin
//! account routes, team/personal scope semantics, platform-admin authority,
//! and the guard rails that keep mutations honest.
//!
//! Every server is spawned in-process against a real ephemeral PostgreSQL
//! database (VOIE_TEST_DATABASE_URL) with dead-model/Blob/Fabric endpoints
//! like the sibling platform-admin contract. Mutations always carry the
//! same-origin + `x-voie-intent: mutate` headers the Web carrier sends.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use uuid::Uuid;
use voie_cloud::auth::{Auth, AuthConfig};
use voie_cloud::integration::Services;
use voie_cloud::web_session;
use voie_cloud::{Config, Kernel, KernelError, serve_with_services};

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvironmentRestore {
    previous: Vec<(&'static str, Option<OsString>)>,
}

impl EnvironmentRestore {
    fn new() -> Self {
        EnvironmentRestore {
            previous: Vec::new(),
        }
    }

    fn set(&mut self, name: &'static str, value: impl AsRef<std::ffi::OsStr>) {
        self.previous.push((name, std::env::var_os(name)));
        // Rust 2024 marks process-wide environment mutation unsafe. The
        // test holds ENV_LOCK, so no sibling contract test observes a
        // half-set fixture.
        unsafe { std::env::set_var(name, value) };
    }
}

impl Drop for EnvironmentRestore {
    fn drop(&mut self) {
        for (name, previous) in self.previous.drain(..).rev() {
            match previous {
                Some(value) => unsafe { std::env::set_var(name, value) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "voie-{label}-{}-{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&path).expect("fixture temp dir creates");
        TempDir(path)
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
    let cert = dir.path("client.pem");
    let key = dir.path("client.key");
    let ca = dir.path("ca.pem");
    let output = std::process::Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-keyout",
            key.to_str().expect("key path is UTF-8"),
            "-out",
            cert.to_str().expect("certificate path is UTF-8"),
            "-subj",
            "/CN=voie-scope-contract",
        ])
        .output()
        .expect("openssl is available for the local fixture");
    assert!(
        output.status.success(),
        "openssl creates fixture certificate"
    );
    std::fs::copy(&cert, &ca).expect("self-signed certificate is a usable test CA");
    (cert, key, ca)
}

struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

impl HttpResponse {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
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

/// One verbatim request with caller-chosen headers, for gate-refusal tests
/// that intentionally omit origin or intent.
async fn raw_request(
    port: u16,
    method: &str,
    path: &str,
    token: &str,
    origin: Option<&str>,
    intent: Option<&str>,
    body: Option<&str>,
) -> HttpResponse {
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\nconnection: close\r\ncookie: voie_session={token}\r\n"
    );
    if let Some(origin) = origin {
        request.push_str(&format!("origin: {origin}\r\n"));
    }
    if let Some(intent) = intent {
        request.push_str(&format!("x-voie-intent: {intent}\r\n"));
    }
    if let Some(body) = body {
        request.push_str(&format!(
            "content-type: application/json\r\ncontent-length: {}\r\n",
            body.len()
        ));
    }
    request.push_str("\r\n");
    if let Some(body) = body {
        request.push_str(body);
    }
    exchange(port, request).await
}

async fn post_json(port: u16, path: &str, token: &str, origin: &str, body: &str) -> HttpResponse {
    raw_request(
        port,
        "POST",
        path,
        token,
        Some(origin),
        Some("mutate"),
        Some(body),
    )
    .await
}

async fn patch_json(port: u16, path: &str, token: &str, origin: &str, body: &str) -> HttpResponse {
    raw_request(
        port,
        "PATCH",
        path,
        token,
        Some(origin),
        Some("mutate"),
        Some(body),
    )
    .await
}

async fn delete_request(
    port: u16,
    path: &str,
    token: &str,
    origin: Option<&str>,
    intent: Option<&str>,
) -> HttpResponse {
    raw_request(port, "DELETE", path, token, origin, intent, None).await
}

fn database_url() -> String {
    std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database")
}

/// Boots one in-process server against the ephemeral database and returns
/// the bound listener plus the shared kernel. The returned TempDir owns the
/// mTLS fixture material and MUST outlive `Services::from_env`; the caller
/// binds it to a name so it drops only after the server task is aborted.
async fn spawn_server(
    label: &str,
    environment: &mut EnvironmentRestore,
) -> (TempDir, tokio::net::TcpListener, Arc<Kernel>) {
    let fixture = TempDir::new(label);
    let (cert, key, ca) = fabric_pem_fixture(&fixture);
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

    let kernel = Arc::new(
        Kernel::connect(&Config::database_url(database_url()))
            .await
            .expect("PostgreSQL connection succeeds"),
    );
    kernel.migrate().await.expect("latest migration applies");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("API listener binds");
    (fixture, listener, kernel)
}

/// Inserts one canonical User plus its personal project scope and owner
/// membership row, mirroring `create_native_user_with_profile`'s layout.
async fn insert_user(
    kernel: &Kernel,
    user_id: Uuid,
    subject: &str,
    username: &str,
    display_name: &str,
    platform_role: &str,
) {
    sqlx::query(
        "insert into users \
         (id, issuer, subject, username, display_name, email, platform_role, status) \
         values ($1, $2, $3, $4, $5, $6, $7, 'active') \
         on conflict (username) where username is not null \
         do update set platform_role = excluded.platform_role, status = 'active'",
    )
    .bind(user_id)
    .bind(&format!("product-contract-{}", Uuid::new_v4()))
    .bind(subject)
    .bind(&format!("{username}-{user_id}"))
    .bind(display_name)
    .bind(format!("{username}@example.test"))
    .bind(platform_role)
    .execute(kernel.pool())
    .await
    .expect("canonical user inserts");
    let personal_id = Uuid::new_v4();
    sqlx::query("insert into projects (id, owner_user_id, name, kind) values ($1, $2, 'Personal', 'personal')")
        .bind(personal_id)
        .bind(user_id)
        .execute(kernel.pool())
        .await
        .expect("personal project inserts");
    sqlx::query("insert into project_members (project_id, user_id, role) values ($1, $2, 'owner')")
        .bind(personal_id)
        .bind(user_id)
        .execute(kernel.pool())
        .await
        .expect("owner membership inserts");
}

async fn insert_session(kernel: &Kernel, user_id: Uuid) -> String {
    web_session::create(
        kernel.pool(),
        user_id,
        std::time::Duration::from_secs(12 * 60 * 60),
    )
    .await
    .expect("web session creates")
    .1
}

const PUBLIC_ORIGIN: &str = "http://scope-contract.test";

/// PUT and DELETE are admitted only with the exact same-origin and the
/// mutate intent marker. Omitting either is refused before any routing.
#[tokio::test]
async fn put_delete_mutations_refused_without_origin_or_intent() {
    let _environment_lock = ENV_LOCK.lock().expect("environment fixture lock");
    let mut environment = EnvironmentRestore::new();
    let (_fixture, listener, kernel) = spawn_server("gate-refusal", &mut environment).await;
    let port = listener.local_addr().expect("listener address").port();
    let admin = Uuid::new_v4();
    insert_user(
        &kernel,
        admin,
        "gate-admin-subject",
        "gate-admin",
        "Gate Admin",
        "admin",
    )
    .await;
    let token = insert_session(&kernel, admin).await;

    let auth = Arc::new(
        Auth::connect(AuthConfig::native(PUBLIC_ORIGIN), kernel.pool().clone())
            .await
            .expect("native auth connects"),
    );
    let services = Services::from_env(kernel.pool().clone()).expect("service seams configure");
    let server = tokio::spawn(serve_with_services(
        listener,
        kernel.clone(),
        auth,
        services,
    ));

    let secret_id = Uuid::new_v4();
    let put_no_intent = raw_request(
        port,
        "PUT",
        &format!("/api/secrets/{secret_id}"),
        &token,
        Some(PUBLIC_ORIGIN),
        None,
        Some(r#"{"value":"material"}"#),
    )
    .await;
    assert_eq!(
        put_no_intent.status,
        403,
        "PUT without the intent marker is refused: {}",
        put_no_intent.text()
    );

    let put_no_origin = raw_request(
        port,
        "PUT",
        &format!("/api/secrets/{secret_id}"),
        &token,
        None,
        Some("mutate"),
        Some(r#"{"value":"material"}"#),
    )
    .await;
    assert_eq!(
        put_no_origin.status,
        403,
        "PUT without a same-origin header is refused: {}",
        put_no_origin.text()
    );

    let delete_bare = delete_request(port, "/api/secrets/1", &token, None, None).await;
    assert_eq!(
        delete_bare.status,
        403,
        "DELETE with neither origin nor intent is refused: {}",
        delete_bare.text()
    );

    server.abort();
    let _ = server.await;
}

/// A provider (issuer, subject) links to exactly one User: repeating the
/// same link is idempotent, linking a pair already owned by another User
/// is a conflict.
#[tokio::test]
async fn link_identity_conflicts_across_users_and_idempotent_within_one() {
    let _environment_lock = ENV_LOCK.lock().expect("environment fixture lock");
    let mut environment = EnvironmentRestore::new();
    let (_fixture, _listener, kernel) = spawn_server("link-identity", &mut environment).await;
    // The shared dev database retains links across runs, so the provider
    // pair must be unique per run to avoid a stale conflict.
    let issuer = format!("https://issuer-{}.example", Uuid::new_v4());
    let subject = format!("subject-{}", Uuid::new_v4());
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    insert_user(
        &kernel,
        first,
        "link-first",
        "link-first",
        "Link First",
        "user",
    )
    .await;
    insert_user(
        &kernel,
        second,
        "link-second",
        "link-second",
        "Link Second",
        "user",
    )
    .await;

    kernel
        .link_identity(first, "oidc", &issuer, &subject)
        .await
        .expect("first link succeeds");
    kernel
        .link_identity(first, "oidc", &issuer, &subject)
        .await
        .expect("same-user repeat is idempotent");
    let conflict = kernel
        .link_identity(second, "oidc", &issuer, &subject)
        .await;
    assert!(
        matches!(conflict, Err(KernelError::Conflict)),
        "a provider pair already linked to another User conflicts"
    );
}

/// GET /api/me carries the stable profile fields the account overview and
/// directory share.
#[tokio::test]
async fn me_exposes_stable_profile_fields() {
    let _environment_lock = ENV_LOCK.lock().expect("environment fixture lock");
    let mut environment = EnvironmentRestore::new();
    let (_fixture, listener, kernel) = spawn_server("me-profile", &mut environment).await;
    let port = listener.local_addr().expect("listener address").port();
    let user = Uuid::new_v4();
    insert_user(
        &kernel,
        user,
        "me-profile-subject",
        "me-profile",
        "Me Profile",
        "user",
    )
    .await;
    let token = insert_session(&kernel, user).await;

    let auth = Arc::new(
        Auth::connect(AuthConfig::native(PUBLIC_ORIGIN), kernel.pool().clone())
            .await
            .expect("native auth connects"),
    );
    let services = Services::from_env(kernel.pool().clone()).expect("service seams configure");
    let server = tokio::spawn(serve_with_services(
        listener,
        kernel.clone(),
        auth,
        services,
    ));

    let me = get(port, "/api/me", &token).await;
    assert_eq!(
        me.status,
        200,
        "session holder reads /api/me: {}",
        me.text()
    );
    let body = me.text();
    for label in [
        "\"userId\"",
        "\"username\"",
        "\"displayName\"",
        "\"email\"",
        "\"username\":\"me-profile-",
        "\"displayName\":\"Me Profile\"",
    ] {
        assert!(body.contains(label), "me profile carries {label}: {body}");
    }

    server.abort();
    let _ = server.await;
}

/// A personal scope's membership is fixed to its owner; the legacy project
/// member route and the scope member route both refuse additions.
#[tokio::test]
async fn personal_scope_membership_refused_on_both_member_routes() {
    let _environment_lock = ENV_LOCK.lock().expect("environment fixture lock");
    let mut environment = EnvironmentRestore::new();
    let (_fixture, listener, kernel) = spawn_server("personal-fixed", &mut environment).await;
    let port = listener.local_addr().expect("listener address").port();
    let owner = Uuid::new_v4();
    let outsider = Uuid::new_v4();
    insert_user(
        &kernel,
        owner,
        "personal-owner",
        "personal-owner",
        "Personal Owner",
        "user",
    )
    .await;
    insert_user(
        &kernel,
        outsider,
        "personal-out",
        "personal-out",
        "Personal Out",
        "user",
    )
    .await;
    let personal_id: Uuid = sqlx::query_scalar(
        "select id from projects where owner_user_id = $1 and kind = 'personal'",
    )
    .bind(owner)
    .fetch_one(kernel.pool())
    .await
    .expect("personal scope exists");
    let token = insert_session(&kernel, owner).await;

    let auth = Arc::new(
        Auth::connect(AuthConfig::native(PUBLIC_ORIGIN), kernel.pool().clone())
            .await
            .expect("native auth connects"),
    );
    let services = Services::from_env(kernel.pool().clone()).expect("service seams configure");
    let server = tokio::spawn(serve_with_services(
        listener,
        kernel.clone(),
        auth,
        services,
    ));

    let body = format!(r#"{{"userId":"{outsider}","role":"member"}}"#);
    let legacy = post_json(
        port,
        &format!("/api/projects/{personal_id}/members"),
        &token,
        PUBLIC_ORIGIN,
        &body,
    )
    .await;
    assert_eq!(
        legacy.status,
        409,
        "legacy member route refuses a personal scope add: {}",
        legacy.text()
    );
    let scope_route = post_json(
        port,
        &format!("/api/scopes/{personal_id}/members"),
        &token,
        PUBLIC_ORIGIN,
        &body,
    )
    .await;
    assert_eq!(
        scope_route.status,
        409,
        "scope member route refuses a personal scope add: {}",
        scope_route.text()
    );

    server.abort();
    let _ = server.await;
}

/// The product API drives a full team flow: create a team scope, search the
/// active directory, add a member, and see the membership through the
/// scope list.
#[tokio::test]
async fn team_scope_create_search_add_flow_through_product_api() {
    let _environment_lock = ENV_LOCK.lock().expect("environment fixture lock");
    let mut environment = EnvironmentRestore::new();
    let (_fixture, listener, kernel) = spawn_server("team-flow", &mut environment).await;
    let port = listener.local_addr().expect("listener address").port();
    let owner = Uuid::new_v4();
    let recruit = Uuid::new_v4();
    insert_user(
        &kernel,
        owner,
        "team-owner",
        "team-owner",
        "Team Owner",
        "user",
    )
    .await;
    insert_user(
        &kernel,
        recruit,
        "team-recruit",
        "team-recruit",
        "Team Recruit",
        "user",
    )
    .await;
    let token = insert_session(&kernel, owner).await;

    let auth = Arc::new(
        Auth::connect(AuthConfig::native(PUBLIC_ORIGIN), kernel.pool().clone())
            .await
            .expect("native auth connects"),
    );
    let services = Services::from_env(kernel.pool().clone()).expect("service seams configure");
    let server = tokio::spawn(serve_with_services(
        listener,
        kernel.clone(),
        auth,
        services,
    ));

    let team_id = Uuid::new_v4();
    let create = post_json(
        port,
        "/api/scopes",
        &token,
        PUBLIC_ORIGIN,
        &format!(r#"{{"id":"{team_id}","name":"Team Contract"}}"#),
    )
    .await;
    assert_eq!(create.status, 201, "team scope creates: {}", create.text());
    assert!(create.text().contains("\"kind\":\"team\""));

    // The search route matches a case-insensitive fragment against username
    // or display name without decoding; a single word keeps the request
    // line clean and still proves the directory search finds the recruit.
    let search = get(
        port,
        &format!("/api/scopes/users/search?q={}", "Recruit"),
        &token,
    )
    .await;
    assert_eq!(
        search.status,
        200,
        "directory search reads: {}",
        search.text()
    );
    assert!(
        search.text().contains(&recruit.to_string()),
        "search finds the recruit by display name: {}",
        search.text()
    );

    let add = post_json(
        port,
        &format!("/api/scopes/{team_id}/members"),
        &token,
        PUBLIC_ORIGIN,
        &format!(r#"{{"userId":"{recruit}","role":"member"}}"#),
    )
    .await;
    assert_eq!(add.status, 200, "team member adds: {}", add.text());

    let recruit_scopes = get(port, "/api/scopes", &insert_session(&kernel, recruit).await).await;
    assert_eq!(
        recruit_scopes.status,
        200,
        "recruit sees their scopes: {}",
        recruit_scopes.text()
    );
    assert!(
        recruit_scopes
            .text()
            .contains(&format!("\"name\":\"Team Contract\"")),
        "recruit's scope list includes the team"
    );

    server.abort();
    let _ = server.await;
}

/// Platform-role operations demand an active platform admin: a regular
/// user is refused and an admin's change lands in the committed store.
#[tokio::test]
async fn admin_role_operations_require_and_honor_platform_admin() {
    let _environment_lock = ENV_LOCK.lock().expect("environment fixture lock");
    let mut environment = EnvironmentRestore::new();
    let (_fixture, listener, kernel) = spawn_server("admin-ops", &mut environment).await;
    let port = listener.local_addr().expect("listener address").port();
    let admin = Uuid::new_v4();
    let regular = Uuid::new_v4();
    let target = Uuid::new_v4();
    insert_user(
        &kernel,
        admin,
        "admin-ops-admin",
        "admin-ops-admin",
        "Admin Ops",
        "admin",
    )
    .await;
    insert_user(
        &kernel,
        regular,
        "admin-ops-user",
        "admin-ops-user",
        "Admin Ops U",
        "user",
    )
    .await;
    insert_user(
        &kernel,
        target,
        "admin-ops-tgt",
        "admin-ops-tgt",
        "Admin Ops T",
        "user",
    )
    .await;
    let admin_token = insert_session(&kernel, admin).await;
    let regular_token = insert_session(&kernel, regular).await;

    let auth = Arc::new(
        Auth::connect(AuthConfig::native(PUBLIC_ORIGIN), kernel.pool().clone())
            .await
            .expect("native auth connects"),
    );
    let services = Services::from_env(kernel.pool().clone()).expect("service seams configure");
    let server = tokio::spawn(serve_with_services(
        listener,
        kernel.clone(),
        auth,
        services,
    ));

    let denied = patch_json(
        port,
        &format!("/api/admin/users/{target}/role"),
        &regular_token,
        PUBLIC_ORIGIN,
        r#"{"platformRole":"admin"}"#,
    )
    .await;
    assert_eq!(
        denied.status,
        403,
        "regular user cannot change platform roles: {}",
        denied.text()
    );

    let granted = patch_json(
        port,
        &format!("/api/admin/users/{target}/role"),
        &admin_token,
        PUBLIC_ORIGIN,
        r#"{"platformRole":"admin"}"#,
    )
    .await;
    assert_eq!(
        granted.status,
        200,
        "admin role change lands: {}",
        granted.text()
    );
    let stored: String = sqlx::query_scalar("select platform_role from users where id = $1")
        .bind(target)
        .fetch_one(kernel.pool())
        .await
        .expect("platform role query succeeds");
    assert_eq!(stored, "admin");

    server.abort();
    let _ = server.await;
}

/// Two concurrent demotions of two active admins cannot leave zero active
/// admins: the advisory-lock guard serializes the count check, so exactly
/// one demotion commits and the other is refused.
#[tokio::test]
async fn concurrent_admin_demotions_lose_exactly_one() {
    let _environment_lock = ENV_LOCK.lock().expect("environment fixture lock");
    let mut environment = EnvironmentRestore::new();
    let (_fixture, listener, kernel) = spawn_server("demote-race", &mut environment).await;
    let port = listener.local_addr().expect("listener address").port();
    // The shared dev database retains users across runs, so the final-admin
    // guard needs a controlled population: exactly two active admins A and
    // B. Demoting both concurrently can only ever commit one.
    sqlx::query("update users set platform_role = 'user' where platform_role = 'admin' and status = 'active'")
        .execute(kernel.pool())
        .await
        .expect("pre-existing admins demote");
    let admin_a = Uuid::new_v4();
    let admin_b = Uuid::new_v4();
    insert_user(
        &kernel, admin_a, "demote-a", "demote-a", "Demote A", "admin",
    )
    .await;
    insert_user(
        &kernel, admin_b, "demote-b", "demote-b", "Demote B", "admin",
    )
    .await;
    let token_a = insert_session(&kernel, admin_a).await;
    let token_b = insert_session(&kernel, admin_b).await;

    let auth = Arc::new(
        Auth::connect(AuthConfig::native(PUBLIC_ORIGIN), kernel.pool().clone())
            .await
            .expect("native auth connects"),
    );
    let services = Services::from_env(kernel.pool().clone()).expect("service seams configure");
    let server = tokio::spawn(serve_with_services(
        listener,
        kernel.clone(),
        auth,
        services,
    ));

    let demote_b = {
        let path = format!("/api/admin/users/{admin_b}/role");
        let token = token_a.clone();
        async move {
            patch_json(
                port,
                &path,
                &token,
                PUBLIC_ORIGIN,
                r#"{"platformRole":"user"}"#,
            )
            .await
        }
    };
    let demote_a = {
        let path = format!("/api/admin/users/{admin_a}/role");
        let token = token_b.clone();
        async move {
            patch_json(
                port,
                &path,
                &token,
                PUBLIC_ORIGIN,
                r#"{"platformRole":"user"}"#,
            )
            .await
        }
    };
    let (first, second) = tokio::join!(demote_b, demote_a);
    let statuses = [first.status, second.status];
    assert_eq!(
        statuses.iter().filter(|status| **status == 200).count(),
        1,
        "exactly one concurrent demotion commits: {statuses:?}"
    );
    assert!(
        statuses.contains(&409),
        "the losing demotion is refused as the final active admin: {statuses:?}"
    );

    let active_admins: i64 = sqlx::query_scalar(
        "select count(*) from users where platform_role = 'admin' and status = 'active'",
    )
    .fetch_one(kernel.pool())
    .await
    .expect("admin count query succeeds");
    assert_eq!(
        active_admins, 1,
        "exactly one of the two concurrent demotions committed"
    );

    server.abort();
    let _ = server.await;
}

/// Resetting a User's native password revokes every live Web session of
/// that User: the old cookie stops authenticating immediately.
#[tokio::test]
async fn admin_reset_password_revokes_target_session_immediately() {
    let _environment_lock = ENV_LOCK.lock().expect("environment fixture lock");
    let mut environment = EnvironmentRestore::new();
    let (_fixture, listener, kernel) = spawn_server("reset-revoke", &mut environment).await;
    let port = listener.local_addr().expect("listener address").port();
    let admin = Uuid::new_v4();
    insert_user(
        &kernel,
        admin,
        "reset-admin",
        "reset-admin",
        "Reset Admin",
        "admin",
    )
    .await;
    // The reset route replaces an existing native credential, so the target
    // must own one; admin_create_user provisions it.
    let target = voie_cloud::auth::admin_create_user(
        &kernel,
        &format!("reset-target-{}", Uuid::new_v4()),
        "Reset Target",
        Some("reset-target@example.test"),
        "user",
        "OldTargetPassw0rd!",
    )
    .await
    .expect("native target creates")
    .id;
    let admin_token = insert_session(&kernel, admin).await;
    let target_token = insert_session(&kernel, target).await;

    let auth = Arc::new(
        Auth::connect(AuthConfig::native(PUBLIC_ORIGIN), kernel.pool().clone())
            .await
            .expect("native auth connects"),
    );
    let services = Services::from_env(kernel.pool().clone()).expect("service seams configure");
    let server = tokio::spawn(serve_with_services(
        listener,
        kernel.clone(),
        auth,
        services,
    ));

    let reset = post_json(
        port,
        &format!("/api/admin/users/{target}/reset-password"),
        &admin_token,
        PUBLIC_ORIGIN,
        r#"{"password":"NewStrongPassw0rd!"}"#,
    )
    .await;
    assert_eq!(
        reset.status,
        200,
        "admin resets the native password: {}",
        reset.text()
    );

    let revoked = get(port, "/api/me", &target_token).await;
    assert_eq!(
        revoked.status,
        401,
        "the revoked session is refused immediately: {}",
        revoked.text()
    );

    server.abort();
    let _ = server.await;
}

/// Self-service password change verifies the current Argon2id credential
/// before any mutation, then revokes every OTHER session while keeping the
/// acting session live. A wrong current password changes nothing.
#[tokio::test]
async fn account_password_change_revokes_others_and_rejects_wrong_current() {
    let _environment_lock = ENV_LOCK.lock().expect("environment fixture lock");
    let mut environment = EnvironmentRestore::new();
    let (_fixture, listener, kernel) = spawn_server("account-password", &mut environment).await;
    let port = listener.local_addr().expect("listener address").port();
    // The public admin_create_user path provisions a canonical User with a
    // real Argon2id native credential and its personal scope in one place.
    let username = format!("acct-pw-{}", Uuid::new_v4());
    let user = voie_cloud::auth::admin_create_user(
        &kernel,
        &username,
        "Account Password",
        Some("acct-pw@example.test"),
        "user",
        "OriginalPassw0rd!",
    )
    .await
    .expect("native user with credential creates")
    .id;
    let session_one = insert_session(&kernel, user).await;
    let session_two = insert_session(&kernel, user).await;

    let auth = Arc::new(
        Auth::connect(AuthConfig::native(PUBLIC_ORIGIN), kernel.pool().clone())
            .await
            .expect("native auth connects"),
    );
    let services = Services::from_env(kernel.pool().clone()).expect("service seams configure");
    let server = tokio::spawn(serve_with_services(
        listener,
        kernel.clone(),
        auth,
        services,
    ));

    // Wrong current password: rejected, and both sessions stay live.
    let wrong = post_json(
        port,
        "/api/account/password",
        &session_one,
        PUBLIC_ORIGIN,
        r#"{"currentPassword":"WrongPassw0rd!","newPassword":"NewerPassw0rd!"}"#,
    )
    .await;
    assert_eq!(
        wrong.status,
        400,
        "wrong current password is rejected: {}",
        wrong.text()
    );
    assert_eq!(
        get(port, "/api/me", &session_two).await.status,
        200,
        "a wrong current password must not revoke anything"
    );

    // Correct current password: succeeds, other sessions revoked, acting
    // session stays live.
    let changed = post_json(
        port,
        "/api/account/password",
        &session_one,
        PUBLIC_ORIGIN,
        r#"{"currentPassword":"OriginalPassw0rd!","newPassword":"NewerPassw0rd!"}"#,
    )
    .await;
    assert_eq!(
        changed.status,
        200,
        "correct current password changes: {}",
        changed.text()
    );
    assert_eq!(
        get(port, "/api/me", &session_two).await.status,
        401,
        "the other session is revoked by the password change"
    );
    assert_eq!(
        get(port, "/api/me", &session_one).await.status,
        200,
        "the acting session survives the password change"
    );

    server.abort();
    let _ = server.await;
}

/// Every platform-admin read is a hard 403 for a regular user.
#[tokio::test]
async fn regular_user_gets_forbidden_on_every_admin_read() {
    let _environment_lock = ENV_LOCK.lock().expect("environment fixture lock");
    let mut environment = EnvironmentRestore::new();
    let (_fixture, listener, kernel) = spawn_server("admin-gate", &mut environment).await;
    let port = listener.local_addr().expect("listener address").port();
    let regular = Uuid::new_v4();
    insert_user(
        &kernel,
        regular,
        "gate-user",
        "gate-user",
        "Gate User",
        "user",
    )
    .await;
    let token = insert_session(&kernel, regular).await;

    let auth = Arc::new(
        Auth::connect(AuthConfig::native(PUBLIC_ORIGIN), kernel.pool().clone())
            .await
            .expect("native auth connects"),
    );
    let services = Services::from_env(kernel.pool().clone()).expect("service seams configure");
    let server = tokio::spawn(serve_with_services(
        listener,
        kernel.clone(),
        auth,
        services,
    ));

    for path in [
        "/api/admin/users",
        "/api/admin/scopes",
        "/api/admin/fabrics",
        "/api/admin/workspaces",
        "/api/admin/audit",
        "/api/admin/health",
    ] {
        let response = get(port, path, &token).await;
        assert_eq!(
            response.status,
            403,
            "regular user is forbidden on {path}: {}",
            response.text()
        );
    }

    server.abort();
    let _ = server.await;
}
/// A platform-admin flip reports success only when the mutation actually
/// committed: `updated:true` never appears for a refused or failed flip,
/// and a committed disable really changed the status AND revoked the
/// User's live sessions.
#[tokio::test]
async fn admin_flip_success_only_after_real_commit() {
    let _environment_lock = ENV_LOCK.lock().expect("environment fixture lock");
    let mut environment = EnvironmentRestore::new();
    let (_fixture, listener, kernel) = spawn_server("flip-commit", &mut environment).await;
    let port = listener.local_addr().expect("listener address").port();
    // The shared dev database retains users across runs, so the "final
    // active platform admin" guard only fires against a controlled admin
    // population. Demote every pre-existing active admin so this test's
    // admin is the sole one; only its own rows are touched.
    sqlx::query("update users set platform_role = 'user' where platform_role = 'admin' and status = 'active'")
        .execute(kernel.pool())
        .await
        .expect("pre-existing admins demote");
    let admin = Uuid::new_v4();
    let target = Uuid::new_v4();
    insert_user(
        &kernel,
        admin,
        "flip-admin",
        "flip-admin",
        "Flip Admin",
        "admin",
    )
    .await;
    insert_user(
        &kernel,
        target,
        "flip-target",
        "flip-target",
        "Flip Target",
        "user",
    )
    .await;
    let admin_token = insert_session(&kernel, admin).await;
    let target_token = insert_session(&kernel, target).await;

    let auth = Arc::new(
        Auth::connect(AuthConfig::native(PUBLIC_ORIGIN), kernel.pool().clone())
            .await
            .expect("native auth connects"),
    );
    let services = Services::from_env(kernel.pool().clone()).expect("service seams configure");
    let server = tokio::spawn(serve_with_services(
        listener,
        kernel.clone(),
        auth,
        services,
    ));

    // A refused flip (final active admin cannot be disabled) carries no
    // success claim: the status stays active and the session stays live.
    let refused = patch_json(
        port,
        &format!("/api/admin/users/{admin}/status"),
        &admin_token,
        PUBLIC_ORIGIN,
        r#"{"status":"disabled"}"#,
    )
    .await;
    assert_eq!(refused.status, 409, "final admin disable is refused");
    assert!(
        !refused.text().contains("\"updated\":true"),
        "a refused flip never claims success: {}",
        refused.text()
    );
    let admin_status: String = sqlx::query_scalar("select status from users where id = $1")
        .bind(admin)
        .fetch_one(kernel.pool())
        .await
        .expect("admin status query succeeds");
    assert_eq!(
        admin_status, "active",
        "refused flip leaves the admin active"
    );
    assert_eq!(
        get(port, "/api/me", &admin_token).await.status,
        200,
        "refused flip leaves the session live"
    );

    // A committed disable really disabled the User AND revoked its
    // sessions, and only then reported updated:true.
    let committed = patch_json(
        port,
        &format!("/api/admin/users/{target}/status"),
        &admin_token,
        PUBLIC_ORIGIN,
        r#"{"status":"disabled"}"#,
    )
    .await;
    assert_eq!(committed.status, 200, "admin disables a regular user");
    assert!(
        committed.text().contains("\"updated\":true"),
        "committed flip reports success: {}",
        committed.text()
    );
    let stored_status: String = sqlx::query_scalar("select status from users where id = $1")
        .bind(target)
        .fetch_one(kernel.pool())
        .await
        .expect("target status query succeeds");
    assert_eq!(stored_status, "disabled", "committed flip stored disabled");
    let remaining: i64 = sqlx::query_scalar("select count(*) from web_sessions where user_id = $1")
        .bind(target)
        .fetch_one(kernel.pool())
        .await
        .expect("session count query succeeds");
    assert_eq!(
        remaining, 0,
        "committed disable revoked the target's sessions"
    );
    assert_eq!(
        get(port, "/api/me", &target_token).await.status,
        401,
        "disabled User's session no longer authenticates"
    );

    server.abort();
    let _ = server.await;
}

/// A platform admin recovers Team RBAC without becoming a Team member:
/// list, add, rerole, and remove through `/api/admin/scopes/:id/members`
/// while the ordinary membership route stays forbidden and the durable
/// owner plus Personal membership stay protected.
#[tokio::test]
async fn platform_admin_recovers_team_rbac_without_joining() {
    let _environment_lock = ENV_LOCK.lock().expect("environment fixture lock");
    let mut environment = EnvironmentRestore::new();
    let (_fixture, listener, kernel) = spawn_server("admin-rbac", &mut environment).await;
    let port = listener.local_addr().expect("listener address").port();
    let admin = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let recruit = Uuid::new_v4();
    let outsider = Uuid::new_v4();
    insert_user(
        &kernel,
        admin,
        "rbac-admin",
        "rbac-admin",
        "Rbac Admin",
        "admin",
    )
    .await;
    insert_user(
        &kernel,
        owner,
        "rbac-owner",
        "rbac-owner",
        "Rbac Owner",
        "user",
    )
    .await;
    insert_user(
        &kernel,
        recruit,
        "rbac-recruit",
        "rbac-recruit",
        "Rbac Recruit",
        "user",
    )
    .await;
    insert_user(
        &kernel, outsider, "rbac-out", "rbac-out", "Rbac Out", "user",
    )
    .await;
    let admin_token = insert_session(&kernel, admin).await;
    let owner_token = insert_session(&kernel, owner).await;
    let outsider_token = insert_session(&kernel, outsider).await;

    let auth = Arc::new(
        Auth::connect(AuthConfig::native(PUBLIC_ORIGIN), kernel.pool().clone())
            .await
            .expect("native auth connects"),
    );
    let services = Services::from_env(kernel.pool().clone()).expect("service seams configure");
    let server = tokio::spawn(serve_with_services(
        listener,
        kernel.clone(),
        auth,
        services,
    ));

    let team_id = Uuid::new_v4();
    let create = post_json(
        port,
        "/api/scopes",
        &owner_token,
        PUBLIC_ORIGIN,
        &format!(r#"{{"id":"{team_id}","name":"Recover Team"}}"#),
    )
    .await;
    assert_eq!(
        create.status,
        201,
        "owner creates the team: {}",
        create.text()
    );

    let ordinary_as_admin = get(
        port,
        &format!("/api/scopes/{team_id}/members"),
        &admin_token,
    )
    .await;
    assert_eq!(
        ordinary_as_admin.status,
        403,
        "platform admin is not a hidden team member: {}",
        ordinary_as_admin.text()
    );

    let outsider_admin_list = get(
        port,
        &format!("/api/admin/scopes/{team_id}/members"),
        &outsider_token,
    )
    .await;
    assert_eq!(
        outsider_admin_list.status,
        403,
        "regular user cannot list admin members: {}",
        outsider_admin_list.text()
    );

    let listed = get(
        port,
        &format!("/api/admin/scopes/{team_id}/members"),
        &admin_token,
    )
    .await;
    assert_eq!(
        listed.status,
        200,
        "admin lists team members: {}",
        listed.text()
    );
    assert!(
        listed.text().contains(&owner.to_string()),
        "admin roster includes the owner: {}",
        listed.text()
    );
    assert!(
        listed.text().contains("\"displayName\":\"Rbac Owner\""),
        "admin roster carries human identity: {}",
        listed.text()
    );

    let add = post_json(
        port,
        &format!("/api/admin/scopes/{team_id}/members"),
        &admin_token,
        PUBLIC_ORIGIN,
        &format!(r#"{{"userId":"{recruit}","role":"member"}}"#),
    )
    .await;
    assert_eq!(add.status, 200, "admin adds a member: {}", add.text());

    let rerole = post_json(
        port,
        &format!("/api/admin/scopes/{team_id}/members"),
        &admin_token,
        PUBLIC_ORIGIN,
        &format!(r#"{{"userId":"{recruit}","role":"admin"}}"#),
    )
    .await;
    assert_eq!(
        rerole.status,
        200,
        "admin reroles the member: {}",
        rerole.text()
    );
    let stored_role: String = sqlx::query_scalar(
        "select role from project_members where project_id = $1 and user_id = $2",
    )
    .bind(team_id)
    .bind(recruit)
    .fetch_one(kernel.pool())
    .await
    .expect("recruit role query succeeds");
    assert_eq!(stored_role, "admin", "rerole committed admin");

    let demote_owner = post_json(
        port,
        &format!("/api/admin/scopes/{team_id}/members"),
        &admin_token,
        PUBLIC_ORIGIN,
        &format!(r#"{{"userId":"{owner}","role":"member"}}"#),
    )
    .await;
    assert_eq!(
        demote_owner.status,
        409,
        "admin cannot demote the durable owner: {}",
        demote_owner.text()
    );

    let remove_owner = delete_request(
        port,
        &format!("/api/admin/scopes/{team_id}/members/{owner}"),
        &admin_token,
        Some(PUBLIC_ORIGIN),
        Some("mutate"),
    )
    .await;
    assert_eq!(
        remove_owner.status,
        409,
        "admin cannot remove the durable owner: {}",
        remove_owner.text()
    );

    let remove_recruit = delete_request(
        port,
        &format!("/api/admin/scopes/{team_id}/members/{recruit}"),
        &admin_token,
        Some(PUBLIC_ORIGIN),
        Some("mutate"),
    )
    .await;
    assert_eq!(
        remove_recruit.status,
        200,
        "admin removes the recovered member: {}",
        remove_recruit.text()
    );

    let admin_joined: bool = sqlx::query_scalar(
        "select exists(select 1 from project_members where project_id = $1 and user_id = $2)",
    )
    .bind(team_id)
    .bind(admin)
    .fetch_one(kernel.pool())
    .await
    .expect("admin membership query succeeds");
    assert!(
        !admin_joined,
        "platform admin recovery must not insert a team membership"
    );

    let personal_id: Uuid = sqlx::query_scalar(
        "select id from projects where owner_user_id = $1 and kind = 'personal'",
    )
    .bind(owner)
    .fetch_one(kernel.pool())
    .await
    .expect("owner personal scope exists");
    let personal_add = post_json(
        port,
        &format!("/api/admin/scopes/{personal_id}/members"),
        &admin_token,
        PUBLIC_ORIGIN,
        &format!(r#"{{"userId":"{recruit}","role":"member"}}"#),
    )
    .await;
    assert_eq!(
        personal_add.status,
        409,
        "admin cannot mutate personal membership: {}",
        personal_add.text()
    );

    let outsider_add = post_json(
        port,
        &format!("/api/admin/scopes/{team_id}/members"),
        &outsider_token,
        PUBLIC_ORIGIN,
        &format!(r#"{{"userId":"{recruit}","role":"viewer"}}"#),
    )
    .await;
    assert_eq!(
        outsider_add.status,
        403,
        "regular user cannot recover team RBAC: {}",
        outsider_add.text()
    );

    server.abort();
    let _ = server.await;
}
