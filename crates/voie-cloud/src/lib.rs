//! `voie-cloud` control state kernel.
//!
//! Owns the PostgreSQL connection pool, the embedded migration sequence,
//! the product resource operations, and liveness/readiness state.
//! The process launcher in `main.rs` wires this library to one HTTP surface.

pub mod activation;
pub mod auth;
pub mod secrets;
pub mod web_session;

use std::convert::Infallible;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::{Connection, PgPool, Row};
use tokio::fs;
use uuid::Uuid;

pub mod exec_journal;
pub mod fabric_client;
pub mod integration;
pub mod model;
pub mod session_store;

const LATEST_MIGRATION: i64 = 10;

/// Process configuration resolved from the environment. The database URL is
/// never rendered in logs or errors.
pub struct Config {
    database_url: String,
}

impl Config {
    pub fn database_url(database_url: impl Into<String>) -> Self {
        Config {
            database_url: database_url.into(),
        }
    }

    /// Reads the database URL from `VOIE_DATABASE_URL`.
    pub fn from_env() -> Result<Self, KernelError> {
        // The DSN may arrive as a 0640 credential file delivered by
        // deployment; a plain environment value remains valid for local dev.
        let database_url = match std::env::var("VOIE_DATABASE_URL_FILE") {
            Ok(path) if !path.trim().is_empty() => std::fs::read_to_string(path.trim())
                .map_err(|_| KernelError::Config("database URL file is unreadable"))?,
            _ => std::env::var("VOIE_DATABASE_URL").unwrap_or_default(),
        };
        if database_url.trim().is_empty() {
            Err(KernelError::Config("VOIE_DATABASE_URL is not set or empty"))
        } else {
            Ok(Config { database_url })
        }
    }
}

/// Typed error at the package boundary. Display never includes the
/// database URL or other secret material.
#[derive(Debug)]
pub enum KernelError {
    Config(&'static str),
    Database,
    /// A required related resource does not exist; the store refused the row.
    RelationRefused,
    Conflict,
    Quota,
    InvalidState,
}

impl fmt::Display for KernelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KernelError::Config(message) => write!(f, "configuration: {message}"),
            KernelError::Database => write!(f, "database operation failed"),
            KernelError::RelationRefused => {
                write!(f, "reference to a missing related resource was refused")
            }
            KernelError::Conflict => write!(f, "resource identity or request conflicts"),
            KernelError::Quota => write!(f, "project workspace quota reached"),
            KernelError::InvalidState => write!(f, "resource state transition was refused"),
        }
    }
}

impl Error for KernelError {}

impl From<sqlx::Error> for KernelError {
    fn from(error: sqlx::Error) -> Self {
        if let sqlx::Error::Database(inner) = &error {
            match inner.kind() {
                sqlx::error::ErrorKind::ForeignKeyViolation => return KernelError::RelationRefused,
                // Caller-supplied identities and unique names surface as
                // conflicts, exactly like the explicit intent checks.
                sqlx::error::ErrorKind::UniqueViolation => return KernelError::Conflict,
                _ => {}
            }
        }
        KernelError::Database
    }
}

/// One Project owned by one User.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub name: String,
    /// Collaboration scope: `personal` is a single-user scope, `team` is a
    /// multi-user collaboration scope. There is no first-class Teams table.
    pub kind: String,
}

/// One canonical VOIE User. Providers authenticate or link Users; they never
/// own them. `username` is the canonical native login name; OIDC-linked
/// users keep their deterministic issuer/subject identity and may claim a
/// username later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: Uuid,
    pub username: Option<String>,
    pub display_name: String,
    pub email: Option<String>,
    /// `active` or `disabled`. A disabled User cannot authenticate.
    pub status: String,
    /// Explicit platform role: `admin` manages users/scopes/Fabrics/
    /// underlay; `user` is the default. Never derived from provider claims.
    pub platform_role: String,
    pub created_at: String,
    pub updated_at: String,
}

/// One project-owned Agent configuration. It is not an IAM principal.
#[derive(Debug, Clone, PartialEq)]
pub struct Agent {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub model: String,
    pub system_prompt: String,
    /// The one bounded execution capability. There is no generic tool list.
    pub bash_enabled: bool,
    pub max_tokens: i32,
}

/// One fixed Fabric resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fabric {
    pub id: Uuid,
    pub name: String,
}

/// One Workspace bound to exactly one Fabric.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub id: Uuid,
    pub fabric_id: Uuid,
    /// The one Project that owns this Workspace. Ownership decides every
    /// authorization answer; there is no ownerless Workspace.
    pub project_id: Uuid,
    /// Durable execution generation. It advances only after the Fabric
    /// confirms a replacement; workspace bytes survive the swap.
    pub exec_generation: i64,
    /// Lifecycle fence: a `fenced` Workspace accepts no new Sessions and no
    /// second lifecycle operation, so delete and replace serialize against
    /// each other and against attachment at the database row itself.
    pub state: WorkspaceState,
}

/// Durable Workspace lifecycle states.
///
/// `Creating` is a durable reservation made before invoking the Fabric: an
/// indeterminate create (Fabricd's Unknown verdict, HTTP 202) keeps the row
/// in `creating` instead of exposing it as ready. Only the Fabric's own
/// 200 success promotes it to `ready`; definite refusals (non-2xx) release
/// the reservation. Existing invariants hold: `creating` rows accept no new
/// Sessions and no second lifecycle operation, just like `fenced`, so the
/// row is never visible as usable truth. Reconciliation is a read-only
/// existence probe on the next user-initiated create for the same identity
/// — without automatically retrying the unknown create — which either
/// activates the row (Fabric holds it) or discards it (Fabric 404).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceState {
    Creating,
    Ready,
    Fenced,
}

impl WorkspaceState {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkspaceState::Creating => "creating",
            WorkspaceState::Ready => "ready",
            WorkspaceState::Fenced => "fenced",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "creating" => Some(WorkspaceState::Creating),
            "ready" => Some(WorkspaceState::Ready),
            "fenced" => Some(WorkspaceState::Fenced),
            _ => None,
        }
    }
}

/// Maximum durable Workspaces a Project may own. Exhaustion of the
/// shared LVM pool is bounded by this small explicit quota.
pub const MAX_WORKSPACES_PER_PROJECT: i64 = 8;

/// Durable Run state. The state machine is owned by voie-cloud.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Accepted,
    Dispatched,
    Terminal,
    Unknown,
    Cancelled,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            RunState::Accepted => "accepted",
            RunState::Dispatched => "dispatched",
            RunState::Terminal => "terminal",
            RunState::Unknown => "unknown",
            RunState::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "accepted" => Some(RunState::Accepted),
            "dispatched" => Some(RunState::Dispatched),
            "terminal" => Some(RunState::Terminal),
            "unknown" => Some(RunState::Unknown),
            "cancelled" => Some(RunState::Cancelled),
            _ => None,
        }
    }
}

/// One durable Run resource and its retained terminal result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: Uuid,
    pub intent_id: Uuid,
    pub session_id: Uuid,
    pub request_hash: Vec<u8>,
    pub mode: String,
    pub prompt: String,
    pub state: RunState,
    pub result: Option<String>,
    /// The human actor who queued this Run, when one exists. Supervisor
    /// recovery replays leave it unset.
    pub actor_user_id: Option<Uuid>,
    /// Durable per-session turn ordinal. Follow-ups queue behind their
    /// predecessor and dispatch only after it settles.
    pub seq: i64,
    pub accepted_at: String,
    pub dispatched_at: Option<String>,
    pub cancel_requested_at: Option<String>,
    pub terminal_at: Option<String>,
    pub cancelled_at: Option<String>,
}

/// Metadata-only audit row. Audit is never an execution dependency.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEvent {
    pub seq: i64,
    pub project_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub actor_user_id: Option<Uuid>,
    pub occurred_at: String,
    /// Stable public event name; also carries the canonical action verb.
    pub kind: String,
    /// The acted-on resource type, e.g. `project`, `run`, `member`.
    pub resource_type: String,
    pub resource_id: Option<Uuid>,
    pub outcome: String,
    /// Structured context decoded from JSONB; free-form legacy text stays in
    /// payload.
    pub metadata: Option<serde_json::Value>,
    pub payload: Option<String>,
}

/// Outcome of one audited action. Rows are written after the attempt; a
/// refused or failed action never invalidates the work itself (audit is not
/// an execution dependency).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOutcome {
    Ok,
    Refused,
    Error,
    Unknown,
}

impl AuditOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditOutcome::Ok => "ok",
            AuditOutcome::Refused => "refused",
            AuditOutcome::Error => "error",
            AuditOutcome::Unknown => "unknown",
        }
    }
}

/// One normalized audit emission. Every field except identity columns is
/// explicit so rows are self-describing without parsing `kind`.
#[derive(Debug, Clone)]
pub struct AuditInsert<'a> {
    pub project_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    /// The authenticated human actor, when one exists. System paths leave it
    /// unset rather than minting a pseudo-user.
    pub actor_user_id: Option<Uuid>,
    pub kind: &'a str,
    pub resource_type: &'a str,
    pub resource_id: Option<Uuid>,
    pub outcome: AuditOutcome,
    /// Structured JSON context serialized into the row.
    pub metadata: Option<&'a serde_json::Value>,
}

/// One Session bound to an Agent and a Workspace inside a Project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: Uuid,
    pub project_id: Uuid,
    pub agent_id: Uuid,
    pub workspace_id: Uuid,
    pub writer_generation: i64,
    pub attention_generation: i64,
    pub head_revision: i64,
    /// The last human who queued a Run on this Session.
    pub last_actor_user_id: Option<Uuid>,
}

fn project_row(row: PgRow) -> Project {
    Project {
        id: row.get("id"),
        owner_user_id: row.get("owner_user_id"),
        name: row.get("name"),
        kind: row.get("kind"),
    }
}

fn user_row(row: PgRow) -> User {
    User {
        id: row.get("id"),
        username: row.get("username"),
        display_name: row.get("display_name"),
        email: row.get("email"),
        status: row.get("status"),
        platform_role: row.get("platform_role"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

fn session_row(row: PgRow) -> Session {
    Session {
        id: row.get("id"),
        project_id: row.get("project_id"),
        agent_id: row.get("agent_id"),
        workspace_id: row.get("workspace_id"),
        writer_generation: row.get("writer_generation"),
        attention_generation: row.get("attention_generation"),
        head_revision: row.get("head_revision"),
        last_actor_user_id: row.get("last_actor_user_id"),
    }
}

fn agent_row(row: PgRow) -> Agent {
    Agent {
        id: row.get("id"),
        project_id: row.get("project_id"),
        name: row.get("name"),
        model: row.get("model"),
        system_prompt: row.get("system_prompt"),
        bash_enabled: row.get("bash_enabled"),
        max_tokens: row.get("max_tokens"),
    }
}

fn fabric_row(row: PgRow) -> Fabric {
    Fabric {
        id: row.get("id"),
        name: row.get("name"),
    }
}

fn workspace_row(row: PgRow) -> Workspace {
    Workspace {
        id: row.get("id"),
        fabric_id: row.get("fabric_id"),
        project_id: row.get("project_id"),
        exec_generation: row.get("exec_generation"),
        state: WorkspaceState::parse(row.get::<String, _>("state").as_str())
            .unwrap_or(WorkspaceState::Ready),
    }
}

fn run_row(row: PgRow) -> Run {
    Run {
        id: row.get("id"),
        intent_id: row.get("intent_id"),
        session_id: row.get("session_id"),
        request_hash: row.get("request_hash"),
        mode: row.get("mode"),
        prompt: row.get("prompt"),
        state: RunState::parse(row.get::<String, _>("state").as_str()).unwrap_or(RunState::Unknown),
        result: row.get("result"),
        actor_user_id: row.get("actor_user_id"),
        seq: row.get("seq"),
        accepted_at: row.get("accepted_at"),
        dispatched_at: row.get("dispatched_at"),
        cancel_requested_at: row.get("cancel_requested_at"),
        terminal_at: row.get("terminal_at"),
        cancelled_at: row.get("cancelled_at"),
    }
}

const FIND_PROJECT_SQL: &str = "select id, owner_user_id, name, kind from projects where id = $1";
const FIND_SESSION_SQL: &str = "select id, project_id, agent_id, workspace_id, writer_generation, attention_generation, head_revision, last_actor_user_id from sessions where id = $1";
const RUN_COLUMNS: &str = "id, intent_id, session_id, request_hash, mode, prompt, state, result, \
    actor_user_id, seq, \
    accepted_at::text as accepted_at, dispatched_at::text as dispatched_at, \
    cancel_requested_at::text as cancel_requested_at, \
    terminal_at::text as terminal_at, cancelled_at::text as cancelled_at";

/// The state kernel: one pool, direct SQL, embedded migrations.
pub struct Kernel {
    pool: PgPool,
}

impl Kernel {
    pub async fn connect(config: &Config) -> Result<Self, KernelError> {
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(Duration::from_secs(5))
            .connect(&config.database_url)
            .await?;
        Ok(Kernel { pool })
    }

    /// Pool handle for focused tests and the later activation bridge.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Applies the embedded migration sequence; repeat application is safe.
    pub async fn migrate(&self) -> Result<(), KernelError> {
        let mut connection = self.pool.acquire().await?;
        // Serialize concurrent process starts; one transaction per version so
        // a failure never leaves a half-applied migration behind.
        sqlx::query("create table if not exists schema_migrations (version bigint primary key)")
            .execute(&mut *connection)
            .await?;
        sqlx::query("select pg_advisory_lock($1, $2)")
            .bind(0x766F_6965_i32)
            .bind(1_i32)
            .execute(&mut *connection)
            .await?;
        let result: Result<(), KernelError> = async {
            apply_version(
                &mut connection,
                1,
                include_str!("../migrations/0001_release0_kernel.sql"),
            )
            .await?;
            apply_version(
                &mut connection,
                2,
                include_str!("../migrations/0002_backend_vertical.sql"),
            )
            .await?;
            apply_version(
                &mut connection,
                3,
                include_str!("../migrations/0003_runs.sql"),
            )
            .await?;
            apply_version(
                &mut connection,
                4,
                include_str!("../migrations/0004_control_gaps.sql"),
            )
            .await?;
            apply_version(
                &mut connection,
                5,
                include_str!("../migrations/0005_workspace_replace.sql"),
            )
            .await?;
            apply_version(
                &mut connection,
                6,
                include_str!("../migrations/0006_workspace_creating.sql"),
            )
            .await?;
            apply_version(
                &mut connection,
                7,
                include_str!("../migrations/0007_native_auth.sql"),
            )
            .await?;
            apply_version(
                &mut connection,
                8,
                include_str!("../migrations/0008_user_secrets.sql"),
            )
            .await?;
            apply_version(
                &mut connection,
                9,
                include_str!("../migrations/0009_workspace_creator.sql"),
            )
            .await?;
            apply_version(
                &mut connection,
                10,
                include_str!("../migrations/0010_workspace_label.sql"),
            )
            .await?;
            Ok(())
        }
        .await;
        let _ = sqlx::query("select pg_advisory_unlock($1, $2)")
            .bind(0x766F_6965_i32)
            .bind(1_i32)
            .execute(&mut *connection)
            .await;
        result?;
        Ok(())
    }

    /// Readiness performs current database work and reports true only while
    /// PostgreSQL is usable and every embedded migration is applied.
    pub async fn ready(&self) -> bool {
        let Ok(mut connection) = self.pool.acquire().await else {
            return false;
        };
        if sqlx::query("select 1")
            .execute(&mut *connection)
            .await
            .is_err()
        {
            return false;
        }
        let applied = match sqlx::query_scalar::<_, i64>("select count(*) from schema_migrations")
            .fetch_one(&mut *connection)
            .await
        {
            Ok(applied) => applied,
            Err(_) => return false,
        };
        applied == LATEST_MIGRATION
    }

    /// Creates one owned Project together with its owner membership row in
    /// one transaction: a project never exists without its owner member.
    /// `kind` is the collaboration scope: `personal` (single-user) or
    /// `team` (multi-user).
    pub async fn create_project(
        &self,
        id: Uuid,
        owner_user_id: Uuid,
        name: &str,
        kind: &str,
    ) -> Result<Project, KernelError> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "insert into projects (id, owner_user_id, name, kind) \
             values ($1, $2, $3, $4) returning id, owner_user_id, name, kind",
        )
        .bind(id)
        .bind(owner_user_id)
        .bind(name)
        .bind(kind)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "insert into project_members (project_id, user_id, role) values ($1, $2, 'owner')",
        )
        .bind(id)
        .bind(owner_user_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(project_row(row))
    }

    /// Reads one Project by identity.
    pub async fn find_project(&self, id: Uuid) -> Result<Option<Project>, KernelError> {
        let row = sqlx::query(FIND_PROJECT_SQL)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(project_row))
    }

    /// Reads one canonical User by identity.
    pub async fn find_user(&self, id: Uuid) -> Result<Option<User>, KernelError> {
        let row = sqlx::query(
            "select id, username, display_name, email, status, platform_role, \
                    created_at::text as created_at, updated_at::text as updated_at \
             from users where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(user_row))
    }

    /// Reads one canonical User by native username.
    pub async fn find_user_by_username(&self, username: &str) -> Result<Option<User>, KernelError> {
        let row = sqlx::query(
            "select id, username, display_name, email, status, platform_role, \
                    created_at::text as created_at, updated_at::text as updated_at \
             from users where username = $1",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(user_row))
    }

    /// Lists every canonical User. Platform-admin surface.
    pub async fn list_users(&self) -> Result<Vec<User>, KernelError> {
        let rows = sqlx::query(
            "select id, username, display_name, email, status, platform_role, \
                    created_at::text as created_at, updated_at::text as updated_at \
             from users order by created_at, id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(user_row).collect())
    }
    /// Whether the platform has an explicitly granted administrator. Native
    /// bootstrap is allowed only while this remains false.
    pub async fn has_platform_admin(&self) -> Result<bool, KernelError> {
        let exists: bool =
            sqlx::query_scalar("select exists(select 1 from users where platform_role = 'admin')")
                .fetch_one(&self.pool)
                .await?;
        Ok(exists)
    }

    /// Creates one canonical User with a native credential and its personal
    /// project scope in one transaction. The first bootstrap User becomes
    /// the platform admin; every later User is a regular user. A repeated
    /// username is a conflict.
    pub async fn create_native_user(
        &self,
        id: Uuid,
        username: &str,
        password_hash: &str,
        platform_role: &str,
    ) -> Result<User, KernelError> {
        // Bootstrap keeps the historical display-name-from-username behavior.
        self.create_native_user_with_profile(
            id,
            username,
            username,
            None,
            platform_role,
            password_hash,
        )
        .await
    }

    /// Creates one canonical User with an explicit profile (display name and
    /// optional email) plus native credential and personal project scope in
    /// one transaction. A repeated username is a conflict.
    pub async fn create_native_user_with_profile(
        &self,
        id: Uuid,
        username: &str,
        display_name: &str,
        email: Option<&str>,
        platform_role: &str,
        password_hash: &str,
    ) -> Result<User, KernelError> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "insert into users (id, username, display_name, email, status, platform_role) \
             values ($1, $2, $3, $4, 'active', $5) \
             returning id, username, display_name, email, status, platform_role, \
                       created_at::text as created_at, updated_at::text as updated_at",
        )
        .bind(id)
        .bind(username)
        .bind(display_name)
        .bind(email)
        .bind(platform_role)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("insert into native_credentials (user_id, password_hash) values ($1, $2)")
            .bind(id)
            .bind(password_hash)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "insert into auth_identities (provider, issuer, subject, user_id) \
             values ('native', 'native', $2, $1)",
        )
        .bind(id)
        .bind(username)
        .execute(&mut *tx)
        .await?;
        // Every User owns exactly one personal project scope.
        let personal_id = Uuid::new_v4();
        sqlx::query(
            "insert into projects (id, owner_user_id, name, kind) \
             values ($1, $2, 'Personal', 'personal')",
        )
        .bind(personal_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "insert into project_members (project_id, user_id, role) values ($1, $2, 'owner')",
        )
        .bind(personal_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(user_row(row))
    }

    /// Resolves or creates the personal project scope for an existing User
    /// (e.g. an OIDC-linked User). Idempotent: a User never owns two
    /// personal projects.
    pub async fn ensure_personal_project(&self, user_id: Uuid) -> Result<Project, KernelError> {
        let existing = sqlx::query(
            "select id, owner_user_id, name, kind from projects \
             where owner_user_id = $1 and kind = 'personal' order by created_at limit 1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(row) = existing {
            return Ok(project_row(row));
        }
        let mut tx = self.pool.begin().await?;
        let id = Uuid::new_v4();
        let row = sqlx::query(
            "insert into projects (id, owner_user_id, name, kind) \
             values ($1, $2, 'Personal', 'personal') \
             on conflict do nothing \
             returning id, owner_user_id, name, kind",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
        let project = match row {
            Some(row) => project_row(row),
            None => {
                // A concurrent bootstrap won the race; read the winner.
                let row = sqlx::query(
                    "select id, owner_user_id, name, kind from projects \
                     where owner_user_id = $1 and kind = 'personal' order by created_at limit 1",
                )
                .bind(user_id)
                .fetch_one(&mut *tx)
                .await?;
                project_row(row)
            }
        };
        sqlx::query(
            "insert into project_members (project_id, user_id, role) values ($1, $2, 'owner') \
             on conflict (project_id, user_id) do nothing",
        )
        .bind(project.id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(project)
    }

    /// Sets the platform role of one User. Platform-admin surface.
    pub async fn set_platform_role(
        &self,
        user_id: Uuid,
        platform_role: &str,
    ) -> Result<bool, KernelError> {
        let updated =
            sqlx::query("update users set platform_role = $2, updated_at = now() where id = $1")
                .bind(user_id)
                .bind(platform_role)
                .execute(&self.pool)
                .await?;
        Ok(updated.rows_affected() == 1)
    }

    /// Sets the durable status of one User. A disabled User cannot
    /// authenticate. Platform-admin surface.
    pub async fn set_user_status(&self, user_id: Uuid, status: &str) -> Result<bool, KernelError> {
        let updated = sqlx::query("update users set status = $2, updated_at = now() where id = $1")
            .bind(user_id)
            .bind(status)
            .execute(&self.pool)
            .await?;
        Ok(updated.rows_affected() == 1)
    }

    /// Replaces the Argon2id credential of one User. Native login only.
    pub async fn set_native_password(
        &self,
        user_id: Uuid,
        password_hash: &str,
    ) -> Result<bool, KernelError> {
        let updated = sqlx::query(
            "insert into native_credentials (user_id, password_hash) values ($1, $2) \
             on conflict (user_id) do update set password_hash = excluded.password_hash, \
                 updated_at = now()",
        )
        .bind(user_id)
        .bind(password_hash)
        .execute(&self.pool)
        .await?;
        Ok(updated.rows_affected() == 1)
    }

    /// Reads the Argon2id credential row for one User, when one exists.
    pub async fn find_native_credential(
        &self,
        user_id: Uuid,
    ) -> Result<Option<String>, KernelError> {
        let row = sqlx::query_scalar::<_, String>(
            "select password_hash from native_credentials where user_id = $1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Links one provider identity to an existing User. A provider
    /// (issuer, subject) never controls authorization; it only
    /// authenticates. A pair already linked to another User is a conflict.
    pub async fn link_identity(
        &self,
        user_id: Uuid,
        provider: &str,
        issuer: &str,
        subject: &str,
    ) -> Result<(), KernelError> {
        let inserted = sqlx::query(
            "insert into auth_identities (provider, issuer, subject, user_id) \
             values ($1, $2, $3, $4) \
             on conflict (provider, issuer, subject) do nothing",
        )
        .bind(provider)
        .bind(issuer)
        .bind(subject)
        .bind(user_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if inserted == 1 {
            return Ok(());
        }
        let linked_to: Uuid = sqlx::query_scalar(
            "select user_id from auth_identities \
                 where provider = $1 and issuer = $2 and subject = $3",
        )
        .bind(provider)
        .bind(issuer)
        .bind(subject)
        .fetch_one(&self.pool)
        .await?;
        if linked_to == user_id {
            Ok(())
        } else {
            Err(KernelError::Conflict)
        }
    }

    /// Resolves the User linked to one provider identity, when one exists.
    pub async fn find_user_by_identity(
        &self,
        provider: &str,
        issuer: &str,
        subject: &str,
    ) -> Result<Option<Uuid>, KernelError> {
        let row = sqlx::query_scalar::<_, Uuid>(
            "select user_id from auth_identities \
             where provider = $1 and issuer = $2 and subject = $3",
        )
        .bind(provider)
        .bind(issuer)
        .bind(subject)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Creates one Session under an existing Project, bound to an Agent and a
    /// Workspace. A missing Project, Agent, or Workspace is refused.
    pub async fn create_session(
        &self,
        id: Uuid,
        project_id: Uuid,
        agent_id: Uuid,
        workspace_id: Uuid,
    ) -> Result<Session, KernelError> {
        // The row lock inside EXISTS serializes this attachment against the
        // deletion fence: whichever transaction claims the Workspace row
        // first, the other re-evaluates `state = 'ready'` against the
        // committed truth, so a Session can never attach to a fenced
        // Workspace.
        let row = sqlx::query(&format!(
            "insert into sessions \
             (id, project_id, agent_id, workspace_id) \
             select $1, $2, $3, $4 \
             where exists( \
                 select 1 from workspaces w \
                 where w.id = $4 and w.state = 'ready' and w.project_id = $2 \
                 for update \
             ) \
             returning id, project_id, agent_id, workspace_id, \
                       writer_generation, attention_generation, head_revision, \
                       last_actor_user_id",
        ))
        .bind(id)
        .bind(project_id)
        .bind(agent_id)
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(KernelError::RelationRefused)?;
        Ok(session_row(row))
    }

    /// Reads one Session by identity.
    pub async fn find_session(&self, id: Uuid) -> Result<Option<Session>, KernelError> {
        let row = sqlx::query(FIND_SESSION_SQL)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(session_row))
    }

    /// Atomically creates one Session and its first accepted Run in one
    /// transaction: a conversation never exists without its first message.
    /// The Run carries the durable per-session turn ordinal 1 and the human
    /// actor. A missing Project, Agent, or Workspace is refused; a repeated
    /// Session identity is a conflict.
    pub async fn create_conversation(
        &self,
        session_id: Uuid,
        project_id: Uuid,
        agent_id: Uuid,
        workspace_id: Uuid,
        run_id: Uuid,
        intent_id: Uuid,
        request_hash: &[u8; 32],
        prompt: &str,
        actor_user_id: Uuid,
    ) -> Result<(Session, Run), KernelError> {
        let mut tx = self.pool.begin().await?;
        let (intent_key1, intent_key2) = session_advisory_keys(intent_id);
        sqlx::query("select pg_advisory_xact_lock($1, $2)")
            .bind(intent_key1)
            .bind(intent_key2)
            .execute(&mut *tx)
            .await?;
        if let Some(existing_row) = sqlx::query(&format!(
            "select {RUN_COLUMNS} from runs where intent_id = $1"
        ))
        .bind(intent_id)
        .fetch_optional(&mut *tx)
        .await?
        {
            let existing_run = run_row(existing_row);
            if existing_run.session_id != session_id
                || existing_run.request_hash.as_slice() != request_hash.as_slice()
                || existing_run.prompt != prompt
            {
                return Err(KernelError::Conflict);
            }
            let existing_session = sqlx::query(FIND_SESSION_SQL)
                .bind(session_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(KernelError::Conflict)?;
            tx.commit().await?;
            return Ok((session_row(existing_session), existing_run));
        }
        let session = sqlx::query(
            "insert into sessions \
             (id, project_id, agent_id, workspace_id, last_actor_user_id) \
             select $1, $2, $3, $4, $5 \
             where exists( \
                 select 1 from workspaces w \
                 where w.id = $4 and w.state = 'ready' and w.project_id = $2 \
                 for update \
             ) \
             returning id, project_id, agent_id, workspace_id, \
                       writer_generation, attention_generation, head_revision, \
                       last_actor_user_id",
        )
        .bind(session_id)
        .bind(project_id)
        .bind(agent_id)
        .bind(workspace_id)
        .bind(actor_user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(KernelError::RelationRefused)?;
        let run = sqlx::query(&format!(
            "insert into runs \
            (id, intent_id, session_id, request_hash, mode, prompt, state, actor_user_id, seq) \
             values ($1, $2, $3, $4, 'create', $5, 'accepted', $6, 1) \
             returning {RUN_COLUMNS}"
        ))
        .bind(run_id)
        .bind(intent_id)
        .bind(session_id)
        .bind(request_hash.as_slice())
        .bind(prompt)
        .bind(actor_user_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok((session_row(session), run_row(run)))
    }

    /// Reads one project-owned Agent configuration.
    pub async fn find_agent(&self, id: Uuid) -> Result<Option<Agent>, KernelError> {
        let row = sqlx::query(
            "select id, project_id, name, model, system_prompt, bash_enabled, max_tokens \
             from agents where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(agent_row))
    }

    /// Creates an Agent with caller-supplied identity and explicit
    /// configuration. Secrets and provider credentials are not Agent fields.
    pub async fn create_agent(
        &self,
        id: Uuid,
        project_id: Uuid,
        name: &str,
        model: &str,
        system_prompt: &str,
        bash_enabled: bool,
        max_tokens: i32,
    ) -> Result<Agent, KernelError> {
        let row = sqlx::query(
            "insert into agents \
             (id, project_id, name, model, system_prompt, bash_enabled, max_tokens) \
             values ($1, $2, $3, $4, $5, $6, $7) \
             returning id, project_id, name, model, system_prompt, bash_enabled, max_tokens",
        )
        .bind(id)
        .bind(project_id)
        .bind(name)
        .bind(model)
        .bind(system_prompt)
        .bind(bash_enabled)
        .bind(max_tokens)
        .fetch_one(&self.pool)
        .await?;
        Ok(agent_row(row))
    }

    /// Creates one fixed Fabric resource with caller-supplied identity.
    pub async fn create_fabric(&self, id: Uuid, name: &str) -> Result<Fabric, KernelError> {
        let row = sqlx::query("insert into fabrics (id, name) values ($1, $2) returning id, name")
            .bind(id)
            .bind(name)
            .fetch_one(&self.pool)
            .await?;
        Ok(fabric_row(row))
    }

    pub async fn find_fabric(&self, id: Uuid) -> Result<Option<Fabric>, KernelError> {
        let row = sqlx::query("select id, name from fabrics where id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(fabric_row))
    }

    /// Creates one Workspace owned by the Project and bound to a pre-existing
    /// Fabric. A missing Project or Fabric is refused.
    ///
    /// Compatibility shim: inserts in `ready` state for code that already
    /// proved the Fabric holds the resource (e.g. test seed helpers).
    /// Production creation must use `reserve_workspace` + `activate_workspace`
    /// so indeterminate Fabric creates leave a reconcilable `creating` row.
    pub async fn create_workspace(
        &self,
        id: Uuid,
        project_id: Uuid,
        fabric_id: Uuid,
    ) -> Result<Workspace, KernelError> {
        let row = sqlx::query(
            "insert into workspaces (id, project_id, fabric_id) values ($1, $2, $3)              returning id, project_id, fabric_id, exec_generation, state",
        )
        .bind(id)
        .bind(project_id)
        .bind(fabric_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(workspace_row(row))
    }

    /// Durably reserves a Workspace identity as `creating` before any
    /// external Fabric effect. An indeterminate Fabric outcome (HTTP 202
    /// Unknown) must keep this row; only a Fabric 200 promotes it. The
    /// creator is recorded durably for the workspace creator rules.
    pub async fn reserve_workspace(
        &self,
        id: Uuid,
        project_id: Uuid,
        fabric_id: Uuid,
        created_by_user_id: Uuid,
    ) -> Result<(), KernelError> {
        let mut tx = self.pool.begin().await?;
        let lock_key: i64 = (project_id.as_u128() as u64) as i64;
        sqlx::query("select pg_advisory_xact_lock($1)")
            .bind(lock_key)
            .execute(&mut *tx)
            .await?;
        let count: i64 =
            sqlx::query_scalar("select count(*) from workspaces where project_id = $1")
                .bind(project_id)
                .fetch_one(&mut *tx)
                .await?;
        if count >= MAX_WORKSPACES_PER_PROJECT {
            return Err(KernelError::Quota);
        }
        sqlx::query(
            "insert into workspaces (id, project_id, fabric_id, state, created_by_user_id) \
             values ($1, $2, $3, 'creating', $4)",
        )
        .bind(id)
        .bind(project_id)
        .bind(fabric_id)
        .bind(created_by_user_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Promotes a `creating` Workspace to `ready` after the Fabric
    /// confirmed it holds the resource (HTTP 200). Returns whether the
    /// transition happened; a missing or non-`creating` row is refused.
    pub async fn activate_workspace(&self, id: Uuid) -> Result<bool, KernelError> {
        let moved = sqlx::query(
            "update workspaces set state = 'ready' where id = $1 and state = 'creating'",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(moved.rows_affected() == 1)
    }

    pub async fn find_workspace(&self, id: Uuid) -> Result<Option<Workspace>, KernelError> {
        let row = sqlx::query(
            "select id, project_id, fabric_id, exec_generation, state \
             from workspaces where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(workspace_row))
    }

    /// Advances the durable Workspace execution generation by exactly one.
    /// Callers bump it only after the Fabric confirmed the replacement; a
    /// missing Workspace is refused like any other dangling reference.
    pub async fn advance_workspace_generation(&self, id: Uuid) -> Result<i64, KernelError> {
        // State guard: generations advance only inside a held lifecycle
        // fence (`fenced`), never on a ready or vanished Workspace.
        let generation: i64 = sqlx::query_scalar(
            "update workspaces set exec_generation = exec_generation + 1 \
             where id = $1 and state = 'fenced' returning exec_generation",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(KernelError::RelationRefused)?;
        Ok(generation)
    }

    /// Claims the lifecycle fence: exactly one caller moves `ready` to
    /// `fenced`; every later claimant (delete, replace, session attach) sees
    /// the fence and must not proceed with a competing operation.
    pub async fn begin_workspace_delete(&self, id: Uuid) -> Result<bool, KernelError> {
        let claimed =
            sqlx::query("update workspaces set state = 'fenced' where id = $1 and state = 'ready'")
                .bind(id)
                .execute(&self.pool)
                .await?;
        Ok(claimed.rows_affected() == 1)
    }

    /// Returns a fenced Workspace to `ready` after the claimed operation
    /// finished without completing its terminal effect.
    pub async fn restore_workspace(&self, id: Uuid) -> Result<bool, KernelError> {
        let restored =
            sqlx::query("update workspaces set state = 'ready' where id = $1 and state = 'fenced'")
                .bind(id)
                .execute(&self.pool)
                .await?;
        Ok(restored.rows_affected() == 1)
    }

    /// Completes a fenced teardown. The fence guarantees no Session attached
    /// after the claim, so the delete cannot hit its foreign key.
    pub async fn finish_workspace_delete(&self, id: Uuid) -> Result<bool, KernelError> {
        let deleted = sqlx::query("delete from workspaces where id = $1 and state = 'fenced'")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(deleted.rows_affected() == 1)
    }

    /// Removes one unreferenced Workspace row. Referencing sessions hold the
    /// row through its foreign key, so a referenced Workspace is refused.
    pub async fn delete_workspace(&self, id: Uuid) -> Result<bool, KernelError> {
        let deleted = sqlx::query("delete from workspaces where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(deleted.rows_affected() == 1)
    }

    /// Reads one durable Run by either server Run identity or caller intent.
    pub async fn find_run(&self, id: Uuid) -> Result<Option<Run>, KernelError> {
        let row = sqlx::query(&format!("select {RUN_COLUMNS} from runs where id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(run_row))
    }

    /// Atomically accepts one caller-identified Run. A repeated intent with
    /// the same request hash returns the existing row and never starts a
    /// second activation; a different hash is a conflict. The durable
    /// per-session turn ordinal (`seq`) is assigned in acceptance order, so
    /// follow-ups queue behind their predecessor.
    pub async fn accept_run(
        &self,
        run_id: Uuid,
        intent_id: Uuid,
        session_id: Uuid,
        request_hash: &[u8; 32],
        mode: &str,
        prompt: &str,
        actor_user_id: Option<Uuid>,
    ) -> Result<Run, KernelError> {
        // The per-session advisory lock serializes sequence allocation:
        // concurrent follow-ups on one Session can never observe the same
        // max(seq) and collide on the unique (session_id, seq) index.
        let mut tx = self.pool.begin().await?;
        let (key1, key2) = session_advisory_keys(session_id);
        sqlx::query("select pg_advisory_xact_lock($1, $2)")
            .bind(key1)
            .bind(key2)
            .execute(&mut *tx)
            .await?;
        let inserted = sqlx::query(&format!(
            "insert into runs \
            (id, intent_id, session_id, request_hash, mode, prompt, state, actor_user_id, seq) \
             values ($1, $2, $3, $4, $5, $6, 'accepted', $7, \
                     coalesce((select max(seq) + 1 from runs where session_id = $3), 1)) \
             on conflict do nothing \
             returning {RUN_COLUMNS}"
        ))
        .bind(run_id)
        .bind(intent_id)
        .bind(session_id)
        .bind(request_hash.as_slice())
        .bind(mode)
        .bind(prompt)
        .bind(actor_user_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(row) = inserted {
            if let Some(actor_user_id) = actor_user_id {
                sqlx::query("update sessions set last_actor_user_id = $2 where id = $1")
                    .bind(session_id)
                    .bind(actor_user_id)
                    .execute(&mut *tx)
                    .await?;
            }
            tx.commit().await?;
            return Ok(run_row(row));
        }
        let row = sqlx::query(&format!(
            "select {RUN_COLUMNS} from runs where intent_id = $1 or id = $2"
        ))
        .bind(intent_id)
        .bind(run_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(KernelError::Conflict)?;
        let run = run_row(row);
        // The intent is the idempotency key: a replayed message mints a
        // fresh Run identity each attempt, so the server Run id must not
        // participate in the equality check.
        if run.intent_id != intent_id
            || run.session_id != session_id
            || run.request_hash.as_slice() != request_hash.as_slice()
            || run.mode != mode
            || run.prompt != prompt
        {
            return Err(KernelError::Conflict);
        }
        tx.commit().await?;
        Ok(run)
    }

    /// Advances accepted -> dispatched. This is the durable no-replay fence
    /// written before the one activation attempt.
    pub async fn dispatch_run(&self, run_id: Uuid) -> Result<bool, KernelError> {
        let updated = sqlx::query(
            "update runs set state = 'dispatched', dispatched_at = now() \
             where id = $1 and state = 'accepted'",
        )
        .bind(run_id)
        .execute(&self.pool)
        .await?;
        Ok(updated.rows_affected() == 1)
    }

    /// Retains one terminal activation result.
    pub async fn complete_run(&self, run_id: Uuid, result: &str) -> Result<bool, KernelError> {
        let updated = sqlx::query(
            "update runs set state = 'terminal', result = $2, terminal_at = now() \
             where id = $1 and state = 'dispatched'",
        )
        .bind(run_id)
        .bind(result)
        .execute(&self.pool)
        .await?;
        Ok(updated.rows_affected() == 1)
    }

    /// Classifies a dispatched attempt with no observed terminal result as
    /// unknown. It is intentionally never changed back to accepted.
    pub async fn mark_run_unknown(&self, run_id: Uuid) -> Result<bool, KernelError> {
        let updated = sqlx::query(
            "update runs set state = 'unknown' \
             where id = $1 and state in ('accepted', 'dispatched')",
        )
        .bind(run_id)
        .execute(&self.pool)
        .await?;
        Ok(updated.rows_affected() == 1)
    }

    /// Cancels only a not-yet-dispatched Run. An in-flight effect remains
    /// classified as dispatched/unknown rather than being hidden as cancelled.
    /// Returns the Run's Session id when a queued head was cancelled, so the
    /// caller can wake the successor; `None` otherwise.
    pub async fn cancel_run(&self, run_id: Uuid) -> Result<(RunState, Option<Uuid>), KernelError> {
        let updated = sqlx::query(
            "update runs set state = 'cancelled', cancelled_at = now() \
             where id = $1 and state = 'accepted' \
             returning session_id",
        )
        .bind(run_id)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(row) = updated {
            return Ok((RunState::Cancelled, Some(row.get("session_id"))));
        }
        let requested = sqlx::query(
            "update runs set cancel_requested_at = now() \
             where id = $1 and state = 'dispatched' and cancel_requested_at is null",
        )
        .bind(run_id)
        .execute(&self.pool)
        .await?;
        if requested.rows_affected() == 1 {
            return Ok((RunState::Dispatched, None));
        }
        let state = self
            .find_run(run_id)
            .await?
            .map(|run| run.state)
            .unwrap_or(RunState::Unknown);
        Ok((state, None))
    }

    /// Restart recovery fence: no dispatched Run is replayed after process
    /// death. Accepted rows remain eligible for the resident supervisor.
    pub async fn classify_restarted_runs(&self) -> Result<u64, KernelError> {
        let updated = sqlx::query("update runs set state = 'unknown' where state = 'dispatched'")
            .execute(&self.pool)
            .await?;
        Ok(updated.rows_affected())
    }

    /// Accepted Runs awaiting the resident supervisor.
    pub async fn accepted_run_ids(&self) -> Result<Vec<Uuid>, KernelError> {
        Ok(
            sqlx::query_scalar("select id from runs where state = 'accepted' order by accepted_at")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    /// The next accepted Run eligible for dispatch: the lowest per-session
    /// turn ordinal whose predecessor on the same Session has settled
    /// (terminal, unknown, or cancelled). A Session never runs two
    /// activations concurrently; follow-ups queue behind their predecessor.
    pub async fn next_dispatchable_run(&self) -> Result<Option<Run>, KernelError> {
        let row = sqlx::query(&format!(
            "select {RUN_COLUMNS} from runs r \
             where r.state = 'accepted' \
               and not exists ( \
                   select 1 from runs p \
                   where p.session_id = r.session_id \
                     and p.seq < r.seq \
                     and p.state in ('accepted', 'dispatched') \
               ) \
             order by r.accepted_at, r.seq \
             limit 1"
        ))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(run_row))
    }

    /// The next accepted Run eligible for dispatch on one Session. Used by
    /// the supervisor after a Run settles, so the next queued follow-up
    /// starts immediately.
    pub async fn next_dispatchable_run_for_session(
        &self,
        session_id: Uuid,
    ) -> Result<Option<Run>, KernelError> {
        let row = sqlx::query(&format!(
            "select {RUN_COLUMNS} from runs r \
             where r.session_id = $1 and r.state = 'accepted' \
               and not exists ( \
                   select 1 from runs p \
                   where p.session_id = r.session_id \
                     and p.seq < r.seq \
                     and p.state in ('accepted', 'dispatched') \
               ) \
             order by r.seq \
             limit 1"
        ))
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(run_row))
    }

    /// True while any Run on the Session is accepted or dispatched (in
    /// flight or queued). The browser's `running` flag and the ordered
    /// dispatch gate both read this.
    pub async fn session_has_pending_run(&self, session_id: Uuid) -> Result<bool, KernelError> {
        let pending: bool = sqlx::query_scalar(
            "select exists(select 1 from runs \
             where session_id = $1 and state in ('accepted', 'dispatched'))",
        )
        .bind(session_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(pending)
    }

    /// Lists runs for a Session in durable acceptance order.
    pub async fn list_runs(&self, session_id: Uuid) -> Result<Vec<Run>, KernelError> {
        let rows = sqlx::query(&format!(
            "select {RUN_COLUMNS} from runs where session_id = $1 order by accepted_at"
        ))
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(run_row).collect())
    }

    /// Append one metadata-only audit row. Failure is intentionally returned
    /// to the caller so production paths can record it without blocking work.
    pub async fn audit(&self, event: AuditInsert<'_>) -> Result<AuditEvent, KernelError> {
        insert_audit(&self.pool, &event).await
    }
}

/// Writes one normalized audit row. Shared by the kernel and the API surface;
/// both treat failure as metadata-only and never block the audited work.
pub async fn insert_audit(
    pool: &PgPool,
    event: &AuditInsert<'_>,
) -> Result<AuditEvent, KernelError> {
    let row = sqlx::query(
        "insert into audit_events \
         (project_id, session_id, run_id, actor_user_id, kind, resource_type, resource_id, \
          outcome, metadata, payload) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, NULL) \
         returning seq, project_id, session_id, run_id, actor_user_id, \
                   occurred_at::text as occurred_at, kind, resource_type, resource_id, outcome, \
                   metadata",
    )
    .bind(event.project_id)
    .bind(event.session_id)
    .bind(event.run_id)
    .bind(event.actor_user_id)
    .bind(event.kind)
    .bind(event.resource_type)
    .bind(event.resource_id)
    .bind(event.outcome.as_str())
    .bind(event.metadata)
    .fetch_one(pool)
    .await?;
    Ok(AuditEvent {
        seq: row.get("seq"),
        project_id: row.get("project_id"),
        session_id: row.get("session_id"),
        run_id: row.get("run_id"),
        actor_user_id: row.get("actor_user_id"),
        occurred_at: row.get("occurred_at"),
        kind: row.get("kind"),
        resource_type: row.get("resource_type"),
        resource_id: row.get("resource_id"),
        outcome: row.get("outcome"),
        metadata: row.get::<Option<serde_json::Value>, _>("metadata"),
        payload: None,
    })
}

async fn apply_version(
    connection: &mut sqlx::PgConnection,
    version: i64,
    sql: &str,
) -> Result<(), KernelError> {
    let mut tx = connection.begin().await?;
    let exists: bool =
        sqlx::query_scalar("select exists(select 1 from schema_migrations where version = $1)")
            .bind(version)
            .fetch_one(&mut *tx)
            .await?;
    if !exists {
        sqlx::raw_sql(sql).execute(&mut *tx).await?;
        sqlx::query("insert into schema_migrations (version) values ($1)")
            .bind(version)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Per-session advisory lock keys. The same derivation as the session
/// writer fencing in `session_store`, so queue allocation and writer
/// acquisition never interleave destructively.
fn session_advisory_keys(session_id: Uuid) -> (i32, i32) {
    let bytes = session_id.as_bytes();
    let key1 = i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let key2 = i32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    (key1, key2)
}

fn respond(status: StatusCode, body: &'static str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::from_static(body.as_bytes())))
        .expect("static response parts are valid")
}

const WEB_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
    img-src 'self' data:; font-src 'self' data:; connect-src 'self'; worker-src 'self'; \
    object-src 'none'; base-uri 'self'; form-action 'self'";

#[derive(Clone)]
struct WebAssets {
    root: PathBuf,
}

impl WebAssets {
    fn from_env() -> Self {
        let root = std::env::var_os("VOIE_WEB_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("web/dist"));
        WebAssets { root }
    }

    /// True only while the required browser artifact is servable. Readiness
    /// fails closed when the built console is missing.
    async fn ready(&self) -> bool {
        web_assets_ready(&self.root).await
    }

    async fn response(&self, path: &str) -> Response<Full<Bytes>> {
        let root = match fs::canonicalize(&self.root).await {
            Ok(root) => root,
            Err(_) => return respond(StatusCode::NOT_FOUND, "web assets unavailable\n"),
        };
        let relative = match static_relative_path(path) {
            Ok(relative) => relative,
            Err(status) => return respond(status, "invalid web path\n"),
        };
        let requested = root.join(&relative);
        let file = match canonical_file(&root, &requested).await {
            Some(file) => file,
            None => match canonical_file(&root, &root.join("index.html")).await {
                Some(index) => index,
                None => return respond(StatusCode::NOT_FOUND, "web assets unavailable\n"),
            },
        };
        let body = match fs::read(&file).await {
            Ok(body) => body,
            Err(_) => return respond(StatusCode::NOT_FOUND, "web asset unavailable\n"),
        };
        let content_type = content_type(&file);
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", content_type)
            .header("content-security-policy", WEB_CSP)
            .header("cache-control", "no-store")
            .body(Full::new(Bytes::from(body)))
            .expect("static web response headers are valid")
    }
}

/// Readiness of one web asset root: the directory resolves and carries the
/// required `index.html` entry.
pub async fn web_assets_ready(root: &Path) -> bool {
    match fs::canonicalize(root).await {
        Ok(root) => fs::metadata(root.join("index.html"))
            .await
            .map(|meta| meta.is_file())
            .unwrap_or(false),
        Err(_) => false,
    }
}

async fn canonical_file(root: &Path, candidate: &Path) -> Option<PathBuf> {
    let file = fs::canonicalize(candidate).await.ok()?;
    if !file.starts_with(root) || !fs::metadata(&file).await.ok()?.is_file() {
        return None;
    }
    Some(file)
}

fn static_relative_path(path: &str) -> Result<PathBuf, StatusCode> {
    let decoded = percent_decode(path).ok_or(StatusCode::BAD_REQUEST)?;
    let trimmed = decoded.strip_prefix('/').unwrap_or(&decoded);
    let relative = if trimmed.is_empty() {
        "index.html"
    } else {
        trimmed
    };
    let path = Path::new(relative);
    for component in path.components() {
        match component {
            std::path::Component::Normal(_) | std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => return Err(StatusCode::FORBIDDEN),
        }
    }
    Ok(path.to_path_buf())
}

fn percent_decode(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return None;
        }
        let high = hex_value(bytes[index + 1])?;
        let low = hex_value(bytes[index + 2])?;
        decoded.push((high << 4) | low);
        index += 3;
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("woff") | Some("woff2") => "font/woff2",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

async fn handle(
    kernel: Arc<Kernel>,
    auth: Option<Arc<auth::Auth>>,
    services: Option<Arc<integration::Services>>,
    web: Option<WebAssets>,
    request: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    if let Some(auth) = auth.as_ref() {
        if request.method() == Method::GET && request.uri().path() == "/api/auth/capabilities" {
            return Ok(auth.capabilities_response());
        }
        // GET /login deliberately serves the VOIE static app (same handler
        // as the portal assets) instead of the bare auth form whenever the
        // app shell is mounted. Web-less servers (health/kernel test
        // fixtures) keep the auth form so login stays reachable there.
        if web.is_none() && request.method() == Method::GET && request.uri().path() == "/login" {
            return Ok(auth.handle(request).await);
        }
        // The auth surface keeps the browser-bound form/login/logout verbs.
        if matches!(
            (request.method(), request.uri().path()),
            (&Method::GET, "/login/oidc")
                | (&Method::POST, "/login")
                | (&Method::GET, "/oidc/callback")
                | (&Method::POST, "/logout")
        ) {
            return Ok(auth.handle(request).await);
        }
    }
    if request.uri().path().starts_with("/api/") {
        if let (Some(services), Some(auth)) = (services.as_ref(), auth.as_ref()) {
            return Ok(services.handle(&kernel, auth, request).await);
        }
        return Ok(respond(
            StatusCode::SERVICE_UNAVAILABLE,
            "API unavailable\n",
        ));
    }
    let response = match (request.method(), request.uri().path()) {
        (&Method::GET, "/healthz") => respond(StatusCode::OK, "ok\n"),
        (&Method::GET, "/readyz") => {
            // Fail closed: the database is not enough. With the full product
            // surface assembled, every dependency and required artifact must
            // answer before readiness reports ready.
            let mut ready = kernel.ready().await;
            if let Some(services) = services.as_ref() {
                ready = ready && services.dependencies_ready().await;
            }
            if ready {
                if let Some(web) = web.as_ref() {
                    ready = web.ready().await;
                }
            }
            if ready {
                respond(StatusCode::OK, "ready\n")
            } else {
                respond(StatusCode::SERVICE_UNAVAILABLE, "not ready\n")
            }
        }
        _ => respond(StatusCode::NOT_FOUND, "not found\n"),
    };
    if response.status() != StatusCode::NOT_FOUND {
        return Ok(response);
    }
    if request.method() == Method::GET {
        if let Some(web) = web {
            return Ok(web.response(request.uri().path()).await);
        }
    }
    Ok(response)
}

/// Serves liveness and readiness on one listener until the task is dropped.
pub async fn serve(listener: tokio::net::TcpListener, kernel: Arc<Kernel>) -> std::io::Result<()> {
    serve_inner(listener, kernel, None, None, None, None).await
}

/// Serves the health-only surface and returns a handle that drains in-flight
/// connections on shutdown instead of dropping them.
pub fn serve_graceful(listener: tokio::net::TcpListener, kernel: Arc<Kernel>) -> RunningServer {
    let drain = Drain::default();
    let task = tokio::spawn(serve_inner(
        listener,
        kernel,
        None,
        None,
        None,
        Some(drain.clone()),
    ));
    RunningServer { task, drain }
}

/// Serves health, readiness, and the configured OIDC/Web-session routes on one
/// listener. The existing [`serve`] entrypoint remains health-only for
/// focused kernel tests.
pub async fn serve_with_auth(
    listener: tokio::net::TcpListener,
    kernel: Arc<Kernel>,
    auth: Arc<auth::Auth>,
) -> std::io::Result<()> {
    serve_inner(
        listener,
        kernel,
        Some(auth),
        None,
        Some(WebAssets::from_env()),
        None,
    )
    .await
}

/// Serves the complete Release 0 HTTP surface with the real backend and
/// activation seams assembled by [`integration::Services`].
pub async fn serve_with_services(
    listener: tokio::net::TcpListener,
    kernel: Arc<Kernel>,
    auth: Arc<auth::Auth>,
    services: Arc<integration::Services>,
) -> std::io::Result<()> {
    serve_inner(
        listener,
        kernel,
        Some(auth),
        Some(services),
        Some(WebAssets::from_env()),
        None,
    )
    .await
}

/// Serves the complete Release 0 surface and returns a handle that drains
/// in-flight connections on shutdown instead of dropping them.
pub fn serve_with_services_graceful(
    listener: tokio::net::TcpListener,
    kernel: Arc<Kernel>,
    auth: Arc<auth::Auth>,
    services: Arc<integration::Services>,
) -> RunningServer {
    let drain = Drain::default();
    let task = tokio::spawn(serve_inner(
        listener,
        kernel,
        Some(auth),
        Some(services),
        Some(WebAssets::from_env()),
        Some(drain.clone()),
    ));
    RunningServer { task, drain }
}

/// A running server that can be drained gracefully on shutdown.
pub struct RunningServer {
    task: tokio::task::JoinHandle<std::io::Result<()>>,
    drain: Drain,
}

impl RunningServer {
    /// Signals every live connection to finish its in-flight request, stops
    /// accepting new ones, and waits at most `grace` for connections to end.
    pub async fn drain(self, grace: Duration) -> std::io::Result<()> {
        self.drain.signal();
        let deadline = tokio::time::Instant::now() + grace;
        while self.drain.open.load(std::sync::atomic::Ordering::SeqCst) > 0 {
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.task
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?
    }
}

/// Shutdown state shared by the accept loop and every connection task,
/// tracked for the bounded graceful drain.
///
/// The atomic latch is authoritative. `Notify::notify_waiters` wakes only
/// tasks that already registered, so a task starting during or after the
/// signal must observe the latch directly or the drain would hang forever.
#[derive(Clone, Default)]
struct Drain {
    latched: Arc<std::sync::atomic::AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
    open: Arc<std::sync::atomic::AtomicUsize>,
}

impl Drain {
    /// Latches shutdown before notifying: a waiter that misses the notify
    /// observes the latch instead.
    fn signal(&self) {
        self.latched
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    fn is_latched(&self) -> bool {
        self.latched.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Resolves once shutdown was signaled, for waiters starting before,
    /// during, or after the signal itself.
    async fn shutdown(&self) {
        if self.is_latched() {
            return;
        }
        let notified = self.notify.notified();
        tokio::pin!(notified);
        // Register interest before re-checking. The latch is stored before
        // every notify, so a concurrently signaled shutdown is either seen
        // by this re-check or delivered through the registered Notify.
        notified.as_mut().enable();
        if self.is_latched() {
            return;
        }
        notified.await;
    }
}

/// Decrements the open-connection count whenever its connection task ends.
struct ConnectionGuard(Arc<std::sync::atomic::AtomicUsize>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

async fn serve_inner(
    listener: tokio::net::TcpListener,
    kernel: Arc<Kernel>,
    auth: Option<Arc<auth::Auth>>,
    services: Option<Arc<integration::Services>>,
    web: Option<WebAssets>,
    drain: Option<Drain>,
) -> std::io::Result<()> {
    loop {
        if drain.as_ref().is_some_and(|drain| drain.is_latched()) {
            break;
        }
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            _ = async {
                match drain.as_ref() {
                    Some(drain) => drain.shutdown().await,
                    None => std::future::pending::<()>().await,
                }
            } => break,
        };
        let (stream, _) = accepted?;
        let io = hyper_util::rt::TokioIo::new(stream);
        let kernel = kernel.clone();
        let auth = auth.clone();
        let services = services.clone();
        let web = web.clone();
        let connection_drain = drain.clone();
        if let Some(drain) = drain.as_ref() {
            drain.open.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        tokio::spawn(async move {
            let _guard = connection_drain
                .as_ref()
                .map(|drain| ConnectionGuard(drain.open.clone()));
            let service = hyper::service::service_fn(move |request| {
                handle(
                    kernel.clone(),
                    auth.clone(),
                    services.clone(),
                    web.clone(),
                    request,
                )
            });
            let connection =
                hyper::server::conn::http1::Builder::new().serve_connection(io, service);
            tokio::pin!(connection);
            match connection_drain.as_ref() {
                Some(drain) => {
                    if drain.is_latched() {
                        connection.as_mut().graceful_shutdown();
                        if let Err(error) = (&mut connection).await {
                            eprintln!("voie-cloud: connection error: {error}");
                        }
                    } else {
                        tokio::select! {
                            result = &mut connection => {
                                if let Err(error) = result {
                                    eprintln!("voie-cloud: connection error: {error}");
                                }
                            }
                            _ = drain.shutdown() => {
                                // Stop reading new requests; the in-flight one
                                // still completes and receives its response.
                                connection.as_mut().graceful_shutdown();
                                if let Err(error) = (&mut connection).await {
                                    eprintln!("voie-cloud: connection error: {error}");
                                }
                            }
                        }
                    }
                }
                None => {
                    if let Err(error) = connection.await {
                        eprintln!("voie-cloud: connection error: {error}");
                    }
                }
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{content_type, static_relative_path};
    use std::path::Path;

    #[test]
    fn static_paths_decode_and_reject_traversal() {
        assert_eq!(
            static_relative_path("/assets%2Fapp.js").expect("encoded slash is valid"),
            Path::new("assets/app.js")
        );
        assert_eq!(
            static_relative_path("/%2e%2e/secret"),
            Err(hyper::StatusCode::FORBIDDEN)
        );
        assert_eq!(
            static_relative_path("/%"),
            Err(hyper::StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn static_content_types_match_web_assets() {
        assert_eq!(
            content_type(Path::new("index.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            content_type(Path::new("assets/app.js")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(content_type(Path::new("assets/app.woff2")), "font/woff2");
    }
}

#[cfg(test)]
mod drain_tests {
    use super::Drain;
    use std::time::Duration;

    /// Regression: a connection task that registers on the Notify only after
    /// `notify_waiters` fired must still observe shutdown through the latch.
    #[tokio::test]
    async fn waiter_started_after_signal_sees_the_latch() {
        let drain = Drain::default();
        drain.signal();
        tokio::time::timeout(Duration::from_millis(200), drain.shutdown())
            .await
            .expect("a late waiter resolves through the latch instead of hanging");
    }

    #[tokio::test]
    async fn signal_wakes_registered_waiters() {
        let drain = Drain::default();
        let waiter = tokio::spawn({
            let drain = drain.clone();
            async move { drain.shutdown().await }
        });
        // Give the waiter a chance to register first; either way the latch
        // makes completion deterministic.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        drain.signal();
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("drain joins")
            .expect("waiter task succeeds");
    }
}
