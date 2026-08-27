//! One configured Fabric endpoint over product mTLS.

use std::error::Error;
use std::fmt;
use std::path::Path;
use std::time::Duration;

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
}

impl fmt::Display for FabricError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FabricError::Config(message) => write!(f, "configuration: {message}"),
            FabricError::Transport => write!(f, "fabric transport failed"),
            FabricError::Response => write!(f, "fabric response was unusable"),
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

/// Product mTLS client. Trusts only the configured Fabric CA.
pub struct FabricClient {
    http: Client,
    endpoint: String,
}

impl FabricClient {
    /// `VOIE_FABRIC_ENDPOINT`, `VOIE_FABRIC_CLIENT_CERT_PATH`,
    /// `VOIE_FABRIC_CLIENT_KEY_PATH`, `VOIE_FABRIC_CA_CERT_PATH`.
    pub fn from_env() -> Result<Self, FabricError> {
        let endpoint = require_env("VOIE_FABRIC_ENDPOINT")?;
        let cert_path = require_env("VOIE_FABRIC_CLIENT_CERT_PATH")?;
        let key_path = require_env("VOIE_FABRIC_CLIENT_KEY_PATH")?;
        let ca_path = require_env("VOIE_FABRIC_CA_CERT_PATH")?;
        Self::from_pem_files(endpoint, cert_path, key_path, ca_path)
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
        })
    }

    pub async fn health(&self) -> Result<(), FabricError> {
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

    pub async fn create_workspace(&self, workspace_id: Uuid) -> Result<CreateOutcome, FabricError> {
        let response = self
            .http
            .post(format!("{}/v1/workspaces", self.endpoint))
            .json(&serde_json::json!({ "workspace_id": workspace_id }))
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
                }
                let probe: Probe = bounded_json(response).await?;
                Ok(Some(probe.state))
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
    pub async fn replace_workspace(&self, workspace_id: Uuid) -> Result<(), FabricError> {
        let response = self
            .http
            .post(format!(
                "{}/v1/workspaces/{workspace_id}/replace",
                self.endpoint
            ))
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
        let response = self
            .http
            .delete(format!("{}/v1/workspaces/{workspace_id}", self.endpoint))
            .send()
            .await
            .map_err(|_| FabricError::Transport)?;
        if response.status().is_success() {
            Ok(())
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
