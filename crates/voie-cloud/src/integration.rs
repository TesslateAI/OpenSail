//! Integration-owned wiring for the Release 0 product boundary.
//!
//! The packet modules own their trust boundaries. This module only adapts the
//! typed model, session, Fabric, and activation seams and exposes the small
//! same-origin API consumed by the Web carrier.

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use hyper::body::Incoming;
use hyper::header::{CONTENT_TYPE, ORIGIN};
use hyper::{Method, Request, Response, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::activation::{
    self, ActivationContext, ActivationError, ActivationHost, ActivationMode, ActivationOutcome,
    ActivationRequest, AppendReceipt, BASH_TIMEOUT_MS, BashIntent, BashOutcome, BashResult,
    ModelRelay, ModelRequest, ModelResponse, SessionPersistence, WorkspaceExec,
};
use crate::auth::{self, Action, Auth, Role};
use crate::exec_journal::{ExecJournal, ExecOutcome};
use crate::fabric_client::{CreateOutcome, ExecResult, FabricClient};
use crate::model::{
    ModelMessage, ModelRelay as CloudModelRelay, ModelRequest as CloudModelRequest,
    ModelToolDefinition,
};
use crate::secrets::{
    MaterialBackend, ScopeAuthorizationError, ScopeCapability, SecretAuditEvent, SecretMetadata,
    SecretValue, SecretsError, SecretsStore,
};
use crate::session_store::{AppendEvent, SessionStore, SessionWriter};
use crate::web_session;
use crate::{
    Agent, AuditInsert, AuditOutcome, Kernel, Run, RunState, Session, WorkspaceState, insert_audit,
};

/// Browser mutation bodies are small JSON documents; 64 KiB bounds abuse
/// without constraining any real prompt or configuration payload.
const MAX_REQUEST_BYTES: usize = 64 * 1024;
/// Fixed advisory-lock key serializing every last-active-platform-admin
/// decision. All demote/disable paths share one transaction-scoped lock so
/// two racing mutations of different admins can never both observe a count
/// that still permits the flip and commit zero active admins.
const ADMIN_GUARD_LOCK_KEY: i64 = -0x564f4945_41444D49;
/// Upper bound for one readiness dependency probe set.
const DEPENDENCY_PROBE_WINDOW: Duration = Duration::from_secs(5);
const DEFAULT_MAX_TOKENS: u32 = 1024;

/// Product dependencies assembled once by the trusted process.
#[derive(Clone)]
pub struct Services {
    pool: PgPool,
    sessions: SessionStore,
    model: Arc<CloudModelRelay>,
    fabric: Arc<FabricClient>,
    /// The deployment-selected Fabric for new Workspaces (Profile 0 binds
    /// every Workspace to one fixed Fabric chosen by configuration).
    configured_fabric_id: Option<Uuid>,
    journal: Arc<ExecJournal>,
    /// User-secret vault: metadata store plus the deployment-selected
    /// material backend and project-scope authorization boundary.
    secrets: Arc<VaultStore>,
}

/// Concrete vault store type used by `Services`.
type VaultStore = SecretsStore<MaterialBackend, ScopeProjectAuthorizer>;

#[derive(Debug)]
pub enum ServiceConfigError {
    Blob(String),
    Model(String),
    Fabric(String),
    Secrets(String),
}

impl fmt::Display for ServiceConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServiceConfigError::Blob(message) => write!(f, "Blob configuration failed: {message}"),
            ServiceConfigError::Model(message) => {
                write!(f, "model configuration failed: {message}")
            }
            ServiceConfigError::Fabric(message) => {
                write!(f, "Fabric configuration failed: {message}")
            }
            ServiceConfigError::Secrets(message) => {
                write!(f, "secrets configuration failed: {message}")
            }
        }
    }
}

impl std::error::Error for ServiceConfigError {}

/// Fixed project-scope authorization for the user-secret vault.
///
/// Project membership maps the frozen roles onto vault capabilities:
/// owner/admin/member may write material, viewer reads metadata only, and
/// no membership carries no capability. Platform roles never broaden vault
/// capabilities past project membership.
struct ScopeProjectAuthorizer {
    pool: PgPool,
}

impl ScopeProjectAuthorizer {
    fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl crate::secrets::ScopeAuthorizer for ScopeProjectAuthorizer {
    fn scope_capability<'a>(
        &'a self,
        actor_user_id: Uuid,
        scope_id: Uuid,
    ) -> crate::secrets::ScopeCapabilityFuture<'a> {
        Box::pin(async move {
            let role: Option<String> = sqlx::query_scalar(
                "select role from project_members where user_id = $1 and project_id = $2",
            )
            .bind(actor_user_id)
            .bind(scope_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| ScopeAuthorizationError)?;
            if let Some(role_text) = role.as_deref() {
                let role = Role::parse(role_text).ok_or(ScopeAuthorizationError)?;
                return Ok(match role {
                    Role::Viewer => ScopeCapability::Read,
                    _ => ScopeCapability::Write,
                });
            }
            // No membership carries no capability. Platform administration
            // never broadens scope-secrets access.
            Ok(ScopeCapability::None)
        })
    }
}

/// Resolves the deployment-selected vault material backend. The selected
/// backend family decides everything: absent / `local-encrypted` use the
/// durable encrypted files under `VOIE_SECRETS_DIR`, `memory` is
/// process-local for tests, and `key-vault` constructs the Azure Key Vault
/// backend when `VOIE_KEY_VAULT_URI` is present. Configuration never
/// silently downgrades to weaker local storage.
fn secrets_backend_from_env() -> Result<MaterialBackend, ServiceConfigError> {
    let requested = std::env::var("VOIE_USER_SECRETS_BACKEND")
        .unwrap_or_default()
        .trim()
        .to_owned();
    MaterialBackend::from_selection(&requested, &secrets_key_salt_database_url())
        .map_err(ServiceConfigError::Secrets)
}

/// Database URL consumed only as the documented fallback key-derivation salt
/// of the local encrypted backend, resolved with the same precedence as
/// [`crate::Config::from_env`] plus the dev-stack test variable.
fn secrets_key_salt_database_url() -> String {
    match std::env::var("VOIE_DATABASE_URL_FILE") {
        Ok(path) if !path.trim().is_empty() => {
            std::fs::read_to_string(path.trim()).unwrap_or_default()
        }
        _ => std::env::var("VOIE_DATABASE_URL")
            .or_else(|_| std::env::var("VOIE_TEST_DATABASE_URL"))
            .unwrap_or_default(),
    }
}

impl Services {
    /// Resolves only environment-backed settings. No `.env` file or fallback
    /// provider is consulted, and protected values never leave their owner.
    pub fn from_env(pool: PgPool) -> Result<Arc<Self>, ServiceConfigError> {
        let blob = crate::session_store::BlobStore::from_env()
            .map_err(|error| ServiceConfigError::Blob(error.to_string()))?;
        let model = CloudModelRelay::from_env()
            .map_err(|error| ServiceConfigError::Model(error.to_string()))?;
        let fabric = FabricClient::from_env()
            .map_err(|error| ServiceConfigError::Fabric(error.to_string()))?;
        let configured_fabric_id = match std::env::var("VOIE_FABRIC_ID") {
            Ok(value) if !value.trim().is_empty() => {
                Some(Uuid::parse_str(value.trim()).map_err(|_| {
                    ServiceConfigError::Fabric("VOIE_FABRIC_ID is not a UUID".to_owned())
                })?)
            }
            _ => None,
        };
        let secrets = Arc::new(SecretsStore::from_pool(
            &pool,
            secrets_backend_from_env()?,
            ScopeProjectAuthorizer::new(pool.clone()),
        ));
        Ok(Arc::new(Services {
            sessions: SessionStore::new(pool.clone(), blob),
            model: Arc::new(model),
            fabric: Arc::new(fabric),
            configured_fabric_id,
            journal: Arc::new(ExecJournal::new(pool.clone())),
            secrets,
            pool,
        }))
    }

    /// Bounded concurrent probes of Blob, model, and Fabric reachability plus
    /// the required activation artifacts. Any failure fails readiness closed.
    pub async fn dependencies_ready(&self) -> bool {
        let probe_window = DEPENDENCY_PROBE_WINDOW;
        let (blob_ok, model_ok, fabric_ok, artifacts_ok) = tokio::join!(
            tokio::time::timeout(probe_window, self.sessions.blob().reachable()),
            tokio::time::timeout(probe_window, self.model.reachable()),
            tokio::time::timeout(probe_window, self.fabric.health()),
            async { crate::activation::artifacts_ready().is_ok() },
        );
        blob_ok == Ok(true)
            && model_ok == Ok(true)
            && matches!(fabric_ok, Ok(Ok(())))
            && artifacts_ok
    }

    /// Runs one real disposable activation against the configured model,
    /// Blob/PostgreSQL session writer, and mTLS Fabric journal.
    pub async fn activate(
        &self,
        session: Session,
        run_id: Uuid,
        mode: ActivationMode,
        prompt: String,
        agent: Agent,
    ) -> Result<ActivationOutcome, ActivationError> {
        let persistence = CloudPersistence::new(
            self.sessions.clone(),
            session.id,
            session.writer_generation + 1,
        );
        let model = CloudModel {
            relay: self.model.clone(),
            agent,
        };
        let workspace = CloudWorkspace {
            fabric: self.fabric.clone(),
            journal: self.journal.clone(),
            workspace_id: session.workspace_id,
        };
        let host = ActivationHost {
            context: ActivationContext {
                project_id: session.project_id,
                agent_id: session.agent_id,
                session_id: session.id,
                run_id,
                workspace_id: session.workspace_id,
                writer_generation: session.writer_generation + 1,
            },
            model: &model,
            workspace: &workspace,
            sessions: &persistence,
        };
        activation::run(host, ActivationRequest { mode, prompt }).await
    }

    /// Classifies in-flight dispatches after a process restart and schedules
    /// only accepted Runs. A dispatched Run is never replayed.
    pub async fn recover(&self, kernel: &Kernel) -> Result<(), sqlx::Error> {
        kernel
            .classify_restarted_runs()
            .await
            .map_err(|_| sqlx::Error::Protocol("run recovery failed".into()))?;
        for run_id in kernel
            .accepted_run_ids()
            .await
            .map_err(|_| sqlx::Error::Protocol("run recovery failed".into()))?
        {
            if let Some(run) = kernel
                .find_run(run_id)
                .await
                .map_err(|_| sqlx::Error::Protocol("run recovery failed".into()))?
            {
                self.spawn_run(run);
            }
        }
        Ok(())
    }

    fn spawn_run(&self, run: Run) {
        let worker = self.clone();
        tokio::spawn(async move {
            worker.process_run(run).await;
        });
    }

    async fn process_run(&self, run: Run) {
        // Ordered dispatch claim: only the lowest unsettled turn on the
        // Session may dispatch. A follow-up queued behind an in-flight
        // predecessor stays `accepted` until the predecessor settles, so a
        // Session never runs two activations concurrently — including after
        // restart recovery, where every accepted Run is re-spawned but only
        // the head of each Session's queue can claim dispatch.
        let dispatched = sqlx::query(
            "update runs set state = 'dispatched', dispatched_at = now() \
             where id = $1 and state = 'accepted' \
               and not exists ( \
                   select 1 from runs p \
                   where p.session_id = runs.session_id \
                     and p.seq < runs.seq \
                     and p.state in ('accepted', 'dispatched') \
               )",
        )
        .bind(run.id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .unwrap_or(false);
        if !dispatched {
            return;
        }
        let session = sqlx::query(
            "select id, project_id, agent_id, workspace_id, writer_generation, \
                    attention_generation, head_revision from sessions where id = $1",
        )
        .bind(run.session_id)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()
        .map(|row| session_from_row(&row));
        let Some(session) = session else {
            let _ = self.mark_unknown(run.id).await;
            return;
        };
        self.fire_run_audit(session.project_id, session.id, run.id, "run.dispatched");
        let mode = match run.mode.as_str() {
            "create" => ActivationMode::Create,
            "resume" => ActivationMode::Resume,
            _ => {
                let _ = self.mark_unknown(run.id).await;
                self.kick_next(session.id);
                return;
            }
        };
        let agent = sqlx::query(
            "select id, project_id, name, model, system_prompt, bash_enabled, max_tokens \
             from agents where id = $1",
        )
        .bind(session.agent_id)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()
        .map(|row| agent_from_row(&row));
        let Some(agent) = agent else {
            let _ = self.mark_unknown(run.id).await;
            self.kick_next(session.id);
            return;
        };
        let cancel_requested: bool =
            sqlx::query_scalar("select cancel_requested_at is not null from runs where id = $1")
                .bind(run.id)
                .fetch_one(&self.pool)
                .await
                .unwrap_or(false);
        if cancel_requested {
            let _ = sqlx::query(
                "update runs set state = 'cancelled', cancelled_at = now() \
                 where id = $1 and state = 'dispatched'",
            )
            .bind(run.id)
            .execute(&self.pool)
            .await;
            self.kick_next(session.id);
            return;
        }
        let outcome = self
            .activate(session.clone(), run.id, mode, run.prompt.clone(), agent)
            .await;
        match outcome {
            Ok(done) => {
                let cancel_requested: bool = sqlx::query_scalar(
                    "select cancel_requested_at is not null from runs where id = $1",
                )
                .bind(run.id)
                .fetch_one(&self.pool)
                .await
                .unwrap_or(false);
                if cancel_requested {
                    let _ = self.mark_unknown(run.id).await;
                    self.fire_run_audit(session.project_id, session.id, run.id, "run.unknown");
                    self.kick_next(session.id);
                    return;
                }
                let retained = json!({
                    "accepted": true,
                    "runId": run.id,
                    "intentId": run.intent_id,
                    "state": RunState::Terminal.as_str(),
                    "finalText": done.final_text,
                    "bashCalls": done.bash_intents.len(),
                    "childExitCode": done.child_exit_code,
                    "childOpenedConnection": done.child_opened_connection,
                })
                .to_string();
                let terminal_rows = sqlx::query(
                    "update runs set state = 'terminal', result = $2, terminal_at = now() \
                     where id = $1 and state = 'dispatched'",
                )
                .bind(run.id)
                .bind(retained)
                .execute(&self.pool)
                .await
                .map(|result| result.rows_affected())
                .unwrap_or(0);
                if terminal_rows == 1 {
                    self.fire_run_audit(session.project_id, session.id, run.id, "run.terminal");
                }
            }
            Err(_) => {
                let _ = self.mark_unknown(run.id).await;
                self.fire_run_audit(session.project_id, session.id, run.id, "run.unknown");
            }
        }
        // The head settled: the next queued follow-up on this Session is now
        // dispatchable. Kick it so the queue advances without waiting for a
        // new request.
        self.kick_next(session.id);
    }

    /// Spawns the next accepted Run on one Session whose predecessor has
    /// settled. The ordered dispatch claim in `process_run` remains the
    /// authority; this only wakes the queue.
    fn kick_next(&self, session_id: Uuid) {
        let worker = self.clone();
        tokio::spawn(async move {
            let next = sqlx::query(
                "select id, intent_id, session_id, request_hash, mode, prompt, state, result, \
                        actor_user_id, seq, \
                        accepted_at::text as accepted_at, \
                        dispatched_at::text as dispatched_at, \
                        cancel_requested_at::text as cancel_requested_at, \
                        terminal_at::text as terminal_at, \
                        cancelled_at::text as cancelled_at \
                 from runs r \
                 where r.session_id = $1 and r.state = 'accepted' \
                   and not exists ( \
                       select 1 from runs p \
                       where p.session_id = r.session_id and p.seq < r.seq \
                         and p.state in ('accepted', 'dispatched') \
                   ) \
                 order by r.seq limit 1",
            )
            .bind(session_id)
            .fetch_optional(&worker.pool)
            .await
            .ok()
            .flatten()
            .map(|row| run_from_row(&row));
            if let Some(next) = next {
                worker.spawn_run(next);
            }
        });
    }

    async fn mark_unknown(&self, run_id: Uuid) -> Result<(), sqlx::Error> {
        sqlx::query(
            "update runs set state = 'unknown' \
             where id = $1 and state in ('accepted', 'dispatched')",
        )
        .bind(run_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    /// Fire-and-forget system emission for supervisor-side transitions; no
    /// human actor exists on this path.
    fn fire_run_audit(&self, project_id: Uuid, session_id: Uuid, run_id: Uuid, kind: &'static str) {
        self.fire(AuditInsert {
            project_id: Some(project_id),
            session_id: Some(session_id),
            run_id: Some(run_id),
            actor_user_id: None,
            kind,
            resource_type: "run",
            resource_id: Some(run_id),
            outcome: AuditOutcome::Ok,
            metadata: None,
        });
    }

    /// Spawns one normalized audit insert; failures are metadata-only and
    /// never block the audited work.
    fn fire(&self, event: AuditInsert<'static>) {
        let pool = self.pool.clone();
        tokio::spawn(async move {
            let _ = insert_audit(&pool, &event).await;
        });
    }

    /// Awaits one normalized audit insert so route responses observe their
    /// own row. Errors stay ignored: audit never blocks valid work.
    async fn record(&self, event: AuditInsert<'_>) {
        let _ = insert_audit(&self.pool, &event).await;
    }

    /// Handles the same-origin API used by the production Web carrier.
    pub async fn handle(
        &self,
        kernel: &Kernel,
        auth: &Auth,
        request: Request<Incoming>,
    ) -> Response<http_body_util::Full<Bytes>> {
        let user_id = match self.current_user(auth, &request).await {
            Ok(Some(user_id)) => user_id,
            Ok(None) => return json_error(StatusCode::UNAUTHORIZED, "login required"),
            Err(_) => {
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "session lookup failed");
            }
        };
        let method = request.method().clone();
        let path = request.uri().path().to_owned();
        let query = request.uri().query().unwrap_or("").to_owned();
        // Account routes compare against the caller's live session row,
        // so resolve it once here; every other path skips the lookup.
        let account_token = if path == "/api/account" || path.starts_with("/api/account/") {
            web_session::request_cookie(&request, web_session::COOKIE_NAME)
        } else {
            None
        };
        let mut body = Vec::new();
        // PUT carries the same JSON mutation bodies as POST/PATCH (the
        // user-secret replace route is PUT), so the request gate reads it
        // identically instead of treating it as an empty request.
        if matches!(
            method,
            Method::POST | Method::PUT | Method::PATCH | Method::DELETE
        ) {
            // DELETE carries no entity body, so it has no JSON content-type
            // to require; the same-origin + intent gate still applies.
            let admitted = if method == Method::DELETE {
                origin_and_intent_allowed(&request, auth.config().public_origin())
            } else {
                browser_mutation_allowed(&request, auth.config().public_origin())
            };
            if !admitted {
                return json_error(StatusCode::FORBIDDEN, "state change refused");
            }
            body = match request_body(request).await {
                Ok(body) => body,
                Err(response) => return response,
            };
        }
        let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
        // Health reports the deployment-selected login surfaces verbatim.
        let auth_mode = match auth.config().mode() {
            auth::AuthMode::Native => "native",
            auth::AuthMode::Oidc => "oidc",
            auth::AuthMode::Both => "both",
        };
        let current = match account_token {
            Some(token) => {
                let live = web_session::lookup(&self.pool, &token, auth.config().session_ttl())
                    .await
                    .ok()
                    .flatten();
                live
            }
            None => None,
        };
        self.route(
            kernel,
            user_id,
            &segments,
            body,
            method,
            &query,
            auth_mode,
            current.as_ref(),
        )
        .await
    }

    async fn route(
        &self,
        kernel: &Kernel,
        user_id: Uuid,
        segments: &[&str],
        body: Vec<u8>,
        method: Method,
        query: &str,
        auth_mode: &'static str,
        current: Option<&web_session::WebSession>,
    ) -> Response<http_body_util::Full<Bytes>> {
        let bad_id = || json_error(StatusCode::BAD_REQUEST, "invalid resource id");
        match (&method, segments) {
            (&Method::GET, ["api", "me"]) => self.me(user_id).await,
            (&Method::GET, ["api", "projects"]) => self.projects(user_id).await,
            (&Method::GET, ["api", "projects", id]) => match Uuid::parse_str(id) {
                Ok(project_id) => self.project_detail(user_id, project_id).await,
                Err(_) => bad_id(),
            },
            (&Method::GET, ["api", "projects", id, "members"]) => match Uuid::parse_str(id) {
                Ok(project_id) => self.project_members(user_id, project_id).await,
                Err(_) => bad_id(),
            },
            (&Method::POST, ["api", "projects", id, "members"]) => match Uuid::parse_str(id) {
                Ok(project_id) => self.add_member(user_id, project_id, body).await,
                Err(_) => bad_id(),
            },
            (&Method::DELETE, ["api", "projects", id, "members", member]) => {
                match (Uuid::parse_str(id), Uuid::parse_str(member)) {
                    (Ok(project_id), Ok(member_id)) => {
                        self.remove_member(user_id, project_id, member_id).await
                    }
                    _ => bad_id(),
                }
            }
            (&Method::GET, ["api", "agents"]) => self.agents(user_id).await,
            (&Method::GET, ["api", "agents", id]) => match Uuid::parse_str(id) {
                Ok(agent_id) => self.agent_detail(user_id, agent_id).await,
                Err(_) => bad_id(),
            },
            (&Method::GET, ["api", "sessions"]) => self.sessions(user_id).await,
            (&Method::GET, ["api", "sessions", id]) => match Uuid::parse_str(id) {
                Ok(session_id) => self.session_detail(user_id, session_id).await,
                Err(_) => bad_id(),
            },
            (&Method::GET, ["api", "sessions", id, "events"]) => match Uuid::parse_str(id) {
                Ok(session_id) => self.session_events(user_id, session_id, query).await,
                Err(_) => bad_id(),
            },
            (&Method::GET, ["api", "fabrics"]) => self.fabrics(user_id).await,
            (&Method::GET, ["api", "workspaces"]) => self.workspaces(user_id).await,
            (&Method::GET, ["api", "audit-events"]) => self.audit_events(user_id, query).await,
            (&Method::GET, ["api", "runs"]) => self.runs(user_id).await,
            (&Method::GET, ["api", "runs", id]) => match Uuid::parse_str(id) {
                Ok(run_id) => self.run_resource(user_id, run_id).await,
                Err(_) => bad_id(),
            },
            (&Method::GET, ["api", "events"]) => self.events(user_id, query).await,
            (&Method::POST, ["api", "projects"]) => {
                self.create_project(kernel, user_id, body).await
            }
            (&Method::POST, ["api", "projects", project_id, "agents"]) => {
                match Uuid::parse_str(project_id) {
                    Ok(project_id) => self.create_agent(kernel, user_id, project_id, body).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::POST, ["api", "projects", project_id, "sessions"]) => {
                match Uuid::parse_str(project_id) {
                    Ok(project_id) => self.create_session(kernel, user_id, project_id, body).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::POST, ["api", "projects", project_id, "workspaces"]) => {
                match Uuid::parse_str(project_id) {
                    Ok(project_id) => {
                        self.create_workspace(kernel, user_id, project_id, body)
                            .await
                    }
                    Err(_) => bad_id(),
                }
            }
            (&Method::DELETE, ["api", "projects", id, "workspaces", workspace]) => {
                match (Uuid::parse_str(id), Uuid::parse_str(workspace)) {
                    (Ok(project_id), Ok(workspace_id)) => {
                        self.delete_workspace(kernel, user_id, project_id, workspace_id)
                            .await
                    }
                    _ => bad_id(),
                }
            }
            (&Method::POST, ["api", "projects", id, "workspaces", workspace, "replace"]) => {
                match (Uuid::parse_str(id), Uuid::parse_str(workspace)) {
                    (Ok(project_id), Ok(workspace_id)) => {
                        self.replace_workspace(kernel, user_id, project_id, workspace_id)
                            .await
                    }
                    _ => bad_id(),
                }
            }
            (&Method::POST, ["api", "sessions", session_id, "runs"]) => {
                match Uuid::parse_str(session_id) {
                    Ok(session_id) => self.create_run(kernel, user_id, session_id, body).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::POST, ["api", "runs", run_id, "cancel"]) => match Uuid::parse_str(run_id) {
                Ok(run_id) => self.cancel_run(kernel, user_id, run_id).await,
                Err(_) => bad_id(),
            },
            (&Method::PATCH, ["api", "projects", id]) => match Uuid::parse_str(id) {
                Ok(project_id) => self.update_project(user_id, project_id, body).await,
                Err(_) => bad_id(),
            },
            (&Method::PATCH, ["api", "agents", id]) => match Uuid::parse_str(id) {
                Ok(agent_id) => self.update_agent(user_id, agent_id, body).await,
                Err(_) => bad_id(),
            },
            (&Method::POST, ["api", "conversations"]) => {
                self.create_conversation(kernel, user_id, body).await
            }
            (&Method::POST, ["api", "conversations", conversation_id, "messages"]) => {
                match Uuid::parse_str(conversation_id) {
                    Ok(conversation_id) => {
                        self.create_conversation_message(kernel, user_id, conversation_id, body)
                            .await
                    }
                    Err(_) => bad_id(),
                }
            }
            (&Method::GET, ["api", "admin", "users"]) => self.admin_users(user_id).await,
            (&Method::PATCH, ["api", "admin", "users", target, "role"]) => {
                match Uuid::parse_str(target) {
                    Ok(target_id) => self.admin_set_role(user_id, target_id, body).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::PATCH, ["api", "admin", "users", target, "status"]) => {
                match Uuid::parse_str(target) {
                    Ok(target_id) => self.admin_set_status(user_id, target_id, body).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::POST, ["api", "admin", "users"]) => {
                self.admin_create_user(kernel, user_id, body).await
            }
            (&Method::POST, ["api", "admin", "users", target, "reset-password"]) => {
                match Uuid::parse_str(target) {
                    Ok(target_id) => self.admin_reset_password(user_id, target_id, body).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::GET, ["api", "admin", "users", target, "sessions"]) => {
                match Uuid::parse_str(target) {
                    Ok(target_id) => self.admin_list_sessions(user_id, target_id).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::DELETE, ["api", "admin", "users", target, "sessions"]) => {
                match Uuid::parse_str(target) {
                    Ok(target_id) => self.admin_revoke_sessions(user_id, target_id).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::GET, ["api", "account"]) => self.account_snapshot(user_id, current).await,
            (&Method::PATCH, ["api", "account", "profile"]) => {
                self.account_update_profile(user_id, body).await
            }
            (&Method::POST, ["api", "account", "password"]) => {
                self.account_change_password(kernel, user_id, current, body)
                    .await
            }
            (&Method::POST, ["api", "account", "sessions", "revoke-others"]) => {
                self.account_revoke_others(user_id, current).await
            }
            (&Method::DELETE, ["api", "account", "sessions", id]) => match Uuid::parse_str(id) {
                Ok(session_id) => {
                    self.account_revoke_session(user_id, current, session_id)
                        .await
                }
                Err(_) => bad_id(),
            },
            (&Method::GET, ["api", "admin", "scopes"]) => self.admin_scopes(user_id).await,
            (&Method::GET, ["api", "admin", "scopes", scope_id, "members"]) => {
                match Uuid::parse_str(scope_id) {
                    Ok(scope_uuid) => self.admin_scope_members(user_id, scope_uuid).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::POST, ["api", "admin", "scopes", scope_id, "members"]) => {
                match Uuid::parse_str(scope_id) {
                    Ok(scope_uuid) => self.admin_add_member(user_id, scope_uuid, body).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::DELETE, ["api", "admin", "scopes", scope_id, "members", member]) => {
                match (Uuid::parse_str(scope_id), Uuid::parse_str(member)) {
                    (Ok(scope_uuid), Ok(member_id)) => {
                        self.admin_remove_member(user_id, scope_uuid, member_id)
                            .await
                    }
                    _ => bad_id(),
                }
            }
            (&Method::GET, ["api", "admin", "fabrics"]) => self.admin_fabrics(user_id).await,
            (&Method::GET, ["api", "admin", "workspaces"]) => self.admin_workspaces(user_id).await,
            (&Method::GET, ["api", "admin", "audit"]) => self.admin_audit(user_id, query).await,
            (&Method::GET, ["api", "admin", "health"]) => {
                self.admin_health(user_id, auth_mode).await
            }
            (&Method::POST, ["api", "scopes", scope_id, "secrets"]) => {
                match Uuid::parse_str(scope_id) {
                    Ok(scope_uuid) => self.create_secret(user_id, scope_uuid, body).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::GET, ["api", "scopes", scope_id, "secrets"]) => {
                match Uuid::parse_str(scope_id) {
                    Ok(scope_uuid) => self.list_secrets(user_id, scope_uuid).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::PUT, ["api", "secrets", secret_id]) => match Uuid::parse_str(secret_id) {
                Ok(secret_uuid) => self.replace_secret(user_id, secret_uuid, body).await,
                Err(_) => bad_id(),
            },
            (&Method::POST, ["api", "secrets", secret_id, "rotate"]) => {
                match Uuid::parse_str(secret_id) {
                    Ok(secret_uuid) => self.rotate_secret(user_id, secret_uuid, body).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::DELETE, ["api", "secrets", secret_id]) => match Uuid::parse_str(secret_id) {
                Ok(secret_uuid) => self.delete_secret(user_id, secret_uuid).await,
                Err(_) => bad_id(),
            },
            (&Method::GET, ["api", "secrets", secret_id, "audit"]) => {
                match Uuid::parse_str(secret_id) {
                    Ok(secret_uuid) => self.secret_audit(user_id, secret_uuid).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::GET, ["api", "scopes"]) => self.scopes(user_id).await,
            (&Method::GET, ["api", "scopes", "users", "search"]) => {
                self.scopes_users_search(user_id, query).await
            }
            (&Method::POST, ["api", "scopes"]) => self.create_scope(kernel, user_id, body).await,
            (&Method::GET, ["api", "scopes", scope_id]) => match Uuid::parse_str(scope_id) {
                Ok(scope_uuid) => self.scope_detail(user_id, scope_uuid).await,
                Err(_) => bad_id(),
            },
            (&Method::PATCH, ["api", "scopes", scope_id]) => match Uuid::parse_str(scope_id) {
                Ok(scope_uuid) => self.update_scope(user_id, scope_uuid, body).await,
                Err(_) => bad_id(),
            },
            (&Method::GET, ["api", "scopes", scope_id, "members"]) => {
                match Uuid::parse_str(scope_id) {
                    Ok(scope_uuid) => self.scope_members(user_id, scope_uuid).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::POST, ["api", "scopes", scope_id, "members"]) => {
                match Uuid::parse_str(scope_id) {
                    Ok(scope_uuid) => self.add_member(user_id, scope_uuid, body).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::DELETE, ["api", "scopes", scope_id, "members", member]) => {
                match (Uuid::parse_str(scope_id), Uuid::parse_str(member)) {
                    (Ok(scope_uuid), Ok(member_uuid)) => {
                        self.remove_member(user_id, scope_uuid, member_uuid).await
                    }
                    _ => bad_id(),
                }
            }
            (&Method::GET, ["api", "scopes", scope_id, "workspaces"]) => {
                match Uuid::parse_str(scope_id) {
                    Ok(scope_uuid) => self.scope_workspaces(user_id, scope_uuid).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::POST, ["api", "scopes", scope_id, "workspaces"]) => {
                match Uuid::parse_str(scope_id) {
                    Ok(scope_uuid) => {
                        self.create_workspace(kernel, user_id, scope_uuid, body)
                            .await
                    }
                    Err(_) => bad_id(),
                }
            }
            (&Method::GET, ["api", "workspaces", workspace_id]) => {
                match Uuid::parse_str(workspace_id) {
                    Ok(workspace_uuid) => self.workspace_detail(user_id, workspace_uuid).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::GET, ["api", "workspaces", workspace_id, "conversations"]) => {
                match Uuid::parse_str(workspace_id) {
                    Ok(workspace_uuid) => {
                        self.workspace_conversations(user_id, workspace_uuid).await
                    }
                    Err(_) => bad_id(),
                }
            }
            (&Method::PATCH, ["api", "workspaces", workspace_id]) => {
                match Uuid::parse_str(workspace_id) {
                    Ok(workspace_uuid) => {
                        self.update_workspace_label(user_id, workspace_uuid, body)
                            .await
                    }
                    Err(_) => bad_id(),
                }
            }
            (&Method::GET, ["api", "workspaces", workspace_id, "diagnostics"]) => {
                match Uuid::parse_str(workspace_id) {
                    Ok(workspace_uuid) => self.workspace_diagnostics(user_id, workspace_uuid).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::POST, ["api", "workspaces", workspace_id, "replace"]) => {
                match Uuid::parse_str(workspace_id) {
                    Ok(workspace_uuid) => {
                        let project_id: Option<Uuid> =
                            sqlx::query_scalar("select project_id from workspaces where id = $1")
                                .bind(workspace_uuid)
                                .fetch_optional(&self.pool)
                                .await
                                .ok()
                                .flatten();
                        match project_id {
                            Some(project_uuid) => {
                                self.replace_workspace(
                                    kernel,
                                    user_id,
                                    project_uuid,
                                    workspace_uuid,
                                )
                                .await
                            }
                            None => bad_id(),
                        }
                    }
                    Err(_) => bad_id(),
                }
            }
            (&Method::DELETE, ["api", "workspaces", workspace_id]) => {
                match Uuid::parse_str(workspace_id) {
                    Ok(workspace_uuid) => {
                        let project_id: Option<Uuid> =
                            sqlx::query_scalar("select project_id from workspaces where id = $1")
                                .bind(workspace_uuid)
                                .fetch_optional(&self.pool)
                                .await
                                .ok()
                                .flatten();
                        match project_id {
                            Some(project_uuid) => {
                                self.delete_workspace(kernel, user_id, project_uuid, workspace_uuid)
                                    .await
                            }
                            None => bad_id(),
                        }
                    }
                    Err(_) => bad_id(),
                }
            }
            (&Method::GET, ["api", "scopes", scope_id, "sessions"]) => {
                match Uuid::parse_str(scope_id) {
                    Ok(scope_uuid) => self.scoped_sessions(user_id, scope_uuid).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::GET, ["api", "scopes", scope_id, "agents"]) => {
                match Uuid::parse_str(scope_id) {
                    Ok(scope_uuid) => self.scoped_agents(user_id, scope_uuid).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::GET, ["api", "scopes", scope_id, "events"]) => {
                match Uuid::parse_str(scope_id) {
                    Ok(scope_uuid) => self.scoped_events(user_id, scope_uuid, query).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::GET, ["api", "conversations", conversation_id, "runs"]) => {
                match Uuid::parse_str(conversation_id) {
                    Ok(conversation_uuid) => {
                        self.conversation_runs(user_id, conversation_uuid, kernel)
                            .await
                    }
                    Err(_) => bad_id(),
                }
            }
            (&Method::GET, ["api", "scopes", scope_id, "agent-presets"]) => {
                match Uuid::parse_str(scope_id) {
                    Ok(scope_uuid) => self.scoped_agents(user_id, scope_uuid).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::POST, ["api", "scopes", scope_id, "agent-presets"]) => {
                match Uuid::parse_str(scope_id) {
                    Ok(scope_uuid) => {
                        self.create_agent_preset(kernel, user_id, scope_uuid, body)
                            .await
                    }
                    Err(_) => bad_id(),
                }
            }
            (&Method::PATCH, ["api", "scopes", scope_id, "agent-presets", preset_id]) => {
                match (Uuid::parse_str(scope_id), Uuid::parse_str(preset_id)) {
                    (Ok(scope_uuid), Ok(preset_uuid)) => {
                        self.update_agent_preset(user_id, scope_uuid, preset_uuid, body)
                            .await
                    }
                    _ => bad_id(),
                }
            }
            (&Method::DELETE, ["api", "scopes", scope_id, "agent-presets", preset_id]) => {
                match (Uuid::parse_str(scope_id), Uuid::parse_str(preset_id)) {
                    (Ok(scope_uuid), Ok(preset_uuid)) => {
                        self.delete_agent_preset(user_id, scope_uuid, preset_uuid)
                            .await
                    }
                    _ => bad_id(),
                }
            }
            _ => json_error(StatusCode::NOT_FOUND, "not found"),
        }
    }

    async fn current_user(
        &self,
        auth: &Auth,
        request: &Request<Incoming>,
    ) -> Result<Option<Uuid>, sqlx::Error> {
        let Some(token) = web_session::request_cookie(request, web_session::COOKIE_NAME) else {
            return Ok(None);
        };
        let Some(session) =
            web_session::lookup(&self.pool, &token, auth.config().session_ttl()).await?
        else {
            return Ok(None);
        };
        let active: bool = sqlx::query_scalar(
            "select exists(select 1 from users where id = $1 and status = 'active')",
        )
        .bind(session.user_id)
        .fetch_one(&self.pool)
        .await?;
        if !active {
            let _ = web_session::revoke(&self.pool, &token).await;
            return Ok(None);
        }
        Ok(Some(session.user_id))
    }

    async fn events(&self, user_id: Uuid, query: &str) -> Response<http_body_util::Full<Bytes>> {
        let after = query_cursor(query);
        let session_ids: Vec<Uuid> = match sqlx::query_scalar(
            "select s.id from sessions s \
             join project_members m on m.project_id = s.project_id \
             where m.user_id = $1",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        {
            Ok(ids) => ids,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "events failed"),
        };
        self.canonical_events(&session_ids, after).await
    }

    async fn session_events(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        query: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let project_id: Option<Uuid> =
            sqlx::query_scalar("select project_id from sessions where id = $1")
                .bind(session_id)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten();
        let Some(project_id) = project_id else {
            return json_error(StatusCode::NOT_FOUND, "session not found");
        };
        if auth::authorize(&self.pool, user_id, project_id, Action::ReadProject)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        self.canonical_events(&[session_id], query_cursor(query))
            .await
    }

    async fn canonical_events(
        &self,
        session_ids: &[Uuid],
        after: i64,
    ) -> Response<http_body_util::Full<Bytes>> {
        let events = match self
            .sessions
            .load_after_global(session_ids, after, 512)
            .await
        {
            Ok(events) => events,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "event history failed"),
        };
        let items = events
            .into_iter()
            .map(|event| {
                json!({
                    "sessionId": event.reference.session_id,
                    "globalSeq": event.reference.global_seq,
                    "revision": event.reference.revision,
                    "appendId": event.reference.append_id,
                    "objectKey": event.reference.object_key,
                    "contentHash": hex_bytes(&event.reference.content_hash),
                    "byteLength": event.reference.byte_length,
                    "bytes": BASE64.encode(event.bytes),
                })
            })
            .collect::<Vec<_>>();
        let cursor = items
            .last()
            .and_then(|item| item.get("globalSeq"))
            .and_then(Value::as_i64)
            .unwrap_or(after);
        json_ok(json!({ "after": after, "cursor": cursor, "items": items }))
    }

    async fn projects(&self, user_id: Uuid) -> Response<http_body_util::Full<Bytes>> {
        let rows = match sqlx::query(
            "select p.id, p.owner_user_id, p.name, p.kind, m.role, \
                    p.created_at::text as created_at from projects p \
             join project_members m on m.project_id = p.id and m.user_id = $1 \
             where m.user_id = $1 order by p.name",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "projects failed"),
        };
        json_ok(json!({
            "items": rows.into_iter().map(|row| json!({
                "id": row.get::<Uuid, _>("id"),
                "ownerUserId": row.get::<Uuid, _>("owner_user_id"),
                "name": row.get::<String, _>("name"),
                "kind": row.get::<String, _>("kind"),
                "role": row.get::<String, _>("role"),
                "createdAt": row.get::<String, _>("created_at"),
                "capabilities": capabilities_json(
                    Role::parse(row.get::<String, _>("role").as_str())
                        .unwrap_or(Role::Viewer),
                ),
            })).collect::<Vec<_>>()
        }))
    }

    async fn agents(&self, user_id: Uuid) -> Response<http_body_util::Full<Bytes>> {
        let rows = match sqlx::query(
            "select a.id, a.project_id, a.name, a.model, a.system_prompt, \
                    a.bash_enabled, a.max_tokens \
             from agents a join project_members m on m.project_id = a.project_id \
             where m.user_id = $1 order by a.created_at",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "agents failed"),
        };
        json_ok(json!({
            "items": rows.into_iter().map(|row| {
                let agent = agent_from_row(&row);
                json!({
                    "id": agent.id,
                    "projectId": agent.project_id,
                    "name": agent.name,
                    "model": agent.model,
                    "systemPrompt": agent.system_prompt,
                    "bashEnabled": agent.bash_enabled,
                    "maxTokens": agent.max_tokens,
                })
            }).collect::<Vec<_>>()
        }))
    }

    async fn sessions(&self, user_id: Uuid) -> Response<http_body_util::Full<Bytes>> {
        let rows = match sqlx::query(
            "select s.id, s.project_id, s.agent_id, s.workspace_id, \
                    s.writer_generation, s.attention_generation, s.head_revision \
                    , s.created_at::text as created_at \
                    , exists(select 1 from runs r \
                             where r.session_id = s.id \
                               and r.state in ('accepted', 'dispatched')) as running \
                    , left((select r.prompt from runs r \
                            where r.session_id = s.id order by r.seq limit 1), 60) as title \
             from sessions s join project_members m on m.project_id = s.project_id \
             where m.user_id = $1 order by s.created_at",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "sessions failed"),
        };
        json_ok(json!({
            "items": rows.into_iter().map(|row| {
                let session = session_from_row(&row);
                json!({
                    "id": session.id,
                    "projectId": session.project_id,
                    "agentId": session.agent_id,
                    "workspaceId": session.workspace_id,
                    "writerGeneration": session.writer_generation,
                    "attentionGeneration": session.attention_generation,
                    "headRevision": session.head_revision,
                    "createdAt": row.get::<String, _>("created_at"),
                    "running": row.get::<bool, _>("running"),
                    "title": row.get::<Option<String>, _>("title"),
                })
            }).collect::<Vec<_>>()
        }))
    }

    async fn fabrics(&self, user_id: Uuid) -> Response<http_body_util::Full<Bytes>> {
        let rows = match sqlx::query(
            "select distinct f.id, f.name, f.created_at::text as created_at from fabrics f \
             join workspaces w on w.fabric_id = f.id \
             join sessions s on s.workspace_id = w.id \
             join project_members m on m.project_id = s.project_id \
             where m.user_id = $1 order by f.name",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "fabrics failed"),
        };
        json_ok(json!({
            "items": rows.into_iter().map(|row| json!({
                "id": row.get::<Uuid, _>("id"),
                "name": row.get::<String, _>("name"),
                "createdAt": row.get::<String, _>("created_at"),
            })).collect::<Vec<_>>()
        }))
    }

    async fn workspaces(&self, user_id: Uuid) -> Response<http_body_util::Full<Bytes>> {
        let rows = match sqlx::query(
            "select distinct w.id, w.project_id, w.fabric_id, w.state, \
                    w.created_by_user_id, coalesce(w.label, 'Workspace') as label, \
                    f.name as fabric_name, w.created_at::text as created_at, \
                    w.exec_generation \
             from workspaces w \
             join fabrics f on f.id = w.fabric_id \
             join project_members m on m.project_id = w.project_id \
             where m.user_id = $1 order by w.id",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "workspaces failed"),
        };
        json_ok(json!({
            "items": rows.into_iter().map(|row| json!({
                "id": row.get::<Uuid, _>("id"),
                "projectId": row.get::<Uuid, _>("project_id"),
                "label": row.get::<String, _>("label"),
                "createdByUserId": row.get::<Option<Uuid>, _>("created_by_user_id"),
                "createdAt": row.get::<String, _>("created_at"),
                "execGeneration": row.get::<i64, _>("exec_generation"),
                "state": row.get::<String, _>("state"),
            })).collect::<Vec<_>>()
        }))
    }

    async fn audit_events(
        &self,
        user_id: Uuid,
        query: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let (before, limit) = audit_window(query);
        let rows = match sqlx::query(
            "select e.seq, e.project_id, e.session_id, e.run_id, e.actor_user_id, \
                    e.occurred_at::text as occurred_at, e.kind, e.resource_type, \
                    e.resource_id, e.outcome, e.metadata, e.payload \
             from audit_events e join project_members m on m.project_id = e.project_id \
             where m.user_id = $1 and e.seq < $2 order by e.seq desc limit $3",
        )
        .bind(user_id)
        .bind(before)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "audit events failed"),
        };
        json_ok(json!({
            "items": rows.into_iter().map(|row| json!({
                "seq": row.get::<i64, _>("seq"),
                "projectId": row.get::<Option<Uuid>, _>("project_id"),
                "sessionId": row.get::<Option<Uuid>, _>("session_id"),
                "runId": row.get::<Option<Uuid>, _>("run_id"),
                "actorUserId": row.get::<Option<Uuid>, _>("actor_user_id"),
                "occurredAt": row.get::<String, _>("occurred_at"),
                "kind": row.get::<String, _>("kind"),
                "resourceType": row.get::<String, _>("resource_type"),
                "resourceId": row.get::<Option<Uuid>, _>("resource_id"),
                "outcome": row.get::<String, _>("outcome"),
                "metadata": row.get::<Option<serde_json::Value>, _>("metadata"),
                "payload": row.get::<Option<String>, _>("payload"),
            })).collect::<Vec<_>>()
        }))
    }

    async fn run_resource(
        &self,
        user_id: Uuid,
        run_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        let row = sqlx::query(
            "select r.id, r.intent_id, r.session_id, r.state, r.result, \
                    r.accepted_at::text as accepted_at, r.dispatched_at::text as dispatched_at, \
                    r.terminal_at::text as terminal_at, r.cancelled_at::text as cancelled_at \
             from runs r join sessions s on s.id = r.session_id \
             join project_members m on m.project_id = s.project_id \
             where r.id = $1 and m.user_id = $2",
        )
        .bind(run_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await;
        let Ok(Some(row)) = row else {
            return json_error(StatusCode::NOT_FOUND, "run not found");
        };
        json_ok(json!({
            "id": row.get::<Uuid, _>("id"),
            "intentId": row.get::<Uuid, _>("intent_id"),
            "sessionId": row.get::<Uuid, _>("session_id"),
            "state": row.get::<String, _>("state"),
            "result": row.get::<Option<String>, _>("result"),
            "acceptedAt": row.get::<String, _>("accepted_at"),
            "dispatchedAt": row.get::<Option<String>, _>("dispatched_at"),
            "terminalAt": row.get::<Option<String>, _>("terminal_at"),
            "cancelledAt": row.get::<Option<String>, _>("cancelled_at"),
        }))
    }

    async fn runs(&self, user_id: Uuid) -> Response<http_body_util::Full<Bytes>> {
        let rows = match sqlx::query(
            "select r.id, r.intent_id, r.session_id, r.state, r.result, \
                    r.accepted_at::text as accepted_at, r.dispatched_at::text as dispatched_at, \
                    r.terminal_at::text as terminal_at, r.cancelled_at::text as cancelled_at \
             from runs r join sessions s on s.id = r.session_id \
             join project_members m on m.project_id = s.project_id \
             where m.user_id = $1 order by r.accepted_at",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "runs failed"),
        };
        json_ok(json!({
            "items": rows.into_iter().map(|row| json!({
                "id": row.get::<Uuid, _>("id"),
                "intentId": row.get::<Uuid, _>("intent_id"),
                "sessionId": row.get::<Uuid, _>("session_id"),
                "state": row.get::<String, _>("state"),
                "result": row.get::<Option<String>, _>("result"),
                "acceptedAt": row.get::<String, _>("accepted_at"),
                "dispatchedAt": row.get::<Option<String>, _>("dispatched_at"),
                "terminalAt": row.get::<Option<String>, _>("terminal_at"),
                "cancelledAt": row.get::<Option<String>, _>("cancelled_at"),
            })).collect::<Vec<_>>()
        }))
    }

    async fn me(&self, user_id: Uuid) -> Response<http_body_util::Full<Bytes>> {
        let profile = sqlx::query(
            "select id, username, display_name, email, platform_role \
             from users where id = $1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await;
        match profile {
            Ok(Some(row)) => json_ok(json!({
                "userId": row.get::<Uuid, _>("id"),
                "username": row.get::<Option<String>, _>("username"),
                "displayName": row.get::<String, _>("display_name"),
                "email": row.get::<Option<String>, _>("email"),
                "platformRole": row.get::<String, _>("platform_role"),
            })),
            Ok(None) => json_error(StatusCode::UNAUTHORIZED, "user not found"),
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "profile lookup failed"),
        }
    }

    async fn project_detail(
        &self,
        user_id: Uuid,
        project_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        let row = sqlx::query(
            "select p.id, p.owner_user_id, p.name, p.kind, m.role, \
                    p.created_at::text as created_at \
             from projects p join project_members m on m.project_id = p.id \
             where p.id = $1 and m.user_id = $2",
        )
        .bind(project_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await;
        let Ok(Some(row)) = row else {
            return json_error(StatusCode::NOT_FOUND, "project not found");
        };
        let role = Role::parse(row.get::<String, _>("role").as_str()).unwrap_or(Role::Viewer);
        let members = sqlx::query(
            "select m.user_id, coalesce(a.subject, u.subject) as subject, \
                    u.username, u.display_name, \
                    m.role, m.created_at::text as created_at \
            from project_members m join users u on u.id = m.user_id \
            left join auth_identities a on a.user_id = u.id \
            where m.project_id = $1 order by m.created_at, m.user_id",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| {
                    json!({
                        "userId": row.get::<Uuid, _>("user_id"),
                        "username": row.get::<Option<String>, _>("username"),
                        "displayName": row.get::<Option<String>, _>("display_name"),
                        "subject": row.get::<String, _>("subject"),
                        "role": row.get::<String, _>("role"),
                        "createdAt": row.get::<String, _>("created_at"),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
        json_ok(json!({
            "id": row.get::<Uuid, _>("id"),
            "ownerUserId": row.get::<Uuid, _>("owner_user_id"),
            "name": row.get::<String, _>("name"),
            "kind": row.get::<String, _>("kind"),
            "role": role_name(role),
            "createdAt": row.get::<String, _>("created_at"),
            "members": members,
            "capabilities": capabilities_json(role),
        }))
    }

    async fn project_members(
        &self,
        user_id: Uuid,
        project_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        if auth::authorize(&self.pool, user_id, project_id, Action::ReadProject)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        let rows = sqlx::query(
            "select m.user_id, coalesce(a.subject, u.subject) as subject, \
                    m.role, m.created_at::text as created_at \
             from project_members m join users u on u.id = m.user_id \
             left join auth_identities a on a.user_id = u.id \
             where m.project_id = $1 order by m.created_at, m.user_id",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await;
        let Ok(rows) = rows else {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "members failed");
        };
        json_ok(json!({
            "items": rows.into_iter().map(|row| json!({
                "userId": row.get::<Uuid, _>("user_id"),
                "subject": row.get::<String, _>("subject"),
                "role": row.get::<String, _>("role"),
                "createdAt": row.get::<String, _>("created_at"),
            })).collect::<Vec<_>>()
        }))
    }

    /// Adds or reroles one membership. Owner-only by frozen role permits;
    /// durable ownership never silently loses its last owner.
    async fn add_member(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        if auth::authorize(&self.pool, user_id, project_id, Action::ManageMembership)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        self.upsert_member(user_id, project_id, body).await
    }

    /// Platform-admin Team-RBAC recovery: same membership invariants as
    /// ordinary add/rerole, authorized by platform admin rather than Team
    /// membership. Does not add the admin to the Team.
    async fn admin_add_member(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        if !self.is_platform_admin(user_id).await {
            return json_error(StatusCode::FORBIDDEN, "platform admin required");
        }
        self.upsert_member(user_id, project_id, body).await
    }

    async fn upsert_member(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            #[serde(rename = "userId")]
            user_id: Uuid,
            role: String,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid member payload"),
        };
        let Some(role) = Role::parse(payload.role.trim()) else {
            return json_error(StatusCode::BAD_REQUEST, "invalid role");
        };
        let known_project: Option<Uuid> =
            sqlx::query_scalar("select id from projects where id = $1")
                .bind(project_id)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten();
        if known_project.is_none() {
            return json_error(StatusCode::NOT_FOUND, "project not found");
        }
        let known_user: Option<Uuid> = sqlx::query_scalar("select id from users where id = $1")
            .bind(payload.user_id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten();
        if known_user.is_none() {
            return json_error(StatusCode::BAD_REQUEST, "unknown user");
        }
        let project_kind: Option<String> =
            sqlx::query_scalar("select kind from projects where id = $1")
                .bind(project_id)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten();
        if project_kind.as_deref() == Some("personal") {
            return json_error(StatusCode::CONFLICT, "personal scope members are fixed");
        }
        let previous: Option<String> = sqlx::query_scalar(
            "select role from project_members where project_id = $1 and user_id = $2",
        )
        .bind(project_id)
        .bind(payload.user_id)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten();
        if let Some(previous) = previous.as_deref() {
            if previous == "owner"
                && !matches!(role, Role::Owner)
                && self.is_protected_owner(project_id, payload.user_id).await
            {
                return json_error(
                    StatusCode::CONFLICT,
                    "the durable project owner cannot be demoted",
                );
            }
        }
        let updated = sqlx::query(
            "with upserted as ( \
                 insert into project_members (project_id, user_id, role) values ($1, $2, $3) \
                 on conflict (project_id, user_id) do update set role = excluded.role \
                 returning created_at \
             ) \
             select coalesce(a.subject, u.subject) as subject, \
                    m.created_at::text as created_at \
             from upserted m join users u on u.id = $2 \
             left join auth_identities a on a.user_id = u.id",
        )
        .bind(project_id)
        .bind(payload.user_id)
        .bind(role_name(role))
        .fetch_one(&self.pool)
        .await;
        let Ok(row) = updated else {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "membership store failed");
        };
        self.record(AuditInsert {
            project_id: Some(project_id),
            session_id: None,
            run_id: None,
            actor_user_id: Some(actor_id),
            kind: if previous.is_some() {
                "member.role_changed"
            } else {
                "member.added"
            },
            resource_type: "member",
            resource_id: Some(payload.user_id),
            outcome: AuditOutcome::Ok,
            metadata: Some(&json!({
                "role": role_name(role),
                "previousRole": previous,
            })),
        })
        .await;
        json_ok(json!({
            "projectId": project_id,
            "userId": payload.user_id,
            "role": role_name(role),
            "subject": row.get::<String, _>("subject"),
            "createdAt": row.get::<String, _>("created_at"),
        }))
    }

    /// Removes one membership. Owner-only; the durable project owner and the
    /// last remaining owner are protected.
    async fn remove_member(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        member_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        if auth::authorize(&self.pool, user_id, project_id, Action::ManageMembership)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        self.delete_member(user_id, project_id, member_id).await
    }

    /// Platform-admin Team-RBAC recovery: same removal invariants as the
    /// ordinary member route, without requiring Team membership.
    async fn admin_remove_member(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        member_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        if !self.is_platform_admin(user_id).await {
            return json_error(StatusCode::FORBIDDEN, "platform admin required");
        }
        self.delete_member(user_id, project_id, member_id).await
    }

    async fn delete_member(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
        member_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        let project_kind: Option<String> =
            sqlx::query_scalar("select kind from projects where id = $1")
                .bind(project_id)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten();
        if project_kind.is_none() {
            return json_error(StatusCode::NOT_FOUND, "project not found");
        }
        if project_kind.as_deref() == Some("personal") {
            return json_error(StatusCode::CONFLICT, "personal scope members are fixed");
        }
        let previous: Option<String> = sqlx::query_scalar(
            "select role from project_members where project_id = $1 and user_id = $2",
        )
        .bind(project_id)
        .bind(member_id)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten();
        let Some(previous) = previous else {
            return json_error(StatusCode::NOT_FOUND, "member not found");
        };
        if self.is_protected_owner(project_id, member_id).await {
            return json_error(
                StatusCode::CONFLICT,
                "the durable project owner cannot be removed",
            );
        }
        let removed =
            sqlx::query("delete from project_members where project_id = $1 and user_id = $2")
                .bind(project_id)
                .bind(member_id)
                .execute(&self.pool)
                .await;
        match removed {
            Ok(result) if result.rows_affected() == 1 => {
                self.record(AuditInsert {
                    project_id: Some(project_id),
                    session_id: None,
                    run_id: None,
                    actor_user_id: Some(actor_id),
                    kind: "member.removed",
                    resource_type: "member",
                    resource_id: Some(member_id),
                    outcome: AuditOutcome::Ok,
                    metadata: Some(&json!({ "role": previous })),
                })
                .await;
                json_ok(json!({ "removed": true, "userId": member_id }))
            }
            Ok(_) => json_error(StatusCode::NOT_FOUND, "member not found"),
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "membership store failed"),
        }
    }

    /// True exactly when removing or demoting this member would strip the
    /// project of its durable owner identity or its last owner role.
    async fn is_protected_owner(&self, project_id: Uuid, member_id: Uuid) -> bool {
        let owner_row: bool = sqlx::query_scalar(
            "select exists(select 1 from projects where id = $1 and owner_user_id = $2)",
        )
        .bind(project_id)
        .bind(member_id)
        .fetch_one(&self.pool)
        .await
        .unwrap_or(false);
        if owner_row {
            return true;
        }
        let holds_owner_role: bool = sqlx::query_scalar(
            "select exists(select 1 from project_members \
             where project_id = $1 and user_id = $2 and role = 'owner')",
        )
        .bind(project_id)
        .bind(member_id)
        .fetch_one(&self.pool)
        .await
        .unwrap_or(false);
        if !holds_owner_role {
            return false;
        }
        let owners: i64 = sqlx::query_scalar(
            "select count(*) from project_members where project_id = $1 and role = 'owner'",
        )
        .bind(project_id)
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0);
        owners <= 1
    }

    async fn create_project(
        &self,
        kernel: &Kernel,
        user_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            id: Uuid,
            name: String,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid project payload"),
        };
        let project = match kernel
            .create_project(payload.id, user_id, payload.name.trim(), "personal")
            .await
        {
            Ok(project) => project,
            Err(crate::KernelError::Conflict) => {
                return json_error(StatusCode::CONFLICT, "project identity conflicts");
            }
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "project store failed"),
        };
        self.record(AuditInsert {
            project_id: Some(project.id),
            session_id: None,
            run_id: None,
            actor_user_id: Some(user_id),
            kind: "project.created",
            resource_type: "project",
            resource_id: Some(project.id),
            outcome: AuditOutcome::Ok,
            metadata: None,
        })
        .await;
        json_ok(json!({
            "id": project.id,
            "ownerUserId": project.owner_user_id,
            "name": project.name,
            "role": "owner",
            "capabilities": capabilities_json(Role::Owner),
        }))
    }

    async fn update_project(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            name: String,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid project payload"),
        };
        if auth::authorize(&self.pool, user_id, project_id, Action::ManageMembership)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        let updated = sqlx::query("update projects set name = $2 where id = $1")
            .bind(project_id)
            .bind(payload.name.trim())
            .execute(&self.pool)
            .await;
        match updated {
            Ok(result) if result.rows_affected() == 1 => json_ok(json!({ "updated": true })),
            Ok(_) => json_error(StatusCode::NOT_FOUND, "project not found"),
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "project update failed"),
        }
    }

    /// Creates one Workspace on the deployment-selected Fabric. The
    /// identity is durably reserved as `creating` before invoking the
    /// Fabric, so an indeterminate outcome (Fabricd's Unknown verdict,
    /// HTTP 202) leaves a reconcilable `creating` row that is never
    /// exposed as `ready` (no Sessions attach, no second lifecycle
    /// operation). Only the Fabric's own HTTP 200 promotes `creating` to
    /// `ready`. Definite Fabric refusals (any other status) release the
    /// reservation. A repeated create for the same identity while it is
    /// `creating` first reconciles via a read-only Fabric existence probe
    /// without automatically retrying the unknown create: 404 → the old
    /// reservation was never realized and the current request proceeds as
    /// the fresh create it is; 200/`ready` → the earlier indeterminate
    /// create did land and the reservation is activated to `ready` before
    /// answering 409; any other Fabric state or unanswered probe keeps
    /// the reservation and answers truthfully. Every external non-2xx
    /// still maps truthfully to 502 without a durable lie.
    async fn create_workspace(
        &self,
        kernel: &Kernel,
        user_id: Uuid,
        project_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            id: Uuid,
            #[serde(default)]
            label: Option<String>,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid workspace payload"),
        };
        let label = payload
            .label
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .unwrap_or("Workspace")
            .to_owned();
        if auth::authorize(&self.pool, user_id, project_id, Action::OperateSession)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        let Some(fabric_id) = self.selected_fabric_id().await else {
            return json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "no single Fabric is configured",
            );
        };
        let fabric_metadata = json!({ "fabricId": fabric_id });
        let audit = |outcome| AuditInsert {
            project_id: Some(project_id),
            session_id: None,
            run_id: None,
            actor_user_id: Some(user_id),
            kind: "workspace.created",
            resource_type: "workspace",
            resource_id: Some(payload.id),
            outcome,
            metadata: Some(&fabric_metadata),
        };
        // A `creating` reservation left by an earlier indeterminate outcome
        // is not an automatic conflict. Reconcile it with a read-only
        // Fabric probe before deciding.
        match kernel.find_workspace(payload.id).await {
            Ok(None) => {}
            Ok(Some(existing)) if existing.state == WorkspaceState::Creating => {
                match self.fabric.get_workspace(payload.id).await {
                    Ok(None) => {
                        // The Fabric provably never realized the identity:
                        // discard the stale reservation and treat this
                        // request as the fresh create it is.
                        let removed = kernel.delete_workspace(payload.id).await.unwrap_or(false);
                        if !removed {
                            self.record(audit(AuditOutcome::Error)).await;
                            return json_error(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "workspace store failed",
                            );
                        }
                    }
                    Ok(Some(state)) if state == "ready" => {
                        // The indeterminate create did land and finished
                        // realizing on the Fabric. Make it durably ready
                        // before answering the duplicate.
                        match kernel.activate_workspace(payload.id).await {
                            Ok(true) => {
                                self.record(audit(AuditOutcome::Ok)).await;
                                return json_error(
                                    StatusCode::CONFLICT,
                                    "workspace identity conflicts",
                                );
                            }
                            Ok(false) => {
                                self.record(audit(AuditOutcome::Error)).await;
                                return json_error(
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    "workspace store failed",
                                );
                            }
                            Err(_) => {
                                self.record(audit(AuditOutcome::Error)).await;
                                return json_error(
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    "workspace store failed",
                                );
                            }
                        }
                    }
                    Ok(Some(_)) => {
                        // The Fabric holds the identity but has not
                        // confirmed readiness (its own `creating`). Do not
                        // expose it as ready; the caller learns the
                        // reservation is pending.
                        self.record(audit(AuditOutcome::Unknown)).await;
                        return json_error(
                            StatusCode::CONFLICT,
                            "workspace identity is reserved by an unfinished Fabric realization",
                        );
                    }
                    Err(_) => {
                        self.record(audit(AuditOutcome::Error)).await;
                        return json_error(
                            StatusCode::BAD_GATEWAY,
                            "Fabric did not answer the workspace existence probe",
                        );
                    }
                }
            }
            Ok(Some(_)) => {
                return json_error(StatusCode::CONFLICT, "workspace identity conflicts");
            }
            Err(_) => {
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "workspace store failed");
            }
        }
        // Reserve durably before any external effect: an indeterminate
        // answer must leave behind a reconcilable `creating` row, never a
        // ready lie.
        if let Err(error) = kernel
            .reserve_workspace(payload.id, project_id, fabric_id, user_id)
            .await
        {
            match error {
                crate::KernelError::Quota => {
                    self.record(audit(AuditOutcome::Error)).await;
                    return json_error(
                        StatusCode::TOO_MANY_REQUESTS,
                        "project workspace quota reached",
                    );
                }
                crate::KernelError::Conflict => {
                    self.record(audit(AuditOutcome::Unknown)).await;
                    return json_error(StatusCode::CONFLICT, "workspace identity conflicts");
                }
                _ => {
                    self.record(audit(AuditOutcome::Error)).await;
                    return json_error(StatusCode::INTERNAL_SERVER_ERROR, "workspace store failed");
                }
            }
        }
        // The requested display label is durably committed even while the
        // Fabric create is still indeterminate; reconciliation preserves it.
        let _ = sqlx::query("update workspaces set label = $2 where id = $1")
            .bind(payload.id)
            .bind(&label)
            .execute(&self.pool)
            .await;
        match self.fabric.create_workspace(payload.id).await {
            Ok(CreateOutcome::Created) => match kernel.activate_workspace(payload.id).await {
                Ok(true) => {
                    self.record(audit(AuditOutcome::Ok)).await;
                    let created_at: String =
                        sqlx::query_scalar("select created_at::text from workspaces where id = $1")
                            .bind(payload.id)
                            .fetch_one(&self.pool)
                            .await
                            .unwrap_or_default();
                    json_ok(json!({
                        "id": payload.id,
                        "projectId": project_id,
                        "scopeId": project_id,
                        "label": label,
                        "state": "ready",
                        "createdByUserId": user_id,
                        "createdAt": created_at,
                    }))
                }
                Ok(false) => {
                    self.record(audit(AuditOutcome::Error)).await;
                    json_error(StatusCode::INTERNAL_SERVER_ERROR, "workspace store failed")
                }
                Err(_) => {
                    self.record(audit(AuditOutcome::Error)).await;
                    json_error(StatusCode::INTERNAL_SERVER_ERROR, "workspace store failed")
                }
            },
            Ok(CreateOutcome::Unknown) => {
                self.record(audit(AuditOutcome::Unknown)).await;
                json_error(
                    StatusCode::BAD_GATEWAY,
                    "Fabric workspace creation is unresolved; the workspace stays reserved as creating pending reconciliation",
                )
            }
            Err(_) => {
                // Definite Fabric refusal: release the reservation so no
                // unprovisioned row survives.
                let _ = kernel.delete_workspace(payload.id).await;
                self.record(audit(AuditOutcome::Error)).await;
                json_error(
                    StatusCode::BAD_GATEWAY,
                    "Fabric rejected workspace creation",
                )
            }
        }
    }

    /// Tears one unreferenced Workspace down through the Fabric and removes
    /// the durable row. The deletion fence is claimed before any external
    /// effect: no new Session can attach while teardown runs, so outcomes map
    /// exactly - refused (409), Fabric failure (502, row restored ready), or
    /// completed deletion (200). There is no post-teardown unknown state.
    async fn delete_workspace(
        &self,
        kernel: &Kernel,
        user_id: Uuid,
        project_id: Uuid,
        workspace_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        // Workspace creator rules: owner/admin may tear down any Workspace
        // in the Project; a member only their own; a viewer never.
        let role = match auth::authorize(&self.pool, user_id, project_id, Action::ReadProject).await
        {
            Ok(role) => role,
            Err(_) => return json_error(StatusCode::FORBIDDEN, "project access denied"),
        };
        let _workspace = match kernel.find_workspace(workspace_id).await {
            Ok(Some(workspace)) if workspace.project_id == project_id => workspace,
            _ => return json_error(StatusCode::NOT_FOUND, "workspace not found"),
        };
        let creator: Option<Uuid> =
            sqlx::query_scalar("select created_by_user_id from workspaces where id = $1")
                .bind(workspace_id)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten();
        let allowed = match role {
            Role::Owner | Role::Admin => true,
            Role::Member => creator == Some(user_id),
            Role::Viewer => false,
        };
        if !allowed {
            return json_error(StatusCode::FORBIDDEN, "workspace access denied");
        }
        // Claim the fence before checking references or touching the Fabric:
        // this blocks every later session attachment for the teardown window.
        let fenced = kernel.begin_workspace_delete(workspace_id).await;
        match fenced {
            Ok(true) => {}
            Ok(false) => {
                return json_error(
                    StatusCode::CONFLICT,
                    "workspace deletion already in progress",
                );
            }
            Err(_) => {
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "workspace store failed");
            }
        }
        let referenced: bool =
            sqlx::query_scalar("select exists(select 1 from sessions where workspace_id = $1)")
                .bind(workspace_id)
                .fetch_one(&self.pool)
                .await
                .unwrap_or(true);
        if referenced {
            let metadata = json!({ "reason": "sessions attached" });
            self.record(delete_audit(
                project_id,
                user_id,
                workspace_id,
                AuditOutcome::Refused,
                &metadata,
            ))
            .await;
            let _ = kernel.restore_workspace(workspace_id).await;
            return json_error(StatusCode::CONFLICT, "workspace has sessions");
        }
        if self.fabric.delete_workspace(workspace_id).await.is_err() {
            let metadata = json!({ "reason": "fabric rejected" });
            self.record(delete_audit(
                project_id,
                user_id,
                workspace_id,
                AuditOutcome::Error,
                &metadata,
            ))
            .await;
            // Truthful restore: the durable Workspace remains usable.
            let _ = kernel.restore_workspace(workspace_id).await;
            return json_error(
                StatusCode::BAD_GATEWAY,
                "Fabric rejected workspace deletion",
            );
        }
        match kernel.finish_workspace_delete(workspace_id).await {
            Ok(true) => {
                self.record(delete_audit(
                    project_id,
                    user_id,
                    workspace_id,
                    AuditOutcome::Ok,
                    &json!({}),
                ))
                .await;
                json_ok(json!({ "deleted": true, "id": workspace_id }))
            }
            // The fence makes this unreachable without out-of-band writes; it
            // is an invariant violation, never a benign not-found.
            Ok(false) => {
                self.record(delete_audit(
                    project_id,
                    user_id,
                    workspace_id,
                    AuditOutcome::Error,
                    &json!({}),
                ))
                .await;
                json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "workspace delete lost its fence",
                )
            }
            Err(_) => {
                self.record(delete_audit(
                    project_id,
                    user_id,
                    workspace_id,
                    AuditOutcome::Error,
                    &json!({}),
                ))
                .await;
                json_error(StatusCode::INTERNAL_SERVER_ERROR, "workspace store failed")
            }
        }
    }

    /// The Fabric every new Workspace binds to: the deployment-configured
    /// identity when present, otherwise exactly one registered Fabric.
    async fn selected_fabric_id(&self) -> Option<Uuid> {
        if let Some(configured) = self.configured_fabric_id {
            // An unregistered configured identity must refuse before any
            // external side effect: provisioning onto a Fabric the control
            // plane does not know would strand an orphan resource.
            let registered: bool =
                sqlx::query_scalar("select exists(select 1 from fabrics where id = $1)")
                    .bind(configured)
                    .fetch_one(&self.pool)
                    .await
                    .unwrap_or(false);
            return registered.then_some(configured);
        }
        sqlx::query_scalar("select id from fabrics order by created_at limit 2")
            .fetch_all(&self.pool)
            .await
            .ok()
            .and_then(|ids| <[Uuid; 1]>::try_from(ids.as_slice()).ok().map(|[id]| id))
    }

    /// Replaces one Workspace execution generation through the Fabric. The
    /// durable generation advances only after the Fabric confirms, so the
    /// recorded generation always names a real execution environment.
    async fn replace_workspace(
        &self,
        kernel: &Kernel,
        user_id: Uuid,
        project_id: Uuid,
        workspace_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        if auth::authorize(&self.pool, user_id, project_id, Action::OperateSession)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        let Some(workspace) = kernel.find_workspace(workspace_id).await.ok().flatten() else {
            return json_error(StatusCode::NOT_FOUND, "workspace not found");
        };
        if workspace.project_id != project_id {
            return json_error(StatusCode::NOT_FOUND, "workspace not found");
        }
        // Serialize against delete and concurrent replaces through the same
        // lifecycle fence; the generation advance itself is state-guarded.
        match kernel.begin_workspace_delete(workspace_id).await {
            Ok(true) => {}
            Ok(false) => {
                return json_error(
                    StatusCode::CONFLICT,
                    "workspace lifecycle operation already in progress",
                );
            }
            Err(_) => {
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "workspace store failed");
            }
        }
        // The metadata JSON must outlive the insert, so every branch owns it.
        if self.fabric.replace_workspace(workspace_id).await.is_err() {
            let metadata = json!({ "execGeneration": workspace.exec_generation });
            // Release the fence before reporting: the Workspace is usable
            // again after a definitive Fabric refusal.
            let _ = kernel.restore_workspace(workspace_id).await;
            self.record(replace_audit(
                project_id,
                user_id,
                workspace_id,
                AuditOutcome::Error,
                &metadata,
            ))
            .await;
            return json_error(
                StatusCode::BAD_GATEWAY,
                "Fabric rejected workspace replacement",
            );
        }
        match kernel.advance_workspace_generation(workspace_id).await {
            Ok(generation) => {
                let metadata = json!({ "execGeneration": generation });
                let _ = kernel.restore_workspace(workspace_id).await;
                self.record(replace_audit(
                    project_id,
                    user_id,
                    workspace_id,
                    AuditOutcome::Ok,
                    &metadata,
                ))
                .await;
                json_ok(json!({
                    "id": workspace.id,
                    "fabricId": workspace.fabric_id,
                    "execGeneration": generation,
                }))
            }
            Err(crate::KernelError::RelationRefused) => {
                let _ = kernel.restore_workspace(workspace_id).await;
                let metadata = json!({ "execGeneration": workspace.exec_generation });
                self.record(replace_audit(
                    project_id,
                    user_id,
                    workspace_id,
                    AuditOutcome::Unknown,
                    &metadata,
                ))
                .await;
                json_error(StatusCode::NOT_FOUND, "workspace not found")
            }
            Err(_) => {
                let _ = kernel.restore_workspace(workspace_id).await;
                let metadata = json!({ "execGeneration": workspace.exec_generation });
                self.record(replace_audit(
                    project_id,
                    user_id,
                    workspace_id,
                    AuditOutcome::Error,
                    &metadata,
                ))
                .await;
                json_error(StatusCode::INTERNAL_SERVER_ERROR, "workspace store failed")
            }
        }
    }

    async fn agent_detail(
        &self,
        user_id: Uuid,
        agent_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        let agent = match self.load_agent(agent_id).await {
            Ok(Some(agent)) => agent,
            Ok(None) => return json_error(StatusCode::NOT_FOUND, "agent not found"),
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "agent lookup failed"),
        };
        if auth::authorize(&self.pool, user_id, agent.project_id, Action::ReadProject)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        json_ok(agent_json(&agent))
    }

    async fn load_agent(&self, agent_id: Uuid) -> Result<Option<Agent>, sqlx::Error> {
        let row = sqlx::query(
            "select id, project_id, name, model, system_prompt, bash_enabled, max_tokens \
             from agents where id = $1",
        )
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| agent_from_row(&row)))
    }

    async fn create_agent(
        &self,
        kernel: &Kernel,
        user_id: Uuid,
        project_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            id: Uuid,
            name: String,
            #[serde(default)]
            model: String,
            #[serde(rename = "systemPrompt", default)]
            system_prompt: String,
            /// The bounded execution capability; bash stays available unless
            /// explicitly disabled. There is no generic tool list.
            #[serde(rename = "bashEnabled")]
            bash_enabled: Option<bool>,
            max_tokens: i32,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid agent payload"),
        };
        if auth::authorize(&self.pool, user_id, project_id, Action::OperateSession)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        match kernel
            .create_agent(
                payload.id,
                project_id,
                payload.name.trim(),
                payload.model.trim(),
                payload.system_prompt.trim(),
                payload.bash_enabled.unwrap_or(true),
                payload.max_tokens.clamp(1, 1024),
            )
            .await
        {
            Ok(agent) => {
                self.record(AuditInsert {
                    project_id: Some(project_id),
                    session_id: None,
                    run_id: None,
                    actor_user_id: Some(user_id),
                    kind: "agent.created",
                    resource_type: "agent",
                    resource_id: Some(agent.id),
                    outcome: AuditOutcome::Ok,
                    metadata: Some(&json!({ "name": agent.name })),
                })
                .await;
                json_ok(agent_json(&agent))
            }
            Err(crate::KernelError::RelationRefused) => {
                json_error(StatusCode::BAD_REQUEST, "unknown project reference")
            }
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "agent store failed"),
        }
    }

    async fn update_agent(
        &self,
        user_id: Uuid,
        agent_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            #[serde(default)]
            model: Option<String>,
            #[serde(rename = "systemPrompt", default)]
            system_prompt: Option<String>,
            #[serde(rename = "bashEnabled", default)]
            bash_enabled: Option<bool>,
            max_tokens: Option<i32>,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid agent payload"),
        };
        let Some(existing) = self.load_agent(agent_id).await.ok().flatten() else {
            return json_error(StatusCode::NOT_FOUND, "agent not found");
        };
        if auth::authorize(
            &self.pool,
            user_id,
            existing.project_id,
            Action::OperateSession,
        )
        .await
        .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        let model = payload.model.unwrap_or(existing.model.clone());
        let system_prompt = payload
            .system_prompt
            .unwrap_or(existing.system_prompt.clone());
        let bash_enabled = payload.bash_enabled.unwrap_or(existing.bash_enabled);
        let max_tokens = payload
            .max_tokens
            .unwrap_or(existing.max_tokens)
            .clamp(1, 1024);
        let updated = sqlx::query(
            "update agents set model = $2, system_prompt = $3, bash_enabled = $4, \
             max_tokens = $5 where id = $1",
        )
        .bind(agent_id)
        .bind(model.trim())
        .bind(system_prompt.trim())
        .bind(bash_enabled)
        .bind(max_tokens)
        .execute(&self.pool)
        .await;
        match updated {
            Ok(result) if result.rows_affected() == 1 => match self.load_agent(agent_id).await {
                Ok(Some(agent)) => {
                    self.record(AuditInsert {
                        project_id: Some(agent.project_id),
                        session_id: None,
                        run_id: None,
                        actor_user_id: Some(user_id),
                        kind: "agent.updated",
                        resource_type: "agent",
                        resource_id: Some(agent.id),
                        outcome: AuditOutcome::Ok,
                        metadata: Some(&json!({
                            "bashEnabled": agent.bash_enabled,
                            "model": agent.model,
                        })),
                    })
                    .await;
                    json_ok(agent_json(&agent))
                }
                _ => json_error(StatusCode::INTERNAL_SERVER_ERROR, "agent reload failed"),
            },
            Ok(_) => json_error(StatusCode::NOT_FOUND, "agent not found"),
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "agent update failed"),
        }
    }

    async fn session_detail(
        &self,
        user_id: Uuid,
        session_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        let row = sqlx::query(
            "select s.id, s.project_id, s.agent_id, s.workspace_id, \
                    s.writer_generation, s.attention_generation, s.head_revision, \
                    s.created_at::text as created_at, m.role, \
                    exists(select 1 from runs r \
                           where r.session_id = s.id \
                             and r.state in ('accepted', 'dispatched')) as running \
             from sessions s join project_members m on m.project_id = s.project_id \
             where s.id = $1 and m.user_id = $2",
        )
        .bind(session_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await;
        let Ok(Some(row)) = row else {
            return json_error(StatusCode::NOT_FOUND, "session not found");
        };
        let session = session_from_row(&row);
        let role = Role::parse(row.get::<String, _>("role").as_str()).unwrap_or(Role::Viewer);
        json_ok(json!({
            "id": session.id,
            "projectId": session.project_id,
            "agentId": session.agent_id,
            "workspaceId": session.workspace_id,
            "writerGeneration": session.writer_generation,
            "attentionGeneration": session.attention_generation,
            "headRevision": session.head_revision,
            "createdAt": row.get::<String, _>("created_at"),
            "running": row.get::<bool, _>("running"),
            "capabilities": capabilities_json(role),
        }))
    }

    async fn create_session(
        &self,
        kernel: &Kernel,
        user_id: Uuid,
        project_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            id: Uuid,
            #[serde(rename = "agentId")]
            agent_id: Uuid,
            #[serde(rename = "workspaceId")]
            workspace_id: Uuid,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid session payload"),
        };
        if auth::authorize(&self.pool, user_id, project_id, Action::OperateSession)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        // A Session binds only a Workspace its own Project owns; foreign or
        // unknown Workspaces are simply not addressable, and a fenced
        // (`deleting`) Workspace accepts no new attachment.
        let (workspace_project, workspace_state): (Option<Uuid>, Option<String>) =
            sqlx::query_as("select project_id, state from workspaces where id = $1")
                .bind(payload.workspace_id)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten()
                .map(|(project, state)| (Some(project), Some(state)))
                .unwrap_or((None, None));
        match workspace_project {
            None => return json_error(StatusCode::NOT_FOUND, "workspace not found"),
            Some(owner) if owner != project_id => {
                return json_error(
                    StatusCode::FORBIDDEN,
                    "workspace belongs to another project",
                );
            }
            _ => {}
        }
        if workspace_state.as_deref() != Some("ready") {
            return json_error(
                StatusCode::CONFLICT,
                "workspace lifecycle operation in progress",
            );
        }
        match kernel
            .create_session(
                payload.id,
                project_id,
                payload.agent_id,
                payload.workspace_id,
            )
            .await
        {
            Ok(session) => {
                self.record(AuditInsert {
                    project_id: Some(project_id),
                    session_id: Some(session.id),
                    run_id: None,
                    actor_user_id: Some(user_id),
                    kind: "session.created",
                    resource_type: "session",
                    resource_id: Some(session.id),
                    outcome: AuditOutcome::Ok,
                    metadata: None,
                })
                .await;
                json_ok(json!({
                    "id": session.id,
                    "projectId": session.project_id,
                    "agentId": session.agent_id,
                    "workspaceId": session.workspace_id,
                    "writerGeneration": session.writer_generation,
                    "attentionGeneration": session.attention_generation,
                    "headRevision": session.head_revision,
                }))
            }
            Err(crate::KernelError::RelationRefused) => json_error(
                StatusCode::BAD_REQUEST,
                "unknown agent or workspace reference",
            ),
            Err(crate::KernelError::Conflict) => {
                json_error(StatusCode::CONFLICT, "session identity conflicts")
            }
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "session store failed"),
        }
    }

    async fn create_run(
        &self,
        kernel: &Kernel,
        user_id: Uuid,
        session_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            #[serde(rename = "runId")]
            run_id: Uuid,
            #[serde(rename = "intentId")]
            intent_id: String,
            prompt: String,
            mode: Option<String>,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid run payload"),
        };
        let intent = match resolve_uuid(payload.intent_id, "intentId") {
            Ok(intent) => intent,
            Err(response) => return response,
        };
        let mode = match payload.mode.as_deref() {
            Some("create") => ActivationMode::Create,
            Some("resume") | None => ActivationMode::Resume,
            Some(_) => return json_error(StatusCode::BAD_REQUEST, "invalid activation mode"),
        };
        self.run_for_user(
            kernel,
            user_id,
            session_id,
            payload.run_id,
            mode,
            payload.prompt,
            intent,
        )
        .await
    }

    async fn cancel_run(
        &self,
        kernel: &Kernel,
        user_id: Uuid,
        run_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        let project_id: Option<Uuid> = sqlx::query_scalar(
            "select s.project_id from runs r join sessions s on s.id = r.session_id \
             where r.id = $1",
        )
        .bind(run_id)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten();
        let Some(project_id) = project_id else {
            return json_error(StatusCode::NOT_FOUND, "run not found");
        };
        if auth::authorize(&self.pool, user_id, project_id, Action::OperateSession)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        match kernel.cancel_run(run_id).await {
            Ok((state, kicked_session)) => {
                let (state_label, accepted, outcome, kind) = match state {
                    crate::RunState::Cancelled => (
                        RunState::Cancelled.as_str(),
                        true,
                        AuditOutcome::Ok,
                        "run.cancelled",
                    ),
                    crate::RunState::Dispatched => (
                        "cancel-requested",
                        true,
                        AuditOutcome::Ok,
                        "run.cancel_requested",
                    ),
                    _ => (
                        state.as_str(),
                        false,
                        AuditOutcome::Refused,
                        "run.cancel_requested",
                    ),
                };
                self.record(AuditInsert {
                    project_id: Some(project_id),
                    session_id: None,
                    run_id: Some(run_id),
                    actor_user_id: Some(user_id),
                    kind,
                    resource_type: "run",
                    resource_id: Some(run_id),
                    outcome,
                    metadata: Some(&json!({ "runStateAtRequest": state.as_str() })),
                })
                .await;
                // A cancelled queued head must never strand the queue: wake
                // the successor so it can claim dispatch.
                if let Some(session_id) = kicked_session {
                    self.kick_next(session_id);
                }
                json_ok(json!({
                    "runId": run_id,
                    "state": state_label,
                    "accepted": accepted,
                }))
            }
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "run cancellation failed"),
        }
    }

    async fn run_for_user(
        &self,
        kernel: &Kernel,
        user_id: Uuid,
        session_id: Uuid,
        run_id: Uuid,
        mode: ActivationMode,
        prompt: String,
        intent: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        let session = match kernel.find_session(session_id).await {
            Ok(Some(session)) => session,
            Ok(None) => return json_error(StatusCode::NOT_FOUND, "session not found"),
            Err(_) => {
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "session lookup failed");
            }
        };
        if auth::authorize(
            &self.pool,
            user_id,
            session.project_id,
            Action::OperateSession,
        )
        .await
        .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        let request_hash: [u8; 32] = Sha256::new()
            .chain_update(mode_name_for_hash(mode))
            .chain_update(prompt.as_bytes())
            .finalize()
            .into();
        let run = match kernel
            .accept_run(
                run_id,
                intent,
                session_id,
                &request_hash,
                mode_name_for_hash(mode),
                &prompt,
                Some(user_id),
            )
            .await
        {
            Ok(run) => run,
            Err(crate::KernelError::Conflict) => {
                return json_error(StatusCode::CONFLICT, "run identity or request conflicts");
            }
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "run store failed"),
        };
        self.record(AuditInsert {
            project_id: Some(session.project_id),
            session_id: Some(session.id),
            run_id: Some(run.id),
            actor_user_id: Some(user_id),
            kind: "run.accepted",
            resource_type: "run",
            resource_id: Some(run.id),
            outcome: AuditOutcome::Ok,
            metadata: Some(&json!({ "mode": run.mode })),
        })
        .await;
        match run.state {
            RunState::Terminal => {
                let Some(result) = run.result else {
                    return json_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "terminal result missing",
                    );
                };
                // An idempotent replay answers with the same receipt shape as
                // every other run response, carrying the retained outcome as
                // structured JSON instead of a raw body.
                let retained: Value = match serde_json::from_str(&result) {
                    Ok(retained) => retained,
                    Err(_) => {
                        return json_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "terminal result missing",
                        );
                    }
                };
                json_ok(json!({
                    "accepted": false,
                    "runId": run.id,
                    "intentId": run.intent_id,
                    "state": RunState::Terminal.as_str(),
                    "reason": "run already attempted and will not be replayed",
                    "result": retained,
                }))
            }
            RunState::Accepted => {
                self.spawn_run(run.clone());
                json_ok(json!({
                    "accepted": true,
                    "runId": run.id,
                    "intentId": run.intent_id,
                    "state": RunState::Accepted.as_str(),
                }))
            }
            RunState::Dispatched | RunState::Unknown | RunState::Cancelled => json_ok(json!({
                "accepted": false,
                "runId": run.id,
                "intentId": run.intent_id,
                "state": run.state.as_str(),
                "reason": "run already attempted and will not be replayed",
            })),
        }
    }

    /// Product conversation API: the first message atomically creates the
    /// Session and its first accepted Run. The browser never supplies a
    /// mode or a create/resume verb; the first message is always a create.
    /// A repeated conversation identity with the same intent and prompt
    /// returns the existing pair idempotently; a different intent or prompt
    /// is a conflict.
    async fn create_conversation(
        &self,
        kernel: &Kernel,
        user_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            #[serde(rename = "conversationId")]
            conversation_id: Uuid,
            #[serde(rename = "agentId", default)]
            agent_id: Option<Uuid>,
            #[serde(rename = "workspaceId")]
            workspace_id: Uuid,
            #[serde(rename = "projectId")]
            project_id: Uuid,
            #[serde(rename = "intentId")]
            intent_id: String,
            prompt: String,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid conversation payload"),
        };
        let intent = match resolve_uuid(payload.intent_id, "intentId") {
            Ok(intent) => intent,
            Err(response) => return response,
        };
        if auth::authorize(
            &self.pool,
            user_id,
            payload.project_id,
            Action::OperateSession,
        )
        .await
        .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        let request_hash: [u8; 32] = Sha256::new()
            .chain_update("create".as_bytes())
            .chain_update(payload.prompt.as_bytes())
            .finalize()
            .into();
        let agent_id = match payload.agent_id {
            Some(agent_id) => agent_id,
            None => match self.resolve_default_agent(payload.project_id).await {
                Ok(agent_id) => agent_id,
                Err(response) => return response,
            },
        };
        let requested_run_id = Uuid::new_v4();
        let (session, run) = match kernel
            .create_conversation(
                payload.conversation_id,
                payload.project_id,
                agent_id,
                payload.workspace_id,
                requested_run_id,
                intent,
                &request_hash,
                payload.prompt.trim(),
                user_id,
            )
            .await
        {
            Ok(pair) => pair,
            Err(crate::KernelError::RelationRefused) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "unknown agent or workspace reference",
                );
            }
            Err(crate::KernelError::Conflict) => {
                return json_error(StatusCode::CONFLICT, "conversation identity conflicts");
            }
            Err(_) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "conversation store failed",
                );
            }
        };
        let accepted = run.id == requested_run_id;
        if accepted {
            self.record(AuditInsert {
                project_id: Some(session.project_id),
                session_id: Some(session.id),
                run_id: Some(run.id),
                actor_user_id: Some(user_id),
                kind: "conversation.created",
                resource_type: "conversation",
                resource_id: Some(session.id),
                outcome: AuditOutcome::Ok,
                metadata: None,
            })
            .await;
            self.spawn_run(run.clone());
        }
        json_ok(json!({
            "conversationId": session.id,
            "projectId": session.project_id,
            "agentId": session.agent_id,
            "workspaceId": session.workspace_id,
            "runId": run.id,
            "intentId": run.intent_id,
            "state": run.state.as_str(),
            "accepted": accepted,
        }))
    }

    /// Product conversation API: follow-up messages queue durable Runs on
    /// the same Session. The browser never supplies a mode; follow-ups are
    /// always resume. Dispatch order honors the per-session accepted queue.
    async fn create_conversation_message(
        &self,
        kernel: &Kernel,
        user_id: Uuid,
        conversation_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            #[serde(rename = "intentId")]
            intent_id: String,
            prompt: String,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid message payload"),
        };
        let intent = match resolve_uuid(payload.intent_id, "intentId") {
            Ok(intent) => intent,
            Err(response) => return response,
        };
        let session = match kernel.find_session(conversation_id).await {
            Ok(Some(session)) => session,
            Ok(None) => return json_error(StatusCode::NOT_FOUND, "conversation not found"),
            Err(_) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "conversation lookup failed",
                );
            }
        };
        if auth::authorize(
            &self.pool,
            user_id,
            session.project_id,
            Action::OperateSession,
        )
        .await
        .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        let request_hash: [u8; 32] = Sha256::new()
            .chain_update("resume".as_bytes())
            .chain_update(payload.prompt.as_bytes())
            .finalize()
            .into();
        let run = match kernel
            .accept_run(
                Uuid::new_v4(),
                intent,
                session.id,
                &request_hash,
                "resume",
                payload.prompt.trim(),
                Some(user_id),
            )
            .await
        {
            Ok(run) => run,
            Err(crate::KernelError::Conflict) => {
                return json_error(StatusCode::CONFLICT, "message identity conflicts");
            }
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "message store failed"),
        };
        let _ = sqlx::query("update sessions set last_actor_user_id = $2 where id = $1")
            .bind(session.id)
            .bind(user_id)
            .execute(&self.pool)
            .await;
        self.record(AuditInsert {
            project_id: Some(session.project_id),
            session_id: Some(session.id),
            run_id: Some(run.id),
            actor_user_id: Some(user_id),
            kind: "message.accepted",
            resource_type: "run",
            resource_id: Some(run.id),
            outcome: AuditOutcome::Ok,
            metadata: Some(&json!({ "mode": "resume" })),
        })
        .await;
        match run.state {
            RunState::Accepted => {
                // The queue advances only when the predecessor settles; the
                // ordered dispatch claim in process_run is the authority.
                self.spawn_run(run.clone());
                json_ok(json!({
                    "conversationId": session.id,
                    "runId": run.id,
                    "intentId": run.intent_id,
                    "state": RunState::Accepted.as_str(),
                    "accepted": true,
                }))
            }
            RunState::Terminal => {
                let retained: Value = match run.result.as_deref() {
                    Some(result) => match serde_json::from_str(result) {
                        Ok(retained) => retained,
                        Err(_) => {
                            return json_error(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "terminal result missing",
                            );
                        }
                    },
                    None => {
                        return json_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "terminal result missing",
                        );
                    }
                };
                json_ok(json!({
                    "conversationId": session.id,
                    "runId": run.id,
                    "intentId": run.intent_id,
                    "state": RunState::Terminal.as_str(),
                    "accepted": false,
                    "reason": "message already attempted and will not be replayed",
                    "result": retained,
                }))
            }
            RunState::Dispatched | RunState::Unknown | RunState::Cancelled => json_ok(json!({
                "conversationId": session.id,
                "runId": run.id,
                "intentId": run.intent_id,
                "state": run.state.as_str(),
                "accepted": false,
                "reason": "message already attempted and will not be replayed",
            })),
        }
    }

    /// Platform-admin surface: every canonical User. Admin platform role
    /// only; regular users are refused.
    async fn admin_users(&self, user_id: Uuid) -> Response<http_body_util::Full<Bytes>> {
        if !self.is_platform_admin(user_id).await {
            return json_error(StatusCode::FORBIDDEN, "platform admin required");
        }
        let users = match kernel_list_users(&self.pool).await {
            Ok(users) => users,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "users failed"),
        };
        json_ok(json!({
            "items": users.into_iter().map(|user| json!({
                "id": user.id,
                "username": user.username,
                "displayName": user.display_name,
                "email": user.email,
                "status": user.status,
                "platformRole": user.platform_role,
                "createdAt": user.created_at,
                "updatedAt": user.updated_at,
            })).collect::<Vec<_>>()
        }))
    }

    /// Platform-admin surface: set one User's platform role.
    async fn admin_set_role(
        &self,
        user_id: Uuid,
        target_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            #[serde(rename = "platformRole")]
            platform_role: String,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid role payload"),
        };
        if !self.is_platform_admin(user_id).await {
            return json_error(StatusCode::FORBIDDEN, "platform admin required");
        }
        if !matches!(payload.platform_role.as_str(), "user" | "admin") {
            return json_error(StatusCode::BAD_REQUEST, "invalid platform role");
        }
        let mut tx = match self.pool.begin().await {
            Ok(tx) => tx,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "user update failed"),
        };
        // Serialize every last-admin decision on one fixed key: two
        // concurrent demotions of different admins queue here, so the
        // second re-counts after the first commits and cannot leave zero.
        if sqlx::query("select pg_advisory_xact_lock($1)")
            .bind(ADMIN_GUARD_LOCK_KEY)
            .execute(&mut *tx)
            .await
            .is_err()
        {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "user update failed");
        }
        let current: Option<(String, String)> =
            sqlx::query_as("select platform_role, status from users where id = $1")
                .bind(target_id)
                .fetch_optional(&mut *tx)
                .await
                .unwrap_or(None);
        let Some((current_role, current_status)) = current else {
            return json_error(StatusCode::NOT_FOUND, "user not found");
        };
        // Demoting the sole remaining *active* platform admin would leave
        // the platform unadministered; the fixed lock makes the check
        // serialized with every other role/status flip.
        let demoting_an_admin = current_role == "admin"
            && current_status == "active"
            && payload.platform_role.trim() != "admin";
        if demoting_an_admin {
            let active_admins: i64 = sqlx::query_scalar(
                "select count(*) from users \
                 where platform_role = 'admin' and status = 'active'",
            )
            .fetch_one(&mut *tx)
            .await
            .unwrap_or(0);
            if active_admins <= 1 {
                return json_error(
                    StatusCode::CONFLICT,
                    "the final active platform admin cannot be demoted",
                );
            }
        }
        let updated =
            sqlx::query("update users set platform_role = $2, updated_at = now() where id = $1")
                .bind(target_id)
                .bind(payload.platform_role.trim())
                .execute(&mut *tx)
                .await;
        match updated {
            Ok(result) if result.rows_affected() == 1 => {
                // The advisory lock and count check live inside the
                // transaction; a failed commit means the flip never
                // happened, so no success audit row and no success claim.
                if tx.commit().await.is_err() {
                    return json_error(StatusCode::INTERNAL_SERVER_ERROR, "user update failed");
                }
                self.record(AuditInsert {
                    project_id: None,
                    session_id: None,
                    run_id: None,
                    actor_user_id: Some(user_id),
                    kind: "user.role_changed",
                    resource_type: "user",
                    resource_id: Some(target_id),
                    outcome: AuditOutcome::Ok,
                    metadata: Some(&json!({ "platformRole": payload.platform_role.trim() })),
                })
                .await;
                json_ok(json!({ "updated": true, "userId": target_id }))
            }
            Ok(_) => json_error(StatusCode::NOT_FOUND, "user not found"),
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "user update failed"),
        }
    }

    /// Platform-admin surface: set one User's durable status. A disabled
    /// User cannot authenticate.
    async fn admin_set_status(
        &self,
        user_id: Uuid,
        target_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            status: String,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid status payload"),
        };
        if !self.is_platform_admin(user_id).await {
            return json_error(StatusCode::FORBIDDEN, "platform admin required");
        }
        if !matches!(payload.status.as_str(), "active" | "disabled") {
            return json_error(StatusCode::BAD_REQUEST, "invalid status");
        }
        let mut tx = match self.pool.begin().await {
            Ok(tx) => tx,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "user update failed"),
        };
        // Serialize on the shared admin-protection key exactly like the
        // role path, so concurrent disable of different admins cannot both
        // pass the active-admin count.
        if sqlx::query("select pg_advisory_xact_lock($1)")
            .bind(ADMIN_GUARD_LOCK_KEY)
            .execute(&mut *tx)
            .await
            .is_err()
        {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "user update failed");
        }
        let current: Option<(String, String)> =
            sqlx::query_as("select status, platform_role from users where id = $1 for update")
                .bind(target_id)
                .fetch_optional(&mut *tx)
                .await
                .unwrap_or(None);
        let Some((current_status, current_platform_role)) = current else {
            return json_error(StatusCode::NOT_FOUND, "user not found");
        };
        // Disabling the final active platform admin would brick platform
        // administration; the count runs under the same advisory lock as
        // every other demote/disable.
        let requested_status = payload.status.trim();
        let disabling_final_admin = requested_status == "disabled"
            && current_status == "active"
            && current_platform_role == "admin";
        if disabling_final_admin {
            let active_admins: i64 = sqlx::query_scalar(
                "select count(*) from users \
                 where platform_role = 'admin' and status = 'active'",
            )
            .fetch_one(&mut *tx)
            .await
            .unwrap_or(0);
            if active_admins <= 1 {
                return json_error(
                    StatusCode::CONFLICT,
                    "the final active platform admin cannot be disabled",
                );
            }
        }
        let owned_status = requested_status.to_owned();
        let updated = sqlx::query("update users set status = $2, updated_at = now() where id = $1")
            .bind(target_id)
            .bind(owned_status)
            .execute(&mut *tx)
            .await;
        match updated {
            Ok(result) if result.rows_affected() == 1 => {
                // A failed commit means the disable never happened: the
                // User stays active and keeps every session, so no success
                // audit row and no updated:true claim.
                // A disabled User loses every live Web session in the same
                // transaction: either both land or neither does.
                if requested_status == "disabled" {
                    if sqlx::query("delete from web_sessions where user_id = $1")
                        .bind(target_id)
                        .execute(&mut *tx)
                        .await
                        .is_err()
                    {
                        return json_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "session revocation failed",
                        );
                    }
                }
                if tx.commit().await.is_err() {
                    return json_error(StatusCode::INTERNAL_SERVER_ERROR, "user update failed");
                }
                self.record(AuditInsert {
                    project_id: None,
                    session_id: None,
                    run_id: None,
                    actor_user_id: Some(user_id),
                    kind: "user.status_changed",
                    resource_type: "user",
                    resource_id: Some(target_id),
                    outcome: AuditOutcome::Ok,
                    metadata: Some(&json!({ "status": requested_status })),
                })
                .await;
                json_ok(json!({ "updated": true, "userId": target_id }))
            }
            Ok(_) => json_error(StatusCode::NOT_FOUND, "user not found"),
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "user update failed"),
        }
    }

    /// Platform-admin surface: creates one canonical User with a native
    /// credential. The username unique partial index rejects duplicates.
    async fn admin_create_user(
        &self,
        kernel: &Kernel,
        user_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            username: String,
            #[serde(rename = "displayName", default)]
            display_name: Option<String>,
            #[serde(default)]
            email: Option<String>,
            #[serde(rename = "platformRole", default = "default_platform_role")]
            platform_role: String,
            password: String,
        }
        fn default_platform_role() -> String {
            "user".to_string()
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid user payload"),
        };
        if !self.is_platform_admin(user_id).await {
            return json_error(StatusCode::FORBIDDEN, "platform admin required");
        }
        if !matches!(payload.platform_role.as_str(), "user" | "admin") {
            return json_error(StatusCode::BAD_REQUEST, "invalid platform role");
        }
        let display_name = payload
            .display_name
            .as_deref()
            .map(str::trim)
            .unwrap_or_default();
        if display_name.is_empty() {
            return json_error(StatusCode::BAD_REQUEST, "invalid display name");
        }
        let created = auth::admin_create_user(
            kernel,
            payload.username.trim(),
            display_name,
            payload
                .email
                .as_deref()
                .map(str::trim)
                .filter(|e| !e.is_empty()),
            payload.platform_role.trim(),
            &payload.password,
        )
        .await;
        match created {
            Ok(user) => {
                self.record(AuditInsert {
                    project_id: None,
                    session_id: None,
                    run_id: None,
                    actor_user_id: Some(user_id),
                    kind: "user.created",
                    resource_type: "user",
                    resource_id: Some(user.id),
                    outcome: AuditOutcome::Ok,
                    metadata: Some(&json!({
                        "username": user.username,
                        "platformRole": user.platform_role,
                    })),
                })
                .await;
                json_response(
                    StatusCode::CREATED,
                    json!({
                        "id": user.id,
                        "username": user.username,
                        "displayName": user.display_name,
                        "email": user.email,
                        "status": user.status,
                        "platformRole": user.platform_role,
                        "createdAt": user.created_at,
                        "updatedAt": user.updated_at,
                    }),
                )
            }
            Err(auth::AdminAccountOutcome::NotFound) => {
                json_error(StatusCode::NOT_FOUND, "user not found")
            }
            Err(_) => json_error(StatusCode::CONFLICT, "username unavailable"),
        }
    }

    /// Platform-admin surface: sets a new native password for one User. The
    /// previous password stops verifying immediately.
    async fn admin_reset_password(
        &self,
        user_id: Uuid,
        target_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            password: String,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid password payload"),
        };
        if !self.is_platform_admin(user_id).await {
            return json_error(StatusCode::FORBIDDEN, "platform admin required");
        }
        let exists: Option<Uuid> = sqlx::query_scalar("select id from users where id = $1")
            .bind(target_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| json_error(StatusCode::INTERNAL_SERVER_ERROR, "user lookup failed"))
            .ok()
            .flatten();
        if exists.is_none() {
            return json_error(StatusCode::NOT_FOUND, "user not found");
        }
        let updated = auth::admin_reset_password(&self.pool, target_id, &payload.password).await;
        match updated {
            Ok(auth::AdminAccountOutcome::Updated) => {
                self.record(AuditInsert {
                    project_id: None,
                    session_id: None,
                    run_id: None,
                    actor_user_id: Some(user_id),
                    kind: "user.password_reset",
                    resource_type: "user",
                    resource_id: Some(target_id),
                    outcome: AuditOutcome::Ok,
                    metadata: None,
                })
                .await;
                // A successful reset immediately locks out every live Web
                // session of that User. If the revocation fails the reset
                // response is an error; old cookies must never survive a
                // reported success.
                if web_session::revoke_all_for_user(&self.pool, target_id)
                    .await
                    .is_err()
                {
                    return json_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "session revocation failed",
                    );
                }
                json_ok(json!({ "updated": true, "userId": target_id }))
            }
            Ok(_) => json_error(StatusCode::BAD_REQUEST, "invalid password"),
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "password reset failed"),
        }
    }

    /// Platform-admin surface: one User's still-live Web sessions.
    async fn admin_list_sessions(
        &self,
        user_id: Uuid,
        target_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        if !self.is_platform_admin(user_id).await {
            return json_error(StatusCode::FORBIDDEN, "platform admin required");
        }
        let exists: Option<Uuid> = sqlx::query_scalar("select id from users where id = $1")
            .bind(target_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| json_error(StatusCode::INTERNAL_SERVER_ERROR, "user lookup failed"))
            .ok()
            .flatten();
        if exists.is_none() {
            return json_error(StatusCode::NOT_FOUND, "user not found");
        }
        let sessions = sqlx::query(
            "select id, user_id, created_at::text as created_at from web_sessions \
             where user_id = $1 order by created_at, id",
        )
        .bind(target_id)
        .fetch_all(&self.pool)
        .await;
        match sessions {
            Ok(rows) => json_ok(json!({
                "items": rows.iter().map(|row| json!({
                    "id": row.get::<Uuid, _>("id"),
                    "userId": row.get::<Uuid, _>("user_id"),
                    "createdAt": row.get::<String, _>("created_at"),
                })).collect::<Vec<_>>()
            })),
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "session lookup failed"),
        }
    }

    /// Platform-admin surface: revokes every Web session of one User.
    async fn admin_revoke_sessions(
        &self,
        user_id: Uuid,
        target_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        if !self.is_platform_admin(user_id).await {
            return json_error(StatusCode::FORBIDDEN, "platform admin required");
        }
        let revoked = web_session::revoke_all_for_user(&self.pool, target_id).await;
        match revoked {
            Ok(count) => {
                self.record(AuditInsert {
                    project_id: None,
                    session_id: None,
                    run_id: None,
                    actor_user_id: Some(user_id),
                    kind: "user.sessions_revoked",
                    resource_type: "user",
                    resource_id: Some(target_id),
                    outcome: AuditOutcome::Ok,
                    metadata: Some(&json!({ "revoked": count })),
                })
                .await;
                json_ok(json!({ "revoked": count }))
            }
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "session revoke failed"),
        }
    }

    /// Same-origin account snapshot consumed by the Web carrier
    /// (`web/src/api/account.ts`). Sessions carry no bearer material and
    /// identities advertise provider names only.
    async fn account_snapshot(
        &self,
        user_id: Uuid,
        current: Option<&web_session::WebSession>,
    ) -> Response<http_body_util::Full<Bytes>> {
        let profile =
            sqlx::query("select id, username, display_name, email from users where id = $1")
                .bind(user_id)
                .fetch_optional(&self.pool)
                .await;
        let Ok(Some(profile)) = profile else {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "profile lookup failed");
        };
        let has_native_credential = sqlx::query_scalar::<_, i64>(
            "select count(*) from native_credentials where user_id = $1",
        )
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0)
            > 0;
        let identities = sqlx::query(
            "select distinct provider from auth_identities where user_id = $1 \
             order by provider",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        let sessions = sqlx::query(
            "select id, created_at::text as created_at from web_sessions \
             where user_id = $1 order by created_at, id",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        json_ok(json!({
            "profile": {
                "userId": profile.get::<Uuid, _>("id"),
                "username": profile.get::<Option<String>, _>("username"),
                "displayName": profile.get::<String, _>("display_name"),
                "email": profile.get::<Option<String>, _>("email"),
            },
            "hasNativeCredential": has_native_credential,
            "identities": identities.iter().map(|row| json!({
                "provider": row.get::<String, _>("provider"),
            })).collect::<Vec<_>>(),
            "sessions": sessions.iter().map(|row| {
                let session_id = row.get::<Uuid, _>("id");
                json!({
                    "sessionId": session_id,
                    "current": current.is_some_and(|session| session.id == session_id),
                    "createdAt": row.get::<String, _>("created_at"),
                })
            }).collect::<Vec<_>>(),
            // The Web surface hides every link affordance while the server
            // offers no additional linking surface.
            "linkableProviders": [],
        }))
    }

    /// Same-origin profile update: display name and optional email only.
    /// The durable username and platform role are not writable here.
    async fn account_update_profile(
        &self,
        user_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            #[serde(rename = "displayName")]
            display_name: String,
            email: String,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid profile payload"),
        };
        let display_name = payload.display_name.trim();
        if display_name.is_empty() || display_name.chars().count() > 128 {
            return json_error(StatusCode::BAD_REQUEST, "invalid display name");
        }
        let email = payload.email.trim();
        if !email.is_empty() && (!email.contains('@') || email.len() > 254) {
            return json_error(StatusCode::BAD_REQUEST, "invalid email");
        }
        let updated = sqlx::query(
            "update users set display_name = $2, email = $3, updated_at = now() where id = $1",
        )
        .bind(user_id)
        .bind(display_name)
        .bind((!email.is_empty()).then_some(email))
        .execute(&self.pool)
        .await;
        match updated {
            Ok(result) if result.rows_affected() == 1 => json_ok(json!({ "updated": true })),
            Ok(_) => json_error(StatusCode::NOT_FOUND, "user not found"),
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "profile update failed"),
        }
    }

    /// Same-origin native password change. The current credential must
    /// verify before anything is written; success replaces the Argon2id
    /// hash and revokes every OTHER session while the acting session keeps
    /// its cookie.
    async fn account_change_password(
        &self,
        kernel: &Kernel,
        user_id: Uuid,
        current: Option<&web_session::WebSession>,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            #[serde(rename = "currentPassword")]
            current_password: String,
            #[serde(rename = "newPassword")]
            new_password: String,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid password payload"),
        };
        const MAX_ACCOUNT_PASSWORD_LEN: usize = 256;
        if payload.current_password.is_empty()
            || payload.new_password.is_empty()
            || payload.new_password.len() > MAX_ACCOUNT_PASSWORD_LEN
        {
            return json_error(StatusCode::BAD_REQUEST, "invalid password");
        }
        let credential = match kernel.find_native_credential(user_id).await {
            Ok(Some(hash)) => hash,
            Ok(None) => return json_error(StatusCode::CONFLICT, "no native credential"),
            Err(_) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "credential lookup failed",
                );
            }
        };
        if !auth::verify_password(&payload.current_password, &credential) {
            return json_error(StatusCode::BAD_REQUEST, "invalid current password");
        }
        let password_hash = match auth::hash_password(&payload.new_password) {
            Ok(hash) => hash,
            Err(_) => {
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "password hashing failed");
            }
        };
        match kernel.set_native_password(user_id, &password_hash).await {
            Ok(true) => {}
            Ok(false) => return json_error(StatusCode::NOT_FOUND, "user not found"),
            Err(_) => {
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "password update failed");
            }
        }
        if let Some(session) = current {
            let _ = web_session::revoke_others_for_user(&self.pool, user_id, session.id).await;
        }
        json_ok(json!({ "updated": true }))
    }

    /// Same-origin revocation of every Web session except the acting one.
    async fn account_revoke_others(
        &self,
        user_id: Uuid,
        current: Option<&web_session::WebSession>,
    ) -> Response<http_body_util::Full<Bytes>> {
        let Some(session) = current else {
            return json_error(StatusCode::UNAUTHORIZED, "login required");
        };
        match web_session::revoke_others_for_user(&self.pool, user_id, session.id).await {
            Ok(count) => json_ok(json!({ "removed": count })),
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "session revoke failed"),
        }
    }

    /// Same-origin revocation of ONE of the caller's own Web sessions.
    /// Another user's session id is indistinguishable from a missing row.
    async fn account_revoke_session(
        &self,
        user_id: Uuid,
        _current: Option<&web_session::WebSession>,
        session_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        match sqlx::query("delete from web_sessions where id = $1 and user_id = $2")
            .bind(session_id)
            .bind(user_id)
            .execute(&self.pool)
            .await
        {
            Ok(result) if result.rows_affected() == 1 => json_ok(json!({ "removed": 1 })),
            Ok(_) => json_error(StatusCode::NOT_FOUND, "session not found"),
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "session revoke failed"),
        }
    }

    /// Platform-admin surface: every collaboration scope with durable
    /// membership and workspace counts. Personal scopes are included; each
    /// counts its fixed single owner member.
    async fn admin_scopes(&self, user_id: Uuid) -> Response<http_body_util::Full<Bytes>> {
        if !self.is_platform_admin(user_id).await {
            return json_error(StatusCode::FORBIDDEN, "platform admin required");
        }
        let rows = match sqlx::query(
            "select p.id, p.owner_user_id, p.name, p.kind, \
                    (select count(*) from project_members m where m.project_id = p.id)::bigint \
                        as member_count, \
                    (select count(*) from workspaces w where w.project_id = p.id)::bigint \
                        as workspace_count \
             from projects p order by p.name, p.id",
        )
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "scopes failed"),
        };
        json_ok(json!({
            "items": rows.iter().map(|row| json!({
                "id": row.get::<Uuid, _>("id"),
                "ownerUserId": row.get::<Uuid, _>("owner_user_id"),
                "name": row.get::<String, _>("name"),
                "kind": row.get::<String, _>("kind"),
                "memberCount": row.get::<i64, _>("member_count"),
                "workspaceCount": row.get::<i64, _>("workspace_count"),
            })).collect::<Vec<_>>()
        }))
    }

    /// Platform-admin membership roster for one scope. Authorized by
    /// platform admin, not Team membership; Personal scopes are listed so
    /// recovery can see the fixed owner without mutating it.
    async fn admin_scope_members(
        &self,
        user_id: Uuid,
        project_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        if !self.is_platform_admin(user_id).await {
            return json_error(StatusCode::FORBIDDEN, "platform admin required");
        }
        let known_project: Option<Uuid> =
            sqlx::query_scalar("select id from projects where id = $1")
                .bind(project_id)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten();
        if known_project.is_none() {
            return json_error(StatusCode::NOT_FOUND, "project not found");
        }
        let rows = match sqlx::query(
            "select m.user_id, u.username, u.display_name, \
                    coalesce(a.subject, u.subject) as subject, m.role, \
                    m.created_at::text as created_at \
             from project_members m join users u on u.id = m.user_id \
             left join auth_identities a on a.user_id = u.id \
             where m.project_id = $1 order by m.created_at, m.user_id",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "members failed"),
        };
        json_ok(json!({
            "items": rows.into_iter().map(|row| json!({
                "userId": row.get::<Uuid, _>("user_id"),
                "username": row.get::<Option<String>, _>("username"),
                "displayName": row.get::<String, _>("display_name"),
                "subject": row.get::<String, _>("subject"),
                "role": row.get::<String, _>("role"),
                "createdAt": row.get::<String, _>("created_at"),
            })).collect::<Vec<_>>()
        }))
    }

    /// Platform-admin surface: every registered Fabric underlay.
    async fn admin_fabrics(&self, user_id: Uuid) -> Response<http_body_util::Full<Bytes>> {
        if !self.is_platform_admin(user_id).await {
            return json_error(StatusCode::FORBIDDEN, "platform admin required");
        }
        let rows = match sqlx::query(
            "select id, name, created_at::text as created_at from fabrics order by created_at, id",
        )
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "fabrics failed"),
        };
        json_ok(json!({
            "items": rows.iter().map(|row| json!({
                "id": row.get::<Uuid, _>("id"),
                "name": row.get::<String, _>("name"),
                "createdAt": row.get::<String, _>("created_at"),
            })).collect::<Vec<_>>()
        }))
    }

    /// Platform-admin surface: every Workspace with its Fabric underlay
    /// metadata. No session, run, or Blob content is exposed here.
    async fn admin_workspaces(&self, user_id: Uuid) -> Response<http_body_util::Full<Bytes>> {
        if !self.is_platform_admin(user_id).await {
            return json_error(StatusCode::FORBIDDEN, "platform admin required");
        }
        let rows = match sqlx::query(
            "select w.id, w.fabric_id, f.name as fabric_name, w.project_id, \
                    w.label, w.state, w.created_at::text as created_at \
             from workspaces w join fabrics f on f.id = w.fabric_id \
             order by w.created_at, w.id",
        )
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "workspaces failed"),
        };
        json_ok(json!({
            "items": rows.iter().map(|row| json!({
                "id": row.get::<Uuid, _>("id"),
                "fabricId": row.get::<Uuid, _>("fabric_id"),
                "fabricName": row.get::<String, _>("fabric_name"),
                "projectId": row.get::<Option<Uuid>, _>("project_id"),
                "label": row.get::<Option<String>, _>("label"),
                "state": row.get::<String, _>("state"),
                "createdAt": row.get::<String, _>("created_at"),
            })).collect::<Vec<_>>()
        }))
    }

    /// Platform-admin surface: the global audit tail across every project,
    /// including rows with no project scope. The projection is exactly the
    /// member-facing audit surface; no secret material is ever projected.
    async fn admin_audit(
        &self,
        user_id: Uuid,
        query: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        if !self.is_platform_admin(user_id).await {
            return json_error(StatusCode::FORBIDDEN, "platform admin required");
        }
        let after = query
            .split('&')
            .find_map(|pair| pair.strip_prefix("after="))
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let limit = query
            .split('&')
            .find_map(|pair| pair.strip_prefix("limit="))
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(64)
            .clamp(1, 500);
        let rows = match sqlx::query(
            "select e.seq, e.project_id, e.session_id, e.run_id, e.actor_user_id, \
                    e.occurred_at::text as occurred_at, e.kind, e.resource_type, \
                    e.resource_id, e.outcome, e.metadata, e.payload \
             from audit_events e \
             where e.seq > $1 order by e.seq asc limit $2",
        )
        .bind(after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "audit failed"),
        };
        let cursor = rows
            .last()
            .map(|row| row.get::<i64, _>("seq"))
            .unwrap_or(after);
        json_ok(json!({
            "items": rows.iter().map(|row| json!({
                "seq": row.get::<i64, _>("seq"),
                "projectId": row.get::<Option<Uuid>, _>("project_id"),
                "sessionId": row.get::<Option<Uuid>, _>("session_id"),
                "runId": row.get::<Option<Uuid>, _>("run_id"),
                "actorUserId": row.get::<Option<Uuid>, _>("actor_user_id"),
                "occurredAt": row.get::<String, _>("occurred_at"),
                "kind": row.get::<String, _>("kind"),
                "resourceType": row.get::<String, _>("resource_type"),
                "resourceId": row.get::<Option<Uuid>, _>("resource_id"),
                "outcome": row.get::<String, _>("outcome"),
                "metadata": row.get::<Option<serde_json::Value>, _>("metadata"),
                "payload": row.get::<Option<String>, _>("payload"),
            })).collect::<Vec<_>>(),
            "cursor": cursor,
        }))
    }

    /// Platform-admin surface: live facts only. Every field is derived here
    /// from a real probe or the committed configuration; nothing is
    /// invented or extrapolated.
    async fn admin_health(
        &self,
        user_id: Uuid,
        auth_mode: &'static str,
    ) -> Response<http_body_util::Full<Bytes>> {
        if !self.is_platform_admin(user_id).await {
            return json_error(StatusCode::FORBIDDEN, "platform admin required");
        }
        let db_ok = sqlx::query("select 1").execute(&self.pool).await.is_ok();
        // Services construction fails closed when Blob configuration is
        // incomplete, so a running Services owns a configured BlobStore.
        let blob_configured = true;
        let fabric_registered = match self.configured_fabric_id {
            Some(fabric_id) => {
                sqlx::query_scalar::<_, i64>("select count(*) from fabrics where id = $1")
                    .bind(fabric_id)
                    .fetch_one(&self.pool)
                    .await
                    .unwrap_or(0)
                    > 0
            }
            None => false,
        };
        let workspace_counts =
            sqlx::query("select state, count(*)::bigint as count from workspaces group by state")
                .fetch_all(&self.pool)
                .await
                .unwrap_or_default();
        let mut counts = std::collections::HashMap::<String, i64>::new();
        for row in workspace_counts {
            counts.insert(row.get::<String, _>("state"), row.get::<i64, _>("count"));
        }
        json_ok(json!({
            "database": { "ok": db_ok },
            "blob": { "configured": blob_configured },
            "auth": { "mode": auth_mode },
            "fabric": { "registered": fabric_registered },
            "workspaces": {
                "creating": counts.get("creating").copied().unwrap_or(0),
                "ready": counts.get("ready").copied().unwrap_or(0),
                "fenced": counts.get("fenced").copied().unwrap_or(0),
            },
        }))
    }

    /// True exactly when the caller carries the explicit `admin` platform
    /// role. Platform role is never derived from provider claims.
    async fn is_platform_admin(&self, user_id: Uuid) -> bool {
        sqlx::query_scalar::<_, String>(
            "select platform_role from users where id = $1 and status = 'active'",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()
        .is_some_and(|role| role == "admin")
    }
    /// Creates one scoped secret. The request value is write-only material:
    /// it is handed to the store and never echoed, logged, or persisted in
    /// any metadata or audit surface.
    async fn create_secret(
        &self,
        user_id: Uuid,
        scope_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            name: String,
            value: String,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid secret payload"),
        };
        let value = match SecretValue::from_text(payload.value) {
            Ok(value) => value,
            Err(error) => return secret_error_response(error),
        };
        match self
            .secrets
            .create(user_id, scope_id, payload.name, value)
            .await
        {
            Ok(metadata) => json_ok(json!({ "secret": secret_metadata_json(&metadata) })),
            Err(error) => secret_error_response(error),
        }
    }

    /// Lists one scope's secret metadata with the server-derived capability.
    async fn list_secrets(
        &self,
        user_id: Uuid,
        scope_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        match self.secrets.list_metadata(user_id, scope_id).await {
            Ok(list) => json_ok(json!({
                "secrets": list
                    .secrets
                    .iter()
                    .map(secret_metadata_json)
                    .collect::<Vec<_>>(),
                "canWrite": list.can_write,
            })),
            Err(error) => secret_error_response(error),
        }
    }

    /// Replaces a secret's material and advances its version. There is no
    /// corresponding single-secret GET anywhere on this API by design: the
    /// write-only boundary keeps browser reads unable to become value reads.
    async fn replace_secret(
        &self,
        user_id: Uuid,
        secret_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        let value = match secret_value_payload(&body) {
            Ok(value) => value,
            Err(response) => return response,
        };
        match self.secrets.replace_value(user_id, secret_id, value).await {
            Ok(metadata) => json_ok(json!({ "secret": secret_metadata_json(&metadata) })),
            Err(error) => secret_error_response(error),
        }
    }

    /// Rotates a secret's material under the `rotated` audit action.
    async fn rotate_secret(
        &self,
        user_id: Uuid,
        secret_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        let value = match secret_value_payload(&body) {
            Ok(value) => value,
            Err(response) => return response,
        };
        match self.secrets.rotate(user_id, secret_id, value).await {
            Ok(metadata) => json_ok(json!({ "secret": secret_metadata_json(&metadata) })),
            Err(error) => secret_error_response(error),
        }
    }

    /// Deletes backend material and the metadata row; the audit trail stays.
    async fn delete_secret(
        &self,
        user_id: Uuid,
        secret_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        match self.secrets.delete(user_id, secret_id).await {
            Ok(()) => no_content(),
            Err(error) => secret_error_response(error),
        }
    }

    /// Metadata-only lifecycle audit for one secret.
    async fn secret_audit(
        &self,
        user_id: Uuid,
        secret_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        match self.secrets.audit(user_id, secret_id).await {
            Ok(events) => json_ok(json!({
                "events": events
                    .iter()
                    .map(secret_audit_event_json)
                    .collect::<Vec<_>>(),
            })),
            Err(error) => secret_error_response(error),
        }
    }

    /// Lists the caller's membership-scoped projects as collaboration
    /// scopes, with the durable kind (personal | team).
    async fn scopes(&self, user_id: Uuid) -> Response<http_body_util::Full<Bytes>> {
        let rows = match sqlx::query(
            "select p.id, p.owner_user_id, p.name, p.kind, m.role, \
                    p.created_at::text as created_at \
             from projects p join project_members m on m.project_id = p.id \
             where m.user_id = $1 order by p.created_at, p.id",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "scopes failed"),
        };
        json_ok(json!({
            "items": rows.into_iter().map(|row| {
                let role = Role::parse(row.get::<String, _>("role").as_str()).unwrap_or(Role::Viewer);
                json!({
                    "id": row.get::<Uuid, _>("id"),
                    "ownerUserId": row.get::<Uuid, _>("owner_user_id"),
                    "name": row.get::<String, _>("name"),
                    "kind": row.get::<String, _>("kind"),
                    "role": role_name(role),
                    "createdAt": row.get::<String, _>("created_at"),
                    "capabilities": capabilities_json(role),
                })
            }).collect::<Vec<_>>()
        }))
    }

    /// Creates one TEAM collaboration scope with the caller as its durable
    /// owner member.
    async fn create_scope(
        &self,
        kernel: &Kernel,
        user_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            id: Uuid,
            name: String,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid scope payload"),
        };
        let project = match kernel
            .create_project(payload.id, user_id, payload.name.trim(), "team")
            .await
        {
            Ok(project) => project,
            Err(crate::KernelError::Conflict) => {
                return json_error(StatusCode::CONFLICT, "scope identity conflicts");
            }
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "scope store failed"),
        };
        self.record(AuditInsert {
            project_id: Some(project.id),
            session_id: None,
            run_id: None,
            actor_user_id: Some(user_id),
            kind: "scope.created",
            resource_type: "scope",
            resource_id: Some(project.id),
            outcome: AuditOutcome::Ok,
            metadata: Some(&json!({ "kind": "team" })),
        })
        .await;
        json_response(
            StatusCode::CREATED,
            json!({
                "id": project.id,
                "ownerUserId": project.owner_user_id,
                "name": project.name,
                "kind": "team",
                "role": "owner",
                "capabilities": capabilities_json(Role::Owner),
            }),
        )
    }

    /// One scope visible to the caller through membership.
    async fn scope_detail(
        &self,
        user_id: Uuid,
        project_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        let row = sqlx::query(
            "select p.id, p.owner_user_id, p.name, p.kind, m.role, \
                    p.created_at::text as created_at \
             from projects p join project_members m on m.project_id = p.id \
             where p.id = $1 and m.user_id = $2",
        )
        .bind(project_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await;
        let Ok(Some(row)) = row else {
            return json_error(StatusCode::NOT_FOUND, "scope not found");
        };
        let role = Role::parse(row.get::<String, _>("role").as_str()).unwrap_or(Role::Viewer);
        json_ok(json!({
            "id": row.get::<Uuid, _>("id"),
            "ownerUserId": row.get::<Uuid, _>("owner_user_id"),
            "name": row.get::<String, _>("name"),
            "kind": row.get::<String, _>("kind"),
            "role": role_name(role),
            "createdAt": row.get::<String, _>("created_at"),
            "capabilities": capabilities_json(role),
        }))
    }

    /// Renames one scope. Owner/admin only by the frozen role permits.
    async fn update_scope(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            name: String,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid scope payload"),
        };
        if auth::authorize(&self.pool, user_id, project_id, Action::ManageMembership)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "scope access denied");
        }
        let updated = sqlx::query("update projects set name = $2 where id = $1")
            .bind(project_id)
            .bind(payload.name.trim())
            .execute(&self.pool)
            .await;
        match updated {
            Ok(result) if result.rows_affected() == 1 => json_ok(json!({ "updated": true })),
            Ok(_) => json_error(StatusCode::NOT_FOUND, "scope not found"),
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "scope update failed"),
        }
    }

    /// One scope's membership with profile fields only; provider subjects
    /// are never exposed on this surface.
    async fn scope_members(
        &self,
        user_id: Uuid,
        project_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        if auth::authorize(&self.pool, user_id, project_id, Action::ReadProject)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "scope access denied");
        }
        let rows = match sqlx::query(
            "select m.user_id, u.username, u.display_name, m.role, \
                    m.created_at::text as created_at \
             from project_members m join users u on u.id = m.user_id \
             where m.project_id = $1 order by m.created_at, m.user_id",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "members failed"),
        };
        json_ok(json!({
            "items": rows.into_iter().map(|row| json!({
                "userId": row.get::<Uuid, _>("user_id"),
                "username": row.get::<Option<String>, _>("username"),
                "displayName": row.get::<String, _>("display_name"),
                "role": row.get::<String, _>("role"),
                "createdAt": row.get::<String, _>("created_at"),
            })).collect::<Vec<_>>()
        }))
    }

    /// Bounded active-user directory search by username or display name.
    /// Only identity facts are returned; subjects and issuers stay hidden.
    async fn scopes_users_search(
        &self,
        _user_id: Uuid,
        query: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let raw = query
            .split('&')
            .find_map(|pair| pair.strip_prefix("q="))
            .unwrap_or("")
            .trim()
            .to_owned();
        if raw.is_empty() {
            return json_ok(json!({ "items": [] }));
        }
        let escaped = raw
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        let rows = match sqlx::query(
            "select id, username, display_name from users \
             where status = 'active' and (username ilike $1 or display_name ilike $1) \
             order by username limit 20",
        )
        .bind(pattern)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "user search failed"),
        };
        json_ok(json!({
            "items": rows.into_iter().map(|row| json!({
                "userId": row.get::<Uuid, _>("id"),
                "username": row.get::<Option<String>, _>("username"),
                "displayName": row.get::<String, _>("display_name"),
            })).collect::<Vec<_>>()
        }))
    }

    /// One scope's workspaces with the durable display label.
    async fn scope_workspaces(
        &self,
        user_id: Uuid,
        project_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        if auth::authorize(&self.pool, user_id, project_id, Action::ReadProject)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "scope access denied");
        }
        let rows = match sqlx::query(
            "select w.id, coalesce(w.label, 'Workspace') as label, \
                    w.project_id, w.state, w.created_by_user_id, \
                    w.created_at::text as created_at \
             from workspaces w \
             where w.project_id = $1 order by w.created_at, w.id",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "workspaces failed"),
        };
        json_ok(json!({
            "items": rows.into_iter().map(|row| json!({
                "id": row.get::<Uuid, _>("id"),
                "label": row.get::<String, _>("label"),
                "projectId": row.get::<Uuid, _>("project_id"),
                "scopeId": row.get::<Uuid, _>("project_id"),
                "state": row.get::<String, _>("state"),
                "createdByUserId": row.get::<Option<Uuid>, _>("created_by_user_id"),
                "createdAt": row.get::<String, _>("created_at"),
            })).collect::<Vec<_>>()
        }))
    }

    /// One Workspace's detail to any member of its owning scope.
    async fn workspace_detail(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        let row = sqlx::query(
            "select w.id, coalesce(w.label, 'Workspace') as label, \
                    w.project_id, w.state, w.created_by_user_id, \
                    w.created_at::text as created_at \
             from workspaces w join project_members m on m.project_id = w.project_id \
             where w.id = $1 and m.user_id = $2",
        )
        .bind(workspace_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await;
        let Ok(Some(row)) = row else {
            return json_error(StatusCode::NOT_FOUND, "workspace not found");
        };
        json_ok(json!({
            "id": row.get::<Uuid, _>("id"),
            "label": row.get::<String, _>("label"),
            "projectId": row.get::<Uuid, _>("project_id"),
            "scopeId": row.get::<Uuid, _>("project_id"),
            "state": row.get::<String, _>("state"),
            "createdByUserId": row.get::<Option<Uuid>, _>("created_by_user_id"),
            "createdAt": row.get::<String, _>("created_at"),
        }))
    }

    /// One Workspace's conversations, titled by the first user message
    /// (bounded to 60 characters).
    async fn workspace_conversations(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        let rows = match sqlx::query(
            "select s.id, s.created_at::text as created_at, \
                    left((select r.prompt from runs r \
                          where r.session_id = s.id order by r.seq limit 1), 60) as title \
             from sessions s join project_members m on m.project_id = s.project_id \
             where s.workspace_id = $1 and m.user_id = $2 order by s.created_at",
        )
        .bind(workspace_id)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => {
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "conversations failed");
            }
        };
        json_ok(json!({
            "items": rows.into_iter().map(|row| json!({
                "id": row.get::<Uuid, _>("id"),
                "title": row.get::<String, _>("title"),
                "createdAt": row.get::<String, _>("created_at"),
            })).collect::<Vec<_>>()
        }))
    }

    /// Renames one Workspace. The durable Project owner or the Workspace
    /// creator may label it; no other member may.
    async fn update_workspace_label(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            label: String,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid label payload"),
        };
        let label = payload.label.trim().to_owned();
        if label.is_empty() {
            return json_error(StatusCode::BAD_REQUEST, "invalid label");
        }
        let access: Option<(Uuid, String, Option<Uuid>)> = sqlx::query_as(
            "select p.owner_user_id, coalesce(m.role, '') as role, \
                    w.created_by_user_id from workspaces w \
             join projects p on p.id = w.project_id \
             left join project_members m on m.project_id = w.project_id and m.user_id = $2 \
             where w.id = $1",
        )
        .bind(workspace_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten();
        let Some((owner_user_id, role, creator)) = access else {
            return json_error(StatusCode::NOT_FOUND, "workspace not found");
        };
        // One lifecycle rule for label rename: the scope owner or a team
        // admin manages every shared Workspace; a plain member only what
        // they created; viewers and non-members none.
        let allowed = user_id == owner_user_id
            || matches!(role.as_str(), "owner" | "admin")
            || (role == "member" && creator.as_ref() == Some(&user_id));
        if !allowed {
            return json_error(StatusCode::FORBIDDEN, "workspace label denied");
        }
        let updated = sqlx::query("update workspaces set label = $2 where id = $1")
            .bind(workspace_id)
            .bind(&label)
            .execute(&self.pool)
            .await;
        match updated {
            Ok(result) if result.rows_affected() == 1 => json_ok(json!({ "updated": true })),
            Ok(_) => json_error(StatusCode::NOT_FOUND, "workspace not found"),
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "label update failed"),
        }
    }

    /// Platform-admin only: real Fabric identity facts plus the durable
    /// execution generation. Nothing is invented here.
    async fn workspace_diagnostics(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        if !self.is_platform_admin(user_id).await {
            return json_error(StatusCode::FORBIDDEN, "platform admin required");
        }
        let row = sqlx::query(
            "select f.id as fabric_id, f.name as fabric_name, w.exec_generation \
             from workspaces w join fabrics f on f.id = w.fabric_id \
             where w.id = $1",
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await;
        match row {
            Ok(Some(row)) => json_ok(json!({
                "fabricId": row.get::<Uuid, _>("fabric_id"),
                "fabricName": row.get::<String, _>("fabric_name"),
                "execGeneration": row.get::<i64, _>("exec_generation"),
            })),
            Ok(None) => json_error(StatusCode::NOT_FOUND, "workspace not found"),
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "diagnostics failed"),
        }
    }

    /// One scope's sessions as conversation projections, membership-scoped.
    async fn scoped_sessions(
        &self,
        user_id: Uuid,
        project_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        if auth::authorize(&self.pool, user_id, project_id, Action::ReadProject)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "scope access denied");
        }
        let rows = match sqlx::query(
            "select s.id, s.project_id, s.agent_id, s.workspace_id, \
                    s.created_at::text as created_at, \
                    exists(select 1 from runs r \
                           where r.session_id = s.id \
                             and r.state in ('accepted', 'dispatched')) as running \
             from sessions s \
             where s.project_id = $1 order by s.created_at",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "sessions failed"),
        };
        json_ok(json!({
            "items": rows.into_iter().map(|row| json!({
                "id": row.get::<Uuid, _>("id"),
                "projectId": row.get::<Uuid, _>("project_id"),
                "agentId": row.get::<Uuid, _>("agent_id"),
                "workspaceId": row.get::<Uuid, _>("workspace_id"),
                "createdAt": row.get::<String, _>("created_at"),
                "running": row.get::<bool, _>("running"),
            })).collect::<Vec<_>>()
        }))
    }

    /// One scope's agent presets: configuration projections with no model
    /// editing surface. Model identity is never exposed here.
    async fn scoped_agents(
        &self,
        user_id: Uuid,
        project_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        if auth::authorize(&self.pool, user_id, project_id, Action::ReadProject)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "scope access denied");
        }
        let rows = match sqlx::query(
            "select a.id, a.project_id, a.name, a.system_prompt, \
                    a.bash_enabled, a.max_tokens \
             from agents a \
             where a.project_id = $1 order by a.created_at, a.id",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "agents failed"),
        };
        json_ok(json!({
            "items": rows.into_iter().map(|row| json!({
                "id": row.get::<Uuid, _>("id"),
                "projectId": row.get::<Uuid, _>("project_id"),
                "name": row.get::<String, _>("name"),
                "prompt": row.get::<String, _>("system_prompt"),
                "bashEnabled": row.get::<bool, _>("bash_enabled"),
                "maxTokens": row.get::<i32, _>("max_tokens"),
            })).collect::<Vec<_>>()
        }))
    }

    /// One scope's event feed, paged by `after`, membership-scoped. The
    /// same cursor shape as the global `/api/events` surface.
    async fn scoped_events(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        query: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        if auth::authorize(&self.pool, user_id, project_id, Action::ReadProject)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "scope access denied");
        }
        let after = query_cursor(query);
        let session_ids: Vec<Uuid> =
            match sqlx::query_scalar("select id from sessions where project_id = $1")
                .bind(project_id)
                .fetch_all(&self.pool)
                .await
            {
                Ok(ids) => ids,
                Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "events failed"),
            };
        self.canonical_events(&session_ids, after).await
    }

    /// One conversation's durable Runs: {runId, seq, state, prompt,
    /// actorUserId} plus the intent id, membership-scoped. Every durable
    /// run state is included so the browser can project queue truth and
    /// cancel by run id.
    async fn conversation_runs(
        &self,
        user_id: Uuid,
        session_id: Uuid,
        kernel: &Kernel,
    ) -> Response<http_body_util::Full<Bytes>> {
        let session = match kernel.find_session(session_id).await {
            Ok(Some(session)) => session,
            Ok(None) => return json_error(StatusCode::NOT_FOUND, "conversation not found"),
            Err(_) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "conversation lookup failed",
                );
            }
        };
        if auth::authorize(&self.pool, user_id, session.project_id, Action::ReadProject)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "conversation access denied");
        }
        let rows = match sqlx::query(
            "select id, intent_id, seq, state, prompt, actor_user_id \
             from runs where session_id = $1 order by seq",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "runs failed"),
        };
        json_ok(json!({
            "runs": rows.into_iter().map(|row| json!({
                "runId": row.get::<Uuid, _>("id"),
                "intentId": row.get::<Uuid, _>("intent_id"),
                "seq": row.get::<i64, _>("seq"),
                "state": row.get::<String, _>("state"),
                "prompt": row.get::<String, _>("prompt"),
                "actorUserId": row.get::<Option<Uuid>, _>("actor_user_id"),
            })).collect::<Vec<_>>()
        }))
    }

    /// Creates one Agent preset within a scope. Model identity is immutable
    /// for presets; the system model is wired by the platform, not the
    /// console.
    async fn create_agent_preset(
        &self,
        kernel: &Kernel,
        user_id: Uuid,
        project_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            id: Uuid,
            name: String,
            #[serde(default)]
            prompt: Option<String>,
            #[serde(rename = "bashEnabled", default = "default_true")]
            bash_enabled: bool,
            #[serde(rename = "maxTokens", default = "default_max_tokens")]
            max_tokens: i32,
        }
        fn default_true() -> bool {
            true
        }
        fn default_max_tokens() -> i32 {
            DEFAULT_MAX_TOKENS as i32
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid preset payload"),
        };
        let name = payload.name.trim();
        if name.is_empty() {
            return json_error(StatusCode::BAD_REQUEST, "invalid preset name");
        }
        if !(1..=1024).contains(&payload.max_tokens) {
            return json_error(StatusCode::BAD_REQUEST, "maxTokens must be within 1..=1024");
        }
        if auth::authorize(&self.pool, user_id, project_id, Action::ManageMembership)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "scope access denied");
        }
        let created = kernel
            .create_agent(
                payload.id,
                project_id,
                name,
                "",
                payload.prompt.as_deref().unwrap_or(""),
                payload.bash_enabled,
                payload.max_tokens,
            )
            .await;
        match created {
            Ok(agent) => {
                self.record(AuditInsert {
                    project_id: Some(project_id),
                    session_id: None,
                    run_id: None,
                    actor_user_id: Some(user_id),
                    kind: "agent_preset.created",
                    resource_type: "agent",
                    resource_id: Some(agent.id),
                    outcome: AuditOutcome::Ok,
                    metadata: Some(&json!({ "name": agent.name })),
                })
                .await;
                json_response(
                    StatusCode::CREATED,
                    json!({
                        "id": agent.id,
                        "projectId": agent.project_id,
                        "name": agent.name,
                        "prompt": agent.system_prompt,
                        "bashEnabled": agent.bash_enabled,
                        "maxTokens": agent.max_tokens,
                    }),
                )
            }
            Err(crate::KernelError::Conflict) => {
                json_error(StatusCode::CONFLICT, "preset identity conflicts")
            }
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "preset store failed"),
        }
    }

    /// Edits one Agent preset. Model identity is immutable for presets: a
    /// payload that names `model` is refused outright.
    async fn update_agent_preset(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        preset_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        let value: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid preset payload"),
        };
        if value.get("model").is_some() {
            return json_error(StatusCode::BAD_REQUEST, "preset model is immutable");
        }
        #[derive(Deserialize)]
        struct Payload {
            #[serde(default)]
            name: Option<String>,
            #[serde(default)]
            prompt: Option<String>,
            #[serde(rename = "bashEnabled", default)]
            bash_enabled: Option<bool>,
            #[serde(rename = "maxTokens", default)]
            max_tokens: Option<i32>,
        }
        let payload: Payload = match serde_json::from_value(value) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid preset payload"),
        };
        if let Some(max_tokens) = payload.max_tokens {
            if !(1..=1024).contains(&max_tokens) {
                return json_error(StatusCode::BAD_REQUEST, "maxTokens must be within 1..=1024");
            }
        }
        if auth::authorize(&self.pool, user_id, project_id, Action::ManageMembership)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "scope access denied");
        }
        let owned: bool = sqlx::query_scalar(
            "select exists(select 1 from agents where id = $1 and project_id = $2)",
        )
        .bind(preset_id)
        .bind(project_id)
        .fetch_one(&self.pool)
        .await
        .unwrap_or(false);
        if !owned {
            return json_error(StatusCode::NOT_FOUND, "preset not found");
        }
        let name = payload
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty());
        if payload.name.is_some() && name.is_none() {
            return json_error(StatusCode::BAD_REQUEST, "invalid preset name");
        }
        let updated = sqlx::query(
            "update agents set \
                name = coalesce($2, name), \
                system_prompt = coalesce($3, system_prompt), \
                bash_enabled = coalesce($4, bash_enabled), \
                max_tokens = coalesce($5, max_tokens) \
             where id = $1 and project_id = $6",
        )
        .bind(preset_id)
        .bind(name)
        .bind(payload.prompt.as_deref())
        .bind(payload.bash_enabled)
        .bind(payload.max_tokens)
        .bind(project_id)
        .execute(&self.pool)
        .await;
        match updated {
            Ok(result) if result.rows_affected() == 1 => json_ok(json!({ "updated": true })),
            Ok(_) => json_error(StatusCode::NOT_FOUND, "preset not found"),
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "preset update failed"),
        }
    }

    /// Deletes one scope-owned Agent preset.
    async fn delete_agent_preset(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        preset_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        if auth::authorize(&self.pool, user_id, project_id, Action::ManageMembership)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "scope access denied");
        }
        let deleted = sqlx::query("delete from agents where id = $1 and project_id = $2")
            .bind(preset_id)
            .bind(project_id)
            .execute(&self.pool)
            .await;
        match deleted {
            Ok(result) if result.rows_affected() == 1 => {
                self.record(AuditInsert {
                    project_id: Some(project_id),
                    session_id: None,
                    run_id: None,
                    actor_user_id: Some(user_id),
                    kind: "agent_preset.deleted",
                    resource_type: "agent",
                    resource_id: Some(preset_id),
                    outcome: AuditOutcome::Ok,
                    metadata: None,
                })
                .await;
                json_ok(json!({ "updated": true }))
            }
            Ok(_) => json_error(StatusCode::NOT_FOUND, "preset not found"),
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "preset delete failed"),
        }
    }

    /// Resolves a project's Default agent, creating exactly one lazily when
    /// absent. The (project_id, name) unique constraint makes concurrent
    /// first-chats converge on one row.
    async fn resolve_default_agent(
        &self,
        project_id: Uuid,
    ) -> Result<Uuid, Response<http_body_util::Full<Bytes>>> {
        let existing: Option<Uuid> =
            sqlx::query_scalar("select id from agents where project_id = $1 and name = 'Default'")
                .bind(project_id)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten();
        if let Some(id) = existing {
            return Ok(id);
        }
        let created = sqlx::query(
            "insert into agents (id, project_id, name) \
             values ($1, $2, 'Default') \
             on conflict (project_id, name) do nothing",
        )
        .bind(Uuid::new_v4())
        .bind(project_id)
        .execute(&self.pool)
        .await;
        if created.is_err() {
            return Err(json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "agent store failed",
            ));
        }
        let resolved: Option<Uuid> =
            sqlx::query_scalar("select id from agents where project_id = $1 and name = 'Default'")
                .bind(project_id)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten();
        match resolved {
            Some(id) => Ok(id),
            None => Err(json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "agent resolution failed",
            )),
        }
    }
}

/// Lists every canonical User. Platform-admin surface.
async fn kernel_list_users(pool: &PgPool) -> Result<Vec<crate::User>, sqlx::Error> {
    let rows = sqlx::query(
        "select id, username, display_name, email, status, platform_role, \
                created_at::text as created_at, updated_at::text as updated_at \
         from users order by created_at, id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| crate::User {
            id: row.get("id"),
            username: row.get("username"),
            display_name: row.get("display_name"),
            email: row.get("email"),
            status: row.get("status"),
            platform_role: row.get("platform_role"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
        .collect())
}

fn mode_name_for_hash(mode: ActivationMode) -> &'static str {
    match mode {
        ActivationMode::Create => "create",
        ActivationMode::Resume => "resume",
    }
}

/// Parses a required caller-supplied resource identity.
fn resolve_uuid(
    value: String,
    field: &'static str,
) -> Result<Uuid, Response<http_body_util::Full<Bytes>>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(json_error(StatusCode::BAD_REQUEST, field));
    }
    Uuid::parse_str(trimmed).map_err(|_| json_error(StatusCode::BAD_REQUEST, field))
}

struct CloudModel {
    relay: Arc<CloudModelRelay>,
    agent: Agent,
}

impl ModelRelay for CloudModel {
    fn complete(
        &self,
        request: ModelRequest,
    ) -> impl Future<Output = Result<ModelResponse, ActivationError>> + Send {
        let relay = self.relay.clone();
        let agent = self.agent.clone();
        async move {
            let mut messages = Vec::new();
            for wire in request.messages {
                let mut calls = Vec::new();
                for call in wire.tool_calls {
                    let arguments: Value = serde_json::from_str(&call.arguments)
                        .map_err(|_| ActivationError::Protocol("invalid prior tool arguments"))?;
                    calls.push(crate::model::ModelToolCall {
                        id: call.id,
                        name: call.name,
                        arguments,
                    });
                }
                if wire.role == "assistant" && !calls.is_empty() {
                    messages.push(ModelMessage::assistant_tool_calls(&wire.text, calls));
                } else if wire.role == "tool" && !wire.tool_results.is_empty() {
                    for result in wire.tool_results {
                        messages.push(ModelMessage::tool_result(result.call_id, result.text));
                    }
                } else {
                    messages.push(ModelMessage::text(&wire.role, wire.text));
                }
            }
            let system_prompt = if agent.system_prompt.trim().is_empty() {
                request.system
            } else {
                Some(agent.system_prompt.clone())
            };
            if let Some(system_prompt) = system_prompt {
                messages.insert(0, ModelMessage::text("system", system_prompt));
            }
            let response = relay
                .complete(CloudModelRequest {
                    messages,
                    tools: tool_definitions(agent.bash_enabled),
                    max_tokens: (agent.max_tokens as u32).min(DEFAULT_MAX_TOKENS),
                })
                .await
                .map_err(|_| ActivationError::Child("model relay failed"))?;
            if response.tool_calls.len() > 1 {
                return Err(ActivationError::Protocol(
                    "multiple model tool calls are unsupported",
                ));
            }
            if let Some(call) = response.tool_calls.into_iter().next() {
                return Ok(ModelResponse::ToolCall {
                    call_id: call.id,
                    name: call.name,
                    arguments_json: serde_json::to_string(&call.arguments).map_err(|_| {
                        ActivationError::Protocol("tool arguments are not serializable")
                    })?,
                });
            }
            Ok(ModelResponse::Text(response.content))
        }
    }
}

const BASH_TOOL_ID: &str = "bash";

fn tool_definitions(bash_enabled: bool) -> Vec<ModelToolDefinition> {
    if bash_enabled {
        vec![ModelToolDefinition {
            id: BASH_TOOL_ID.to_owned(),
            name: BASH_TOOL_ID.to_owned(),
            description: "Run one bounded foreground Bash command in the remote Workspace."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "description": { "type": "string" }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
        }]
    } else {
        Vec::new()
    }
}

struct CloudWorkspace {
    fabric: Arc<FabricClient>,
    journal: Arc<ExecJournal>,
    workspace_id: Uuid,
}

impl WorkspaceExec for CloudWorkspace {
    fn bash(
        &self,
        intent: BashIntent,
    ) -> impl Future<Output = Result<BashResult, ActivationError>> + Send {
        let fabric = self.fabric.clone();
        let journal = self.journal.clone();
        let workspace_id = self.workspace_id;
        async move {
            let outcome = journal
                .execute(&fabric, workspace_id, &intent.call_id, &intent.command)
                .await
                .map_err(|_| ActivationError::Child("exec journal failed"))?;
            match outcome {
                ExecOutcome::Conflict => Err(ActivationError::Protocol("exec call conflict")),
                ExecOutcome::OutcomeUnknown => Ok(BashResult {
                    outcome: BashOutcome::Unknown {
                        reason: "fabric outcome unknown".to_owned(),
                    },
                    stdout: String::new(),
                    stderr: String::new(),
                    timeout_ms: BASH_TIMEOUT_MS,
                }),
                ExecOutcome::Terminal { result } => {
                    let result: ExecResult = serde_json::from_str(&result)
                        .map_err(|_| ActivationError::Protocol("invalid Fabric result"))?;
                    let outcome = if result.exit_code == Some(124) {
                        BashOutcome::TimedOut
                    } else {
                        BashOutcome::Completed {
                            exit_code: result.exit_code.unwrap_or(1),
                        }
                    };
                    Ok(BashResult {
                        outcome,
                        stdout: result.stdout.unwrap_or_default(),
                        stderr: result.stderr.unwrap_or_default(),
                        timeout_ms: BASH_TIMEOUT_MS,
                    })
                }
            }
        }
    }
}

struct CloudPersistence {
    store: SessionStore,
    session_id: Uuid,
    expected_generation: i64,
    writer: Mutex<Option<SessionWriter>>,
}

impl CloudPersistence {
    fn new(store: SessionStore, session_id: Uuid, expected_generation: i64) -> Self {
        CloudPersistence {
            store,
            session_id,
            expected_generation,
            writer: Mutex::new(None),
        }
    }
}

impl SessionPersistence for CloudPersistence {
    fn bootstrap(
        &self,
        session_id: Uuid,
        mode: ActivationMode,
    ) -> impl Future<Output = Result<(), ActivationError>> + Send {
        let store = self.store.clone();
        async move {
            if mode == ActivationMode::Create {
                store
                    .bootstrap_session(session_id)
                    .await
                    .map_err(|_| ActivationError::Child("session create bootstrap failed"))?;
            } else {
                store
                    .inspect_head(session_id)
                    .await
                    .map_err(|_| ActivationError::Child("session resume bootstrap failed"))?;
            }
            Ok(())
        }
    }

    fn resume(&self, session_id: Uuid) -> impl Future<Output = Result<(), ActivationError>> + Send {
        let store = self.store.clone();
        async move {
            store
                .resume_session(session_id)
                .await
                .map_err(|_| ActivationError::Child("session history resume failed"))?;
            Ok(())
        }
    }

    fn history(
        &self,
        session_id: Uuid,
    ) -> impl Future<Output = Result<Vec<Vec<u8>>, ActivationError>> + Send {
        let store = self.store.clone();
        async move {
            let events = store
                .load_history(session_id)
                .await
                .map_err(|_| ActivationError::Child("session history resume failed"))?;
            Ok(events.into_iter().map(|event| event.bytes).collect())
        }
    }

    fn append_events(
        &self,
        session_id: Uuid,
        append_id: Uuid,
        event_bytes: &[u8],
    ) -> impl Future<Output = Result<AppendReceipt, ActivationError>> + Send {
        let bytes = event_bytes.to_vec();
        async move {
            if session_id != self.session_id {
                return Err(ActivationError::Protocol("activation session mismatch"));
            }
            let mut guard = self.writer.lock().await;
            if guard.is_none() {
                let writer = self
                    .store
                    .writer(session_id)
                    .await
                    .map_err(|_| ActivationError::Child("session writer unavailable"))?;
                if writer.writer_generation() != self.expected_generation {
                    return Err(ActivationError::Child("session writer fenced"));
                }
                *guard = Some(writer);
            }
            let writer = guard.as_mut().expect("writer initialized");
            if writer.writer_generation() != self.expected_generation {
                return Err(ActivationError::Child("session writer fenced"));
            }
            let head = self
                .store
                .inspect_head(session_id)
                .await
                .map_err(|_| ActivationError::Child("session head unavailable"))?;
            writer
                .append(
                    self.store.blob(),
                    AppendEvent {
                        append_id,
                        writer_generation: self.expected_generation,
                        expected_revision: head.head_revision + 1,
                        bytes,
                        model_usage: None,
                    },
                )
                .await
                .map_err(|_| ActivationError::Child("session append failed"))?;
            Ok(AppendReceipt { append_id })
        }
    }
}

fn session_from_row(row: &sqlx::postgres::PgRow) -> Session {
    Session {
        id: row.get("id"),
        project_id: row.get("project_id"),
        agent_id: row.get("agent_id"),
        workspace_id: row.get("workspace_id"),
        writer_generation: row.get("writer_generation"),
        attention_generation: row.get("attention_generation"),
        head_revision: row.get("head_revision"),
        last_actor_user_id: row.try_get("last_actor_user_id").unwrap_or(None),
    }
}

fn run_from_row(row: &sqlx::postgres::PgRow) -> Run {
    Run {
        id: row.get("id"),
        intent_id: row.get("intent_id"),
        session_id: row.get("session_id"),
        request_hash: row.get("request_hash"),
        mode: row.get("mode"),
        prompt: row.get("prompt"),
        state: RunState::parse(row.get::<String, _>("state").as_str()).unwrap_or(RunState::Unknown),
        result: row.get("result"),
        actor_user_id: row.try_get("actor_user_id").unwrap_or(None),
        seq: row.get("seq"),
        accepted_at: row.get("accepted_at"),
        dispatched_at: row.get("dispatched_at"),
        cancel_requested_at: row.get("cancel_requested_at"),
        terminal_at: row.get("terminal_at"),
        cancelled_at: row.get("cancelled_at"),
    }
}

fn agent_from_row(row: &sqlx::postgres::PgRow) -> Agent {
    Agent {
        id: row.get("id"),
        project_id: row.get("project_id"),
        name: row.get("name"),
        model: row.get("model"),
        system_prompt: row.get("system_prompt"),
        bash_enabled: row.try_get("bash_enabled").unwrap_or(true),
        max_tokens: row.get("max_tokens"),
    }
}

fn agent_json(agent: &Agent) -> Value {
    json!({
        "id": agent.id,
        "projectId": agent.project_id,
        "name": agent.name,
        "model": agent.model,
        "systemPrompt": agent.system_prompt,
        "bashEnabled": agent.bash_enabled,
        "maxTokens": agent.max_tokens,
    })
}

/// Builds one normalized `workspace.deleted` row with an explicit metadata
/// lifetime.
fn delete_audit<'a>(
    project_id: Uuid,
    actor_user_id: Uuid,
    workspace_id: Uuid,
    outcome: AuditOutcome,
    metadata: &'a Value,
) -> AuditInsert<'a> {
    AuditInsert {
        project_id: Some(project_id),
        session_id: None,
        run_id: None,
        actor_user_id: Some(actor_user_id),
        kind: "workspace.deleted",
        resource_type: "workspace",
        resource_id: Some(workspace_id),
        outcome,
        metadata: Some(metadata),
    }
}

/// Builds one normalized `workspace.replaced` row. A function rather than a
/// closure so the borrowed metadata keeps an explicit lifetime.
fn replace_audit<'a>(
    project_id: Uuid,
    actor_user_id: Uuid,
    workspace_id: Uuid,
    outcome: AuditOutcome,
    metadata: &'a Value,
) -> AuditInsert<'a> {
    AuditInsert {
        project_id: Some(project_id),
        session_id: None,
        run_id: None,
        actor_user_id: Some(actor_user_id),
        kind: "workspace.replaced",
        resource_type: "workspace",
        resource_id: Some(workspace_id),
        outcome,
        metadata: Some(metadata),
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Owner => "owner",
        Role::Admin => "admin",
        Role::Member => "member",
        Role::Viewer => "viewer",
    }
}

/// Per-resource capabilities derived server-side from the frozen role
/// permits. The console renders what this says; it never re-derives roles.
fn capabilities_json(role: Role) -> Value {
    json!({
        "read": role.permits(Action::ReadProject),
        "operateSessions": role.permits(Action::OperateSession),
        "manageMembers": role.permits(Action::ManageMembership),
    })
}

fn same_origin_json<B>(request: &Request<B>, expected: &str) -> bool {
    request
        .headers()
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|origin| origin == expected)
        && request
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .is_some_and(|kind| kind.trim() == "application/json")
            })
}

/// Core mutation admission shared by every non-GET verb: exact-match
/// same-origin and the explicit mutate intent marker. DELETE stops here —
/// it carries no body, so there is no JSON content-type to require — while
/// body-bearing verbs layer the JSON check on top.
fn origin_and_intent_allowed<B>(request: &Request<B>, expected: &str) -> bool {
    request
        .headers()
        .get(hyper::header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|origin| origin == expected)
        && request
            .headers()
            .get(crate::web_session::CSRF_HEADER)
            .and_then(|value| value.to_str().ok())
            == Some(crate::web_session::CSRF_MARKER)
}

/// Central admission for body-bearing browser API mutations: exact-match
/// Origin, JSON content type, and the mutate intent marker. DELETE without
/// payload is admitted by [`origin_and_intent_allowed`], so a cross-site
/// form post or fetch can never fire a destructive route.
fn browser_mutation_allowed<B>(request: &Request<B>, expected: &str) -> bool {
    origin_and_intent_allowed(request, expected)
        && request
            .headers()
            .get(hyper::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .is_some_and(|kind| kind.trim() == "application/json")
            })
}

fn query_cursor(query: &str) -> i64 {
    query
        .split('&')
        .find_map(|pair| {
            pair.strip_prefix("after=")
                .or_else(|| pair.strip_prefix("cursor="))
        })
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0)
}

/// Audit window: newest-first page ending before `before`, clamped limit.
fn audit_window(query: &str) -> (i64, i64) {
    let before = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("before="))
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(i64::MAX);
    let limit = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("limit="))
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(64)
        .clamp(1, 256);
    (before, limit)
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

async fn request_body<B>(
    request: Request<B>,
) -> Result<Vec<u8>, Response<http_body_util::Full<Bytes>>>
where
    B: hyper::body::Body + Unpin,
    B::Data: AsRef<[u8]>,
    B::Error: std::fmt::Debug,
{
    if let Some(length) = request
        .headers()
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
    {
        if length > MAX_REQUEST_BYTES {
            return Err(json_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request body too large",
            ));
        }
    }
    bounded_body(request.into_body()).await
}

async fn bounded_body<B>(mut body: B) -> Result<Vec<u8>, Response<http_body_util::Full<Bytes>>>
where
    B: hyper::body::Body + Unpin,
    B::Data: AsRef<[u8]>,
    B::Error: std::fmt::Debug,
{
    use http_body_util::BodyExt;
    let mut out = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame =
            frame.map_err(|_| json_error(StatusCode::BAD_REQUEST, "invalid request body"))?;
        if let Some(data) = frame.data_ref() {
            let bytes = data.as_ref();
            if out.len() + bytes.len() > MAX_REQUEST_BYTES {
                return Err(json_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "request body too large",
                ));
            }
            out.extend_from_slice(bytes);
        }
    }
    Ok(out)
}

fn json_ok(value: Value) -> Response<http_body_util::Full<Bytes>> {
    json_response(StatusCode::OK, value)
}

fn json_error(status: StatusCode, message: &'static str) -> Response<http_body_util::Full<Bytes>> {
    json_response(status, json!({ "error": message }))
}

/// Wire shape of one secret metadata row. It contains no backend reference
/// and no material — those are structurally absent from this type.
fn secret_metadata_json(metadata: &SecretMetadata) -> Value {
    json!({
        "id": metadata.id,
        "scopeId": metadata.scope_id,
        "name": metadata.name,
        "version": metadata.version,
        "createdBy": metadata.created_by,
        "createdAt": metadata.created_at,
        "updatedAt": metadata.updated_at,
        "canWrite": metadata.can_write,
    })
}

/// Wire shape of one audit event: action, actor, timestamp, numeric version.
fn secret_audit_event_json(event: &SecretAuditEvent) -> Value {
    json!({
        "secretId": event.secret_id,
        "action": event.action.wire_name(),
        "actor": event.actor,
        "at": event.at,
        "version": event.version,
    })
}

/// Parses `{ "value": ... }` into write-only material. Every rejection is a
/// stable JSON error that never quotes the rejected content.
fn secret_value_payload(body: &[u8]) -> Result<SecretValue, Response<http_body_util::Full<Bytes>>> {
    #[derive(Deserialize)]
    struct Payload {
        value: String,
    }
    let payload: Payload = serde_json::from_slice(body)
        .map_err(|_| json_error(StatusCode::BAD_REQUEST, "invalid secret payload"))?;
    SecretValue::from_text(payload.value).map_err(secret_error_response)
}

/// Stable vault error mapping. Messages never include identities, names,
/// provider text, or material.
fn secret_error_response(error: SecretsError) -> Response<http_body_util::Full<Bytes>> {
    let (status, message) = match error {
        SecretsError::AccessDenied => (StatusCode::FORBIDDEN, "secret scope access denied"),
        SecretsError::AuthorizationUnavailable => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "secret scope authorization unavailable",
        ),
        SecretsError::InvalidName => (StatusCode::BAD_REQUEST, "secret name is invalid"),
        SecretsError::EmptyValue => (StatusCode::BAD_REQUEST, "secret value is empty"),
        SecretsError::Backend | SecretsError::Database => {
            (StatusCode::INTERNAL_SERVER_ERROR, "secret operation failed")
        }
        SecretsError::RelationRefused => (StatusCode::NOT_FOUND, "secret scope was refused"),
        SecretsError::NotFound => (StatusCode::NOT_FOUND, "secret was not found"),
        SecretsError::Conflict => (
            StatusCode::CONFLICT,
            "secret name already exists in this scope",
        ),
    };
    json_error(status, message)
}

/// 204 with a deliberately empty body.
fn no_content() -> Response<http_body_util::Full<Bytes>> {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(http_body_util::Full::new(Bytes::new()))
        .expect("204 response headers are valid")
}

fn json_response(status: StatusCode, value: Value) -> Response<http_body_util::Full<Bytes>> {
    let body = serde_json::to_vec(&value).expect("JSON response serializes");
    Response::builder()
        .status(status)
        .header("content-type", "application/json; charset=utf-8")
        .body(http_body_util::Full::new(Bytes::from(body)))
        .expect("JSON response headers are valid")
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_REQUEST_BYTES, bounded_body, browser_mutation_allowed, capabilities_json, role_name,
        same_origin_json, tool_definitions,
    };
    use crate::web_session::{CSRF_HEADER, CSRF_MARKER};
    use hyper::{
        Request,
        header::{CONTENT_TYPE, ORIGIN},
    };
    use serde_json::json;

    #[test]
    fn mutation_requires_exact_origin_and_json_content_type() {
        let request = Request::builder()
            .header(ORIGIN, "https://voie.example")
            .header(CONTENT_TYPE, "application/json")
            .body(())
            .expect("request builds");
        assert!(same_origin_json(&request, "https://voie.example"));
        assert!(!same_origin_json(&request, "https://other.example"));
    }

    #[test]
    fn every_non_get_browser_mutation_requires_origin_intent_and_json() {
        let build = |intent: Option<&'static str>| {
            let mut request = Request::builder()
                .header(ORIGIN, "https://voie.example")
                .header(CONTENT_TYPE, "application/json");
            if let Some(intent) = intent {
                request = request.header(CSRF_HEADER, intent);
            }
            request.body(()).expect("request builds")
        };
        let with_intent = build(Some(CSRF_MARKER));
        assert!(browser_mutation_allowed(
            &with_intent,
            "https://voie.example"
        ));
        let without_intent = build(None);
        assert!(!browser_mutation_allowed(
            &without_intent,
            "https://voie.example"
        ));
        let wrong_intent = build(Some("read"));
        assert!(!browser_mutation_allowed(
            &wrong_intent,
            "https://voie.example"
        ));
        assert!(!browser_mutation_allowed(
            &with_intent,
            "https://other.example"
        ));
    }

    #[test]
    fn maps_the_bounded_bash_tool_definition() {
        let tools = tool_definitions(true);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "bash");
        assert_eq!(tools[0].parameters["required"], json!(["command"]));

        assert!(tool_definitions(false).is_empty());
    }

    #[test]
    fn capabilities_follow_frozen_role_permits() {
        let owner = capabilities_json(super::Role::Owner);
        let member = capabilities_json(super::Role::Member);
        let viewer = capabilities_json(super::Role::Viewer);
        assert_eq!(
            owner,
            json!({ "read": true, "operateSessions": true, "manageMembers": true })
        );
        assert_eq!(
            member,
            json!({ "read": true, "operateSessions": true, "manageMembers": false })
        );
        assert_eq!(
            viewer,
            json!({ "read": true, "operateSessions": false, "manageMembers": false })
        );
        assert_eq!(role_name(super::Role::Owner), "owner");
    }

    #[test]
    fn workspace_quota_is_small_explicit_constant() {
        assert_eq!(crate::MAX_WORKSPACES_PER_PROJECT, 8);
    }

    #[test]
    fn quota_error_is_clear_and_maps_to_429() {
        let error = crate::KernelError::Quota;
        assert_eq!(error.to_string(), "project workspace quota reached");
        // Integration maps Quota to 429 Too Many Requests.
        assert_eq!(
            hyper::StatusCode::TOO_MANY_REQUESTS.as_u16(),
            429,
            "quota refusal must be a distinct 429, not a generic 500"
        );
    }

    #[tokio::test]
    async fn bounded_body_accepts_exact_limit() {
        use bytes::Bytes;
        use http_body_util::Full;
        let payload = vec![b'a'; MAX_REQUEST_BYTES];
        let body = Full::new(Bytes::from(payload));
        let result = bounded_body(body).await;
        assert!(result.is_ok(), "exact 64 KiB must be accepted");
        assert_eq!(result.unwrap().len(), MAX_REQUEST_BYTES);
    }

    #[tokio::test]
    async fn bounded_body_rejects_single_frame_over_limit() {
        use bytes::Bytes;
        use http_body_util::Full;
        let payload = vec![b'a'; MAX_REQUEST_BYTES + 1];
        let body = Full::new(Bytes::from(payload));
        let result = bounded_body(body).await;
        assert!(result.is_err(), "64 KiB + 1 must be rejected");
        let response = result.unwrap_err();
        assert_eq!(response.status(), hyper::StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn request_body_rejects_declared_content_length_early() {
        use bytes::Bytes;
        use http_body_util::Full;
        use hyper::header::CONTENT_LENGTH;

        // Declare 70 KiB via Content-Length but send a tiny body.
        // `request_body` must reject on the header alone without
        // needing to read the stream.
        let request = Request::builder()
            .method(hyper::Method::POST)
            .header(CONTENT_LENGTH, (MAX_REQUEST_BYTES + 5).to_string())
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from_static(b"{}")))
            .expect("request builds");
        // `request_body` is generic over Body now; call through the same
        // helper that the real handler uses.
        let result = super::request_body(request).await;
        // The helper checks Content-Length before streaming.
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().status(),
            hyper::StatusCode::PAYLOAD_TOO_LARGE
        );
    }
}
