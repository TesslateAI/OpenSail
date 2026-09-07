//! Dedicated PostgreSQL Database per Application Environment.

use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::applications::{self, ApplicationError};
use crate::auth::Action;
use crate::session_store::BlobStore;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Oldest backups beyond this bound are dropped after a successful record.
pub const MAX_BACKUPS_PER_DATABASE: i64 = crate::storage::BACKUP_RETENTION;
pub const MAX_INFLIGHT_BACKUPS_PER_DATABASE: i64 =
    crate::storage::MAX_INFLIGHT_BACKUPS_PER_DATABASE;
pub const MAX_INFLIGHT_BACKUPS_PER_PROJECT: i64 = crate::storage::MAX_INFLIGHT_BACKUPS_PER_PROJECT;

/// Empty or non-UTF8 Key Vault material cannot provision. This is a
/// deterministic failure, not an unknown dispatched effect. Desired
/// present stays so a later usable secret can still provision.
const FAIL_CLOSED_CREATING_SQL: &str = "update application_databases \
             set observed_state = 'failed', last_error_code = 'secret_material_unavailable' \
             where id = $1 and desired_state = 'present'";

/// Blob object keys that still hold recoverable plaintext.
pub fn recoverable_blob_key(object_key: &str) -> bool {
    !object_key.is_empty() && !object_key.starts_with("reclaimed/")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Database {
    pub id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub engine: String,
    pub engine_profile: String,
    pub fabric_id: Uuid,
    pub state: String,
    pub desired_revision: i64,
    pub observed_revision: i64,
    pub credential_secret_id: Option<Uuid>,
    pub storage_bytes: i64,
    pub storage_tier: String,
    pub desired_state: String,
    pub observed_state: String,
    pub last_error_code: Option<String>,
    pub security_profile: i32,
    pub created_at: String,
}

impl Database {
    /// HTTP `state` is not the leftover process column. Desired `absent`
    /// presents as `deleted` once observed absent. Observed `present`/`ready`
    /// presents as `ready`. Leftover process `ready` is not product authority.
    pub fn wire_state(&self) -> &str {
        if self.desired_state == "absent" {
            return if self.observed_state == "absent" || self.observed_state == "deleted" {
                "deleted"
            } else {
                "deleting"
            };
        }
        if self.observed_state == "present" || self.observed_state == "ready" {
            "ready"
        } else {
            "creating"
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSnapshot {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub object_key: String,
    pub content_hash: Vec<u8>,
    pub byte_length: i64,
    pub kind: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backup {
    pub id: Uuid,
    pub database_id: Uuid,
    pub object_key: String,
    pub content_hash: Vec<u8>,
    pub byte_length: i64,
    pub kind: String,
    pub created_at: String,
}

#[derive(Clone)]
pub struct DatabaseStore {
    pool: PgPool,
}

impl DatabaseStore {
    pub fn new(pool: PgPool) -> Self {
        DatabaseStore { pool }
    }

    pub async fn create(
        &self,
        actor_user_id: Uuid,
        environment_id: Uuid,
        fabric_id: Uuid,
        operation_id: Uuid,
        request_hash: &[u8; 32],
    ) -> Result<Database, ApplicationError> {
        self.create_with_tier(
            actor_user_id,
            environment_id,
            fabric_id,
            operation_id,
            request_hash,
            false,
            None,
        )
        .await
    }

    pub async fn create_with_tier(
        &self,
        actor_user_id: Uuid,
        environment_id: Uuid,
        fabric_id: Uuid,
        operation_id: Uuid,
        request_hash: &[u8; 32],
        elevated: bool,
        approval_id: Option<Uuid>,
    ) -> Result<Database, ApplicationError> {
        let environment = applications::load_environment(&self.pool, environment_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let create_action = if environment.kind == "dev" && !elevated {
            Action::DeployDev
        } else {
            Action::ManageProduction
        };
        applications::ApplicationStore::new(self.pool.clone(), String::new())
            .require_in_project(actor_user_id, environment.application_id, create_action)
            .await?;
        let project_id: Uuid =
            sqlx::query_scalar("select project_id from applications where id = $1")
                .bind(environment.application_id)
                .fetch_one(&self.pool)
                .await?;
        let tier_target = applications::ApprovalTarget {
            application_id: Some(environment.application_id),
            environment_id: Some(environment_id),
            ..Default::default()
        };
        if elevated && approval_id.is_none() {
            applications::require_approval(
                &self.pool,
                None,
                project_id,
                "increase_resource_tier",
                &tier_target,
                actor_user_id,
            )
            .await?;
        }
        if let Some(existing) = self.by_environment(environment_id).await? {
            return Ok(existing);
        }
        let storage_bytes = crate::storage::database_bytes(environment.kind == "prod", elevated);
        let storage_tier = if elevated { "elevated" } else { "default" };
        let mut tx = self.pool.begin().await?;
        applications::claim_actor(
            &mut tx,
            actor_user_id,
            environment.application_id,
            create_action,
        )
        .await?;
        if elevated {
            let Some(approval_id) = approval_id else {
                return Err(ApplicationError::Auth);
            };
            applications::require_approval_tx(
                &mut tx,
                approval_id,
                project_id,
                "increase_resource_tier",
                &tier_target,
                actor_user_id,
            )
            .await?;
        }
        let database_id = Uuid::new_v4();
        // Occupancy and HTTP `state` use desired/observed. Schema default
        // satisfies leftover process CHECK; this mutation persists desired.
        let row = sqlx::query(
            "insert into application_databases \
             (id, application_id, environment_id, engine_profile, fabric_id, storage_bytes, storage_tier, \
              desired_state, desired_revision, security_profile) \
             values ($1, $2, $3, 'voie-postgres:v1', $4, $5, $6, 'present', 1, 1) \
             on conflict (environment_id) do nothing \
             returning id, application_id, environment_id, engine, engine_profile, fabric_id, \
                       state, desired_revision, observed_revision, credential_secret_id, \
                       storage_bytes, storage_tier, desired_state, observed_state, last_error_code, security_profile, \
                       created_at::text as created_at",
        )
        .bind(database_id)
        .bind(environment.application_id)
        .bind(environment_id)
        .bind(fabric_id)
        .bind(storage_bytes)
        .bind(storage_tier)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.rollback().await.ok();
            return self
                .by_environment(environment_id)
                .await?
                .ok_or(ApplicationError::WorkspaceBusy);
        };
        // Present is a reconciler. One row per Environment is create
        // idempotency; backup/restore/migrate keep the at-most-once journal.
        let _ = (operation_id, request_hash);
        tx.commit().await?;
        Ok(row_database(row))
    }

    /// Claims the platform credential UUID, or returns the row that already
    /// won. This id addresses Key Vault material, not a `user_secrets` row.
    /// Release 0 has one Database credential identity.
    pub async fn attach_credential(
        &self,
        database_id: Uuid,
        secret_id: Uuid,
    ) -> Result<Uuid, ApplicationError> {
        let claimed: Option<Uuid> = sqlx::query_scalar(
            "update application_databases set credential_secret_id = $2 \
             where id = $1 and credential_secret_id is null \
             returning credential_secret_id",
        )
        .bind(database_id)
        .bind(secret_id)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(winner) = claimed {
            return Ok(winner);
        }
        sqlx::query_scalar("select credential_secret_id from application_databases where id = $1")
            .bind(database_id)
            .fetch_optional(&self.pool)
            .await?
            .flatten()
            .ok_or(ApplicationError::NotFound)
    }

    pub async fn mark_ready(
        &self,
        database_id: Uuid,
        secret_id: Uuid,
    ) -> Result<Database, ApplicationError> {
        sqlx::query(
            "update application_databases set observed_state = 'ready', \
                    credential_secret_id = coalesce(credential_secret_id, $2), \
                    observed_revision = case \
                      when desired_revision > greatest(observed_revision, 1) \
                        then greatest(observed_revision, 1) \
                      else desired_revision \
                    end, \
                    last_error_code = null, \
                    reconcile_after = now() + ($3 * interval '1 second') \
             where id = $1 and desired_state <> 'absent'",
        )
        .bind(database_id)
        .bind(secret_id)
        .bind(crate::reconcile::OBSERVE_AFTER_SECS)
        .execute(&self.pool)
        .await?;
        self.get_internal(database_id).await
    }

    /// Persist desired `security_profile` 2. Repeatable; not a journaled
    /// `database/secure` operation. Observed PostgreSQL roles remain the
    /// authority for SecurityReady.
    pub async fn set_security_profile(
        &self,
        actor_user_id: Uuid,
        database_id: Uuid,
        security_profile: i32,
    ) -> Result<Database, ApplicationError> {
        if security_profile != 2 {
            return Err(ApplicationError::InvalidSecurityProfile);
        }
        let database = self.get_internal(database_id).await?;
        if database.desired_state == "absent" {
            return Err(ApplicationError::NotFound);
        }
        let environment = applications::load_environment(&self.pool, database.environment_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let action = if environment.kind == "dev" {
            Action::DeployDev
        } else {
            Action::ManageProduction
        };
        applications::ApplicationStore::new(self.pool.clone(), String::new())
            .require_in_project(actor_user_id, database.application_id, action)
            .await?;
        if database.security_profile == 2 {
            return Ok(database);
        }
        if database.security_profile != 1 {
            return Err(ApplicationError::InvalidSecurityProfile);
        }
        let mut tx = self.pool.begin().await?;
        applications::claim_actor(&mut tx, actor_user_id, database.application_id, action).await?;
        let row = sqlx::query(
            "update application_databases \
             set security_profile = 2, desired_revision = desired_revision + 1 \
             where id = $1 and security_profile = 1 \
               and desired_state <> 'absent' \
             returning id, application_id, environment_id, engine, engine_profile, fabric_id, \
                       state, desired_revision, observed_revision, credential_secret_id, \
                       storage_bytes, storage_tier, desired_state, observed_state, last_error_code, security_profile, \
                       created_at::text as created_at",
        )
        .bind(database_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.commit().await?;
            let current = self.get_internal(database_id).await?;
            if current.security_profile == 2 {
                return Ok(current);
            }
            return Err(ApplicationError::NotFound);
        };
        tx.commit().await?;
        Ok(row_database(row))
    }

    /// Release 0 desired security is profile 2. Control applies this without
    /// an actor so leftover profile-1 rows converge after deploy. Lost is
    /// still advanced so the desired contract is visible; it does not remint.
    pub async fn advance_release0_security_profile(
        &self,
        database_id: Uuid,
    ) -> Result<Database, ApplicationError> {
        let row = sqlx::query(
            "update application_databases \
             set security_profile = 2, desired_revision = desired_revision + 1 \
             where id = $1 and security_profile = 1 \
               and desired_state <> 'absent' \
             returning id, application_id, environment_id, engine, engine_profile, fabric_id, \
                       state, desired_revision, observed_revision, credential_secret_id, \
                       storage_bytes, storage_tier, desired_state, observed_state, last_error_code, security_profile, \
                       created_at::text as created_at",
        )
        .bind(database_id)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(row) = row {
            return Ok(row_database(row));
        }
        let current = self.get_internal(database_id).await?;
        if current.security_profile == 2 {
            return Ok(current);
        }
        Err(ApplicationError::NotFound)
    }

    /// Empty or non-UTF8 Key Vault material cannot provision. This is a
    /// deterministic failure, not an unknown dispatched effect. Desired
    /// present stays so a later usable secret can still provision.
    pub async fn fail_closed_creating(&self, database_id: Uuid) -> Result<(), ApplicationError> {
        sqlx::query(FAIL_CLOSED_CREATING_SQL)
            .bind(database_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn record_lost(
        &self,
        database_id: Uuid,
        error: &str,
        observed_revision: Option<i64>,
    ) -> Result<(), ApplicationError> {
        sqlx::query(
            "update application_databases \
             set observed_state = 'lost', last_error_code = $2, \
                 observed_revision = coalesce($4, observed_revision), \
                 reconcile_after = now() + ($3 * interval '1 second') \
             where id = $1",
        )
        .bind(database_id)
        .bind(error)
        .bind(crate::reconcile::OBSERVE_AFTER_SECS)
        .bind(observed_revision)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn unknown(
        &self,
        database_id: Uuid,
        operation_id: Uuid,
    ) -> Result<(), ApplicationError> {
        sqlx::query(
            "update database_operations set state = 'unknown' \
             where database_id = $1 and operation_id = $2 and state = 'dispatched'",
        )
        .bind(database_id)
        .bind(operation_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Fabric 202 / outcome-unknown for a backup: release control admission.
    /// Transport failures stay `dispatched` so the same operation can retry.
    pub async fn unknown_backup(
        &self,
        database_id: Uuid,
        operation_id: Uuid,
    ) -> Result<(), ApplicationError> {
        sqlx::query(
            "update database_operations set state = 'unknown' \
             where database_id = $1 and operation_id = $2 and kind = 'backup' \
               and state = 'dispatched'",
        )
        .bind(database_id)
        .bind(operation_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get(
        &self,
        actor_user_id: Uuid,
        database_id: Uuid,
    ) -> Result<Database, ApplicationError> {
        let database = self.get_internal(database_id).await?;
        applications::ApplicationStore::new(self.pool.clone(), String::new())
            .require_in_project(actor_user_id, database.application_id, Action::ReadProject)
            .await?;
        Ok(database)
    }

    pub fn backup_key(database_id: Uuid, operation_id: Uuid) -> String {
        format!("backups/databases/{database_id}/{operation_id}.pgdump")
    }

    pub fn workspace_snapshot_key(workspace_id: Uuid, operation_id: Uuid) -> String {
        format!("backups/workspaces/{workspace_id}/{operation_id}.tar.zst")
    }

    pub async fn begin_backup(
        &self,
        actor_user_id: Uuid,
        database_id: Uuid,
        operation_id: Uuid,
        request_hash: &[u8; 32],
    ) -> Result<(), ApplicationError> {
        let mut tx = self.pool.begin().await?;
        let application_id: Uuid =
            sqlx::query_scalar("select application_id from application_databases where id = $1")
                .bind(database_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(ApplicationError::NotFound)?;
        let application_state = applications::claim_actor(
            &mut tx,
            actor_user_id,
            application_id,
            Action::ManageProduction,
        )
        .await?;
        sqlx::query_scalar::<_, Uuid>(
            "select id from application_databases where id = $1 for update",
        )
        .bind(database_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApplicationError::NotFound)?;
        let db_inflight: i64 = sqlx::query_scalar(
            "select count(*) from database_operations \
             where database_id = $1 and kind = 'backup' and state = 'dispatched'",
        )
        .bind(database_id)
        .fetch_one(&mut *tx)
        .await?;
        if db_inflight >= MAX_INFLIGHT_BACKUPS_PER_DATABASE {
            return Err(ApplicationError::WorkspaceBusy);
        }
        let project_id: Uuid =
            sqlx::query_scalar("select project_id from applications where id = $1")
                .bind(application_id)
                .fetch_one(&mut *tx)
                .await?;
        let project_inflight: i64 = sqlx::query_scalar(
            "select count(*) from database_operations o \
             join application_databases d on d.id = o.database_id \
             join applications a on a.id = d.application_id \
             where a.project_id = $1 and o.kind = 'backup' and o.state = 'dispatched'",
        )
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await?;
        // Archive capture is already serialized on the Application row.
        // A leftover dispatched backup on another Application must not
        // pin the shared Control inflight cap and block restore-point
        // capture. Fabric staging admission remains the physical gate.
        if application_state != "archiving" && project_inflight >= MAX_INFLIGHT_BACKUPS_PER_PROJECT
        {
            return Err(ApplicationError::WorkspaceBusy);
        }
        let inserted = sqlx::query(
            "insert into database_operations \
             (id, database_id, operation_id, kind, request_hash, state) \
             values ($1, $2, $3, 'backup', $4, 'dispatched') \
             on conflict (database_id, operation_id) do nothing \
             returning id",
        )
        .bind(Uuid::new_v4())
        .bind(database_id)
        .bind(operation_id)
        .bind(request_hash.as_slice())
        .fetch_optional(&mut *tx)
        .await?;
        if inserted.is_none() {
            return Err(ApplicationError::WorkspaceBusy);
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn dispatched_backup_operation(
        &self,
        database_id: Uuid,
    ) -> Result<Option<Uuid>, ApplicationError> {
        let operation_id = sqlx::query_scalar(
            "select operation_id from database_operations \
             where database_id = $1 and kind = 'backup' and state = 'dispatched' \
             order by created_at desc limit 1",
        )
        .bind(database_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(operation_id)
    }

    pub async fn complete_backup(
        &self,
        database_id: Uuid,
        operation_id: Uuid,
    ) -> Result<(), ApplicationError> {
        let updated = sqlx::query(
            "update database_operations set state = 'ready' \
             where database_id = $1 and operation_id = $2 and kind = 'backup' \
               and state = 'dispatched'",
        )
        .bind(database_id)
        .bind(operation_id)
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() == 0 {
            return Err(ApplicationError::WorkspaceBusy);
        }
        Ok(())
    }

    pub async fn begin_restore(
        &self,
        actor_user_id: Uuid,
        database_id: Uuid,
        backup_id: Uuid,
        operation_id: Uuid,
        approval_id: Option<Uuid>,
        request_hash: &[u8; 32],
    ) -> Result<Backup, ApplicationError> {
        let database = self.get_internal(database_id).await?;
        let project_id: Uuid =
            sqlx::query_scalar("select project_id from applications where id = $1")
                .bind(database.application_id)
                .fetch_one(&self.pool)
                .await?;
        let backup = self.get_backup(backup_id).await?;
        if backup.database_id != database_id {
            return Err(ApplicationError::NotFound);
        }
        if !recoverable_blob_key(&backup.object_key) {
            return Err(ApplicationError::NotFound);
        }
        let target = applications::ApprovalTarget {
            application_id: Some(database.application_id),
            environment_id: Some(database.environment_id),
            database_id: Some(database_id),
            backup_id: Some(backup_id),
            ..Default::default()
        };
        let Some(approval_id) = approval_id else {
            applications::require_approval(
                &self.pool,
                None,
                project_id,
                "restore_database",
                &target,
                actor_user_id,
            )
            .await?;
            return Err(ApplicationError::Auth);
        };
        let mut tx = self.pool.begin().await?;
        applications::claim_actor(
            &mut tx,
            actor_user_id,
            database.application_id,
            Action::ManageProduction,
        )
        .await?;
        sqlx::query_scalar::<_, Uuid>(
            "select id from application_databases where id = $1 for update",
        )
        .bind(database_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApplicationError::NotFound)?;
        applications::require_approval_tx(
            &mut tx,
            approval_id,
            project_id,
            "restore_database",
            &target,
            actor_user_id,
        )
        .await?;
        let inserted = sqlx::query(
            "insert into database_operations \
             (id, database_id, operation_id, kind, request_hash, state) \
             values ($1, $2, $3, 'restore', $4, 'dispatched') \
             on conflict (database_id, operation_id) do nothing \
             returning id",
        )
        .bind(Uuid::new_v4())
        .bind(database_id)
        .bind(operation_id)
        .bind(request_hash.as_slice())
        .fetch_optional(&mut *tx)
        .await?;
        if inserted.is_none() {
            return Err(ApplicationError::WorkspaceBusy);
        }
        tx.commit().await?;
        Ok(backup)
    }

    pub async fn get_backup(&self, backup_id: Uuid) -> Result<Backup, ApplicationError> {
        let row = sqlx::query(
            "select id, database_id, object_key, content_hash, byte_length, kind, \
                    created_at::text as created_at \
             from database_backups where id = $1",
        )
        .bind(backup_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(ApplicationError::NotFound)?;
        Ok(Backup {
            id: row.get("id"),
            database_id: row.get("database_id"),
            object_key: row.get("object_key"),
            content_hash: row.get("content_hash"),
            byte_length: row.get("byte_length"),
            kind: row.get("kind"),
            created_at: row.get("created_at"),
        })
    }

    pub async fn get_internal(&self, database_id: Uuid) -> Result<Database, ApplicationError> {
        let row = sqlx::query(
            "select id, application_id, environment_id, engine, engine_profile, fabric_id, \
                    state, desired_revision, observed_revision, credential_secret_id, \
                    storage_bytes, storage_tier, desired_state, observed_state, last_error_code, security_profile, \
                    created_at::text as created_at \
             from application_databases where id = $1",
        )
        .bind(database_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(ApplicationError::NotFound)?;
        Ok(row_database(row))
    }

    pub async fn list_due(&self) -> Result<Vec<Database>, ApplicationError> {
        let rows = sqlx::query(
            "select id, application_id, environment_id, engine, engine_profile, fabric_id, \
                    state, desired_revision, observed_revision, credential_secret_id, \
                    storage_bytes, storage_tier, desired_state, observed_state, last_error_code, security_profile, \
                    created_at::text as created_at \
             from application_databases \
             where desired_revision > observed_revision \
                or (desired_state = 'absent' \
                    and coalesce(nullif(observed_state, ''), '') not in ('absent', 'deleted')) \
                or (desired_state <> 'absent' \
                    and reconcile_after is not null and reconcile_after <= now()) \
             order by created_at, id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_database).collect())
    }

    /// Application delete used to mark Control `deleted` while leaving
    /// `desired_state = present`, so `list_due` never PUT absent and Fabric
    /// leftover postgres stayed. Heal those rows onto the teardown wake.
    pub async fn persist_absent_desired_for_removing_applications(
        &self,
    ) -> Result<(), ApplicationError> {
        sqlx::query(
            "update application_databases d \
             set desired_state = 'absent', \
                 desired_revision = case \
                     when d.desired_state = 'absent' then d.desired_revision \
                     else d.desired_revision + 1 \
                 end, \
                 reconcile_after = now(), \
                 deleted_at = coalesce(d.deleted_at, now()) \
             from applications a \
             where d.application_id = a.id \
               and a.state in ('deleting', 'deleted') \
               and d.desired_state <> 'absent'",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Platform-admin security census. Teardown Applications and absent
    /// desired are not live estate: leftover Present rows must not keep
    /// `/api/admin/health` asking for a Ready postgres.
    pub async fn list_live_census(&self) -> Result<Vec<Database>, ApplicationError> {
        let rows = sqlx::query(
            "select d.id, d.application_id, d.environment_id, d.engine, d.engine_profile, d.fabric_id, \
                    d.state, d.desired_revision, d.observed_revision, d.credential_secret_id, \
                    d.storage_bytes, d.storage_tier, d.desired_state, d.observed_state, d.last_error_code, d.security_profile, \
                    d.created_at::text as created_at \
             from application_databases d \
             inner join applications a on a.id = d.application_id \
             where d.desired_state <> 'absent' \
               and a.state not in ('deleting', 'deleted', 'archiving', 'archived') \
             order by d.created_at, d.id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_database).collect())
    }

    pub async fn convergence_counts(&self) -> Result<(i64, i64, i64), ApplicationError> {
        let row = sqlx::query(
            "select \
                count(*) filter (where desired_state <> 'absent' \
                    and desired_revision = observed_revision \
                    and observed_state not in ('lost', 'failed') \
                    and (last_error_code is null or last_error_code = '') \
                    and ((desired_state = 'present' and observed_state in ('present', 'ready')) \
                         or (desired_state = observed_state)))::bigint as converged, \
                count(*) filter (where desired_state <> 'absent' \
                    and desired_revision > observed_revision \
                    and observed_state <> 'lost' \
                    and (last_error_code is null or last_error_code in ('', 'fabric_unreachable')))::bigint as reconciling, \
                count(*) filter (where desired_state <> 'absent' \
                    and (observed_state in ('lost', 'failed') \
                         or (last_error_code is not null and last_error_code <> '' \
                             and last_error_code <> 'fabric_unreachable')))::bigint as failed \
             from application_databases",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok((
            row.get::<i64, _>("converged"),
            row.get::<i64, _>("reconciling"),
            row.get::<i64, _>("failed"),
        ))
    }

    pub async fn record_reconcile_error(
        &self,
        database_id: Uuid,
        code: &str,
        after_secs: i64,
    ) -> Result<(), ApplicationError> {
        sqlx::query(
            "update application_databases \
             set last_error_code = $2, reconcile_after = now() + ($3 * interval '1 second') \
             where id = $1",
        )
        .bind(database_id)
        .bind(code)
        .bind(after_secs)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn by_environment(
        &self,
        environment_id: Uuid,
    ) -> Result<Option<Database>, ApplicationError> {
        let row = sqlx::query(
            "select id, application_id, environment_id, engine, engine_profile, fabric_id, \
                    state, desired_revision, observed_revision, credential_secret_id, \
                    storage_bytes, storage_tier, desired_state, observed_state, last_error_code, security_profile, \
                    created_at::text as created_at \
             from application_databases \
             where environment_id = $1",
        )
        .bind(environment_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(row_database))
    }

    pub async fn backup_by_object_key(
        &self,
        object_key: &str,
    ) -> Result<Option<Backup>, ApplicationError> {
        let row = sqlx::query(
            "select id, database_id, object_key, content_hash, byte_length, kind, \
                    created_at::text as created_at \
             from database_backups where object_key = $1",
        )
        .bind(object_key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(row_backup))
    }

    pub async fn snapshot_by_object_key(
        &self,
        object_key: &str,
    ) -> Result<Option<Uuid>, ApplicationError> {
        let id = sqlx::query_scalar("select id from workspace_snapshots where object_key = $1")
            .bind(object_key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(id)
    }

    pub async fn record_backup(
        &self,
        database_id: Uuid,
        object_key: &str,
        content_hash: &[u8; 32],
        byte_length: i64,
        kind: &str,
    ) -> Result<Backup, ApplicationError> {
        if let Some(existing) = sqlx::query(
            "select id, database_id, object_key, content_hash, byte_length, kind, \
                    created_at::text as created_at \
             from database_backups where object_key = $1",
        )
        .bind(object_key)
        .fetch_optional(&self.pool)
        .await?
        {
            let backup = row_backup(existing);
            if backup.database_id == database_id
                && backup.content_hash.as_slice() == content_hash.as_slice()
                && backup.byte_length == byte_length
            {
                return Ok(backup);
            }
            return Err(ApplicationError::Kernel(crate::KernelError::Conflict));
        }
        let row = sqlx::query(
            "insert into database_backups \
             (id, database_id, object_key, content_hash, byte_length, kind) \
             values ($1, $2, $3, $4, $5, $6) \
             returning id, database_id, object_key, content_hash, byte_length, kind, \
                       created_at::text as created_at",
        )
        .bind(Uuid::new_v4())
        .bind(database_id)
        .bind(object_key)
        .bind(content_hash.as_slice())
        .bind(byte_length)
        .bind(kind)
        .fetch_one(&self.pool)
        .await?;
        Ok(row_backup(row))
    }

    pub async fn pin_backup(&self, backup_id: Uuid) -> Result<(), ApplicationError> {
        sqlx::query("update database_backups set pinned = true where id = $1")
            .bind(backup_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn begin_workspace_snapshot(
        &self,
        workspace_id: Uuid,
        purpose: &str,
        archive_generation: Option<i64>,
    ) -> Result<Uuid, ApplicationError> {
        let mut tx = self.pool.begin().await?;
        let operation_id =
            Self::begin_workspace_snapshot_tx(&mut tx, workspace_id, purpose, archive_generation)
                .await?;
        tx.commit().await?;
        Ok(operation_id)
    }

    pub async fn begin_workspace_snapshot_tx(
        tx: &mut Transaction<'_, Postgres>,
        workspace_id: Uuid,
        purpose: &str,
        archive_generation: Option<i64>,
    ) -> Result<Uuid, ApplicationError> {
        if purpose != "manual" && purpose != "archive" {
            return Err(ApplicationError::Kernel(crate::KernelError::Conflict));
        }
        if purpose == "archive" && archive_generation.is_none() {
            return Err(ApplicationError::WorkspaceBusy);
        }
        if purpose == "manual" && archive_generation.is_some() {
            return Err(ApplicationError::Kernel(crate::KernelError::Conflict));
        }
        let existing = sqlx::query(
            "select operation_id, purpose, archive_generation from workspace_snapshot_operations \
             where workspace_id = $1 and state = 'dispatched' for update",
        )
        .bind(workspace_id)
        .fetch_optional(&mut **tx)
        .await?;
        if let Some(row) = existing {
            let operation_id: Uuid = row.get("operation_id");
            let stored_purpose: String = row.get("purpose");
            let stored_generation: Option<i64> = row.get("archive_generation");
            if stored_purpose == purpose && stored_generation == archive_generation {
                return Ok(operation_id);
            }
            return Err(ApplicationError::WorkspaceBusy);
        }
        let operation_id = Uuid::new_v4();
        sqlx::query(
            "insert into workspace_snapshot_operations \
             (workspace_id, operation_id, purpose, archive_generation, state) \
             values ($1, $2, $3, $4, 'dispatched')",
        )
        .bind(workspace_id)
        .bind(operation_id)
        .bind(purpose)
        .bind(archive_generation)
        .execute(&mut **tx)
        .await?;
        Ok(operation_id)
    }

    /// User-row serialization, membership, Application state, and the durable
    /// manual snapshot claim in one transaction. Fabric I/O happens after.
    pub async fn accept_manual_workspace_snapshot(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        application_id: Uuid,
    ) -> Result<Uuid, ApplicationError> {
        let mut tx = self.pool.begin().await?;
        crate::Kernel::lock_user_row(&mut tx, actor_user_id).await?;
        let status: Option<String> = sqlx::query_scalar("select status from users where id = $1")
            .bind(actor_user_id)
            .fetch_optional(&mut *tx)
            .await?;
        if status.as_deref() != Some("active") {
            return Err(ApplicationError::Auth);
        }
        let application = sqlx::query(
            "select project_id, workspace_id, state from applications where id = $1 for update",
        )
        .bind(application_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApplicationError::NotFound)?;
        let project_id: Uuid = application.get("project_id");
        let bound_workspace: Uuid = application.get("workspace_id");
        let state: String = application.get("state");
        if bound_workspace != workspace_id {
            return Err(ApplicationError::NotFound);
        }
        if matches!(
            state.as_str(),
            "archiving" | "archived" | "restoring" | "deleting"
        ) {
            return Err(ApplicationError::WorkspaceBusy);
        }
        let role_text: Option<String> = sqlx::query_scalar(
            "select role from project_members where user_id = $1 and project_id = $2",
        )
        .bind(actor_user_id)
        .bind(project_id)
        .fetch_optional(&mut *tx)
        .await?;
        let permitted = role_text
            .as_deref()
            .and_then(crate::auth::Role::parse)
            .is_some_and(|role| role.permits(Action::ManageProduction));
        if !permitted {
            return Err(ApplicationError::Auth);
        }
        applications::lock_project(&mut tx, project_id).await?;
        applications::lock_application(&mut tx, application_id).await?;
        let operation_id =
            Self::begin_workspace_snapshot_tx(&mut tx, workspace_id, "manual", None).await?;
        tx.commit().await?;
        Ok(operation_id)
    }

    pub async fn complete_workspace_snapshot(
        &self,
        workspace_id: Uuid,
        operation_id: Uuid,
    ) -> Result<(), ApplicationError> {
        let updated = sqlx::query(
            "update workspace_snapshot_operations set state = 'ready' \
             where workspace_id = $1 and operation_id = $2 and state = 'dispatched'",
        )
        .bind(workspace_id)
        .bind(operation_id)
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() == 0 {
            return Err(ApplicationError::WorkspaceBusy);
        }
        Ok(())
    }

    pub async fn unknown_workspace_snapshot(
        &self,
        workspace_id: Uuid,
        operation_id: Uuid,
    ) -> Result<(), ApplicationError> {
        sqlx::query(
            "update workspace_snapshot_operations set state = 'unknown' \
             where workspace_id = $1 and operation_id = $2 and state = 'dispatched'",
        )
        .bind(workspace_id)
        .bind(operation_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_workspace_snapshot(
        &self,
        workspace_id: Uuid,
        object_key: &str,
        content_hash: &[u8; 32],
        byte_length: i64,
        kind: &str,
    ) -> Result<Uuid, ApplicationError> {
        if let Some(existing) = sqlx::query(
            "select id, content_hash, byte_length from workspace_snapshots where object_key = $1",
        )
        .bind(object_key)
        .fetch_optional(&self.pool)
        .await?
        {
            let id: Uuid = existing.get("id");
            let hash: Vec<u8> = existing.get("content_hash");
            let length: i64 = existing.get("byte_length");
            if hash.as_slice() == content_hash.as_slice() && length == byte_length {
                return Ok(id);
            }
            return Err(ApplicationError::Kernel(crate::KernelError::Conflict));
        }
        let id = Uuid::new_v4();
        sqlx::query(
            "insert into workspace_snapshots \
             (id, workspace_id, object_key, content_hash, byte_length, kind, pinned) \
             values ($1, $2, $3, $4, $5, $6, false)",
        )
        .bind(id)
        .bind(workspace_id)
        .bind(object_key)
        .bind(content_hash.as_slice())
        .bind(byte_length)
        .bind(kind)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    pub async fn get_workspace_snapshot(
        &self,
        snapshot_id: Uuid,
    ) -> Result<WorkspaceSnapshot, ApplicationError> {
        let row = sqlx::query(
            "select id, workspace_id, object_key, content_hash, byte_length, kind, \
                    created_at::text as created_at \
             from workspace_snapshots where id = $1",
        )
        .bind(snapshot_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(ApplicationError::NotFound)?;
        Ok(WorkspaceSnapshot {
            id: row.get("id"),
            workspace_id: row.get("workspace_id"),
            object_key: row.get("object_key"),
            content_hash: row.get("content_hash"),
            byte_length: row.get("byte_length"),
            kind: row.get("kind"),
            created_at: row.get("created_at"),
        })
    }

    pub async fn expired_snapshots(
        &self,
        workspace_id: Uuid,
    ) -> Result<Vec<WorkspaceSnapshot>, ApplicationError> {
        let rows = sqlx::query(
            "select id, workspace_id, object_key, content_hash, byte_length, kind, \
                    created_at::text as created_at \
             from workspace_snapshots \
             where workspace_id = $1 and not pinned \
               and object_key not like 'reclaimed/%' \
             order by created_at desc, id desc",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await?;
        let newest_first: Vec<WorkspaceSnapshot> = rows
            .into_iter()
            .map(|row| WorkspaceSnapshot {
                id: row.get("id"),
                workspace_id: row.get("workspace_id"),
                object_key: row.get("object_key"),
                content_hash: row.get("content_hash"),
                byte_length: row.get("byte_length"),
                kind: row.get("kind"),
                created_at: row.get("created_at"),
            })
            .collect();
        Ok(crate::storage::expired_by_retention(newest_first, |row| {
            row.byte_length
        }))
    }

    pub async fn drop_snapshot(&self, snapshot_id: Uuid) -> Result<(), ApplicationError> {
        sqlx::query("delete from workspace_snapshots where id = $1")
            .bind(snapshot_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Backups older than the per-Database retention bound, oldest first.
    pub async fn expired_backups(
        &self,
        database_id: Uuid,
    ) -> Result<Vec<Backup>, ApplicationError> {
        let rows = sqlx::query(
            "select id, database_id, object_key, content_hash, byte_length, kind, \
                    created_at::text as created_at \
             from database_backups \
             where database_id = $1 and not pinned \
               and object_key not like 'reclaimed/%' \
             order by created_at desc, id desc",
        )
        .bind(database_id)
        .fetch_all(&self.pool)
        .await?;
        let newest_first: Vec<Backup> = rows.into_iter().map(row_backup).collect();
        Ok(crate::storage::expired_by_retention(newest_first, |row| {
            row.byte_length
        }))
    }

    pub async fn drop_backup(&self, backup_id: Uuid) -> Result<(), ApplicationError> {
        sqlx::query("delete from database_backups where id = $1")
            .bind(backup_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_backups(
        &self,
        actor_user_id: Uuid,
        database_id: Uuid,
    ) -> Result<Vec<Backup>, ApplicationError> {
        let _ = self.get(actor_user_id, database_id).await?;
        let rows = sqlx::query(
            "select id, database_id, object_key, content_hash, byte_length, kind, \
                    created_at::text as created_at \
             from database_backups where database_id = $1 order by created_at desc",
        )
        .bind(database_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_backup).collect())
    }

    pub async fn delete(
        &self,
        actor_user_id: Uuid,
        database_id: Uuid,
        approval_id: Option<Uuid>,
    ) -> Result<(), ApplicationError> {
        let database = self.get(actor_user_id, database_id).await?;
        applications::ApplicationStore::new(self.pool.clone(), String::new())
            .require_in_project(
                actor_user_id,
                database.application_id,
                Action::DestroyApplication,
            )
            .await?;
        let project_id: Uuid =
            sqlx::query_scalar("select project_id from applications where id = $1")
                .bind(database.application_id)
                .fetch_one(&self.pool)
                .await?;
        applications::require_approval(
            &self.pool,
            approval_id,
            project_id,
            "delete_database",
            &applications::ApprovalTarget {
                application_id: Some(database.application_id),
                environment_id: Some(database.environment_id),
                database_id: Some(database_id),
                ..Default::default()
            },
            actor_user_id,
        )
        .await?;
        sqlx::query(
            "update application_databases \
             set desired_state = 'absent', \
                 desired_revision = case \
                     when desired_state = 'absent' then desired_revision \
                     else desired_revision + 1 \
                 end, \
                 reconcile_after = now(), \
                 deleted_at = coalesce(deleted_at, now()) \
             where id = $1 \
               and desired_state <> 'absent'",
        )
        .bind(database_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_application_recovery_keys(
        &self,
        application_id: Uuid,
    ) -> Result<Vec<String>, ApplicationError> {
        let snapshots: Vec<String> = sqlx::query_scalar(
            "select s.object_key from workspace_snapshots s \
             join application_archives ar on ar.workspace_snapshot_id = s.id \
             where ar.application_id = $1",
        )
        .bind(application_id)
        .fetch_all(&self.pool)
        .await?;
        let backups: Vec<String> = sqlx::query_scalar(
            "select b.object_key from database_backups b \
             join application_databases d on d.id = b.database_id \
             where d.application_id = $1",
        )
        .bind(application_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(snapshots
            .into_iter()
            .chain(backups)
            .filter(|key| recoverable_blob_key(key))
            .collect())
    }

    /// Deletes recoverable backup/snapshot Blob objects for one Application.
    /// Rows stay as hash tombstones; plaintext objects must not survive
    /// Application deletion. Fails closed if Blob is missing while keys remain.
    pub async fn reclaim_application_recovery_blobs(
        &self,
        application_id: Uuid,
        blob: Option<&BlobStore>,
    ) -> Result<(), ApplicationError> {
        let keys = self.list_application_recovery_keys(application_id).await?;
        if keys.is_empty() {
            return Ok(());
        }
        let Some(blob) = blob else {
            return Err(ApplicationError::Kernel(crate::KernelError::Database));
        };
        for key in keys {
            blob.delete(&key)
                .await
                .map_err(|_| ApplicationError::Kernel(crate::KernelError::Database))?;
            sqlx::query(
                "update database_backups set object_key = $2 \
                 where object_key = $1 returning id",
            )
            .bind(&key)
            .bind(reclaimed_blob_key_from(&key))
            .fetch_optional(&self.pool)
            .await?;
            sqlx::query(
                "update workspace_snapshots set object_key = $2 \
                 where object_key = $1 returning id",
            )
            .bind(&key)
            .bind(reclaimed_blob_key_from(&key))
            .fetch_optional(&self.pool)
            .await?;
        }
        if !self
            .list_application_recovery_keys(application_id)
            .await?
            .is_empty()
        {
            return Err(ApplicationError::Kernel(crate::KernelError::Database));
        }
        Ok(())
    }
}

fn reclaimed_blob_key_from(object_key: &str) -> String {
    let digest = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(object_key.as_bytes());
        format!("{:x}", hasher.finalize())
    };
    format!("reclaimed/{digest}")
}

/// One-time Database password. The value is written to Key Vault or the
/// encrypted backend by the caller and must never enter Application JSON,
/// Workspace, or conversation events.
pub fn generate_postgres_password() -> Result<String, ApplicationError> {
    let rng = ring::rand::SystemRandom::new();
    let mut bytes = [0u8; 24];
    ring::rand::SecureRandom::fill(&rng, &mut bytes)
        .map_err(|_| ApplicationError::Kernel(crate::KernelError::Database))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn row_backup(row: sqlx::postgres::PgRow) -> Backup {
    Backup {
        id: row.get("id"),
        database_id: row.get("database_id"),
        object_key: row.get("object_key"),
        content_hash: row.get("content_hash"),
        byte_length: row.get("byte_length"),
        kind: row.get("kind"),
        created_at: row.get("created_at"),
    }
}

fn row_database(row: sqlx::postgres::PgRow) -> Database {
    Database {
        id: row.get("id"),
        application_id: row.get("application_id"),
        environment_id: row.get("environment_id"),
        engine: row.get("engine"),
        engine_profile: row.get("engine_profile"),
        fabric_id: row.get("fabric_id"),
        state: row.get("state"),
        desired_revision: row.get("desired_revision"),
        observed_revision: row.get("observed_revision"),
        credential_secret_id: row.get("credential_secret_id"),
        storage_bytes: row.get("storage_bytes"),
        storage_tier: row
            .try_get("storage_tier")
            .unwrap_or_else(|_| "default".into()),
        desired_state: row.get("desired_state"),
        observed_state: row.get("observed_state"),
        last_error_code: row.get("last_error_code"),
        security_profile: row.get("security_profile"),
        created_at: row.get("created_at"),
    }
}

#[cfg(test)]
mod tests {
    use super::FAIL_CLOSED_CREATING_SQL;

    #[test]
    fn secret_material_failure_is_not_unknown() {
        assert!(FAIL_CLOSED_CREATING_SQL.contains("observed_state = 'failed'"));
        assert!(FAIL_CLOSED_CREATING_SQL.contains("secret_material_unavailable"));
        assert!(FAIL_CLOSED_CREATING_SQL.contains("desired_state = 'present'"));
        assert!(
            !FAIL_CLOSED_CREATING_SQL.contains("state = 'unknown'"),
            "empty Key Vault material is deterministic, not an unknown dispatched effect"
        );
    }

    #[test]
    fn wire_state_follows_desired_and_observed() {
        fn sample(desired: &str, observed: &str, process: &str) -> super::Database {
            super::Database {
                id: uuid::Uuid::nil(),
                application_id: uuid::Uuid::nil(),
                environment_id: uuid::Uuid::nil(),
                engine: "postgres".into(),
                engine_profile: "voie-postgres:v1".into(),
                fabric_id: uuid::Uuid::nil(),
                state: process.into(),
                desired_revision: 1,
                observed_revision: 0,
                credential_secret_id: None,
                storage_bytes: 1,
                storage_tier: "default".into(),
                desired_state: desired.into(),
                observed_state: observed.into(),
                last_error_code: None,
                security_profile: 1,
                created_at: String::new(),
            }
        }
        assert_eq!(sample("present", "", "creating").wire_state(), "creating");
        assert_eq!(
            sample("present", "", "ready").wire_state(),
            "creating",
            "leftover process ready is not product authority"
        );
        assert_eq!(sample("present", "ready", "creating").wire_state(), "ready");
        assert_eq!(
            sample("present", "present", "creating").wire_state(),
            "ready"
        );
        assert_eq!(sample("absent", "ready", "ready").wire_state(), "deleting");
        assert_eq!(
            sample("absent", "absent", "creating").wire_state(),
            "deleted"
        );
    }
}
