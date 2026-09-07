//! OIDC relying party, native username/password login, Web session minting,
//! and Project authorization.
//!
//! Auth modes are explicit: `VOIE_AUTH_MODE` ∈ {native, oidc, both}
//! (default `native`). Native login uses Argon2id credentials in
//! `native_credentials`; OIDC only links an existing User through
//! `auth_identities` and never derives identity or roles from provider
//! claims.

mod authorize;

pub use authorize::{authorize, Action, Role};

use std::collections::HashMap;
use std::convert::Infallible;
use std::error::Error;
use std::fmt;
use std::fs;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{HeaderValue, LOCATION, ORIGIN};
use hyper::{Method, Request, Response, StatusCode};
use openidconnect::core::{
    CoreAuthenticationFlow, CoreClient, CoreIdTokenClaims, CoreProviderMetadata,
};
use openidconnect::{
    AuthType, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet,
    EndpointNotSet, EndpointSet, IssuerUrl, Nonce, RedirectUrl, TokenResponse,
};
use sqlx::{PgPool, Row};
use tokio::net::TcpListener;
use uuid::Uuid;

use crate::web_session::{self, CSRF_HEADER, CSRF_MARKER, OIDC_STATE_COOKIE};
use crate::{Kernel, KernelError};

const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const OIDC_PENDING_TTL: Duration = Duration::from_secs(10 * 60);
const OIDC_STATE_MAX_AGE: u64 = 600;
const OIDC_PROVIDER: &str = "oidc";
const MAX_NATIVE_USERNAME_LEN: usize = 64;
const MAX_NATIVE_PASSWORD_LEN: usize = 256;
const MAX_LOGIN_BODY_BYTES: usize = 2048;
const OIDC_PENDING_MAX: usize = 1024;
const ARGON2_MAX_CONCURRENT: usize = 2;
const LOGIN_WINDOW: Duration = Duration::from_secs(60);
const LOGIN_SOURCE_MAX_FAILURES: u32 = 20;
const LOGIN_ACCOUNT_MAX_FAILURES: u32 = 8;

type DiscoveredClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

/// Which login surfaces are enabled. OIDC is optional; native auth works
/// alone. `AuthMode::from_env` defaults to native.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    Native,
    Oidc,
    Both,
}

impl AuthMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "native" => Some(AuthMode::Native),
            "oidc" => Some(AuthMode::Oidc),
            "both" => Some(AuthMode::Both),
            _ => None,
        }
    }

    pub fn oidc_enabled(self) -> bool {
        matches!(self, AuthMode::Oidc | AuthMode::Both)
    }

    pub fn native_enabled(self) -> bool {
        matches!(self, AuthMode::Native | AuthMode::Both)
    }
}

#[derive(Clone)]
struct OidcSettings {
    issuer_url: String,
    client_id: String,
    client_secret: String,
    redirect_url: String,
}

/// Native admin bootstrap settings resolved from the environment. Secrets
/// are never displayed.
#[derive(Clone)]
pub struct NativeAdminConfig {
    pub username: String,
    pub password: String,
}

/// Web auth configuration. OIDC is optional; native login uses the same
/// opaque cookie. Secrets are never displayed.
#[derive(Clone)]
pub struct AuthConfig {
    mode: AuthMode,
    oidc: Option<OidcSettings>,
    public_origin: String,
    session_ttl: Duration,
    native_admin: Option<NativeAdminConfig>,
}

impl AuthConfig {
    pub fn new(
        issuer_url: impl Into<String>,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        redirect_url: impl Into<String>,
        public_origin: impl Into<String>,
    ) -> Self {
        AuthConfig {
            mode: AuthMode::Oidc,
            oidc: Some(OidcSettings {
                issuer_url: issuer_url.into(),
                client_id: client_id.into(),
                client_secret: client_secret.into(),
                redirect_url: redirect_url.into(),
            }),
            public_origin: public_origin.into().trim_end_matches('/').to_string(),
            session_ttl: DEFAULT_SESSION_TTL,
            native_admin: None,
        }
    }

    /// Native-only configuration: no OIDC discovery, no provider.
    pub fn native(public_origin: impl Into<String>) -> Self {
        AuthConfig {
            mode: AuthMode::Native,
            oidc: None,
            public_origin: public_origin.into().trim_end_matches('/').to_string(),
            session_ttl: DEFAULT_SESSION_TTL,
            native_admin: None,
        }
    }

    pub fn with_session_ttl(mut self, ttl: Duration) -> Self {
        self.session_ttl = ttl;
        self
    }

    /// Reads `VOIE_AUTH_MODE` (default `native`), the public origin, the
    /// complete OIDC client set when OIDC is enabled, and the optional native
    /// admin bootstrap pair (`VOIE_BOOTSTRAP_ADMIN_USERNAME` plus its
    /// password file, with a direct variable fallback).
    pub fn from_env() -> Result<Self, AuthError> {
        let mode = match std::env::var("VOIE_AUTH_MODE") {
            Ok(value) => {
                AuthMode::parse(&value).ok_or(AuthError::Config("VOIE_AUTH_MODE is invalid"))?
            }
            Err(_) => AuthMode::Native,
        };
        let public_origin = env_nonempty("VOIE_PUBLIC_ORIGIN")?;
        let oidc = if mode.oidc_enabled() {
            let issuer_url = env_nonempty("VOIE_OIDC_ISSUER")?;
            let client_id = env_nonempty("VOIE_OIDC_CLIENT_ID")?;
            let redirect_url = env_nonempty("VOIE_OIDC_REDIRECT_URL")?;
            let client_secret = oidc_client_secret()?;
            Some(OidcSettings {
                issuer_url,
                client_id,
                client_secret,
                redirect_url,
            })
        } else {
            None
        };
        let native_admin = native_admin_from_env()?;
        let mut config = AuthConfig {
            mode,
            oidc,
            public_origin: public_origin.trim_end_matches('/').to_string(),
            session_ttl: DEFAULT_SESSION_TTL,
            native_admin,
        };
        if let Ok(seconds) = std::env::var("VOIE_SESSION_TTL_SECS") {
            let seconds: u64 = seconds
                .parse()
                .map_err(|_| AuthError::Config("VOIE_SESSION_TTL_SECS is not a number"))?;
            config.session_ttl = Duration::from_secs(seconds);
        }
        Ok(config)
    }

    pub fn session_ttl(&self) -> Duration {
        self.session_ttl
    }

    pub fn public_origin(&self) -> &str {
        &self.public_origin
    }

    pub fn mode(&self) -> AuthMode {
        self.mode
    }

    pub fn native_admin(&self) -> Option<&NativeAdminConfig> {
        self.native_admin.as_ref()
    }
}

fn env_nonempty(name: &'static str) -> Result<String, AuthError> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(AuthError::Config("missing auth configuration")),
    }
}

fn optional_env(name: &'static str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

fn oidc_client_secret() -> Result<String, AuthError> {
    if let Ok(path) = std::env::var("VOIE_OIDC_CLIENT_SECRET_FILE") {
        let path = path.trim();
        if path.is_empty() {
            return Err(AuthError::Config("VOIE_OIDC_CLIENT_SECRET_FILE is empty"));
        }
        let secret = fs::read_to_string(path)
            .map_err(|_| AuthError::Config("OIDC client secret file is unreadable"))?
            .trim()
            .to_string();
        if secret.is_empty() {
            return Err(AuthError::Config("OIDC client secret is empty"));
        }
        return Ok(secret);
    }
    env_nonempty("VOIE_OIDC_CLIENT_SECRET")
}

/// Resolves the native admin bootstrap pair. The password file is trimmed
/// of exactly one trailing newline (like the OIDC client-secret file
/// handling); the direct variable is the fallback. Either both halves are
/// present or neither is.
fn native_admin_from_env() -> Result<Option<NativeAdminConfig>, AuthError> {
    let username = match optional_env("VOIE_BOOTSTRAP_ADMIN_USERNAME")
        .or_else(|| optional_env("VOIE_NATIVE_ADMIN_USERNAME"))
    {
        Some(username) => username,
        None => {
            if optional_env("VOIE_BOOTSTRAP_ADMIN_PASSWORD").is_some()
                || std::env::var("VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE").is_ok()
                || optional_env("VOIE_NATIVE_ADMIN_PASSWORD").is_some()
                || std::env::var("VOIE_NATIVE_ADMIN_PASSWORD_FILE").is_ok()
            {
                return Err(AuthError::Config(
                    "admin password set without VOIE_BOOTSTRAP_ADMIN_USERNAME",
                ));
            }
            return Ok(None);
        }
    };
    let password = if let Ok(path) = std::env::var("VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE")
        .or_else(|_| std::env::var("VOIE_NATIVE_ADMIN_PASSWORD_FILE"))
    {
        let path = path.trim();
        if path.is_empty() {
            return Err(AuthError::Config("admin password file is empty"));
        }
        let contents = fs::read_to_string(path)
            .map_err(|_| AuthError::Config("native admin password file is unreadable"))?;
        // Trim exactly one trailing newline; interior whitespace is
        // significant.
        let trimmed = contents.strip_suffix('\n').unwrap_or(&contents);
        if trimmed.is_empty() {
            return Err(AuthError::Config("native admin password is empty"));
        }
        trimmed.to_string()
    } else {
        match optional_env("VOIE_BOOTSTRAP_ADMIN_PASSWORD")
            .or_else(|| optional_env("VOIE_NATIVE_ADMIN_PASSWORD"))
        {
            Some(password) => password,
            None => {
                return Err(AuthError::Config(
                    "VOIE_BOOTSTRAP_ADMIN_USERNAME without a password source",
                ));
            }
        }
    };
    Ok(Some(NativeAdminConfig { username, password }))
}

struct PendingLogin {
    nonce: Nonce,
    created: Instant,
}

struct LoginBucket {
    failures: u32,
    window_start: Instant,
}

/// Web auth bound to one PostgreSQL pool. OIDC discovery runs only when
/// OIDC is enabled by the auth mode.
pub struct Auth {
    config: AuthConfig,
    pool: PgPool,
    http: openidconnect::reqwest::Client,
    client: Option<DiscoveredClient>,
    pending: Mutex<HashMap<String, PendingLogin>>,
    login_sources: Mutex<HashMap<String, LoginBucket>>,
    login_accounts: Mutex<HashMap<String, LoginBucket>>,
    argon2_slots: tokio::sync::Semaphore,
}

impl Auth {
    /// Build the relying party. Discovers the issuer only when OIDC is
    /// enabled by the auth mode.
    pub async fn connect(config: AuthConfig, pool: PgPool) -> Result<Self, AuthError> {
        let http = openidconnect::reqwest::ClientBuilder::new()
            .redirect(openidconnect::reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| AuthError::Oidc)?;
        let client = if let Some(oidc) = &config.oidc {
            let issuer = IssuerUrl::new(oidc.issuer_url.clone())
                .map_err(|_| AuthError::Config("OIDC issuer URL is invalid"))?;
            let metadata = CoreProviderMetadata::discover_async(issuer, &http)
                .await
                .map_err(|_| AuthError::Oidc)?;
            if metadata.issuer().as_str() != oidc.issuer_url {
                return Err(AuthError::Oidc);
            }
            let redirect = RedirectUrl::new(oidc.redirect_url.clone())
                .map_err(|_| AuthError::Config("OIDC redirect URL is invalid"))?;
            Some(
                CoreClient::from_provider_metadata(
                    metadata,
                    ClientId::new(oidc.client_id.clone()),
                    Some(ClientSecret::new(oidc.client_secret.clone())),
                )
                .set_auth_type(AuthType::RequestBody)
                .set_redirect_uri(redirect),
            )
        } else {
            None
        };
        Ok(Auth {
            config,
            pool,
            http,
            client,
            pending: Mutex::new(HashMap::new()),
            login_sources: Mutex::new(HashMap::new()),
            login_accounts: Mutex::new(HashMap::new()),
            argon2_slots: tokio::sync::Semaphore::new(ARGON2_MAX_CONCURRENT),
        })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn config(&self) -> &AuthConfig {
        &self.config
    }

    /// Public pre-session capability document. It exposes only route
    /// metadata; it never exposes provider configuration or credentials.
    pub fn capabilities_response(&self) -> Response<Full<Bytes>> {
        let external = if self.config.mode.oidc_enabled() && self.client.is_some() {
            serde_json::json!([{
                "id": "oidc",
                "label": "Continue with external provider",
                "href": "/login/oidc"
            }])
        } else {
            serde_json::json!([])
        };
        let body = serde_json::json!({
            "native": self.config.mode.native_enabled(),
            "external": external,
        })
        .to_string();
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .header("cache-control", "no-store")
            .body(Full::new(Bytes::from(body)))
            .expect("auth capability response headers are valid")
    }

    /// Seeds exactly one platform admin on an empty native deployment. Once
    /// any admin exists, the bootstrap credential is ignored forever; normal
    /// audited admin management owns later password and role changes.
    pub async fn bootstrap_native_admin(&self, kernel: &Kernel) -> Result<(), AuthError> {
        if !self.config.mode.native_enabled()
            || kernel
                .has_platform_admin()
                .await
                .map_err(|_| AuthError::Database)?
        {
            return Ok(());
        }
        let Some(admin) = self.config.native_admin() else {
            return Ok(());
        };
        if !valid_native_username(&admin.username) {
            return Err(AuthError::Config("native admin username is invalid"));
        }
        if admin.password.is_empty() || admin.password.len() > MAX_NATIVE_PASSWORD_LEN {
            return Err(AuthError::Config("native admin password is invalid"));
        }
        if kernel
            .find_user_by_username(admin.username.trim())
            .await
            .map_err(|_| AuthError::Database)?
            .is_some()
        {
            return Err(AuthError::Config(
                "bootstrap username exists but no platform admin exists",
            ));
        }
        let password_hash = hash_password(&admin.password)?;
        let _ = kernel
            .create_native_user(
                Uuid::new_v4(),
                admin.username.trim(),
                &password_hash,
                "admin",
            )
            .await
            .map_err(|_| AuthError::Database)?;
        Ok(())
    }

    pub async fn handle(&self, request: Request<Incoming>) -> Response<Full<Bytes>> {
        match (request.method(), request.uri().path()) {
            (&Method::GET, "/login/oidc") => {
                if self.config.mode.oidc_enabled() {
                    self.oidc_login()
                } else {
                    respond(StatusCode::NOT_FOUND, "not found\n")
                }
            }
            (&Method::POST, "/login") => {
                if self.config.mode.native_enabled() {
                    self.native_login(request).await
                } else {
                    respond(StatusCode::NOT_FOUND, "not found\n")
                }
            }
            (&Method::GET, "/oidc/callback") => {
                if self.config.mode.oidc_enabled() {
                    self.callback(&request).await
                } else {
                    respond(StatusCode::NOT_FOUND, "not found\n")
                }
            }
            (&Method::POST, "/logout") => self.logout(&request).await,
            _ => respond(StatusCode::NOT_FOUND, "not found\n"),
        }
    }

    fn oidc_login(&self) -> Response<Full<Bytes>> {
        let Some(client) = &self.client else {
            return respond(StatusCode::NOT_FOUND, "not found\n");
        };
        let (auth_url, csrf, nonce) = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .url();
        {
            let mut pending = self.pending.lock().unwrap_or_else(|err| err.into_inner());
            pending.retain(|_, item| item.created.elapsed() < OIDC_PENDING_TTL);
            if pending.len() >= OIDC_PENDING_MAX {
                return respond(StatusCode::TOO_MANY_REQUESTS, "too many pending logins\n");
            }
            pending.insert(
                csrf.secret().clone(),
                PendingLogin {
                    nonce,
                    created: Instant::now(),
                },
            );
        }
        let mut response = redirect(StatusCode::FOUND, auth_url.as_str());
        web_session::append_set_cookie(
            &mut response,
            web_session::set_cookie(OIDC_STATE_COOKIE, csrf.secret(), OIDC_STATE_MAX_AGE),
        );
        response
    }

    /// Native username/password login: form-encoded POST, same-origin only.
    /// Success mints the same opaque `voie_session` cookie as every other
    /// login path and answers 303 See Other. An unknown username with a
    /// valid password auto-provisions a regular User (platform role
    /// `user`) with its native credential and personal project scope; only
    /// the env-seeded admin ever carries the `admin` platform role.
    async fn native_login(&self, request: Request<Incoming>) -> Response<Full<Bytes>> {
        if !same_origin(&request, &self.config.public_origin) {
            return respond(StatusCode::FORBIDDEN, "invalid origin\n");
        }
        let source = request_source(&request);
        let body = match collect_login_body(request).await {
            Ok(body) => body,
            Err(StatusCode::PAYLOAD_TOO_LARGE) => {
                return respond(StatusCode::PAYLOAD_TOO_LARGE, "request body too large\n");
            }
            Err(_) => return respond(StatusCode::BAD_REQUEST, "body unreadable\n"),
        };
        let fields: HashMap<String, String> = url::form_urlencoded::parse(body.as_ref())
            .into_owned()
            .collect();
        let Some(username) = fields.get("username").map(|value| value.trim().to_string()) else {
            return respond(StatusCode::UNAUTHORIZED, "invalid credentials\n");
        };
        let Some(password) = fields.get("password") else {
            return respond(StatusCode::UNAUTHORIZED, "invalid credentials\n");
        };
        if username.len() > MAX_NATIVE_USERNAME_LEN || password.len() > MAX_NATIVE_PASSWORD_LEN {
            return respond(StatusCode::UNAUTHORIZED, "invalid credentials\n");
        }
        if !valid_native_username(&username) || password.is_empty() {
            return respond(StatusCode::UNAUTHORIZED, "invalid credentials\n");
        }
        if self.login_throttled(&source, &username) {
            return respond(StatusCode::TOO_MANY_REQUESTS, "invalid credentials\n");
        }
        let user_id = match self.verify_native(&username, password).await {
            Ok(user_id) => {
                self.clear_login_failures(&source, &username);
                user_id
            }
            Err(AuthError::Denied) => {
                self.record_login_failure(&source, &username);
                return respond(StatusCode::UNAUTHORIZED, "invalid credentials\n");
            }
            Err(_) => return respond(StatusCode::INTERNAL_SERVER_ERROR, "login failed\n"),
        };
        match web_session::create(&self.pool, user_id, self.config.session_ttl).await {
            Ok((_session, token)) => {
                let mut response = redirect(StatusCode::SEE_OTHER, "/");
                web_session::append_set_cookie(
                    &mut response,
                    web_session::set_cookie(
                        web_session::COOKIE_NAME,
                        &token,
                        self.config.session_ttl.as_secs(),
                    ),
                );
                response
            }
            Err(_) => respond(StatusCode::INTERNAL_SERVER_ERROR, "login failed\n"),
        }
    }

    /// Verifies one native login. A disabled User is refused. Unknown
    /// usernames still perform Argon2 against a dummy hash so existence is
    /// not a timing oracle; a global semaphore bounds concurrent hashing.
    async fn verify_native(&self, username: &str, password: &str) -> Result<Uuid, AuthError> {
        let _permit = self
            .argon2_slots
            .acquire()
            .await
            .map_err(|_| AuthError::Database)?;
        let row = sqlx::query(
            "select u.id, u.status, nc.password_hash \
             from users u join native_credentials nc on nc.user_id = u.id \
             where u.username = $1",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AuthError::Database)?;
        let (user_id, status, expected) = match row {
            Some(row) => (
                Some(row.get::<Uuid, _>("id")),
                row.get::<String, _>("status"),
                row.get::<String, _>("password_hash"),
            ),
            None => (None, String::new(), dummy_password_hash().to_owned()),
        };
        if !verify_password(password, &expected) {
            return Err(AuthError::Denied);
        }
        if status != "active" {
            return Err(AuthError::Denied);
        }
        user_id.ok_or(AuthError::Denied)
    }

    fn login_throttled(&self, source: &str, username: &str) -> bool {
        bucket_throttled(&self.login_sources, source, LOGIN_SOURCE_MAX_FAILURES)
            || bucket_throttled(&self.login_accounts, username, LOGIN_ACCOUNT_MAX_FAILURES)
    }

    fn record_login_failure(&self, source: &str, username: &str) {
        bump_bucket(&self.login_sources, source);
        bump_bucket(&self.login_accounts, username);
    }

    fn clear_login_failures(&self, source: &str, username: &str) {
        clear_bucket(&self.login_sources, source);
        clear_bucket(&self.login_accounts, username);
    }

    async fn callback(&self, request: &Request<Incoming>) -> Response<Full<Bytes>> {
        let Some(client) = &self.client else {
            return respond(StatusCode::NOT_FOUND, "not found\n");
        };
        let query = request.uri().query().unwrap_or("");
        let params: HashMap<String, String> = url::form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect();
        if params.contains_key("error") {
            return respond(StatusCode::BAD_REQUEST, "oidc error\n");
        }
        let Some(code) = params.get("code").cloned() else {
            return respond(StatusCode::BAD_REQUEST, "missing code\n");
        };
        let Some(state) = params.get("state").cloned() else {
            return respond(StatusCode::BAD_REQUEST, "missing state\n");
        };
        let Some(cookie_state) = web_session::request_cookie(request, OIDC_STATE_COOKIE) else {
            return respond(StatusCode::BAD_REQUEST, "missing oidc state\n");
        };
        if cookie_state != state {
            return respond(StatusCode::BAD_REQUEST, "invalid state\n");
        }
        let nonce = {
            let mut pending = self.pending.lock().unwrap_or_else(|err| err.into_inner());
            match pending.remove(&state) {
                Some(item) if item.created.elapsed() < OIDC_PENDING_TTL => item.nonce,
                _ => return respond(StatusCode::BAD_REQUEST, "invalid state\n"),
            }
        };
        match self.finish_login(client, code, nonce).await {
            Ok(token) => {
                let mut response = redirect(StatusCode::SEE_OTHER, "/");
                web_session::append_set_cookie(
                    &mut response,
                    web_session::clear_cookie(OIDC_STATE_COOKIE),
                );
                web_session::append_set_cookie(
                    &mut response,
                    web_session::set_cookie(
                        web_session::COOKIE_NAME,
                        &token,
                        self.config.session_ttl.as_secs(),
                    ),
                );
                response
            }
            Err(AuthError::Oidc) => respond(StatusCode::BAD_REQUEST, "invalid oidc login\n"),
            Err(_) => respond(StatusCode::INTERNAL_SERVER_ERROR, "login failed\n"),
        }
    }

    async fn finish_login(
        &self,
        client: &DiscoveredClient,
        code: String,
        nonce: Nonce,
    ) -> Result<String, AuthError> {
        let oidc = self.config.oidc.as_ref().ok_or(AuthError::Oidc)?;
        let token_response = client
            .exchange_code(AuthorizationCode::new(code))
            .map_err(|_| AuthError::Oidc)?
            .request_async(&self.http)
            .await
            .map_err(|_| AuthError::Oidc)?;
        let id_token = token_response.id_token().ok_or(AuthError::Oidc)?;
        let claims: &CoreIdTokenClaims = id_token
            .claims(&client.id_token_verifier(), &nonce)
            .map_err(|_| AuthError::Oidc)?;
        if claims.issuer().as_str() != oidc.issuer_url {
            return Err(AuthError::Oidc);
        }
        if !claims
            .audiences()
            .iter()
            .any(|audience| audience.as_str() == oidc.client_id)
        {
            return Err(AuthError::Oidc);
        }
        let subject = claims.subject().as_str().to_string();
        // OIDC only links an explicit identity to a User. It never derives
        // the User id or any role from claims.
        let user_id = link_or_create_oidc_user(&self.pool, &oidc.issuer_url, &subject).await?;
        let (_session, token) = web_session::create(&self.pool, user_id, self.config.session_ttl)
            .await
            .map_err(|_| AuthError::Database)?;
        Ok(token)
    }

    async fn logout(&self, request: &Request<Incoming>) -> Response<Full<Bytes>> {
        if !same_origin(request, &self.config.public_origin) {
            return respond(StatusCode::FORBIDDEN, "invalid origin\n");
        }
        let intent = request
            .headers()
            .get(CSRF_HEADER)
            .and_then(|value| value.to_str().ok());
        if intent != Some(CSRF_MARKER) {
            return respond(StatusCode::FORBIDDEN, "missing csrf marker\n");
        }
        if let Some(token) = web_session::request_cookie(request, web_session::COOKIE_NAME) {
            let _ = web_session::revoke(&self.pool, &token).await;
        }
        let mut response = respond(StatusCode::NO_CONTENT, "");
        web_session::append_set_cookie(
            &mut response,
            web_session::clear_cookie(web_session::COOKIE_NAME),
        );
        response
    }
}

/// Resolves a canonical User through one external identity link. Existing
/// legacy links retain their User id; new Users receive random internal ids.
/// Provider claims never decide role, status, or authorization.
async fn link_or_create_oidc_user(
    pool: &PgPool,
    issuer: &str,
    subject: &str,
) -> Result<Uuid, AuthError> {
    let mut tx = pool.begin().await.map_err(|_| AuthError::Database)?;
    let identity_key = format!("{OIDC_PROVIDER}|{issuer}|{subject}");
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(identity_key)
        .execute(&mut *tx)
        .await
        .map_err(|_| AuthError::Database)?;

    if let Some(row) = sqlx::query(
        "select a.user_id, u.status \
         from auth_identities a join users u on u.id = a.user_id \
         where a.provider = $1 and a.issuer = $2 and a.subject = $3",
    )
    .bind(OIDC_PROVIDER)
    .bind(issuer)
    .bind(subject)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| AuthError::Database)?
    {
        let user_id: Uuid = row.get("user_id");
        let status: String = row.get("status");
        if status != "active" {
            return Err(AuthError::Denied);
        }
        tx.commit().await.map_err(|_| AuthError::Database)?;
        return Ok(user_id);
    }

    let user_id = Uuid::new_v4();
    sqlx::query("insert into users (id, status, platform_role) values ($1, 'active', 'user')")
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AuthError::Database)?;
    sqlx::query(
        "insert into auth_identities (provider, issuer, subject, user_id) \
         values ($1, $2, $3, $4)",
    )
    .bind(OIDC_PROVIDER)
    .bind(issuer)
    .bind(subject)
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| AuthError::Database)?;
    sqlx::query(
        "insert into projects (id, owner_user_id, name, kind) \
         values ($1, $2, 'Personal', 'personal') \
         on conflict (owner_user_id, name) do nothing",
    )
    .bind(Uuid::new_v4())
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| AuthError::Database)?;
    sqlx::query(
        "insert into project_members (project_id, user_id, role) \
         select id, $1, 'owner' from projects \
         where owner_user_id = $1 and kind = 'personal' \
         order by created_at limit 1 \
         on conflict (project_id, user_id) do nothing",
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| AuthError::Database)?;
    tx.commit().await.map_err(|_| AuthError::Database)?;
    Ok(user_id)
}

fn request_source(request: &Request<Incoming>) -> String {
    request
        .headers()
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("direct")
        .to_owned()
}

async fn collect_login_body(request: Request<Incoming>) -> Result<Bytes, StatusCode> {
    if let Some(length) = request
        .headers()
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
    {
        if length > MAX_LOGIN_BODY_BYTES {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
    }
    let mut body = request.into_body();
    let mut out = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| StatusCode::BAD_REQUEST)?;
        if let Some(data) = frame.data_ref() {
            let bytes = data.as_ref();
            if out.len() + bytes.len() > MAX_LOGIN_BODY_BYTES {
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
            out.extend_from_slice(bytes);
        }
    }
    Ok(Bytes::from(out))
}

fn dummy_password_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| {
        hash_password("voie-login-dummy").unwrap_or_else(|_| {
            "$argon2id$v=19$m=16,t=2,p=1$c2FsdHNhbHRzYWx0$MDAwMDAwMDAwMDAwMDAwMA".to_owned()
        })
    })
}

fn bucket_throttled(
    map: &Mutex<HashMap<String, LoginBucket>>,
    key: &str,
    max_failures: u32,
) -> bool {
    let mut map = map.lock().unwrap_or_else(|err| err.into_inner());
    map.retain(|_, bucket| bucket.window_start.elapsed() < LOGIN_WINDOW);
    map.get(key)
        .is_some_and(|bucket| bucket.failures >= max_failures)
}

fn bump_bucket(map: &Mutex<HashMap<String, LoginBucket>>, key: &str) {
    let mut map = map.lock().unwrap_or_else(|err| err.into_inner());
    map.retain(|_, bucket| bucket.window_start.elapsed() < LOGIN_WINDOW);
    match map.get_mut(key) {
        Some(bucket) if bucket.window_start.elapsed() < LOGIN_WINDOW => {
            bucket.failures = bucket.failures.saturating_add(1);
        }
        _ => {
            map.insert(
                key.to_owned(),
                LoginBucket {
                    failures: 1,
                    window_start: Instant::now(),
                },
            );
        }
    }
}

fn clear_bucket(map: &Mutex<HashMap<String, LoginBucket>>, key: &str) {
    let mut map = map.lock().unwrap_or_else(|err| err.into_inner());
    map.remove(key);
}

fn valid_native_username(username: &str) -> bool {
    !username.is_empty()
        && username.len() <= MAX_NATIVE_USERNAME_LEN
        && username
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.')
}

/// Argon2id PHC string. The salt is random per hash; the encoded string
/// carries the parameters, so verification needs no separate columns.
/// Shared with the same-origin account password-change route.
pub(crate) fn hash_password(password: &str) -> Result<String, AuthError> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| AuthError::Config("native password hashing failed"))?;
    Ok(hash.to_string())
}

pub(crate) fn verify_password(password: &str, expected: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(expected) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// Result of one platform-admin account mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminAccountOutcome {
    /// The credential row was written; the caller decides the response.
    Updated,
    /// The request is invalid or the username already exists.
    Conflict,
    /// The target User does not exist.
    NotFound,
}

impl std::fmt::Display for AdminAccountOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdminAccountOutcome::Updated => write!(formatter, "updated"),
            AdminAccountOutcome::Conflict => write!(formatter, "conflict"),
            AdminAccountOutcome::NotFound => write!(formatter, "not found"),
        }
    }
}

/// Creates one canonical User with a native credential via the existing
/// kernel path. The username unique partial index surfaces conflicts as
/// [`AdminAccountOutcome::Conflict`]. Platform-admin surface.
pub async fn admin_create_user(
    kernel: &Kernel,
    username: &str,
    display_name: &str,
    email: Option<&str>,
    platform_role: &str,
    password: &str,
) -> Result<crate::User, AdminAccountOutcome> {
    if !valid_native_username(username) {
        return Err(AdminAccountOutcome::Conflict);
    }
    if password.is_empty() || password.len() > MAX_NATIVE_PASSWORD_LEN {
        return Err(AdminAccountOutcome::Conflict);
    }
    let password_hash = hash_password(password).map_err(|_| AdminAccountOutcome::Conflict)?;
    kernel
        .create_native_user_with_profile(
            Uuid::new_v4(),
            username,
            display_name,
            email,
            platform_role,
            &password_hash,
        )
        .await
        .map_err(|error| match error {
            KernelError::Conflict => AdminAccountOutcome::Conflict,
            KernelError::RelationRefused => AdminAccountOutcome::NotFound,
            _ => AdminAccountOutcome::Conflict,
        })
}

/// Overwrites one User's native credential hash with a fresh Argon2id
/// string. The old password stops verifying on the next login.
/// Platform-admin surface.
pub async fn admin_reset_password(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    password: &str,
) -> Result<AdminAccountOutcome, sqlx::Error> {
    if password.is_empty() || password.len() > MAX_NATIVE_PASSWORD_LEN {
        return Ok(AdminAccountOutcome::Conflict);
    }
    let password_hash = match hash_password(password) {
        Ok(hash) => hash,
        Err(_) => return Ok(AdminAccountOutcome::Conflict),
    };
    // The credential overwrite and the purge of every live Web session of
    // the target commit together: a successful reset can never leave an
    // old cookie valid, and nothing changes when either step fails.
    let mut tx = pool.begin().await?;
    let updated = sqlx::query(
        "update native_credentials set password_hash = $2, updated_at = now() \
         where user_id = $1",
    )
    .bind(user_id)
    .bind(&password_hash)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Ok(AdminAccountOutcome::NotFound);
    }
    sqlx::query("delete from web_sessions where user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(AdminAccountOutcome::Updated)
}

fn same_origin<B>(request: &Request<B>, public_origin: &str) -> bool {
    request
        .headers()
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|origin| origin == public_origin)
}

fn respond(status: StatusCode, body: &'static str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::from_static(body.as_bytes())))
        .expect("static response parts are valid")
}

fn html_owned(status: StatusCode, body: String) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "text/html; charset=utf-8")
        .body(Full::new(Bytes::from(body)))
        .expect("HTML response parts are valid")
}

fn redirect(status: StatusCode, location: &str) -> Response<Full<Bytes>> {
    let mut response = respond(status, "");
    if let Ok(value) = HeaderValue::from_str(location) {
        response.headers_mut().insert(LOCATION, value);
    }
    response
}

/// Typed error at the auth boundary. Display never includes secrets.
#[derive(Debug)]
pub enum AuthError {
    Config(&'static str),
    Oidc,
    Denied,
    /// The actor is a current member but the named Action is not permitted.
    MissingAction(authorize::Action),
    Database,
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthError::Config(message) => write!(f, "configuration: {message}"),
            AuthError::Oidc => write!(f, "oidc authentication failed"),
            AuthError::Denied | AuthError::MissingAction(_) => write!(f, "not authorized"),
            AuthError::Database => write!(f, "database operation failed"),
        }
    }
}

impl Error for AuthError {}

/// Serves the login surface until the task is dropped.
pub async fn serve(listener: TcpListener, auth: Arc<Auth>) -> std::io::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let io = hyper_util::rt::TokioIo::new(stream);
        let auth = auth.clone();
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |request| {
                let auth = auth.clone();
                async move { Ok::<_, Infallible>(auth.handle(request).await) }
            });
            if let Err(error) = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .await
            {
                eprintln!("voie-cloud auth: connection error: {error}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_failure_window_trips_at_the_account_cap() {
        let map = Mutex::new(HashMap::new());
        assert!(!bucket_throttled(&map, "alice", LOGIN_ACCOUNT_MAX_FAILURES));
        for _ in 0..LOGIN_ACCOUNT_MAX_FAILURES {
            bump_bucket(&map, "alice");
        }
        assert!(bucket_throttled(&map, "alice", LOGIN_ACCOUNT_MAX_FAILURES));
        assert!(!bucket_throttled(&map, "bob", LOGIN_ACCOUNT_MAX_FAILURES));
        clear_bucket(&map, "alice");
        assert!(!bucket_throttled(&map, "alice", LOGIN_ACCOUNT_MAX_FAILURES));
    }

    #[test]
    fn login_and_oidc_pending_caps_are_small_and_explicit() {
        assert_eq!(MAX_LOGIN_BODY_BYTES, 2048);
        assert_eq!(OIDC_PENDING_MAX, 1024);
        assert_eq!(LOGIN_SOURCE_MAX_FAILURES, 20);
        assert_eq!(LOGIN_ACCOUNT_MAX_FAILURES, 8);
    }
}
