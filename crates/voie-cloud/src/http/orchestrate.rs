//! Control-plane orchestration for Release pack, Deployment materialize,
//! and Database provision. Fabric realizes; Blob and Key Vault hold bytes.

use std::ops::DerefMut;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use sqlx::Row;
use sqlx::pool::PoolConnection;
use sqlx::{PgPool, Postgres};
use uuid::Uuid;

use crate::applications::ApplicationError;
use crate::fabric_client::{FabricClient, FabricError};
use crate::secrets::{MaterialBackend, SecretValue};
use crate::session_store::BlobStore;

use super::Platform;

/// Public Caddy + wildcard DNS after Fabric has switched the Environment
/// Service. Fabric waits for voie-gateway Ready before opening the
/// activate journal. 45 × 2s covers public Caddy, TLS, and DNS catch-up
/// without inferring success.
const WILDCARD_EDGE_ATTEMPTS: u32 = 45;
const WILDCARD_EDGE_SLEEP: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct ProductRuntime {
    pub fabric: Arc<FabricClient>,
    pub blob: BlobStore,
    pub secrets: Arc<MaterialBackend>,
}

impl Platform {
    pub fn with_runtime(mut self, runtime: ProductRuntime) -> Self {
        self.runtime = Some(runtime);
        self
    }

    async fn try_hold_operation(pool: &PgPool, id: Uuid) -> Option<PoolConnection<Postgres>> {
        let mut conn = pool.acquire().await.ok()?;
        let key = i64::from_be_bytes(id.as_bytes()[0..8].try_into().ok()?);
        let locked: bool = sqlx::query_scalar("select pg_try_advisory_lock($1)")
            .bind(key)
            .fetch_one(conn.deref_mut())
            .await
            .unwrap_or(false);
        if locked { Some(conn) } else { None }
    }

    /// Blocking Database-identity lock. Restore completion is spawned, so
    /// waiting here serializes Blob → Fabric instead of abandoning a
    /// dispatched operation.
    async fn hold_operation(
        pool: &PgPool,
        id: Uuid,
    ) -> Result<PoolConnection<Postgres>, ApplicationError> {
        let mut conn = pool.acquire().await?;
        let bytes: [u8; 8] = id.as_bytes()[0..8]
            .try_into()
            .map_err(|_| ApplicationError::Kernel(crate::KernelError::Database))?;
        let key = i64::from_be_bytes(bytes);
        sqlx::query("select pg_advisory_lock($1)")
            .bind(key)
            .execute(conn.deref_mut())
            .await?;
        Ok(conn)
    }

    async fn release_operation(conn: &mut PoolConnection<Postgres>, id: Uuid) {
        let Ok(bytes) = <[u8; 8]>::try_from(&id.as_bytes()[0..8]) else {
            return;
        };
        let key = i64::from_be_bytes(bytes);
        let _ = sqlx::query_scalar::<_, bool>("select pg_advisory_unlock($1)")
            .bind(key)
            .fetch_one(conn.deref_mut())
            .await;
    }

    pub async fn realize_workspace_handoff(
        &self,
        workspace_id: Uuid,
    ) -> Result<(), ApplicationError> {
        let Some(runtime) = self.runtime.as_ref() else {
            return Ok(());
        };
        let row = sqlx::query("select allocated_bytes, storage_tier from workspaces where id = $1")
            .bind(workspace_id)
            .fetch_optional(self.applications.pool())
            .await?;
        let allocated = match row {
            Some(row) => {
                let allocated: i64 = row.get("allocated_bytes");
                allocated.max(0) as u64
            }
            None => crate::storage::WORKSPACE_BYTES as u64,
        };
        match runtime
            .fabric
            .create_workspace(workspace_id, Some(allocated), Some(false))
            .await
        {
            Ok(crate::fabric_client::CreateOutcome::Created) => {
                sqlx::query(
                    "update workspaces set state = 'ready' \
                     where id = $1 and state in ('creating', 'archived')",
                )
                .bind(workspace_id)
                .execute(self.applications.pool())
                .await?;
                return Ok(());
            }
            Ok(crate::fabric_client::CreateOutcome::Unknown) | Err(_) => {}
        }
        // Indeterminate create: reconcile with the read-only probe. Keep
        // `creating` unless Fabric proves the identity is absent (404).
        match runtime.fabric.get_workspace(workspace_id).await {
            Ok(Some(state)) if state == "ready" => {
                sqlx::query(
                    "update workspaces set state = 'ready' \
                     where id = $1 and state in ('creating', 'archived')",
                )
                .bind(workspace_id)
                .execute(self.applications.pool())
                .await?;
                Ok(())
            }
            Ok(Some(_)) => Err(ApplicationError::WorkspaceBusy),
            Ok(None) => Err(ApplicationError::WorkspaceMissing),
            Err(_) => Err(ApplicationError::WorkspaceBusy),
        }
    }

    pub async fn grow_workspace_elevated(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
        approval_id: Option<Uuid>,
    ) -> Result<i64, ApplicationError> {
        let workspace =
            sqlx::query("select project_id, allocated_bytes from workspaces where id = $1")
                .bind(workspace_id)
                .fetch_optional(self.applications.pool())
                .await?
                .ok_or(ApplicationError::NotFound)?;
        let project_id: Uuid = workspace.get("project_id");
        let allocated: i64 = workspace.get("allocated_bytes");
        if allocated != crate::storage::WORKSPACE_LARGE_BYTES {
            return Err(ApplicationError::WorkspaceBusy);
        }
        let application_id = self
            .applications
            .by_workspace(workspace_id)
            .await?
            .map(|application| application.id);
        let target = crate::applications::ApprovalTarget {
            application_id,
            ..Default::default()
        };
        let Some(approval_id) = approval_id else {
            crate::applications::require_approval(
                self.applications.pool(),
                None,
                project_id,
                "increase_resource_tier",
                &target,
                user_id,
            )
            .await?;
            return Err(ApplicationError::Auth);
        };
        let target_bytes = crate::storage::WORKSPACE_ELEVATED_BYTES;
        let grow_operation = self
            .applications
            .accept_elevated_workspace_grow(user_id, workspace_id, approval_id, target_bytes)
            .await?;
        let runtime = self
            .runtime
            .as_ref()
            .ok_or(ApplicationError::WorkspaceBusy)?;
        let probe = runtime
            .fabric
            .grow_workspace(workspace_id, target_bytes as u64)
            .await
            .map_err(|_| ApplicationError::WorkspaceBusy)?;
        let grown = probe.allocated_bytes.unwrap_or(target_bytes as u64) as i64;
        sqlx::query(
            "update workspaces set allocated_bytes = $2, storage_tier = 'elevated' where id = $1",
        )
        .bind(workspace_id)
        .bind(grown)
        .execute(self.applications.pool())
        .await?;
        self.applications
            .complete_workspace_grow(workspace_id, grow_operation)
            .await?;
        Ok(grown)
    }

    pub(crate) async fn sync_workspace_allocated_bytes(&self, workspace_id: Uuid) {
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let Ok(Some(probe)) = runtime.fabric.get_workspace_probe(workspace_id).await else {
            return;
        };
        let Some(bytes) = probe.allocated_bytes else {
            return;
        };
        let tier = crate::storage::workspace_tier_for_bytes(bytes as i64);
        let _ = sqlx::query(
            "update workspaces set allocated_bytes = $2, storage_tier = $3 \
             where id = $1 and allocated_bytes <> $2",
        )
        .bind(workspace_id)
        .bind(bytes as i64)
        .bind(tier)
        .execute(self.applications.pool())
        .await;
    }

    /// Drops an Application whose new Workspace never realized. Only a
    /// `creating` Workspace is removed so a ready guest is never deleted.
    /// Call only after a probe proved the Fabric identity is absent.
    pub(crate) async fn abort_unrealized_handoff(&self, application_id: Uuid, workspace_id: Uuid) {
        let pool = self.applications.pool();
        let _ = sqlx::query("delete from application_environments where application_id = $1")
            .bind(application_id)
            .execute(pool)
            .await;
        let _ = sqlx::query("delete from applications where id = $1")
            .bind(application_id)
            .execute(pool)
            .await;
        let _ = sqlx::query("delete from workspaces where id = $1 and state = 'creating'")
            .bind(workspace_id)
            .execute(pool)
            .await;
    }

    /// Refuses Application create on a confirmed unknown guest. A Profile 0
    /// `voie-runner:c1` guest is replaced with `voie-workspace:v1` (same
    /// volume) when the estate image is configured. Missing runtime (contract
    /// tests without a Fabric client) fail open. A configured Fabric client
    /// never fail-opens on transport or health failure: an indeterminate
    /// guest stays unknown and create cannot skip the workspace image upgrade.
    pub(crate) async fn require_profile1_workspace(
        &self,
        workspace_id: Uuid,
    ) -> Result<(), ApplicationError> {
        if self
            .applications
            .by_workspace(workspace_id)
            .await?
            .is_some()
        {
            return Ok(());
        }
        let Some(runtime) = self.runtime.as_ref() else {
            return Ok(());
        };
        let mut saw_transport = false;
        for attempt in 0..3 {
            match runtime.fabric.workspace_guest_image(workspace_id).await {
                Ok(Some(image)) if profile1_workspace_image(&image) => return Ok(()),
                Ok(Some(image)) if profile0_runner_image(&image) => {
                    return self.upgrade_workspace_profile(workspace_id).await;
                }
                Ok(Some(_)) => return Err(ApplicationError::WorkspaceImage),
                Err(FabricError::Transport) => {
                    saw_transport = true;
                    if attempt + 1 < 3 {
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    }
                }
                Ok(None) | Err(_) => return Err(ApplicationError::WorkspaceBusy),
            }
        }
        if saw_transport {
            return Err(ApplicationError::WorkspaceBusy);
        }
        Err(ApplicationError::WorkspaceBusy)
    }

    /// Recreates the Firecracker execution on the existing volume using the
    /// estate `voie-workspace:v1` profile. Cloud generation advances only
    /// after Fabric confirms the replacement.
    async fn upgrade_workspace_profile(&self, workspace_id: Uuid) -> Result<(), ApplicationError> {
        let Some(runtime) = self.runtime.as_ref() else {
            return Ok(());
        };
        let claimed =
            sqlx::query(
                "update workspaces set state = 'fenced' where id = $1 and state in ('ready', 'archived')",
            )
                .bind(workspace_id)
                .execute(self.applications.pool())
                .await?
                .rows_affected()
                == 1;
        if !claimed {
            return Err(ApplicationError::WorkspaceBusy);
        }
        match runtime.fabric.replace_workspace(workspace_id).await {
            Ok(()) => {
                let _ = sqlx::query(
                    "update workspaces set exec_generation = exec_generation + 1, state = 'ready' \
                     where id = $1 and state = 'fenced'",
                )
                .bind(workspace_id)
                .execute(self.applications.pool())
                .await;
                match runtime.fabric.workspace_guest_image(workspace_id).await {
                    Ok(Some(image)) if profile1_workspace_image(&image) => Ok(()),
                    Ok(Some(_)) => Err(ApplicationError::WorkspaceImage),
                    Ok(None) | Err(_) => Err(ApplicationError::WorkspaceBusy),
                }
            }
            Err(_) => match runtime.fabric.workspace_guest_image(workspace_id).await {
                Ok(Some(image)) if profile1_workspace_image(&image) => {
                    let _ = sqlx::query(
                        "update workspaces set exec_generation = exec_generation + 1, \
                             state = 'ready' where id = $1 and state = 'fenced'",
                    )
                    .bind(workspace_id)
                    .execute(self.applications.pool())
                    .await;
                    Ok(())
                }
                _ => Err(ApplicationError::WorkspaceBusy),
            },
        }
    }

    /// Reads `voie.toml` from the Workspace guest. Transport or a missing
    /// runtime returns `None` so contract tests without Fabric can still
    /// pass a caller manifest. A present but unusable file is an error.
    pub(crate) async fn read_guest_manifest(
        &self,
        workspace_id: Uuid,
        relative_root: &str,
    ) -> Result<Option<String>, ApplicationError> {
        let Some(runtime) = self.runtime.as_ref() else {
            return Ok(None);
        };
        let path = if relative_root == "." {
            "/workspace/voie.toml".to_owned()
        } else if relative_root.is_empty()
            || relative_root.starts_with('/')
            || relative_root
                .split('/')
                .any(|part| part == ".." || part.is_empty())
        {
            return Err(ApplicationError::InvalidRoot);
        } else {
            format!("/workspace/{relative_root}/voie.toml")
        };
        let quoted_path =
            serde_json::to_string(&path).map_err(|_| ApplicationError::InvalidRoot)?;
        let py = format!("print(open({quoted_path}).read(), end='')");
        let quoted_py = serde_json::to_string(&py).map_err(|_| ApplicationError::InvalidRoot)?;
        let command = format!("python3 -c {quoted_py}");
        let call_id = Uuid::new_v4().to_string();
        match runtime.fabric.exec(workspace_id, &call_id, &command).await {
            Ok(result) if result.is_completed() && result.exit_code == Some(0) => {
                self.sync_workspace_allocated_bytes(workspace_id).await;
                let text = result.stdout.unwrap_or_default();
                crate::applications::Manifest::parse(&text)
                    .map_err(|_| ApplicationError::InvalidName)?;
                Ok(Some(text))
            }
            Ok(result) if result.is_outcome_unknown() => Err(ApplicationError::WorkspaceBusy),
            Ok(_) => Ok(None),
            Err(FabricError::Transport) => Ok(None),
            Err(_) => Err(ApplicationError::InvalidName),
        }
    }

    /// Guest pack → Blob commit. Transport or a missing guest file leaves
    /// `dispatched` so a later GET/list can finish; unknown guest exec is
    /// terminal and is not replayed. Invalid toml fails the Release. A live
    /// Fabric that cannot re-read guest `voie.toml` does not pack unverified
    /// bytes.
    pub async fn complete_dispatched_release(
        &self,
        build_intent_id: Uuid,
        workspace_id: Uuid,
        relative_root: &str,
    ) {
        let Some(mut lock) =
            Self::try_hold_operation(self.applications.pool(), build_intent_id).await
        else {
            return;
        };
        self.complete_dispatched_release_locked(build_intent_id, workspace_id, relative_root)
            .await;
        Self::release_operation(&mut lock, build_intent_id).await;
    }

    async fn complete_dispatched_release_locked(
        &self,
        build_intent_id: Uuid,
        workspace_id: Uuid,
        relative_root: &str,
    ) {
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let Ok(release) = self.releases.get_internal_by_intent(build_intent_id).await else {
            return;
        };
        if release.state != "dispatched" {
            return;
        }
        let mut guest_text = None;
        const GUEST_MANIFEST_ATTEMPTS: u32 = 20;
        for attempt in 0..GUEST_MANIFEST_ATTEMPTS {
            match self.read_guest_manifest(workspace_id, relative_root).await {
                Ok(Some(guest)) => {
                    guest_text = Some(guest);
                    break;
                }
                Ok(None) if attempt + 1 < GUEST_MANIFEST_ATTEMPTS => {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Ok(None) => return,
                Err(ApplicationError::WorkspaceBusy) => {
                    let _ = self.releases.unknown(build_intent_id).await;
                    return;
                }
                Err(_) => {
                    let _ = self
                        .releases
                        .fail(build_intent_id, "guest voie.toml is unreadable")
                        .await;
                    return;
                }
            }
        }
        let Some(guest) = guest_text else {
            return;
        };
        match crate::applications::Manifest::parse(&guest) {
            Ok(parsed) => {
                let hash = parsed.hash(&guest);
                if release.manifest_hash.as_slice() != hash.as_slice() {
                    let _ = self
                        .releases
                        .fail(
                            build_intent_id,
                            "guest voie.toml does not match reserved manifest",
                        )
                        .await;
                    return;
                }
            }
            Err(_) => {
                let _ = self
                    .releases
                    .fail(build_intent_id, "guest voie.toml is invalid")
                    .await;
                return;
            }
        }
        if let Err(failed) = self
            .run_declared_guest_ops(
                runtime,
                workspace_id,
                relative_root,
                &release.manifest,
                build_intent_id,
            )
            .await
        {
            match failed {
                FabricError::Transport => return,
                FabricError::OutcomeUnknown | FabricError::Response => {
                    let _ = self.releases.unknown(build_intent_id).await;
                    return;
                }
                FabricError::Config(_) => {
                    let _ = self
                        .releases
                        .fail(build_intent_id, "guest operation refused")
                        .await;
                    return;
                }
            }
        }
        let hash = format!("pack:{build_intent_id}");
        match runtime
            .fabric
            .pack_workspace(workspace_id, build_intent_id, &hash, relative_root)
            .await
        {
            Ok(response) => {
                let Some(hex) = response
                    .headers()
                    .get("x-voie-artifact-hash")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned)
                else {
                    let _ = self.releases.unknown(build_intent_id).await;
                    return;
                };
                let stream = response.bytes_stream();
                let Ok(_release) = self.releases.get_internal_by_intent(build_intent_id).await
                else {
                    return;
                };
                let Ok((_release_id, key, artifact_hash, artifact_bytes)) = self
                    .releases
                    .stage_blob_stream(&runtime.blob, build_intent_id, &hex, stream)
                    .await
                else {
                    let _ = self.releases.unknown(build_intent_id).await;
                    return;
                };
                if self
                    .releases
                    .complete(
                        build_intent_id,
                        &key,
                        &artifact_hash,
                        artifact_bytes,
                        "packed",
                    )
                    .await
                    .is_err()
                {
                    let _ = self.releases.unknown(build_intent_id).await;
                    return;
                }
                let _ = runtime
                    .fabric
                    .ack_workspace_pack(workspace_id, build_intent_id)
                    .await;
            }
            Err(FabricError::Transport) => {}
            Err(FabricError::OutcomeUnknown) | Err(FabricError::Response) => {
                let _ = self.releases.unknown(build_intent_id).await;
            }
            Err(FabricError::Config(_)) => {
                let _ = self.releases.fail(build_intent_id, "pack refused").await;
            }
        }
    }

    /// Finishes a Transport-interrupted pack when the client next reads the
    /// Release. Unknown and failed rows are not replayed.
    pub async fn resume_dispatched_release(&self, release: &crate::releases::Release) {
        if release.state != "dispatched" {
            return;
        }
        let Ok(application) = crate::applications::ApplicationStore::new(
            self.applications.pool().clone(),
            String::new(),
        )
        .get_internal(release.application_id)
        .await
        else {
            return;
        };
        self.complete_dispatched_release(
            release.build_intent_id,
            release.source_workspace_id,
            &application.root_path,
        )
        .await;
    }

    /// Retries Fabric create while the candidate is still `materializing`.
    /// `starting` takes the same advisory lock as materialize so a status
    /// poll cannot mark unknown while a health window is still running.
    /// `superseded` and definite `failed` drain through the same stop/delete
    /// queue. Ambiguous `unknown` is not auto-cleaned.
    pub async fn resume_dispatched_deployment(&self, deployment: &crate::deployments::Deployment) {
        match deployment.state.as_str() {
            "materializing" => self.materialize_dispatched_deployment(deployment.id).await,
            "starting" => self.continue_starting_deployment(deployment.id).await,
            "superseded" | "failed" => {
                let _ = self.finish_cleanup_deployment(deployment.id).await;
            }
            _ => {}
        }
    }

    /// Idempotent predecessor or failed-candidate teardown. A transient
    /// `fabric_stop` failure leaves the cleanup-queue row so the next resume
    /// retries; SQL `stopped` is recorded only after Fabric reports success
    /// (or this process has no Fabric runtime).
    async fn finish_cleanup_deployment(&self, deployment_id: Uuid) -> Result<(), ApplicationError> {
        self.fabric_stop(deployment_id).await?;
        self.deployments.commit_stop(deployment_id).await?;
        Ok(())
    }

    async fn finish_superseded_deployment(
        &self,
        deployment_id: Uuid,
    ) -> Result<(), ApplicationError> {
        self.finish_cleanup_deployment(deployment_id).await
    }

    async fn settle_definite_materialize_failure(&self, deployment_id: Uuid) {
        let _ = self.deployments.fail(deployment_id).await;
        let Ok(row) = self.deployments.get_internal(deployment_id).await else {
            return;
        };
        if row.state != "failed" {
            return;
        }
        if self.finish_cleanup_deployment(deployment_id).await.is_err() {
            self.kick_resume_deployment(&row);
        }
    }

    /// After SQL cutover the predecessor is `superseded`. A settled
    /// activation must stop it. Failure keeps that durable queue row and
    /// schedules one immediate resume; the caller must not report success.
    pub(crate) async fn settle_superseded_predecessor(
        &self,
        predecessor_id: Uuid,
    ) -> Result<(), ApplicationError> {
        let predecessor = match self.deployments.get_internal(predecessor_id).await {
            Ok(row) => row,
            Err(ApplicationError::NotFound) => return Ok(()),
            Err(error) => return Err(error),
        };
        if predecessor.state != "superseded" {
            return Ok(());
        }
        match self.finish_superseded_deployment(predecessor.id).await {
            Ok(()) => Ok(()),
            Err(_) => {
                self.kick_resume_deployment(&predecessor);
                Err(ApplicationError::PredecessorCleanupPending)
            }
        }
    }

    /// Retries Database create with the already-attached credential. Does
    /// not mint a second password after Transport.
    pub async fn resume_creating_database(&self, database: &crate::databases::Database) {
        if database.state != "creating" {
            return;
        }
        // Kubelet Ready is observational. Fabric's create journal is already
        // terminal after apply, and Key Vault get is not required to promote
        // a Database whose credential is already attached.
        if let Some(secret_id) = database.credential_secret_id {
            if self
                .observe_and_mark_database_ready(database.id, secret_id)
                .await
            {
                return;
            }
        }
        let operation_id = match self
            .databases
            .dispatched_create_operation(database.id)
            .await
        {
            Ok(Some(operation_id)) => operation_id,
            Ok(None) => {
                eprintln!(
                    "voie-cloud: database {} has no dispatched create operation",
                    database.id
                );
                return;
            }
            Err(error) => {
                eprintln!(
                    "voie-cloud: database {} create-operation lookup failed: {}",
                    database.id,
                    error.message()
                );
                return;
            }
        };
        if let Err(error) = self.provision_database(database.id, operation_id).await {
            let detail = match &error {
                crate::applications::ApplicationError::Kernel(kernel) => format!("kernel:{kernel}"),
                other => other.message().to_string(),
            };
            eprintln!(
                "voie-cloud: database {} provision failed: {detail}",
                database.id
            );
        }
    }

    async fn observe_and_mark_database_ready(&self, database_id: Uuid, secret_id: Uuid) -> bool {
        let Some(runtime) = self.runtime.as_ref() else {
            return false;
        };
        match runtime
            .fabric
            .product_get(&format!("/v1/databases/{database_id}"))
            .await
        {
            Ok(outcome) if outcome.state == "ready" => {
                match self.databases.mark_ready(database_id, secret_id).await {
                    Ok(_) => true,
                    Err(error) => {
                        eprintln!(
                            "voie-cloud: database {database_id} mark_ready failed: {}",
                            error.message()
                        );
                        false
                    }
                }
            }
            Ok(_) | Err(FabricError::Transport) | Err(FabricError::Response) => false,
            Err(_) => false,
        }
    }

    /// Pack, materialize, and provision run off the HTTP and activation
    /// request. GET/list kick resume and return the current snapshot.
    pub fn kick_complete_release(
        &self,
        build_intent_id: Uuid,
        workspace_id: Uuid,
        relative_root: String,
    ) {
        let platform = self.clone();
        tokio::spawn(async move {
            platform
                .complete_dispatched_release(build_intent_id, workspace_id, &relative_root)
                .await;
        });
    }

    pub fn kick_materialize_deployment(&self, deployment_id: Uuid) {
        let platform = self.clone();
        tokio::spawn(async move {
            platform
                .materialize_dispatched_deployment(deployment_id)
                .await;
        });
    }

    pub fn kick_provision_database(&self, database_id: Uuid, operation_id: Uuid) {
        let platform = self.clone();
        tokio::spawn(async move {
            let _ = platform.provision_database(database_id, operation_id).await;
        });
    }

    pub fn kick_continue_starting(&self, deployment_id: Uuid) {
        let platform = self.clone();
        tokio::spawn(async move {
            platform.continue_starting_deployment(deployment_id).await;
        });
    }

    pub fn kick_resume_release(&self, release: &crate::releases::Release) {
        if release.state != "dispatched" {
            return;
        }
        let platform = self.clone();
        let release = release.clone();
        tokio::spawn(async move {
            platform.resume_dispatched_release(&release).await;
        });
    }

    pub fn kick_resume_deployment(&self, deployment: &crate::deployments::Deployment) {
        if deployment.state != "materializing"
            && deployment.state != "starting"
            && deployment.state != "superseded"
            && deployment.state != "failed"
        {
            return;
        }
        let platform = self.clone();
        let deployment = deployment.clone();
        tokio::spawn(async move {
            platform.resume_dispatched_deployment(&deployment).await;
        });
    }

    pub fn kick_resume_database(&self, database: &crate::databases::Database) {
        if database.state != "creating" {
            return;
        }
        let platform = self.clone();
        let database = database.clone();
        tokio::spawn(async move {
            platform.resume_creating_database(&database).await;
        });
    }

    pub fn kick_complete_backup(&self, database_id: Uuid, operation_id: Uuid) {
        let platform = self.clone();
        tokio::spawn(async move {
            platform
                .complete_database_backup(database_id, operation_id)
                .await;
        });
    }

    pub fn kick_complete_restore(&self, database_id: Uuid, backup_id: Uuid, operation_id: Uuid) {
        let platform = self.clone();
        tokio::spawn(async move {
            let _ = platform
                .complete_database_restore(database_id, backup_id, operation_id)
                .await;
        });
    }

    /// Health-gates a `starting` Deployment. If this row is still the
    /// Environment's active selector, SQL `active` is restored after healthy
    /// so a Pod restart does not leave production without a cutover.
    pub async fn continue_starting_deployment(&self, deployment_id: Uuid) {
        let Some(mut lock) =
            Self::try_hold_operation(self.applications.pool(), deployment_id).await
        else {
            return;
        };
        self.probe_and_mark_healthy(deployment_id).await;
        if let Ok(deployment) = self.deployments.get_internal(deployment_id).await {
            if deployment.state == "healthy" {
                if let Ok(Some(environment)) = crate::applications::load_environment(
                    self.applications.pool(),
                    deployment.environment_id,
                )
                .await
                {
                    if environment.active_deployment_id == Some(deployment_id) {
                        let _ = self.deployments.activate(deployment_id).await;
                    }
                }
            }
        }
        Self::release_operation(&mut lock, deployment_id).await;
    }

    /// Materializes a candidate Deployment. Does not mark healthy or cut over.
    /// Transport failure leaves `materializing`.
    pub async fn materialize_dispatched_deployment(&self, deployment_id: Uuid) {
        let Some(mut lock) =
            Self::try_hold_operation(self.applications.pool(), deployment_id).await
        else {
            return;
        };
        self.materialize_dispatched_deployment_locked(deployment_id)
            .await;
        Self::release_operation(&mut lock, deployment_id).await;
    }

    async fn materialize_dispatched_deployment_locked(&self, deployment_id: Uuid) {
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let Ok(deployment) = self.deployments.get_internal(deployment_id).await else {
            return;
        };
        if deployment.state != "materializing" {
            return;
        }
        let Ok(release) = self.releases.get_internal(deployment.release_id).await else {
            return;
        };
        let Ok(environment) = crate::applications::load_environment(
            self.applications.pool(),
            deployment.environment_id,
        )
        .await
        else {
            return;
        };
        let Some(environment) = environment else {
            return;
        };
        let Ok(application) = crate::applications::ApplicationStore::new(
            self.applications.pool().clone(),
            String::new(),
        )
        .get_internal(environment.application_id)
        .await
        else {
            return;
        };
        let (Some(key), Some(hash_bytes), Some(_artifact_bytes)) = (
            release.artifact_key.as_deref(),
            release.artifact_hash.as_ref(),
            release.artifact_bytes,
        ) else {
            let _ = self.deployments.unknown(deployment_id).await;
            return;
        };
        let hex = hex_sha(hash_bytes);
        match Self::stage_release_cache(runtime, release.id, &hex, key).await {
            Ok(()) => {}
            Err(FabricError::Transport) => return,
            Err(FabricError::Config(_)) => {
                self.settle_definite_materialize_failure(deployment_id)
                    .await;
                return;
            }
            Err(FabricError::OutcomeUnknown) | Err(FabricError::Response) => {
                let _ = self.deployments.unknown(deployment_id).await;
                return;
            }
        }
        let run_argv = manifest_run_argv(&release.manifest);
        let port = manifest_port(&release.manifest);
        let health_path = manifest_health(&release.manifest);
        let mut body = json!({
            "operation_id": deployment.deployment_intent_id,
            "request_hash": hex_sha(&deployment.request_hash),
            "desired_revision": deployment.desired_revision,
            "release_id": release.id,
            "slug": application.slug,
            "kind": environment.kind,
            "port": port,
            "health_path": health_path,
            "run_argv": run_argv,
            "cpu_millis": manifest_cpu_millis(&release.manifest),
            "memory_mb": manifest_memory_mb(&release.manifest),
            "console_host": self.applications.console_host(),
        });
        if let Ok(Some(database)) = self.databases.by_environment(environment.id).await {
            if environment.kind == "prod" && database.environment_id != environment.id {
                return;
            }
            body["database_id"] = json!(database.id.to_string());
        }
        match self.bindings.list_internal(environment.id).await {
            Ok(bindings) => {
                let mut streamed = Vec::new();
                let mut complete = true;
                for binding in bindings {
                    match runtime
                        .secrets
                        .get_platform_material(binding.secret_id)
                        .await
                    {
                        Ok(material) => match std::str::from_utf8(material.as_bytes()) {
                            Ok(text) if !text.is_empty() => {
                                streamed.push(json!({
                                    "name": binding.environment_name,
                                    "value": text,
                                }));
                            }
                            _ => {
                                complete = false;
                                break;
                            }
                        },
                        Err(_) => {
                            complete = false;
                            break;
                        }
                    }
                }
                if !complete {
                    let _ = self.deployments.unknown(deployment_id).await;
                    return;
                }
                if !streamed.is_empty() {
                    body["env_bindings"] = json!(streamed);
                }
            }
            Err(_) => {
                let _ = self.deployments.unknown(deployment_id).await;
                return;
            }
        }
        match runtime
            .fabric
            .product_mutate(&format!("/v1/deployments/{deployment_id}"), &body)
            .await
        {
            Ok(outcome) if outcome.state == "unknown" => {
                let _ = self.deployments.unknown(deployment_id).await;
                return;
            }
            Err(FabricError::OutcomeUnknown) | Err(FabricError::Response) => {
                let _ = self.deployments.unknown(deployment_id).await;
                return;
            }
            Err(FabricError::Transport) => return,
            Err(FabricError::Config(_)) => {
                self.settle_definite_materialize_failure(deployment_id)
                    .await;
                return;
            }
            Ok(_) => {}
        }
        if let Some(migrate) = manifest_migrate_argv(&release.manifest) {
            let mut migrate_body = json!({
                "operation_id": migrate_operation_id(deployment_id),
                "request_hash": format!("migrate:{}", hex_sha(&deployment.request_hash)),
                "desired_revision": deployment.desired_revision,
                "run_argv": ["true"],
                "migrate_argv": migrate,
            });
            if let Some(id) = body.get("database_id") {
                migrate_body["database_id"] = id.clone();
            }
            match runtime
                .fabric
                .product_mutate(
                    &format!("/v1/deployments/{deployment_id}/migrate"),
                    &migrate_body,
                )
                .await
            {
                Ok(outcome) if outcome.state == "unknown" => {
                    let _ = self.deployments.unknown(deployment_id).await;
                    return;
                }
                Err(FabricError::OutcomeUnknown) | Err(FabricError::Response) => {
                    let _ = self.deployments.unknown(deployment_id).await;
                    return;
                }
                Err(FabricError::Transport) => return,
                Err(FabricError::Config(_)) => {
                    self.settle_definite_materialize_failure(deployment_id)
                        .await;
                    return;
                }
                Ok(_) => {}
            }
        }
        let _ = self.deployments.advance(deployment_id, "starting").await;
        self.probe_and_mark_healthy(deployment_id).await;
        self.ship_deployment_logs(deployment_id).await;
    }

    pub async fn probe_and_mark_healthy(&self, deployment_id: Uuid) {
        self.probe_health_loop(deployment_id, 60).await;
    }

    async fn probe_health_loop(&self, deployment_id: Uuid, attempts: u32) {
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let Ok(deployment) = self.deployments.get_internal(deployment_id).await else {
            return;
        };
        if !matches!(
            deployment.state.as_str(),
            "starting" | "materializing" | "unknown"
        ) {
            return;
        }
        let Ok(release) = self.releases.get_internal(deployment.release_id).await else {
            return;
        };
        for attempt in 0..attempts {
            match runtime
                .fabric
                .probe_deployment_health(
                    deployment_id,
                    manifest_port(&release.manifest),
                    &manifest_health(&release.manifest),
                )
                .await
            {
                Ok(true) => {
                    let _ = self.deployments.mark_healthy(deployment_id).await;
                    return;
                }
                Ok(false)
                | Err(FabricError::Transport)
                | Err(FabricError::OutcomeUnknown)
                | Err(FabricError::Response)
                    if attempt + 1 < attempts =>
                {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                Ok(false)
                | Err(FabricError::Transport)
                | Err(FabricError::OutcomeUnknown)
                | Err(FabricError::Response) => return,
                Err(_) => {
                    let _ = self.deployments.unknown(deployment_id).await;
                    return;
                }
            }
        }
    }

    pub async fn ship_deployment_logs(&self, deployment_id: Uuid) {
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let Ok(bytes) = runtime.fabric.get_deployment_logs(deployment_id).await else {
            return;
        };
        if bytes.is_empty() {
            return;
        }
        let Ok(deployment) = self.deployments.get_internal(deployment_id).await else {
            return;
        };
        let Ok(Some(environment)) = crate::applications::load_environment(
            self.applications.pool(),
            deployment.environment_id,
        )
        .await
        else {
            return;
        };
        let Ok(application) = crate::applications::ApplicationStore::new(
            self.applications.pool().clone(),
            String::new(),
        )
        .get_internal(environment.application_id)
        .await
        else {
            return;
        };
        let logs = crate::deployment_logs::DeploymentLogs::new(self.applications.pool().clone());
        let Ok(seq) = logs.next_seq(deployment_id).await else {
            return;
        };
        let digest: [u8; 32] = {
            use sha2::{Digest, Sha256};
            Sha256::digest(&bytes).into()
        };
        let key = format!(
            "logs/{}/{}/{}/{seq}",
            application.project_id, application.id, deployment_id
        );
        if runtime
            .blob
            .put_artifact_if_absent(&key, &bytes)
            .await
            .is_err()
        {
            return;
        }
        let stamp: String = sqlx::query_scalar("select now()::text")
            .fetch_one(self.applications.pool())
            .await
            .unwrap_or_else(|_| "1970-01-01 00:00:00+00".into());
        let _ = logs
            .append(
                deployment_id,
                seq,
                &key,
                &digest,
                bytes.len() as i64,
                &stamp,
                &stamp,
            )
            .await;
    }

    pub async fn fabric_stop(&self, deployment_id: Uuid) -> Result<(), ApplicationError> {
        self.fabric_mutate_deployment(deployment_id, "stop").await
    }

    pub async fn fabric_restart(&self, deployment_id: Uuid) -> Result<(), ApplicationError> {
        self.fabric_mutate_deployment(deployment_id, "restart")
            .await
    }

    async fn fabric_mutate_deployment(
        &self,
        deployment_id: Uuid,
        action: &str,
    ) -> Result<(), ApplicationError> {
        let Some(runtime) = self.runtime.as_ref() else {
            return Ok(());
        };
        let deployment = self.deployments.get_internal(deployment_id).await?;
        let environment = crate::applications::load_environment(
            self.applications.pool(),
            deployment.environment_id,
        )
        .await?
        .ok_or(ApplicationError::NotFound)?;
        let application = crate::applications::ApplicationStore::new(
            self.applications.pool().clone(),
            String::new(),
        )
        .get_internal(environment.application_id)
        .await?;
        let release = self.releases.get_internal(deployment.release_id).await?;
        let operation_id = if action == "restart" {
            Uuid::new_v4()
        } else {
            typed_operation_id(b"voie-stop:", deployment_id)
        };
        let request_hash = if action == "restart" {
            hex_sha(&deployment.request_hash)
        } else {
            hex_sha(deployment_id.as_bytes())
        };
        let body = json!({
            "operation_id": operation_id,
            "request_hash": request_hash,
            "desired_revision": deployment.desired_revision,
            "release_id": release.id,
            "slug": application.slug,
            "kind": environment.kind,
            "port": manifest_port(&release.manifest),
            "health_path": manifest_health(&release.manifest),
            "run_argv": manifest_run_argv(&release.manifest),
            "cpu_millis": manifest_cpu_millis(&release.manifest),
            "memory_mb": manifest_memory_mb(&release.manifest),
            "console_host": self.applications.console_host(),
        });
        match runtime
            .fabric
            .product_mutate(&format!("/v1/deployments/{deployment_id}/{action}"), &body)
            .await
        {
            Ok(outcome) if outcome.state == "unknown" => Err(ApplicationError::WorkspaceBusy),
            Ok(_) => Ok(()),
            Err(FabricError::Transport) => Err(ApplicationError::WorkspaceBusy),
            Err(_) => Err(ApplicationError::Kernel(crate::KernelError::Database)),
        }
    }

    /// Switches the Environment Service selector. `Ok(true)` means Fabric
    /// applied the switch. `Ok(false)` means this process has no Fabric
    /// runtime (contract tests). Live transport failure is not a switch.
    pub async fn fabric_activate(&self, deployment_id: Uuid) -> Result<bool, ApplicationError> {
        let Some(runtime) = self.runtime.as_ref() else {
            return Ok(false);
        };
        let deployment = self.deployments.get_internal(deployment_id).await?;
        let environment = crate::applications::load_environment(
            self.applications.pool(),
            deployment.environment_id,
        )
        .await?
        .ok_or(ApplicationError::NotFound)?;
        let application = crate::applications::ApplicationStore::new(
            self.applications.pool().clone(),
            String::new(),
        )
        .get_internal(environment.application_id)
        .await?;
        let release = self.releases.get_internal(deployment.release_id).await?;
        let body = json!({
            "operation_id": typed_operation_id(b"voie-activate:", deployment_id),
            "request_hash": hex_sha(&deployment.request_hash),
            "desired_revision": deployment.desired_revision,
            "release_id": release.id,
            "slug": application.slug,
            "kind": environment.kind,
            "port": manifest_port(&release.manifest),
            "health_path": manifest_health(&release.manifest),
            "run_argv": manifest_run_argv(&release.manifest),
            "console_host": self.applications.console_host(),
            "previous_deployment_id": deployment.previous_deployment_id,
        });
        match runtime
            .fabric
            .product_mutate(&format!("/v1/deployments/{deployment_id}/activate"), &body)
            .await
        {
            Ok(outcome) if outcome.state == "unknown" => {
                let _ = self.deployments.unknown(deployment_id).await;
                Err(ApplicationError::WorkspaceBusy)
            }
            Ok(_) => Ok(true),
            Err(FabricError::Transport) => Err(ApplicationError::WorkspaceBusy),
            // Fabric 409: Application Pod or voie-gateway was not Ready.
            // SQL stays healthy so activate can retry; the typed journal
            // was not opened.
            Err(FabricError::Response) => Err(ApplicationError::DeploymentNotReady),
            Err(_) => Err(ApplicationError::Kernel(crate::KernelError::Database)),
        }
    }

    /// GET the candidate through public Caddy after the Fabric selector
    /// switch. Fabric already waited for voie-gateway Ready before
    /// cutover; this loop covers public Caddy, TLS, and wildcard DNS.
    /// 401/403 fail immediately. Transport or exhausted non-2xx fails
    /// closed; success is never inferred.
    pub async fn probe_wildcard_edge(
        &self,
        user_id: Uuid,
        deployment_id: Uuid,
    ) -> Result<(), ApplicationError> {
        let deployment = self.deployments.get_internal(deployment_id).await?;
        let environment = crate::applications::load_environment(
            self.applications.pool(),
            deployment.environment_id,
        )
        .await?
        .ok_or(ApplicationError::NotFound)?;
        let application = crate::applications::ApplicationStore::new(
            self.applications.pool().clone(),
            String::new(),
        )
        .get_internal(environment.application_id)
        .await?;
        let release = self.releases.get_internal(deployment.release_id).await?;
        let health = manifest_health(&release.manifest);
        let path = if health.starts_with('/') {
            health
        } else {
            format!("/{health}")
        };
        let url = format!("https://{}{path}", environment.hostname);
        let token = self
            .preview
            .mint_session_token(
                user_id,
                application.id,
                environment.id,
                &environment.hostname,
            )
            .await?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(4))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| ApplicationError::WorkspaceBusy)?;
        let cookie = format!("{}={token}", crate::preview_auth::PREVIEW_COOKIE);
        for attempt in 0..WILDCARD_EDGE_ATTEMPTS {
            match client
                .get(&url)
                .header(reqwest::header::COOKIE, &cookie)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => return Ok(()),
                Ok(response)
                    if response.status() == reqwest::StatusCode::UNAUTHORIZED
                        || response.status() == reqwest::StatusCode::FORBIDDEN =>
                {
                    return Err(ApplicationError::WorkspaceBusy);
                }
                Ok(_) | Err(_) if attempt + 1 < WILDCARD_EDGE_ATTEMPTS => {
                    tokio::time::sleep(WILDCARD_EDGE_SLEEP).await;
                }
                Ok(_) | Err(_) => return Err(ApplicationError::WorkspaceBusy),
            }
        }
        Err(ApplicationError::WorkspaceBusy)
    }

    pub async fn provision_database(
        &self,
        database_id: Uuid,
        operation_id: Uuid,
    ) -> Result<(), ApplicationError> {
        let Some(mut lock) = Self::try_hold_operation(self.applications.pool(), database_id).await
        else {
            eprintln!("voie-cloud: database {database_id} provision lock is busy");
            return Ok(());
        };
        let result = self
            .provision_database_locked(database_id, operation_id)
            .await;
        Self::release_operation(&mut lock, database_id).await;
        result
    }

    async fn provision_database_locked(
        &self,
        database_id: Uuid,
        operation_id: Uuid,
    ) -> Result<(), ApplicationError> {
        let Some(runtime) = self.runtime.as_ref() else {
            eprintln!(
                "voie-cloud: database {database_id} provision skipped: fabric runtime is not attached"
            );
            return Ok(());
        };
        let database = self.databases.get_internal(database_id).await?;
        if database.state != "creating" {
            return Ok(());
        }
        let environment = crate::applications::load_environment(
            self.applications.pool(),
            database.environment_id,
        )
        .await?
        .ok_or(ApplicationError::NotFound)?;
        let application = crate::applications::ApplicationStore::new(
            self.applications.pool().clone(),
            String::new(),
        )
        .get_internal(environment.application_id)
        .await?;
        let (secret_id, password) = if let Some(existing) = database.credential_secret_id {
            match runtime.secrets.get_platform_material(existing).await {
                Ok(material) => match std::str::from_utf8(material.as_bytes()) {
                    Ok(text) if !text.is_empty() => (existing, text.to_owned()),
                    _ => {
                        // Empty or non-UTF8 material cannot become a postgres
                        // password. Leave `creating` would hang live C3 until
                        // the wait budget; unknown fails closed immediately.
                        let _ = self.databases.unknown(database_id, operation_id).await;
                        return Ok(());
                    }
                },
                Err(_) => return Ok(()),
            }
        } else {
            let password = crate::databases::generate_postgres_password()?;
            let secret_id = Uuid::new_v4();
            let value = SecretValue::from_text(password.clone())
                .map_err(|_| ApplicationError::Kernel(crate::KernelError::Database))?;
            runtime
                .secrets
                .put_platform_material(secret_id, value)
                .await
                .map_err(|error| {
                    eprintln!(
                        "voie-cloud: database {database_id} credential store failed: {error}"
                    );
                    ApplicationError::Kernel(crate::KernelError::Database)
                })?;
            self.databases
                .attach_credential(database_id, secret_id)
                .await?;
            (secret_id, password)
        };
        let body = json!({
            "operation_id": operation_id,
            "request_hash": hex_sha(database_id.as_bytes()),
            "desired_revision": database.desired_revision,
            "slug": application.slug,
            "kind": environment.kind,
            "postgres_password": password,
            "allocated_bytes": database.storage_bytes as u64,
        });
        match runtime
            .fabric
            .product_mutate(&format!("/v1/databases/{database_id}"), &body)
            .await
        {
            Ok(outcome) if outcome.state == "unknown" => {
                let _ = self.databases.unknown(database_id, operation_id).await;
                return Ok(());
            }
            Ok(_) => {}
            Err(FabricError::Transport) => return Ok(()),
            Err(FabricError::OutcomeUnknown) | Err(FabricError::Response) | Err(_) => {
                let _ = self.databases.unknown(database_id, operation_id).await;
                return Ok(());
            }
        }
        // Apply is journaled. Ready is kubelet: stay `creating` until GET
        // sees the postgres Pod Ready so a slow initdb is not unknown.
        let _ = self
            .observe_and_mark_database_ready(database_id, secret_id)
            .await;
        Ok(())
    }

    async fn run_declared_guest_ops(
        &self,
        runtime: &ProductRuntime,
        workspace_id: Uuid,
        relative_root: &str,
        manifest: &Value,
        build_intent_id: Uuid,
    ) -> Result<(), FabricError> {
        if let Some(test) = manifest_test_argv(manifest) {
            let operation_id = typed_operation_id(b"voie-test:", build_intent_id);
            let hash = format!("test:{workspace_id}:{build_intent_id}");
            let code = runtime
                .fabric
                .guest_run(workspace_id, operation_id, &hash, relative_root, &test)
                .await?;
            if code != 0 {
                return Err(FabricError::Config("declared test command failed"));
            }
        }
        let build = manifest_build_argv(manifest);
        if !build.is_empty() {
            let operation_id = typed_operation_id(b"voie-build:", build_intent_id);
            let hash = format!("build:{workspace_id}:{build_intent_id}");
            let code = runtime
                .fabric
                .guest_run(workspace_id, operation_id, &hash, relative_root, &build)
                .await?;
            if code != 0 {
                return Err(FabricError::Config("declared build command failed"));
            }
        }
        Ok(())
    }

    pub async fn complete_database_backup(&self, database_id: Uuid, operation_id: Uuid) {
        let Some(mut lock) = Self::try_hold_operation(self.applications.pool(), operation_id).await
        else {
            return;
        };
        self.complete_database_backup_locked(database_id, operation_id)
            .await;
        Self::release_operation(&mut lock, operation_id).await;
    }

    async fn complete_database_backup_locked(&self, database_id: Uuid, operation_id: Uuid) {
        let _ = self
            .finish_database_backup(database_id, operation_id, "manual")
            .await;
    }

    pub async fn complete_database_restore(
        &self,
        database_id: Uuid,
        backup_id: Uuid,
        operation_id: Uuid,
    ) -> Result<(), ApplicationError> {
        // Serialize the Blob -> Fabric restore transition by Database
        // identity. operation_id remains the journal/idempotency identity.
        let mut lock = Self::hold_operation(self.applications.pool(), database_id).await?;
        let result = self
            .complete_database_restore_locked(database_id, backup_id, operation_id)
            .await;
        Self::release_operation(&mut lock, database_id).await;
        result
    }

    async fn complete_database_restore_locked(
        &self,
        database_id: Uuid,
        backup_id: Uuid,
        operation_id: Uuid,
    ) -> Result<(), ApplicationError> {
        let Some(runtime) = self.runtime.as_ref() else {
            return Ok(());
        };
        let backup = self.databases.get_backup(backup_id).await?;
        if backup.database_id != database_id {
            return Err(ApplicationError::NotFound);
        }
        let database = self.databases.get_internal(database_id).await?;
        let environment = crate::applications::load_environment(
            self.applications.pool(),
            database.environment_id,
        )
        .await?
        .ok_or(ApplicationError::NotFound)?;
        let application = crate::applications::ApplicationStore::new(
            self.applications.pool().clone(),
            String::new(),
        )
        .get_internal(environment.application_id)
        .await?;
        let password = if let Some(secret_id) = database.credential_secret_id {
            match runtime.secrets.get_platform_material(secret_id).await {
                Ok(material) => std::str::from_utf8(material.as_bytes())
                    .ok()
                    .filter(|text| !text.is_empty())
                    .map(ToOwned::to_owned),
                Err(_) => None,
            }
        } else {
            None
        };
        let hex = hex_sha(&backup.content_hash);
        runtime
            .fabric
            .put_restore_from_blob(database_id, &hex, &runtime.blob, &backup.object_key)
            .await
            .map_err(|_| ApplicationError::WorkspaceBusy)?;
        let mut body = json!({
            "operation_id": operation_id,
            "request_hash": hex,
            "desired_revision": 1,
            "artifact_hash": hex,
            "allocated_bytes": database.storage_bytes as u64,
            "slug": application.slug,
            "kind": environment.kind,
        });
        if let Some(password) = password {
            body["postgres_password"] = json!(password);
        }
        match runtime
            .fabric
            .product_mutate(&format!("/v1/databases/{database_id}/restore"), &body)
            .await
        {
            Ok(outcome) if outcome.state == "unknown" => {
                let _ = self.databases.unknown(database_id, operation_id).await;
                Err(ApplicationError::WorkspaceBusy)
            }
            Ok(_) => Ok(()),
            Err(FabricError::Transport) => Err(ApplicationError::WorkspaceBusy),
            Err(FabricError::OutcomeUnknown) | Err(FabricError::Response) => {
                let _ = self.databases.unknown(database_id, operation_id).await;
                Err(ApplicationError::WorkspaceBusy)
            }
            Err(_) => Err(ApplicationError::Kernel(crate::KernelError::Database)),
        }
    }

    pub async fn cleanup_application_fabric(
        &self,
        cleanup: &crate::applications::ApplicationCleanup,
    ) -> Result<(), ApplicationError> {
        let Some(runtime) = self.runtime.as_ref() else {
            return Ok(());
        };
        for target in &cleanup.deployments {
            let body = json!({
                "operation_id": typed_operation_id(b"voie-stop:", target.id),
                "request_hash": hex_sha(target.id.as_bytes()),
                "desired_revision": 1,
                "slug": cleanup.slug,
                "kind": target.kind,
                "run_argv": ["true"],
            });
            retry_cleanup_mutate(runtime, format!("/v1/deployments/{}/stop", target.id), body)
                .await?;
        }
        for database_id in &cleanup.databases {
            let body = json!({
                "operation_id": typed_operation_id(b"voie-db-delete:", *database_id),
                "request_hash": hex_sha(database_id.as_bytes()),
                "desired_revision": 1,
            });
            retry_cleanup_mutate(runtime, format!("/v1/databases/{database_id}/delete"), body)
                .await?;
        }
        if let Some(workspace_id) = cleanup.workspace_id {
            runtime
                .fabric
                .delete_workspace(workspace_id)
                .await
                .map_err(|_| ApplicationError::WorkspaceBusy)?;
        }
        for release_id in &cleanup.releases {
            let body = json!({
                "operation_id": typed_operation_id(b"voie-release-delete:", *release_id),
                "request_hash": hex_sha(release_id.as_bytes()),
                "desired_revision": 1,
            });
            retry_cleanup_mutate(runtime, format!("/v1/releases/{release_id}/delete"), body)
                .await?;
        }
        Ok(())
    }

    async fn stop_application_traffic(
        &self,
        cleanup: &crate::applications::ApplicationCleanup,
    ) -> Result<(), ApplicationError> {
        let Some(runtime) = self.runtime.as_ref() else {
            return Ok(());
        };
        for target in &cleanup.deployments {
            let body = json!({
                "operation_id": typed_operation_id(b"voie-stop:", target.id),
                "request_hash": hex_sha(target.id.as_bytes()),
                "desired_revision": 1,
                "slug": cleanup.slug,
                "kind": target.kind,
                "run_argv": ["true"],
            });
            retry_cleanup_mutate(runtime, format!("/v1/deployments/{}/stop", target.id), body)
                .await?;
        }
        Ok(())
    }

    pub async fn archive_application(
        &self,
        user_id: Uuid,
        application_id: Uuid,
    ) -> Result<crate::applications::Application, ApplicationError> {
        let phase = self
            .applications
            .begin_archive(user_id, application_id)
            .await?;
        if phase == "archived" {
            return self.applications.get(user_id, application_id).await;
        }
        let cleanup = self
            .applications
            .plan_archive(user_id, application_id)
            .await?;
        let capturing = self
            .applications
            .capturing_archive(application_id)
            .await?
            .ok_or(ApplicationError::WorkspaceBusy)?;
        let generation = capturing.generation;
        let mut points = ArchiveRestorePoints {
            workspace_snapshot_id: capturing.workspace_snapshot_id,
            dev_database_backup_id: capturing.dev_database_backup_id,
            prod_database_backup_id: capturing.prod_database_backup_id,
            dev_release_id: capturing.dev_release_id,
            prod_release_id: capturing.prod_release_id,
        };
        // Fence only while the Workspace snapshot is still outstanding.
        // Retry after a completed snapshot must not require the live
        // volume: cleanup may already have deleted it.
        if points.workspace_snapshot_id.is_none() {
            if let Some(workspace_id) = cleanup.workspace_id {
                if let Some(runtime) = self.runtime.as_ref() {
                    runtime
                        .fabric
                        .fence_workspace(workspace_id)
                        .await
                        .map_err(|_| ApplicationError::WorkspaceBusy)?;
                }
            }
        }
        self.capture_archive_release_ids(application_id, &mut points)
            .await?;
        self.applications
            .persist_archive_restore_points(
                application_id,
                points.workspace_snapshot_id,
                points.dev_database_backup_id,
                points.prod_database_backup_id,
                points.dev_release_id,
                points.prod_release_id,
            )
            .await?;
        self.stop_application_traffic(&cleanup).await?;
        self.capture_archive_restore_points(
            user_id,
            application_id,
            &cleanup,
            &mut points,
            generation,
        )
        .await?;
        self.applications
            .persist_archive_restore_points(
                application_id,
                points.workspace_snapshot_id,
                points.dev_database_backup_id,
                points.prod_database_backup_id,
                points.dev_release_id,
                points.prod_release_id,
            )
            .await?;
        self.cleanup_application_fabric(&cleanup).await?;
        self.applications
            .commit_archive(
                user_id,
                application_id,
                points.workspace_snapshot_id,
                points.dev_database_backup_id,
                points.prod_database_backup_id,
                points.dev_release_id,
                points.prod_release_id,
            )
            .await?;
        self.reclaim_unpinned_recovery(application_id).await;
        self.applications.get(user_id, application_id).await
    }

    pub async fn restore_application(
        &self,
        user_id: Uuid,
        application_id: Uuid,
        approval_id: Option<Uuid>,
    ) -> Result<crate::applications::Application, ApplicationError> {
        let application = self.applications.get(user_id, application_id).await?;
        if application.state == "archived" {
            let archive = self
                .applications
                .get_archive(application_id)
                .await?
                .ok_or(ApplicationError::NotFound)?;
            let target = self.restore_application_target(&archive).await?;
            crate::applications::require_approval(
                self.applications.pool(),
                approval_id,
                application.project_id,
                "restore_application",
                &target,
                user_id,
            )
            .await?;
        }
        let phase = self
            .applications
            .begin_restore(user_id, application_id)
            .await?;
        if phase == "ready" {
            return self.applications.get(user_id, application_id).await;
        }
        let archive = self
            .applications
            .get_archive(application_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let application = self.applications.get(user_id, application_id).await?;
        if self.runtime.is_some() {
            if let Some(snapshot_id) = archive.workspace_snapshot_id {
                self.restore_workspace_from_snapshot(application.workspace_id, snapshot_id)
                    .await?;
            } else {
                self.realize_workspace_handoff(application.workspace_id)
                    .await?;
            }
            for backup_id in [
                archive.dev_database_backup_id,
                archive.prod_database_backup_id,
            ]
            .into_iter()
            .flatten()
            {
                self.restore_archived_database(backup_id).await?;
            }
            self.restore_prior_deployments(user_id, application_id, &archive)
                .await?;
        }
        self.applications
            .commit_restore(user_id, application_id)
            .await?;
        self.applications.get(user_id, application_id).await
    }

    async fn restore_application_target(
        &self,
        archive: &crate::applications::ApplicationArchive,
    ) -> Result<crate::applications::ApprovalTarget, ApplicationError> {
        let mut target = crate::applications::ApprovalTarget {
            application_id: Some(archive.application_id),
            archive_generation: Some(archive.generation),
            workspace_snapshot_id: archive.workspace_snapshot_id,
            dev_release_id: archive.dev_release_id,
            prod_release_id: archive.prod_release_id,
            ..Default::default()
        };
        if let Some(backup_id) = archive.dev_database_backup_id {
            let backup = self.databases.get_backup(backup_id).await?;
            target.dev_database_id = Some(backup.database_id);
            target.dev_backup_id = Some(backup.id);
        }
        if let Some(backup_id) = archive.prod_database_backup_id {
            let backup = self.databases.get_backup(backup_id).await?;
            target.prod_database_id = Some(backup.database_id);
            target.prod_backup_id = Some(backup.id);
        }
        Ok(target)
    }

    async fn restore_workspace_from_snapshot(
        &self,
        workspace_id: Uuid,
        snapshot_id: Uuid,
    ) -> Result<(), ApplicationError> {
        let runtime = self
            .runtime
            .as_ref()
            .ok_or(ApplicationError::WorkspaceBusy)?;
        let snapshot = self.databases.get_workspace_snapshot(snapshot_id).await?;
        if snapshot.workspace_id != workspace_id {
            return Err(ApplicationError::NotFound);
        }
        match runtime.fabric.get_workspace(workspace_id).await {
            Ok(Some(state)) if state == "ready" => {
                sqlx::query("update workspaces set state = 'ready' where id = $1")
                    .bind(workspace_id)
                    .execute(self.applications.pool())
                    .await?;
                return Ok(());
            }
            Ok(_) => {}
            Err(_) => return Err(ApplicationError::WorkspaceBusy),
        }
        let hex = hex_sha(&snapshot.content_hash);
        runtime
            .fabric
            .put_workspace_restore_from_blob(
                workspace_id,
                &hex,
                &runtime.blob,
                &snapshot.object_key,
            )
            .await
            .map_err(|_| ApplicationError::WorkspaceBusy)?;
        let allocated: i64 =
            sqlx::query_scalar("select allocated_bytes from workspaces where id = $1")
                .bind(workspace_id)
                .fetch_one(self.applications.pool())
                .await
                .unwrap_or(crate::storage::WORKSPACE_BYTES);
        let operation_id = typed_operation_id(b"voie-archive-restore-ws:", snapshot_id);
        match runtime
            .fabric
            .restore_workspace(
                workspace_id,
                operation_id,
                &hex,
                &hex,
                Some(allocated.max(0) as u64),
                None,
            )
            .await
        {
            Ok(crate::fabric_client::CreateOutcome::Created) => {
                sqlx::query("update workspaces set state = 'ready' where id = $1")
                    .bind(workspace_id)
                    .execute(self.applications.pool())
                    .await?;
                Ok(())
            }
            Ok(crate::fabric_client::CreateOutcome::Unknown) | Err(_) => {
                Err(ApplicationError::WorkspaceBusy)
            }
        }
    }

    /// Restores an archived Database onto a candidate LV. Does not first
    /// create an empty live Database volume: that would charge the product
    /// budget twice and can fail when only the recovery reserve is free.
    async fn restore_archived_database(&self, backup_id: Uuid) -> Result<(), ApplicationError> {
        let backup = self.databases.get_backup(backup_id).await?;
        let runtime = self
            .runtime
            .as_ref()
            .ok_or(ApplicationError::WorkspaceBusy)?;
        match runtime
            .fabric
            .product_get(&format!("/v1/databases/{}", backup.database_id))
            .await
        {
            Ok(outcome) if outcome.state == "ready" => {
                if let Ok(database) = self.databases.get_internal(backup.database_id).await {
                    if let Some(secret_id) = database.credential_secret_id {
                        let _ = self
                            .databases
                            .mark_ready(backup.database_id, secret_id)
                            .await;
                    } else {
                        sqlx::query(
                            "update application_databases set state = 'ready' \
                             where id = $1 and state <> 'deleted'",
                        )
                        .bind(backup.database_id)
                        .execute(self.applications.pool())
                        .await?;
                    }
                }
                return Ok(());
            }
            Ok(_) => {}
            Err(FabricError::Response) => {}
            Err(_) => return Err(ApplicationError::WorkspaceBusy),
        }
        sqlx::query(
            "update application_databases set state = 'creating' where id = $1 and state <> 'deleted'",
        )
        .bind(backup.database_id)
        .execute(self.applications.pool())
        .await?;
        let restore_op = typed_operation_id(b"voie-archive-restore-db:", backup_id);
        self.complete_database_restore(backup.database_id, backup_id, restore_op)
            .await?;
        let database = self.databases.get_internal(backup.database_id).await?;
        if let Some(secret_id) = database.credential_secret_id {
            let _ = self
                .databases
                .mark_ready(backup.database_id, secret_id)
                .await;
        } else {
            sqlx::query(
                "update application_databases set state = 'ready' where id = $1 and state = 'creating'",
            )
            .bind(backup.database_id)
            .execute(self.applications.pool())
            .await?;
        }
        Ok(())
    }

    async fn restore_prior_deployments(
        &self,
        user_id: Uuid,
        application_id: Uuid,
        archive: &crate::applications::ApplicationArchive,
    ) -> Result<(), ApplicationError> {
        let envs = self
            .applications
            .environments(user_id, application_id)
            .await?;
        for env in envs {
            let release_id = match env.kind.as_str() {
                "dev" => archive.dev_release_id,
                "prod" => archive.prod_release_id,
                _ => None,
            };
            let Some(release_id) = release_id else {
                continue;
            };
            let intent = typed_operation_id_generation(
                b"voie-archive-restore-deploy:",
                env.id,
                release_id,
                archive.generation,
            );
            let (begin, existing) = self
                .deployments
                .deploy_for_restore(user_id, env.id, release_id, intent)
                .await?;
            let deployment_id = match begin {
                crate::deployments::BeginDeployment::ReadyToDispatch { id } => {
                    self.materialize_dispatched_deployment(id).await;
                    id
                }
                crate::deployments::BeginDeployment::Active { id } => id,
                crate::deployments::BeginDeployment::OutcomeUnknown => {
                    if existing.state == "materializing" {
                        self.materialize_dispatched_deployment(existing.id).await;
                    }
                    existing.id
                }
                _ => return Err(ApplicationError::WorkspaceBusy),
            };
            let current = self.deployments.get_internal(deployment_id).await?;
            if current.state == "active" {
                continue;
            }
            if current.state != "healthy" {
                self.probe_and_mark_healthy(deployment_id).await;
            }
            let current = self.deployments.get_internal(deployment_id).await?;
            if current.state != "healthy" && current.state != "active" {
                return Err(ApplicationError::WorkspaceBusy);
            }
            if current.state == "healthy" {
                self.activate_deployment(user_id, deployment_id).await?;
            }
        }
        Ok(())
    }

    async fn capture_archive_release_ids(
        &self,
        application_id: Uuid,
        points: &mut ArchiveRestorePoints,
    ) -> Result<(), ApplicationError> {
        if points.dev_release_id.is_some() && points.prod_release_id.is_some() {
            return Ok(());
        }
        let release_rows = sqlx::query(
            "select e.kind, d.release_id \
             from application_environments e \
             left join application_deployments d on d.id = e.active_deployment_id \
             where e.application_id = $1",
        )
        .bind(application_id)
        .fetch_all(self.applications.pool())
        .await?;
        for row in release_rows {
            let kind: String = row.get("kind");
            let release_id: Option<Uuid> = row.get("release_id");
            match kind.as_str() {
                "dev" if points.dev_release_id.is_none() => points.dev_release_id = release_id,
                "prod" if points.prod_release_id.is_none() => points.prod_release_id = release_id,
                _ => {}
            }
        }
        Ok(())
    }

    async fn capture_archive_restore_points(
        &self,
        user_id: Uuid,
        application_id: Uuid,
        cleanup: &crate::applications::ApplicationCleanup,
        points: &mut ArchiveRestorePoints,
        generation: i64,
    ) -> Result<(), ApplicationError> {
        let Some(_runtime) = self.runtime.as_ref() else {
            return Ok(());
        };
        if points.workspace_snapshot_id.is_none() {
            if let Some(workspace_id) = cleanup.workspace_id {
                points.workspace_snapshot_id = Some(
                    self.snapshot_workspace_to_blob(workspace_id, "archive", Some(generation))
                        .await?,
                );
                self.applications
                    .persist_archive_restore_points(
                        application_id,
                        points.workspace_snapshot_id,
                        points.dev_database_backup_id,
                        points.prod_database_backup_id,
                        points.dev_release_id,
                        points.prod_release_id,
                    )
                    .await?;
            }
        }
        let rows = sqlx::query(
            "select d.id, e.kind from application_databases d \
             join application_environments e on e.id = d.environment_id \
             where d.application_id = $1 and d.state <> 'deleted'",
        )
        .bind(application_id)
        .fetch_all(self.applications.pool())
        .await?;
        for row in rows {
            let database_id: Uuid = row.get("id");
            let kind: String = row.get("kind");
            let already = match kind.as_str() {
                "dev" => points.dev_database_backup_id.is_some(),
                "prod" => points.prod_database_backup_id.is_some(),
                _ => true,
            };
            if already {
                continue;
            }
            let backup = self
                .backup_database_to_blob(user_id, database_id, "archive")
                .await?;
            match kind.as_str() {
                "dev" => points.dev_database_backup_id = Some(backup.id),
                "prod" => points.prod_database_backup_id = Some(backup.id),
                _ => {}
            }
            self.applications
                .persist_archive_restore_points(
                    application_id,
                    points.workspace_snapshot_id,
                    points.dev_database_backup_id,
                    points.prod_database_backup_id,
                    points.dev_release_id,
                    points.prod_release_id,
                )
                .await?;
        }
        Ok(())
    }

    pub(super) async fn snapshot_workspace_to_blob(
        &self,
        workspace_id: Uuid,
        kind: &str,
        archive_generation: Option<i64>,
    ) -> Result<Uuid, ApplicationError> {
        let runtime = self
            .runtime
            .as_ref()
            .ok_or(ApplicationError::WorkspaceBusy)?;
        let operation_id = self
            .databases
            .begin_workspace_snapshot(workspace_id, kind, archive_generation)
            .await?;
        let key =
            crate::databases::DatabaseStore::workspace_snapshot_key(workspace_id, operation_id);
        if let Some(id) = self.databases.snapshot_by_object_key(&key).await? {
            runtime
                .fabric
                .ack_workspace_snapshot(workspace_id, operation_id)
                .await
                .map_err(|_| ApplicationError::WorkspaceBusy)?;
            self.databases
                .complete_workspace_snapshot(workspace_id, operation_id)
                .await?;
            if kind != "archive" {
                self.reclaim_expired_snapshots(workspace_id).await;
            }
            return Ok(id);
        }
        let hash = format!("snapshot:{workspace_id}:{operation_id}");
        let allocated: i64 =
            sqlx::query_scalar("select allocated_bytes from workspaces where id = $1")
                .bind(workspace_id)
                .fetch_optional(self.applications.pool())
                .await?
                .unwrap_or(crate::storage::WORKSPACE_BYTES);
        let (digest, byte_length) = match runtime
            .fabric
            .snapshot_workspace(workspace_id, operation_id, &hash)
            .await
        {
            Ok(response) => {
                let stream = response.bytes_stream();
                runtime
                    .blob
                    .put_stream_if_absent(&key, stream, Some(allocated.max(0) as u64))
                    .await
                    .map_err(|_| ApplicationError::WorkspaceBusy)?
            }
            Err(FabricError::OutcomeUnknown) => {
                match runtime.blob.digest_if_present(&key).await.ok().flatten() {
                    Some(value) => value,
                    None => {
                        self.databases
                            .unknown_workspace_snapshot(workspace_id, operation_id)
                            .await?;
                        return Err(ApplicationError::WorkspaceBusy);
                    }
                }
            }
            Err(_) => runtime
                .blob
                .digest_if_present(&key)
                .await
                .ok()
                .flatten()
                .ok_or(ApplicationError::WorkspaceBusy)?,
        };
        if byte_length == 0 {
            return Err(ApplicationError::WorkspaceBusy);
        }
        let id = self
            .databases
            .record_workspace_snapshot(workspace_id, &key, &digest, byte_length as i64, kind)
            .await?;
        runtime
            .fabric
            .ack_workspace_snapshot(workspace_id, operation_id)
            .await
            .map_err(|_| ApplicationError::WorkspaceBusy)?;
        self.databases
            .complete_workspace_snapshot(workspace_id, operation_id)
            .await?;
        if kind != "archive" {
            self.reclaim_expired_snapshots(workspace_id).await;
        }
        Ok(id)
    }

    async fn backup_database_to_blob(
        &self,
        user_id: Uuid,
        database_id: Uuid,
        kind: &str,
    ) -> Result<crate::databases::Backup, ApplicationError> {
        self.release_abandoned_backup_claims().await?;
        let operation_id = if let Some(existing) = self
            .databases
            .dispatched_backup_operation(database_id)
            .await?
        {
            existing
        } else {
            let operation_id = Uuid::new_v4();
            let request_hash = crate::applications::request_hash(&[
                b"backup",
                database_id.as_bytes(),
                operation_id.as_bytes(),
            ]);
            self.databases
                .begin_backup(user_id, database_id, operation_id, &request_hash)
                .await?;
            operation_id
        };
        self.finish_database_backup(database_id, operation_id, kind)
            .await
    }

    /// A dispatched Control backup whose Fabric database never became ready
    /// cannot occupy the project inflight cap. Creating/unknown Fabric
    /// identity or a GET miss is positive evidence the dump cannot run.
    async fn release_abandoned_backup_claims(&self) -> Result<(), ApplicationError> {
        let Some(runtime) = self.runtime.as_ref() else {
            return Ok(());
        };
        let rows = sqlx::query(
            "select database_id, operation_id from database_operations \
             where kind = 'backup' and state = 'dispatched'",
        )
        .fetch_all(self.applications.pool())
        .await?;
        for row in rows {
            let database_id: Uuid = row.get("database_id");
            let operation_id: Uuid = row.get("operation_id");
            match runtime
                .fabric
                .product_get(&format!("/v1/databases/{database_id}"))
                .await
            {
                Ok(outcome)
                    if matches!(
                        outcome.state.as_str(),
                        "creating" | "unknown" | "failed" | "restore"
                    ) =>
                {
                    self.databases
                        .unknown_backup(database_id, operation_id)
                        .await?;
                }
                Err(FabricError::Response) => {
                    self.databases
                        .unknown_backup(database_id, operation_id)
                        .await?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn finish_database_backup(
        &self,
        database_id: Uuid,
        operation_id: Uuid,
        kind: &str,
    ) -> Result<crate::databases::Backup, ApplicationError> {
        let runtime = self
            .runtime
            .as_ref()
            .ok_or(ApplicationError::WorkspaceBusy)?;
        let database = self.databases.get_internal(database_id).await?;
        let key = crate::databases::DatabaseStore::backup_key(database_id, operation_id);
        if let Some(existing) = self.databases.backup_by_object_key(&key).await? {
            runtime
                .fabric
                .ack_database_backup(database_id, operation_id)
                .await
                .map_err(|_| ApplicationError::WorkspaceBusy)?;
            self.databases
                .complete_backup(database_id, operation_id)
                .await?;
            self.reclaim_expired_backups(database_id).await;
            return Ok(existing);
        }
        let fabric_hash = hex_sha(operation_id.as_bytes());
        let (digest, byte_length) = match runtime
            .fabric
            .backup_database(database_id, operation_id, &fabric_hash)
            .await
        {
            Ok(response) => {
                let stream = response.bytes_stream();
                runtime
                    .blob
                    .put_stream_if_absent(&key, stream, Some(database.storage_bytes.max(0) as u64))
                    .await
                    .map_err(|_| ApplicationError::WorkspaceBusy)?
            }
            Err(FabricError::OutcomeUnknown) => {
                match runtime.blob.digest_if_present(&key).await.ok().flatten() {
                    Some(value) => value,
                    None => {
                        self.databases
                            .unknown_backup(database_id, operation_id)
                            .await?;
                        return Err(ApplicationError::WorkspaceBusy);
                    }
                }
            }
            Err(_) => runtime
                .blob
                .digest_if_present(&key)
                .await
                .ok()
                .flatten()
                .ok_or(ApplicationError::WorkspaceBusy)?,
        };
        if byte_length == 0 {
            return Err(ApplicationError::WorkspaceBusy);
        }
        let backup = self
            .databases
            .record_backup(database_id, &key, &digest, byte_length as i64, kind)
            .await?;
        runtime
            .fabric
            .ack_database_backup(database_id, operation_id)
            .await
            .map_err(|_| ApplicationError::WorkspaceBusy)?;
        self.databases
            .complete_backup(database_id, operation_id)
            .await?;
        self.reclaim_expired_backups(database_id).await;
        Ok(backup)
    }

    async fn reclaim_expired_snapshots(&self, workspace_id: Uuid) {
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        if let Ok(expired) = self.databases.expired_snapshots(workspace_id).await {
            for snapshot in expired {
                if runtime.blob.delete(&snapshot.object_key).await.is_ok() {
                    let _ = self.databases.drop_snapshot(snapshot.id).await;
                }
            }
        }
    }

    async fn reclaim_expired_backups(&self, database_id: Uuid) {
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        if let Ok(expired) = self.databases.expired_backups(database_id).await {
            for backup in expired {
                if runtime.blob.delete(&backup.object_key).await.is_ok() {
                    let _ = self.databases.drop_backup(backup.id).await;
                }
            }
        }
    }

    async fn reclaim_unpinned_recovery(&self, application_id: Uuid) {
        let workspace_id: Option<Uuid> =
            sqlx::query_scalar("select workspace_id from applications where id = $1")
                .bind(application_id)
                .fetch_optional(self.applications.pool())
                .await
                .ok()
                .flatten();
        if let Some(workspace_id) = workspace_id {
            self.reclaim_expired_snapshots(workspace_id).await;
        }
        let databases: Vec<Uuid> =
            sqlx::query_scalar("select id from application_databases where application_id = $1")
                .bind(application_id)
                .fetch_all(self.applications.pool())
                .await
                .unwrap_or_default();
        for database_id in databases {
            self.reclaim_expired_backups(database_id).await;
        }
    }

    /// Stages the artifact on Fabric as a Deployment cache. A ready Release
    /// is Blob + PostgreSQL metadata; there is no permanent Release LV.
    /// Bytes stream Blob → Fabric; control never holds the pack as `Vec<u8>`.
    async fn stage_release_cache(
        runtime: &ProductRuntime,
        release_id: Uuid,
        hex: &str,
        object_key: &str,
    ) -> Result<(), FabricError> {
        runtime
            .fabric
            .put_release_from_blob(release_id, hex, &runtime.blob, object_key)
            .await
    }
}

#[derive(Default)]
struct ArchiveRestorePoints {
    workspace_snapshot_id: Option<Uuid>,
    dev_database_backup_id: Option<Uuid>,
    prod_database_backup_id: Option<Uuid>,
    dev_release_id: Option<Uuid>,
    prod_release_id: Option<Uuid>,
}

fn hex_sha(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn require_cleanup_outcome(
    outcome: Result<crate::fabric_client::ProductOutcome, FabricError>,
) -> Result<(), ApplicationError> {
    match outcome {
        Ok(outcome) if outcome.state == "unknown" => Err(ApplicationError::WorkspaceBusy),
        Ok(_) => Ok(()),
        Err(FabricError::Transport)
        | Err(FabricError::OutcomeUnknown)
        | Err(FabricError::Response) => Err(ApplicationError::WorkspaceBusy),
        Err(_) => Err(ApplicationError::Kernel(crate::KernelError::Database)),
    }
}

/// Stop/delete journal Conflict is retryable. Unknown is not: C5 forbids
/// replaying ambiguous effects, and begin remaps leftover dispatched.
async fn retry_cleanup_mutate(
    runtime: &ProductRuntime,
    path: String,
    body: Value,
) -> Result<(), ApplicationError> {
    const ATTEMPTS: u32 = 6;
    for attempt in 0..ATTEMPTS {
        match runtime.fabric.product_mutate(&path, &body).await {
            Ok(outcome) if outcome.state == "unknown" => {
                return Err(ApplicationError::WorkspaceBusy);
            }
            Ok(_) => return Ok(()),
            Err(FabricError::Response) | Err(FabricError::Transport) if attempt + 1 < ATTEMPTS => {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            other => return require_cleanup_outcome(other),
        }
    }
    Err(ApplicationError::WorkspaceBusy)
}

fn typed_operation_id(namespace: &[u8], id: Uuid) -> Uuid {
    use sha2::{Digest, Sha256};
    let digest: [u8; 32] = {
        let mut hasher = Sha256::new();
        hasher.update(namespace);
        hasher.update(id.as_bytes());
        hasher.finalize().into()
    };
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes)
}

fn typed_operation_id_pair(namespace: &[u8], a: Uuid, b: Uuid) -> Uuid {
    use sha2::{Digest, Sha256};
    let digest: [u8; 32] = {
        let mut hasher = Sha256::new();
        hasher.update(namespace);
        hasher.update(a.as_bytes());
        hasher.update(b.as_bytes());
        hasher.finalize().into()
    };
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes)
}

fn typed_operation_id_generation(
    namespace: &[u8],
    a: Uuid,
    b: Uuid,
    generation: i64,
) -> Uuid {
    use sha2::{Digest, Sha256};
    let digest: [u8; 32] = {
        let mut hasher = Sha256::new();
        hasher.update(namespace);
        hasher.update(a.as_bytes());
        hasher.update(b.as_bytes());
        hasher.update(generation.to_le_bytes());
        hasher.finalize().into()
    };
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes)
}

fn migrate_operation_id(deployment_id: Uuid) -> Uuid {
    typed_operation_id(b"voie-migrate:", deployment_id)
}

fn manifest_run_argv(manifest: &Value) -> Vec<String> {
    manifest
        .get("run")
        .and_then(|run| run.get("command"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .filter(|items: &Vec<String>| !items.is_empty())
        .unwrap_or_else(|| vec!["true".into()])
}

fn manifest_port(manifest: &Value) -> u16 {
    manifest
        .get("run")
        .and_then(|run| run.get("port"))
        .and_then(Value::as_u64)
        .unwrap_or(3000) as u16
}

fn manifest_health(manifest: &Value) -> String {
    manifest
        .get("run")
        .and_then(|run| run.get("healthPath"))
        .and_then(Value::as_str)
        .unwrap_or("/healthz")
        .to_owned()
}

fn manifest_cpu_millis(manifest: &Value) -> u32 {
    clamp_resource(
        manifest
            .get("resources")
            .and_then(|resources| resources.get("cpuMillis"))
            .and_then(Value::as_u64),
        crate::applications::DEFAULT_CPU_MILLIS,
        crate::applications::MIN_CPU_MILLIS,
        crate::applications::MAX_CPU_MILLIS,
    )
}

fn manifest_memory_mb(manifest: &Value) -> u32 {
    clamp_resource(
        manifest
            .get("resources")
            .and_then(|resources| resources.get("memoryMb"))
            .and_then(Value::as_u64),
        crate::applications::DEFAULT_MEMORY_MB,
        crate::applications::MIN_MEMORY_MB,
        crate::applications::MAX_MEMORY_MB,
    )
}

fn clamp_resource(value: Option<u64>, default: u32, min: u32, max: u32) -> u32 {
    value
        .and_then(|number| u32::try_from(number).ok())
        .filter(|number| (min..=max).contains(number))
        .unwrap_or(default)
}

fn manifest_build_argv(manifest: &Value) -> Vec<String> {
    manifest
        .get("build")
        .and_then(|build| build.get("command"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn manifest_test_argv(manifest: &Value) -> Option<Vec<String>> {
    manifest
        .get("test")
        .and_then(|test| test.get("command"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .filter(|items: &Vec<String>| !items.is_empty())
}

fn manifest_migrate_argv(manifest: &Value) -> Option<Vec<String>> {
    manifest
        .get("database")
        .and_then(|database| database.get("migrationCommand"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .filter(|items: &Vec<String>| !items.is_empty())
}

fn profile1_workspace_image(image: &str) -> bool {
    let name = image.rsplit('/').next().unwrap_or(image);
    name == "voie-workspace:v1" || name.starts_with("voie-workspace:v1@")
}

fn profile0_runner_image(image: &str) -> bool {
    let name = image.rsplit('/').next().unwrap_or(image);
    name == "voie-runner:c1" || name.starts_with("voie-runner:c1@")
}

#[cfg(test)]
mod tests {
    use super::{
        profile0_runner_image, profile1_workspace_image, typed_operation_id_generation,
        typed_operation_id_pair,
    };
    use uuid::Uuid;

    #[test]
    fn profile_images_are_versioned_names_not_user_tags() {
        assert!(profile1_workspace_image("voie-workspace:v1"));
        assert!(profile1_workspace_image("localhost/voie-workspace:v1"));
        assert!(profile0_runner_image("voie-runner:c1"));
        assert!(profile0_runner_image("voie-runner:c1@sha256:abc"));
        assert!(!profile1_workspace_image("voie-runner:c1"));
        assert!(!profile0_runner_image("voie-workspace:v1"));
        assert!(!profile0_runner_image("evil:latest"));
        assert!(!profile1_workspace_image("voie-workspace:v2"));
    }

    #[test]
    fn archive_restore_deploy_intent_is_scoped_to_generation() {
        let env = Uuid::from_u128(1);
        let release = Uuid::from_u128(2);
        let gen1 = typed_operation_id_generation(
            b"voie-archive-restore-deploy:",
            env,
            release,
            1,
        );
        let gen2 = typed_operation_id_generation(
            b"voie-archive-restore-deploy:",
            env,
            release,
            2,
        );
        let unscoped =
            typed_operation_id_pair(b"voie-archive-restore-deploy:", env, release);
        assert_ne!(gen1, gen2);
        assert_ne!(unscoped, gen2);
        assert_eq!(
            gen2,
            typed_operation_id_generation(b"voie-archive-restore-deploy:", env, release, 2),
        );
    }
}
