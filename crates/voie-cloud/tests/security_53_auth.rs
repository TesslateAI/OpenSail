//! #53 authorization: no Fabric effect before ownership, and Deployment
//! restart uses DeployDev / ManageProduction rather than ReadProject.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use uuid::Uuid;
use voie_cloud::auth::{Auth, AuthConfig};
use voie_cloud::integration::Services;
use voie_cloud::web_session;
use voie_cloud::{Config, Kernel, serve_with_services};

#[path = "common/tls_pems.rs"]
mod tls_pems;

static ENV_LOCK: Mutex<()> = Mutex::new(());

const PUBLIC_ORIGIN: &str = "https://console.test";
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

const STUB_SCRIPT: &str = r#"
import http.server, ssl, sys, os, json, urllib.parse

port, cert, key, ca, log_path = (
    int(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5]
)

def record(method, path):
    with open(log_path, "a") as f:
        f.write(f"{method} {path}\n")

class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def handle_one(self):
        length = int(self.headers.get("content-length", 0))
        if length:
            self.rfile.read(length)
        path = urllib.parse.urlparse(self.path).path
        record(self.command, path)
        body = b'{"state":"ok","resourceId":"x"}'
        if self.command == "GET" and path.startswith("/v1/workspaces/"):
            body = json.dumps({"state": "ready", "image": "voie-workspace:v1"}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self): self.handle_one()
    def do_POST(self): self.handle_one()
    def do_PUT(self): self.handle_one()
    def do_DELETE(self): self.handle_one()
    def log_message(self, *_a): pass

server = http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler)
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(cert, key)
ctx.verify_mode = ssl.CERT_REQUIRED
ctx.load_verify_locations(ca)
server.socket = ctx.wrap_socket(server.socket, server_side=True)
print(server.server_address[1], flush=True)
server.serve_forever()
"#;

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

    fn remove(&mut self, name: &'static str) {
        self.previous.push((name, std::env::var_os(name)));
        unsafe { std::env::remove_var(name) };
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

struct Stub {
    child: Child,
    log: PathBuf,
}

impl Drop for Stub {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Stub {
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .filter(|line| !line.is_empty())
            .collect()
    }

    fn clear(&self) {
        std::fs::write(&self.log, "").expect("fabric log truncates");
    }
}

struct HttpResponse {
    status: u16,
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
        body: body.to_vec(),
    }
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

fn spawn_recording_fabric(dir: &Path) -> (u16, Stub) {
    let pems = tls_pems::write_v3_mtls_bundle(dir);
    let cert = pems.server_pem;
    let key = pems.server_key;
    let ca = pems.ca_pem;
    let script = dir.join("stub.py");
    std::fs::write(&script, STUB_SCRIPT).unwrap();
    let log = dir.join("fabric.log");
    std::fs::write(&log, "").unwrap();
    let err_log = dir.join("stub.err");
    let err_file = std::fs::File::create(&err_log).expect("stub stderr file");
    let mut child = Command::new("python3")
        .arg(&script)
        .arg("0")
        .arg(&cert)
        .arg(&key)
        .arg(&ca)
        .arg(&log)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::from(err_file))
        .spawn()
        .expect("python stub starts");
    let mut stdout = child.stdout.take().expect("stub prints port");
    let mut buf = [0u8; 32];
    let n = std::io::Read::read(&mut stdout, &mut buf).expect("stub port");
    let port: u16 = std::str::from_utf8(&buf[..n])
        .unwrap()
        .trim()
        .parse()
        .expect("numeric port");
    (port, Stub { child, log })
}

async fn insert_user(kernel: &Kernel, user_id: Uuid, username: &str) {
    sqlx::query(
        "insert into users (id, issuer, subject, username, display_name, email, platform_role, status) \
         values ($1, $2, $3, $4, $5, $6, 'user', 'active')",
    )
    .bind(user_id)
    .bind(format!("sec53-{}", Uuid::new_v4()))
    .bind(username)
    .bind(format!("{username}-{user_id}"))
    .bind(username)
    .bind(format!("{username}@example.test"))
    .execute(kernel.pool())
    .await
    .expect("user inserts");
}

async fn insert_project(kernel: &Kernel, project: Uuid, owner: Uuid, name: &str) {
    sqlx::query(
        "insert into projects (id, owner_user_id, name, kind) values ($1, $2, $3, 'personal')",
    )
    .bind(project)
    .bind(owner)
    .bind(name)
    .execute(kernel.pool())
    .await
    .unwrap();
    sqlx::query("insert into project_members (project_id, user_id, role) values ($1, $2, 'owner')")
        .bind(project)
        .bind(owner)
        .execute(kernel.pool())
        .await
        .unwrap();
}

async fn insert_member(kernel: &Kernel, project: Uuid, user: Uuid, role: &str) {
    sqlx::query("insert into project_members (project_id, user_id, role) values ($1, $2, $3)")
        .bind(project)
        .bind(user)
        .bind(role)
        .execute(kernel.pool())
        .await
        .unwrap();
}

async fn insert_workspace(kernel: &Kernel, workspace: Uuid, fabric: Uuid, project: Uuid) {
    sqlx::query(
        "insert into workspaces (id, fabric_id, project_id, state, exec_generation, \
             desired_state, observed_state, desired_revision, observed_revision, reconcile_after) \
         values ($1, $2, $3, 'creating', 1, 'active', 'ready', 1, 1, now() + interval '1 hour')",
    )
    .bind(workspace)
    .bind(fabric)
    .bind(project)
    .execute(kernel.pool())
    .await
    .unwrap();
}

async fn session(kernel: &Kernel, user: Uuid) -> String {
    web_session::create(kernel.pool(), user, std::time::Duration::from_secs(3600))
        .await
        .expect("session")
        .1
}

#[tokio::test]
async fn foreign_workspace_create_and_release_cause_zero_fabric_effects() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut environment = EnvironmentRestore::new();
    let fixture = TempDir::new("sec53-auth");
    let (fabric_port, stub) = spawn_recording_fabric(fixture.0.as_path());
    environment.remove("VOIE_PRODUCT_RUNTIME");
    environment.set("VOIE_PUBLIC_ORIGIN", PUBLIC_ORIGIN);
    environment.set("VOIE_AZURE_BLOB_ACCOUNT", "sec53-account");
    environment.set(
        "VOIE_AZURE_BLOB_KEY",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    environment.set("VOIE_AZURE_BLOB_CONTAINER", "sec53-container");
    environment.set("VOIE_AZURE_BLOB_ENDPOINT", "http://127.0.0.1:1");
    environment.set("VOIE_MODEL_BASE_URL", "http://127.0.0.1:1");
    environment.set("VOIE_MODEL_NAME", "sec53-model");
    environment.set("VOIE_MODEL_API_KEY", "sec53-key");
    environment.set(
        "VOIE_FABRIC_ENDPOINT",
        format!("https://127.0.0.1:{fabric_port}"),
    );
    environment.set(
        "VOIE_FABRIC_CLIENT_CERT_PATH",
        fixture.path("client.pem").to_str().unwrap(),
    );
    environment.set(
        "VOIE_FABRIC_CLIENT_KEY_PATH",
        fixture.path("client.key").to_str().unwrap(),
    );
    environment.set(
        "VOIE_FABRIC_CA_CERT_PATH",
        fixture.path("ca.pem").to_str().unwrap(),
    );
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
    let port = listener.local_addr().expect("addr").port();

    let user_a = Uuid::new_v4();
    let user_b = Uuid::new_v4();
    insert_user(&kernel, user_a, "owner-a").await;
    insert_user(&kernel, user_b, "owner-b").await;
    let project_a = Uuid::new_v4();
    let project_b = Uuid::new_v4();
    let fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .unwrap();
    insert_project(&kernel, project_a, user_a, "A").await;
    insert_project(&kernel, project_b, user_b, "B").await;
    let workspace_a = Uuid::new_v4();
    insert_workspace(&kernel, workspace_a, fabric, project_a).await;
    let token_a = session(&kernel, user_a).await;
    let token_b = session(&kernel, user_b).await;
    let auth = Arc::new(
        Auth::connect(AuthConfig::native(PUBLIC_ORIGIN), kernel.pool().clone())
            .await
            .expect("auth"),
    );
    let services = Services::from_env(kernel.pool().clone()).expect("services");
    {
        let fabric = voie_cloud::fabric_client::FabricClient::from_env().expect("fabric client");
        fabric.health().await.expect("fabric mTLS health");
    }
    let server = tokio::spawn(serve_with_services(
        listener,
        kernel.clone(),
        auth,
        services,
    ));
    stub.clear();

    let denied_create = mutate(
        port,
        "POST",
        &format!("/api/projects/{project_b}/applications"),
        &token_b,
        &format!(
            r#"{{"name":"Stolen","slug":"stolen-{}","workspace_id":"{workspace_a}"}}"#,
            Uuid::new_v4().simple()
        ),
    )
    .await;
    assert_eq!(denied_create.status, 404, "{}", denied_create.text());
    let (state, generation): (String, i64) =
        sqlx::query_as("select state, exec_generation from workspaces where id = $1")
            .bind(workspace_a)
            .fetch_one(kernel.pool())
            .await
            .unwrap();
    assert_eq!(state, "creating");
    assert_eq!(generation, 1);
    let fabric_calls = stub.calls();
    assert!(
        fabric_calls.is_empty(),
        "foreign Application create must not contact Fabric: {fabric_calls:?}"
    );

    let slug = format!("app-a-{}", Uuid::new_v4().simple());
    let created = mutate(
        port,
        "POST",
        &format!("/api/projects/{project_a}/applications"),
        &token_a,
        &format!(r#"{{"name":"Invoice","slug":"{slug}","workspace_id":"{workspace_a}"}}"#),
    )
    .await;
    assert_eq!(created.status, 201, "{}", created.text());
    let application_id = created.json()["application"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    stub.clear();

    let denied_release = mutate(
        port,
        "POST",
        &format!("/api/applications/{application_id}/releases"),
        &token_b,
        &format!(
            r#"{{"build_intent_id":"{}","workspace_id":"{workspace_a}","source_exec_generation":1,"manifest":{}}}"#,
            Uuid::new_v4(),
            serde_json::to_string(SAMPLE_MANIFEST).unwrap()
        ),
    )
    .await;
    assert!(
        denied_release.status == 403 || denied_release.status == 404,
        "foreign Release create must be denied: {} {}",
        denied_release.status,
        denied_release.text()
    );
    let fabric_calls = stub.calls();
    assert!(
        fabric_calls.iter().all(|call| !call.contains("/exec")
            && !call.contains("/replace")
            && !call.contains("guest")),
        "foreign Release create must not exec or replace: {fabric_calls:?}"
    );
    assert!(
        !fabric_calls.iter().any(|call| call.starts_with("POST ")),
        "foreign Release create must not POST to Fabric: {fabric_calls:?}"
    );

    let (state, generation): (String, i64) =
        sqlx::query_as("select state, exec_generation from workspaces where id = $1")
            .bind(workspace_a)
            .fetch_one(kernel.pool())
            .await
            .unwrap();
    assert_eq!(state, "creating");
    assert_eq!(generation, 1);
    server.abort();
}

#[tokio::test]
async fn deployment_restart_uses_environment_mutation_permission() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut environment = EnvironmentRestore::new();
    let fixture = TempDir::new("sec53-restart");
    let (fabric_port, stub) = spawn_recording_fabric(fixture.0.as_path());
    environment.remove("VOIE_PRODUCT_RUNTIME");
    environment.set("VOIE_PUBLIC_ORIGIN", PUBLIC_ORIGIN);
    environment.set("VOIE_AZURE_BLOB_ACCOUNT", "sec53-account");
    environment.set(
        "VOIE_AZURE_BLOB_KEY",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    environment.set("VOIE_AZURE_BLOB_CONTAINER", "sec53-container");
    environment.set("VOIE_AZURE_BLOB_ENDPOINT", "http://127.0.0.1:1");
    environment.set("VOIE_MODEL_BASE_URL", "http://127.0.0.1:1");
    environment.set("VOIE_MODEL_NAME", "sec53-model");
    environment.set("VOIE_MODEL_API_KEY", "sec53-key");
    environment.set(
        "VOIE_FABRIC_ENDPOINT",
        format!("https://127.0.0.1:{fabric_port}"),
    );
    environment.set(
        "VOIE_FABRIC_CLIENT_CERT_PATH",
        fixture.path("client.pem").to_str().unwrap(),
    );
    environment.set(
        "VOIE_FABRIC_CLIENT_KEY_PATH",
        fixture.path("client.key").to_str().unwrap(),
    );
    environment.set(
        "VOIE_FABRIC_CA_CERT_PATH",
        fixture.path("ca.pem").to_str().unwrap(),
    );
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
    let port = listener.local_addr().expect("addr").port();

    let owner = Uuid::new_v4();
    let admin = Uuid::new_v4();
    let member = Uuid::new_v4();
    let viewer = Uuid::new_v4();
    insert_user(&kernel, owner, "owner").await;
    insert_user(&kernel, admin, "admin").await;
    insert_user(&kernel, member, "member").await;
    insert_user(&kernel, viewer, "viewer").await;
    let project = Uuid::new_v4();
    let fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .unwrap();
    insert_project(&kernel, project, owner, "Restart").await;
    insert_member(&kernel, project, admin, "admin").await;
    insert_member(&kernel, project, member, "member").await;
    insert_member(&kernel, project, viewer, "viewer").await;
    let workspace = Uuid::new_v4();
    insert_workspace(&kernel, workspace, fabric, project).await;
    let application = Uuid::new_v4();
    sqlx::query(
        "insert into applications (id, project_id, workspace_id, name, slug, root_path, runtime_profile, state, created_by_user_id) \
         values ($1, $2, $3, 'App', $4, '.', 'universal-v1', 'ready', $5)",
    )
    .bind(application)
    .bind(project)
    .bind(workspace)
    .bind(format!("app-{}", Uuid::new_v4().simple()))
    .bind(owner)
    .execute(kernel.pool())
    .await
    .unwrap();
    let dev_env = Uuid::new_v4();
    let prod_env = Uuid::new_v4();
    sqlx::query(
        "insert into application_environments (id, application_id, kind, visibility, hostname, state) \
         values ($1, $2, 'dev', 'private', $3, 'ready')",
    )
    .bind(dev_env)
    .bind(application)
    .bind(format!("dev-{}.example.test", Uuid::new_v4().simple()))
    .execute(kernel.pool())
    .await
    .unwrap();
    sqlx::query(
        "insert into application_environments (id, application_id, kind, visibility, hostname, state) \
         values ($1, $2, 'prod', 'private', $3, 'ready')",
    )
    .bind(prod_env)
    .bind(application)
    .bind(format!("prod-{}.example.test", Uuid::new_v4().simple()))
    .execute(kernel.pool())
    .await
    .unwrap();
    let release = Uuid::new_v4();
    sqlx::query(
        "insert into application_releases (id, application_id, build_intent_id, request_hash, source_workspace_id, \
         source_exec_generation, runtime_profile, manifest, manifest_hash, artifact_key, artifact_hash, artifact_bytes, \
         state, created_by_user_id) \
         values ($1, $2, $3, $4, $5, 1, 'universal-v1', $8::jsonb, $4, $7, $4, 12, 'ready', $6)",
    )
    .bind(release)
    .bind(application)
    .bind(Uuid::new_v4())
    .bind(&[1u8; 32][..])
    .bind(workspace)
    .bind(owner)
    .bind(format!("releases/{release}"))
    .bind(
        r#"{"run":{"command":["true"],"port":3000,"healthPath":"/healthz"},"resources":{"cpuMillis":500,"memoryMb":512}}"#,
    )
    .execute(kernel.pool())
    .await
    .unwrap();
    async fn insert_deployment(
        kernel: &Kernel,
        environment: Uuid,
        release: Uuid,
        owner: Uuid,
        hash: &[u8],
    ) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "insert into application_deployments (id, environment_id, release_id, deployment_intent_id, request_hash, \
             proven, desired_revision, observed_revision, created_by_user_id, active_at) \
             values ($1, $2, $3, $4, $5, true, 1, 1, $6, now())",
        )
        .bind(id)
        .bind(environment)
        .bind(release)
        .bind(Uuid::new_v4())
        .bind(hash)
        .bind(owner)
        .execute(kernel.pool())
        .await
        .unwrap();
        sqlx::query(
            "update application_environments set active_deployment_id = $1, revision = revision + 1 \
             where id = $2",
        )
        .bind(id)
        .bind(environment)
        .execute(kernel.pool())
        .await
        .unwrap();
        id
    }
    let dev = insert_deployment(&kernel, dev_env, release, owner, &[2u8; 32]).await;
    let prod = insert_deployment(&kernel, prod_env, release, owner, &[3u8; 32]).await;

    let viewer_token = session(&kernel, viewer).await;
    let member_token = session(&kernel, member).await;
    let admin_token = session(&kernel, admin).await;
    let owner_token = session(&kernel, owner).await;
    let auth = Arc::new(
        Auth::connect(AuthConfig::native(PUBLIC_ORIGIN), kernel.pool().clone())
            .await
            .expect("auth"),
    );
    let services = Services::from_env(kernel.pool().clone()).expect("services");
    {
        let fabric = voie_cloud::fabric_client::FabricClient::from_env().expect("fabric client");
        fabric.health().await.expect("fabric mTLS health");
    }
    let server = tokio::spawn(serve_with_services(
        listener,
        kernel.clone(),
        auth,
        services,
    ));

    async fn snapshot(kernel: &Kernel, id: Uuid) -> (String, i64, i64) {
        sqlx::query_as(
            "select state, desired_revision, observed_revision from application_deployments where id = $1",
        )
        .bind(id)
        .fetch_one(kernel.pool())
        .await
        .unwrap()
    }

    let before_dev = snapshot(&kernel, dev).await;
    let before_prod = snapshot(&kernel, prod).await;
    stub.clear();
    let viewer_dev = mutate(
        port,
        "POST",
        &format!("/api/deployments/{dev}/restart"),
        &viewer_token,
        "{}",
    )
    .await;
    assert_eq!(viewer_dev.status, 403, "{}", viewer_dev.text());
    let viewer_prod = mutate(
        port,
        "POST",
        &format!("/api/deployments/{prod}/restart"),
        &viewer_token,
        "{}",
    )
    .await;
    assert_eq!(viewer_prod.status, 403, "{}", viewer_prod.text());
    assert_eq!(snapshot(&kernel, dev).await, before_dev);
    assert_eq!(snapshot(&kernel, prod).await, before_prod);
    assert!(
        !stub
            .calls()
            .iter()
            .any(|call| call.contains("PUT /v1/deployments/")),
        "viewer restart must not call Fabric: {:?}",
        stub.calls()
    );

    stub.clear();
    let member_prod = mutate(
        port,
        "POST",
        &format!("/api/deployments/{prod}/restart"),
        &member_token,
        "{}",
    )
    .await;
    assert_eq!(member_prod.status, 403, "{}", member_prod.text());
    assert_eq!(snapshot(&kernel, prod).await, before_prod);
    assert!(
        !stub
            .calls()
            .iter()
            .any(|call| call.contains("PUT /v1/deployments/")),
        "member prod restart must not call Fabric: {:?}",
        stub.calls()
    );

    stub.clear();
    let member_dev = mutate(
        port,
        "POST",
        &format!("/api/deployments/{dev}/restart"),
        &member_token,
        "{}",
    )
    .await;
    assert_ne!(member_dev.status, 403, "{}", member_dev.text());
    let after_dev = snapshot(&kernel, dev).await;
    assert_ne!(after_dev, before_dev, "member may restart dev");
    assert!(
        stub.calls()
            .iter()
            .any(|call| call.contains("PUT /v1/deployments/")),
        "member dev restart must PUT the running Deployment spec: status={} body={} calls={:?}",
        member_dev.status,
        member_dev.text(),
        stub.calls()
    );

    stub.clear();
    sqlx::query("update application_deployments set proven = true where id = $1")
        .bind(prod)
        .execute(kernel.pool())
        .await
        .unwrap();
    let admin_prod = mutate(
        port,
        "POST",
        &format!("/api/deployments/{prod}/restart"),
        &admin_token,
        "{}",
    )
    .await;
    assert_ne!(admin_prod.status, 403, "{}", admin_prod.text());
    assert!(
        stub.calls()
            .iter()
            .any(|call| call.contains("PUT /v1/deployments/")),
        "admin prod restart must PUT the running Deployment spec: {:?}",
        stub.calls()
    );

    stub.clear();
    sqlx::query("update application_deployments set proven = true where id = $1")
        .bind(prod)
        .execute(kernel.pool())
        .await
        .unwrap();
    let owner_prod = mutate(
        port,
        "POST",
        &format!("/api/deployments/{prod}/restart"),
        &owner_token,
        "{}",
    )
    .await;
    assert_ne!(owner_prod.status, 403, "{}", owner_prod.text());
    assert!(
        stub.calls()
            .iter()
            .any(|call| call.contains("PUT /v1/deployments/")),
        "owner prod restart must PUT the running Deployment spec: {:?}",
        stub.calls()
    );
    server.abort();
}
