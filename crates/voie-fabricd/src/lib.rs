//! `voie-fabricd`: local SQLite facts, one block-backed Workspace, Firecracker execution.

mod fabric;
mod gateway_edge;
mod observe;
mod product;
mod product_realize;
mod realize;
mod reconcile;
mod routes;
mod specs;
mod storage;
mod store;

use std::convert::Infallible;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use serde::Deserialize;
use sha2::Digest;
use tokio::sync::Notify;
use uuid::Uuid;

use fabric::RestoreArchive;

pub(crate) type FabricBody = BoxBody<Bytes, std::io::Error>;
pub use fabric::{CleanupView, ExecView, Fabric, StartupReport, WorkspaceView};
pub use product::{
    JournalBody, is_product_path, reconcile_accepted_specs, reconcile_runtime_edge,
    reject_forbidden,
};
pub use product_realize::{
    APP_IMAGE, GATEWAY_IMAGE, POSTGRES_IMAGE, app_pod_yaml, app_service_yaml, gateway_pod_yaml,
    postgres_pod_yaml, verify_artifact_hash,
};
pub use realize::{
    ApprovedEgress, BlockSlot, ExecVerdict, Live, StartupRetargetAction, classify_exec,
    encrypted_mapper_device, ephemeral_devmapper_path, lv_name_for_deployment,
    lv_name_for_postgres, lv_name_for_release, lv_name_for_restore, require_stable_block_path,
    startup_retarget_action,
};
pub use reconcile::workspace_run::retry_held_workspace_releases;
pub use routes::{RouteIntent, render_map, render_route};
pub use storage::{
    CapacityReport, StoragePolicy, VolumeKind, admit_database_restore, admit_linear, admit_normal,
    admit_permanent_promotion, admit_workspace, admit_workspace_restore, k8s_quantity, lv_size_arg,
};
pub use store::{
    BeginDispatch, GenerationRow, ReservationRow, ResourceSpecRow, Store, WorkspaceRow,
};

/// A pre-TLS client must complete the mTLS handshake within this window;
/// slower peers are closed so half-open connections cannot accumulate.
pub(crate) const PRE_TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Upper bound on concurrent connections (handshake + service).
const MAX_CONNECTIONS: usize = 64;

#[derive(Debug)]
pub enum FabricError {
    Config(&'static str),
    Conflict(String),
    NotFound,
    Foreign(String),
    Unknown(String),
    Realize(String),
    Store(String),
}

impl fmt::Display for FabricError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FabricError::Config(message) => write!(f, "configuration: {message}"),
            FabricError::Conflict(message) => write!(f, "conflict: {message}"),
            FabricError::NotFound => write!(f, "not found"),
            FabricError::Foreign(message) => write!(f, "foreign object: {message}"),
            FabricError::Unknown(message) => write!(f, "unknown: {message}"),
            FabricError::Realize(message) => write!(f, "{message}"),
            FabricError::Store(message) => write!(f, "sqlite: {message}"),
        }
    }
}

impl Error for FabricError {}

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    pub sqlite: PathBuf,
    pub node_name: String,
    pub namespace: String,
    pub storage_class: String,
    pub runtime_class: String,
    /// The CRI handler the configured RuntimeClass must select. Pod
    /// admission only names the class, so the handler is what a workspace
    /// actually executes under; verifying it is what stops a same-named
    /// RuntimeClass backed by some other runtime from hosting guests.
    pub runtime_handler: String,
    pub runner_image: String,
    pub jailer_root: PathBuf,
    /// The declared local linear-LV pool. Workspace bytes may live nowhere
    /// else; there is no file- or loop-backed override because such a device
    /// hides whether durability is real.
    pub vg: String,
    pub storage: StoragePolicy,
    /// How long lifecycle teardown waits for runtime residue to disappear
    /// before deciding on the final observation. Deterministic tests shrink
    /// this; production keeps the full bound.
    pub residue_wait_secs: u64,
    /// How long workspace realization waits for the estate RuntimeClass to
    /// converge before pod admission proceeds. Deterministic tests shrink
    /// this; production keeps the full bound.
    pub runtime_class_wait_secs: u64,
    pub kubectl_program: String,
    pub kubectl_prefix: Vec<String>,
    pub kubeconfig: Option<PathBuf>,
    pub crictl_program: String,
    pub crictl_prefix: Vec<String>,
    pub tls_cert: PathBuf,
    pub tls_key: PathBuf,
    pub tls_ca: PathBuf,
    /// Deployment-approved guest egress destinations, when any exist. This
    /// feeds the one concrete NetworkPolicy; there is no policy language.
    pub approved_egress: Option<ApprovedEgress>,
    /// SHA-256 over DER of the one client certificate allowed to call this
    /// daemon: its control plane's identity. Any other CA-signed client is
    /// refused after the handshake; without the pin the daemon refuses to
    /// start rather than silently trust every CA client.
    pub client_sha256: String,
}

impl Config {
    pub fn from_env() -> Result<Self, FabricError> {
        // Fail closed: workspace bytes may only live in the declared local
        // linear-LV pool. The former per-device override was removed because
        // a file- or loop-backed device hides whether durability is real.
        if env_opt("VOIE_WORKSPACE_BLOCK_DEVICE").is_some() {
            return Err(FabricError::Config(
                "VOIE_WORKSPACE_BLOCK_DEVICE was removed; declare VOIE_WORKSPACE_VG instead",
            ));
        }
        let vg = env_opt("VOIE_WORKSPACE_VG").ok_or(FabricError::Config(
            "the declared workspace pool is required; set VOIE_WORKSPACE_VG",
        ))?;
        let (kubectl_program, kubectl_prefix) =
            split_command(env_opt("VOIE_KUBECTL").unwrap_or_else(|| "k3s kubectl".to_owned()));
        let (crictl_program, crictl_prefix) =
            split_command(env_opt("VOIE_CRICTL").unwrap_or_else(|| "k3s crictl".to_owned()));
        let kubeconfig = env_opt("VOIE_KUBECONFIG")
            .or_else(|| env_opt("KUBECONFIG"))
            .map(PathBuf::from);
        // Product transport is HTTPS with mutual TLS, without exception:
        // there is no plaintext fallback and no optional production mode.
        let (tls_cert, tls_key, tls_ca) = match (
            env_opt("VOIE_FABRIC_CERT"),
            env_opt("VOIE_FABRIC_KEY"),
            env_opt("VOIE_FABRIC_CA"),
        ) {
            (Some(cert), Some(key), Some(ca)) => {
                (PathBuf::from(cert), PathBuf::from(key), PathBuf::from(ca))
            }
            _ => {
                return Err(FabricError::Config(
                    "product mTLS is required; set VOIE_FABRIC_CERT, VOIE_FABRIC_KEY, and VOIE_FABRIC_CA",
                ));
            }
        };
        Ok(Config {
            bind: env_opt("VOIE_FABRICD_BIND").unwrap_or_else(|| "0.0.0.0:7840".to_owned()),
            sqlite: PathBuf::from(
                env_opt("VOIE_FABRICD_SQLITE")
                    .unwrap_or_else(|| "/var/lib/voie-fabricd/state.sqlite".to_owned()),
            ),
            node_name: env_opt("VOIE_NODE_NAME").unwrap_or_else(|| "baremetal-1".to_owned()),
            namespace: env_opt("VOIE_NAMESPACE").unwrap_or_else(|| "voie-workspace".to_owned()),
            storage_class: env_opt("VOIE_STORAGE_CLASS")
                .unwrap_or_else(|| "voie-workspace-block".to_owned()),
            runtime_class: env_opt("VOIE_RUNTIME_CLASS")
                .unwrap_or_else(|| "voie-firecracker".to_owned()),
            runtime_handler: env_opt("VOIE_RUNTIME_HANDLER")
                .unwrap_or_else(|| "kata-fc-rs-voie".to_owned()),
            runner_image: env_opt("VOIE_RUNNER_IMAGE")
                .unwrap_or_else(|| "voie-runner:c1".to_owned()),
            jailer_root: PathBuf::from(
                env_opt("VOIE_JAILER_ROOT")
                    .unwrap_or_else(|| "/run/kata-containers/shared/firecracker".to_owned()),
            ),
            vg,
            storage: StoragePolicy::from_env()?,
            residue_wait_secs: 120,
            runtime_class_wait_secs: 60,
            kubectl_program,
            kubectl_prefix,
            kubeconfig,
            crictl_program,
            crictl_prefix,
            tls_cert,
            tls_key,
            tls_ca,
            approved_egress: ApprovedEgress::parse(
                env_opt("VOIE_WORKSPACE_EGRESS_CIDRS"),
                env_opt("VOIE_WORKSPACE_EGRESS_PORT"),
            )?,
            client_sha256: parse_client_sha256(&env_opt("VOIE_FABRIC_CLIENT_SHA256").ok_or(
                FabricError::Config(
                    "the allowed control client identity is required; set VOIE_FABRIC_CLIENT_SHA256 to the SHA-256 fingerprint of the control client certificate",
                ),
            )?)?,
        })
    }

    /// Product mTLS acceptor; the daemon serves no other transport.
    pub fn tls_acceptor(&self) -> Result<tokio_rustls::TlsAcceptor, FabricError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let certs = load_certs(&self.tls_cert)?;
        let key = load_key(&self.tls_key)?;
        let mut roots = rustls::RootCertStore::empty();
        for ca in load_certs(&self.tls_ca)? {
            roots
                .add(ca)
                .map_err(|_| FabricError::Config("fabric CA PEM is unusable"))?;
        }
        let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .map_err(|_| FabricError::Config("fabric CA cannot verify clients"))?;
        let mut server = rustls::ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, key)
            .map_err(|_| FabricError::Config("fabric TLS certificate is unusable"))?;
        server.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(tokio_rustls::TlsAcceptor::from(Arc::new(server)))
    }
}

fn env_opt(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

/// Normalizes and validates the pinned control identity fingerprint: hex,
/// case-insensitive, optional `:` separators, exactly 32 bytes.
fn parse_client_sha256(raw: &str) -> Result<String, FabricError> {
    let normalized: String = raw
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|ch| *ch != ':')
        .collect();
    if normalized.len() != 64 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(FabricError::Config(
            "VOIE_FABRIC_CLIENT_SHA256 must be a 64-hex-character SHA-256 fingerprint",
        ));
    }
    Ok(normalized)
}

/// True only when the peer presented exactly the one certificate this
/// Fabric pins as its control identity. The CA verifier already proved the
/// chain; this check narrows "signed by our CA" to the single deployed
/// control certificate, so no other CA-signed key can call the API.
pub fn client_identity_matches(
    peer: Option<&[rustls::pki_types::CertificateDer<'_>]>,
    expected_sha256_hex: &str,
) -> bool {
    let Some(leaf) = peer.and_then(|certs| certs.first()) else {
        return false;
    };
    let digest: String = sha2::Sha256::digest(leaf.as_ref())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    match parse_client_sha256(expected_sha256_hex) {
        Ok(expected) => digest == expected,
        Err(_) => false,
    }
}

fn split_command(value: String) -> (String, Vec<String>) {
    let mut parts = value.split_whitespace();
    let program = parts.next().unwrap_or("k3s").to_owned();
    (program, parts.map(ToOwned::to_owned).collect())
}

#[derive(Debug, Deserialize)]
struct ExecBody {
    call_id: String,
    command: String,
}

#[derive(Debug, Deserialize)]
struct PackBody {
    operation_id: Uuid,
    request_hash: String,
    #[serde(default)]
    relative_root: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RestoreBody {
    operation_id: Uuid,
    request_hash: String,
    #[serde(default)]
    allocated_bytes: Option<u64>,
    #[serde(default)]
    elevated: Option<bool>,
}

struct WorkspaceRestoreHeaders {
    artifact_hash: String,
    operation_id: String,
    request_hash: String,
    allocated_bytes: Option<u64>,
    elevated: Option<bool>,
}

fn request_header<'a>(request: &'a Request<Incoming>, name: &str) -> Option<&'a str> {
    request
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
}

fn workspace_restore_headers(
    request: &Request<Incoming>,
) -> Result<WorkspaceRestoreHeaders, FabricError> {
    let artifact_hash = request_header(request, "x-voie-artifact-hash")
        .map(|value| value.to_ascii_lowercase())
        .ok_or(FabricError::Config(
            "restore artifact hash header is required",
        ))?;
    let operation_id = request_header(request, "x-voie-operation-id")
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or(FabricError::Config(
            "restore operation id header is required",
        ))?
        .to_string();
    let request_hash = request_header(request, "x-voie-request-hash")
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| artifact_hash.clone());
    let allocated_bytes =
        request_header(request, "x-voie-allocated-bytes").and_then(|value| value.parse().ok());
    let elevated = request_header(request, "x-voie-elevated").and_then(|value| value.parse().ok());
    Ok(WorkspaceRestoreHeaders {
        artifact_hash,
        operation_id,
        request_hash,
        allocated_bytes,
        elevated,
    })
}

#[derive(Debug, Deserialize)]
struct GrowBody {
    allocated_bytes: u64,
}

#[derive(Debug, Deserialize)]
struct GuestRunBody {
    operation_id: Uuid,
    request_hash: String,
    #[serde(default)]
    relative_root: Option<String>,
    run_argv: Vec<String>,
}

pub(crate) fn full_body(bytes: Bytes) -> FabricBody {
    Full::new(bytes)
        .map_err(|never: std::convert::Infallible| match never {})
        .boxed()
}

pub(crate) fn json_response(status: StatusCode, body: String) -> Response<FabricBody> {
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(full_body(Bytes::from(body)))
        .expect("response parts are valid")
}

pub(crate) fn error_response(error: FabricError) -> Response<FabricBody> {
    let (status, code) = match &error {
        FabricError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
        FabricError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
        FabricError::Foreign(_) => (StatusCode::CONFLICT, "foreign"),
        FabricError::Config(_) => (StatusCode::BAD_REQUEST, "invalid"),
        FabricError::Unknown(_) => (StatusCode::ACCEPTED, "unknown"),
        FabricError::Realize(_) | FabricError::Store(_) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "error")
        }
    };
    json_response(
        status,
        serde_json::json!({ "error": code, "message": error.to_string() }).to_string(),
    )
}

async fn read_json<T: serde::de::DeserializeOwned>(
    request: Request<Incoming>,
) -> Result<T, FabricError> {
    let bytes = request
        .into_body()
        .collect()
        .await
        .map_err(|_| FabricError::Config("request body is unreadable"))?
        .to_bytes();
    if bytes.is_empty() {
        return serde_json::from_slice(b"{}").map_err(|_| FabricError::Config("JSON is unusable"));
    }
    serde_json::from_slice(&bytes).map_err(|_| FabricError::Config("JSON is unusable"))
}

async fn put_workspace_spec(
    fabric: &Fabric,
    workspace_id: &str,
    request: Request<Incoming>,
) -> Response<FabricBody> {
    let spec: crate::specs::workspace::WorkspaceSpec = match read_json(request).await {
        Ok(spec) => spec,
        Err(error) => return error_response(error),
    };
    if spec.revision < 1 {
        return error_response(FabricError::Config("revision must be >= 1"));
    }
    if spec.desired == crate::specs::workspace::WorkspaceDesiredName::Active
        && spec.volume_bytes_for(fabric.live().storage()) == 0
    {
        return error_response(FabricError::Config(
            "storageTier or volumeBytes is required",
        ));
    }
    if let Err(error) =
        crate::reconcile::workspace_run::persist_workspace_spec_for(fabric, workspace_id, &spec)
    {
        return error_response(error);
    }
    match crate::reconcile::workspace_run::reconcile_workspace(fabric, workspace_id).await {
        Ok(status) => json_response(
            StatusCode::OK,
            serde_json::json!({
                "desiredRevision": status.desired_revision,
                "observedRevision": status.observed_revision,
                "state": status.observed_state,
                "desiredState": status.desired_state,
                "runtimeProfile": spec.runtime_profile,
                "lastErrorCode": status.last_error,
                "allocatedBytes": fabric
                    .get_allocation(crate::VolumeKind::Workspace, workspace_id)
                    .ok()
                    .flatten()
                    .map(|row| row.allocated_bytes),
            })
            .to_string(),
        ),
        Err(error) => error_response(error),
    }
}

async fn get_workspace_status(fabric: &Fabric, workspace_id: &str) -> Response<FabricBody> {
    // PUT may return WaitPod. GET is Control's 2s proof poll; when the
    // lifecycle lock is free, record convergence here so observedRevision
    // does not wait on the 15s loop or a sibling Workspace's exec/delete.
    let spec_status = match crate::reconcile::workspace_run::try_reconcile_workspace(
        fabric,
        workspace_id,
    )
    .await
    {
        Ok(Some(status)) => Some(status),
        Ok(None) | Err(_) => match fabric.store.get_resource_spec("workspace", workspace_id) {
            Ok(Some(row)) => Some(crate::reconcile::workspace_run::status_from_spec_row(&row)),
            Ok(None) => None,
            Err(error) => return error_response(error),
        },
    };
    match fabric.observe_workspace(workspace_id).await {
        Ok(view) => {
            let mut value = serde_json::to_value(&view).unwrap_or_else(|_| serde_json::json!({}));
            if view.state == "lost" {
                value["state"] = serde_json::json!("lost");
                value["lastErrorCode"] = serde_json::json!("durable_volume_missing");
                if let Some(status) = spec_status {
                    value["desiredRevision"] = serde_json::json!(status.desired_revision);
                    value["observedRevision"] = serde_json::json!(status.observed_revision);
                    value["desiredState"] = serde_json::json!(status.desired_state);
                }
            } else if let Some(status) = spec_status {
                let observed = if status.observed_state == "lost"
                    && (view.state == "ready" || view.state == "active")
                {
                    view.state.clone()
                } else {
                    status.observed_state
                };
                value["state"] = serde_json::json!(observed);
                value["desiredRevision"] = serde_json::json!(status.desired_revision);
                value["observedRevision"] = serde_json::json!(status.observed_revision);
                value["desiredState"] = serde_json::json!(status.desired_state);
                value["lastErrorCode"] = serde_json::json!(status.last_error);
            }
            json_response(StatusCode::OK, value.to_string())
        }
        Err(FabricError::NotFound) => {
            if let Some(status) = spec_status {
                return json_response(
                    StatusCode::OK,
                    serde_json::json!({
                        "id": workspace_id,
                        "state": status.observed_state,
                        "desiredRevision": status.desired_revision,
                        "observedRevision": status.observed_revision,
                        "desiredState": status.desired_state,
                        "lastErrorCode": status.last_error,
                    })
                    .to_string(),
                );
            }
            match fabric.materialized_workspace_missing_lv(workspace_id) {
                Ok(true) => json_response(
                    StatusCode::OK,
                    serde_json::json!({
                        "id": workspace_id,
                        "state": "lost",
                        "lastErrorCode": "durable_volume_missing",
                    })
                    .to_string(),
                ),
                Ok(false) => error_response(FabricError::NotFound),
                Err(error) => error_response(error),
            }
        }
        Err(error) => error_response(error),
    }
}

async fn put_route_map(fabric: &Fabric, request: Request<Incoming>) -> Response<FabricBody> {
    let spec: crate::specs::routes::RouteMapSpec = match read_json(request).await {
        Ok(spec) => spec,
        Err(error) => return error_response(error),
    };
    if spec.revision < 1 {
        return error_response(FabricError::Config("route revision must be >= 1"));
    }
    let existing = fabric
        .store
        .get_resource_spec("routes", "fabric")
        .ok()
        .flatten();
    let hash = spec.hash_bytes();
    // Control is the desired-map authority. A leftover higher Fabric
    // revision from journaled cutover must not ignore a new PUT.
    if existing
        .as_ref()
        .is_some_and(|row| row.spec_hash == hash && row.observed_revision >= spec.revision)
    {
        return json_response(
            StatusCode::OK,
            serde_json::json!({
                "desiredRevision": spec.revision,
                "observedRevision": spec.revision,
                "state": "ready",
            })
            .to_string(),
        );
    }
    let host = if spec.console_host.is_empty() {
        fabric
            .store
            .gateway_console_host()
            .ok()
            .flatten()
            .unwrap_or_default()
    } else {
        spec.console_host.clone()
    };
    let rows: Vec<(String, String, String, String)> = spec
        .routes
        .iter()
        .map(|entry| {
            (
                entry.slug.clone(),
                entry.kind.clone(),
                entry.service.clone(),
                host.clone(),
            )
        })
        .collect();
    if let Err(error) = fabric.replace_gateway_routes(&rows) {
        return error_response(error);
    }
    let typed = match serde_json::to_string(&spec) {
        Ok(typed) => typed,
        Err(_) => {
            return error_response(FabricError::Store("cannot encode route map".into()));
        }
    };
    if let Err(error) =
        fabric
            .store
            .upsert_resource_spec("routes", "fabric", spec.revision, &hash, &typed)
    {
        return error_response(error);
    }
    match crate::product::realize_gateway_routes(fabric).await {
        Ok(()) => {
            let _ = fabric.store.set_resource_spec_observed(
                "routes",
                "fabric",
                spec.revision,
                "ready",
                None,
            );
            json_response(
                StatusCode::OK,
                serde_json::json!({
                    "desiredRevision": spec.revision,
                    "observedRevision": spec.revision,
                    "state": "ready",
                })
                .to_string(),
            )
        }
        Err(error) => error_response(error),
    }
}

fn parse_workspace_id(id: &str) -> Result<Uuid, FabricError> {
    Uuid::parse_str(id).map_err(|_| FabricError::Config("workspace id is not a UUID"))
}

async fn handle(
    fabric: Arc<Fabric>,
    request: Request<Incoming>,
) -> Result<Response<FabricBody>, Infallible> {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let response = match (method, path.as_str()) {
        (Method::GET, "/healthz") => Response::builder()
            .status(StatusCode::OK)
            .header(hyper::header::CONTENT_TYPE, "text/plain")
            .body(full_body(Bytes::from_static(b"ok\n")))
            .expect("response parts are valid"),
        (Method::GET, "/v1/health") => json_response(
            StatusCode::OK,
            serde_json::json!({ "status": "ok" }).to_string(),
        ),
        (Method::GET, "/readyz") => match fabric.list_workspaces() {
            Ok(_) => Response::builder()
                .status(StatusCode::OK)
                .header(hyper::header::CONTENT_TYPE, "text/plain")
                .body(full_body(Bytes::from_static(b"ok\n")))
                .expect("response parts are valid"),
            Err(_) => Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .header(hyper::header::CONTENT_TYPE, "text/plain")
                .body(full_body(Bytes::from_static(b"not ready\n")))
                .expect("response parts are valid"),
        },
        (Method::GET, "/v1/capacity") => match fabric.capacity().await {
            Ok(report) => json_response(StatusCode::OK, serde_json::to_string(&report).unwrap()),
            Err(error) => error_response(error),
        },
        (Method::GET, "/v1/workspaces") => match fabric.list_workspaces() {
            Ok(views) => json_response(StatusCode::OK, serde_json::to_string(&views).unwrap()),
            Err(error) => error_response(error),
        },
        (Method::GET, "/v1/routes") => match fabric.list_gateway_routes() {
            Ok(items) => {
                let caddyfile = fabric.rendered_caddyfile().ok();
                json_response(
                    StatusCode::OK,
                    serde_json::json!({
                        "items": items.iter().map(|item| serde_json::json!({
                            "slug": item.slug,
                            "kind": item.kind,
                            "service": item.service,
                        })).collect::<Vec<_>>(),
                        "caddyfile": caddyfile,
                    })
                    .to_string(),
                )
            }
            Err(error) => error_response(error),
        },
        (Method::PUT, "/v1/routes") => put_route_map(&fabric, request).await,
        (Method::PUT, path) if path.starts_with("/v1/traffic/") => {
            let id = path.trim_start_matches("/v1/traffic/");
            product::put_traffic_spec(&fabric, id, request).await
        }
        (Method::GET, path) if path.starts_with("/v1/traffic/") => {
            let id = path.trim_start_matches("/v1/traffic/");
            product::get_traffic_spec(&fabric, id).await
        }
        (method, path) => {
            if product::is_product_path(path) {
                product::handle(&fabric, method, path, request).await
            } else {
                match parse_workspace_route(path) {
                    Some(Route::Workspace(id)) if method == Method::PUT => {
                        match parse_workspace_id(&id) {
                            Err(error) => error_response(error),
                            Ok(_) => put_workspace_spec(&fabric, &id, request).await,
                        }
                    }
                    Some(Route::Workspace(id)) if method == Method::GET => {
                        match parse_workspace_id(&id) {
                            Err(error) => error_response(error),
                            Ok(_) => get_workspace_status(&fabric, &id).await,
                        }
                    }
                    Some(Route::Workspace(id)) if method == Method::DELETE => {
                        match parse_workspace_id(&id) {
                            Err(error) => error_response(error),
                            Ok(_) => match fabric.delete_workspace(&id).await {
                                Ok(view) => json_response(
                                    StatusCode::OK,
                                    serde_json::to_string(&view).unwrap(),
                                ),
                                Err(error) => error_response(error),
                            },
                        }
                    }
                    Some(Route::Replace(id)) if method == Method::POST => {
                        match parse_workspace_id(&id) {
                            Err(error) => error_response(error),
                            Ok(_) => match fabric.replace(&id).await {
                                Ok(view) => json_response(
                                    StatusCode::OK,
                                    serde_json::to_string(&view).unwrap(),
                                ),
                                Err(error) => error_response(error),
                            },
                        }
                    }
                    Some(Route::Pack(id)) if method == Method::POST => {
                        match parse_workspace_id(&id) {
                            Err(error) => error_response(error),
                            Ok(_) => match read_json::<PackBody>(request).await {
                                Err(error) => error_response(error),
                                Ok(body) => {
                                    match fabric
                                        .pack_workspace(
                                            &id,
                                            &body.operation_id.to_string(),
                                            &body.request_hash,
                                            body.relative_root.as_deref().unwrap_or("."),
                                        )
                                        .await
                                    {
                                        Ok((pod, remote, hash)) => {
                                            crate::product::stream_workspace_pack(
                                                fabric.live().clone(),
                                                &pod,
                                                &remote,
                                                &hash,
                                            )
                                        }
                                        Err(error) => error_response(error),
                                    }
                                }
                            },
                        }
                    }
                    Some(Route::Pack(id)) if method == Method::DELETE => {
                        match parse_workspace_id(&id) {
                            Err(error) => error_response(error),
                            Ok(_) => match read_json::<PackBody>(request).await {
                                Err(error) => error_response(error),
                                Ok(body) => {
                                    match fabric
                                        .ack_workspace_pack(&id, &body.operation_id.to_string())
                                        .await
                                    {
                                        Ok(()) => json_response(
                                            StatusCode::OK,
                                            serde_json::json!({ "state": "acked" }).to_string(),
                                        ),
                                        Err(error) => error_response(error),
                                    }
                                }
                            },
                        }
                    }
                    Some(Route::Snapshot(id)) if method == Method::POST => {
                        match parse_workspace_id(&id) {
                            Err(error) => error_response(error),
                            Ok(_) => match read_json::<PackBody>(request).await {
                                Err(error) => error_response(error),
                                Ok(body) => {
                                    match fabric
                                        .snapshot_workspace(
                                            &id,
                                            &body.operation_id.to_string(),
                                            &body.request_hash,
                                        )
                                        .await
                                    {
                                        Ok((pod, _pack_hash)) => {
                                            crate::product::stream_workspace_snapshot(
                                                fabric.live().clone(),
                                                fabric.store.clone(),
                                                &id,
                                                &body.operation_id.to_string(),
                                                &pod,
                                            )
                                        }
                                        Err(error) => error_response(error),
                                    }
                                }
                            },
                        }
                    }
                    Some(Route::Snapshot(id)) if method == Method::DELETE => {
                        match parse_workspace_id(&id) {
                            Err(error) => error_response(error),
                            Ok(_) => match read_json::<PackBody>(request).await {
                                Err(error) => error_response(error),
                                Ok(body) => {
                                    match fabric
                                        .ack_workspace_snapshot(&id, &body.operation_id.to_string())
                                    {
                                        Ok(()) => json_response(
                                            StatusCode::OK,
                                            serde_json::json!({ "state": "acked" }).to_string(),
                                        ),
                                        Err(error) => error_response(error),
                                    }
                                }
                            },
                        }
                    }
                    Some(Route::RestoreArtifact(id)) if method == Method::PUT => {
                        match parse_workspace_id(&id) {
                            Err(error) => error_response(error),
                            Ok(_) => match workspace_restore_headers(&request) {
                                Err(error) => error_response(error),
                                Ok(headers) => {
                                    match fabric
                                        .restore_workspace(
                                            &id,
                                            &headers.operation_id,
                                            &headers.request_hash,
                                            headers.allocated_bytes,
                                            headers.elevated,
                                            Some(RestoreArchive::Body {
                                                body: request.into_body(),
                                                expected_hash: headers.artifact_hash.clone(),
                                            }),
                                        )
                                        .await
                                    {
                                        Ok(view) => json_response(
                                            StatusCode::CREATED,
                                            serde_json::to_string(&view).unwrap(),
                                        ),
                                        Err(error) => error_response(error),
                                    }
                                }
                            },
                        }
                    }
                    Some(Route::Restore(id)) if method == Method::POST => {
                        match parse_workspace_id(&id) {
                            Err(error) => error_response(error),
                            Ok(_) => match read_json::<RestoreBody>(request).await {
                                Err(error) => error_response(error),
                                Ok(body) => {
                                    match fabric
                                        .restore_workspace(
                                            &id,
                                            &body.operation_id.to_string(),
                                            &body.request_hash,
                                            body.allocated_bytes,
                                            body.elevated,
                                            None,
                                        )
                                        .await
                                    {
                                        Ok(view) => json_response(
                                            StatusCode::OK,
                                            serde_json::to_string(&view).unwrap(),
                                        ),
                                        Err(error) => error_response(error),
                                    }
                                }
                            },
                        }
                    }
                    Some(Route::Fence(id)) if method == Method::POST => {
                        match parse_workspace_id(&id) {
                            Err(error) => error_response(error),
                            Ok(_) => match fabric.fence_workspace(&id).await {
                                Ok(view) => json_response(
                                    StatusCode::OK,
                                    serde_json::to_string(&view).unwrap(),
                                ),
                                Err(error) => error_response(error),
                            },
                        }
                    }
                    Some(Route::Grow(id)) if method == Method::POST => {
                        match parse_workspace_id(&id) {
                            Err(error) => error_response(error),
                            Ok(_) => match read_json::<GrowBody>(request).await {
                                Err(error) => error_response(error),
                                Ok(body) => {
                                    match fabric.grow_workspace(&id, body.allocated_bytes).await {
                                        Ok(view) => json_response(
                                            StatusCode::OK,
                                            serde_json::to_string(&view).unwrap(),
                                        ),
                                        Err(error) => error_response(error),
                                    }
                                }
                            },
                        }
                    }
                    Some(Route::GuestRun(id)) if method == Method::POST => {
                        match parse_workspace_id(&id) {
                            Err(error) => error_response(error),
                            Ok(_) => match read_json::<GuestRunBody>(request).await {
                                Err(error) => error_response(error),
                                Ok(body) => {
                                    match fabric
                                        .guest_run(
                                            &id,
                                            &body.operation_id.to_string(),
                                            &body.request_hash,
                                            body.relative_root.as_deref().unwrap_or("."),
                                            &body.run_argv,
                                        )
                                        .await
                                    {
                                        Ok(exit_code) => json_response(
                                            StatusCode::OK,
                                            serde_json::json!({
                                                "state": if exit_code == 0 { "terminal" } else { "failed" },
                                                "exitCode": exit_code,
                                                "operationId": body.operation_id,
                                            })
                                            .to_string(),
                                        ),
                                        Err(error) => error_response(error),
                                    }
                                }
                            },
                        }
                    }
                    Some(Route::Exec { id, call_id: None }) if method == Method::POST => {
                        match parse_workspace_id(&id) {
                            Err(error) => error_response(error),
                            Ok(_) => match read_json::<ExecBody>(request).await {
                                Err(error) => error_response(error),
                                Ok(body) => {
                                    match fabric.exec(&id, &body.call_id, &body.command).await {
                                        Ok(view) => json_response(
                                            StatusCode::OK,
                                            serde_json::to_string(&view).unwrap(),
                                        ),
                                        Err(error) => error_response(error),
                                    }
                                }
                            },
                        }
                    }
                    Some(Route::Exec {
                        id,
                        call_id: Some(call_id),
                    }) if method == Method::GET => match parse_workspace_id(&id) {
                        Err(error) => error_response(error),
                        Ok(_) => match fabric.get_exec(&id, &call_id) {
                            Ok(view) => {
                                json_response(StatusCode::OK, serde_json::to_string(&view).unwrap())
                            }
                            Err(error) => error_response(error),
                        },
                    },
                    _ => json_response(
                        StatusCode::NOT_FOUND,
                        serde_json::json!({ "error": "not_found" }).to_string(),
                    ),
                }
            }
        }
    };
    Ok(response)
}

enum Route {
    Workspace(String),
    Exec { id: String, call_id: Option<String> },
    Replace(String),
    Pack(String),
    Snapshot(String),
    RestoreArtifact(String),
    Restore(String),
    GuestRun(String),
    Fence(String),
    Grow(String),
}

fn parse_workspace_route(path: &str) -> Option<Route> {
    let rest = path.strip_prefix("/v1/workspaces/")?;
    let mut parts = rest.split('/');
    let id = parts.next()?.to_owned();
    if id.is_empty() {
        return None;
    }
    match (parts.next(), parts.next(), parts.next()) {
        (None, None, None) => Some(Route::Workspace(id)),
        (Some("exec"), None, None) => Some(Route::Exec { id, call_id: None }),
        (Some("exec"), Some(call_id), None) if !call_id.is_empty() => Some(Route::Exec {
            id,
            call_id: Some(call_id.to_owned()),
        }),
        (Some("replace"), None, None) => Some(Route::Replace(id)),
        (Some("pack"), None, None) => Some(Route::Pack(id)),
        (Some("snapshot"), None, None) => Some(Route::Snapshot(id)),
        (Some("restore-artifact"), None, None) => Some(Route::RestoreArtifact(id)),
        (Some("restore"), None, None) => Some(Route::Restore(id)),
        (Some("guest-run"), None, None) => Some(Route::GuestRun(id)),
        (Some("fence"), None, None) => Some(Route::Fence(id)),
        (Some("grow"), None, None) => Some(Route::Grow(id)),
        _ => None,
    }
}

fn load_certs(path: &Path) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>, FabricError> {
    let file = std::fs::File::open(path)
        .map_err(|_| FabricError::Config("fabric TLS certificate is unreadable"))?;
    let mut reader = std::io::BufReader::new(file);
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| FabricError::Config("fabric TLS certificate PEM is unusable"))
}

fn load_key(path: &Path) -> Result<rustls::pki_types::PrivateKeyDer<'static>, FabricError> {
    let file = std::fs::File::open(path)
        .map_err(|_| FabricError::Config("fabric TLS key is unreadable"))?;
    let mut reader = std::io::BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)
        .map_err(|_| FabricError::Config("fabric TLS key PEM is unusable"))?
        .ok_or(FabricError::Config("fabric TLS key PEM is empty"))
}

/// Serves the private Fabric API over product mTLS.
///
/// Transport is unchanged product mTLS: the CA verifier still decides
/// certificate validity during the handshake. On top of it, every accepted
/// client must present exactly the certificate pinned in
/// `allowed_client_sha256` — this Fabric's control identity. Any other
/// CA-signed identity is closed immediately after the handshake.
///
/// A permit on `shutdown` stops new connections from being accepted;
/// `inflight` counts connections currently being served (including those
/// mid-handshake) so a supervisor can drain them bounded instead of
/// aborting mid-lifecycle. Admission is bounded: when `MAX_CONNECTIONS`
/// are already admitted, new TCP accepts are dropped immediately. Each
/// handshake is bounded by `PRE_TLS_HANDSHAKE_TIMEOUT`.
pub async fn serve_tls(
    listener: tokio::net::TcpListener,
    fabric: Arc<Fabric>,
    acceptor: tokio_rustls::TlsAcceptor,
    allowed_client_sha256: Arc<str>,
    shutdown: Arc<Notify>,
    inflight: Arc<AtomicUsize>,
) -> std::io::Result<()> {
    serve_tls_with(
        listener,
        fabric,
        acceptor,
        allowed_client_sha256,
        shutdown,
        inflight,
        PRE_TLS_HANDSHAKE_TIMEOUT,
        MAX_CONNECTIONS,
    )
    .await
}

async fn serve_tls_with(
    listener: tokio::net::TcpListener,
    fabric: Arc<Fabric>,
    acceptor: tokio_rustls::TlsAcceptor,
    allowed_client_sha256: Arc<str>,
    shutdown: Arc<Notify>,
    inflight: Arc<AtomicUsize>,
    handshake_timeout: Duration,
    max_connections: usize,
) -> std::io::Result<()> {
    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                if inflight.load(Ordering::SeqCst) >= max_connections {
                    eprintln!(
                        "voie-fabricd: connection refused: admission limit {max_connections} reached"
                    );
                    drop(stream);
                    continue;
                }
                // Reserve admission before spawning so the check and the
                // increment are atomic within this single accept task.
                inflight.fetch_add(1, Ordering::SeqCst);
                run_connection(
                    stream,
                    &fabric,
                    &acceptor,
                    &allowed_client_sha256,
                    inflight.clone(),
                    handshake_timeout,
                );
            }
        }
    }
    Ok(())
}

/// Accepts one TLS stream, gates it on the pinned control identity, and
/// serves HTTP until the connection ends. Spawned per connection.
fn run_connection(
    stream: tokio::net::TcpStream,
    fabric: &Arc<Fabric>,
    acceptor: &tokio_rustls::TlsAcceptor,
    allowed_client_sha256: &str,
    inflight: Arc<AtomicUsize>,
    handshake_timeout: Duration,
) {
    let acceptor = acceptor.clone();
    let fabric = fabric.clone();
    let allowed_client_sha256 = allowed_client_sha256.to_owned();
    let inflight_counter = inflight;
    tokio::spawn(async move {
        let tls = match tokio::time::timeout(handshake_timeout, acceptor.accept(stream)).await {
            Ok(Ok(tls)) => tls,
            Ok(Err(error)) => {
                eprintln!("voie-fabricd: tls handshake error: {error}");
                inflight_counter.fetch_sub(1, Ordering::SeqCst);
                return;
            }
            Err(_) => {
                eprintln!(
                    "voie-fabricd: closed pre-TLS connection: handshake exceeded {handshake_timeout:?}"
                );
                inflight_counter.fetch_sub(1, Ordering::SeqCst);
                return;
            }
        };
        if !client_identity_matches(tls.get_ref().1.peer_certificates(), &allowed_client_sha256) {
            eprintln!("voie-fabricd: rejected client: not this Fabric's pinned control identity");
        } else {
            let io = hyper_util::rt::TokioIo::new(tls);
            let service =
                hyper::service::service_fn(move |request| handle(fabric.clone(), request));
            if let Err(error) = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await
            {
                eprintln!("voie-fabricd: connection error: {error}");
            }
        }
        inflight_counter.fetch_sub(1, Ordering::SeqCst);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_routes_parse_private_surface() {
        assert!(matches!(
            parse_workspace_route("/v1/workspaces/abc"),
            Some(Route::Workspace(id)) if id == "abc"
        ));
        assert!(matches!(
            parse_workspace_route("/v1/workspaces/abc/exec"),
            Some(Route::Exec { call_id: None, .. })
        ));
        assert!(matches!(
            parse_workspace_route("/v1/workspaces/abc/exec/c1"),
            Some(Route::Exec { call_id: Some(call), .. }) if call == "c1"
        ));
        assert!(matches!(
            parse_workspace_route("/v1/workspaces/abc/replace"),
            Some(Route::Replace(id)) if id == "abc"
        ));
        assert!(matches!(
            parse_workspace_route("/v1/workspaces/abc/pack"),
            Some(Route::Pack(id)) if id == "abc"
        ));
        assert!(matches!(
            parse_workspace_route("/v1/workspaces/abc/snapshot"),
            Some(Route::Snapshot(id)) if id == "abc"
        ));
        assert!(matches!(
            parse_workspace_route("/v1/workspaces/abc/restore-artifact"),
            Some(Route::RestoreArtifact(id)) if id == "abc"
        ));
        assert!(matches!(
            parse_workspace_route("/v1/workspaces/abc/restore"),
            Some(Route::Restore(id)) if id == "abc"
        ));
        assert!(matches!(
            parse_workspace_route("/v1/workspaces/abc/guest-run"),
            Some(Route::GuestRun(id)) if id == "abc"
        ));
        assert!(matches!(
            parse_workspace_route("/v1/workspaces/abc/fence"),
            Some(Route::Fence(id)) if id == "abc"
        ));
        assert!(matches!(
            parse_workspace_route("/v1/workspaces/abc/grow"),
            Some(Route::Grow(id)) if id == "abc"
        ));
        assert!(parse_workspace_route("/v1/workspaces/abc/exec/c1/extra").is_none());
        assert!(parse_workspace_route("/v1/health").is_none());
    }

    #[test]
    fn desired_policy_spec_denies_ingress_and_constrains_egress() {
        let live = Live::from_config(&test_config("spec-deny")).unwrap();
        let spec = live.desired_network_policy_spec();
        assert_eq!(spec, live.desired_network_policy_spec());
        assert_eq!(
            spec["policyTypes"],
            serde_json::json!(["Ingress", "Egress"])
        );
        assert_eq!(
            spec["podSelector"],
            serde_json::json!({ "matchLabels": { "io.voie/kind": "workspace" } })
        );
        // Default-deny ingress: an empty ingress list admits nothing.
        assert_eq!(spec["ingress"], serde_json::json!([]));
        assert_eq!(spec["egress"].as_array().map(Vec::len), Some(1));
        assert_eq!(spec["egress"][0]["ports"][0]["port"], 53);
    }

    #[test]
    fn desired_policy_spec_admits_only_approved_cidr_port() {
        let mut config = test_config("spec-approved");
        config.approved_egress = Some(ApprovedEgress {
            cidrs: vec!["203.0.113.0/24".into(), "198.51.100.7/32".into()],
            tcp_port: 443,
        });
        let live = Live::from_config(&config).unwrap();
        let spec = live.desired_network_policy_spec();
        let egress = spec["egress"].as_array().unwrap();
        assert_eq!(egress.len(), 2);
        assert_eq!(
            egress[1]["to"],
            serde_json::json!([
                { "ipBlock": { "cidr": "203.0.113.0/24" } },
                { "ipBlock": { "cidr": "198.51.100.7/32" } }
            ])
        );
        assert_eq!(
            egress[1]["ports"],
            serde_json::json!([{ "protocol": "TCP", "port": 443 }])
        );
        // The rendered manifest and the compared spec agree on shape.
        let yaml = live.network_policy_yaml();
        assert!(yaml.contains("policyTypes:\n    - Ingress\n    - Egress\n"));
        assert!(yaml.contains("  ingress: []\n"));
        assert!(yaml.contains("cidr: 203.0.113.0/24"));
        assert!(yaml.contains("port: 443"));
    }

    /// Minimal offline configuration; every field is explicit so tests never
    /// depend on host environment.
    fn test_config(tag: &str) -> Config {
        Config {
            bind: "127.0.0.1:0".into(),
            sqlite: std::env::temp_dir().join(format!("voie-fabricd-lib-{tag}.sqlite")),
            node_name: "node-under-test".into(),
            namespace: "voie-workspace".into(),
            storage_class: "voie-workspace-block".into(),
            runtime_class: "voie-firecracker".into(),
            runtime_handler: "kata-fc-rs-voie".into(),
            runner_image: "voie-runner:c1".into(),
            jailer_root: std::env::temp_dir().join(format!("voie-fabricd-jails-{tag}")),
            vg: "voie-ws".into(),
            storage: StoragePolicy::test(),
            residue_wait_secs: 120,
            runtime_class_wait_secs: 120,
            kubectl_program: "kubectl".into(),
            kubectl_prefix: vec![],
            kubeconfig: None,
            crictl_program: "crictl".into(),
            crictl_prefix: vec![],
            tls_cert: PathBuf::from("/dev/null"),
            tls_key: PathBuf::from("/dev/null"),
            tls_ca: PathBuf::from("/dev/null"),
            approved_egress: None,
            client_sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .into(),
        }
    }
    const TEST_CERT_PEM: &str = concat!(
        "-----BEGIN CERTIFICATE-----\n",
        "MIIBnjCCAUSgAwIBAgIUOvC1IJf5xbIrCx80jEHd6dJ/rKkwCgYIKoZIzj0EAwIw\n",
        "HDEaMBgGA1UEAwwRdm9pZS1mYWJyaWNkLXRlc3QwHhcNMjYwODI2MTM1ODUyWhcN\n",
        "MjYwOTAyMTM1ODUyWjAcMRowGAYDVQQDDBF2b2llLWZhYnJpY2QtdGVzdDBZMBMG\n",
        "ByqGSM49AgEGCCqGSM49AwEHA0IABAHJvIz/iyMePHOMRVdBUqCqnQkfkwFSwuJS\n",
        "LZc7vOxYpYUJJz1H6udegyxYPjeTRk8ziUQk74hcRuOKr+iDtZGjZDBiMB0GA1Ud\n",
        "DgQWBBRyp4x8lSJslPqi5UtvXCZGdgRgvzAfBgNVHSMEGDAWgBRyp4x8lSJslPqi\n",
        "5UtvXCZGdgRgvzAPBgNVHRMBAf8EBTADAQH/MA8GA1UdEQQIMAaHBH8AAAEwCgYI\n",
        "KoZIzj0EAwIDSAAwRQIgB8QZB66pNOOUGqol/XQiRq8Z/Ud5MpW45pvDhtXvbH4C\n",
        "IQDtTxksTrsC09/vCHdq6ugmTdhKkTK581jtRd5pPAMiNw==\n",
        "-----END CERTIFICATE-----\n",
    );
    const TEST_KEY_PEM: &str = concat!(
        "-----BEGIN EC PRIVATE KEY-----\n",
        "MHcCAQEEIElTnQACzWzAtcrOF386y9gvBTVXcY9qo/71ahxxQEcfoAoGCCqGSM49\n",
        "AwEHoUQDQgAEAcm8jP+LIx48c4xFV0FSoKqdCR+TAVLC4lItlzu87FilhQknPUfq\n",
        "516DLFg+N5NGTzOJRCTviFxG44qv6IO1kQ==\n",
        "-----END EC PRIVATE KEY-----\n",
    );

    fn test_acceptor() -> tokio_rustls::TlsAcceptor {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let certs: Vec<rustls::pki_types::CertificateDer<'static>> =
            rustls_pemfile::certs(&mut std::io::BufReader::new(TEST_CERT_PEM.as_bytes()))
                .collect::<Result<Vec<_>, _>>()
                .expect("test cert parses")
                .into_iter()
                .map(|c| c.to_owned())
                .collect();
        let key =
            rustls_pemfile::private_key(&mut std::io::BufReader::new(TEST_KEY_PEM.as_bytes()))
                .expect("test key parses")
                .expect("test key present");
        let mut roots = rustls::RootCertStore::empty();
        for ca in rustls_pemfile::certs(&mut std::io::BufReader::new(TEST_CERT_PEM.as_bytes())) {
            roots
                .add(ca.expect("ca cert parses").to_owned())
                .expect("ca cert inserts");
        }
        let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .expect("verifier builds");
        let mut server = rustls::ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, key)
            .expect("server cert builds");
        server.alpn_protocols = vec![b"http/1.1".to_vec()];
        tokio_rustls::TlsAcceptor::from(Arc::new(server))
    }

    #[tokio::test]
    async fn pre_tls_handshake_times_out_silent_peer() {
        use tokio::io::AsyncReadExt as _;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener binds");
        let addr = listener.local_addr().expect("addr");
        let acceptor = test_acceptor();
        let config = test_config("pre-tls-timeout");
        let live = Live::from_config(&config).expect("live from test config");
        let fabric = Arc::new(Fabric::open(config, live).expect("fabric opens"));
        let shutdown = Arc::new(Notify::new());
        let inflight = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn(serve_tls_with(
            listener,
            fabric,
            acceptor,
            Arc::from("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
            shutdown.clone(),
            inflight.clone(),
            Duration::from_millis(200),
            64,
        ));
        tokio::time::sleep(Duration::from_millis(10)).await;
        let mut client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("client connects");
        let mut buf = [0u8; 1];
        let result = tokio::time::timeout(Duration::from_millis(800), client.read(&mut buf)).await;
        assert!(
            matches!(result, Ok(Ok(0))),
            "silent peer should be closed after handshake timeout, got {result:?}"
        );
        assert_eq!(
            inflight.load(Ordering::SeqCst),
            0,
            "inflight must be drained after timeout"
        );
        shutdown.notify_one();
        let _ = tokio::time::timeout(Duration::from_millis(200), server).await;
    }

    #[tokio::test]
    async fn admission_limit_drops_new_connection_immediately() {
        use tokio::io::AsyncReadExt as _;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener binds");
        let addr = listener.local_addr().expect("addr");
        let acceptor = test_acceptor();
        let config = test_config("admission-drop");
        let live = Live::from_config(&config).expect("live from test config");
        let fabric = Arc::new(Fabric::open(config, live).expect("fabric opens"));
        let shutdown = Arc::new(Notify::new());
        let inflight = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn(serve_tls_with(
            listener,
            fabric,
            acceptor,
            Arc::from("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
            shutdown.clone(),
            inflight.clone(),
            Duration::from_millis(600),
            1,
        ));
        tokio::time::sleep(Duration::from_millis(10)).await;
        let mut a = tokio::net::TcpStream::connect(addr)
            .await
            .expect("first client connects");
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(inflight.load(Ordering::SeqCst), 1);
        let mut b = tokio::net::TcpStream::connect(addr)
            .await
            .expect("second client connects");
        let mut buf = [0u8; 1];
        let b_result = tokio::time::timeout(Duration::from_millis(250), b.read(&mut buf)).await;
        assert!(
            matches!(b_result, Ok(Ok(0))),
            "second connection should be refused immediately when at capacity, got {b_result:?}"
        );
        assert_eq!(inflight.load(Ordering::SeqCst), 1);
        let mut buf_a = [0u8; 1];
        let a_still_open =
            tokio::time::timeout(Duration::from_millis(80), a.read(&mut buf_a)).await;
        assert!(
            matches!(a_still_open, Err(_)),
            "first connection should remain open until its handshake timeout, got {a_still_open:?}"
        );
        let a_closed = tokio::time::timeout(Duration::from_millis(900), a.read(&mut buf_a)).await;
        assert!(
            matches!(a_closed, Ok(Ok(0))),
            "first connection should close after handshake timeout, got {a_closed:?}"
        );
        assert_eq!(inflight.load(Ordering::SeqCst), 0);
        shutdown.notify_one();
        let _ = tokio::time::timeout(Duration::from_millis(200), server).await;
    }
}
