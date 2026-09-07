//! Focused regression for indeterminate Fabric workspace creation.
//!
//! The defect: Fabricd returns HTTP 202 for its own `FabricError::Unknown`
//! (indeterminate outcome), but the old client treated any 2xx as success
//! and the control recorded a `ready` Workspace after an indeterminate
//! create. After the fix the client distinguishes only 200→Created from
//! 202→Unknown; every other status is `Response`, and transport failures
//! are `Transport`. A read-only existence probe (`GET /v1/workspaces/{id}`)
//! converges earlier indeterminate reservations without retrying the
//! unknown create automatically.
//!
//! Disposable doubles: throwaway mTLS material (CA + client + server certs
//! via openssl) and small Python HTTPS+mTLS stubs mirroring the Fabric's
//! real contract. No remote estate.

use std::path::{Path, PathBuf};
use tokio::io::AsyncBufReadExt;
use tokio::net::TcpListener;
use uuid::Uuid;
use voie_cloud::fabric_client::{CreateOutcome, FabricClient};

const STUB_SCRIPT: &str = r#"
import http.server, ssl, sys, os, json, urllib.parse

port, cert, key, ca, post_flag, get_flag = (
    int(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5], sys.argv[6]
)

class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def status_for_post(self):
        # post_flag file content (e.g. "202", "500", "201") overrides; absent => 200
        try:
            with open(post_flag) as f:
                return int(f.read().strip() or "200")
        except FileNotFoundError:
            return 200
        except Exception:
            return 500

    def handle_one(self):
        length = int(self.headers.get("content-length", 0))
        if length:
            self.rfile.read(length)
        # Route dispatch
        path = urllib.parse.urlparse(self.path).path
        method = self.command
        if method == "GET" and path == "/v1/health":
            self.send_response(200)
            self.send_header("content-length", "0")
            self.end_headers()
            return
        if method == "PUT" and path.startswith("/v1/workspaces/"):
            status = self.status_for_post()
            body = json.dumps({"state": "ready", "id": path.split("/")[-1]}).encode()
            self.send_response(status)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if method == "POST" and path == "/v1/workspaces":
            self.send_response(404)
            self.send_header("content-length", "0")
            self.end_headers()
            return
        if method == "GET" and path.startswith("/v1/workspaces/"):
            if os.path.exists(get_flag):
                self.send_response(404)
                self.send_header("content-length", "0")
                self.end_headers()
                return
            # Existence probe: 200 with state field
            state = "ready"
            try:
                # Allow get_flag content to encode state for exercising
                # the `creating`-on-Fabric branch (file contains "creating").
                with open(get_flag + ".state") as f:
                    state = f.read().strip() or "ready"
            except FileNotFoundError:
                pass
            body = json.dumps({"state": state, "id": path.split("/")[-1]}).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if method == "DELETE" and path.startswith("/v1/workspaces/"):
            status = 200
            state = "deleted"
            try:
                with open(get_flag + ".delete") as f:
                    raw = f.read().strip()
                if raw.isdigit():
                    status = int(raw)
                elif raw:
                    state = raw
            except FileNotFoundError:
                pass
            body = json.dumps({"id": path.split("/")[-1], "state": state}).encode()
            self.send_response(status)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            if status != 204:
                self.wfile.write(body)
            return
        if method == "POST" and path.endswith("/delete"):
            self.send_response(404)
            body = b'{"error":"not_found"}'
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        # Fallback
        self.send_response(500)
        self.send_header("content-length", "0")
        self.end_headers()

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

struct Pems {
    client_cert: PathBuf,
    client_key: PathBuf,
    ca_cert: PathBuf,
    server_cert: PathBuf,
    server_key: PathBuf,
}

fn pems(dir: &Path) -> Pems {
    fn sh(args: &[&str]) {
        let out = std::process::Command::new("openssl")
            .args(args)
            .output()
            .expect("openssl runs");
        assert!(
            out.status.success(),
            "openssl failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let ca_key = dir.join("ca.key");
    let ca_pem = dir.join("ca.pem");
    let client_key = dir.join("client.key");
    let client_csr = dir.join("client.csr");
    let client_pem = dir.join("client.pem");
    let server_key = dir.join("server.key");
    let server_csr = dir.join("server.csr");
    let server_pem = dir.join("server.pem");
    sh(&[
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-keyout",
        ca_key.to_str().unwrap(),
        "-out",
        ca_pem.to_str().unwrap(),
        "-days",
        "2",
        "-nodes",
        "-sha256",
        "-subj",
        "/CN=voie-test-ca",
        "-addext",
        "basicConstraints=critical,CA:TRUE",
        "-addext",
        "keyUsage=critical,keyCertSign,cRLSign",
    ]);
    let san = dir.join("san.ext");
    std::fs::write(
        &san,
        "basicConstraints=CA:FALSE\n\
         keyUsage=digitalSignature,keyEncipherment\n\
         extendedKeyUsage=serverAuth,clientAuth\n\
         subjectAltName=IP:127.0.0.1,DNS:localhost\n",
    )
    .unwrap();
    sh(&[
        "req",
        "-newkey",
        "rsa:2048",
        "-keyout",
        client_key.to_str().unwrap(),
        "-out",
        client_csr.to_str().unwrap(),
        "-nodes",
        "-subj",
        "/CN=voie-test-client",
    ]);
    sh(&[
        "x509",
        "-req",
        "-in",
        client_csr.to_str().unwrap(),
        "-CA",
        ca_pem.to_str().unwrap(),
        "-CAkey",
        ca_key.to_str().unwrap(),
        "-out",
        client_pem.to_str().unwrap(),
        "-days",
        "2",
        "-extfile",
        san.to_str().unwrap(),
    ]);
    sh(&[
        "req",
        "-newkey",
        "rsa:2048",
        "-keyout",
        server_key.to_str().unwrap(),
        "-out",
        server_csr.to_str().unwrap(),
        "-nodes",
        "-subj",
        "/CN=voie-test-fabric",
    ]);
    sh(&[
        "x509",
        "-req",
        "-in",
        server_csr.to_str().unwrap(),
        "-CA",
        ca_pem.to_str().unwrap(),
        "-CAkey",
        ca_key.to_str().unwrap(),
        "-out",
        server_pem.to_str().unwrap(),
        "-days",
        "2",
        "-extfile",
        san.to_str().unwrap(),
    ]);
    Pems {
        client_cert: client_pem,
        client_key,
        ca_cert: ca_pem,
        server_cert: server_pem,
        server_key,
    }
}

async fn spawn(
    dir: &Path,
    pems: &Pems,
    post_flag: PathBuf,
    get_flag: PathBuf,
) -> (u16, tokio::process::Child) {
    let script = dir.join(format!("stub_{}.py", Uuid::new_v4()));
    std::fs::write(&script, STUB_SCRIPT).unwrap();
    let mut child = tokio::process::Command::new("python3")
        .arg(&script)
        .arg("0")
        .arg(pems.server_cert.to_str().unwrap())
        .arg(pems.server_key.to_str().unwrap())
        .arg(pems.ca_cert.to_str().unwrap())
        .arg(post_flag.to_str().unwrap())
        .arg(get_flag.to_str().unwrap())
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut r = tokio::io::BufReader::new(stdout);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let mut line = Vec::new();
        match tokio::time::timeout(
            std::time::Duration::from_millis(200),
            r.read_until(b'\n', &mut line),
        )
        .await
        {
            Ok(Ok(n)) if n > 0 => {
                let port: u16 = String::from_utf8_lossy(&line).trim().parse().unwrap();
                return (port, child);
            }
            _ if std::time::Instant::now() > deadline => panic!("stub never reported port"),
            _ => continue,
        }
    }
}

fn client(pems: &Pems, port: u16) -> FabricClient {
    FabricClient::from_pem_files(
        format!("https://127.0.0.1:{port}"),
        &pems.client_cert,
        &pems.client_key,
        &pems.ca_cert,
    )
    .unwrap()
}

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn tmp(label: &str) -> (TempDir, PathBuf) {
    let dir = std::env::temp_dir().join(format!("voie-fabric-create-{label}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.clone();
    (TempDir(dir), path)
}

#[tokio::test]
async fn create_200_is_created() {
    let (tmpdir, dir) = tmp("200");
    let _tmpdir = tmpdir;
    let pems = pems(&dir);
    let post_flag = dir.join("post.status");
    let get_flag = dir.join("get.missing");
    let (port, mut child) = spawn(&dir, &pems, post_flag.clone(), get_flag).await;
    // No flag => POST 200
    let c = client(&pems, port);
    let outcome = c
        .create_workspace(Uuid::new_v4(), None, None)
        .await
        .unwrap();
    assert_eq!(outcome, CreateOutcome::Created);
    let _ = child.kill().await;
}

#[tokio::test]
async fn create_202_is_not_spec_success_or_unknown() {
    let (tmpdir, dir) = tmp("202");
    let _tmpdir = tmpdir;
    let pems = pems(&dir);
    let post_flag = dir.join("post.status");
    std::fs::write(&post_flag, b"202").unwrap();
    let get_flag = dir.join("get.missing");
    let (port, mut child) = spawn(&dir, &pems, post_flag, get_flag).await;
    let c = client(&pems, port);
    let err = c
        .create_workspace(Uuid::new_v4(), None, None)
        .await
        .expect_err("spec PUT is 200; 202 is not a desired-state accept");
    assert!(
        matches!(err, voie_cloud::fabric_client::FabricError::Response),
        "202 is not OutcomeUnknown: {err:?}"
    );
    let _ = child.kill().await;
}

#[tokio::test]
async fn create_500_maps_to_response_error() {
    let (tmpdir, dir) = tmp("500");
    let _tmpdir = tmpdir;
    let pems = pems(&dir);
    let post_flag = dir.join("post.status");
    std::fs::write(&post_flag, b"500").unwrap();
    let get_flag = dir.join("get.missing");
    let (port, mut child) = spawn(&dir, &pems, post_flag, get_flag).await;
    let c = client(&pems, port);
    let err = c
        .create_workspace(Uuid::new_v4(), None, None)
        .await
        .expect_err("500 is not success");
    assert!(
        matches!(err, voie_cloud::fabric_client::FabricError::Response),
        "non-2xx maps to Response, got {err:?}"
    );
    let _ = child.kill().await;
}

#[tokio::test]
async fn create_201_is_not_success() {
    let (tmpdir, dir) = tmp("201");
    let _tmpdir = tmpdir;
    let pems = pems(&dir);
    let post_flag = dir.join("post.status");
    std::fs::write(&post_flag, b"201").unwrap();
    let get_flag = dir.join("get.missing");
    let (port, mut child) = spawn(&dir, &pems, post_flag, get_flag).await;
    let c = client(&pems, port);
    let err = c
        .create_workspace(Uuid::new_v4(), None, None)
        .await
        .expect_err("201 must not be treated as Created");
    assert!(matches!(
        err,
        voie_cloud::fabric_client::FabricError::Response
    ));
    let _ = child.kill().await;
}

#[tokio::test]
async fn create_transport_error_maps_to_transport() {
    // No server on this port -> Transport.
    let (tmpdir, dir) = tmp("transport");
    let _tmpdir = tmpdir;
    let pems = pems(&dir);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let c = client(&pems, port);
    let err = c
        .create_workspace(Uuid::new_v4(), None, None)
        .await
        .expect_err("unreachable port is Transport");
    assert!(
        matches!(err, voie_cloud::fabric_client::FabricError::Transport),
        "got {err:?}"
    );
}

#[tokio::test]
async fn probe_404_is_none_and_200_is_state() {
    let (tmpdir, dir) = tmp("probe");
    let _tmpdir = tmpdir;
    let pems = pems(&dir);
    let post_flag = dir.join("post.status");
    let get_flag = dir.join("get.missing");
    let (port, mut child) = spawn(&dir, &pems, post_flag.clone(), get_flag.clone()).await;
    let c = client(&pems, port);

    // Default: GET 200 with state ready
    let probe = c.get_workspace(Uuid::new_v4()).await.unwrap();
    assert_eq!(probe.as_deref(), Some("ready"));

    // Flip to 404
    std::fs::write(&get_flag, b"1").unwrap();
    let probe = c.get_workspace(Uuid::new_v4()).await.unwrap();
    assert_eq!(probe, None, "404 maps to None (provably absent)");

    // Still 404 for a second id
    let probe = c.get_workspace(Uuid::new_v4()).await.unwrap();
    assert_eq!(probe, None);

    // Back to 200 but with `creating` state (Fabric hasn't finished realizing)
    std::fs::remove_file(&get_flag).unwrap();
    std::fs::write(get_flag.with_extension("missing.state"), b"creating").unwrap();
    let probe = c.get_workspace(Uuid::new_v4()).await.unwrap();
    assert_eq!(
        probe.as_deref(),
        Some("creating"),
        "Fabric's own creating must be surfaced, not coerced to ready"
    );

    let _ = child.kill().await;
}

#[tokio::test]
async fn delete_workspace_requires_deleted_state() {
    let (tmpdir, dir) = tmp("delete");
    let _tmpdir = tmpdir;
    let pems = pems(&dir);
    let post_flag = dir.join("post.status");
    let get_flag = dir.join("get.missing");
    let (port, mut child) = spawn(&dir, &pems, post_flag, get_flag.clone()).await;
    let c = client(&pems, port);
    let id = Uuid::new_v4();

    c.delete_workspace(id)
        .await
        .expect("200 deleted is success");

    std::fs::write(format!("{}.delete", get_flag.display()), b"deleting").unwrap();
    let err = c
        .delete_workspace(id)
        .await
        .expect_err("200 deleting must not commit cleanup");
    assert!(
        matches!(err, voie_cloud::fabric_client::FabricError::OutcomeUnknown),
        "got {err:?}"
    );

    std::fs::write(format!("{}.delete", get_flag.display()), b"404").unwrap();
    c.delete_workspace(id)
        .await
        .expect("404 is already-gone success");

    let outcome = c
        .product_mutate(
            &format!("/v1/releases/{id}/delete"),
            &serde_json::json!({
                "operation_id": id,
                "request_hash": "h",
                "desired_revision": 1
            }),
        )
        .await
        .expect("delete 404 is cleanup absence");
    assert_eq!(outcome.state, "absent");

    let _ = child.kill().await;
}
