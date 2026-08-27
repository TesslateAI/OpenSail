//! Regression contract for `/api/audit-events`: JSONB audit metadata is
//! decoded as typed JSON — never as a string — and serialized safely through
//! the console API. The row round-trips through the real SQL contract
//! (`insert_audit` plus the live HTTP route) against a real PostgreSQL.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::{CONTENT_TYPE, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;
use voie_cloud::auth::{Auth, AuthConfig};
use voie_cloud::web_session::{self, COOKIE_NAME};
use voie_cloud::{AuditInsert, AuditOutcome, Config, Kernel, insert_audit, serve_with_services};

fn database_url() -> String {
    std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database")
}

/// Process-global service configuration consumed by `Services::from_env`.
fn set_env(name: &str, value: &str) {
    unsafe { std::env::set_var(name, value) };
}

struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(label: &str) -> TempDir {
    let dir = std::env::temp_dir().join(format!("voie-audit-{label}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("temp dir creates");
    TempDir(dir)
}

/// Throwaway mTLS material; `FabricClient::from_env` loads it eagerly even
/// though this test never calls Fabric.
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
        "/CN=voie-audit-test-ca",
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
        "/CN=voie-audit-test-client",
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
    let str_path = |path: &std::path::Path| path.to_str().expect("pem path").to_string();
    (
        str_path(&client_pem),
        str_path(&client_key),
        str_path(&ca_pem),
    )
}

/// Discovery-only OIDC issuer: `Auth::connect` needs provider metadata and a
/// JWKS document; no token exchange ever happens because the Web session is
/// minted directly.
async fn serve_discovery(listener: TcpListener, issuer_url: String) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        let issuer_url = issuer_url.clone();
        tokio::spawn(async move {
            let io = hyper_util::rt::TokioIo::new(stream);
            let service = hyper::service::service_fn(move |request: Request<Incoming>| {
                let issuer_url = issuer_url.clone();
                async move {
                    let body = match (request.method(), request.uri().path()) {
                        (&Method::GET, "/.well-known/openid-configuration") => json!({
                            "issuer": issuer_url,
                            "authorization_endpoint": format!("{issuer_url}/authorize"),
                            "token_endpoint": format!("{issuer_url}/token"),
                            "jwks_uri": format!("{issuer_url}/jwks"),
                            "response_types_supported": ["code"],
                            "subject_types_supported": ["public"],
                            "id_token_signing_alg_values_supported": ["RS256"],
                            "token_endpoint_auth_methods_supported": ["client_secret_post"],
                        })
                        .to_string(),
                        (_, "/jwks") => json!({ "keys": [] }).to_string(),
                        _ => String::new(),
                    };
                    let response = Response::builder()
                        .status(if body.is_empty() {
                            StatusCode::NOT_FOUND
                        } else {
                            StatusCode::OK
                        })
                        .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                        .body(Full::new(bytes::Bytes::from(body)))
                        .expect("static response");
                    Ok::<_, Infallible>(response)
                }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await;
        });
    }
}

async fn get_json(port: u16, path: &str, session_token: &str) -> (u16, Value) {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("API listener accepts connections");
    let request = format!(
        "GET {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\n\
         cookie: {COOKIE_NAME}={session_token}\r\nconnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("request writes");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("response reads");
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n").expect("HTTP header terminator");
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .expect("status line")
        .parse()
        .expect("numeric status");
    let value = serde_json::from_str(body.trim_start_matches('\u{0}').trim())
        .expect("JSON response body parses");
    (status, value)
}

#[tokio::test]
async fn audit_events_route_decodes_jsonb_metadata_as_typed_json() {
    let kernel = Arc::new(
        Kernel::connect(&Config::database_url(database_url()))
            .await
            .expect("PostgreSQL connection succeeds"),
    );
    kernel.migrate().await.expect("migration succeeds");

    // Own project with one member row; assertions only touch rows this test
    // inserted so parallel suites sharing the database stay isolated.
    let owner = Uuid::new_v4();
    sqlx::query("insert into users (id, issuer, subject) values ($1, $2, $3)")
        .bind(owner)
        .bind("audit-route-test-issuer")
        .bind(owner.to_string())
        .execute(kernel.pool())
        .await
        .expect("user inserts");
    let project_id = Uuid::new_v4();
    kernel
        .create_project(project_id, owner, &format!("audit-{owner}"), "personal")
        .await
        .expect("project creates");

    // Structured metadata through the public insert contract.
    let metadata = json!({
        "role": "owner",
        "previousRole": null,
        "nested": { "answer": 42, "tags": ["a", "b"] },
    });
    let event = insert_audit(
        kernel.pool(),
        &AuditInsert {
            project_id: Some(project_id),
            session_id: None,
            run_id: None,
            actor_user_id: Some(owner),
            kind: "audit.decode.typed",
            resource_type: "test",
            resource_id: Some(owner),
            outcome: AuditOutcome::Ok,
            metadata: Some(&metadata),
        },
    )
    .await
    .expect("typed audit row inserts");
    assert_eq!(
        event.metadata,
        Some(metadata.clone()),
        "the returned AuditEvent carries typed JSONB, not a string"
    );

    // Legacy shape: NULL metadata and free-form payload text.
    sqlx::query(
        "insert into audit_events \
         (project_id, actor_user_id, kind, resource_type, resource_id, outcome, metadata, payload) \
         values ($1, $2, 'audit.decode.null', 'test', $3, 'ok', NULL, 'legacy text')",
    )
    .bind(project_id)
    .bind(owner)
    .bind(owner)
    .execute(kernel.pool())
    .await
    .expect("null-metadata audit row inserts");

    // Local-only service configuration; no network dependency is contacted.
    set_env("VOIE_AZURE_BLOB_ACCOUNT", "voie-audit-test-account");
    set_env(
        "VOIE_AZURE_BLOB_KEY",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    );
    set_env("VOIE_AZURE_BLOB_CONTAINER", "voie-audit-test-container");
    set_env(
        "VOIE_AZURE_BLOB_ENDPOINT",
        "http://127.0.0.1:9/unreached-blob",
    );
    set_env("VOIE_MODEL_BASE_URL", "http://127.0.0.1:9/v1");
    set_env("VOIE_MODEL_NAME", "audit-model");
    set_env("VOIE_MODEL_API_KEY", "audit-key");
    set_env("VOIE_FABRIC_ENDPOINT", "https://127.0.0.1:9/");
    let certs = temp_dir("certs");
    let (client_cert, client_key, ca_cert) = fabric_pem_files(certs.0.as_path());
    set_env("VOIE_FABRIC_CLIENT_CERT_PATH", &client_cert);
    set_env("VOIE_FABRIC_CLIENT_KEY_PATH", &client_key);
    set_env("VOIE_FABRIC_CA_CERT_PATH", &ca_cert);

    let services = voie_cloud::integration::Services::from_env(kernel.pool().clone())
        .expect("local service configuration resolves");

    let issuer_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("issuer binds");
    let issuer_port = issuer_listener.local_addr().expect("issuer addr").port();
    let issuer_url = format!("http://127.0.0.1:{issuer_port}");
    tokio::spawn(serve_discovery(issuer_listener, issuer_url.clone()));
    let auth_listener = TcpListener::bind("127.0.0.1:0").await.expect("auth binds");
    let port = auth_listener.local_addr().expect("auth addr").port();
    let auth = Auth::connect(
        AuthConfig::new(
            issuer_url,
            "voie-audit-test".to_string(),
            "voie-audit-test-secret".to_string(),
            format!("http://127.0.0.1:{port}/oidc/callback"),
            format!("http://127.0.0.1:{port}"),
        ),
        kernel.pool().clone(),
    )
    .await
    .expect("OIDC discovery succeeds");
    tokio::spawn(serve_with_services(
        auth_listener,
        kernel.clone(),
        Arc::new(auth),
        services,
    ));

    // Mint the Web session directly; login itself is covered elsewhere.
    let session_token = web_session::create(kernel.pool(), owner, Duration::from_secs(300))
        .await
        .expect("web session creates")
        .1;

    let (status, body) = get_json(port, "/api/audit-events", &session_token).await;
    assert_eq!(status, 200, "the audit route answers without panicking");
    let items = body
        .get("items")
        .and_then(Value::as_array)
        .expect("items array");
    let typed = items
        .iter()
        .find(|item| item.get("kind") == Some(&json!("audit.decode.typed")))
        .unwrap_or_else(|| panic!("typed row listed in {items:#?}"));
    assert_eq!(
        typed.get("metadata"),
        Some(&metadata),
        "JSONB metadata arrives as typed JSON, not an encoded string"
    );
    assert!(
        typed.get("metadata").and_then(Value::as_object).is_some(),
        "metadata stays an object on the wire"
    );
    assert_eq!(typed.get("seq"), Some(&json!(event.seq)));
    assert_eq!(typed.get("actorUserId"), Some(&json!(owner)));

    let legacy = items
        .iter()
        .find(|item| item.get("kind") == Some(&json!("audit.decode.null")))
        .unwrap_or_else(|| panic!("legacy row listed in {items:#?}"));
    assert_eq!(
        legacy.get("metadata"),
        Some(&Value::Null),
        "a missing JSONB metadata serializes as null"
    );
    assert_eq!(
        legacy.get("payload"),
        Some(&json!("legacy text")),
        "free-form payload text passes through unchanged"
    );
}
