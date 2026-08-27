//! Readiness fails closed on every product dependency, and shutdown drains
//! in-flight connections instead of dropping them.
//!
//! Both behaviors run against local stand-ins only: dead loopback ports for
//! unreachable dependencies, tiny local servers for reachable ones (the
//! Fabric speaks real HTTPS+mTLS through a throwaway CA), and throwaway files
//! for activation artifacts. No remote estate or real provider credentials
//! are involved.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;
use voie_cloud::{Config, Kernel};

/// Arbitrary 32-byte key material; only shape matters to the local stubs.
const BLOB_KEY_BASE64: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const BLOB_ACCOUNT: &str = "voie-readiness-account";
const BLOB_CONTAINER: &str = "voie-readiness-container";
/// Unbound loopback port: every connection attempt fails immediately.
const DEAD_ENDPOINT: &str = "https://127.0.0.1:9";

fn set_env(name: &str, value: &str) {
    // Process-global configuration consumed by Services::from_env in this
    // test process only; each phase sets its values before construction.
    unsafe { std::env::set_var(name, value) };
}

fn clear_env(name: &str) {
    unsafe { std::env::remove_var(name) };
}

fn temp_dir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("voie-ready-{label}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("temp dir creates");
    dir
}

/// Throwaway mTLS material: a CA plus the client identity voie-cloud presents
/// and a Fabric server certificate for the local HTTPS health stand-in.
type FabricPems = (String, String, String, String, String);

fn fabric_pem_files(dir: &std::path::Path) -> FabricPems {
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
    let server_key = dir.join("server.key");
    let server_csr = dir.join("server.csr");
    let server_pem = dir.join("server.pem");
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
        "/CN=voie-ready-ca",
    ]);
    openssl(&[
        "req",
        "-newkey",
        "rsa:2048",
        "-keyout",
        client_key.to_str().expect("client key path"),
        "-out",
        client_csr.to_str().expect("csr path"),
        "-nodes",
        "-subj",
        "/CN=voie-ready-client",
    ]);
    openssl(&[
        "x509",
        "-req",
        "-in",
        client_csr.to_str().expect("csr path"),
        "-CA",
        ca_pem.to_str().expect("ca pem path"),
        "-CAkey",
        ca_key.to_str().expect("ca key path"),
        "-out",
        client_pem.to_str().expect("client pem path"),
        "-days",
        "2",
    ]);
    openssl(&[
        "req",
        "-newkey",
        "rsa:2048",
        "-keyout",
        server_key.to_str().expect("server key path"),
        "-out",
        server_csr.to_str().expect("server csr path"),
        "-nodes",
        "-subj",
        "/CN=voie-ready-fabric",
    ]);
    let san = dir.join("server-san.ext");
    std::fs::write(&san, "subjectAltName=IP:127.0.0.1,DNS:localhost")
        .expect("SAN extension writes");
    openssl(&[
        "x509",
        "-req",
        "-in",
        server_csr.to_str().expect("server csr path"),
        "-CA",
        ca_pem.to_str().expect("ca pem path"),
        "-CAkey",
        ca_key.to_str().expect("ca key path"),
        "-out",
        server_pem.to_str().expect("server pem path"),
        "-days",
        "2",
        "-extfile",
        san.to_str().expect("san path"),
    ]);
    (
        client_pem.display().to_string(),
        client_key.display().to_string(),
        ca_pem.display().to_string(),
        server_pem.display().to_string(),
        server_key.display().to_string(),
    )
}

const FABRIC_STUB_SCRIPT: &str = r#"
import http.server, ssl, sys

port, cert, key, ca, fixed = (
    int(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4], int(sys.argv[5])
)

class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def respond(self):
        self.send_response(fixed)
        self.send_header("content-length", "0")
        self.end_headers()

    def do_GET(self):
        self.respond()

    def do_POST(self):
        length = int(self.headers.get("content-length", 0))
        if length:
            self.rfile.read(length)
        self.respond()

    def log_message(self, *_args):
        pass

server = http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(cert, key)
context.verify_mode = ssl.CERT_REQUIRED
context.load_verify_locations(ca)
server.socket = context.wrap_socket(server.socket, server_side=True)
print(server.server_address[1], flush=True)
server.serve_forever()
"#;

/// Spawns the Fabric health stand-in over product-style HTTPS+mTLS: it
/// presents the CA-signed server certificate and requires the voie-cloud
/// client certificate against the same root. Returns the bound port.
async fn spawn_fabric_stub(
    dir: &std::path::Path,
    pems: &FabricPems,
    fixed_status: u16,
) -> (u16, tokio::process::Child) {
    let script_path = dir.join(format!("fabric_health_{}.py", fixed_status));
    std::fs::write(&script_path, FABRIC_STUB_SCRIPT).expect("fabric stub script writes");
    let mut child = tokio::process::Command::new("python3")
        .arg(&script_path)
        .arg("0")
        .arg(&pems.3)
        .arg(&pems.4)
        .arg(&pems.2)
        .arg(fixed_status.to_string())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("python3 runs the fabric health stub");
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
                let port: u16 = text.trim().parse().expect("stub prints its port");
                return (port, child);
            }
            _ if std::time::Instant::now() > deadline => {
                panic!("fabric health stub never reported its port");
            }
            _ => continue,
        }
    }
}

async fn bind_local() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stub listener binds");
    let port = listener.local_addr().expect("addr").port();
    (listener, port)
}

/// Blob stub that answers every authenticated GET with 404: an authorized
/// miss proves reachability exactly like the console-flow stub does.
async fn serve_blob_missing(listener: TcpListener, _objects: Arc<Mutex<HashMap<String, Vec<u8>>>>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        let io = hyper_util::rt::TokioIo::new(stream);
        tokio::spawn(async move {
            let service =
                hyper::service::service_fn(move |_request: Request<Incoming>| async move {
                    Ok::<_, std::convert::Infallible>(
                        Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body(Full::new(bytes::Bytes::new()))
                            .expect("constant response"),
                    )
                });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await;
        });
    }
}

/// Serves one fixed status for model probes; enough for reachability.
async fn serve_constant(listener: TcpListener, status: StatusCode) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        let io = hyper_util::rt::TokioIo::new(stream);
        tokio::spawn(async move {
            let service =
                hyper::service::service_fn(move |_request: Request<Incoming>| async move {
                    Ok::<_, std::convert::Infallible>(
                        Response::builder()
                            .status(status)
                            .body(Full::new(bytes::Bytes::new()))
                            .expect("constant response"),
                    )
                });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await;
        });
    }
}

async fn raw_request(addr: SocketAddr, request: &str) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr)
        .await
        .expect("server accepts connections");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("request writes");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("response reads");
    response
}

#[tokio::test]
async fn readiness_fails_closed_and_shutdown_drains() {
    let database_url = std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database");
    let kernel = Arc::new(
        Kernel::connect(&Config::database_url(&database_url))
            .await
            .expect("PostgreSQL connection succeeds"),
    );
    kernel.migrate().await.expect("migration succeeds");

    // --- dependency matrix ----------------------------------------------------
    let certs = temp_dir("certs");
    let (client_cert, client_key, ca_cert, _, _) = fabric_pem_files(&certs);
    set_env("VOIE_AZURE_BLOB_ACCOUNT", BLOB_ACCOUNT);
    set_env("VOIE_AZURE_BLOB_KEY", BLOB_KEY_BASE64);
    set_env("VOIE_AZURE_BLOB_CONTAINER", BLOB_CONTAINER);
    set_env("VOIE_MODEL_NAME", "readiness-model");
    set_env("VOIE_MODEL_API_KEY", "readiness-key");
    set_env("VOIE_FABRIC_CLIENT_CERT_PATH", &client_cert);
    set_env("VOIE_FABRIC_CLIENT_KEY_PATH", &client_key);
    set_env("VOIE_FABRIC_CA_CERT_PATH", &ca_cert);

    // Phase 1: every network dependency is unreachable.
    set_env("VOIE_AZURE_BLOB_ENDPOINT", &format!("{DEAD_ENDPOINT}"));
    set_env("VOIE_MODEL_BASE_URL", &format!("{DEAD_ENDPOINT}/v1"));
    set_env("VOIE_FABRIC_ENDPOINT", &format!("{DEAD_ENDPOINT}/"));
    let services = voie_cloud::integration::Services::from_env(kernel.pool().clone())
        .expect("configuration resolves even while endpoints are down");
    assert!(
        !services.dependencies_ready().await,
        "unreachable Blob, model, or Fabric must fail readiness closed"
    );

    // Phase 2: real local stubs answer every probe; the Fabric one over mTLS.
    let (blob_listener, blob_port) = bind_local().await;
    tokio::spawn(serve_blob_missing(
        blob_listener,
        Arc::new(Mutex::new(HashMap::new())),
    ));
    let (model_listener, model_port) = bind_local().await;
    tokio::spawn(serve_constant(model_listener, StatusCode::OK));
    let (healthy_fabric_port, _) = spawn_fabric_stub(
        &certs,
        &{
            let pems = fabric_pem_files(&certs);
            pems
        },
        StatusCode::OK.as_u16(),
    )
    .await;
    set_env(
        "VOIE_AZURE_BLOB_ENDPOINT",
        &format!("http://127.0.0.1:{blob_port}"),
    );
    set_env(
        "VOIE_MODEL_BASE_URL",
        &format!("http://127.0.0.1:{model_port}/v1"),
    );
    set_env(
        "VOIE_FABRIC_ENDPOINT",
        &format!("https://127.0.0.1:{healthy_fabric_port}/"),
    );

    // Activation artifacts: a provisioned entry plus any executable node
    // stand-in satisfies the launch prerequisites without starting a child.
    let artifact_dir = temp_dir("artifacts");
    let entry = artifact_dir.join("index.js");
    std::fs::write(&entry, "// stand-in entry\n").expect("entry writes");
    let fake_node = artifact_dir.join("fake-node.sh");
    std::fs::write(&fake_node, "#!/bin/sh\nexec /bin/true\n").expect("node writes");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_node, std::fs::Permissions::from_mode(0o755))
            .expect("node chmod");
    }
    set_env("VOIE_ACTIVATION_ENTRY", entry.to_str().expect("entry path"));
    set_env("VOIE_NODE", fake_node.to_str().expect("node path"));

    let services = voie_cloud::integration::Services::from_env(kernel.pool().clone())
        .expect("live configuration resolves");
    assert!(
        services.dependencies_ready().await,
        "reachable Blob, model, Fabric, and present artifacts are ready"
    );

    // Phase 3: the Fabric answering garbage fails readiness again.
    let (dead_fabric_port, _) = spawn_fabric_stub(&certs, &fabric_pems_for(&certs), 503).await;
    set_env(
        "VOIE_FABRIC_ENDPOINT",
        &format!("https://127.0.0.1:{dead_fabric_port}/"),
    );
    let services = voie_cloud::integration::Services::from_env(kernel.pool().clone())
        .expect("degraded configuration still resolves");
    assert!(
        !services.dependencies_ready().await,
        "an unhealthy Fabric fails readiness closed"
    );

    // Phase 3b: the model answering with an error status fails readiness,
    // even though the socket connected.
    let (unhealthy_model_listener, unhealthy_model_port) = bind_local().await;
    tokio::spawn(serve_constant(
        unhealthy_model_listener,
        StatusCode::TOO_MANY_REQUESTS,
    ));
    set_env(
        "VOIE_MODEL_BASE_URL",
        &format!("http://127.0.0.1:{unhealthy_model_port}/v1"),
    );
    let services = voie_cloud::integration::Services::from_env(kernel.pool().clone())
        .expect("degraded model configuration still resolves");
    assert!(
        !services.dependencies_ready().await,
        "a non-success model answer fails readiness closed"
    );
    set_env(
        "VOIE_MODEL_BASE_URL",
        &format!("http://[IP_ADDRESS]:{model_port}/v1"),
    );

    // Phase 4: a missing activation artifact fails readiness closed.
    set_env(
        "VOIE_FABRIC_ENDPOINT",
        &format!("https://127.0.0.1:{healthy_fabric_port}/"),
    );
    set_env("VOIE_ACTIVATION_ENTRY", "/nonexistent/voie/entry.js");
    let services = voie_cloud::integration::Services::from_env(kernel.pool().clone())
        .expect("artifact failure is runtime, not configuration");
    assert!(
        !services.dependencies_ready().await,
        "a missing activation child entry fails readiness closed"
    );

    // --- web artifact readiness -------------------------------------------------
    let web_root = temp_dir("web");
    assert!(
        !voie_cloud::web_assets_ready(&web_root).await,
        "an empty web root never reports ready"
    );
    std::fs::write(web_root.join("index.html"), "<html></html>").expect("index writes");
    assert!(
        voie_cloud::web_assets_ready(&web_root).await,
        "the required browser artifact makes the root servable"
    );

    // --- graceful shutdown drain --------------------------------------------------
    clear_env("VOIE_ACTIVATION_ENTRY");
    let (listener, port) = bind_local().await;
    let running = voie_cloud::serve_graceful(listener, kernel.clone());
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().expect("addr parses");

    let healthy = raw_request(
        addr,
        "GET /healthz HTTP/1.1\r\nhost: t\r\nconnection: close\r\n\r\n",
    )
    .await;
    assert!(
        healthy.starts_with(b"HTTP/1.1 200"),
        "the server answers before shutdown"
    );

    // Hold one keep-alive connection open across the signal: the drain owns
    // its termination even though the client never hangs up.
    let mut parked = TcpStream::connect(addr)
        .await
        .expect("kept-alive connection connects");
    parked
        .write_all(b"GET /healthz HTTP/1.1\r\nhost: t\r\n\r\n")
        .await
        .expect("pre-shutdown request writes");

    // The pre-shutdown request is fully answered before any signal exists.
    let mut first = [0u8; 128];
    let read = parked.read(&mut first).await.expect("response reads");
    assert!(
        first[..read].starts_with(b"HTTP/1.1 200"),
        "the pre-shutdown request is answered, got {:?}",
        String::from_utf8_lossy(&first[..read])
    );

    let drain = tokio::spawn(running.drain(std::time::Duration::from_secs(5)));

    // New work is refused promptly: the accept loop stops and drops the socket.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut refused = false;
    while std::time::Instant::now() < deadline {
        if TcpStream::connect(addr).await.is_err() {
            refused = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(refused, "drained servers stop accepting new connections");

    // ...and the drain terminates the parked keep-alive connection instead
    // of answering further requests or leaving it open past the grace.
    let _ = parked
        .write_all(b"GET /healthz HTTP/1.1\r\nhost: t\r\n\r\n")
        .await;
    let mut finished = Vec::new();
    parked
        .read_to_end(&mut finished)
        .await
        .expect("connection ends");
    assert!(
        !finished.starts_with(b"HTTP/1.1"),
        "post-signal work on a drained connection is refused, got {:?}",
        String::from_utf8_lossy(&finished)
    );

    drain
        .await
        .expect("drain task joins")
        .expect("bounded drain completes cleanly");
}

/// Regenerates deterministic per-directory material for additional stubs.
fn fabric_pems_for(dir: &std::path::Path) -> FabricPems {
    fabric_pem_files(dir)
}
