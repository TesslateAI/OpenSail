//! Azure Blob SharedKey client for one container. Immutable canonical objects.

use std::error::Error;
use std::fmt;
use std::time::{Duration, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use hmac::{Hmac, Mac};
use reqwest::{Client, StatusCode};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

const API_VERSION: &str = "2021-08-06";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Azure Blob access. The account key is never rendered.
#[derive(Clone)]
pub struct BlobStore {
    account: String,
    key: Vec<u8>,
    container: String,
    endpoint: String,
    http: Client,
}

#[derive(Debug)]
pub enum BlobStoreError {
    Config(&'static str),
    Transport,
    UnexpectedStatus,
    Missing,
}

impl fmt::Display for BlobStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlobStoreError::Config(message) => write!(f, "configuration: {message}"),
            BlobStoreError::Transport => write!(f, "blob transport failed"),
            BlobStoreError::UnexpectedStatus => write!(f, "blob returned an unexpected status"),
            BlobStoreError::Missing => write!(f, "blob object is missing"),
        }
    }
}

impl Error for BlobStoreError {}

impl BlobStore {
    /// `VOIE_AZURE_BLOB_ACCOUNT`, the account key from either
    /// `VOIE_AZURE_BLOB_KEY` or `VOIE_AZURE_BLOB_KEY_FILE`,
    /// `VOIE_AZURE_BLOB_CONTAINER`, and optional `VOIE_AZURE_BLOB_ENDPOINT`.
    pub fn from_env() -> Result<Self, BlobStoreError> {
        let account = require_env("VOIE_AZURE_BLOB_ACCOUNT")?;
        let key_b64 = credential(
            "VOIE_AZURE_BLOB_KEY",
            "VOIE_AZURE_BLOB_KEY_FILE",
            "required blob setting is missing",
        )
        .map_err(BlobStoreError::Config)?;
        let container = require_env("VOIE_AZURE_BLOB_CONTAINER")?;
        let endpoint = std::env::var("VOIE_AZURE_BLOB_ENDPOINT")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| format!("https://{account}.blob.core.windows.net"));
        Self::new(account, &key_b64, container, endpoint)
    }

    pub fn new(
        account: String,
        key_b64: &str,
        container: String,
        endpoint: String,
    ) -> Result<Self, BlobStoreError> {
        let key = BASE64
            .decode(key_b64.trim().as_bytes())
            .map_err(|_| BlobStoreError::Config("VOIE_AZURE_BLOB_KEY is not valid base64"))?;
        let http = Client::builder()
            .https_only(endpoint.starts_with("https://"))
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| BlobStoreError::Transport)?;
        Ok(BlobStore {
            account,
            key,
            container,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            http,
        })
    }

    /// Conditionally creates an immutable block blob. A same-key retry that
    /// already holds identical bytes succeeds. Unreferenced objects are orphans.
    pub async fn put_if_absent(
        &self,
        object_key: &str,
        bytes: &[u8],
    ) -> Result<(), BlobStoreError> {
        let url = self.object_url(object_key);
        let date = httpdate::fmt_http_date(SystemTime::now());
        let length = bytes.len().to_string();
        let authorization = self.authorization(
            "PUT",
            &length,
            "application/json",
            Some("*"),
            &date,
            object_key,
        )?;
        let response = self
            .http
            .put(&url)
            .header("Authorization", authorization)
            .header("Content-Length", &length)
            .header("Content-Type", "application/json")
            .header("If-None-Match", "*")
            .header("x-ms-blob-type", "BlockBlob")
            .header("x-ms-date", &date)
            .header("x-ms-version", API_VERSION)
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(|_| BlobStoreError::Transport)?;
        match response.status() {
            StatusCode::CREATED | StatusCode::OK => Ok(()),
            StatusCode::PRECONDITION_FAILED | StatusCode::CONFLICT => {
                let existing = self.get(object_key).await?;
                if existing == bytes {
                    Ok(())
                } else {
                    Err(BlobStoreError::UnexpectedStatus)
                }
            }
            _ => Err(BlobStoreError::UnexpectedStatus),
        }
    }

    pub async fn get(&self, object_key: &str) -> Result<Vec<u8>, BlobStoreError> {
        let url = self.object_url(object_key);
        let date = httpdate::fmt_http_date(SystemTime::now());
        let authorization = self.authorization("GET", "", "", None, &date, object_key)?;
        let response = self
            .http
            .get(&url)
            .header("Authorization", authorization)
            .header("x-ms-date", &date)
            .header("x-ms-version", API_VERSION)
            .send()
            .await
            .map_err(|_| BlobStoreError::Transport)?;
        match response.status() {
            StatusCode::OK => {
                if response
                    .content_length()
                    .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
                {
                    return Err(BlobStoreError::UnexpectedStatus);
                }
                let bytes = response
                    .bytes()
                    .await
                    .map_err(|_| BlobStoreError::Transport)?;
                if bytes.len() > MAX_RESPONSE_BYTES {
                    return Err(BlobStoreError::UnexpectedStatus);
                }
                Ok(bytes.to_vec())
            }
            StatusCode::NOT_FOUND => Err(BlobStoreError::Missing),
            _ => Err(BlobStoreError::UnexpectedStatus),
        }
    }

    /// True when the signed control path reaches the Blob service: an
    /// authorized GET answers, found or missing. Transport and authorization
    /// failures fail closed.
    pub async fn reachable(&self) -> bool {
        matches!(
            self.get("__voie_readiness_probe").await,
            Ok(_) | Err(BlobStoreError::Missing)
        )
    }

    fn object_url(&self, object_key: &str) -> String {
        format!("{}/{}/{}", self.endpoint, self.container, object_key)
    }

    fn authorization(
        &self,
        verb: &str,
        content_length: &str,
        content_type: &str,
        if_none_match: Option<&str>,
        date: &str,
        object_key: &str,
    ) -> Result<String, BlobStoreError> {
        let if_none = if_none_match.unwrap_or("");
        let mut canonical_headers = String::new();
        if verb == "PUT" {
            canonical_headers.push_str("x-ms-blob-type:BlockBlob\n");
        }
        canonical_headers.push_str(&format!("x-ms-date:{date}\nx-ms-version:{API_VERSION}\n"));
        let canonical_resource = format!("/{}/{}/{object_key}", self.account, self.container);
        let string_to_sign = format!(
            "{verb}\n\n\n{content_length}\n\n{content_type}\n\n\n\n{if_none}\n\n\n{canonical_headers}{canonical_resource}"
        );
        let mut mac = HmacSha256::new_from_slice(&self.key)
            .map_err(|_| BlobStoreError::Config("blob account key is unusable"))?;
        mac.update(string_to_sign.as_bytes());
        let signature = BASE64.encode(mac.finalize().into_bytes());
        Ok(format!("SharedKey {}:{}", self.account, signature))
    }
}

fn require_env(name: &'static str) -> Result<String, BlobStoreError> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(BlobStoreError::Config("required blob setting is missing")),
    }
}

/// Resolves one credential from its direct variable or its credential file.
/// Exactly one source must be present; the value is never logged.
fn credential(direct: &str, file: &str, missing: &'static str) -> Result<String, &'static str> {
    if let Ok(path) = std::env::var(file) {
        if !path.trim().is_empty() {
            return std::fs::read_to_string(path.trim())
                .map(|value| value.trim().to_owned())
                .map_err(|_| "credential file is unreadable");
        }
    }
    match std::env::var(direct) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(missing),
    }
}
