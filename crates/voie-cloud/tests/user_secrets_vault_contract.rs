//! Scoped user-secret vault contracts.
//!
//! Secret values are write-only server/KV material. The control plane exposes
//! scope-authorized metadata and an auditable version trail, never values in
//! resource responses, audit metadata, or event feeds. These tests exercise
//! the real authenticated HTTP surface and inspect only the durable metadata
//! rows that are allowed to remain in PostgreSQL.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use sqlx::Row;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;
use voie_cloud::auth::{Auth, AuthConfig};
use voie_cloud::web_session::{self, COOKIE_NAME};
use voie_cloud::{Config, Kernel, serve_with_services};

fn database_url() -> String {
    std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database")
}

async fn fresh_kernel() -> Arc<Kernel> {
    let kernel = Kernel::connect(&Config::database_url(database_url()))
        .await
        .expect("PostgreSQL connection succeeds");
    kernel.migrate().await.expect("all migrations apply");
    Arc::new(kernel)
}

fn set_env(name: &str, value: &str) {
    // Services::from_env reads these values while the test surface starts.
    unsafe { std::env::set_var(name, value) };
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir() -> TempDir {
    let path = std::env::temp_dir().join(format!("voie-secret-vault-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&path).expect("test temp directory creates");
    TempDir(path)
}

/// A single self-signed certificate is sufficient because this contract never
/// connects to the Fabric; Services still validates the configured material.
fn fabric_certificate_files(dir: &Path) -> (String, String, String) {
    let cert = dir.join("fabric.pem");
    let key = dir.join("fabric.key");
    let output = std::process::Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-keyout",
            key.to_str().expect("key path is UTF-8"),
            "-out",
            cert.to_str().expect("certificate path is UTF-8"),
            "-days",
            "2",
            "-nodes",
            "-subj",
            "/CN=voie-secret-vault-test",
        ])
        .output()
        .expect("openssl runs");
    assert!(
        output.status.success(),
        "openssl failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cert = cert.to_str().expect("certificate path is UTF-8").to_owned();
    let key = key.to_str().expect("key path is UTF-8").to_owned();
    // The local contract does not make an outbound Fabric request, so the
    // self-signed certificate can serve as the trust root as well.
    (cert.clone(), key, cert)
}

struct Surface {
    kernel: Arc<Kernel>,
    auth: Arc<Auth>,
    port: u16,
    origin: String,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
    _certs: TempDir,
}

impl Surface {
    async fn stop(self) {
        self.server.abort();
        let _ = self.server.await;
    }
}

async fn http_surface() -> Surface {
    let kernel = fresh_kernel().await;
    let certs = temp_dir();
    let (client_cert, client_key, ca_cert) = fabric_certificate_files(&certs.0);

    set_env("VOIE_AZURE_BLOB_ACCOUNT", "voie-secret-vault-test");
    set_env(
        "VOIE_AZURE_BLOB_KEY",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    set_env("VOIE_AZURE_BLOB_CONTAINER", "voie-secret-vault-test");
    set_env("VOIE_AZURE_BLOB_ENDPOINT", "http://127.0.0.1:9");
    set_env("VOIE_MODEL_BASE_URL", "https://127.0.0.1:9/v1");
    set_env("VOIE_MODEL_NAME", "secret-vault-test-model");
    set_env("VOIE_MODEL_API_KEY", "secret-vault-test-key");
    set_env("VOIE_FABRIC_ENDPOINT", "https://127.0.0.1:9/");
    set_env("VOIE_FABRIC_CLIENT_CERT_PATH", &client_cert);
    set_env("VOIE_FABRIC_CLIENT_KEY_PATH", &client_key);
    set_env("VOIE_FABRIC_CA_CERT_PATH", &ca_cert);

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("API listener binds");
    let port = listener
        .local_addr()
        .expect("listener address exists")
        .port();
    let origin = format!("http://127.0.0.1:{port}");
    let auth = Arc::new(
        Auth::connect(AuthConfig::native(origin.clone()), kernel.pool().clone())
            .await
            .expect("native auth initializes"),
    );
    let services = voie_cloud::integration::Services::from_env(kernel.pool().clone())
        .expect("local service configuration resolves");
    let server = tokio::spawn(serve_with_services(
        listener,
        kernel.clone(),
        auth.clone(),
        services,
    ));

    Surface {
        kernel,
        auth,
        port,
        origin,
        server,
        _certs: certs,
    }
}

async fn insert_user(kernel: &Kernel, id: Uuid, issuer: &str, subject: &str, status: &str) {
    sqlx::query("insert into users (id, issuer, subject, status) values ($1, $2, $3, $4)")
        .bind(id)
        .bind(issuer)
        .bind(subject)
        .bind(status)
        .execute(kernel.pool())
        .await
        .expect("test user inserts");
}

async fn add_member(kernel: &Kernel, scope_id: Uuid, user_id: Uuid, role: &str) {
    sqlx::query("insert into project_members (project_id, user_id, role) values ($1, $2, $3)")
        .bind(scope_id)
        .bind(user_id)
        .bind(role)
        .execute(kernel.pool())
        .await
        .expect("scope membership inserts");
}

struct ScopeSeed {
    marker: String,
    owner: Uuid,
    admin: Uuid,
    member: Uuid,
    viewer: Uuid,
    foreign: Uuid,
    disabled: Uuid,
    personal: Uuid,
    team: Uuid,
}

async fn seed_scopes(kernel: &Kernel) -> ScopeSeed {
    let marker = format!("secret-vault-contract-{}", Uuid::new_v4());
    let owner = Uuid::new_v4();
    let admin = Uuid::new_v4();
    let member = Uuid::new_v4();
    let viewer = Uuid::new_v4();
    let foreign = Uuid::new_v4();
    let disabled = Uuid::new_v4();

    for (id, subject, status) in [
        (owner, "owner", "active"),
        (admin, "admin", "active"),
        (member, "member", "active"),
        (viewer, "viewer", "active"),
        (foreign, "foreign", "active"),
        (disabled, "disabled", "disabled"),
    ] {
        insert_user(kernel, id, &marker, subject, status).await;
    }

    let personal = Uuid::new_v4();
    kernel
        .create_project(personal, owner, &format!("personal-{personal}"), "personal")
        .await
        .expect("personal scope creates");

    let team = Uuid::new_v4();
    kernel
        .create_project(team, owner, &format!("team-{team}"), "team")
        .await
        .expect("team scope creates");
    add_member(kernel, team, admin, "admin").await;
    add_member(kernel, team, member, "member").await;
    add_member(kernel, team, viewer, "viewer").await;
    add_member(kernel, team, disabled, "member").await;

    ScopeSeed {
        marker,
        owner,
        admin,
        member,
        viewer,
        foreign,
        disabled,
        personal,
        team,
    }
}

async fn session_token(surface: &Surface, user_id: Uuid) -> String {
    web_session::create(
        surface.kernel.pool(),
        user_id,
        surface.auth.config().session_ttl(),
    )
    .await
    .expect("web session creates")
    .1
}

struct Exchange {
    status: u16,
    body: Vec<u8>,
}

impl Exchange {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("JSON response body parses")
    }
}

async fn exchange(
    surface: &Surface,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> Exchange {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nhost: 127.0.0.1:{}\r\naccept: application/json\r\n\
         cookie: {COOKIE_NAME}={token}\r\n",
        surface.port
    );
    if method != "GET" {
        request.push_str(&format!(
            "origin: {}\r\ncontent-type: application/json\r\n\
             x-voie-intent: mutate\r\ncontent-length: {}\r\n",
            surface.origin,
            payload.len()
        ));
    }
    request.push_str("connection: close\r\n\r\n");
    request.push_str(&payload);

    let mut stream = TcpStream::connect(("127.0.0.1", surface.port))
        .await
        .expect("API listener accepts connections");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("request writes");
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
        .await
        .expect("response completes inside 10s")
        .expect("response reads");
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n").expect("HTTP header terminator");
    let status = head
        .lines()
        .next()
        .expect("HTTP status line")
        .split_whitespace()
        .nth(1)
        .expect("HTTP status code")
        .parse()
        .expect("HTTP status is numeric");
    Exchange {
        status,
        body: body.as_bytes().to_vec(),
    }
}

fn assert_no_value(value: &Value, raw_values: &[&str]) {
    let serialized = serde_json::to_string(value).expect("JSON serializes");
    for raw in raw_values {
        assert!(
            !serialized.contains(raw),
            "secret value leaked in {serialized}"
        );
    }

    fn walk(value: &Value) {
        match value {
            Value::Object(object) => {
                assert!(
                    !object.contains_key("value"),
                    "secret response contains a value field: {object:?}"
                );
                for child in object.values() {
                    walk(child);
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    walk(value);
}

fn assert_metadata(
    response: &Value,
    scope_id: Uuid,
    actor: Uuid,
    version: i64,
    can_write: bool,
    raw_values: &[&str],
) -> Uuid {
    assert_no_value(response, raw_values);
    let secret = response
        .get("secret")
        .and_then(Value::as_object)
        .expect("secret metadata envelope");
    for field in [
        "id",
        "scopeId",
        "name",
        "version",
        "createdBy",
        "createdAt",
        "updatedAt",
        "canWrite",
    ] {
        assert!(secret.contains_key(field), "metadata is missing {field}");
    }
    assert_eq!(secret.get("scopeId"), Some(&json!(scope_id)));
    assert_eq!(secret.get("createdBy"), Some(&json!(actor)));
    assert_eq!(secret.get("version"), Some(&json!(version)));
    assert_eq!(secret.get("canWrite"), Some(&json!(can_write)));
    Uuid::parse_str(secret.get("id").and_then(Value::as_str).expect("secret id"))
        .expect("secret id is a UUID")
}

fn assert_list_metadata(response: &Value, can_write: bool, raw_values: &[&str]) -> Vec<Uuid> {
    assert_no_value(response, raw_values);
    assert_eq!(response.get("canWrite"), Some(&json!(can_write)));
    response
        .get("secrets")
        .and_then(Value::as_array)
        .expect("secret list array")
        .iter()
        .map(|secret| {
            let object = secret.as_object().expect("secret list metadata object");
            for field in [
                "id",
                "scopeId",
                "name",
                "version",
                "createdBy",
                "createdAt",
                "updatedAt",
                "canWrite",
            ] {
                assert!(object.contains_key(field), "metadata is missing {field}");
            }
            assert_eq!(object.get("canWrite"), Some(&json!(can_write)));
            Uuid::parse_str(
                object
                    .get("id")
                    .and_then(Value::as_str)
                    .expect("listed secret id"),
            )
            .expect("listed secret id is a UUID")
        })
        .collect()
}

async fn count_for_scope(kernel: &Kernel, sql: &str, scope_id: Uuid) -> i64 {
    sqlx::query_scalar(sql)
        .bind(scope_id)
        .fetch_one(kernel.pool())
        .await
        .expect("scoped count reads")
}

struct SideEffectCounts {
    members: i64,
    workspaces: i64,
    agents: i64,
    sessions: i64,
    runs: i64,
    exec_calls: i64,
    users_with_marker: i64,
}

async fn side_effect_counts(kernel: &Kernel, scope_id: Uuid, marker: &str) -> SideEffectCounts {
    SideEffectCounts {
        members: count_for_scope(
            kernel,
            "select count(*) from project_members where project_id = $1",
            scope_id,
        )
        .await,
        workspaces: count_for_scope(
            kernel,
            "select count(*) from workspaces where project_id = $1",
            scope_id,
        )
        .await,
        agents: count_for_scope(
            kernel,
            "select count(*) from agents where project_id = $1",
            scope_id,
        )
        .await,
        sessions: count_for_scope(
            kernel,
            "select count(*) from sessions where project_id = $1",
            scope_id,
        )
        .await,
        runs: count_for_scope(
            kernel,
            "select count(*) from runs where session_id in (
                 select id from sessions where project_id = $1
             )",
            scope_id,
        )
        .await,
        exec_calls: count_for_scope(
            kernel,
            "select count(*) from exec_calls where workspace_id in (
                 select id from workspaces where project_id = $1
             )",
            scope_id,
        )
        .await,
        users_with_marker: sqlx::query_scalar("select count(*) from users where issuer = $1")
            .bind(marker)
            .fetch_one(kernel.pool())
            .await
            .expect("marker user count reads"),
    }
}

#[tokio::test]
async fn scoped_secret_permissions_and_metadata_contract() {
    let surface = http_surface().await;
    let seed = seed_scopes(&surface.kernel).await;
    let owner = session_token(&surface, seed.owner).await;
    let admin = session_token(&surface, seed.admin).await;
    let member = session_token(&surface, seed.member).await;
    let viewer = session_token(&surface, seed.viewer).await;
    let foreign = session_token(&surface, seed.foreign).await;

    let personal_value = format!("personal-value-{}", Uuid::new_v4());
    let personal_create = exchange(
        &surface,
        "POST",
        &format!("/api/scopes/{}/secrets", seed.personal),
        &owner,
        Some(json!({"name": "personal-key", "value": personal_value})),
    )
    .await;
    assert_eq!(personal_create.status, 200, "personal owner can write");
    let personal_json = personal_create.json();
    let personal_id = assert_metadata(
        &personal_json,
        seed.personal,
        seed.owner,
        1,
        true,
        &[personal_value.as_str()],
    );

    let personal_list = exchange(
        &surface,
        "GET",
        &format!("/api/scopes/{}/secrets", seed.personal),
        &owner,
        None,
    )
    .await;
    assert_eq!(personal_list.status, 200);
    let personal_list_json = personal_list.json();
    assert_list_metadata(&personal_list_json, true, &[personal_value.as_str()]);

    let foreign_personal = exchange(
        &surface,
        "GET",
        &format!("/api/scopes/{}/secrets", seed.personal),
        &foreign,
        None,
    )
    .await;
    assert_eq!(
        foreign_personal.status, 403,
        "foreign user cannot read a personal scope"
    );

    let owner_value = format!("owner-value-{}", Uuid::new_v4());
    let owner_create = exchange(
        &surface,
        "POST",
        &format!("/api/scopes/{}/secrets", seed.team),
        &owner,
        Some(json!({"name": "owner-key", "value": owner_value})),
    )
    .await;
    assert_eq!(owner_create.status, 200, "team owner can write");
    let owner_json = owner_create.json();
    let owner_secret_id = assert_metadata(
        &owner_json,
        seed.team,
        seed.owner,
        1,
        true,
        &[owner_value.as_str()],
    );

    let admin_value = format!("admin-value-{}", Uuid::new_v4());
    let admin_create = exchange(
        &surface,
        "POST",
        &format!("/api/scopes/{}/secrets", seed.team),
        &admin,
        Some(json!({"name": "admin-key", "value": admin_value})),
    )
    .await;
    assert_eq!(admin_create.status, 200, "team admin can write");
    let admin_json = admin_create.json();
    assert_metadata(
        &admin_json,
        seed.team,
        seed.admin,
        1,
        true,
        &[admin_value.as_str()],
    );

    // Members may operate a scoped secret, but viewers are metadata-only.
    let member_value = format!("member-value-{}", Uuid::new_v4());
    let member_create = exchange(
        &surface,
        "POST",
        &format!("/api/scopes/{}/secrets", seed.team),
        &member,
        Some(json!({"name": "member-key", "value": member_value})),
    )
    .await;
    assert_eq!(member_create.status, 200, "team member can write");
    let member_json = member_create.json();
    assert_metadata(
        &member_json,
        seed.team,
        seed.member,
        1,
        true,
        &[member_value.as_str()],
    );

    let team_values = [
        owner_value.as_str(),
        admin_value.as_str(),
        member_value.as_str(),
    ];
    for (token, expected_write, label) in [
        (&owner, true, "owner"),
        (&admin, true, "admin"),
        (&member, true, "member"),
        (&viewer, false, "viewer"),
    ] {
        let listed = exchange(
            &surface,
            "GET",
            &format!("/api/scopes/{}/secrets", seed.team),
            token,
            None,
        )
        .await;
        assert_eq!(listed.status, 200, "{label} can read scoped metadata");
        let listed_json = listed.json();
        let ids = assert_list_metadata(&listed_json, expected_write, &team_values);
        assert!(
            ids.contains(&owner_secret_id),
            "{label} sees the owner secret metadata"
        );
    }

    let viewer_update = exchange(
        &surface,
        "PUT",
        &format!("/api/secrets/{owner_secret_id}"),
        &viewer,
        Some(json!({"value": format!("viewer-update-{}", Uuid::new_v4())})),
    )
    .await;
    assert_eq!(
        viewer_update.status, 403,
        "viewer cannot replace a secret value"
    );
    let viewer_rotate = exchange(
        &surface,
        "POST",
        &format!("/api/secrets/{owner_secret_id}/rotate"),
        &viewer,
        Some(json!({"value": format!("viewer-rotate-{}", Uuid::new_v4())})),
    )
    .await;
    assert_eq!(
        viewer_rotate.status, 403,
        "viewer cannot rotate a secret value"
    );
    let viewer_delete = exchange(
        &surface,
        "DELETE",
        &format!("/api/secrets/{owner_secret_id}"),
        &viewer,
        None,
    )
    .await;
    assert_eq!(viewer_delete.status, 403, "viewer cannot delete a secret");

    let foreign_create = exchange(
        &surface,
        "POST",
        &format!("/api/scopes/{}/secrets", seed.team),
        &foreign,
        Some(json!({
            "name": "foreign-key",
            "value": format!("foreign-value-{}", Uuid::new_v4()),
        })),
    )
    .await;
    assert_eq!(
        foreign_create.status, 403,
        "foreign user cannot create a secret"
    );
    let foreign_audit = exchange(
        &surface,
        "GET",
        &format!("/api/secrets/{owner_secret_id}/audit"),
        &foreign,
        None,
    )
    .await;
    assert_eq!(
        foreign_audit.status, 403,
        "foreign user cannot read secret audit metadata"
    );
    let foreign_update = exchange(
        &surface,
        "PUT",
        &format!("/api/secrets/{owner_secret_id}"),
        &foreign,
        Some(json!({"value": "foreign-update"})),
    )
    .await;
    assert_eq!(
        foreign_update.status, 403,
        "foreign user cannot replace a secret"
    );

    let disabled_token = session_token(&surface, seed.disabled).await;
    let disabled = exchange(
        &surface,
        "GET",
        &format!("/api/scopes/{}/secrets", seed.team),
        &disabled_token,
        None,
    )
    .await;
    assert_eq!(
        disabled.status, 401,
        "disabled users cannot read secret metadata"
    );
    assert!(
        web_session::lookup(
            surface.kernel.pool(),
            &disabled_token,
            surface.auth.config().session_ttl(),
        )
        .await
        .expect("disabled session lookup")
        .is_none(),
        "disabled session is revoked on first use"
    );

    // A fresh disabled session is refused on mutation as well; the first
    // refusal above revoked only the token used for the metadata read.
    let disabled_create_token = session_token(&surface, seed.disabled).await;
    let disabled_create = exchange(
        &surface,
        "POST",
        &format!("/api/scopes/{}/secrets", seed.team),
        &disabled_create_token,
        Some(json!({
            "name": "disabled-key",
            "value": format!("disabled-value-{}", Uuid::new_v4()),
        })),
    )
    .await;
    assert_eq!(
        disabled_create.status, 401,
        "disabled users cannot write secrets"
    );
    assert!(
        web_session::lookup(
            surface.kernel.pool(),
            &disabled_create_token,
            surface.auth.config().session_ttl(),
        )
        .await
        .expect("disabled mutation session lookup")
        .is_none(),
        "disabled mutation session is revoked"
    );

    // The personal id is retained above to ensure the scope kind does not
    // broaden authorization, while the team matrix covers every role.
    assert_ne!(personal_id, owner_secret_id);
    surface.stop().await;
}

#[tokio::test]
async fn secret_lifecycle_audit_storage_and_no_implicit_injection_contract() {
    let surface = http_surface().await;
    let seed = seed_scopes(&surface.kernel).await;
    let owner_token = session_token(&surface, seed.owner).await;

    let columns: Vec<String> = sqlx::query_scalar(
        "select column_name from information_schema.columns \
         where table_schema = 'public' and table_name = 'user_secrets' \
         order by ordinal_position",
    )
    .fetch_all(surface.kernel.pool())
    .await
    .expect("user secret schema introspects");
    assert!(
        columns.contains(&"kv_name".to_owned()),
        "secret row stores a KV reference"
    );
    assert!(
        columns.contains(&"version".to_owned()),
        "secret row stores a version"
    );
    assert!(
        !columns.contains(&"value".to_owned()),
        "plaintext value is not a vault column"
    );

    let before = side_effect_counts(&surface.kernel, seed.team, &seed.marker).await;
    let audit_seq_before: i64 =
        sqlx::query_scalar("select coalesce(max(seq), 0) from audit_events")
            .fetch_one(surface.kernel.pool())
            .await
            .expect("audit sequence reads");

    let first_value = format!("first-value-{}", Uuid::new_v4());
    let create = exchange(
        &surface,
        "POST",
        &format!("/api/scopes/{}/secrets", seed.team),
        &owner_token,
        Some(json!({"name": "lifecycle-key", "value": first_value})),
    )
    .await;
    assert_eq!(create.status, 200);
    let create_json = create.json();
    let secret_id = assert_metadata(
        &create_json,
        seed.team,
        seed.owner,
        1,
        true,
        &[first_value.as_str()],
    );

    let kv_name: String = sqlx::query_scalar("select kv_name from user_secrets where id = $1")
        .bind(secret_id)
        .fetch_one(surface.kernel.pool())
        .await
        .expect("KV reference reads");
    assert!(
        kv_name.starts_with("us-"),
        "secret row points to server/KV material"
    );
    assert!(
        !kv_name.contains(&first_value),
        "KV reference contains no secret value"
    );

    let second_value = format!("second-value-{}", Uuid::new_v4());
    // PUT deliberately carries a JSON body: the API request gate must read
    // PUT exactly as it reads POST/PATCH, rather than treating it as empty.
    let update = exchange(
        &surface,
        "PUT",
        &format!("/api/secrets/{secret_id}"),
        &owner_token,
        Some(json!({"value": second_value})),
    )
    .await;
    assert_eq!(update.status, 200, "owner can replace a secret value");
    let update_json = update.json();
    assert_metadata(
        &update_json,
        seed.team,
        seed.owner,
        2,
        true,
        &[first_value.as_str(), second_value.as_str()],
    );

    let third_value = format!("third-value-{}", Uuid::new_v4());
    let rotate = exchange(
        &surface,
        "POST",
        &format!("/api/secrets/{secret_id}/rotate"),
        &owner_token,
        Some(json!({"value": third_value})),
    )
    .await;
    assert_eq!(rotate.status, 200, "owner can rotate a secret value");
    let rotate_json = rotate.json();
    assert_metadata(
        &rotate_json,
        seed.team,
        seed.owner,
        3,
        true,
        &[
            first_value.as_str(),
            second_value.as_str(),
            third_value.as_str(),
        ],
    );

    let listed = exchange(
        &surface,
        "GET",
        &format!("/api/scopes/{}/secrets", seed.team),
        &owner_token,
        None,
    )
    .await;
    assert_eq!(listed.status, 200);
    let listed_json = listed.json();
    assert_list_metadata(
        &listed_json,
        true,
        &[
            first_value.as_str(),
            second_value.as_str(),
            third_value.as_str(),
        ],
    );

    // There is deliberately no single-secret GET endpoint: no browser path
    // can turn a resource read into a value read. Scope listing is the only
    // metadata collection read.
    let direct_get = exchange(
        &surface,
        "GET",
        &format!("/api/secrets/{secret_id}"),
        &owner_token,
        None,
    )
    .await;
    assert_eq!(direct_get.status, 404, "single-secret GET is not exposed");
    assert_no_value(
        &direct_get.json(),
        &[
            first_value.as_str(),
            second_value.as_str(),
            third_value.as_str(),
        ],
    );

    let audit = exchange(
        &surface,
        "GET",
        &format!("/api/secrets/{secret_id}/audit"),
        &owner_token,
        None,
    )
    .await;
    assert_eq!(audit.status, 200);
    let audit_json = audit.json();
    assert_no_value(
        &audit_json,
        &[
            first_value.as_str(),
            second_value.as_str(),
            third_value.as_str(),
        ],
    );
    let events = audit_json
        .get("events")
        .and_then(Value::as_array)
        .expect("secret audit events array");
    assert_eq!(
        events.len(),
        3,
        "create/update/rotate each produce one event"
    );
    let actions = events
        .iter()
        .map(|event| event.get("action").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(
        actions,
        vec![Some("created"), Some("updated"), Some("rotated")],
        "secret audit actions preserve lifecycle order"
    );
    let versions = events
        .iter()
        .map(|event| event.get("version").and_then(Value::as_i64))
        .collect::<Vec<_>>();
    assert_eq!(
        versions,
        vec![Some(1), Some(2), Some(3)],
        "secret audit versions are monotonic"
    );
    let owner_text = seed.owner.to_string();
    for event in events {
        assert_eq!(
            event.get("actor").and_then(Value::as_str),
            Some(owner_text.as_str())
        );
        assert!(
            event.get("at").is_some(),
            "audit event carries its timestamp"
        );
    }

    // Canonical event and audit feeds are metadata-only too; a secret write
    // never becomes browser-readable event content.
    for path in ["/api/events", "/api/audit-events"] {
        let feed = exchange(&surface, "GET", path, &owner_token, None).await;
        assert_eq!(feed.status, 200, "{path} remains readable");
        let feed_json = feed.json();
        assert_no_value(
            &feed_json,
            &[
                first_value.as_str(),
                second_value.as_str(),
                third_value.as_str(),
            ],
        );
    }

    let deleted = exchange(
        &surface,
        "DELETE",
        &format!("/api/secrets/{secret_id}"),
        &owner_token,
        None,
    )
    .await;
    assert_eq!(deleted.status, 204, "delete answers with no JSON body");
    assert!(deleted.body.is_empty(), "delete response has no body");

    let after_delete_list = exchange(
        &surface,
        "GET",
        &format!("/api/scopes/{}/secrets", seed.team),
        &owner_token,
        None,
    )
    .await;
    assert_eq!(after_delete_list.status, 200);
    let after_delete_json = after_delete_list.json();
    let remaining = assert_list_metadata(
        &after_delete_json,
        true,
        &[
            first_value.as_str(),
            second_value.as_str(),
            third_value.as_str(),
        ],
    );
    assert!(
        !remaining.contains(&secret_id),
        "deleted secret leaves the metadata list"
    );

    let stored_rows: i64 = sqlx::query_scalar("select count(*) from user_secrets where id = $1")
        .bind(secret_id)
        .fetch_one(surface.kernel.pool())
        .await
        .expect("deleted secret row count reads");
    assert_eq!(stored_rows, 0, "delete removes the server-side secret row");

    let audit_rows = sqlx::query(
        "select kind, actor_user_id, resource_type, resource_id, outcome, \
                metadata::text as metadata_text, payload \
         from audit_events where resource_id = $1 order by seq",
    )
    .bind(secret_id)
    .fetch_all(surface.kernel.pool())
    .await
    .expect("secret audit rows read");
    assert_eq!(
        audit_rows.len(),
        4,
        "delete remains auditable after row removal"
    );
    let expected_kinds = [
        "secret.created",
        "secret.updated",
        "secret.rotated",
        "secret.deleted",
    ];
    let expected_versions = [1, 2, 3, 3];
    for ((row, expected_kind), expected_version) in audit_rows
        .iter()
        .zip(expected_kinds.iter().copied())
        .zip(expected_versions.iter().copied())
    {
        assert_eq!(row.get::<String, _>("kind"), expected_kind);
        assert_eq!(
            row.get::<Option<Uuid>, _>("actor_user_id"),
            Some(seed.owner)
        );
        assert_eq!(row.get::<String, _>("resource_type"), "secret");
        assert_eq!(row.get::<Option<Uuid>, _>("resource_id"), Some(secret_id));
        assert_eq!(row.get::<String, _>("outcome"), "ok");

        let metadata_text = row.get::<Option<String>, _>("metadata_text");
        let metadata = metadata_text
            .as_deref()
            .map(|text| serde_json::from_str::<Value>(text).expect("audit metadata is JSON"));
        let metadata = metadata.expect("secret audit metadata is present");
        assert_no_value(
            &metadata,
            &[
                first_value.as_str(),
                second_value.as_str(),
                third_value.as_str(),
            ],
        );
        let object = metadata.as_object().expect("audit metadata object");
        assert_eq!(
            object.len(),
            3,
            "audit metadata contains only scope/name/version"
        );
        assert!(object.contains_key("scopeId"));
        assert!(object.contains_key("name"));
        assert_eq!(object.get("version"), Some(&json!(expected_version)));

        let payload = row.get::<Option<String>, _>("payload");
        if let Some(payload) = payload {
            for value in [
                first_value.as_str(),
                second_value.as_str(),
                third_value.as_str(),
            ] {
                assert!(
                    !payload.contains(value),
                    "secret value leaked in audit payload"
                );
            }
        }
    }

    let after = side_effect_counts(&surface.kernel, seed.team, &seed.marker).await;
    assert_eq!(
        after.members, before.members,
        "secret lifecycle adds no guest membership"
    );
    assert_eq!(
        after.workspaces, before.workspaces,
        "secret lifecycle adds no workspace"
    );
    assert_eq!(
        after.agents, before.agents,
        "secret lifecycle adds no agent"
    );
    assert_eq!(
        after.sessions, before.sessions,
        "secret lifecycle adds no session"
    );
    assert_eq!(
        after.runs, before.runs,
        "secret lifecycle adds no activation run"
    );
    assert_eq!(
        after.exec_calls, before.exec_calls,
        "secret lifecycle adds no exec call"
    );
    assert_eq!(
        after.users_with_marker, before.users_with_marker,
        "secret lifecycle adds no guest user"
    );

    // The only rows after the snapshot that can reference this secret are the
    // four intended lifecycle audit events; no activation/guest event is
    // synthesized as a side effect.
    let kinds: Vec<String> = sqlx::query_scalar(
        "select kind from audit_events \
         where seq > $1 and resource_id = $2 order by seq",
    )
    .bind(audit_seq_before)
    .bind(secret_id)
    .fetch_all(surface.kernel.pool())
    .await
    .expect("secret audit kind rows read");
    assert_eq!(
        kinds,
        expected_kinds
            .iter()
            .map(|kind| (*kind).to_owned())
            .collect::<Vec<_>>()
    );

    surface.stop().await;
}
