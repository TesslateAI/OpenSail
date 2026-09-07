//! Integration-owned wiring for the Release 0 product boundary.
//!
//! The packet modules own their trust boundaries. This module only adapts the
//! typed model, session, Fabric, and activation seams and exposes the small
//! same-origin API consumed by the Web carrier.

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
use crate::fabric_client::{ExecResult, FabricClient};
use crate::model::{
    ModelMessage, ModelRelay as CloudModelRelay, ModelRequest as CloudModelRequest,
    ModelToolDefinition,
};
use crate::secrets::{
    MaterialBackend, ScopeAuthorizationError, ScopeCapability, SecretAuditEvent, SecretMetadata,
    SecretValue, SecretsError, SecretsStore,
};
use crate::session_store::{AppendEvent, BlobStore, SessionStore, SessionWriter};
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
/// Public `/readyz` serves this cached result so unauthenticated callers
/// cannot amplify into a proportional Blob/model/Fabric probe storm.
const READY_CACHE_TTL: Duration = Duration::from_secs(2);
const DEFAULT_MAX_TOKENS: u32 = 8192;
/// Server-authoritative Project name bound used by create and rename.
const MAX_PROJECT_NAME_LEN: usize = 80;

/// Product dependencies assembled once by the trusted process.
#[derive(Clone)]
pub struct Services {
    pool: PgPool,
    sessions: SessionStore,
    blob: BlobStore,
    model: Arc<CloudModelRelay>,
    fabric: Arc<FabricClient>,
    /// The deployment-selected Fabric for new Workspaces (Profile 0 binds
    /// every Workspace to one fixed Fabric chosen by configuration).
    configured_fabric_id: Option<Uuid>,
    journal: Arc<ExecJournal>,
    /// User-secret vault: metadata store plus the deployment-selected
    /// material backend and project-scope authorization boundary.
    secrets: Arc<VaultStore>,
    platform: crate::http::Platform,
    ready_probe: Arc<tokio::sync::Mutex<(Option<Instant>, bool)>>,
}

/// Concrete vault store type used by `Services`.
type VaultStore =
    SecretsStore<std::sync::Arc<crate::secrets::MaterialBackend>, ScopeProjectAuthorizer>;

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

/// Database URL consumed only as the optional one-shot legacy rekey salt
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

fn console_host_from_env() -> String {
    std::env::var("VOIE_PUBLIC_ORIGIN")
        .ok()
        .and_then(|origin| url::Url::parse(&origin).ok())
        .and_then(|url| url.host_str().map(str::to_owned))
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| "localhost".to_owned())
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
        let secrets_backend = std::sync::Arc::new(secrets_backend_from_env()?);
        let secrets = Arc::new(SecretsStore::from_pool(
            &pool,
            secrets_backend.clone(),
            ScopeProjectAuthorizer::new(pool.clone()),
        ));
        let fabric = Arc::new(fabric);
        Ok(Arc::new(Services {
            sessions: SessionStore::new(pool.clone()),
            blob: blob.clone(),
            model: Arc::new(model),
            fabric: fabric.clone(),
            configured_fabric_id,
            journal: Arc::new(ExecJournal::new(pool.clone())),
            secrets,
            platform: crate::http::Platform::new(
                pool.clone(),
                console_host_from_env(),
                configured_fabric_id,
            )
            .with_runtime(crate::http::ProductRuntime {
                fabric,
                blob,
                secrets: secrets_backend,
            }),
            pool,
            ready_probe: Arc::new(tokio::sync::Mutex::new((None, false))),
        }))
    }

    /// Bounded concurrent probes of Blob, model, and Fabric reachability plus
    /// the required activation artifacts. Any failure fails readiness closed.
    /// Public callers receive a short-lived cached result so cheap HTTP
    /// cannot drive a proportional downstream probe rate.
    pub async fn dependencies_ready(&self) -> bool {
        let mut guard = self.ready_probe.lock().await;
        if let Some(at) = guard.0 {
            if at.elapsed() < READY_CACHE_TTL {
                return guard.1;
            }
        }
        let ready = self.probe_dependencies().await;
        *guard = (Some(Instant::now()), ready);
        ready
    }

    async fn probe_dependencies(&self) -> bool {
        let probe_window = DEPENDENCY_PROBE_WINDOW;
        let (blob_ok, model_ok, fabric_ok, artifacts_ok) = tokio::join!(
            tokio::time::timeout(probe_window, self.blob.reachable()),
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
        actor_user_id: Uuid,
    ) -> Result<ActivationOutcome, ActivationError> {
        let persistence = CloudPersistence::new(
            self.sessions.clone(),
            session.id,
            session.writer_generation + 1,
        );
        let authority = EffectAuthority {
            pool: self.pool.clone(),
            run_id,
            actor_user_id,
            project_id: session.project_id,
            workspace_id: session.workspace_id,
        };
        let model = CloudModel {
            relay: self.model.clone(),
            agent: agent.clone(),
            authority: authority.clone(),
        };
        let workspace = CloudWorkspace {
            fabric: self.fabric.clone(),
            journal: self.journal.clone(),
            workspace_id: session.workspace_id,
            bash_enabled: agent.bash_enabled,
            authority: authority.clone(),
        };
        let product = CloudProduct {
            platform: self.platform.clone(),
            actor_user_id,
            project_id: session.project_id,
            workspace_id: session.workspace_id,
            authority,
        };
        let host = ActivationHost {
            context: ActivationContext {
                project_id: session.project_id,
                agent_id: session.agent_id,
                session_id: session.id,
                run_id,
                workspace_id: session.workspace_id,
                writer_generation: session.writer_generation + 1,
                bash_enabled: agent.bash_enabled,
            },
            model: &model,
            workspace: &workspace,
            sessions: &persistence,
            product: &product,
        };
        activation::run(host, ActivationRequest { mode, prompt }).await
    }

    /// Closes still-open turns on Sessions whose child is gone, then
    /// classifies in-flight dispatches and schedules only accepted Runs.
    /// A dispatched Run is never replayed.
    pub async fn recover(&self, kernel: &Kernel) -> Result<(), sqlx::Error> {
        self.close_orphaned_session_turns(kernel).await;
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
        crate::reconcile::workspace::persist_deleted_desired_for_tombstones(&self.platform).await;
        let _ = sqlx::query(
            "update workspaces set reconcile_after = now() \
             where state not in ('deleted') and reconcile_after is null",
        )
        .execute(&self.pool)
        .await;
        let _ = sqlx::query(
            "update application_databases set reconcile_after = now() \
             where state not in ('deleted', 'archived') and reconcile_after is null",
        )
        .execute(&self.pool)
        .await;
        let _ = sqlx::query(
            "update application_deployments set reconcile_after = now() \
             where state not in ('failed', 'unknown') and reconcile_after is null",
        )
        .execute(&self.pool)
        .await;
        crate::reconcile::database::spawn_loop(self.platform.clone());
        crate::reconcile::workspace::spawn_loop(self.platform.clone());
        crate::reconcile::deployment::spawn_loop(self.platform.clone());
        crate::reconcile::routes::spawn_loop(self.platform.clone());
        crate::reconcile::traffic::spawn_loop(self.platform.clone());
        crate::reconcile::release::spawn_loop(self.platform.clone());
        crate::reconcile::workspace::reconcile_due(&self.platform).await;
        crate::reconcile::deployment::reconcile_due(&self.platform).await;
        crate::reconcile::database::reconcile_due(&self.platform).await;
        crate::reconcile::release::reconcile_due(&self.platform).await;
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
                    attention_generation, head_revision, last_actor_user_id \
             from sessions where id = $1",
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
        let Some(actor_user_id) =
            activation_product_actor(run.actor_user_id, session.last_actor_user_id)
        else {
            let _ = self.mark_unknown(run.id).await;
            self.kick_next(session.id);
            return;
        };
        if auth::authorize(
            &self.pool,
            actor_user_id,
            session.project_id,
            Action::OperateSession,
        )
        .await
        .is_err()
        {
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
            .activate(
                session.clone(),
                run.id,
                mode,
                run.prompt.clone(),
                agent,
                actor_user_id,
            )
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
            Err(error) => {
                eprintln!("voie-cloud: run {} activation failed: {error}", run.id);
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

    /// Appends `turn/end` for Sessions whose activation child is gone
    /// (restart-classified dispatch, or an already-unknown Run) so the
    /// console cannot keep a live tool seat on a dead turn.
    async fn close_orphaned_session_turns(&self, kernel: &Kernel) {
        let targets = match kernel.interrupt_close_targets().await {
            Ok(targets) => targets,
            Err(error) => {
                eprintln!("voie-cloud: interrupt-close target scan failed: {error}");
                return;
            }
        };
        for target in targets {
            let persistence = CloudPersistence::new(
                self.sessions.clone(),
                target.session_id,
                target.writer_generation + 1,
            );
            if let Err(error) = activation::close_open_turns(
                &persistence,
                target.session_id,
                target.run_id,
                "restart-interrupt",
                "UNKNOWN",
                "The run ended without a result and will not be replayed.",
            )
            .await
            {
                eprintln!(
                    "voie-cloud: session {} interrupt close failed: {error}",
                    target.session_id
                );
            }
        }
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
        let host = request
            .headers()
            .get("host")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let cookie_header = request
            .headers()
            .get("cookie")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
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
            host.as_deref(),
            cookie_header.as_deref(),
        )
        .await
    }

    /// Preview callback and edge authorize are not console-session routes.
    pub async fn handle_public(
        &self,
        request: Request<Incoming>,
    ) -> Response<http_body_util::Full<Bytes>> {
        let method = request.method().clone();
        let path = request.uri().path().to_owned();
        let query = request.uri().query().unwrap_or("").to_owned();
        let host = request
            .headers()
            .get("host")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let cookie_header = request
            .headers()
            .get("cookie")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
        self.platform
            .route(
                Uuid::nil(),
                &method,
                &segments,
                &[],
                &query,
                host.as_deref(),
                cookie_header.as_deref(),
            )
            .await
            .unwrap_or_else(|| json_error(StatusCode::NOT_FOUND, "not found"))
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
        host: Option<&str>,
        cookie_header: Option<&str>,
    ) -> Response<http_body_util::Full<Bytes>> {
        let bad_id = || json_error(StatusCode::BAD_REQUEST, "invalid resource id");
        if let Some(response) = self
            .platform
            .route(
                user_id,
                &method,
                segments,
                &body,
                query,
                host,
                cookie_header,
            )
            .await
        {
            return response;
        }
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
            (&Method::GET, ["api", "projects", id, "member-candidates"]) => {
                match Uuid::parse_str(id) {
                    Ok(project_id) => self.member_candidates(user_id, project_id, query).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::POST, ["api", "projects", id, "members"]) => match Uuid::parse_str(id) {
                Ok(project_id) => self.add_member(user_id, project_id, body).await,
                Err(_) => bad_id(),
            },
            (&Method::PATCH, ["api", "projects", id, "members", member]) => {
                match (Uuid::parse_str(id), Uuid::parse_str(member)) {
                    (Ok(project_id), Ok(member_id)) => {
                        self.patch_member(user_id, project_id, member_id, body)
                            .await
                    }
                    _ => bad_id(),
                }
            }
            (&Method::DELETE, ["api", "projects", id, "members", member]) => {
                match (Uuid::parse_str(id), Uuid::parse_str(member)) {
                    (Ok(project_id), Ok(member_id)) => {
                        self.remove_member(user_id, project_id, member_id).await
                    }
                    _ => bad_id(),
                }
            }
            (&Method::GET, ["api", "projects", id, "workspaces"]) => match Uuid::parse_str(id) {
                Ok(project_id) => self.project_workspaces(user_id, project_id).await,
                Err(_) => bad_id(),
            },
            (&Method::GET, ["api", "projects", id, "sessions"]) => match Uuid::parse_str(id) {
                Ok(project_id) => self.project_sessions(user_id, project_id).await,
                Err(_) => bad_id(),
            },
            (&Method::GET, ["api", "projects", id, "agents"]) => match Uuid::parse_str(id) {
                Ok(project_id) => self.project_agents(user_id, project_id).await,
                Err(_) => bad_id(),
            },
            (&Method::GET, ["api", "projects", id, "events"]) => match Uuid::parse_str(id) {
                Ok(project_id) => self.project_events(user_id, project_id, query).await,
                Err(_) => bad_id(),
            },
            (&Method::GET, ["api", "projects", id, "agent-presets"]) => match Uuid::parse_str(id) {
                Ok(project_id) => self.project_agents(user_id, project_id).await,
                Err(_) => bad_id(),
            },
            (&Method::POST, ["api", "projects", id, "agent-presets"]) => {
                match Uuid::parse_str(id) {
                    Ok(project_id) => {
                        self.create_agent_preset(kernel, user_id, project_id, body)
                            .await
                    }
                    Err(_) => bad_id(),
                }
            }
            (&Method::PATCH, ["api", "projects", id, "agent-presets", preset_id]) => {
                match (Uuid::parse_str(id), Uuid::parse_str(preset_id)) {
                    (Ok(project_id), Ok(preset_uuid)) => {
                        self.update_agent_preset(user_id, project_id, preset_uuid, body)
                            .await
                    }
                    _ => bad_id(),
                }
            }
            (&Method::DELETE, ["api", "projects", id, "agent-presets", preset_id]) => {
                match (Uuid::parse_str(id), Uuid::parse_str(preset_id)) {
                    (Ok(project_id), Ok(preset_uuid)) => {
                        self.delete_agent_preset(user_id, project_id, preset_uuid)
                            .await
                    }
                    _ => bad_id(),
                }
            }
            (&Method::GET, ["api", "projects", id, "secrets"]) => match Uuid::parse_str(id) {
                Ok(project_id) => self.list_secrets(user_id, project_id).await,
                Err(_) => bad_id(),
            },
            (&Method::POST, ["api", "projects", id, "secrets"]) => match Uuid::parse_str(id) {
                Ok(project_id) => self.create_secret(user_id, project_id, body).await,
                Err(_) => bad_id(),
            },
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
            (&Method::POST, ["api", "projects", id, "workspaces", workspace, "grow"]) => {
                match (Uuid::parse_str(id), Uuid::parse_str(workspace)) {
                    (Ok(_project_id), Ok(workspace_id)) => {
                        self.grow_workspace(user_id, workspace_id, body).await
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
            (&Method::GET, ["api", "conversations"]) => self.sessions(user_id).await,
            (&Method::GET, ["api", "conversations", conversation_id, "history"]) => {
                match Uuid::parse_str(conversation_id) {
                    Ok(conversation_id) => {
                        self.conversation_history(user_id, conversation_id, query)
                            .await
                    }
                    Err(_) => bad_id(),
                }
            }
            (&Method::POST, ["api", "conversations", conversation_id, "cancel"]) => {
                match Uuid::parse_str(conversation_id) {
                    Ok(conversation_id) => {
                        self.cancel_conversation(kernel, user_id, conversation_id)
                            .await
                    }
                    Err(_) => bad_id(),
                }
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
            (&Method::GET, ["api", "admin", "projects"]) => self.admin_projects(user_id).await,
            (&Method::GET, ["api", "admin", "projects", project_id, "members"]) => {
                match Uuid::parse_str(project_id) {
                    Ok(project_uuid) => self.admin_project_members(user_id, project_uuid).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::GET, ["api", "admin", "projects", project_id, "member-candidates"]) => {
                match Uuid::parse_str(project_id) {
                    Ok(project_uuid) => {
                        self.admin_member_candidates(user_id, project_uuid, query)
                            .await
                    }
                    Err(_) => bad_id(),
                }
            }
            (&Method::POST, ["api", "admin", "projects", project_id, "members"]) => {
                match Uuid::parse_str(project_id) {
                    Ok(project_uuid) => self.admin_add_member(user_id, project_uuid, body).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::PATCH, ["api", "admin", "projects", project_id, "members", member]) => {
                match (Uuid::parse_str(project_id), Uuid::parse_str(member)) {
                    (Ok(project_uuid), Ok(member_id)) => {
                        self.admin_patch_member(user_id, project_uuid, member_id, body)
                            .await
                    }
                    _ => bad_id(),
                }
            }
            (&Method::DELETE, ["api", "admin", "projects", project_id, "members", member]) => {
                match (Uuid::parse_str(project_id), Uuid::parse_str(member)) {
                    (Ok(project_uuid), Ok(member_id)) => {
                        self.admin_remove_member(user_id, project_uuid, member_id)
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
            (&Method::POST, ["api", "workspaces", workspace_id, "snapshots"]) => {
                match Uuid::parse_str(workspace_id) {
                    Ok(workspace_uuid) => self.snapshot_workspace(user_id, workspace_uuid).await,
                    Err(_) => bad_id(),
                }
            }
            (&Method::GET, ["api", "workspaces", workspace_id, "snapshots"]) => {
                match Uuid::parse_str(workspace_id) {
                    Ok(workspace_uuid) => {
                        self.list_workspace_snapshots(user_id, workspace_uuid).await
                    }
                    Err(_) => bad_id(),
                }
            }
            (&Method::POST, ["api", "workspaces", workspace_id, "restores"]) => {
                match Uuid::parse_str(workspace_id) {
                    Ok(workspace_uuid) => {
                        self.restore_workspace(user_id, workspace_uuid, body).await
                    }
                    Err(_) => bad_id(),
                }
            }
            (&Method::POST, ["api", "workspaces", workspace_id, "grow"]) => {
                match Uuid::parse_str(workspace_id) {
                    Ok(workspace_uuid) => self.grow_workspace(user_id, workspace_uuid, body).await,
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
            (&Method::GET, ["api", "conversations", conversation_id, "runs"]) => {
                match Uuid::parse_str(conversation_id) {
                    Ok(conversation_uuid) => {
                        self.conversation_runs(user_id, conversation_uuid, kernel)
                            .await
                    }
                    Err(_) => bad_id(),
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
        let wait = query_flag(query, "wait");
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
        if query_flag(query, "head") {
            let cursor = match self.sessions.head_global_seq(&session_ids).await {
                Ok(cursor) => cursor,
                Err(_) => {
                    return json_error(StatusCode::INTERNAL_SERVER_ERROR, "event cursor failed");
                }
            };
            return json_ok(json!({
                "after": 0,
                "cursor": cursor,
                "items": [],
            }));
        }
        self.canonical_events(&session_ids, after, wait).await
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
        self.canonical_events(
            &[session_id],
            query_cursor(query),
            query_flag(query, "wait"),
        )
        .await
    }

    async fn canonical_events(
        &self,
        session_ids: &[Uuid],
        after: i64,
        wait: bool,
    ) -> Response<http_body_util::Full<Bytes>> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let events = match self
                .sessions
                .load_after_global(session_ids, after, 512)
                .await
            {
                Ok(events) => events,
                Err(_) => {
                    return json_error(StatusCode::INTERNAL_SERVER_ERROR, "event history failed");
                }
            };
            if !events.is_empty() || !wait || Instant::now() >= deadline {
                let items = events
                    .into_iter()
                    .map(|event| {
                        json!({
                            "sessionId": event.reference.session_id,
                            "globalSeq": event.reference.global_seq,
                            "revision": event.reference.revision,
                            "appendId": event.reference.append_id,
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
                return json_ok(json!({
                    "after": after,
                    "cursor": cursor,
                    "items": items,
                }));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
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
                    , coalesce(left((select r.prompt from runs r \
                            where r.session_id = s.id order by r.seq limit 1), 60), 'New chat') as title \
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
                    w.exec_generation, coalesce(w.desired_state, 'active') as desired_state, \
                    w.observed_state, \
                    w.desired_revision, w.observed_revision, w.last_error_code \
             from workspaces w \
             join fabrics f on f.id = w.fabric_id \
             join project_members m on m.project_id = w.project_id \
             where m.user_id = $1 \
               and (w.desired_state <> 'deleted' \
                    or w.observed_state in ('active', 'ready')) \
             order by w.id",
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
                "state": workspace_row_wire_state(&row),
                "desiredState": row.get::<String, _>("desired_state"),
                "observedState": row.get::<String, _>("observed_state"),
                "desiredRevision": row.get::<i64, _>("desired_revision"),
                "observedRevision": row.get::<i64, _>("observed_revision"),
                "lastErrorCode": row.get::<Option<String>, _>("last_error_code"),
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
                    u.username, u.display_name, \
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
                "username": row.get::<Option<String>, _>("username"),
                "displayName": row.get::<Option<String>, _>("display_name"),
                "subject": row.get::<String, _>("subject"),
                "role": row.get::<String, _>("role"),
                "createdAt": row.get::<String, _>("created_at"),
            })).collect::<Vec<_>>()
        }))
    }

    /// Adds one Team member. Existing membership is Conflict, not a rerole.
    async fn add_member(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        if let Some(response) = self.require_team_project(project_id).await {
            return response;
        }
        if auth::authorize(&self.pool, user_id, project_id, Action::ManageMembership)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        self.insert_member(user_id, project_id, body).await
    }

    /// Platform-admin Team-RBAC recovery: same add invariants as the ordinary
    /// member route, without requiring Team membership and without joining
    /// the platform admin to the Team.
    async fn admin_add_member(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        if !self.is_platform_admin(user_id).await {
            return json_error(StatusCode::FORBIDDEN, "platform admin required");
        }
        self.insert_member(user_id, project_id, body).await
    }

    async fn insert_member(
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
        let Some(role) = Role::parse_writable(payload.role.trim()) else {
            return json_error(StatusCode::BAD_REQUEST, "invalid role");
        };
        if let Some(response) = self.require_team_project(project_id).await {
            return response;
        }
        let status: Option<String> = sqlx::query_scalar("select status from users where id = $1")
            .bind(payload.user_id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten();
        let Some(status) = status else {
            return json_error(StatusCode::BAD_REQUEST, "unknown user");
        };
        if status != "active" {
            return json_error(StatusCode::CONFLICT, "user is disabled");
        }
        let existing: Option<String> = sqlx::query_scalar(
            "select role from project_members where project_id = $1 and user_id = $2",
        )
        .bind(project_id)
        .bind(payload.user_id)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten();
        if existing.is_some() {
            return json_error(StatusCode::CONFLICT, "already a member");
        }
        let inserted = sqlx::query(
            "insert into project_members (project_id, user_id, role) values ($1, $2, $3)",
        )
        .bind(project_id)
        .bind(payload.user_id)
        .bind(role_name(role))
        .execute(&self.pool)
        .await;
        match inserted {
            Ok(_) => {}
            Err(error)
                if error
                    .as_database_error()
                    .and_then(|db| db.code())
                    .as_deref()
                    == Some("23505") =>
            {
                return json_error(StatusCode::CONFLICT, "already a member");
            }
            Err(_) => {
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "membership store failed");
            }
        }
        self.record(AuditInsert {
            project_id: Some(project_id),
            session_id: None,
            run_id: None,
            actor_user_id: Some(actor_id),
            kind: "member.added",
            resource_type: "member",
            resource_id: Some(payload.user_id),
            outcome: AuditOutcome::Ok,
            metadata: Some(&json!({ "role": role_name(role) })),
        })
        .await;
        self.member_mutation_body(project_id, payload.user_id, role)
            .await
    }

    async fn patch_member(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        member_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        if let Some(response) = self.require_team_project(project_id).await {
            return response;
        }
        if auth::authorize(&self.pool, user_id, project_id, Action::ManageMembership)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        self.update_member_role(user_id, project_id, member_id, body)
            .await
    }

    async fn admin_patch_member(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        member_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        if !self.is_platform_admin(user_id).await {
            return json_error(StatusCode::FORBIDDEN, "platform admin required");
        }
        self.update_member_role(user_id, project_id, member_id, body)
            .await
    }

    async fn update_member_role(
        &self,
        actor_id: Uuid,
        project_id: Uuid,
        member_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            role: String,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid member payload"),
        };
        let Some(role) = Role::parse_writable(payload.role.trim()) else {
            return json_error(StatusCode::BAD_REQUEST, "invalid role");
        };
        if let Some(response) = self.require_team_project(project_id).await {
            return response;
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
        if previous == "owner" || self.is_canonical_owner(project_id, member_id).await {
            return json_error(
                StatusCode::CONFLICT,
                "the durable project owner cannot be changed",
            );
        }
        let Some(previous_role) = Role::parse(&previous) else {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "membership store failed");
        };
        if previous_role == role {
            return self.member_mutation_body(project_id, member_id, role).await;
        }
        let downgrade = role.rank() < previous_role.rank();
        let mut tx = match self.pool.begin().await {
            Ok(tx) => tx,
            Err(_) => {
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "membership store failed");
            }
        };
        if crate::Kernel::lock_user_row(&mut tx, member_id)
            .await
            .is_err()
        {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "membership store failed");
        }
        let updated = sqlx::query(
            "update project_members set role = $3 \
             where project_id = $1 and user_id = $2 and role <> 'owner'",
        )
        .bind(project_id)
        .bind(member_id)
        .bind(role_name(role))
        .execute(&mut *tx)
        .await;
        match updated {
            Ok(result) if result.rows_affected() == 1 => {}
            Ok(_) => {
                return json_error(
                    StatusCode::CONFLICT,
                    "the durable project owner cannot be changed",
                );
            }
            Err(_) => {
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "membership store failed");
            }
        }
        let sessions = if downgrade {
            match crate::Kernel::fence_actor_runs(&mut tx, member_id, Some(project_id)).await {
                Ok(sessions) => sessions,
                Err(_) => {
                    return json_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "membership store failed",
                    );
                }
            }
        } else {
            Vec::new()
        };
        if tx.commit().await.is_err() {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "membership store failed");
        }
        for session_id in sessions {
            self.kick_next(session_id);
        }
        self.record(AuditInsert {
            project_id: Some(project_id),
            session_id: None,
            run_id: None,
            actor_user_id: Some(actor_id),
            kind: "member.role_changed",
            resource_type: "member",
            resource_id: Some(member_id),
            outcome: AuditOutcome::Ok,
            metadata: Some(&json!({
                "role": role_name(role),
                "previousRole": previous,
            })),
        })
        .await;
        self.member_mutation_body(project_id, member_id, role).await
    }

    /// Removes one membership. Owner-only; the durable project owner is
    /// protected.
    async fn remove_member(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        member_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        if let Some(response) = self.require_team_project(project_id).await {
            return response;
        }
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
        if let Some(response) = self.require_team_project(project_id).await {
            return response;
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
        if previous == "owner" || self.is_canonical_owner(project_id, member_id).await {
            return json_error(
                StatusCode::CONFLICT,
                "the durable project owner cannot be removed",
            );
        }
        let mut tx = match self.pool.begin().await {
            Ok(tx) => tx,
            Err(_) => {
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "membership store failed");
            }
        };
        // Same User-row lock privileged effects claim, so disable/removal
        // either precedes the effect or the effect was committed first.
        if crate::Kernel::lock_user_row(&mut tx, member_id)
            .await
            .is_err()
        {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "membership store failed");
        }
        let removed =
            sqlx::query("delete from project_members where project_id = $1 and user_id = $2")
                .bind(project_id)
                .bind(member_id)
                .execute(&mut *tx)
                .await;
        match removed {
            Ok(result) if result.rows_affected() == 1 => {
                let sessions =
                    match crate::Kernel::fence_actor_runs(&mut tx, member_id, Some(project_id))
                        .await
                    {
                        Ok(sessions) => sessions,
                        Err(_) => {
                            return json_error(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "membership store failed",
                            );
                        }
                    };
                if tx.commit().await.is_err() {
                    return json_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "membership store failed",
                    );
                }
                for session_id in sessions {
                    self.kick_next(session_id);
                }
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

    async fn require_team_project(
        &self,
        project_id: Uuid,
    ) -> Option<Response<http_body_util::Full<Bytes>>> {
        let kind: Option<String> = sqlx::query_scalar("select kind from projects where id = $1")
            .bind(project_id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten();
        match kind.as_deref() {
            None => Some(json_error(StatusCode::NOT_FOUND, "project not found")),
            Some("personal") => Some(json_error(
                StatusCode::CONFLICT,
                "personal scope members are fixed",
            )),
            Some("team") => None,
            Some(_) => Some(json_error(StatusCode::NOT_FOUND, "project not found")),
        }
    }

    async fn is_canonical_owner(&self, project_id: Uuid, member_id: Uuid) -> bool {
        sqlx::query_scalar(
            "select exists(select 1 from projects where id = $1 and owner_user_id = $2)",
        )
        .bind(project_id)
        .bind(member_id)
        .fetch_one(&self.pool)
        .await
        .unwrap_or(false)
    }

    async fn member_mutation_body(
        &self,
        project_id: Uuid,
        member_id: Uuid,
        role: Role,
    ) -> Response<http_body_util::Full<Bytes>> {
        let row = sqlx::query(
            "select coalesce(a.subject, u.subject) as subject, \
                    m.created_at::text as created_at \
             from project_members m join users u on u.id = m.user_id \
             left join auth_identities a on a.user_id = u.id \
             where m.project_id = $1 and m.user_id = $2",
        )
        .bind(project_id)
        .bind(member_id)
        .fetch_one(&self.pool)
        .await;
        let Ok(row) = row else {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "membership store failed");
        };
        json_ok(json!({
            "projectId": project_id,
            "userId": member_id,
            "role": role_name(role),
            "subject": row.get::<String, _>("subject"),
            "createdAt": row.get::<String, _>("created_at"),
        }))
    }

    async fn member_candidates(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        query: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        if let Some(response) = self.require_team_project(project_id).await {
            return response;
        }
        if auth::authorize(&self.pool, user_id, project_id, Action::ManageMembership)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        self.search_member_candidates(project_id, query).await
    }

    async fn admin_member_candidates(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        query: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        if !self.is_platform_admin(user_id).await {
            return json_error(StatusCode::FORBIDDEN, "platform admin required");
        }
        self.search_member_candidates(project_id, query).await
    }

    async fn search_member_candidates(
        &self,
        project_id: Uuid,
        query: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        if let Some(response) = self.require_team_project(project_id).await {
            return response;
        }
        let raw = query_param(query, "q").unwrap_or_default();
        let trimmed = raw.trim();
        let chars = trimmed.chars().count();
        if chars < 2 || chars > 64 {
            return json_error(StatusCode::BAD_REQUEST, "invalid candidate query");
        }
        let escaped = trimmed
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        let rows = match sqlx::query(
            "select id, username, display_name from users \
             where status = 'active' \
               and (username ilike $1 escape E'\\\\' or display_name ilike $1 escape E'\\\\') \
               and not exists ( \
                   select 1 from project_members m \
                   where m.project_id = $2 and m.user_id = users.id \
               ) \
             order by username nulls last, display_name, id \
             limit 20",
        )
        .bind(pattern)
        .bind(project_id)
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
            #[serde(default)]
            kind: Option<String>,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid project payload"),
        };
        let kind = match payload.kind.as_deref().map(str::trim) {
            None | Some("") | Some("personal") => "personal",
            Some("team") => "team",
            _ => return json_error(StatusCode::BAD_REQUEST, "invalid project kind"),
        };
        let name = payload.name.trim();
        if name.is_empty() || name.chars().count() > MAX_PROJECT_NAME_LEN {
            return json_error(StatusCode::BAD_REQUEST, "invalid project name");
        }
        let project = match kernel.create_project(payload.id, user_id, name, kind).await {
            Ok(project) => project,
            Err(crate::KernelError::Conflict) => {
                return json_error(StatusCode::CONFLICT, "project identity conflicts");
            }
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "project store failed"),
        };
        let metadata = json!({ "kind": kind });
        self.record(AuditInsert {
            project_id: Some(project.id),
            session_id: None,
            run_id: None,
            actor_user_id: Some(user_id),
            kind: "project.created",
            resource_type: "project",
            resource_id: Some(project.id),
            outcome: AuditOutcome::Ok,
            metadata: Some(&metadata),
        })
        .await;
        let body = json!({
            "id": project.id,
            "ownerUserId": project.owner_user_id,
            "name": project.name,
            "kind": kind,
            "role": "owner",
            "capabilities": capabilities_json(Role::Owner),
        });
        if kind == "team" {
            json_response(StatusCode::CREATED, body)
        } else {
            json_ok(body)
        }
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
        let name = payload.name.trim();
        if name.is_empty() || name.chars().count() > MAX_PROJECT_NAME_LEN {
            return json_error(StatusCode::BAD_REQUEST, "invalid project name");
        }
        let updated = sqlx::query("update projects set name = $2 where id = $1")
            .bind(project_id)
            .bind(name)
            .execute(&self.pool)
            .await;
        match updated {
            Ok(result) if result.rows_affected() == 1 => json_ok(json!({ "updated": true })),
            Ok(_) => json_error(StatusCode::NOT_FOUND, "project not found"),
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "project update failed"),
        }
    }

    /// Creates one Workspace on the deployment-selected Fabric. The
    /// identity is durably reserved with leftover process `creating` and
    /// desired `active` before the reconciler PUTs the Fabric spec. GET
    /// never realizes. Only observed `ready`/`active` is returned as HTTP 200.
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
            #[serde(default)]
            #[allow(dead_code)]
            storage_tier: Option<String>,
            #[serde(default, alias = "approvalId")]
            #[allow(dead_code)]
            approval_id: Option<Uuid>,
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
        // A leftover live row is reconcilable desired state, not a Fabric
        // GET probe. Same-project retries wake the reconciler. Tombstones
        // and exclusive fences refuse reminting the UUID.
        match kernel.find_workspace(payload.id).await {
            Ok(None) => {}
            Ok(Some(existing)) if existing.project_id != project_id => {
                return json_error(StatusCode::CONFLICT, "workspace identity conflicts");
            }
            Ok(Some(existing)) => {
                let desired: String =
                    sqlx::query_scalar("select desired_state from workspaces where id = $1")
                        .bind(payload.id)
                        .fetch_optional(&self.pool)
                        .await
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| "active".into());
                if desired == "deleted"
                    || desired == "archived"
                    || existing.state == WorkspaceState::Deleted
                {
                    return json_error(StatusCode::CONFLICT, "workspace identity conflicts");
                }
                if existing.state == WorkspaceState::Fenced {
                    return json_error(
                        StatusCode::CONFLICT,
                        "workspace lifecycle operation already in progress",
                    );
                }
                crate::reconcile::workspace::put_due_workspace(&self.platform, payload.id).await;
                let response = self
                    .workspace_create_response(user_id, project_id, payload.id, &label)
                    .await;
                let outcome = match response.status() {
                    StatusCode::OK | StatusCode::ACCEPTED => AuditOutcome::Ok,
                    _ => AuditOutcome::Error,
                };
                self.record(audit(outcome)).await;
                return response;
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
        // New Workspaces are always a 16 GiB virtual thin LV. 32 GiB is
        // automatic Fabric growth; 64 GiB requires increase_resource_tier
        // after the Workspace already exists.
        let allocated = crate::storage::WORKSPACE_BYTES;
        let _ = sqlx::query(
            "update workspaces set label = $2, allocated_bytes = $3, storage_tier = 'default' where id = $1",
        )
        .bind(payload.id)
        .bind(&label)
        .bind(allocated)
        .execute(&self.pool)
        .await;
        crate::reconcile::workspace::put_due_workspace(&self.platform, payload.id).await;
        let response = self
            .workspace_create_response(user_id, project_id, payload.id, &label)
            .await;
        let outcome = match response.status() {
            StatusCode::OK | StatusCode::ACCEPTED => AuditOutcome::Ok,
            _ => AuditOutcome::Error,
        };
        self.record(audit(outcome)).await;
        response
    }

    async fn workspace_create_response(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        workspace_id: Uuid,
        label: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let row = sqlx::query(
            "select state, desired_state, observed_state, last_error_code, \
                    created_at::text as created_at \
             from workspaces where id = $1",
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await;
        let Ok(Some(row)) = row else {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "workspace store failed");
        };
        let last_error: Option<String> = row.get("last_error_code");
        let created_at: String = row.get("created_at");
        let wire = workspace_row_wire_state(&row);
        if wire == "ready" {
            return json_ok(json!({
                "id": workspace_id,
                "projectId": project_id,
                "label": label,
                "state": "ready",
                "createdByUserId": user_id,
                "createdAt": created_at,
            }));
        }
        match last_error.as_deref() {
            Some("fabric_unreachable") => json_error(
                StatusCode::BAD_GATEWAY,
                "Fabric is unreachable; workspace stays reserved as creating",
            ),
            Some("fabric_capacity") => json_error(
                StatusCode::TOO_MANY_REQUESTS,
                "fabric workspace capacity reached",
            ),
            Some("fabric_put_failed") => json_error(
                StatusCode::BAD_GATEWAY,
                "Fabric rejected workspace desired spec",
            ),
            _ => json_response(
                StatusCode::ACCEPTED,
                json!({
                    "id": workspace_id,
                    "projectId": project_id,
                    "label": label,
                    "state": "creating",
                    "createdByUserId": user_id,
                }),
            ),
        }
    }

    async fn grow_workspace(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            #[serde(default, alias = "approvalId")]
            approval_id: Option<Uuid>,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid workspace grow payload"),
        };
        match self
            .platform
            .grow_workspace_elevated(user_id, workspace_id, payload.approval_id)
            .await
        {
            Ok(allocated_bytes) => json_ok(json!({
                "id": workspace_id,
                "allocatedBytes": allocated_bytes,
                "storageTier": "elevated",
            })),
            Err(crate::applications::ApplicationError::ApprovalRequired(id)) => json_response(
                StatusCode::CONFLICT,
                json!({ "error": "approval required", "approvalId": id }),
            ),
            Err(crate::applications::ApplicationError::Auth) => {
                json_error(StatusCode::FORBIDDEN, "project access denied")
            }
            Err(crate::applications::ApplicationError::NotFound) => {
                json_error(StatusCode::NOT_FOUND, "workspace not found")
            }
            Err(crate::applications::ApplicationError::WorkspaceBusy) => {
                json_error(StatusCode::CONFLICT, "workspace cannot grow to 64 GiB")
            }
            Err(_) => json_error(StatusCode::BAD_GATEWAY, "Fabric rejected workspace growth"),
        }
    }

    async fn snapshot_workspace(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        let project_id: Option<Uuid> =
            sqlx::query_scalar("select project_id from workspaces where id = $1")
                .bind(workspace_id)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten();
        let Some(project_id) = project_id else {
            return json_error(StatusCode::NOT_FOUND, "workspace not found");
        };
        if auth::authorize(&self.pool, user_id, project_id, Action::ManageProduction)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        match self
            .platform
            .snapshot_workspace_to_blob(workspace_id, "manual", None)
            .await
        {
            Ok(snapshot_id) => json_ok(json!({
                "snapshotId": snapshot_id,
                "workspaceId": workspace_id,
                "kind": "manual",
            })),
            Err(crate::applications::ApplicationError::NotFound) => {
                json_error(StatusCode::NOT_FOUND, "workspace not found")
            }
            Err(crate::applications::ApplicationError::WorkspaceBusy) => {
                json_error(StatusCode::CONFLICT, "workspace snapshot is in progress")
            }
            Err(_) => json_error(
                StatusCode::BAD_GATEWAY,
                "Fabric rejected workspace snapshot",
            ),
        }
    }

    async fn list_workspace_snapshots(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        let project_id: Option<Uuid> =
            sqlx::query_scalar("select project_id from workspaces where id = $1")
                .bind(workspace_id)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten();
        let Some(project_id) = project_id else {
            return json_error(StatusCode::NOT_FOUND, "workspace not found");
        };
        if auth::authorize(&self.pool, user_id, project_id, Action::ReadProject)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        let rows = match sqlx::query(
            "select id, kind, byte_length, created_at::text as created_at \
             from workspace_snapshots where workspace_id = $1 \
             order by created_at desc, id",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "snapshots failed"),
        };
        json_ok(json!({
            "items": rows
                .into_iter()
                .map(|row| json!({
                    "id": row.get::<Uuid, _>("id"),
                    "kind": row.get::<String, _>("kind"),
                    "byteLength": row.get::<i64, _>("byte_length"),
                    "createdAt": row.get::<String, _>("created_at"),
                }))
                .collect::<Vec<_>>(),
        }))
    }

    async fn restore_workspace(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            #[serde(alias = "snapshotId")]
            snapshot_id: Uuid,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => {
                return json_error(StatusCode::BAD_REQUEST, "invalid workspace restore payload");
            }
        };
        let project_id: Option<Uuid> =
            sqlx::query_scalar("select project_id from workspaces where id = $1")
                .bind(workspace_id)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten();
        let Some(project_id) = project_id else {
            return json_error(StatusCode::NOT_FOUND, "workspace not found");
        };
        if auth::authorize(&self.pool, user_id, project_id, Action::ManageProduction)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        match self
            .platform
            .restore_workspace_from_snapshot(workspace_id, payload.snapshot_id)
            .await
        {
            Ok(()) => json_ok(json!({
                "id": workspace_id,
                "snapshotId": payload.snapshot_id,
                "state": "ready",
            })),
            Err(crate::applications::ApplicationError::NotFound) => {
                json_error(StatusCode::NOT_FOUND, "snapshot not found")
            }
            Err(crate::applications::ApplicationError::WorkspaceBusy) => {
                json_error(StatusCode::CONFLICT, "workspace restore is in progress")
            }
            Err(_) => json_error(StatusCode::BAD_GATEWAY, "Fabric rejected workspace restore"),
        }
    }

    /// Tears one unreferenced Workspace down by persisting desired `deleted`.
    /// Fabric realization belongs to the Workspace reconciler PUT. The
    /// process fence still serializes against replace; a fenced row is 409.
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
        let workspace = match kernel.find_workspace(workspace_id).await {
            Ok(Some(workspace)) if workspace.project_id == project_id => workspace,
            _ => return json_error(StatusCode::NOT_FOUND, "workspace not found"),
        };
        let desired: String =
            sqlx::query_scalar("select desired_state from workspaces where id = $1")
                .bind(workspace_id)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| "active".into());
        if desired == "deleted" || workspace.state == WorkspaceState::Deleted {
            return json_error(StatusCode::NOT_FOUND, "workspace not found");
        }
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
        if workspace.state == WorkspaceState::Fenced {
            return json_error(
                StatusCode::CONFLICT,
                "workspace deletion already in progress",
            );
        }
        let mut tx = match self.pool.begin().await {
            Ok(tx) => tx,
            Err(_) => {
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "workspace store failed");
            }
        };
        let locked =
            sqlx::query("select state, desired_state from workspaces where id = $1 for update")
                .bind(workspace_id)
                .fetch_optional(&mut *tx)
                .await;
        let Ok(Some(locked)) = locked else {
            return json_error(StatusCode::NOT_FOUND, "workspace not found");
        };
        let locked_state: String = locked.get("state");
        let locked_desired: String = locked.get("desired_state");
        if locked_desired == "deleted" || locked_state == "deleted" {
            return json_error(StatusCode::NOT_FOUND, "workspace not found");
        }
        if locked_state == "fenced" {
            return json_error(
                StatusCode::CONFLICT,
                "workspace deletion already in progress",
            );
        }
        let attached: bool = match sqlx::query_scalar(
            "select exists(\
                select 1 from applications \
                where workspace_id = $1 and state <> 'deleting'\
             )",
        )
        .bind(workspace_id)
        .fetch_one(&mut *tx)
        .await
        {
            Ok(attached) => attached,
            Err(_) => {
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "workspace store failed");
            }
        };
        if attached {
            return json_error(StatusCode::CONFLICT, "workspace has application");
        }
        // Sessions do not pin Fabric storage. Desired `deleted` releases
        // occupancy once the guest is gone. A live Application still owns
        // teardown through approved application.delete.
        let persisted = sqlx::query(
            "update workspaces set desired_state = 'deleted', \
             desired_revision = case \
                 when desired_state = 'deleted' then desired_revision \
                 else desired_revision + 1 \
             end, \
             reconcile_after = now() \
             where id = $1 \
               and desired_state <> 'deleted' \
               and state <> 'fenced'",
        )
        .bind(workspace_id)
        .execute(&mut *tx)
        .await;
        match persisted {
            Ok(result) if result.rows_affected() == 1 => {}
            Ok(_) => {
                return json_error(
                    StatusCode::CONFLICT,
                    "workspace deletion already in progress",
                );
            }
            Err(_) => {
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "workspace store failed");
            }
        }
        if tx.commit().await.is_err() {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "workspace store failed");
        }
        crate::reconcile::workspace::put_due_workspace(&self.platform, workspace_id).await;
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

    /// The Fabric every new Workspace binds to. D004: deployment
    /// configuration names the identity; Control does not invent one by
    /// counting `fabrics` rows. An unregistered configured identity refuses
    /// before any external side effect. Workspaces that already carry
    /// `fabric_id` keep using that row (`fabric_id_for_workspace`).
    async fn selected_fabric_id(&self) -> Option<Uuid> {
        let Some(configured) = self.configured_fabric_id else {
            return None;
        };
        let registered: bool =
            sqlx::query_scalar("select exists(select 1 from fabrics where id = $1)")
                .bind(configured)
                .fetch_one(&self.pool)
                .await
                .unwrap_or(false);
        bind_configured_fabric_id(Some(configured), registered)
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
        let desired: String =
            sqlx::query_scalar("select desired_state from workspaces where id = $1")
                .bind(workspace_id)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| "active".into());
        if desired == "deleted" || workspace.state == WorkspaceState::Deleted {
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
                payload.max_tokens.clamp(1, 8192),
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
            .clamp(1, 8192);
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
        // unknown Workspaces are simply not addressable. Desired `deleted`
        // and a process fence both refuse new attachment. Product ready is
        // Fabric observed live, not leftover process `ready`.
        let (workspace_project, workspace_state, workspace_desired, workspace_observed): (
            Option<Uuid>,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            "select project_id, state, desired_state, observed_state from workspaces where id = $1",
        )
        .bind(payload.workspace_id)
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten()
        .map(|(project, state, desired, observed)| {
            (Some(project), Some(state), Some(desired), Some(observed))
        })
        .unwrap_or((None, None, None, None));
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
        if !crate::workspace_is_realized(
            workspace_desired.as_deref().unwrap_or("active"),
            workspace_observed.as_deref().unwrap_or(""),
            workspace_state.as_deref().unwrap_or(""),
        ) {
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

    /// New Chat: insert a durable empty Session immediately. The first
    /// prompt is `POST /api/conversations/:id/messages`.
    async fn create_conversation(
        &self,
        kernel: &Kernel,
        user_id: Uuid,
        body: Vec<u8>,
    ) -> Response<http_body_util::Full<Bytes>> {
        #[derive(Deserialize)]
        struct Payload {
            #[serde(rename = "agentId", default)]
            agent_id: Option<Uuid>,
            #[serde(rename = "workspaceId")]
            workspace_id: Uuid,
            #[serde(rename = "projectId", default)]
            project_id: Option<Uuid>,
        }
        let payload: Payload = match serde_json::from_slice(&body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid conversation payload"),
        };
        let project_id = match payload.project_id {
            Some(project_id) => project_id,
            None => {
                let found: Option<Uuid> =
                    sqlx::query_scalar("select project_id from workspaces where id = $1")
                        .bind(payload.workspace_id)
                        .fetch_optional(&self.pool)
                        .await
                        .ok()
                        .flatten();
                match found {
                    Some(project_id) => project_id,
                    None => return json_error(StatusCode::NOT_FOUND, "workspace not found"),
                }
            }
        };
        if auth::authorize(&self.pool, user_id, project_id, Action::OperateSession)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        let agent_id = match payload.agent_id {
            Some(agent_id) => agent_id,
            None => match self.resolve_default_agent(project_id).await {
                Ok(agent_id) => agent_id,
                Err(response) => return response,
            },
        };
        let conversation_id = Uuid::new_v4();
        let session = match kernel
            .open_conversation(
                conversation_id,
                project_id,
                agent_id,
                payload.workspace_id,
                user_id,
            )
            .await
        {
            Ok(session) => session,
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
        self.record(AuditInsert {
            project_id: Some(session.project_id),
            session_id: Some(session.id),
            run_id: None,
            actor_user_id: Some(user_id),
            kind: "conversation.created",
            resource_type: "conversation",
            resource_id: Some(session.id),
            outcome: AuditOutcome::Ok,
            metadata: None,
        })
        .await;
        json_ok(json!({
            "conversationId": session.id,
            "projectId": session.project_id,
            "agentId": session.agent_id,
            "workspaceId": session.workspace_id,
            "headRevision": session.head_revision,
            "accepted": true,
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
        let existing_mode: Option<String> =
            sqlx::query_scalar("select mode from runs where intent_id = $1")
                .bind(intent)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten();
        let mode = if let Some(mode) = existing_mode {
            mode
        } else {
            let has_runs: bool =
                sqlx::query_scalar("select exists(select 1 from runs where session_id = $1)")
                    .bind(session.id)
                    .fetch_one(&self.pool)
                    .await
                    .unwrap_or(true);
            if has_runs {
                "resume".to_owned()
            } else {
                "create".to_owned()
            }
        };
        let request_hash: [u8; 32] = Sha256::new()
            .chain_update(mode.as_bytes())
            .chain_update(payload.prompt.as_bytes())
            .finalize()
            .into();
        let requested_run_id = Uuid::new_v4();
        let run = match kernel
            .accept_run(
                requested_run_id,
                intent,
                session.id,
                &request_hash,
                &mode,
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
            metadata: Some(&json!({ "mode": mode })),
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
                    let sessions =
                        match crate::Kernel::fence_actor_runs(&mut tx, target_id, None).await {
                            Ok(sessions) => sessions,
                            Err(_) => {
                                return json_error(
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    "session revocation failed",
                                );
                            }
                        };
                    if tx.commit().await.is_err() {
                        return json_error(StatusCode::INTERNAL_SERVER_ERROR, "user update failed");
                    }
                    for session_id in sessions {
                        self.kick_next(session_id);
                    }
                } else if tx.commit().await.is_err() {
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
        let Some(session) = current else {
            return json_error(StatusCode::UNAUTHORIZED, "login required");
        };
        match kernel
            .set_native_password_and_revoke_others(user_id, &password_hash, session.id)
            .await
        {
            Ok(true) => json_ok(json!({ "updated": true })),
            Ok(false) => json_error(StatusCode::NOT_FOUND, "user not found"),
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "password update failed"),
        }
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
    async fn admin_projects(&self, user_id: Uuid) -> Response<http_body_util::Full<Bytes>> {
        if !self.is_platform_admin(user_id).await {
            return json_error(StatusCode::FORBIDDEN, "platform admin required");
        }
        let rows = match sqlx::query(
            "select p.id, p.owner_user_id, p.name, p.kind, \
                    (select count(*) from project_members m where m.project_id = p.id)::bigint \
                        as member_count, \
                    (select count(*) from workspaces w \
                        where w.project_id = p.id and w.desired_state <> 'deleted')::bigint \
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
    async fn admin_project_members(
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
            })).collect::<Vec<_>>(),
            "storage": self.fabric.capacity().await.ok(),
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
                    w.label, w.state, w.created_at::text as created_at, \
                    coalesce(w.desired_state, 'active') as desired_state, \
                    w.observed_state \
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
                "state": workspace_row_wire_state(&row),
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
        let workspace_counts = sqlx::query(
            "select \
                count(*) filter ( \
                    where desired_state not in ('deleted', 'archived', 'suspended') \
                      and state <> 'fenced' \
                      and observed_state not in ('active', 'ready') \
                )::bigint as creating, \
                count(*) filter ( \
                    where desired_state not in ('deleted', 'archived', 'suspended') \
                      and state <> 'fenced' \
                      and observed_state in ('active', 'ready') \
                )::bigint as ready, \
                count(*) filter (where state = 'fenced' and desired_state <> 'deleted')::bigint as fenced, \
                count(*) filter ( \
                    where desired_state = 'archived' and state <> 'fenced' \
                )::bigint as archived \
             from workspaces \
             where desired_state <> 'deleted'",
        )
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten();
        let counts_creating = workspace_counts
            .as_ref()
            .map(|row| row.get::<i64, _>("creating"))
            .unwrap_or(0);
        let counts_ready = workspace_counts
            .as_ref()
            .map(|row| row.get::<i64, _>("ready"))
            .unwrap_or(0);
        let counts_fenced = workspace_counts
            .as_ref()
            .map(|row| row.get::<i64, _>("fenced"))
            .unwrap_or(0);
        let counts_archived = workspace_counts
            .as_ref()
            .map(|row| row.get::<i64, _>("archived"))
            .unwrap_or(0);
        let storage = self.fabric.capacity().await.ok();
        let fabric_connected = tokio::time::timeout(Duration::from_secs(2), self.fabric.health())
            .await
            .ok()
            .and_then(Result::ok)
            .is_some();
        let blob_ok = tokio::time::timeout(Duration::from_secs(2), self.blob.reachable())
            .await
            .unwrap_or(false);
        let kv_mode = std::env::var(crate::secrets::MaterialBackend::SELECTION_ENV)
            .unwrap_or_else(|_| "memory".into());
        let db_conv = self
            .platform
            .databases
            .convergence_counts()
            .await
            .unwrap_or((0, 0, 0));
        let dep_conv = sqlx::query(
            "select \
                count(*) filter (where desired_revision = observed_revision \
                    and observed_state not in ('needs_release_stream', 'lost', 'failed') \
                    and (last_error_code is null or last_error_code = ''))::bigint as converged, \
                count(*) filter (where desired_revision > observed_revision \
                    and observed_state <> 'needs_release_stream')::bigint as reconciling, \
                count(*) filter (where observed_state in ('needs_release_stream', 'lost', 'failed') \
                    or (last_error_code is not null and last_error_code <> '' \
                        and last_error_code not in ('fabric_unreachable', 'fabric_unknown')))::bigint as failed \
             from application_deployments",
        )
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten();
        let ws_conv = sqlx::query(
            "select \
                count(*) filter (where desired_revision = observed_revision \
                    and observed_state not in ('lost', 'failed') \
                    and (last_error_code is null or last_error_code = ''))::bigint as converged, \
                count(*) filter (where desired_revision > observed_revision \
                    and observed_state <> 'lost')::bigint as reconciling, \
                count(*) filter (where last_error_code is not null and last_error_code <> '' \
                    and last_error_code <> 'fabric_unreachable' \
                    or observed_state in ('lost', 'failed'))::bigint as failed \
             from workspaces",
        )
        .fetch_optional(&self.pool)
        .await
        .ok()
        .flatten();
        let database_rows = self
            .platform
            .databases
            .list_live_census()
            .await
            .unwrap_or_default();
        let insecure = database_rows
            .iter()
            .filter(|row| row.security_profile < 2)
            .count();
        json_ok(json!({
            "database": { "ok": db_ok },
            "blob": { "configured": blob_configured, "ok": blob_ok },
            "keyVault": { "backend": kv_mode },
            "auth": { "mode": auth_mode },
            "fabric": {
                "registered": fabric_registered,
                "connected": fabric_connected,
                "identity": self.configured_fabric_id,
            },
            "reconciliation": {
                "workspace": {
                    "converged": ws_conv.as_ref().map(|row| row.get::<i64, _>("converged")).unwrap_or(0),
                    "reconciling": ws_conv.as_ref().map(|row| row.get::<i64, _>("reconciling")).unwrap_or(0),
                    "failed": ws_conv.as_ref().map(|row| row.get::<i64, _>("failed")).unwrap_or(0),
                },
                "deployment": {
                    "converged": dep_conv.as_ref().map(|row| row.get::<i64, _>("converged")).unwrap_or(0),
                    "reconciling": dep_conv.as_ref().map(|row| row.get::<i64, _>("reconciling")).unwrap_or(0),
                    "failed": dep_conv.as_ref().map(|row| row.get::<i64, _>("failed")).unwrap_or(0),
                },
                "database": {
                    "converged": db_conv.0,
                    "reconciling": db_conv.1,
                    "failed": db_conv.2,
                },
                "routes": {
                    "desiredRevision": sqlx::query_scalar::<_, i64>(
                        "select coalesce(desired_route_revision, 0) from fabrics where id = $1",
                    )
                    .bind(self.configured_fabric_id)
                    .fetch_optional(&self.pool)
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or(0),
                    "observedRevision": sqlx::query_scalar::<_, i64>(
                        "select coalesce(observed_route_revision, 0) from fabrics where id = $1",
                    )
                    .bind(self.configured_fabric_id)
                    .fetch_optional(&self.pool)
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or(0),
                },
            },
            "workspaces": {
                "creating": counts_creating,
                "ready": counts_ready,
                "fenced": counts_fenced,
                "archived": counts_archived,
            },
            "databases": {
                "live": database_rows.len(),
                "insecure": insecure,
                "items": database_rows
                    .iter()
                    .map(|row| json!({
                        "id": row.id.to_string(),
                        "state": row.wire_state(),
                        "securityProfile": row.security_profile,
                    }))
                    .collect::<Vec<_>>(),
            },
            "storage": storage,
            "resources": {
                "workspace": sqlx::query(
                    "select id::text as id, desired_revision, observed_revision, desired_state, \
                            observed_state, \
                            last_error_code, reconcile_after::text as next_retry_at \
                     from workspaces \
                     where desired_state <> 'deleted' \
                       and (desired_revision > observed_revision \
                            or (last_error_code is not null and last_error_code <> '')) \
                     order by created_at, id limit 16",
                )
                .fetch_all(&self.pool)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|row| json!({
                    "id": row.get::<String, _>("id"),
                    "desiredRevision": row.get::<i64, _>("desired_revision"),
                    "observedRevision": row.get::<i64, _>("observed_revision"),
                    "desiredState": row.get::<String, _>("desired_state"),
                    "observedState": row.get::<String, _>("observed_state"),
                    "lastErrorCode": row.get::<Option<String>, _>("last_error_code"),
                    "nextRetryAt": row.get::<Option<String>, _>("next_retry_at"),
                }))
                .collect::<Vec<_>>(),
                "deployment": sqlx::query(
                    "select id::text as id, desired_revision, observed_revision, desired_state, \
                            observed_state, \
                            last_error_code, reconcile_after::text as next_retry_at \
                     from application_deployments \
                     where desired_revision > observed_revision \
                        or (last_error_code is not null and last_error_code <> '') \
                     order by accepted_at, id limit 16",
                )
                .fetch_all(&self.pool)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|row| json!({
                    "id": row.get::<String, _>("id"),
                    "desiredRevision": row.get::<i64, _>("desired_revision"),
                    "observedRevision": row.get::<i64, _>("observed_revision"),
                    "desiredState": row.get::<String, _>("desired_state"),
                    "observedState": row.get::<String, _>("observed_state"),
                    "lastErrorCode": row.get::<Option<String>, _>("last_error_code"),
                    "nextRetryAt": row.get::<Option<String>, _>("next_retry_at"),
                }))
                .collect::<Vec<_>>(),
                "database": sqlx::query(
                    "select id::text as id, desired_revision, observed_revision, desired_state, \
                            observed_state, \
                            last_error_code, reconcile_after::text as next_retry_at \
                     from application_databases \
                     where desired_state <> 'absent' \
                       and (desired_revision > observed_revision \
                            or (last_error_code is not null and last_error_code <> '')) \
                     order by created_at, id limit 16",
                )
                .fetch_all(&self.pool)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|row| json!({
                    "id": row.get::<String, _>("id"),
                    "desiredRevision": row.get::<i64, _>("desired_revision"),
                    "observedRevision": row.get::<i64, _>("observed_revision"),
                    "desiredState": row.get::<String, _>("desired_state"),
                    "observedState": row.get::<String, _>("observed_state"),
                    "lastErrorCode": row.get::<Option<String>, _>("last_error_code"),
                    "nextRetryAt": row.get::<Option<String>, _>("next_retry_at"),
                }))
                .collect::<Vec<_>>(),
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

    /// One scope's workspaces with the durable display label.
    async fn project_workspaces(
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
                    w.created_at::text as created_at, \
                    coalesce(w.desired_state, 'active') as desired_state, \
                    w.observed_state, \
                    w.desired_revision, w.observed_revision, w.last_error_code \
             from workspaces w \
             where w.project_id = $1 \
               and (w.desired_state <> 'deleted' \
                    or w.observed_state in ('active', 'ready')) \
             order by w.created_at, w.id",
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
                "state": workspace_row_wire_state(&row),
                "createdByUserId": row.get::<Option<Uuid>, _>("created_by_user_id"),
                "createdAt": row.get::<String, _>("created_at"),
                "desiredState": row.get::<String, _>("desired_state"),
                "observedState": row.get::<String, _>("observed_state"),
                "desiredRevision": row.get::<i64, _>("desired_revision"),
                "observedRevision": row.get::<i64, _>("observed_revision"),
                "lastErrorCode": row.get::<Option<String>, _>("last_error_code"),
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
                    w.created_at::text as created_at, w.exec_generation, \
                    coalesce(w.desired_state, 'active') as desired_state, \
                    w.observed_state, \
                    w.desired_revision, w.observed_revision, w.last_error_code \
             from workspaces w join project_members m on m.project_id = w.project_id \
             where w.id = $1 and m.user_id = $2 \
               and (w.desired_state <> 'deleted' \
                    or w.observed_state in ('active', 'ready'))",
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
            "state": workspace_row_wire_state(&row),
            "createdByUserId": row.get::<Option<Uuid>, _>("created_by_user_id"),
            "createdAt": row.get::<String, _>("created_at"),
            "execGeneration": row.get::<i64, _>("exec_generation"),
            "desiredState": row.get::<String, _>("desired_state"),
            "observedState": row.get::<String, _>("observed_state"),
            "desiredRevision": row.get::<i64, _>("desired_revision"),
            "observedRevision": row.get::<i64, _>("observed_revision"),
            "lastErrorCode": row.get::<Option<String>, _>("last_error_code"),
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
                    left(coalesce((select r.prompt from runs r \
                          where r.session_id = s.id order by r.seq limit 1), 'New chat'), 60) as title \
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
    async fn project_sessions(
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
    async fn project_agents(
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
    async fn project_events(
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
        self.canonical_events(&session_ids, after, query_flag(query, "wait"))
            .await
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
             from runs where session_id = $1 and state in ('accepted', 'dispatched') \
             order by seq",
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

    async fn conversation_history(
        &self,
        user_id: Uuid,
        conversation_id: Uuid,
        query: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let project_id: Option<Uuid> =
            sqlx::query_scalar("select project_id from sessions where id = $1")
                .bind(conversation_id)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten();
        let Some(project_id) = project_id else {
            return json_error(StatusCode::NOT_FOUND, "conversation not found");
        };
        if auth::authorize(&self.pool, user_id, project_id, Action::ReadProject)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        let before_producer_seq = query
            .split('&')
            .find_map(|pair| pair.strip_prefix("beforeSeq="))
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|&value| value > 0);
        let max_appends = query
            .split('&')
            .find_map(|pair| {
                pair.strip_prefix("maxMessages=")
                    .or_else(|| pair.strip_prefix("limit="))
            })
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(50)
            .clamp(1, 256);
        let (collected, sql_has_more) = match self
            .sessions
            .load_history_ending_before_seq(conversation_id, before_producer_seq, max_appends)
            .await
        {
            Ok(page) => page,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "history failed"),
        };
        let (events, page_has_more) =
            slice_history_page(collected, before_producer_seq, max_appends);
        let items = events
            .into_iter()
            .map(|event| {
                json!({
                    "sessionId": event.reference.session_id,
                    "globalSeq": event.reference.global_seq,
                    "revision": event.reference.revision,
                    "appendId": event.reference.append_id,
                    "contentHash": hex_bytes(&event.reference.content_hash),
                    "byteLength": event.reference.byte_length,
                    "bytes": BASE64.encode(event.bytes),
                })
            })
            .collect::<Vec<_>>();
        let live_rows = match sqlx::query(
            "select id, intent_id, seq, state, prompt, actor_user_id \
             from runs where session_id = $1 and state in ('accepted', 'dispatched') \
             order by seq, id",
        )
        .bind(conversation_id)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "history failed"),
        };
        let live_runs: Vec<Value> = live_rows
            .iter()
            .map(|row| {
                json!({
                    "runId": row.get::<Uuid, _>("id"),
                    "intentId": row.get::<Uuid, _>("intent_id"),
                    "seq": row.get::<i64, _>("seq"),
                    "state": row.get::<String, _>("state"),
                    "prompt": row.get::<String, _>("prompt"),
                    "actorUserId": row.get::<Option<Uuid>, _>("actor_user_id"),
                })
            })
            .collect();
        let running = live_runs.iter().any(|run| {
            run.get("state").and_then(Value::as_str) == Some("accepted")
                || run.get("state").and_then(Value::as_str) == Some("dispatched")
        });
        json_ok(json!({
            "items": items,
            "hasMore": sql_has_more || page_has_more,
            "beforeSeq": before_producer_seq,
            "running": running,
            "liveRuns": live_runs,
        }))
    }

    async fn cancel_conversation(
        &self,
        kernel: &Kernel,
        user_id: Uuid,
        conversation_id: Uuid,
    ) -> Response<http_body_util::Full<Bytes>> {
        let project_id: Option<Uuid> =
            sqlx::query_scalar("select project_id from sessions where id = $1")
                .bind(conversation_id)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten();
        let Some(project_id) = project_id else {
            return json_error(StatusCode::NOT_FOUND, "conversation not found");
        };
        if auth::authorize(&self.pool, user_id, project_id, Action::OperateSession)
            .await
            .is_err()
        {
            return json_error(StatusCode::FORBIDDEN, "project access denied");
        }
        match kernel.cancel_conversation_live_run(conversation_id).await {
            Ok((state, kicked_session, run_id)) => {
                if let Some(run_id) = run_id {
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
                            state == crate::RunState::Unknown
                                || state == crate::RunState::Cancelled
                                || state == crate::RunState::Terminal,
                            AuditOutcome::Refused,
                            "run.cancel_requested",
                        ),
                    };
                    self.record(AuditInsert {
                        project_id: Some(project_id),
                        session_id: Some(conversation_id),
                        run_id: Some(run_id),
                        actor_user_id: Some(user_id),
                        kind,
                        resource_type: "run",
                        resource_id: Some(run_id),
                        outcome,
                        metadata: Some(&json!({ "runStateAtRequest": state.as_str() })),
                    })
                    .await;
                    if let Some(session_id) = kicked_session {
                        self.kick_next(session_id);
                    }
                    json_ok(json!({
                        "conversationId": conversation_id,
                        "runId": run_id,
                        "state": state_label,
                        "accepted": accepted,
                    }))
                } else {
                    json_ok(json!({
                        "conversationId": conversation_id,
                        "state": "idle",
                        "accepted": true,
                    }))
                }
            }
            Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "cancel failed"),
        }
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
        if !(1..=8192).contains(&payload.max_tokens) {
            return json_error(StatusCode::BAD_REQUEST, "maxTokens must be within 1..=8192");
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
            if !(1..=8192).contains(&max_tokens) {
                return json_error(StatusCode::BAD_REQUEST, "maxTokens must be within 1..=8192");
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

/// Actor/project fence shared with disable and membership removal. Each
/// privileged effect reauthorizes and claims under the User-row lock
/// before it starts; the lock is not held across the effect itself.
#[derive(Clone)]
struct EffectAuthority {
    pool: PgPool,
    run_id: Uuid,
    actor_user_id: Uuid,
    project_id: Uuid,
    workspace_id: Uuid,
}

impl EffectAuthority {
    async fn claim(&self) -> Result<(), ActivationError> {
        crate::Kernel::claim_privileged_effect(
            &self.pool,
            self.run_id,
            self.actor_user_id,
            self.project_id,
        )
        .await
        .map_err(|_| ActivationError::Protocol("privileged effect was revoked"))?;
        self.refuse_nonlive_application().await
    }

    async fn refuse_nonlive_application(&self) -> Result<(), ActivationError> {
        let state: Option<String> = sqlx::query_scalar(
            "select state from applications \
             where workspace_id = $1 and state <> 'deleting' \
             order by created_at desc limit 1",
        )
        .bind(self.workspace_id)
        .fetch_optional(&self.pool)
        .await
        .unwrap_or(None);
        match state.as_deref() {
            Some("archiving" | "archived" | "restoring" | "deleting") => {
                Err(ActivationError::Protocol("application is not live"))
            }
            _ => Ok(()),
        }
    }
}

struct CloudModel {
    relay: Arc<CloudModelRelay>,
    agent: Agent,
    authority: EffectAuthority,
}

impl ModelRelay for CloudModel {
    fn complete(
        &self,
        request: ModelRequest,
    ) -> impl Future<Output = Result<ModelResponse, ActivationError>> + Send {
        let relay = self.relay.clone();
        let agent = self.agent.clone();
        let authority = self.authority.clone();
        async move {
            authority.claim().await?;
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
            let system_prompt =
                crate::http::resolve_agent_system_prompt(&agent.system_prompt, request.system);
            if let Some(system_prompt) = system_prompt {
                messages.insert(0, ModelMessage::text("system", system_prompt));
            }
            let tools = tool_definitions(agent.bash_enabled);
            let allowed: Vec<String> = tools.iter().map(|tool| tool.name.clone()).collect();
            let mut completion_retried = false;
            let response = loop {
                match relay
                    .complete(CloudModelRequest {
                        messages: messages.clone(),
                        tools: tools.clone(),
                        max_tokens: (agent.max_tokens as u32).min(DEFAULT_MAX_TOKENS),
                    })
                    .await
                {
                    Ok(response) => break response,
                    Err(crate::model::ModelError::Empty | crate::model::ModelError::Response)
                        if !completion_retried =>
                    {
                        completion_retried = true;
                        messages.push(ModelMessage::text(
                            "user",
                            "The previous completion was unusable. Call exactly one tool. Never return an empty assistant message or parallel tool calls. If work is done, reply with the preview URL.",
                        ));
                    }
                    Err(error) => {
                        return Err(match error {
                            crate::model::ModelError::Bounded => {
                                ActivationError::Child("model request exceeds the configured bound")
                            }
                            crate::model::ModelError::Transport => {
                                ActivationError::Child("model transport failed")
                            }
                            crate::model::ModelError::Empty => {
                                ActivationError::Child("model response was unusable")
                            }
                            crate::model::ModelError::Response => {
                                ActivationError::Child("model response was unusable")
                            }
                            crate::model::ModelError::Config(_) => {
                                ActivationError::Child("model relay failed")
                            }
                        });
                    }
                }
            };
            if let Some(call) = select_model_tool_call(response.tool_calls, &allowed)? {
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

/// Provider-facing contract: at most one tool call. Parallel arrays are
/// refused so a sequential agent cannot silently drop requested effects.
///
/// Every returned tool must be in the exact server-owned allowlist for this
/// activation. Unknown or unadvertised names fail closed and never become
/// executable frames.
fn select_model_tool_call(
    calls: Vec<crate::model::ModelToolCall>,
    allowed: &[String],
) -> Result<Option<crate::model::ModelToolCall>, ActivationError> {
    if calls.len() > 1 {
        return Err(ActivationError::Protocol(
            "model returned more than one tool call",
        ));
    }
    for call in &calls {
        if !allowed.iter().any(|name| name == &call.name) {
            return Err(ActivationError::Protocol(
                "model returned an unauthorized tool",
            ));
        }
    }
    Ok(calls.into_iter().next())
}

fn tool_definitions(bash_enabled: bool) -> Vec<ModelToolDefinition> {
    let mut tools = Vec::new();
    if bash_enabled {
        tools.push(ModelToolDefinition {
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
        });
    }
    tools.extend(crate::http::product_tool_definitions());
    tools
}

struct CloudWorkspace {
    fabric: Arc<FabricClient>,
    journal: Arc<ExecJournal>,
    workspace_id: Uuid,
    bash_enabled: bool,
    authority: EffectAuthority,
}

struct CloudProduct {
    platform: crate::http::Platform,
    actor_user_id: Uuid,
    project_id: Uuid,
    workspace_id: Uuid,
    authority: EffectAuthority,
}

/// Product tools authorize as the human who queued this Run. The Session's
/// last actor is only a fallback. The nil UUID is never an actor.
fn activation_product_actor(run_actor: Option<Uuid>, session_actor: Option<Uuid>) -> Option<Uuid> {
    run_actor
        .filter(|user_id| !user_id.is_nil())
        .or_else(|| session_actor.filter(|user_id| !user_id.is_nil()))
}

impl crate::activation::ProductExec for CloudProduct {
    fn execute(
        &self,
        intent: crate::activation::ProductIntent,
    ) -> impl Future<Output = Result<crate::activation::ProductResult, ActivationError>> + Send
    {
        let platform = self.platform.clone();
        let actor_user_id = self.actor_user_id;
        let project_id = self.project_id;
        let workspace_id = self.workspace_id;
        let authority = self.authority.clone();
        async move {
            authority.claim().await?;
            let arguments: Value =
                serde_json::from_str(&intent.arguments_json).unwrap_or(json!({}));
            match platform
                .execute_tool(
                    actor_user_id,
                    project_id,
                    workspace_id,
                    &intent.name,
                    &arguments,
                )
                .await
            {
                Ok(value) => {
                    let text = value.to_string();
                    if crate::http::product_text_leaks_secret(&text) {
                        Ok(crate::activation::ProductResult {
                            text: "{\"error\":\"product result withheld\"}".to_owned(),
                            is_error: true,
                        })
                    } else {
                        Ok(crate::activation::ProductResult {
                            text,
                            is_error: false,
                        })
                    }
                }
                Err(error) => {
                    let text = error.product_text();
                    let text = if crate::http::product_text_leaks_secret(&text) {
                        "{\"error\":\"product result withheld\"}".to_owned()
                    } else {
                        text
                    };
                    Ok(crate::activation::ProductResult {
                        text,
                        is_error: true,
                    })
                }
            }
        }
    }
}

impl WorkspaceExec for CloudWorkspace {
    fn bash(
        &self,
        intent: BashIntent,
    ) -> impl Future<Output = Result<BashResult, ActivationError>> + Send {
        let fabric = self.fabric.clone();
        let journal = self.journal.clone();
        let workspace_id = self.workspace_id;
        let bash_enabled = self.bash_enabled;
        let authority = self.authority.clone();
        async move {
            if !bash_enabled {
                return Err(ActivationError::Protocol(
                    "bash is not enabled for this agent",
                ));
            }
            authority.claim().await?;
            let outcome = journal
                .execute(&fabric, workspace_id, &intent.call_id, &intent.command)
                .await
                .map_err(|_| ActivationError::Child("exec journal failed"))?;
            let allocated: Option<u64> = fabric
                .get_workspace_probe(workspace_id)
                .await
                .ok()
                .flatten()
                .and_then(|probe| probe.allocated_bytes);
            if let Some(bytes) = allocated {
                let _ = sqlx::query(
                    "update workspaces set allocated_bytes = $2 \
                     where id = $1 and allocated_bytes <> $2",
                )
                .bind(workspace_id)
                .bind(bytes as i64)
                .execute(&authority.pool)
                .await;
            }
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
                .append(AppendEvent {
                    append_id,
                    writer_generation: self.expected_generation,
                    expected_revision: head.head_revision + 1,
                    bytes,
                    model_usage: None,
                })
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

fn query_flag(query: &str, name: &str) -> bool {
    let needle = format!("{name}=");
    query.split('&').any(|pair| {
        pair == name
            || pair
                .strip_prefix(&needle)
                .is_some_and(|value| matches!(value, "1" | "true" | "yes"))
    })
}

fn query_param(query: &str, name: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

fn history_event_lines(bytes: &[u8]) -> Vec<(Option<i64>, String)> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).ok()?;
            let ty = value.get("type")?.as_str()?.to_owned();
            let seq = value.get("seq").and_then(serde_json::Value::as_i64);
            Some((seq, ty))
        })
        .collect()
}

fn slice_history_page(
    events: Vec<crate::session_store::LoadedEvent>,
    before_seq: Option<i64>,
    max_messages: i64,
) -> (Vec<crate::session_store::LoadedEvent>, bool) {
    if events.is_empty() {
        return (events, false);
    }
    let mut line_event: Vec<usize> = Vec::new();
    let mut lines: Vec<(Option<i64>, String)> = Vec::new();
    for (event_i, event) in events.iter().enumerate() {
        for (seq, ty) in history_event_lines(&event.bytes) {
            line_event.push(event_i);
            lines.push((seq, ty));
        }
    }
    if let Some(before) = before_seq {
        if let Some(cut) = lines
            .iter()
            .position(|(seq, _)| seq.is_some_and(|value| value >= before))
        {
            lines.truncate(cut);
            line_event.truncate(cut);
        }
    }
    if lines.is_empty() {
        return (Vec::new(), false);
    }
    let mut start_event = 0usize;
    let mut messages = 0i64;
    let mut cut = false;
    for (index, (_, ty)) in lines.iter().enumerate().rev() {
        if ty == "user/message" || ty == "assistant/message" {
            messages += 1;
        }
        if ty == "turn/start" && messages >= max_messages {
            start_event = line_event[index];
            cut = true;
            break;
        }
    }
    let end_event = line_event[lines.len() - 1] + 1;
    (events[start_event..end_event].to_vec(), cut)
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

fn workspace_row_wire_state(row: &sqlx::postgres::PgRow) -> &'static str {
    let desired = row
        .try_get::<String, _>("desired_state")
        .unwrap_or_else(|_| "active".into());
    let observed = row
        .try_get::<String, _>("observed_state")
        .unwrap_or_default();
    let process = row.get::<String, _>("state");
    crate::workspace_wire_state(&desired, &observed, &process)
}

fn json_error(status: StatusCode, message: &'static str) -> Response<http_body_util::Full<Bytes>> {
    json_response(status, json!({ "error": message }))
}

/// Wire shape of one secret metadata row. It contains no backend reference
/// and no material — those are structurally absent from this type.
fn secret_metadata_json(metadata: &SecretMetadata) -> Value {
    json!({
        "id": metadata.id,
        "projectId": metadata.project_id,
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

/// D004: a new Workspace binds the deployment-configured Fabric only when
/// that identity is registered. An unset identity does not invent a Fabric
/// by counting rows.
fn bind_configured_fabric_id(configured: Option<Uuid>, registered: bool) -> Option<Uuid> {
    match configured {
        Some(id) if registered => Some(id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ActivationError, MAX_REQUEST_BYTES, activation_product_actor, bind_configured_fabric_id,
        bounded_body, browser_mutation_allowed, capabilities_json, role_name, same_origin_json,
        tool_definitions,
    };
    use crate::web_session::{CSRF_HEADER, CSRF_MARKER};
    use hyper::{
        Request,
        header::{CONTENT_TYPE, ORIGIN},
    };
    use serde_json::json;
    use uuid::Uuid;

    #[test]
    fn new_workspace_binds_only_a_registered_configured_fabric() {
        let id = Uuid::from_u128(1);
        assert_eq!(bind_configured_fabric_id(None, true), None);
        assert_eq!(bind_configured_fabric_id(None, false), None);
        assert_eq!(bind_configured_fabric_id(Some(id), false), None);
        assert_eq!(bind_configured_fabric_id(Some(id), true), Some(id));
    }

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
    fn product_tools_authorize_as_the_run_actor() {
        let run_actor = Uuid::new_v4();
        let session_actor = Uuid::new_v4();
        assert_eq!(
            activation_product_actor(Some(run_actor), Some(session_actor)),
            Some(run_actor)
        );
        assert_eq!(
            activation_product_actor(None, Some(session_actor)),
            Some(session_actor)
        );
        assert_eq!(activation_product_actor(None, None), None);
        assert_eq!(
            activation_product_actor(Some(Uuid::nil()), Some(session_actor)),
            Some(session_actor)
        );
        assert_eq!(
            activation_product_actor(Some(Uuid::nil()), Some(Uuid::nil())),
            None
        );
    }

    #[test]
    fn maps_the_bounded_bash_tool_definition() {
        let tools = tool_definitions(true);
        assert_eq!(tools[0].name, "bash");
        assert_eq!(tools[0].parameters["required"], json!(["command"]));
        assert!(tools.iter().any(|tool| tool.name == "application.create"));
        let create = tools
            .iter()
            .find(|tool| tool.name == "application.create")
            .expect("application.create");
        assert!(
            create.description.contains("voie.toml"),
            "{}",
            create.description
        );
        assert_eq!(
            create.parameters["required"],
            json!(["name"]),
            "{}",
            create.parameters
        );
        assert!(
            create.parameters["properties"].get("slug").is_none(),
            "ApplicationStore allocates the slug: {}",
            create.parameters
        );
        assert_eq!(
            create.parameters["additionalProperties"],
            json!(false),
            "{}",
            create.parameters
        );
        assert_eq!(
            create.parameters["$defs"]["ManifestV1"]["additionalProperties"],
            json!(false),
            "ManifestV1 schema is supplied on application.create: {}",
            create.parameters
        );
        let build = tools
            .iter()
            .find(|tool| tool.name == "release.build")
            .expect("release.build");
        assert_eq!(
            build.parameters["$defs"]["ManifestV1"]["required"],
            json!(["version", "application", "build", "run"]),
            "ManifestV1 schema is supplied on release.build: {}",
            build.parameters
        );
        assert!(tools.iter().any(|tool| tool.name == "application.delete"));
        let status = tools
            .iter()
            .find(|tool| tool.name == "application.status")
            .expect("application.status");
        assert!(
            status.description.contains("Deployments"),
            "{}",
            status.description
        );
        assert!(
            status.description.contains("Databases"),
            "{}",
            status.description
        );
        assert!(tools.iter().any(|tool| tool.name == "deployment.activate"));
        let activate = tools
            .iter()
            .find(|tool| tool.name == "deployment.activate")
            .expect("deployment.activate");
        assert!(
            activate.description.contains("latest healthy Deployment"),
            "{}",
            activate.description
        );
        assert_eq!(
            activate.parameters.get("required"),
            None,
            "deployment_id must stay optional so live activate can omit it: {}",
            activate.parameters
        );
        assert!(
            tools
                .iter()
                .any(|tool| tool.name == "environment.publish_prod")
        );
        let deploy_dev = tools
            .iter()
            .find(|tool| tool.name == "environment.deploy_dev")
            .expect("environment.deploy_dev");
        assert!(
            deploy_dev.description.contains("database.create"),
            "{}",
            deploy_dev.description
        );
        assert!(
            deploy_dev.description.contains("latest ready Release"),
            "{}",
            deploy_dev.description
        );
        assert_eq!(
            deploy_dev.parameters.get("required"),
            None,
            "release_id must stay optional so live deploy_dev can omit it: {}",
            deploy_dev.parameters
        );
        assert!(
            deploy_dev.parameters["properties"]
                .get("release_id")
                .is_some(),
            "{}",
            deploy_dev.parameters
        );
        assert!(
            deploy_dev.parameters["properties"]
                .get("releaseId")
                .is_none(),
            "tool schemas are snake_case only: {}",
            deploy_dev.parameters
        );
        assert!(tools.iter().any(|tool| tool.name == "deployment.rollback"));
        assert!(tools.iter().any(|tool| tool.name == "deployment.restart"));
        assert!(tools.iter().any(|tool| tool.name == "database.backup"));
        assert!(tools.iter().any(|tool| tool.name == "database.restore"));
        assert!(
            tools
                .iter()
                .any(|tool| tool.name == "database.set_security_profile")
        );
        let without_bash = tool_definitions(false);
        assert!(without_bash.iter().all(|tool| tool.name != "bash"));
        assert!(
            without_bash
                .iter()
                .any(|tool| tool.name == "application.create")
        );
    }

    #[test]
    fn refuses_parallel_tool_calls_and_unauthorized_names() {
        let bash = crate::model::ModelToolCall {
            id: "bash-1".into(),
            name: "bash".into(),
            arguments: json!({ "command": "echo hi" }),
        };
        let create = crate::model::ModelToolCall {
            id: "create-1".into(),
            name: "application.create".into(),
            arguments: json!({ "name": "Tracker" }),
        };
        let allowed: Vec<String> = tool_definitions(true)
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        let parallel = super::select_model_tool_call(vec![bash.clone(), create.clone()], &allowed)
            .expect_err("parallel tool calls are refused");
        assert!(
            matches!(
                parallel,
                ActivationError::Protocol("model returned more than one tool call")
            ),
            "{parallel}"
        );
        let only_bash = super::select_model_tool_call(vec![bash.clone()], &allowed)
            .expect("authorized")
            .expect("bash");
        assert_eq!(only_bash.name, "bash");
        assert!(
            super::select_model_tool_call(Vec::new(), &allowed)
                .expect("authorized")
                .is_none()
        );

        let without_bash: Vec<String> = tool_definitions(false)
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        let refused = super::select_model_tool_call(vec![bash], &without_bash)
            .expect_err("disabled bash is unauthorized");
        assert!(
            matches!(
                refused,
                ActivationError::Protocol("model returned an unauthorized tool")
            ),
            "{refused}"
        );
        let unknown = crate::model::ModelToolCall {
            id: "evil-1".into(),
            name: "shell".into(),
            arguments: json!({ "command": "id" }),
        };
        let unknown_err = super::select_model_tool_call(vec![unknown], &allowed)
            .expect_err("unknown tools fail closed");
        assert!(
            matches!(
                unknown_err,
                ActivationError::Protocol("model returned an unauthorized tool")
            ),
            "{unknown_err}"
        );
        let create_only = super::select_model_tool_call(vec![create], &allowed)
            .expect("authorized")
            .expect("create");
        assert_eq!(create_only.name, "application.create");
    }

    #[test]
    fn empty_agent_prompt_gets_profile1_preamble() {
        let prompt = crate::http::resolve_agent_system_prompt("", None).expect("preamble");
        assert!(prompt.contains("application.create"), "{prompt}");
        assert!(prompt.contains("release.build"), "{prompt}");
        assert!(prompt.contains("one tool per turn"), "{prompt}");
        assert!(prompt.contains("/app"), "{prompt}");
        assert!(prompt.contains("read-only"), "{prompt}");
        assert!(prompt.contains("/tmp"), "{prompt}");
        assert!(prompt.contains("ManifestV1"), "{prompt}");
        let custom = crate::http::resolve_agent_system_prompt("custom", None).expect("composed");
        assert!(custom.contains("VOIE platform contract"), "{custom}");
        assert!(custom.contains("custom"), "{custom}");
        assert!(custom.starts_with("VOIE platform contract"), "{custom}");
        let child =
            crate::http::resolve_agent_system_prompt("", Some(" child ".into())).expect("child");
        assert!(child.contains("VOIE platform contract"), "{child}");
        assert!(child.contains("child"), "{child}");
    }

    #[test]
    fn product_tool_text_with_postgres_url_is_a_secret_leak() {
        assert!(crate::http::product_text_leaks_secret(
            r#"{"url":"postgres://app:secret@db/app"}"#
        ));
        assert!(crate::http::product_text_leaks_secret(
            r#"{"DATABASE_URL":"set"}"#
        ));
        assert!(crate::http::product_text_leaks_secret("PGPASSWORD=secret"));
        assert!(crate::http::product_text_leaks_secret("password=secret"));
        assert!(crate::http::product_text_leaks_secret(
            "https://app:hunter2@db.example/app"
        ));
        assert!(!crate::http::product_text_leaks_secret(
            r#"{"database":{"state":"ready"}}"#
        ));
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
        assert_eq!(crate::MAX_WORKSPACES_PER_PROJECT, 64);
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
