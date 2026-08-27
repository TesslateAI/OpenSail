//! Azure Key Vault material backend for the production profile.
//!
//! # Data plane
//!
//! The managed identity controlling this backend holds ONLY the `Set` and
//! `Delete` [Key Vault secret permissions](https://learn.microsoft.com/rest/api/keyvault/secrets),
//! so this implementation issues exactly two request shapes against
//! `{VOIE_KEY_VAULT_URI}`:
//!
//! ```text
//! PUT    {uri}/secrets/{name}?api-version=7.4   body {"value": "<utf-8 material>"}
//! DELETE {uri}/secrets/{name}?api-version=7.4
//! ```
//!
//! There is intentionally no Get/List call site for secret material anywhere
//! in this module: responses beyond the status code are discarded, values
//! travel one way into the vault, and deletion treats `404` as idempotent
//! success. The single `Method::GET` below targets the instance metadata
//! service (IMDS), not Key Vault.
//!
//! # Identity
//!
//! Tokens come from the Azure IMDS endpoint using the managed identity. When
//! `AZURE_CLIENT_ID` selects a user-assigned identity, it is expressed both
//! as the documented `client_id` query parameter and as an accompanying
//! request header. Tokens stay process-local in a bounded cache until shortly
//! before their stated expiry and are never logged, rendered through `Debug`,
//! or folded into error values.
//!
//! Material itself must be valid UTF-8 because Key Vault stores secret
//! values as JSON strings; binary material fails closed with
//! [`BackendError`].

use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::{Client, Method, StatusCode};

use super::{
    BackendError, BackendFuture, BackendKind, BackendWrite, SecretBackend, SecretReference,
    SecretValue,
};

/// Vault endpoint injected by deployment.
pub const KEY_VAULT_URI_ENV: &str = "VOIE_KEY_VAULT_URI";
/// Optional user-assigned managed-identity selector.
pub const AZURE_CLIENT_ID_ENV: &str = "AZURE_CLIENT_ID";

const SECRET_API_VERSION: &str = "7.4";
// The IMDS token endpoint is a link-local metadata address (169.254.169.254)
// and never carries secret material; it is spelled out so the static
// surface audit can confirm the sole GET targets identity acquisition.
const IMDS_ENDPOINT: &str = "http://169.254.169.254/metadata/identity/oauth2/token";
const IMDS_API_VERSION: &str = "2018-02-01";
const KEY_VAULT_RESOURCE: &str = "https://vault.azure.net";
/// Re-acquire at least this long before the token's declared expiry.
const TOKEN_REFRESH_MARGIN_SECS: u64 = 300;
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Builds the single REST URL shape shared by `put` and `delete`.
///
/// Unit tests pin this string byte-for-byte against the Key Vault contract:
/// `<https://learn.microsoft.com/en-us/rest/api/keyvault/secrets/set-secret/set-secret>`.
fn secret_operation_url(vault_uri: &str, name: &str) -> String {
    format!(
        "{}/secrets/{name}?api-version={SECRET_API_VERSION}",
        vault_uri.trim_end_matches('/')
    )
}

/// Validates and normalizes the configured vault base URI.
///
/// Fails closed when [`KEY_VAULT_URI_ENV`] is absent rather than degrading to
/// any weaker storage.
fn normalize_vault_uri(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "{KEY_VAULT_URI_ENV} is required when \
             VOIE_USER_SECRETS_BACKEND=key-vault"
        ));
    }
    if !trimmed.starts_with("https://") {
        return Err(format!("{KEY_VAULT_URI_ENV} must be an https:// vault URI"));
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err(format!("{KEY_VAULT_URI_ENV} must not contain whitespace"));
    }
    Ok(trimmed.trim_end_matches('/').to_owned())
}

/// Encodes the `Set Secret` JSON body. UTF-8 only; anything else fails.
fn put_body_json(value: &[u8]) -> Result<Vec<u8>, ()> {
    let text = std::str::from_utf8(value).map_err(|_| ())?;
    let encoded = serde_json::to_string(text).map_err(|_| ())?;
    let mut body = String::with_capacity(encoded.len() + 12);
    body.push_str("{\"value\":");
    body.push_str(&encoded);
    body.push('}');
    Ok(body.into_bytes())
}

/// IMDS token URL; appends `client_id` when a user-assigned identity applies.
fn imds_token_url(client_id: Option<&str>) -> String {
    let mut url =
        format!("{IMDS_ENDPOINT}?resource={KEY_VAULT_RESOURCE}&api-version={IMDS_API_VERSION}");
    if let Some(id) = client_id {
        url.push_str("&client_id=");
        url.push_str(id);
    }
    url
}

/// Request headers for IMDS; `Metadata` is mandatory per the service spec.
fn imds_headers(client_id: Option<&str>) -> Vec<(&'static str, String)> {
    let mut headers = vec![("Metadata", "true".to_owned())];
    if let Some(id) = client_id {
        headers.push(("client_id", id.to_owned()));
    }
    headers
}

/// Parses an IMDS response into a usable token entry.
///
/// Accepts either `expires_on` (unix seconds, quoted by IMDS) or, failing
/// that, `expires_in`; both absent means the response is unusable.
#[allow(clippy::option_option)]
fn parse_imds_token(body: &str, now_unix: u64) -> Result<(String, u64), ()> {
    let parsed: serde_json::Value = serde_json::from_str(body).map_err(|_| ())?;
    let access_token = parsed["access_token"]
        .as_str()
        .filter(|token| !token.is_empty())
        .ok_or(())?
        .to_owned();
    let expires_at_unix = if let Some(expires_on) = parsed["expires_on"]
        .as_str()
        .and_then(|v| v.parse::<u64>().ok())
    {
        expires_on
    } else if let Some(expires_in) = parsed["expires_in"]
        .as_str()
        .and_then(|v| v.parse::<u64>().ok())
    {
        now_unix.saturating_add(expires_in)
    } else {
        return Err(());
    };
    Ok((access_token, expires_at_unix))
}

/// True while the cache entry outlives the refresh margin.
fn token_is_fresh(entry: Option<&(String, u64)>, now_unix: u64) -> bool {
    match entry {
        Some((_, expires_at_unix)) => {
            *expires_at_unix > now_unix.saturating_add(TOKEN_REFRESH_MARGIN_SECS)
        }
        None => false,
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default()
}

fn lock_ignoring_poison(
    cache: &Mutex<Option<(String, u64)>>,
) -> MutexGuard<'_, Option<(String, u64)>> {
    cache.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Production material backend over Azure Key Vault.
pub struct AzureSecretBackend {
    /// Normalized https vault base URI; never carries tokens or material.
    vault_uri: String,
    http: Client,
    /// Process-local bearer token cache: `(token, expiry unix seconds)`.
    cached_token: Mutex<Option<(String, u64)>>,
}

impl fmt::Debug for AzureSecretBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AzureSecretBackend")
            .field("vault_uri", &self.vault_uri)
            .field("cached_token", &self.cached_token.lock().is_ok())
            .finish_non_exhaustive()
    }
}

impl AzureSecretBackend {
    /// Constructs the backend against a vault URI taken from environment
    /// input; see [`normalize_vault_uri`] for accepted shapes.
    pub fn new(vault_uri_raw: &str) -> Result<Self, String> {
        let vault_uri = normalize_vault_uri(vault_uri_raw)?;
        let http = Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|_| "failed to build the Key Vault HTTP client".to_owned())?;
        Ok(Self {
            vault_uri,
            http,
            cached_token: Mutex::new(None),
        })
    }

    /// Deployment entry point: resolves [`KEY_VAULT_URI_ENV`], refusing to
    /// construct anything when it is missing.
    pub fn from_env() -> Result<Self, String> {
        let raw = std::env::var(KEY_VAULT_URI_ENV).unwrap_or_default();
        Self::new(&raw)
    }

    /// Normalized vault base URI (configuration detail, not material).
    pub fn vault_uri(&self) -> &str {
        &self.vault_uri
    }

    fn fresh_cached_token(&self) -> Option<String> {
        let guard = lock_ignoring_poison(&self.cached_token);
        if token_is_fresh(guard.as_ref(), unix_now()) {
            guard.as_ref().map(|(token, _)| token.clone())
        } else {
            None
        }
    }

    /// Single IMDS round trip acquiring a token with `client_id` expressed
    /// both as query parameter and header when `AZURE_CLIENT_ID` is set.
    ///
    /// This is the module's only `Method::GET`, aimed at the metadata
    /// service — Key Vault itself sees PUT and DELETE exclusively.
    async fn acquire_token(&self) -> Result<(String, u64), ()> {
        let client_id = std::env::var(AZURE_CLIENT_ID_ENV)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let url = imds_token_url(client_id.as_deref());
        let mut request = self.http.request(Method::GET, &url);
        for (name, value) in imds_headers(client_id.as_deref()) {
            request = request.header(name, value);
        }
        let response = request.send().await.map_err(|_| ())?;
        if !response.status().is_success() {
            return Err(());
        }
        let body = response.text().await.map_err(|_| ())?;
        parse_imds_token(&body, unix_now())
    }

    async fn access_token(&self) -> Result<String, ()> {
        if let Some(token) = self.fresh_cached_token() {
            return Ok(token);
        }
        let acquired = self.acquire_token().await?;
        *lock_ignoring_poison(&self.cached_token) = Some(acquired.clone());
        Ok(acquired.0)
    }
}

impl SecretBackend for AzureSecretBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::KeyVault
    }

    /// `Set Secret`: `PUT {uri}/secrets/{name}?api-version=7.4`. Status is
    /// checked, everything past it discarded; `changed = true`.
    fn put<'a>(&'a self, reference: &'a SecretReference, value: SecretValue) -> BackendFuture<'a> {
        Box::pin(async move {
            let url = secret_operation_url(self.vault_uri(), reference.name());
            let body = put_body_json(value.as_bytes()).map_err(|_| BackendError)?;
            let token = self.access_token().await.map_err(|_| BackendError)?;
            let response = self
                .http
                .request(Method::PUT, url)
                .bearer_auth(token)
                .header("Content-Type", "application/json")
                .body(body)
                .send()
                .await
                .map_err(|_| BackendError)?;
            if !response.status().is_success() {
                return Err(BackendError);
            }
            drop(response);
            Ok(BackendWrite::changed())
        })
    }

    /// `Delete Secret`: `DELETE {uri}/secrets/{name}?api-version=7.4`;
    /// `404` maps to idempotent success, mirroring the local backend.
    fn delete<'a>(&'a self, reference: &'a SecretReference) -> BackendFuture<'a> {
        Box::pin(async move {
            let url = secret_operation_url(self.vault_uri(), reference.name());
            let token = self.access_token().await.map_err(|_| BackendError)?;
            let response = self
                .http
                .request(Method::DELETE, url)
                .bearer_auth(token)
                .send()
                .await
                .map_err(|_| BackendError)?;
            let status = response.status();
            if !(status.is_success() || status == StatusCode::NOT_FOUND) {
                return Err(BackendError);
            }
            drop(response);
            Ok(BackendWrite::changed())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODULE_SOURCE: &str = include_str!("keyvault_backend.rs");

    #[test]
    fn operation_url_pins_key_vault_rest_contract() {
        let reference = SecretReference::for_test(
            BackendKind::KeyVault,
            uuid::Uuid::parse_str("123e4567-e89b-42d3-a456-426614174000").expect("fixture"),
        );
        assert_eq!(
            secret_operation_url("https://vault.example.net/", reference.name()),
            "https://vault.example.net/secrets/us-123e4567-e89b-42d3-a456-426614174000\
             ?api-version=7.4"
        );
    }

    #[test]
    fn put_body_matches_set_secret_schema() {
        assert_eq!(
            put_body_json(b"hunter2").expect("utf-8 material"),
            b"{\"value\":\"hunter2\"}".to_vec()
        );
        let escaped =
            put_body_json("quote:\" backslash:\\ unicode:\u{1F984}".as_bytes()).expect("escapes");
        assert_eq!(
            escaped,
            concat!(
                "{\"value\":\"quote:\\\" backslash:",
                "\\\\ unicode:\u{1F984}\"}"
            )
            .as_bytes()
            .to_vec()
        );
    }

    #[test]
    fn put_body_rejects_binary_material() {
        assert_eq!(put_body_json(&[0x80, 0xff]), Err(()));
    }

    #[test]
    fn vault_uri_normalization_errors_naming_env() {
        let missing = normalize_vault_uri("").expect_err("empty refused");
        assert!(missing.contains(KEY_VAULT_URI_ENV));
        let scheme = normalize_vault_uri("http://vault.example.net").expect_err("plain http");
        assert!(scheme.contains("https://"));
        assert_eq!(
            normalize_vault_uri("  https://Vault.Example.net/a/// ").expect("trimmed"),
            "https://Vault.Example.net/a"
        );
    }

    #[test]
    fn imds_request_shape_covers_identity_selection() {
        assert_eq!(
            imds_token_url(None),
            format!("{IMDS_ENDPOINT}?resource={KEY_VAULT_RESOURCE}&api-version={IMDS_API_VERSION}")
        );
        let url = imds_token_url(Some("11111111-2222-3333-4444-555555555555"));
        assert!(url.ends_with("&client_id=11111111-2222-3333-4444-555555555555"));
        assert_eq!(imds_headers(None), vec![("Metadata", "true".to_owned())]);
        assert_eq!(
            imds_headers(Some("11111111-2222-3333-4444-555555555555")),
            vec![
                ("Metadata", "true".to_owned()),
                (
                    "client_id",
                    "11111111-2222-3333-4444-555555555555".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn imds_token_parse_expires_on_expires_in_and_refusal() {
        let now = 1_700_000_000;
        let (token, expiry) = parse_imds_token(
            concat!(
                r#"{"access_token":"abc.def","expires_on":""#,
                "1700003",
                "599",
                r#""}"#
            ),
            now,
        )
        .expect("expires_on form");
        assert_eq!((token.as_str(), expiry), ("abc.def", 1_700_003_599));

        let (token, expiry) = parse_imds_token(r#"{"access_token":"t","expires_in":"900"}"#, now)
            .expect("expires_in fallback");
        assert_eq!(expiry, now + 900);
        assert_eq!(token, "t");

        assert_eq!(parse_imds_token("{}", now), Err(()));
        assert_eq!(
            parse_imds_token(r#"{"access_token":"","expires_on":"9"}"#, now),
            Err(())
        );
        assert_eq!(parse_imds_token("not json", now), Err(()));
    }

    #[test]
    fn freshness_requires_the_refresh_margin() {
        let cached = Some(("tok".to_owned(), 10_000_u64));
        assert!(token_is_fresh(
            cached.as_ref(),
            10_000 - TOKEN_REFRESH_MARGIN_SECS - 1
        ));
        assert!(!token_is_fresh(
            cached.as_ref(),
            10_000 - TOKEN_REFRESH_MARGIN_SECS
        ));
        assert!(!token_is_fresh(None, 0));
    }

    #[test]
    fn debug_projection_carries_no_token_or_material() {
        let backend = AzureSecretBackend::new("https://vault.example.net").expect("valid");
        let rendered = format!("{backend:?}");
        assert!(rendered.contains("vault.example.net"));
        assert!(!rendered.contains("\"value"));
        assert!(!rendered.contains("Bearer "));
    }

    /// Behavioral guarantee demanded by the permission model: aside from the
    /// single IMDS token acquisition, the only HTTP verbs reachable from this
    /// module are PUT and DELETE against `/secrets`; `.get(` convenience
    /// calls and any list-style API are banned outright.
    #[test]
    fn module_issues_no_get_list_against_key_vault() {
        // Audit surface = everything before the test module, with prose
        // stripped so documentation may discuss the verbs it forbids.
        let implementation = MODULE_SOURCE
            .split("#[cfg(test)]")
            .next()
            .expect("test module separator");
        let code_only: String = implementation
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(
            code_only.matches(".get(").count(),
            0,
            "reqwest convenience getters forbidden"
        );
        assert_eq!(code_only.matches(".list(").count(), 0);
        assert_eq!(
            code_only.matches("Method::PUT").count(),
            1,
            "exactly the Set Secret verb"
        );
        assert_eq!(
            code_only.matches("Method::DELETE").count(),
            1,
            "exactly the Delete Secret verb"
        );
        assert_eq!(
            code_only.matches("Method::GET").count(),
            1,
            "the sole GET belongs to IMDS token acquisition"
        );
        let imds_line = code_only
            .lines()
            .find(|line| line.contains("metadata/identity/oauth2/token"))
            .expect("IMDS endpoint documented");
        assert!(imds_line.contains("IMDS_ENDPOINT:"));
        for line in code_only.lines().filter(|line| line.contains("/secrets")) {
            assert!(
                !line.contains("get(") && !line.to_lowercase().contains("list("),
                "secret surface touched outside set/delete: {line}"
            );
        }
    }

    /// Deployment contract, environment-tolerant: with
    /// [`KEY_VAULT_URI_ENV`] exported the `key-vault` selection constructs
    /// a [`BackendKind::KeyVault`] backend; without it the refusal names the
    /// missing variable instead of silently degrading storage.
    #[test]
    fn selection_contract_matches_deployment_env() {
        use crate::secrets::MaterialBackend;
        match std::env::var(KEY_VAULT_URI_ENV) {
            Ok(raw) if !raw.trim().is_empty() => {
                let selected = MaterialBackend::from_selection("key-vault", "");
                let backend = selected.expect("vault URI present in environment");
                assert_eq!(backend.kind(), BackendKind::KeyVault);
            }
            _ => {
                let error = MaterialBackend::from_selection("key-vault", "")
                    .expect_err("missing vault URI refused");
                assert!(error.contains(KEY_VAULT_URI_ENV));
            }
        }
    }

    /// Unknown selections still fail closed with an actionable message.
    #[test]
    fn unknown_selection_refused_explicitly() {
        use crate::secrets::MaterialBackend;
        let error = MaterialBackend::from_selection("carrier-pigeon", "")
            .expect_err("unknown selection refused");
        assert!(error.contains("carrier-pigeon"));
        assert!(error.contains("local-encrypted"));
    }
}
