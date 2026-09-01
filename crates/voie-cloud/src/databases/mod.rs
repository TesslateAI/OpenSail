//! Dedicated PostgreSQL Database per Application Environment.

use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::applications::{self, ApplicationError};
use crate::auth::Action;
use crate::session_store::BlobStore;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

/// Oldest backups beyond this bound are dropped after a successful record.
pub const MAX_BACKUPS_PER_DATABASE: i64 = crate::storage::BACKUP_RETENTION;
pub const MAX_INFLIGHT_BACKUPS_PER_DATABASE: i64 =
    crate::storage::MAX_INFLIGHT_BACKUPS_PER_DATABASE;
pub const MAX_INFLIGHT_BACKUPS_PER_PROJECT: i64 = crate::storage::MAX_INFLIGHT_BACKUPS_PER_PROJECT;

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
    pub created_at: String,
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
        let row = sqlx::query(
            "insert into application_databases \
             (id, application_id, environment_id, engine_profile, fabric_id, state, storage_bytes, storage_tier) \
             values ($1, $2, $3, 'voie-postgres:v1', $4, 'creating', $5, $6) \
             on conflict (environment_id) do nothing \
             returning id, application_id, environment_id, engine, engine_profile, fabric_id, \
                       state, desired_revision, observed_revision, credential_secret_id, \
                       storage_bytes, created_at::text as created_at",
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
        let op_insert = sqlx::query(
            "insert into database_operations \
             (id, database_id, operation_id, kind, request_hash, state) \
             values ($1, $2, $3, 'create', $4, 'dispatched') \
             on conflict (database_id, operation_id) do nothing \
             returning id",
        )
        .bind(Uuid::new_v4())
        .bind(database_id)
        .bind(operation_id)
        .bind(request_hash.as_slice())
        .fetch_optional(&mut *tx)
        .await?;
        if op_insert.is_none() {
            let existing_state: String = sqlx::query_scalar(
                "select state from database_operations where database_id = $1 and operation_id = $2",
            )
            .bind(database_id)
            .bind(operation_id)
            .fetch_one(&mut *tx)
            .await
            .unwrap_or_else(|_| "unknown".to_owned());
            if existing_state == "dispatched" || existing_state == "unknown" {
                tx.rollback().await.ok();
                return Err(ApplicationError::WorkspaceBusy);
            }
        }
        tx.commit().await?;
        Ok(row_database(row))
    }

    /// Records the platform credential UUID. This id addresses Key Vault
    /// material, not a `user_secrets` row.
    pub async fn attach_credential(
        &self,
        database_id: Uuid,
        secret_id: Uuid,
    ) -> Result<(), ApplicationError> {
        sqlx::query(
            "update application_databases set credential_secret_id = $2 \
             where id = $1 and credential_secret_id is null",
        )
        .bind(database_id)
        .bind(secret_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn mark_ready(
        &self,
        database_id: Uuid,
        secret_id: Uuid,
    ) -> Result<Database, ApplicationError> {
        sqlx::query(
            "update application_databases set state = 'ready', credential_secret_id = $2, \
                    observed_revision = desired_revision \
             where id = $1 and state = 'creating'",
        )
        .bind(database_id)
        .bind(secret_id)
        .execute(&self.pool)
        .await?;
        self.get_internal(database_id).await
    }

    pub async fn dispatched_create_operation(
        &self,
        database_id: Uuid,
    ) -> Result<Option<Uuid>, ApplicationError> {
        let operation_id = sqlx::query_scalar(
            "select operation_id from database_operations \
             where database_id = $1 and kind = 'create' and state = 'dispatched' \
             order by created_at desc limit 1",
        )
        .bind(database_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(operation_id)
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
        sqlx::query(
            "update application_databases set state = 'unknown' \
             where id = $1 and state = 'creating'",
        )
        .bind(database_id)
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
        if application_state != "archiving"
            && project_inflight >= MAX_INFLIGHT_BACKUPS_PER_PROJECT
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
                    storage_bytes, created_at::text as created_at \
             from application_databases where id = $1",
        )
        .bind(database_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(ApplicationError::NotFound)?;
        Ok(row_database(row))
    }

    pub async fn list_creating(&self) -> Result<Vec<Database>, ApplicationError> {
        let rows = sqlx::query(
            "select id, application_id, environment_id, engine, engine_profile, fabric_id, \
                    state, desired_revision, observed_revision, credential_secret_id, \
                    storage_bytes, created_at::text as created_at \
             from application_databases where state = 'creating'",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_database).collect())
    }

    pub async fn by_environment(
        &self,
        environment_id: Uuid,
    ) -> Result<Option<Database>, ApplicationError> {
        let row = sqlx::query(
            "select id, application_id, environment_id, engine, engine_profile, fabric_id, \
                    state, desired_revision, observed_revision, credential_secret_id, \
                    storage_bytes, created_at::text as created_at \
             from application_databases \
             where environment_id = $1 and state <> 'deleted'",
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
            "update application_databases set state = 'deleting', deleted_at = now() where id = $1",
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
        created_at: row.get("created_at"),
    }
}
