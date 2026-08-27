//! Opaque server-side Web session: random cookie value, hashed row, expiry, revoke.

use std::time::Duration;

use hyper::header::{HeaderValue, SET_COOKIE};
use hyper::{Request, Response};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub const COOKIE_NAME: &str = "voie_session";
pub const CSRF_HEADER: &str = "x-voie-intent";
pub const CSRF_MARKER: &str = "mutate";
pub const OIDC_STATE_COOKIE: &str = "voie_oidc";

const SESSION_COOKIE_FLAGS: &str = "HttpOnly; Secure; SameSite=Lax; Path=/";

/// Authenticated Web session bound to one User.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSession {
    pub id: Uuid,
    pub user_id: Uuid,
}

/// Hash the cookie secret with SHA-256. Only the hex digest is stored.
pub fn token_hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Cryptographically random opaque cookie secret.
pub fn new_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

pub fn set_cookie(name: &str, value: &str, max_age: u64) -> String {
    format!("{name}={value}; {SESSION_COOKIE_FLAGS}; Max-Age={max_age}")
}

pub fn clear_cookie(name: &str) -> String {
    format!("{name}=; {SESSION_COOKIE_FLAGS}; Max-Age=0")
}

pub fn cookie_value(header: Option<&HeaderValue>, name: &str) -> Option<String> {
    let header = header?.to_str().ok()?;
    header.split(';').find_map(|part| {
        let part = part.trim();
        let (key, value) = part.split_once('=')?;
        (key == name).then(|| value.to_string())
    })
}

pub fn append_set_cookie<B>(response: &mut Response<B>, cookie: String) {
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(SET_COOKIE, value);
    }
}

/// Create one Web session and return the unhashed cookie secret.
pub async fn create(
    pool: &PgPool,
    user_id: Uuid,
    ttl: Duration,
) -> Result<(WebSession, String), sqlx::Error> {
    let token = new_token();
    let hash = token_hash(&token);
    let id = session_id_from_token(&token);
    sqlx::query("insert into web_sessions (id, user_id, token_hash) values ($1, $2, $3)")
        .bind(id)
        .bind(user_id)
        .bind(&hash)
        .execute(pool)
        .await?;
    let _ = ttl;
    Ok((WebSession { id, user_id }, token))
}

/// Derive the database row identity from the opaque cookie secret. The
/// server still generates the secret, but it does not mint a second
/// unrelated product identifier.
fn session_id_from_token(token: &str) -> Uuid {
    let digest = Sha256::digest(token.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

/// Resolve a live session from the cookie secret. Expired or missing rows are none.
pub async fn lookup(
    pool: &PgPool,
    token: &str,
    ttl: Duration,
) -> Result<Option<WebSession>, sqlx::Error> {
    let hash = token_hash(token);
    let ttl_secs = i64::try_from(ttl.as_secs()).unwrap_or(i64::MAX);
    // Membership in web_sessions alone is not authority: a disabled User
    // must never present a still-live cookie, so the owning account must
    // be active for the session to resolve.
    let row = sqlx::query(
        "select w.id, w.user_id from web_sessions w \
         join users u on u.id = w.user_id and u.status = 'active' \
         where w.token_hash = $1 \
           and w.created_at > now() - ($2 * interval '1 second')",
    )
    .bind(&hash)
    .bind(ttl_secs)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| WebSession {
        id: row.get("id"),
        user_id: row.get("user_id"),
    }))
}

/// Server-side revocation: delete the hashed session row.
pub async fn revoke(pool: &PgPool, token: &str) -> Result<(), sqlx::Error> {
    let hash = token_hash(token);
    sqlx::query("delete from web_sessions where token_hash = $1")
        .bind(hash)
        .execute(pool)
        .await?;
    Ok(())
}

/// One active Web session as exposed on the platform-admin surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionListing {
    pub id: Uuid,
    pub user_id: Uuid,
    pub created_at: String,
}

/// Lists one User's Web sessions that are still inside the session TTL.
/// Expired rows are invisible here and are pruned by this call.
pub async fn list_for_user(
    pool: &PgPool,
    user_id: Uuid,
    ttl: Duration,
) -> Result<Vec<SessionListing>, sqlx::Error> {
    let ttl_secs = i64::try_from(ttl.as_secs()).unwrap_or(i64::MAX);
    sqlx::query("delete from web_sessions where created_at <= now() - ($1 * interval '1 second')")
        .bind(ttl_secs)
        .execute(pool)
        .await?;
    let rows = sqlx::query(
        "select id, user_id, created_at::text as created_at from web_sessions \
         where user_id = $1 order by created_at, id",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| SessionListing {
            id: row.get("id"),
            user_id: row.get("user_id"),
            created_at: row.get("created_at"),
        })
        .collect())
}

/// Revokes every Web session of one User; returns the removed row count.
pub async fn revoke_all_for_user(pool: &PgPool, user_id: Uuid) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("delete from web_sessions where user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

pub fn request_cookie<B>(request: &Request<B>, name: &str) -> Option<String> {
    cookie_value(request.headers().get(hyper::header::COOKIE), name)
}
/// Revokes every Web session of one User except one kept session id;
/// returns the removed row count. The self-service password change uses
/// this so the acting browser keeps its cookie while every other surface
/// is forced back to login.
pub async fn revoke_others_for_user(
    pool: &PgPool,
    user_id: Uuid,
    keep_session_id: Uuid,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query("delete from web_sessions where user_id = $1 and id <> $2")
        .bind(user_id)
        .bind(keep_session_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}
