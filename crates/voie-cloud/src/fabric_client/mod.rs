//! One configured Fabric endpoint over product mTLS.

use std::error::Error;
use std::fmt;
use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::{Certificate, Client, Identity};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub enum FabricError {
    Config(&'static str),
    Transport,
    Response,
    OutcomeUnknown,
}

impl fmt::Display for FabricError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FabricError::Config(message) => write!(f, "configuration: {message}"),
            FabricError::Transport => write!(f, "fabric transport failed"),
            FabricError::Response => write!(f, "fabric response was unusable"),
            FabricError::OutcomeUnknown => write!(f, "fabric outcome unknown"),
        }
    }
}

impl Error for FabricError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecResult {
    pub call_id: String,
    pub state: String,
    pub exit_code: Option<i32>,
    /// Lossy-UTF8 guest output through the runner shell.
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

impl ExecResult {
    /// The program's own exit status was observed (any code, including
    /// nonzero); the authoritative verdict of the one dispatch attempt.
    pub fn is_completed(&self) -> bool {
        self.state == "terminal"
    }

    /// The attempt never resolved or died at the guest runner's own
    /// deadline; durably recorded once and never re-attempted.
    pub fn is_outcome_unknown(&self) -> bool {
        self.state == "unknown"
    }

    pub fn payload(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| self.state.clone())
    }
}

/// Classified outcome of one workspace-create dispatch.
///
/// Fabricd maps its own `FabricError::Unknown` to HTTP 202 (Accepted):
/// the create may or may not have taken effect. Control treats that as
/// indeterminate — never a success — and keeps a `creating` reservation.
/// Only HTTP 200 is truthful success. Every other status (including any
/// other 2xx) is an unusable response; transport failures are `Transport`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateOutcome {
    /// Fabric confirmed it holds the Workspace (HTTP 200).
    Created,
    /// Fabric returned its Unknown verdict (HTTP 202); the create is
    /// indeterminate and must not be exposed as ready. It stays
    /// reconcilable via a read-only existence probe.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceProbe {
    pub state: String,
    pub allocated_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductOutcome {
    pub state: String,
    #[serde(default, rename = "resourceId")]
    pub resource_id: String,
    #[serde(default, rename = "operationId")]
    pub operation_id: Option<Uuid>,
}

/// Product mTLS client. Trusts only the configured Fabric CA.
#[derive(Clone)]
pub struct FabricClient {
    http: Client,
    endpoint: String,
    /// Contract-test stub: guest image is Profile 1, every effect is
    /// transport-failed. Production never sets `VOIE_PRODUCT_RUNTIME=stub`.
    contract_stub: bool,
}

impl FabricClient {
    /// `VOIE_FABRIC_ENDPOINT`, `VOIE_FABRIC_CLIENT_CERT_PATH`,
    /// `VOIE_FABRIC_CLIENT_KEY_PATH`, `VOIE_FABRIC_CA_CERT_PATH`.
    pub fn from_env() -> Result<Self, FabricError> {
        if std::env::var("VOIE_PRODUCT_RUNTIME")
            .map(|value| value.trim() == "stub")
            .unwrap_or(false)
        {
            return Ok(Self::contract_stub());
        }
        let endpoint = require_env("VOIE_FABRIC_ENDPOINT")?;
        let cert_path = require_env("VOIE_FABRIC_CLIENT_CERT_PATH")?;
        let key_path = require_env("VOIE_FABRIC_CLIENT_KEY_PATH")?;
        let ca_path = require_env("VOIE_FABRIC_CA_CERT_PATH")?;
        Self::from_pem_files(endpoint, cert_path, key_path, ca_path)
    }

    fn contract_stub() -> Self {
        FabricClient {
            http: Client::new(),
            endpoint: String::new(),
            contract_stub: true,
        }
    }

    fn refuse_stub(&self) -> Result<(), FabricError> {
        if self.contract_stub {
            Err(FabricError::Transport)
        } else {
            Ok(())
        }
    }

    pub fn from_pem_files(
        endpoint: String,
        cert_path: impl AsRef<Path>,
        key_path: impl AsRef<Path>,
        ca_path: impl AsRef<Path>,
    ) -> Result<Self, FabricError> {
        let cert = std::fs::read(cert_path.as_ref())
            .map_err(|_| FabricError::Config("client certificate is unreadable"))?;
        let key = std::fs::read(key_path.as_ref())
            .map_err(|_| FabricError::Config("client key is unreadable"))?;
        let ca = std::fs::read(ca_path.as_ref())
            .map_err(|_| FabricError::Config("fabric CA is unreadable"))?;
        let mut pem = cert;
        pem.extend_from_slice(&key);
        let identity = Identity::from_pem(&pem)
            .map_err(|_| FabricError::Config("client certificate PEM is unusable"))?;
        let ca = Certificate::from_pem(&ca)
            .map_err(|_| FabricError::Config("fabric CA PEM is unusable"))?;
        let http = Client::builder()
            // Product Fabric transport is always HTTPS with mTLS, including
            // the local development stack.
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(ca)
            .identity(identity)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| FabricError::Transport)?;
        Ok(FabricClient {
            http,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            contract_stub: false,
        })
    }

    pub async fn health(&self) -> Result<(), FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .get(format!("{}/v1/health", self.endpoint))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(FabricError::Response)
        }
    }

    pub async fn capacity(&self) -> Result<serde_json::Value, FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .get(format!("{}/v1/capacity", self.endpoint))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        match response.status().as_u16() {
            200 => bounded_json(response).await,
            _ => Err(FabricError::Response),
        }
    }

    pub async fn create_workspace(
        &self,
        workspace_id: Uuid,
        allocated_bytes: Option<u64>,
        elevated: Option<bool>,
    ) -> Result<CreateOutcome, FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .post(format!("{}/v1/workspaces", self.endpoint))
            .timeout(Duration::from_secs(360))
            .json(&serde_json::json!({
                "workspace_id": workspace_id,
                "allocated_bytes": allocated_bytes,
                "elevated": elevated,
            }))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        match response.status().as_u16() {
            200 => Ok(CreateOutcome::Created),
            202 => Ok(CreateOutcome::Unknown),
            _ => Err(FabricError::Response),
        }
    }

    /// Read-only existence probe used to reconcile an indeterminate create.
    ///
    /// * `Ok(Some(state))` — the Fabric answered 200 and holds the identity;
    ///   `state` is the Fabric-reported lifecycle state (e.g. `ready` or
    ///   `creating`). A `creating` Fabric workspace must not be exposed as
    ///   control-ready.
    /// * `Ok(None)` — the Fabric answered 404 (its own `NotFound`): the
    ///   identity is provably absent.
    /// * `Err` — transport failure or any other unusable HTTP status.
    pub async fn get_workspace(&self, workspace_id: Uuid) -> Result<Option<String>, FabricError> {
        Ok(self
            .get_workspace_probe(workspace_id)
            .await?
            .map(|probe| probe.state))
    }

    pub async fn get_workspace_probe(
        &self,
        workspace_id: Uuid,
    ) -> Result<Option<WorkspaceProbe>, FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .get(format!("{}/v1/workspaces/{workspace_id}", self.endpoint))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        match response.status().as_u16() {
            200 => {
                #[derive(Deserialize)]
                struct Probe {
                    state: String,
                    #[serde(default, alias = "allocatedBytes")]
                    allocated_bytes: Option<u64>,
                }
                let probe: Probe = bounded_json(response).await?;
                Ok(Some(WorkspaceProbe {
                    state: probe.state,
                    allocated_bytes: probe.allocated_bytes,
                }))
            }
            404 => Ok(None),
            _ => Err(FabricError::Response),
        }
    }

    pub async fn fence_workspace(&self, workspace_id: Uuid) -> Result<(), FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .post(format!(
                "{}/v1/workspaces/{workspace_id}/fence",
                self.endpoint
            ))
            .timeout(Duration::from_secs(120))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        match response.status().as_u16() {
            200 => Ok(()),
            202 => Err(FabricError::OutcomeUnknown),
            _ => Err(FabricError::Response),
        }
    }

    pub async fn grow_workspace(
        &self,
        workspace_id: Uuid,
        allocated_bytes: u64,
    ) -> Result<WorkspaceProbe, FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .post(format!(
                "{}/v1/workspaces/{workspace_id}/grow",
                self.endpoint
            ))
            .timeout(Duration::from_secs(180))
            .json(&serde_json::json!({
                "allocated_bytes": allocated_bytes,
            }))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        match response.status().as_u16() {
            200 => {
                #[derive(Deserialize)]
                struct Probe {
                    state: String,
                    #[serde(default, alias = "allocatedBytes")]
                    allocated_bytes: Option<u64>,
                }
                let probe: Probe = bounded_json(response).await?;
                Ok(WorkspaceProbe {
                    state: probe.state,
                    allocated_bytes: probe.allocated_bytes,
                })
            }
            202 => Err(FabricError::OutcomeUnknown),
            _ => Err(FabricError::Response),
        }
    }

    /// Observed Workspace guest image when Fabric could read the running Pod.
    /// Transport is distinct from a missing guest: a configured client never
    /// fail-opens on transport; only a missing runtime skips the probe.
    pub async fn workspace_guest_image(
        &self,
        workspace_id: Uuid,
    ) -> Result<Option<String>, FabricError> {
        if self.contract_stub {
            return Ok(Some("voie-workspace:v1".into()));
        }
        let response = self
            .http
            .get(format!("{}/v1/workspaces/{workspace_id}", self.endpoint))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        match response.status().as_u16() {
            200 => {
                #[derive(Deserialize)]
                struct Probe {
                    image: Option<String>,
                }
                let probe: Probe = bounded_json(response).await?;
                Ok(probe
                    .image
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty()))
            }
            404 => Ok(None),
            _ => Err(FabricError::Response),
        }
    }

    pub async fn exec(
        &self,
        workspace_id: Uuid,
        call_id: &str,
        command: &str,
    ) -> Result<ExecResult, FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .post(format!(
                "{}/v1/workspaces/{workspace_id}/exec",
                self.endpoint
            ))
            .json(&serde_json::json!({
                "call_id": call_id,
                "command": command,
            }))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        if response.status().is_success() {
            bounded_json(response).await
        } else {
            Err(FabricError::Response)
        }
    }

    pub async fn exec_result(
        &self,
        workspace_id: Uuid,
        call_id: &str,
    ) -> Result<ExecResult, FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .get(format!(
                "{}/v1/workspaces/{workspace_id}/exec/{call_id}",
                self.endpoint
            ))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        if !response.status().is_success() {
            return Err(FabricError::Response);
        }
        bounded_json(response).await
    }

    /// Replaces the Workspace execution generation after teardown confirms.
    /// Firecracker replacement can wait for a new Ready pod, so this uses
    /// the same long timeout as product mutations.
    pub async fn replace_workspace(&self, workspace_id: Uuid) -> Result<(), FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .post(format!(
                "{}/v1/workspaces/{workspace_id}/replace",
                self.endpoint
            ))
            .timeout(Duration::from_secs(360))
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(FabricError::Response)
        }
    }

    pub async fn delete_workspace(&self, workspace_id: Uuid) -> Result<(), FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .delete(format!("{}/v1/workspaces/{workspace_id}", self.endpoint))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        match response.status().as_u16() {
            200 | 404 => Ok(()),
            202 => Err(FabricError::OutcomeUnknown),
            _ => Err(FabricError::Response),
        }
    }

    pub async fn product_mutate(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<ProductOutcome, FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .post(format!("{}{path}", self.endpoint))
            .timeout(Duration::from_secs(360))
            .json(body)
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        match response.status().as_u16() {
            200 | 202 => bounded_json(response).await,
            _ => Err(FabricError::Response),
        }
    }

    /// Observational product GET. Database Ready is kubelet, not the typed
    /// create journal.
    pub async fn product_get(&self, path: &str) -> Result<ProductOutcome, FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .get(format!("{}{path}", self.endpoint))
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        match response.status().as_u16() {
            200 => bounded_json(response).await,
            _ => Err(FabricError::Response),
        }
    }

    pub async fn put_release_artifact(
        &self,
        release_id: Uuid,
        artifact_hash: &str,
        bytes: Vec<u8>,
    ) -> Result<(), FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .put(format!(
                "{}/v1/releases/{release_id}/artifact",
                self.endpoint
            ))
            .timeout(Duration::from_secs(300))
            .header("x-voie-artifact-hash", artifact_hash)
            .header("content-type", "application/octet-stream")
            .body(bytes)
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(FabricError::Response)
        }
    }

    /// Asks Fabric to run guest `voie-pack` and stream the artifact. The
    /// client never sends a Blob credential. HTTP 202 is outcome-unknown.
    pub async fn pack_workspace(
        &self,
        workspace_id: Uuid,
        operation_id: Uuid,
        request_hash: &str,
        relative_root: &str,
    ) -> Result<reqwest::Response, FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .post(format!(
                "{}/v1/workspaces/{workspace_id}/pack",
                self.endpoint
            ))
            .timeout(Duration::from_secs(300))
            .json(&serde_json::json!({
                "operation_id": operation_id,
                "request_hash": request_hash,
                "relative_root": relative_root,
            }))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        match response.status().as_u16() {
            200 => Ok(response),
            202 => Err(FabricError::OutcomeUnknown),
            _ => Err(FabricError::Response),
        }
    }

    pub async fn snapshot_workspace(
        &self,
        workspace_id: Uuid,
        operation_id: Uuid,
        request_hash: &str,
    ) -> Result<reqwest::Response, FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .post(format!(
                "{}/v1/workspaces/{workspace_id}/snapshot",
                self.endpoint
            ))
            .timeout(Duration::from_secs(3600))
            .json(&serde_json::json!({
                "operation_id": operation_id,
                "request_hash": request_hash,
                "relative_root": ".",
            }))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        match response.status().as_u16() {
            200 => Ok(response),
            202 => Err(FabricError::OutcomeUnknown),
            _ => Err(FabricError::Response),
        }
    }

    pub async fn ack_workspace_snapshot(
        &self,
        workspace_id: Uuid,
        operation_id: Uuid,
    ) -> Result<(), FabricError> {
        self.ack_workspace_staging(workspace_id, operation_id, "snapshot")
            .await
    }

    pub async fn ack_workspace_pack(
        &self,
        workspace_id: Uuid,
        operation_id: Uuid,
    ) -> Result<(), FabricError> {
        self.ack_workspace_staging(workspace_id, operation_id, "pack")
            .await
    }

    async fn ack_workspace_staging(
        &self,
        workspace_id: Uuid,
        operation_id: Uuid,
        kind: &str,
    ) -> Result<(), FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .delete(format!(
                "{}/v1/workspaces/{workspace_id}/{kind}",
                self.endpoint
            ))
            .timeout(Duration::from_secs(30))
            .json(&serde_json::json!({
                "operation_id": operation_id,
                "request_hash": format!("ack:{operation_id}"),
            }))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        match response.status().as_u16() {
            200 | 204 | 404 => Ok(()),
            _ => Err(FabricError::Response),
        }
    }

    pub async fn ack_database_backup(
        &self,
        database_id: Uuid,
        operation_id: Uuid,
    ) -> Result<(), FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .delete(format!(
                "{}/v1/databases/{database_id}/backup",
                self.endpoint
            ))
            .timeout(Duration::from_secs(30))
            .json(&serde_json::json!({
                "operation_id": operation_id,
                "request_hash": format!("ack:{operation_id}"),
                "desired_revision": 1,
            }))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        match response.status().as_u16() {
            200 | 204 | 404 => Ok(()),
            _ => Err(FabricError::Response),
        }
    }

    pub async fn guest_run(
        &self,
        workspace_id: Uuid,
        operation_id: Uuid,
        request_hash: &str,
        relative_root: &str,
        argv: &[String],
    ) -> Result<i32, FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .post(format!(
                "{}/v1/workspaces/{workspace_id}/guest-run",
                self.endpoint
            ))
            .timeout(Duration::from_secs(300))
            .json(&serde_json::json!({
                "operation_id": operation_id,
                "request_hash": request_hash,
                "relative_root": relative_root,
                "run_argv": argv,
            }))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        match response.status().as_u16() {
            200 => {
                #[derive(Deserialize)]
                struct Guest {
                    #[serde(rename = "exitCode")]
                    exit_code: i32,
                }
                let guest: Guest = bounded_json(response).await?;
                Ok(guest.exit_code)
            }
            202 => Err(FabricError::OutcomeUnknown),
            _ => Err(FabricError::Response),
        }
    }

    pub async fn backup_database(
        &self,
        database_id: Uuid,
        operation_id: Uuid,
        request_hash: &str,
    ) -> Result<reqwest::Response, FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .post(format!(
                "{}/v1/databases/{database_id}/backup",
                self.endpoint
            ))
            .timeout(Duration::from_secs(3600))
            .json(&serde_json::json!({
                "operation_id": operation_id,
                "request_hash": request_hash,
                "desired_revision": 1,
            }))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        match response.status().as_u16() {
            200 => Ok(response),
            202 => Err(FabricError::OutcomeUnknown),
            _ => Err(FabricError::Response),
        }
    }

    pub async fn put_restore_artifact(
        &self,
        database_id: Uuid,
        artifact_hash: &str,
        bytes: Vec<u8>,
    ) -> Result<(), FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .put(format!(
                "{}/v1/databases/{database_id}/restore-artifact",
                self.endpoint
            ))
            .timeout(Duration::from_secs(3600))
            .header("x-voie-artifact-hash", artifact_hash)
            .header("content-type", "application/octet-stream")
            .body(bytes)
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(FabricError::Response)
        }
    }

    pub async fn put_release_from_blob(
        &self,
        release_id: Uuid,
        artifact_hash: &str,
        blob: &crate::session_store::BlobStore,
        object_key: &str,
    ) -> Result<(), FabricError> {
        self.put_hashed_from_blob(
            format!("/v1/releases/{release_id}/artifact"),
            artifact_hash,
            blob,
            object_key,
        )
        .await
    }

    pub async fn put_restore_from_blob(
        &self,
        database_id: Uuid,
        artifact_hash: &str,
        blob: &crate::session_store::BlobStore,
        object_key: &str,
    ) -> Result<(), FabricError> {
        self.put_hashed_from_blob(
            format!("/v1/databases/{database_id}/restore-artifact"),
            artifact_hash,
            blob,
            object_key,
        )
        .await
    }

    pub async fn put_workspace_restore_from_blob(
        &self,
        workspace_id: Uuid,
        artifact_hash: &str,
        blob: &crate::session_store::BlobStore,
        object_key: &str,
    ) -> Result<(), FabricError> {
        self.put_hashed_from_blob(
            format!("/v1/workspaces/{workspace_id}/restore-artifact"),
            artifact_hash,
            blob,
            object_key,
        )
        .await
    }

    async fn put_hashed_from_blob(
        &self,
        path: String,
        artifact_hash: &str,
        blob: &crate::session_store::BlobStore,
        object_key: &str,
    ) -> Result<(), FabricError> {
        self.refuse_stub()?;
        let blob = blob.clone();
        let key = object_key.to_owned();
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(4);
        tokio::spawn(async move {
            match blob.get_stream(&key).await {
                Ok(stream) => {
                    let mut stream = std::pin::pin!(stream);
                    while let Some(item) = stream.next().await {
                        let mapped = item.map_err(|error| std::io::Error::other(error.to_string()));
                        if tx.send(mapped).await.is_err() {
                            break;
                        }
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(std::io::Error::other(error.to_string()))).await;
                }
            }
        });
        let stream = futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        let response = self
            .http
            .put(format!("{}{path}", self.endpoint))
            .timeout(Duration::from_secs(3600))
            .header("x-voie-artifact-hash", artifact_hash)
            .header("content-type", "application/octet-stream")
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(FabricError::Response)
        }
    }

    pub async fn restore_workspace(
        &self,
        workspace_id: Uuid,
        operation_id: Uuid,
        request_hash: &str,
        artifact_hash: &str,
        allocated_bytes: Option<u64>,
        elevated: Option<bool>,
    ) -> Result<CreateOutcome, FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .post(format!(
                "{}/v1/workspaces/{workspace_id}/restore",
                self.endpoint
            ))
            .timeout(Duration::from_secs(3600))
            .json(&serde_json::json!({
                "operation_id": operation_id,
                "request_hash": request_hash,
                "artifact_hash": artifact_hash,
                "allocated_bytes": allocated_bytes,
                "elevated": elevated,
            }))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        match response.status().as_u16() {
            200 => Ok(CreateOutcome::Created),
            202 => Ok(CreateOutcome::Unknown),
            _ => Err(FabricError::Response),
        }
    }

    pub async fn probe_deployment_health(
        &self,
        deployment_id: Uuid,
        port: u16,
        health_path: &str,
    ) -> Result<bool, FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .post(format!(
                "{}/v1/deployments/{deployment_id}/health",
                self.endpoint
            ))
            .json(&serde_json::json!({
                "operation_id": Uuid::new_v4(),
                "request_hash": format!("health:{deployment_id}"),
                "desired_revision": 1,
                "port": port,
                "health_path": health_path,
                "run_argv": ["true"],
            }))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        match response.status().as_u16() {
            200 => Ok(true),
            409 | 202 => Ok(false),
            _ => Err(FabricError::Response),
        }
    }

    pub async fn get_deployment_logs(&self, deployment_id: Uuid) -> Result<Vec<u8>, FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .get(format!(
                "{}/v1/deployments/{deployment_id}/logs",
                self.endpoint
            ))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        match response.status().as_u16() {
            200 => {
                let bytes = response.bytes().await.map_err(|_| FabricError::Transport)?;
                if bytes.len() > 256 * 1024 {
                    Ok(bytes[..256 * 1024].to_vec())
                } else {
                    Ok(bytes.to_vec())
                }
            }
            _ => Err(FabricError::Response),
        }
    }

    pub async fn routes(&self) -> Result<serde_json::Value, FabricError> {
        self.refuse_stub()?;
        let response = self
            .http
            .get(format!("{}/v1/routes", self.endpoint))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        if response.status().is_success() {
            bounded_json(response).await
        } else {
            Err(FabricError::Response)
        }
    }
}

fn require_env(name: &'static str) -> Result<String, FabricError> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(FabricError::Config("required fabric setting is missing")),
    }
}

/// Parses a success response under an explicit byte bound. Oversized bodies
/// are refused instead of buffered without limit; no body text is logged.
async fn bounded_json<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, FabricError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(FabricError::Response);
    }
    let bytes = response.bytes().await.map_err(|_| FabricError::Transport)?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(FabricError::Response);
    }
    serde_json::from_slice(&bytes).map_err(|_| FabricError::Response)
}
