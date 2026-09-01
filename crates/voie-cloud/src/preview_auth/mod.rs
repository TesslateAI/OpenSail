//! Exact-host private preview sessions. The console cookie is never widened.

use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::applications::{self, ApplicationError};
use crate::auth::Action;

pub const PREVIEW_COOKIE: &str = "__Host-voie-preview";
const CODE_TTL_SECS: i64 = 60;
const SESSION_TTL_SECS: i64 = 12 * 60 * 60;
const CUTOVER_TTL_SECS: i64 = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewLogin {
    pub redirect: String,
    pub hostname: String,
}

#[derive(Clone)]
pub struct PreviewAuth {
    pool: PgPool,
}

impl PreviewAuth {
    pub fn new(pool: PgPool) -> Self {
        PreviewAuth { pool }
    }

    /// Console-authenticated start of the exact-host preview handshake.
    pub async fn start_login(
        &self,
        actor_user_id: Uuid,
        application_id: Uuid,
        environment_id: Uuid,
    ) -> Result<PreviewLogin, ApplicationError> {
        let environment = applications::load_environment(&self.pool, environment_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        if environment.application_id != application_id {
            return Err(ApplicationError::NotFound);
        }
        applications::ApplicationStore::new(self.pool.clone(), String::new())
            .require_in_project(actor_user_id, application_id, Action::ReadProject)
            .await?;
        let code = format!("{:x}", Uuid::new_v4().as_u128());
        sqlx::query(
            "insert into preview_codes \
             (code, user_id, application_id, environment_id, hostname, expires_at) \
             values ($1, $2, $3, $4, $5, now() + ($6 * interval '1 second'))",
        )
        .bind(&code)
        .bind(actor_user_id)
        .bind(application_id)
        .bind(environment_id)
        .bind(&environment.hostname)
        .bind(CODE_TTL_SECS)
        .execute(&self.pool)
        .await?;
        Ok(PreviewLogin {
            redirect: format!(
                "https://{}/.voie/auth/callback?code={code}",
                environment.hostname
            ),
            hostname: environment.hostname,
        })
    }

    pub async fn exchange_code(
        &self,
        code: &str,
        hostname: &str,
    ) -> Result<(String, String), ApplicationError> {
        let row = sqlx::query(
            "update preview_codes set consumed_at = now() \
             where code = $1 and hostname = $2 and consumed_at is null and expires_at > now() \
             returning user_id, application_id, environment_id, hostname",
        )
        .bind(code)
        .bind(hostname)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(ApplicationError::Auth)?;
        let token = format!(
            "{:x}{:x}",
            Uuid::new_v4().as_u128(),
            Uuid::new_v4().as_u128()
        );
        let token_hash = Sha256::digest(token.as_bytes());
        sqlx::query(
            "insert into preview_sessions \
             (id, user_id, application_id, environment_id, hostname, token_hash, expires_at) \
             values ($1, $2, $3, $4, $5, $6, now() + ($7 * interval '1 second'))",
        )
        .bind(Uuid::new_v4())
        .bind(row.get::<Uuid, _>("user_id"))
        .bind(row.get::<Uuid, _>("application_id"))
        .bind(row.get::<Uuid, _>("environment_id"))
        .bind(row.get::<String, _>("hostname"))
        .bind(token_hash.as_slice())
        .bind(SESSION_TTL_SECS)
        .execute(&self.pool)
        .await?;
        let cookie = format!(
            "{PREVIEW_COOKIE}={token}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={SESSION_TTL_SECS}"
        );
        Ok((cookie, row.get("hostname")))
    }

    /// Short-lived exact-host cookie for the control-plane wildcard-edge
    /// cutover probe. Never returned to a conversation or Workspace.
    pub async fn mint_session_token(
        &self,
        user_id: Uuid,
        application_id: Uuid,
        environment_id: Uuid,
        hostname: &str,
    ) -> Result<String, ApplicationError> {
        let token = format!(
            "{:x}{:x}",
            Uuid::new_v4().as_u128(),
            Uuid::new_v4().as_u128()
        );
        let token_hash = Sha256::digest(token.as_bytes());
        sqlx::query(
            "insert into preview_sessions \
             (id, user_id, application_id, environment_id, hostname, token_hash, expires_at) \
             values ($1, $2, $3, $4, $5, $6, now() + ($7 * interval '1 second'))",
        )
        .bind(Uuid::new_v4())
        .bind(user_id)
        .bind(application_id)
        .bind(environment_id)
        .bind(hostname)
        .bind(token_hash.as_slice())
        .bind(CUTOVER_TTL_SECS)
        .execute(&self.pool)
        .await?;
        Ok(token)
    }

    /// Edge authorization: cookie plus exact Host must match a live preview session,
    /// or the Environment is public.
    pub async fn authorize(
        &self,
        hostname: &str,
        cookie_value: Option<&str>,
    ) -> Result<bool, ApplicationError> {
        let environment =
            sqlx::query("select id, visibility from application_environments where hostname = $1")
                .bind(hostname)
                .fetch_optional(&self.pool)
                .await?;
        let Some(environment) = environment else {
            return Ok(false);
        };
        let visibility: String = environment.get("visibility");
        if visibility == "public" {
            return Ok(true);
        }
        let Some(cookie_value) = cookie_value else {
            return Ok(false);
        };
        let token_hash = Sha256::digest(cookie_value.as_bytes());
        let hit: bool = sqlx::query_scalar(
            "select exists( \
                select 1 from preview_sessions s \
                join users u on u.id = s.user_id and u.status = 'active' \
                join applications a on a.id = s.application_id \
                join project_members m on m.project_id = a.project_id and m.user_id = s.user_id \
             where s.token_hash = $1 and s.hostname = $2 and s.expires_at > now())",
        )
        .bind(token_hash.as_slice())
        .bind(hostname)
        .fetch_one(&self.pool)
        .await?;
        Ok(hit)
    }

    pub fn set_cookie_header(cookie: &str) -> String {
        cookie.to_owned()
    }
}
