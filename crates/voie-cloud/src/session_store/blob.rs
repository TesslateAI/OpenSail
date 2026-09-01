//! Azure Blob SharedKey client for one container. Immutable canonical objects.

use std::error::Error;
use std::fmt;
use std::time::{Duration, SystemTime};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use hmac::{Hmac, Mac};
use reqwest::{Client, StatusCode};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

const API_VERSION: &str = "2021-08-06";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const ARTIFACT_TIMEOUT: Duration = Duration::from_secs(3600);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_ARTIFACT_BYTES: usize = 512 * 1024 * 1024;

/// Azure Blob access. The account key is never rendered.
#[derive(Clone)]
pub struct BlobStore {
    account: String,
    key: Vec<u8>,
    container: String,
    endpoint: String,
    http: Client,
    artifact_http: Client,
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
        let artifact_http = Client::builder()
            .https_only(endpoint.starts_with("https://"))
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(ARTIFACT_TIMEOUT)
            .build()
            .map_err(|_| BlobStoreError::Transport)?;
        Ok(BlobStore {
            account,
            key,
            container,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            http,
            artifact_http,
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
        let authorization = self.sign(
            "PUT",
            &length,
            "application/json",
            Some("*"),
            &date,
            object_key,
            &[("x-ms-blob-type", "BlockBlob")],
            &[],
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
        let authorization = self.sign("GET", "", "", None, &date, object_key, &[], &[])?;
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

    /// Digest and length of an object that already exists. `None` when
    /// absent. Used to resume a Blob upload after Fabric has already ACKed.
    pub async fn digest_if_present(
        &self,
        object_key: &str,
    ) -> Result<Option<([u8; 32], u64)>, BlobStoreError> {
        if !self.head(object_key).await? {
            return Ok(None);
        }
        Ok(Some(self.hash_existing_object(object_key).await?))
    }

    /// Removes one unreferenced backup object. Missing is success; a
    /// different status fails closed so retention cannot invent deletion.
    pub async fn delete(&self, object_key: &str) -> Result<(), BlobStoreError> {
        let url = self.object_url(object_key);
        let date = httpdate::fmt_http_date(SystemTime::now());
        let authorization = self.sign("DELETE", "", "", None, &date, object_key, &[], &[])?;
        let response = self
            .http
            .request(reqwest::Method::DELETE, &url)
            .header("Authorization", authorization)
            .header("x-ms-date", &date)
            .header("x-ms-version", API_VERSION)
            .send()
            .await
            .map_err(|_| BlobStoreError::Transport)?;
        match response.status() {
            StatusCode::ACCEPTED | StatusCode::OK | StatusCode::NOT_FOUND => Ok(()),
            _ => Err(BlobStoreError::UnexpectedStatus),
        }
    }

    /// Immutable Release artifact. The object key encodes the content hash, so
    /// a same-key conflict is identity without downloading the object.
    /// Workspace and Fabric never receive this credential.
    pub async fn put_artifact_if_absent(
        &self,
        object_key: &str,
        bytes: &[u8],
    ) -> Result<(), BlobStoreError> {
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(BlobStoreError::UnexpectedStatus);
        }
        let url = self.object_url(object_key);
        let date = httpdate::fmt_http_date(SystemTime::now());
        let length = bytes.len().to_string();
        let authorization = self.sign(
            "PUT",
            &length,
            "application/octet-stream",
            Some("*"),
            &date,
            object_key,
            &[("x-ms-blob-type", "BlockBlob")],
            &[],
        )?;
        let response = self
            .artifact_http
            .put(&url)
            .header("Authorization", authorization)
            .header("Content-Length", &length)
            .header("Content-Type", "application/octet-stream")
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
                let existing = self.get_artifact(object_key).await?;
                if existing == bytes {
                    Ok(())
                } else {
                    Err(BlobStoreError::UnexpectedStatus)
                }
            }
            _ => Err(BlobStoreError::UnexpectedStatus),
        }
    }

    pub async fn get_artifact(&self, object_key: &str) -> Result<Vec<u8>, BlobStoreError> {
        let url = self.object_url(object_key);
        let date = httpdate::fmt_http_date(SystemTime::now());
        let authorization = self.sign("GET", "", "", None, &date, object_key, &[], &[])?;
        let response = self
            .artifact_http
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
                    .is_some_and(|length| length > MAX_ARTIFACT_BYTES as u64)
                {
                    return Err(BlobStoreError::UnexpectedStatus);
                }
                let bytes = response
                    .bytes()
                    .await
                    .map_err(|_| BlobStoreError::Transport)?;
                if bytes.len() > MAX_ARTIFACT_BYTES {
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

    /// Uploads an object from a byte stream as a block blob. Same-key
    /// retries that already hold the object succeed without rewriting.
    /// `max_bytes` is enforced only when set (Release artifacts stay 512 MiB).
    pub async fn put_stream_if_absent<S, E>(
        &self,
        object_key: &str,
        mut stream: S,
        max_bytes: Option<u64>,
    ) -> Result<([u8; 32], u64), BlobStoreError>
    where
        S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Unpin,
        E: std::error::Error + Send + Sync + 'static,
    {
        use futures_util::StreamExt;
        use sha2::Digest;

        if self.head(object_key).await? {
            return self.hash_existing_object(object_key).await;
        }
        let mut hasher = sha2::Sha256::new();
        let mut total = 0u64;
        let mut blocks = Vec::new();
        let mut index: u32 = 0;
        let mut pending = Vec::new();
        const BLOCK: usize = 4 * 1024 * 1024;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| BlobStoreError::Transport)?;
            if chunk.is_empty() {
                continue;
            }
            hasher.update(&chunk);
            total = total.saturating_add(chunk.len() as u64);
            if max_bytes.is_some_and(|limit| total > limit) {
                return Err(BlobStoreError::UnexpectedStatus);
            }
            pending.extend_from_slice(&chunk);
            while pending.len() >= BLOCK {
                let block: Vec<u8> = pending.drain(..BLOCK).collect();
                self.put_block(object_key, index, &block).await?;
                blocks.push(index);
                index += 1;
            }
        }
        if !pending.is_empty() || blocks.is_empty() {
            self.put_block(object_key, index, &pending).await?;
            blocks.push(index);
        }
        let conflict = self.commit_blocks(object_key, &blocks, true).await?;
        let digest: [u8; 32] = hasher.finalize().into();
        if conflict {
            let (existing, length) = self.hash_existing_object(object_key).await?;
            if existing != digest || length != total {
                return Err(BlobStoreError::UnexpectedStatus);
            }
            return Ok((existing, length));
        }
        Ok((digest, total))
    }

    /// Streams object bytes without assembling them in memory. Release
    /// artifacts still use [`Self::get_artifact`].
    pub async fn get_stream(
        &self,
        object_key: &str,
    ) -> Result<
        impl futures_util::Stream<Item = Result<bytes::Bytes, BlobStoreError>>,
        BlobStoreError,
    > {
        use futures_util::StreamExt;
        let url = self.object_url(object_key);
        let date = httpdate::fmt_http_date(SystemTime::now());
        let authorization = self.sign("GET", "", "", None, &date, object_key, &[], &[])?;
        let response = self
            .artifact_http
            .get(&url)
            .header("Authorization", authorization)
            .header("x-ms-date", &date)
            .header("x-ms-version", API_VERSION)
            .send()
            .await
            .map_err(|_| BlobStoreError::Transport)?;
        match response.status() {
            StatusCode::OK => Ok(response
                .bytes_stream()
                .map(|item| item.map_err(|_| BlobStoreError::Transport))),
            StatusCode::NOT_FOUND => Err(BlobStoreError::Missing),
            _ => Err(BlobStoreError::UnexpectedStatus),
        }
    }

    async fn head(&self, object_key: &str) -> Result<bool, BlobStoreError> {
        let url = self.object_url(object_key);
        let date = httpdate::fmt_http_date(SystemTime::now());
        let authorization = self.sign("HEAD", "", "", None, &date, object_key, &[], &[])?;
        let response = self
            .http
            .head(&url)
            .header("Authorization", authorization)
            .header("x-ms-date", &date)
            .header("x-ms-version", API_VERSION)
            .send()
            .await
            .map_err(|_| BlobStoreError::Transport)?;
        match response.status() {
            StatusCode::OK => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            _ => Err(BlobStoreError::UnexpectedStatus),
        }
    }

    async fn put_block(
        &self,
        object_key: &str,
        index: u32,
        bytes: &[u8],
    ) -> Result<(), BlobStoreError> {
        let block_id = block_id(index);
        let url = format!(
            "{}?comp=block&blockid={}",
            self.object_url(object_key),
            urlencoding_plus(&block_id)
        );
        let date = httpdate::fmt_http_date(SystemTime::now());
        let length = bytes.len().to_string();
        let authorization = self.sign(
            "PUT",
            &length,
            "application/octet-stream",
            None,
            &date,
            object_key,
            &[],
            &[("blockid", &block_id), ("comp", "block")],
        )?;
        let response = self
            .artifact_http
            .put(&url)
            .header("Authorization", authorization)
            .header("Content-Length", &length)
            .header("Content-Type", "application/octet-stream")
            .header("x-ms-date", &date)
            .header("x-ms-version", API_VERSION)
            .body(bytes.to_vec())
            .send()
            .await
            .map_err(|_| BlobStoreError::Transport)?;
        match response.status() {
            StatusCode::CREATED | StatusCode::OK => Ok(()),
            _ => Err(BlobStoreError::UnexpectedStatus),
        }
    }

    async fn commit_blocks(
        &self,
        object_key: &str,
        blocks: &[u32],
        if_none_match: bool,
    ) -> Result<bool, BlobStoreError> {
        let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList>");
        for index in blocks {
            xml.push_str("<Latest>");
            xml.push_str(&block_id(*index));
            xml.push_str("</Latest>");
        }
        xml.push_str("</BlockList>");
        let url = format!("{}?comp=blocklist", self.object_url(object_key));
        let date = httpdate::fmt_http_date(SystemTime::now());
        let length = xml.len().to_string();
        let if_none = if if_none_match { Some("*") } else { None };
        let authorization = self.sign(
            "PUT",
            &length,
            "application/xml",
            if_none,
            &date,
            object_key,
            &[("x-ms-blob-content-type", "application/octet-stream")],
            &[("comp", "blocklist")],
        )?;
        let mut request = self
            .artifact_http
            .put(&url)
            .header("Authorization", authorization)
            .header("Content-Length", &length)
            .header("Content-Type", "application/xml")
            .header("x-ms-blob-content-type", "application/octet-stream")
            .header("x-ms-date", &date)
            .header("x-ms-version", API_VERSION)
            .body(xml.into_bytes());
        if if_none_match {
            request = request.header("If-None-Match", "*");
        }
        let response = request
            .send()
            .await
            .map_err(|_| BlobStoreError::Transport)?;
        match response.status() {
            StatusCode::CREATED | StatusCode::OK => Ok(false),
            StatusCode::PRECONDITION_FAILED | StatusCode::CONFLICT => Ok(true),
            _ => Err(BlobStoreError::UnexpectedStatus),
        }
    }

    async fn hash_existing_object(
        &self,
        object_key: &str,
    ) -> Result<([u8; 32], u64), BlobStoreError> {
        use futures_util::StreamExt;
        use sha2::Digest;
        let mut existing = self.get_stream(object_key).await?;
        let mut hasher = sha2::Sha256::new();
        let mut total = 0u64;
        while let Some(chunk) = existing.next().await {
            let chunk = chunk?;
            hasher.update(&chunk);
            total = total.saturating_add(chunk.len() as u64);
        }
        Ok((hasher.finalize().into(), total))
    }

    fn object_url(&self, object_key: &str) -> String {
        format!("{}/{}/{}", self.endpoint, self.container, object_key)
    }

    fn sign(
        &self,
        verb: &str,
        content_length: &str,
        content_type: &str,
        if_none_match: Option<&str>,
        date: &str,
        object_key: &str,
        extra_ms: &[(&str, &str)],
        query: &[(&str, &str)],
    ) -> Result<String, BlobStoreError> {
        let if_none = if_none_match.unwrap_or("");
        let mut headers: Vec<(&str, &str)> = extra_ms.to_vec();
        headers.push(("x-ms-date", date));
        headers.push(("x-ms-version", API_VERSION));
        headers.sort_by(|a, b| a.0.cmp(b.0));
        let mut canonical_headers = String::new();
        for (name, value) in &headers {
            canonical_headers.push_str(&format!("{name}:{value}\n"));
        }
        let mut canonical_resource = format!("/{}/{}/{object_key}", self.account, self.container);
        let mut query_parts = query.to_vec();
        query_parts.sort_by(|a, b| a.0.cmp(b.0));
        for (name, value) in query_parts {
            canonical_resource.push('\n');
            canonical_resource.push_str(name);
            canonical_resource.push(':');
            canonical_resource.push_str(value);
        }
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

fn block_id(index: u32) -> String {
    BASE64.encode(format!("{index:08}").as_bytes())
}

fn urlencoding_plus(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
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
