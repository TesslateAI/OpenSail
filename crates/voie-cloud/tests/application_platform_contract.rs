//! Profile 1 Application platform contracts: Project stays the authorization
//! scope, Application is the deployable, slugs are unique, one Application per
//! Workspace, Release no-replay, exact promotion, preview host binding, and
//! production credentials never enter Application metadata.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use uuid::Uuid;
use voie_cloud::auth::{Auth, AuthConfig};
use voie_cloud::integration::Services;
use voie_cloud::web_session;
use voie_cloud::{Config, Kernel, serve_with_services};

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
            "/CN=voie-app-contract",
        ])
        .output()
        .expect("openssl is available");
    assert!(
        output.status.success(),
        "openssl creates fixture certificate"
    );
    std::fs::copy(&cert, &ca).expect("self-signed certificate is a usable test CA");
    (cert, key, ca)
}

struct HttpResponse {
    status: u16,
    headers: String,
    body: Vec<u8>,
}

impl HttpResponse {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
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
    let head_text = String::from_utf8_lossy(head);
    let status = head_text
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
        headers: head_text.into_owned(),
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

const PUBLIC_ORIGIN: &str = "https://console.test";

fn unique_slug(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4().simple())
}

async fn mutate(port: u16, method: &str, path: &str, token: &str, body: &str) -> HttpResponse {
    exchange(
        port,
        format!(
            "{method} {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\nconnection: close\r\norigin: {PUBLIC_ORIGIN}\r\nx-voie-intent: mutate\r\ncookie: voie_session={token}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        ),
    )
    .await
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

async fn spawn_server(
    label: &str,
    environment: &mut EnvironmentRestore,
) -> (TempDir, tokio::net::TcpListener, Arc<Kernel>) {
    let fixture = TempDir::new(label);
    let (cert, key, ca) = fabric_pem_fixture(&fixture);
    environment.set("VOIE_PUBLIC_ORIGIN", PUBLIC_ORIGIN);
    environment.set("VOIE_AZURE_BLOB_ACCOUNT", "app-test-account");
    environment.set(
        "VOIE_AZURE_BLOB_KEY",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    environment.set("VOIE_AZURE_BLOB_CONTAINER", "app-test-container");
    environment.set("VOIE_AZURE_BLOB_ENDPOINT", "http://127.0.0.1:1");
    environment.set("VOIE_MODEL_BASE_URL", "http://127.0.0.1:1");
    environment.set("VOIE_MODEL_NAME", "app-test-model");
    environment.set("VOIE_MODEL_API_KEY", "app-test-key");
    environment.set("VOIE_FABRIC_ENDPOINT", "https://127.0.0.1:1");
    environment.set("VOIE_PRODUCT_RUNTIME", "stub");
    environment.set("VOIE_FABRIC_CLIENT_CERT_PATH", &cert);
    environment.set("VOIE_FABRIC_CLIENT_KEY_PATH", &key);
    environment.set("VOIE_FABRIC_CA_CERT_PATH", &ca);
    environment.set("VOIE_USER_SECRETS_BACKEND", "memory");
    let kernel = Arc::new(
        Kernel::connect(&Config::database_url(
            std::env::var("VOIE_TEST_DATABASE_URL").expect("VOIE_TEST_DATABASE_URL"),
        ))
        .await
        .expect("PostgreSQL connection succeeds"),
    );
    kernel.migrate().await.expect("latest migration applies");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("API listener binds");
    (fixture, listener, kernel)
}

async fn insert_user(kernel: &Kernel, user_id: Uuid, username: &str) {
    sqlx::query(
        "insert into users (id, issuer, subject, username, display_name, email, platform_role, status) \
         values ($1, $2, $3, $4, $5, $6, 'user', 'active')",
    )
    .bind(user_id)
    .bind(format!("app-contract-{}", Uuid::new_v4()))
    .bind(username)
    .bind(format!("{username}-{user_id}"))
    .bind(username)
    .bind(format!("{username}@example.test"))
    .execute(kernel.pool())
    .await
    .expect("user inserts");
}

const SAMPLE_MANIFEST: &str = r#"
version = 1
[application]
runtime = "universal-v1"
[build]
command = ["sh", ".voie/build.sh"]
output = "dist"
[run]
command = ["node", "dist/server.js"]
port = 3000
health_path = "/healthz"
"#;

const POSTGRES_MANIFEST: &str = r#"
version = 1
[application]
runtime = "universal-v1"
[build]
command = ["sh", ".voie/build.sh"]
output = "dist"
[run]
command = ["node", "dist/server.js"]
port = 3000
health_path = "/healthz"
[database]
postgres = true
migration_command = ["python3", "server.py", "migrate"]
"#;

#[tokio::test]
async fn application_create_is_project_bound_and_slug_unique() {
    let _lock = ENV_LOCK.lock().expect("environment fixture lock");
    let mut environment = EnvironmentRestore::new();
    let (_fixture, listener, kernel) = spawn_server("app-create", &mut environment).await;
    let port = listener.local_addr().expect("addr").port();
    let owner = Uuid::new_v4();
    let stranger = Uuid::new_v4();
    insert_user(&kernel, owner, "owner").await;
    insert_user(&kernel, stranger, "stranger").await;
    let project = Uuid::new_v4();
    let other_project = Uuid::new_v4();
    let fabric = Uuid::new_v4();
    let workspace = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .unwrap();
    sqlx::query(
        "insert into projects (id, owner_user_id, name, kind) values ($1, $2, 'App', 'personal')",
    )
    .bind(project)
    .bind(owner)
    .execute(kernel.pool())
    .await
    .unwrap();
    sqlx::query(
        "insert into projects (id, owner_user_id, name, kind) values ($1, $2, 'Other', 'personal')",
    )
    .bind(other_project)
    .bind(stranger)
    .execute(kernel.pool())
    .await
    .unwrap();
    sqlx::query("insert into project_members (project_id, user_id, role) values ($1, $2, 'owner')")
        .bind(project)
        .bind(owner)
        .execute(kernel.pool())
        .await
        .unwrap();
    sqlx::query("insert into project_members (project_id, user_id, role) values ($1, $2, 'owner')")
        .bind(other_project)
        .bind(stranger)
        .execute(kernel.pool())
        .await
        .unwrap();
    sqlx::query(
        "insert into workspaces (id, fabric_id, project_id, state, exec_generation) values ($1, $2, $3, 'ready', 1)",
    )
    .bind(workspace)
    .bind(fabric)
    .bind(project)
    .execute(kernel.pool())
    .await
    .unwrap();
    let owner_token =
        web_session::create(kernel.pool(), owner, std::time::Duration::from_secs(3600))
            .await
            .expect("session")
            .1;
    let stranger_token = web_session::create(
        kernel.pool(),
        stranger,
        std::time::Duration::from_secs(3600),
    )
    .await
    .expect("session")
    .1;
    let auth = Arc::new(
        Auth::connect(AuthConfig::native(PUBLIC_ORIGIN), kernel.pool().clone())
            .await
            .expect("auth"),
    );
    let services = Services::from_env(kernel.pool().clone()).expect("services");
    let server = tokio::spawn(serve_with_services(
        listener,
        kernel.clone(),
        auth,
        services,
    ));

    let reserved = mutate(
        port,
        "POST",
        &format!("/api/projects/{project}/applications"),
        &owner_token,
        &format!(r#"{{"name":"Invoice","slug":"admin","workspace_id":"{workspace}"}}"#),
    )
    .await;
    assert_eq!(reserved.status, 400, "{}", reserved.text());

    let slug = unique_slug("invoice-demo");
    let created = mutate(
        port,
        "POST",
        &format!("/api/projects/{project}/applications"),
        &owner_token,
        &format!(r#"{{"name":"Invoice","slug":"{slug}","workspace_id":"{workspace}"}}"#),
    )
    .await;
    assert_eq!(created.status, 201, "{}", created.text());
    let body = created.json();
    let created_id = body["application"]["id"].as_str().unwrap().to_owned();
    assert_eq!(body["application"]["slug"], slug);
    assert_eq!(body["application"]["projectId"], project.to_string());
    assert_eq!(body["environments"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        body["environments"][0]["hostname"]
            .as_str()
            .unwrap()
            .contains(".dev.console.test")
            || body["environments"][1]["hostname"]
                .as_str()
                .unwrap()
                .contains(".dev.console.test"),
        true
    );

    let prod_env = body["environments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["kind"] == "prod")
        .expect("prod environment");
    let dev_env = body["environments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["kind"] == "dev")
        .expect("dev environment");
    let prod_env_id = prod_env["id"].as_str().unwrap();
    let dev_env_id = dev_env["id"].as_str().unwrap();
    let marker = format!("voie-p1-marker-{}", Uuid::new_v4().simple());
    let secret = mutate(
        port,
        "POST",
        &format!("/api/scopes/{project}/secrets"),
        &owner_token,
        &format!(r#"{{"name":"p1-marker","value":"{marker}"}}"#),
    )
    .await;
    assert_eq!(secret.status, 200, "{}", secret.text());
    assert!(
        !secret.text().contains(&marker),
        "secret create echoed material: {}",
        secret.text()
    );
    let secret_id = secret.json()["secret"]["id"].as_str().unwrap().to_owned();
    let refused = mutate(
        port,
        "PUT",
        &format!("/api/environments/{prod_env_id}/secret-bindings/P1_MARKER"),
        &owner_token,
        &format!(r#"{{"secret_id":"{secret_id}"}}"#),
    )
    .await;
    assert_eq!(refused.status, 409, "{}", refused.text());
    let approval_id = refused.json()["approvalId"].as_str().unwrap().to_owned();
    let accepted = mutate(
        port,
        "POST",
        &format!("/api/approvals/{approval_id}/accept"),
        &owner_token,
        "{}",
    )
    .await;
    assert_eq!(accepted.status, 200, "{}", accepted.text());
    let bound = mutate(
        port,
        "PUT",
        &format!("/api/environments/{prod_env_id}/secret-bindings/P1_MARKER"),
        &owner_token,
        &format!(r#"{{"secret_id":"{secret_id}","approval_id":"{approval_id}"}}"#),
    )
    .await;
    assert_eq!(bound.status, 200, "{}", bound.text());
    assert!(
        !bound.text().contains(&marker),
        "binding response contained material: {}",
        bound.text()
    );
    let prod_bindings = get(
        port,
        &format!("/api/environments/{prod_env_id}/secret-bindings"),
        &owner_token,
    )
    .await;
    assert_eq!(prod_bindings.status, 200, "{}", prod_bindings.text());
    assert_eq!(
        prod_bindings.json()["items"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(prod_bindings.json()["items"][0]["name"], "P1_MARKER");
    assert!(prod_bindings.json()["items"][0].get("value").is_none());
    assert!(
        !prod_bindings.text().contains(&marker),
        "prod binding list contained material: {}",
        prod_bindings.text()
    );
    let dev_bindings = get(
        port,
        &format!("/api/environments/{dev_env_id}/secret-bindings"),
        &owner_token,
    )
    .await;
    assert_eq!(dev_bindings.status, 200, "{}", dev_bindings.text());
    assert_eq!(
        dev_bindings.json()["items"].as_array().map(Vec::len),
        Some(0),
        "prod secret must not appear on the dev Environment: {}",
        dev_bindings.text()
    );

    let duplicate = mutate(
        port,
        "POST",
        &format!("/api/projects/{project}/applications"),
        &owner_token,
        &format!(r#"{{"name":"Other","slug":"{slug}","workspace_id":"{workspace}"}}"#),
    )
    .await;
    assert_eq!(duplicate.status, 201, "{}", duplicate.text());
    assert_eq!(
        duplicate.json()["application"]["id"],
        created_id,
        "same-slug retry on this Workspace must return the existing Application: {}",
        duplicate.text()
    );
    assert!(
        duplicate.json()["workspaceHandoff"].is_null(),
        "same-slug retry must not hand off a Workspace: {}",
        duplicate.text()
    );

    let other_workspace = Uuid::new_v4();
    sqlx::query(
        "insert into workspaces (id, fabric_id, project_id, state, exec_generation) values ($1, $2, $3, 'ready', 1)",
    )
    .bind(other_workspace)
    .bind(fabric)
    .bind(project)
    .execute(kernel.pool())
    .await
    .unwrap();
    let colliding = mutate(
        port,
        "POST",
        &format!("/api/projects/{project}/applications"),
        &owner_token,
        &format!(r#"{{"name":"Other","slug":"{slug}","workspace_id":"{other_workspace}"}}"#),
    )
    .await;
    assert_eq!(
        colliding.status,
        409,
        "slug must stay unique across Workspaces: {}",
        colliding.text()
    );

    let foreign = mutate(
        port,
        "POST",
        &format!("/api/projects/{other_project}/applications"),
        &stranger_token,
        &format!(
            r#"{{"name":"Steal","slug":"{}","workspace_id":"{workspace}"}}"#,
            unique_slug("steal-app")
        ),
    )
    .await;
    assert_eq!(foreign.status, 404, "{}", foreign.text());

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn release_no_replay_and_exact_promotion_share_one_hash() {
    let _lock = ENV_LOCK.lock().expect("environment fixture lock");
    let mut environment = EnvironmentRestore::new();
    let (_fixture, listener, kernel) = spawn_server("app-release", &mut environment).await;
    let port = listener.local_addr().expect("addr").port();
    let owner = Uuid::new_v4();
    insert_user(&kernel, owner, "rel-owner").await;
    let project = Uuid::new_v4();
    let fabric = Uuid::new_v4();
    let workspace = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("fab-{fabric}"))
        .execute(kernel.pool())
        .await
        .unwrap();
    sqlx::query(
        "insert into projects (id, owner_user_id, name, kind) values ($1, $2, 'Rel', 'personal')",
    )
    .bind(project)
    .bind(owner)
    .execute(kernel.pool())
    .await
    .unwrap();
    sqlx::query("insert into project_members (project_id, user_id, role) values ($1, $2, 'owner')")
        .bind(project)
        .bind(owner)
        .execute(kernel.pool())
        .await
        .unwrap();
    sqlx::query(
        "insert into workspaces (id, fabric_id, project_id, state, exec_generation) values ($1, $2, $3, 'ready', 4)",
    )
    .bind(workspace)
    .bind(fabric)
    .bind(project)
    .execute(kernel.pool())
    .await
    .unwrap();
    let token = web_session::create(kernel.pool(), owner, std::time::Duration::from_secs(3600))
        .await
        .expect("session")
        .1;
    let auth = Arc::new(
        Auth::connect(AuthConfig::native(PUBLIC_ORIGIN), kernel.pool().clone())
            .await
            .expect("auth"),
    );
    let services = Services::from_env(kernel.pool().clone()).expect("services");
    let server = tokio::spawn(serve_with_services(
        listener,
        kernel.clone(),
        auth,
        services,
    ));

    let created = mutate(
        port,
        "POST",
        &format!("/api/projects/{project}/applications"),
        &token,
        &format!(
            r#"{{"name":"Tracker","slug":"{}","workspace_id":"{workspace}"}}"#,
            unique_slug("task-tracker")
        ),
    )
    .await;
    assert_eq!(created.status, 201, "{}", created.text());
    let application_id = created.json()["application"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let intent = Uuid::new_v4();
    let escaped = SAMPLE_MANIFEST
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    let first = mutate(
        port,
        "POST",
        &format!("/api/applications/{application_id}/releases"),
        &token,
        &format!(
            r#"{{"build_intent_id":"{intent}","workspace_id":"{workspace}","source_exec_generation":4,"manifest":"{escaped}"}}"#
        ),
    )
    .await;
    assert_eq!(first.status, 202, "{}", first.text());
    let unknown = mutate(
        port,
        "POST",
        &format!("/api/applications/{application_id}/releases"),
        &token,
        &format!(
            r#"{{"build_intent_id":"{intent}","workspace_id":"{workspace}","source_exec_generation":4,"manifest":"{escaped}"}}"#
        ),
    )
    .await;
    assert_eq!(unknown.status, 409, "{}", unknown.text());
    assert!(unknown.text().contains("unknown"));

    let conflict_manifest = SAMPLE_MANIFEST.replace("dist", "other");
    let escaped_conflict = conflict_manifest
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    let conflict = mutate(
        port,
        "POST",
        &format!("/api/applications/{application_id}/releases"),
        &token,
        &format!(
            r#"{{"build_intent_id":"{intent}","workspace_id":"{workspace}","source_exec_generation":4,"manifest":"{escaped_conflict}"}}"#
        ),
    )
    .await;
    assert_eq!(conflict.status, 409, "{}", conflict.text());

    sqlx::query(
        "update application_releases set state = 'ready', artifact_hash = $2, artifact_bytes = 12, artifact_key = 'k' \
         where build_intent_id = $1",
    )
    .bind(intent)
    .bind([7u8; 32].as_slice())
    .execute(kernel.pool())
    .await
    .unwrap();
    let release_id: Uuid =
        sqlx::query_scalar("select id from application_releases where build_intent_id = $1")
            .bind(intent)
            .fetch_one(kernel.pool())
            .await
            .unwrap();
    let environments = get(
        port,
        &format!("/api/applications/{application_id}/environments"),
        &token,
    )
    .await;
    let items = environments.json()["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let prod = items
        .iter()
        .find(|item| item["kind"] == "prod")
        .expect("prod env");
    let prod_id = prod["id"].as_str().unwrap();
    let deploy_intent = Uuid::new_v4();
    let publish = mutate(
        port,
        "POST",
        &format!("/api/environments/{prod_id}/deployments"),
        &token,
        &format!(r#"{{"release_id":"{release_id}","deployment_intent_id":"{deploy_intent}"}}"#),
    )
    .await;
    assert_eq!(
        publish.status,
        409,
        "production requires approval: {}",
        publish.text()
    );
    let approval_id = publish.json()["approvalId"].as_str().unwrap().to_owned();
    let accepted = mutate(
        port,
        "POST",
        &format!("/api/approvals/{approval_id}/accept"),
        &token,
        "{}",
    )
    .await;
    assert_eq!(accepted.status, 200, "{}", accepted.text());
    let second_intent = Uuid::new_v4();
    let approved = mutate(
        port,
        "POST",
        &format!("/api/environments/{prod_id}/deployments"),
        &token,
        &format!(
            r#"{{"release_id":"{release_id}","deployment_intent_id":"{second_intent}","approval_id":"{approval_id}"}}"#
        ),
    )
    .await;
    assert_eq!(approved.status, 202, "{}", approved.text());
    assert_eq!(
        approved.json()["deployment"]["releaseId"],
        release_id.to_string()
    );

    let dev = items
        .iter()
        .find(|item| item["kind"] == "dev")
        .expect("dev env");
    let dev_id = dev["id"].as_str().unwrap();
    let first_dev = mutate(
        port,
        "POST",
        &format!("/api/environments/{dev_id}/deployments"),
        &token,
        &format!(
            r#"{{"release_id":"{release_id}","deployment_intent_id":"{}"}}"#,
            Uuid::new_v4()
        ),
    )
    .await;
    assert_eq!(first_dev.status, 202, "{}", first_dev.text());
    let first_dev_id = Uuid::parse_str(first_dev.json()["deploymentId"].as_str().unwrap()).unwrap();
    sqlx::query(
        "update application_deployments set state = 'active', active_at = now() where id = $1",
    )
    .bind(first_dev_id)
    .execute(kernel.pool())
    .await
    .unwrap();
    sqlx::query(
        "update application_environments set active_deployment_id = $1, revision = revision + 1 where id = $2::uuid",
    )
    .bind(first_dev_id)
    .bind(dev_id)
    .execute(kernel.pool())
    .await
    .unwrap();

    let second_release_id = Uuid::new_v4();
    sqlx::query(
        "insert into application_releases (
            id, application_id, build_intent_id, request_hash, source_workspace_id,
            source_exec_generation, runtime_profile, manifest, manifest_hash,
            artifact_key, artifact_hash, artifact_bytes, state, created_by_user_id
         )
         select $1, application_id, $2, $3, source_workspace_id, source_exec_generation,
                runtime_profile, manifest, manifest_hash, artifact_key, $4, artifact_bytes,
                'ready', created_by_user_id
         from application_releases where id = $5",
    )
    .bind(second_release_id)
    .bind(Uuid::new_v4())
    .bind([8u8; 32].as_slice())
    .bind([9u8; 32].as_slice())
    .bind(release_id)
    .execute(kernel.pool())
    .await
    .unwrap();
    let second_dev = mutate(
        port,
        "POST",
        &format!("/api/environments/{dev_id}/deployments"),
        &token,
        &format!(
            r#"{{"release_id":"{second_release_id}","deployment_intent_id":"{}"}}"#,
            Uuid::new_v4()
        ),
    )
    .await;
    assert_eq!(second_dev.status, 202, "{}", second_dev.text());
    let second_dev_id =
        Uuid::parse_str(second_dev.json()["deploymentId"].as_str().unwrap()).unwrap();
    sqlx::query(
        "update application_deployments set state = 'superseded', terminal_at = now() where id = $1",
    )
    .bind(first_dev_id)
    .execute(kernel.pool())
    .await
    .unwrap();
    sqlx::query(
        "update application_deployments set state = 'active', active_at = now() where id = $1",
    )
    .bind(second_dev_id)
    .execute(kernel.pool())
    .await
    .unwrap();
    sqlx::query(
        "update application_environments set active_deployment_id = $1, revision = revision + 1 where id = $2::uuid",
    )
    .bind(second_dev_id)
    .bind(dev_id)
    .execute(kernel.pool())
    .await
    .unwrap();
    let blocked = mutate(
        port,
        "POST",
        &format!("/api/environments/{dev_id}/deployments"),
        &token,
        &format!(
            r#"{{"release_id":"{release_id}","deployment_intent_id":"{}"}}"#,
            Uuid::new_v4()
        ),
    )
    .await;
    assert_eq!(
        blocked.status, 429,
        "superseded Deployments still own Fabric resources and consume quota: {}",
        blocked.text()
    );
    sqlx::query(
        "update application_deployments set state = 'stopped', terminal_at = now() where id = $1",
    )
    .bind(first_dev_id)
    .execute(kernel.pool())
    .await
    .unwrap();
    let rolled = mutate(
        port,
        "POST",
        &format!("/api/deployments/{second_dev_id}/rollback"),
        &token,
        &format!(r#"{{"deployment_intent_id":"{}"}}"#, Uuid::new_v4()),
    )
    .await;
    assert_eq!(
        rolled.status,
        202,
        "rollback of a superseded Release must insert a new Deployment: {}",
        rolled.text()
    );
    assert_eq!(
        rolled.json()["deployment"]["releaseId"],
        release_id.to_string()
    );

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn preview_cookie_is_host_only_and_public_bypasses() {
    let _lock = ENV_LOCK.lock().expect("environment fixture lock");
    let mut environment = EnvironmentRestore::new();
    let (_fixture, listener, kernel) = spawn_server("app-preview", &mut environment).await;
    let port = listener.local_addr().expect("addr").port();
    let owner = Uuid::new_v4();
    insert_user(&kernel, owner, "prev-owner").await;
    let project = Uuid::new_v4();
    let fabric = Uuid::new_v4();
    let workspace = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("fab-{fabric}"))
        .execute(kernel.pool())
        .await
        .unwrap();
    sqlx::query(
        "insert into projects (id, owner_user_id, name, kind) values ($1, $2, 'Prev', 'personal')",
    )
    .bind(project)
    .bind(owner)
    .execute(kernel.pool())
    .await
    .unwrap();
    sqlx::query("insert into project_members (project_id, user_id, role) values ($1, $2, 'owner')")
        .bind(project)
        .bind(owner)
        .execute(kernel.pool())
        .await
        .unwrap();
    sqlx::query(
        "insert into workspaces (id, fabric_id, project_id, state, exec_generation) values ($1, $2, $3, 'ready', 1)",
    )
    .bind(workspace)
    .bind(fabric)
    .bind(project)
    .execute(kernel.pool())
    .await
    .unwrap();
    let token = web_session::create(kernel.pool(), owner, std::time::Duration::from_secs(3600))
        .await
        .expect("session")
        .1;
    let auth = Arc::new(
        Auth::connect(AuthConfig::native(PUBLIC_ORIGIN), kernel.pool().clone())
            .await
            .expect("auth"),
    );
    let services = Services::from_env(kernel.pool().clone()).expect("services");
    let server = tokio::spawn(serve_with_services(
        listener,
        kernel.clone(),
        auth,
        services,
    ));
    let created = mutate(
        port,
        "POST",
        &format!("/api/projects/{project}/applications"),
        &token,
        &format!(
            r#"{{"name":"Preview","slug":"{}","workspace_id":"{workspace}"}}"#,
            unique_slug("acme-portal")
        ),
    )
    .await;
    assert_eq!(created.status, 201, "{}", created.text());
    let application_id = created.json()["application"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let environments = get(
        port,
        &format!("/api/applications/{application_id}/environments"),
        &token,
    )
    .await;
    let items = environments.json()["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let dev = items
        .iter()
        .find(|item| item["kind"] == "dev")
        .expect("dev");
    let dev_id = dev["id"].as_str().unwrap();
    let hostname = dev["hostname"].as_str().unwrap();
    let login = get(
        port,
        &format!("/api/preview/login?applicationId={application_id}&environmentId={dev_id}"),
        &token,
    )
    .await;
    assert_eq!(login.status, 200, "{}", login.text());
    let redirect = login.json()["redirect"].as_str().unwrap().to_owned();
    assert!(redirect.contains(hostname));
    assert!(redirect.contains("/.voie/auth/callback?code="));
    let code = redirect.split("code=").nth(1).unwrap();
    let callback = exchange(
        port,
        format!(
            "GET /.voie/auth/callback?code={code} HTTP/1.1\r\nhost: {hostname}\r\nconnection: close\r\n\r\n"
        ),
    )
    .await;
    assert_eq!(callback.status, 302, "{}", callback.text());
    assert!(
        callback.headers.contains("__Host-voie-preview="),
        "preview cookie is host-only: {}",
        callback.headers
    );
    assert!(callback.headers.contains("Path=/"));
    assert!(!callback.headers.to_ascii_lowercase().contains("domain="));
    assert!(
        callback
            .headers
            .contains(&format!("location: https://{hostname}/"))
            || callback
                .headers
                .contains(&format!("Location: https://{hostname}/")),
        "callback must send the browser to the Application host: {}",
        callback.headers
    );
    let preview_cookie = callback
        .headers
        .lines()
        .find_map(|line| {
            let lower = line.to_ascii_lowercase();
            if !lower.starts_with("set-cookie:") {
                return None;
            }
            line.split_once(':')
                .map(|(_, value)| value.trim())?
                .split(';')
                .next()
                .map(str::trim)
                .filter(|part| part.starts_with("__Host-voie-preview="))
                .map(str::to_owned)
        })
        .expect("callback Set-Cookie carries __Host-voie-preview");
    let authorized_get = exchange(
        port,
        format!("GET /internal/preview/authorize HTTP/1.1\r\nhost: {hostname}\r\ncookie: {preview_cookie}\r\nconnection: close\r\n\r\n"),
    )
    .await;
    assert_eq!(
        authorized_get.status,
        200,
        "Caddy GET forward_auth with the exact-host cookie must authorize: {}",
        authorized_get.text()
    );

    let unauthorized = exchange(
        port,
        format!("POST /internal/preview/authorize HTTP/1.1\r\nhost: {hostname}\r\nconnection: close\r\ncontent-length: 0\r\n\r\n"),
    )
    .await;
    assert_eq!(unauthorized.status, 401, "{}", unauthorized.text());
    let unauthorized_get = exchange(
        port,
        format!("GET /internal/preview/authorize HTTP/1.1\r\nhost: {hostname}\r\nconnection: close\r\n\r\n"),
    )
    .await;
    assert_eq!(
        unauthorized_get.status,
        401,
        "Caddy forward_auth uses the incoming GET: {}",
        unauthorized_get.text()
    );

    let prod = items
        .iter()
        .find(|item| item["kind"] == "prod")
        .expect("prod");
    assert_eq!(prod["visibility"], "public");
    let prod_hostname = prod["hostname"].as_str().unwrap();
    let public_get = exchange(
        port,
        format!("GET /internal/preview/authorize HTTP/1.1\r\nhost: {prod_hostname}\r\nconnection: close\r\n\r\n"),
    )
    .await;
    assert_eq!(
        public_get.status,
        200,
        "public prod GET authorize must pass Caddy forward_auth without a cookie: {}",
        public_get.text()
    );
    let public_get_port = exchange(
        port,
        format!("GET /internal/preview/authorize HTTP/1.1\r\nhost: {prod_hostname}:443\r\nconnection: close\r\n\r\n"),
    )
    .await;
    assert_eq!(
        public_get_port.status,
        200,
        "Host with :443 must still authorize public prod: {}",
        public_get_port.text()
    );

    server.abort();
    let _ = server.await;
}

#[test]
fn voie_toml_rejects_infrastructure_fields() {
    assert!(voie_cloud::applications::Manifest::parse(SAMPLE_MANIFEST).is_ok());
    assert!(voie_cloud::applications::Manifest::parse("version = 1\nimage = \"evil\"\n").is_err());
}

#[tokio::test]
async fn unhealthy_candidate_cannot_cut_over_and_deletion_stops_deployments() {
    let _lock = ENV_LOCK.lock().expect("environment fixture lock");
    let mut environment = EnvironmentRestore::new();
    let (_fixture, listener, kernel) = spawn_server("app-cutover", &mut environment).await;
    let port = listener.local_addr().expect("addr").port();
    let owner = Uuid::new_v4();
    insert_user(&kernel, owner, "cut-owner").await;
    let project = Uuid::new_v4();
    let fabric = Uuid::new_v4();
    let workspace = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("fab-{fabric}"))
        .execute(kernel.pool())
        .await
        .unwrap();
    sqlx::query(
        "insert into projects (id, owner_user_id, name, kind) values ($1, $2, 'Cut', 'personal')",
    )
    .bind(project)
    .bind(owner)
    .execute(kernel.pool())
    .await
    .unwrap();
    sqlx::query("insert into project_members (project_id, user_id, role) values ($1, $2, 'owner')")
        .bind(project)
        .bind(owner)
        .execute(kernel.pool())
        .await
        .unwrap();
    sqlx::query(
        "insert into workspaces (id, fabric_id, project_id, state, exec_generation) values ($1, $2, $3, 'ready', 1)",
    )
    .bind(workspace)
    .bind(fabric)
    .bind(project)
    .execute(kernel.pool())
    .await
    .unwrap();
    let token = web_session::create(kernel.pool(), owner, std::time::Duration::from_secs(3600))
        .await
        .expect("session")
        .1;
    let auth = Arc::new(
        Auth::connect(AuthConfig::native(PUBLIC_ORIGIN), kernel.pool().clone())
            .await
            .expect("auth"),
    );
    let services = Services::from_env(kernel.pool().clone()).expect("services");
    let server = tokio::spawn(serve_with_services(
        listener,
        kernel.clone(),
        auth,
        services,
    ));
    let created = mutate(
        port,
        "POST",
        &format!("/api/projects/{project}/applications"),
        &token,
        &format!(
            r#"{{"name":"Tracker","slug":"{}","workspace_id":"{workspace}"}}"#,
            unique_slug("cutover-app")
        ),
    )
    .await;
    assert_eq!(created.status, 201, "{}", created.text());
    let application_id =
        Uuid::parse_str(created.json()["application"]["id"].as_str().unwrap()).unwrap();
    let intent = Uuid::new_v4();
    let escaped = SAMPLE_MANIFEST
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    let first = mutate(
        port,
        "POST",
        &format!("/api/applications/{application_id}/releases"),
        &token,
        &format!(
            r#"{{"build_intent_id":"{intent}","workspace_id":"{workspace}","source_exec_generation":1,"manifest":"{escaped}"}}"#
        ),
    )
    .await;
    assert_eq!(first.status, 202, "{}", first.text());
    sqlx::query(
        "update application_releases set state = 'ready', artifact_hash = $2, artifact_bytes = 12, artifact_key = 'k' \
         where build_intent_id = $1",
    )
    .bind(intent)
    .bind([9u8; 32].as_slice())
    .execute(kernel.pool())
    .await
    .unwrap();
    let release_id: Uuid =
        sqlx::query_scalar("select id from application_releases where build_intent_id = $1")
            .bind(intent)
            .fetch_one(kernel.pool())
            .await
            .unwrap();
    let environments = get(
        port,
        &format!("/api/applications/{application_id}/environments"),
        &token,
    )
    .await;
    let items = environments.json()["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let dev = items
        .iter()
        .find(|item| item["kind"] == "dev")
        .expect("dev");
    let dev_id = Uuid::parse_str(dev["id"].as_str().unwrap()).unwrap();
    let deploy_intent = Uuid::new_v4();
    let deployed = mutate(
        port,
        "POST",
        &format!("/api/environments/{dev_id}/deployments"),
        &token,
        &format!(r#"{{"release_id":"{release_id}","deployment_intent_id":"{deploy_intent}"}}"#),
    )
    .await;
    assert_eq!(deployed.status, 202, "{}", deployed.text());
    let deployment_id = Uuid::parse_str(deployed.json()["deploymentId"].as_str().unwrap()).unwrap();
    let polled = get(port, &format!("/api/deployments/{deployment_id}"), &token).await;
    assert_eq!(polled.status, 200, "{}", polled.text());
    assert_eq!(
        polled.json()["deployment"]["state"],
        "materializing",
        "status poll must not invent a healthy candidate: {}",
        polled.text()
    );
    let refused_cutover = mutate(
        port,
        "POST",
        &format!("/api/deployments/{deployment_id}/activate"),
        &token,
        "{}",
    )
    .await;
    assert_eq!(
        refused_cutover.status,
        409,
        "materializing candidate must not receive traffic: {}",
        refused_cutover.text()
    );
    let store = voie_cloud::deployments::DeploymentStore::new(kernel.pool().clone());
    assert!(
        store.activate(deployment_id).await.is_err(),
        "materializing candidate must not receive traffic"
    );
    store
        .mark_healthy(deployment_id)
        .await
        .expect("probes can mark healthy");
    let cut = mutate(
        port,
        "POST",
        &format!("/api/deployments/{deployment_id}/activate"),
        &token,
        "{}",
    )
    .await;
    assert_eq!(
        cut.status,
        409,
        "dummy Fabric transport must not invent an active Deployment: {}",
        cut.text()
    );
    let active = store
        .activate(deployment_id)
        .await
        .expect("healthy candidate can SQL-activate without Fabric");
    assert_eq!(active.state, "active");
    assert_eq!(active.release_id, release_id);
    let observed = get(port, &format!("/api/deployments/{deployment_id}"), &token).await;
    assert_eq!(observed.status, 200, "{}", observed.text());
    assert_eq!(observed.json()["deployment"]["state"], "active");

    let db_store = voie_cloud::databases::DatabaseStore::new(kernel.pool().clone());
    let operation = Uuid::new_v4();
    let hash = voie_cloud::applications::request_hash(&[b"create", dev_id.as_bytes()]);
    let database = db_store
        .create(owner, dev_id, fabric, operation, &hash)
        .await
        .expect("database create journals");
    let again = db_store
        .create(owner, dev_id, fabric, Uuid::new_v4(), &hash)
        .await
        .expect("database create is one row per Environment");
    assert_eq!(database.id, again.id);
    let listed = get(port, &format!("/api/databases/{}", database.id), &token).await;
    assert_eq!(listed.status, 200, "{}", listed.text());
    let db_text = listed.text();
    assert!(!db_text.contains("postgres://"), "{db_text}");
    assert!(
        !db_text.to_ascii_lowercase().contains("password"),
        "{db_text}"
    );
    assert!(!db_text.contains("DATABASE_URL"), "{db_text}");
    assert!(database.credential_secret_id.is_none());
    let platform_secret = Uuid::new_v4();
    db_store
        .attach_credential(database.id, platform_secret)
        .await
        .expect("platform Database credential is not a user_secrets row");
    let attached = db_store
        .get_internal(database.id)
        .await
        .expect("attached Database");
    assert_eq!(attached.credential_secret_id, Some(platform_secret));
    let listed_after = get(port, &format!("/api/databases/{}", database.id), &token).await;
    assert_eq!(listed_after.status, 200, "{}", listed_after.text());
    assert!(
        !listed_after.text().contains(&platform_secret.to_string()),
        "Database JSON must not expose the platform credential id: {}",
        listed_after.text()
    );
    let by_env = get(
        port,
        &format!("/api/environments/{dev_id}/database"),
        &token,
    )
    .await;
    assert_eq!(by_env.status, 200, "{}", by_env.text());
    assert_eq!(
        by_env.json()["database"]["id"],
        database.id.to_string(),
        "{}",
        by_env.text()
    );
    assert!(!by_env.text().contains("DATABASE_URL"), "{}", by_env.text());

    let backup = mutate(
        port,
        "POST",
        &format!("/api/databases/{}/backups", database.id),
        &token,
        "{}",
    )
    .await;
    assert_eq!(backup.status, 202, "{}", backup.text());
    let listed_backups = get(
        port,
        &format!("/api/databases/{}/backups", database.id),
        &token,
    )
    .await;
    assert_eq!(listed_backups.status, 200, "{}", listed_backups.text());
    assert_eq!(
        listed_backups.json()["items"].as_array().map(Vec::len),
        Some(0),
        "transport failure must not invent a backup object: {}",
        listed_backups.text()
    );
    let restore_missing = mutate(
        port,
        "POST",
        &format!("/api/databases/{}/restores", database.id),
        &token,
        &format!(
            r#"{{"backup_id":"{}","operation_id":"{}"}}"#,
            Uuid::new_v4(),
            Uuid::new_v4()
        ),
    )
    .await;
    assert_eq!(
        restore_missing.status,
        404,
        "restore of an unknown backup must not mint an approval: {}",
        restore_missing.text()
    );
    let backup_id = Uuid::new_v4();
    sqlx::query(
        "insert into database_backups \
         (id, database_id, object_key, content_hash, byte_length, kind) \
         values ($1, $2, 'backups/http-fixture', $3, 12, 'manual')",
    )
    .bind(backup_id)
    .bind(database.id)
    .bind([7u8; 32].as_slice())
    .execute(kernel.pool())
    .await
    .unwrap();
    let restore = mutate(
        port,
        "POST",
        &format!("/api/databases/{}/restores", database.id),
        &token,
        &format!(
            r#"{{"backup_id":"{backup_id}","operation_id":"{}"}}"#,
            Uuid::new_v4()
        ),
    )
    .await;
    assert_eq!(
        restore.status,
        409,
        "restore requires typed approval: {}",
        restore.text()
    );
    let metrics = get(
        port,
        &format!("/api/applications/{application_id}/metrics"),
        &token,
    )
    .await;
    assert_eq!(metrics.status, 200, "{}", metrics.text());
    assert_eq!(metrics.json()["applicationQuota"], 8);
    assert_eq!(metrics.json()["backupRetention"], 14);
    assert_eq!(
        metrics.json()["logChunkByteLimit"],
        voie_cloud::deployment_logs::MAX_LOG_CHUNK_BYTES
    );

    let suspended = mutate(
        port,
        "PATCH",
        &format!("/api/applications/{application_id}"),
        &token,
        r#"{"state":"suspended"}"#,
    )
    .await;
    assert_eq!(
        suspended.status,
        409,
        "dummy Fabric transport must not invent a suspended Application: {}",
        suspended.text()
    );
    let still_ready: String = sqlx::query_scalar("select state from applications where id = $1")
        .bind(application_id)
        .fetch_one(kernel.pool())
        .await
        .unwrap();
    assert_eq!(still_ready, "ready");

    let pending = mutate(
        port,
        "DELETE",
        &format!("/api/applications/{application_id}"),
        &token,
        "{}",
    )
    .await;
    assert_eq!(pending.status, 409, "{}", pending.text());
    let approval_id = pending.json()["approvalId"].as_str().unwrap().to_owned();
    let accepted = mutate(
        port,
        "POST",
        &format!("/api/approvals/{approval_id}/accept"),
        &token,
        "{}",
    )
    .await;
    assert_eq!(accepted.status, 200, "{}", accepted.text());
    assert_eq!(accepted.json()["approval"]["state"], "accepted");
    let deleted = mutate(
        port,
        "DELETE",
        &format!("/api/applications/{application_id}"),
        &token,
        &format!(r#"{{"approvalId":"{approval_id}"}}"#),
    )
    .await;
    assert_eq!(
        deleted.status,
        409,
        "dummy Fabric transport must not invent Application deletion: {}",
        deleted.text()
    );
    let app_state: String = sqlx::query_scalar("select state from applications where id = $1")
        .bind(application_id)
        .fetch_one(kernel.pool())
        .await
        .unwrap();
    assert_eq!(
        app_state, "deleting",
        "approved delete fences the Application even when Fabric cleanup fails"
    );
    let deploy_state: String =
        sqlx::query_scalar("select state from application_deployments where id = $1")
            .bind(deployment_id)
            .fetch_one(kernel.pool())
            .await
            .unwrap();
    assert_eq!(deploy_state, "active");
    let env_state: String =
        sqlx::query_scalar("select state from application_environments where id = $1")
            .bind(dev_id)
            .fetch_one(kernel.pool())
            .await
            .unwrap();
    assert_eq!(env_state, "ready");

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn agent_tools_create_inspect_build_deploy_and_activate() {
    use serde_json::json;
    use voie_cloud::applications::ApplicationError;
    use voie_cloud::http::Platform;

    let _lock = ENV_LOCK.lock().expect("environment fixture lock");
    let mut environment = EnvironmentRestore::new();
    let (_fixture, _listener, kernel) = spawn_server("app-tools", &mut environment).await;
    let owner = Uuid::new_v4();
    insert_user(&kernel, owner, "tool-owner").await;
    let project = Uuid::new_v4();
    let fabric = Uuid::new_v4();
    let workspace = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("fab-{fabric}"))
        .execute(kernel.pool())
        .await
        .unwrap();
    sqlx::query(
        "insert into projects (id, owner_user_id, name, kind) values ($1, $2, 'Tools', 'personal')",
    )
    .bind(project)
    .bind(owner)
    .execute(kernel.pool())
    .await
    .unwrap();
    sqlx::query("insert into project_members (project_id, user_id, role) values ($1, $2, 'owner')")
        .bind(project)
        .bind(owner)
        .execute(kernel.pool())
        .await
        .unwrap();
    sqlx::query(
        "insert into workspaces (id, fabric_id, project_id, state, exec_generation) values ($1, $2, $3, 'ready', 1)",
    )
    .bind(workspace)
    .bind(fabric)
    .bind(project)
    .execute(kernel.pool())
    .await
    .unwrap();

    let platform = Platform::new(kernel.pool().clone(), "console.test".into(), Some(fabric));
    let stranger = Uuid::new_v4();
    insert_user(&kernel, stranger, "tool-stranger").await;
    let denied_stranger = platform
        .execute_tool(
            stranger,
            project,
            workspace,
            "application.create",
            &json!({ "name": "Tracker", "slug": unique_slug("denied") }),
        )
        .await;
    assert!(
        matches!(denied_stranger, Err(ApplicationError::Auth)),
        "a non-member must not create an Application: {denied_stranger:?}"
    );
    let denied_nil = platform
        .execute_tool(
            Uuid::nil(),
            project,
            workspace,
            "application.create",
            &json!({ "name": "Tracker", "slug": unique_slug("nil") }),
        )
        .await;
    assert!(
        matches!(denied_nil, Err(ApplicationError::Auth)),
        "activation must not authorize product tools as the nil UUID: {denied_nil:?}"
    );
    let slug = unique_slug("tool-app");
    let created = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "application.create",
            &json!({ "name": "Tracker", "slug": slug }),
        )
        .await
        .expect("application.create");
    assert_eq!(created["application"]["slug"], slug);
    assert_eq!(created["application"]["projectId"], project.to_string());
    assert_eq!(created["environments"].as_array().map(Vec::len), Some(2));
    let created_id = created["application"]["id"].as_str().unwrap().to_owned();
    let created_again = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "application.create",
            &json!({ "name": "Tracker", "slug": slug }),
        )
        .await
        .expect("application.create on the same Workspace is idempotent");
    assert_eq!(
        created_again["application"]["id"], created_id,
        "retrying application.create with the same slug must not hand off a new Workspace: {created_again}"
    );
    assert!(
        created_again.get("workspaceHandoff").is_none()
            || created_again["workspaceHandoff"].is_null(),
        "same-slug retry must stay on this Workspace: {created_again}"
    );

    let missing_ready = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "environment.deploy_dev",
            &json!({}),
        )
        .await;
    assert!(
        matches!(missing_ready, Err(ApplicationError::NotFound)),
        "deploy_dev without a ready Release must fail closed: {missing_ready:?}"
    );

    let inspected = platform
        .execute_tool(owner, project, workspace, "application.inspect", &json!({}))
        .await
        .expect("application.inspect");
    assert_eq!(inspected["application"]["slug"], slug);

    let status = platform
        .execute_tool(owner, project, workspace, "application.status", &json!({}))
        .await
        .expect("application.status");
    assert_eq!(status["releases"].as_array().map(Vec::len), Some(0));
    assert_eq!(status["deployments"].as_array().map(Vec::len), Some(0));
    assert_eq!(status["databases"].as_array().map(Vec::len), Some(0));
    assert_eq!(status["approvals"].as_array().map(Vec::len), Some(0));

    let platform_without_env_fabric =
        Platform::new(kernel.pool().clone(), "console.test".into(), None);
    let database = platform_without_env_fabric
        .execute_tool(
            owner,
            project,
            workspace,
            "database.create",
            &json!({ "kind": "dev" }),
        )
        .await
        .expect("database.create uses the Workspace fabric_id");
    let db_text = database.to_string();
    assert!(!db_text.contains("postgres://"), "{db_text}");
    assert!(!db_text.contains("DATABASE_URL"), "{db_text}");
    assert!(
        !db_text.to_ascii_lowercase().contains("password"),
        "{db_text}"
    );
    assert_eq!(database["database"]["state"], "creating");
    let database_id = Uuid::parse_str(database["database"]["id"].as_str().unwrap()).unwrap();
    let repeated = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "database.create",
            &json!({ "kind": "dev" }),
        )
        .await
        .expect("database.create is one Database per Environment");
    assert_eq!(
        repeated["database"]["id"],
        database_id.to_string(),
        "retrying database.create must resume the existing Environment Database: {repeated}"
    );
    let status_omit = platform
        .execute_tool(owner, project, workspace, "database.status", &json!({}))
        .await
        .expect("database.status without database_id lists this Application");
    assert_eq!(
        status_omit["database"]["id"],
        database_id.to_string(),
        "single Database poll must still expose database: {status_omit}"
    );
    assert_eq!(status_omit["items"].as_array().map(Vec::len), Some(1));
    let status_text = status_omit.to_string();
    assert!(!status_text.contains("postgres://"), "{status_text}");
    assert!(!status_text.contains("DATABASE_URL"), "{status_text}");
    let status_dev = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "database.status",
            &json!({ "kind": "dev" }),
        )
        .await
        .expect("database.status kind=dev");
    assert_eq!(status_dev["database"]["id"], database_id.to_string());
    let status_prod = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "database.status",
            &json!({ "kind": "prod" }),
        )
        .await
        .expect("database.status kind=prod with no Database");
    assert_eq!(status_prod["items"].as_array().map(Vec::len), Some(0));
    assert!(
        status_prod.get("database").is_none(),
        "prod poll must not invent a Database: {status_prod}"
    );
    let status_explicit = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "database.status",
            &json!({ "database_id": database_id.to_string() }),
        )
        .await
        .expect("database.status with database_id");
    assert_eq!(status_explicit["database"]["id"], database_id.to_string());

    let backup = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "database.backup",
            &json!({ "database_id": database_id.to_string() }),
        )
        .await
        .expect("database.backup");
    assert_eq!(backup["state"], "dispatched");
    let listed = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "database.list_backups",
            &json!({ "database_id": database_id.to_string() }),
        )
        .await
        .expect("database.list_backups");
    assert_eq!(
        listed["items"].as_array().map(Vec::len),
        Some(0),
        "transport failure must not invent a backup: {listed}"
    );
    let missing_restore = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "database.restore",
            &json!({
                "database_id": database_id.to_string(),
                "backup_id": Uuid::new_v4().to_string(),
            }),
        )
        .await;
    assert!(
        matches!(missing_restore, Err(ApplicationError::NotFound)),
        "restore of an unknown backup must not mint an approval: {missing_restore:?}"
    );
    let backup_id = Uuid::new_v4();
    sqlx::query(
        "insert into database_backups \
         (id, database_id, object_key, content_hash, byte_length, kind) \
         values ($1, $2, 'backups/fixture', $3, 12, 'manual')",
    )
    .bind(backup_id)
    .bind(database_id)
    .bind([7u8; 32].as_slice())
    .execute(kernel.pool())
    .await
    .unwrap();
    let restore = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "database.restore",
            &json!({
                "database_id": database_id.to_string(),
                "backup_id": backup_id.to_string(),
            }),
        )
        .await;
    assert!(
        matches!(restore, Err(ApplicationError::ApprovalRequired(_))),
        "restore requires typed approval: {restore:?}"
    );
    let restore_text = match &restore {
        Err(error) => error.product_text(),
        Ok(_) => String::new(),
    };
    assert!(
        restore_text.contains("approvalId"),
        "agent must see the approval id: {restore_text}"
    );
    let restore_approval = match restore {
        Err(ApplicationError::ApprovalRequired(id)) => id,
        other => panic!("expected restore approval id: {other:?}"),
    };
    let other_backup = Uuid::new_v4();
    sqlx::query(
        "insert into database_backups \
         (id, database_id, object_key, content_hash, byte_length, kind) \
         values ($1, $2, 'backups/other', $3, 8, 'manual')",
    )
    .bind(other_backup)
    .bind(database_id)
    .bind([8u8; 32].as_slice())
    .execute(kernel.pool())
    .await
    .unwrap();
    platform
        .applications
        .accept_pending_approval(owner, restore_approval)
        .await
        .expect("accept restore_database");
    let reused = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "database.restore",
            &json!({
                "database_id": database_id.to_string(),
                "backup_id": other_backup.to_string(),
                "approval_id": restore_approval.to_string(),
            }),
        )
        .await;
    assert!(
        matches!(reused, Err(ApplicationError::Auth)),
        "accepted restore approval must not cover a different backup: {reused:?}"
    );

    let built = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "release.build",
            &json!({ "manifest": SAMPLE_MANIFEST }),
        )
        .await
        .expect("release.build");
    assert_eq!(built["state"], "dispatched");
    let high_manifest =
        format!("{SAMPLE_MANIFEST}\n[resources]\ncpu_millis = 2000\nmemory_mb = 2048\n");
    let refused_tier = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "release.build",
            &json!({ "manifest": high_manifest }),
        )
        .await;
    let tier_approval = match refused_tier {
        Err(ApplicationError::ApprovalRequired(id)) => id,
        other => panic!("resources above the default tier require approval: {other:?}"),
    };
    platform
        .applications
        .accept_pending_approval(owner, tier_approval)
        .await
        .expect("accept increase_resource_tier");
    let raised = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "release.build",
            &json!({
                "manifest": high_manifest,
                "approval_id": tier_approval.to_string(),
            }),
        )
        .await
        .expect("release.build after increase_resource_tier");
    assert_eq!(raised["state"], "dispatched");
    let raised_intent = Uuid::parse_str(raised["buildIntentId"].as_str().unwrap()).unwrap();
    let raised_manifest: serde_json::Value =
        sqlx::query_scalar("select manifest from application_releases where build_intent_id = $1")
            .bind(raised_intent)
            .fetch_one(kernel.pool())
            .await
            .unwrap();
    assert_eq!(raised_manifest["resources"]["cpuMillis"], 2000);
    assert_eq!(raised_manifest["resources"]["memoryMb"], 2048);
    assert_eq!(built["state"], "dispatched");
    let intent = Uuid::parse_str(built["buildIntentId"].as_str().unwrap()).unwrap();
    sqlx::query(
        "update application_releases set state = 'ready', artifact_hash = $2, artifact_bytes = 12, artifact_key = 'k' \
         where build_intent_id = $1",
    )
    .bind(intent)
    .bind([9u8; 32].as_slice())
    .execute(kernel.pool())
    .await
    .unwrap();
    let release_id: Uuid =
        sqlx::query_scalar("select id from application_releases where build_intent_id = $1")
            .bind(intent)
            .fetch_one(kernel.pool())
            .await
            .unwrap();

    let deployed = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "environment.deploy_dev",
            &json!({}),
        )
        .await
        .expect("environment.deploy_dev without release_id");
    assert_eq!(deployed["state"], "materializing");
    assert_eq!(
        deployed["deployment"]["releaseId"],
        release_id.to_string(),
        "omitted release_id must select the latest ready Release"
    );
    let deployment_id = Uuid::parse_str(deployed["deploymentId"].as_str().unwrap()).unwrap();
    let status = platform
        .execute_tool(owner, project, workspace, "application.status", &json!({}))
        .await
        .expect("application.status after deploy");
    let status_text = status.to_string();
    assert!(!status_text.contains("postgres://"), "{status_text}");
    assert!(!status_text.contains("DATABASE_URL"), "{status_text}");
    assert_eq!(status["deployments"].as_array().map(Vec::len), Some(1));
    assert_eq!(status["deployments"][0]["id"], deployment_id.to_string());
    assert_eq!(status["deployments"][0]["state"], "materializing");
    let dep_status = platform
        .execute_tool(owner, project, workspace, "deployment.status", &json!({}))
        .await
        .expect("deployment.status without deployment_id");
    assert_eq!(
        dep_status["deployment"]["id"],
        deployment_id.to_string(),
        "omitted deployment_id must list this Application: {dep_status}"
    );
    assert_eq!(dep_status["items"].as_array().map(Vec::len), Some(1));

    sqlx::query(
        "update application_releases set state = 'ready', artifact_hash = $2, artifact_bytes = 12, artifact_key = 'k' \
         where build_intent_id = $1",
    )
    .bind(raised_intent)
    .bind([8u8; 32].as_slice())
    .execute(kernel.pool())
    .await
    .unwrap();
    let raised_release: Uuid =
        sqlx::query_scalar("select id from application_releases where build_intent_id = $1")
            .bind(raised_intent)
            .fetch_one(kernel.pool())
            .await
            .unwrap();
    let later = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "environment.deploy_dev",
            &json!({}),
        )
        .await
        .expect("environment.deploy_dev selects the newest ready Release");
    assert_eq!(
        later["deployment"]["releaseId"],
        raised_release.to_string(),
        "omitted release_id must not reuse an older ready Release: {later}"
    );
    let later_id = Uuid::parse_str(later["deploymentId"].as_str().unwrap()).unwrap();
    sqlx::query(
        "update application_deployments set state = 'stopped', terminal_at = now() where id = $1",
    )
    .bind(later_id)
    .execute(kernel.pool())
    .await
    .unwrap();
    let explicit_older = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "environment.deploy_dev",
            &json!({ "releaseId": release_id.to_string() }),
        )
        .await
        .expect("explicit releaseId still wins");
    assert_eq!(
        explicit_older["deployment"]["releaseId"],
        release_id.to_string()
    );
    let explicit_id = Uuid::parse_str(explicit_older["deploymentId"].as_str().unwrap()).unwrap();
    sqlx::query(
        "update application_deployments set state = 'stopped', terminal_at = now() where id = $1",
    )
    .bind(explicit_id)
    .execute(kernel.pool())
    .await
    .unwrap();
    assert_eq!(status["databases"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        status["databases"][0]["id"],
        database_id.to_string(),
        "{status}"
    );

    let refused = platform
        .execute_tool(owner, project, workspace, "deployment.activate", &json!({}))
        .await;
    assert!(
        matches!(refused, Err(ApplicationError::DeploymentNotReady)),
        "materializing candidate must not receive traffic: {refused:?}"
    );

    sqlx::query("update application_deployments set state = 'healthy' where id = $1")
        .bind(deployment_id)
        .execute(kernel.pool())
        .await
        .unwrap();
    let activated = platform
        .execute_tool(owner, project, workspace, "deployment.activate", &json!({}))
        .await
        .expect("deployment.activate without deployment_id");
    assert_eq!(activated["deployment"]["state"], "active");
    assert_eq!(activated["deployment"]["releaseId"], release_id.to_string());

    let refused_prod = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "environment.publish_prod",
            &json!({ "release_id": release_id.to_string() }),
        )
        .await;
    let publish_approval = match refused_prod {
        Err(ApplicationError::ApprovalRequired(id)) => id,
        other => panic!("production publish requires typed approval: {other:?}"),
    };
    platform
        .applications
        .accept_pending_approval(owner, publish_approval)
        .await
        .expect("accept publish_production");
    let published = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "environment.publish_prod",
            &json!({
                "release_id": release_id.to_string(),
                "approval_id": publish_approval.to_string(),
            }),
        )
        .await
        .expect("environment.publish_prod after approval");
    assert_eq!(published["state"], "materializing");

    let foreign = platform
        .execute_tool(
            owner,
            project,
            Uuid::new_v4(),
            "deployment.activate",
            &json!({ "deployment_id": deployment_id.to_string() }),
        )
        .await;
    assert!(
        matches!(foreign, Err(ApplicationError::NotFound)),
        "foreign workspace cannot activate this Deployment: {foreign:?}"
    );

    let pg_built = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "release.build",
            &json!({ "manifest": POSTGRES_MANIFEST }),
        )
        .await
        .expect("postgres release.build");
    assert_eq!(pg_built["state"], "dispatched");
    let pg_intent = Uuid::parse_str(pg_built["buildIntentId"].as_str().unwrap()).unwrap();
    sqlx::query(
        "update application_releases set state = 'ready', artifact_hash = $2, artifact_bytes = 12, artifact_key = 'k' \
         where build_intent_id = $1",
    )
    .bind(pg_intent)
    .bind([3u8; 32].as_slice())
    .execute(kernel.pool())
    .await
    .unwrap();
    let pg_release: Uuid =
        sqlx::query_scalar("select id from application_releases where build_intent_id = $1")
            .bind(pg_intent)
            .fetch_one(kernel.pool())
            .await
            .unwrap();
    let blocked = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "environment.deploy_dev",
            &json!({ "release_id": pg_release.to_string() }),
        )
        .await;
    assert!(
        matches!(blocked, Err(ApplicationError::DatabaseRequired)),
        "postgres Release must not deploy before the Database is ready: {blocked:?}"
    );
    sqlx::query("update application_databases set state = 'ready' where id = $1")
        .bind(database_id)
        .execute(kernel.pool())
        .await
        .unwrap();
    let pg_deployed = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "environment.deploy_dev",
            &json!({ "release_id": pg_release.to_string() }),
        )
        .await
        .expect("postgres deploy after ready Database");
    assert_eq!(pg_deployed["state"], "materializing");

    let refused_delete = platform
        .execute_tool(owner, project, workspace, "application.delete", &json!({}))
        .await;
    let approval_id = match refused_delete {
        Err(ApplicationError::ApprovalRequired(id)) => id,
        other => panic!("delete requires typed approval: {other:?}"),
    };
    let pending = platform
        .execute_tool(owner, project, workspace, "application.status", &json!({}))
        .await
        .expect("application.status after delete request");
    let pending_kinds: Vec<String> = pending["approvals"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| {
            item.get("kind")
                .and_then(|kind| kind.as_str())
                .map(str::to_owned)
        })
        .collect();
    assert!(
        pending_kinds
            .iter()
            .any(|kind| kind == "delete_application"),
        "status must surface the pending delete approval: {pending}"
    );
    platform
        .applications
        .accept_pending_approval(owner, approval_id)
        .await
        .expect("accept delete_application");
    // Platform-only harness has no Blob store. Dummy artifact keys must be
    // cleared before delete; reclaim of real blobs is covered by
    // resource_retention tests.
    sqlx::query(
        "update application_releases set artifact_key = null, artifact_bytes = 0 \
         where application_id = $1",
    )
    .bind(Uuid::parse_str(created["application"]["id"].as_str().unwrap()).unwrap())
    .execute(kernel.pool())
    .await
    .unwrap();
    let deleted = platform
        .execute_tool(
            owner,
            project,
            workspace,
            "application.delete",
            &json!({ "approval_id": approval_id.to_string() }),
        )
        .await
        .expect("application.delete after approval");
    assert_eq!(deleted["state"], "deleting");
    let gone: String = sqlx::query_scalar("select state from applications where id = $1")
        .bind(Uuid::parse_str(created["application"]["id"].as_str().unwrap()).unwrap())
        .fetch_one(kernel.pool())
        .await
        .unwrap();
    assert_eq!(gone, "deleting");
}
