//! Immutable Application Release: one build intent packaged once. A new
//! intent packs the current guest, so a follow-up can ship source that
//! changed after the last pack.

use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::applications::{self, ApplicationError, Manifest};
use crate::auth::Action;
use crate::session_store::BlobStore;

/// Retained ready/in-flight Release objects per Application. Failed bulky
/// rows may be trimmed after the intent ledger records them. Unknown
/// identities stay in the ledger forever.
pub const MAX_RELEASES_PER_APPLICATION: i64 = 16;
/// Project aggregate of retained ready/in-flight Releases.
pub const MAX_RELEASES_PER_PROJECT: i64 =
    MAX_RELEASES_PER_APPLICATION * crate::applications::MAX_APPLICATIONS_PER_PROJECT;
/// Newest unreferenced failed bulky rows kept for inspect. The intent
/// ledger is the no-replay identity; unknown bulky rows are not trimmed.
pub const MAX_FAILED_RELEASE_TOMBSTONES_PER_APPLICATION: i64 = 8;
/// Retained artifact bytes per Application (8 GiB).
pub const MAX_RELEASE_BYTES_PER_APPLICATION: i64 = 8 * 1024 * 1024 * 1024;
/// Project aggregate of retained artifact bytes.
pub const MAX_RELEASE_BYTES_PER_PROJECT: i64 =
    MAX_RELEASE_BYTES_PER_APPLICATION * crate::applications::MAX_APPLICATIONS_PER_PROJECT;
/// Pack ceiling. Reserved/dispatched rows count as this many bytes until
/// `complete` records the real size, so concurrent builds cannot overshoot
/// the Application or Project budget.
pub const MAX_PACKED_ARTIFACT_BYTES: i64 = 512 * 1024 * 1024;
/// In-flight pack operations per Application.
pub const MAX_CONCURRENT_RELEASES_PER_APPLICATION: i64 = 2;
/// In-flight pack operations per Project.
pub const MAX_CONCURRENT_RELEASES_PER_PROJECT: i64 = 2;
/// In-flight pack operations per actor.
pub const MAX_CONCURRENT_RELEASES_PER_USER: i64 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub id: Uuid,
    pub application_id: Uuid,
    pub build_intent_id: Uuid,
    pub request_hash: Vec<u8>,
    pub source_workspace_id: Uuid,
    pub source_exec_generation: i64,
    pub runtime_profile: String,
    pub manifest: serde_json::Value,
    pub manifest_hash: Vec<u8>,
    pub artifact_key: Option<String>,
    pub artifact_hash: Option<Vec<u8>>,
    pub artifact_bytes: Option<i64>,
    pub test_summary: Option<String>,
    pub state: String,
    pub created_by_user_id: Uuid,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeginRelease {
    ReadyToDispatch,
    Ready { id: Uuid },
    Failed { id: Uuid },
    OutcomeUnknown,
    Conflict,
}

#[derive(Clone)]
pub struct ReleaseStore {
    pool: PgPool,
}

impl ReleaseStore {
    pub fn new(pool: PgPool) -> Self {
        ReleaseStore { pool }
    }

    pub fn request_hash(
        workspace_id: Uuid,
        generation: i64,
        manifest_hash: &[u8; 32],
        runtime_profile: &str,
        build_command: &[String],
        test_command: Option<&[String]>,
        output_path: &str,
        build_intent_id: Uuid,
    ) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(workspace_id.as_bytes());
        hasher.update(generation.to_be_bytes());
        hasher.update(build_intent_id.as_bytes());
        hasher.update(manifest_hash);
        hasher.update(runtime_profile.as_bytes());
        hasher.update(0u8.to_be_bytes());
        for part in build_command {
            hasher.update(part.as_bytes());
            hasher.update([0xff]);
        }
        hasher.update([0xfe]);
        if let Some(test) = test_command {
            for part in test {
                hasher.update(part.as_bytes());
                hasher.update([0xff]);
            }
        }
        hasher.update(output_path.as_bytes());
        hasher.finalize().into()
    }

    /// Reserve one build intent. The same intent returns the existing result.
    /// A different hash for the same intent is a conflict. Dispatched or
    /// unknown is never executed again. A new intent packs the current guest.
    pub async fn begin(
        &self,
        actor_user_id: Uuid,
        application_id: Uuid,
        build_intent_id: Uuid,
        workspace_id: Uuid,
        generation: i64,
        manifest_text: &str,
        approval_id: Option<Uuid>,
    ) -> Result<(BeginRelease, Option<Release>), ApplicationError> {
        let application = applications::ApplicationStore::new(self.pool.clone(), String::new())
            .require_in_project(actor_user_id, application_id, Action::OperateSession)
            .await?
            .0;
        if application.workspace_id != workspace_id {
            return Err(ApplicationError::WorkspaceMissing);
        }
        let workspace = sqlx::query(
            "select state, desired_state, observed_state from workspaces where id = $1",
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(ApplicationError::WorkspaceMissing)?;
        let process: String = workspace.get("state");
        let desired: String = workspace.get("desired_state");
        let observed: String = workspace.get("observed_state");
        if !crate::workspace_is_realized(&desired, &observed, &process) {
            return Err(ApplicationError::WorkspaceBusy);
        }
        let recorded_generation: i64 =
            sqlx::query_scalar("select exec_generation from workspaces where id = $1")
                .bind(workspace_id)
                .fetch_one(&self.pool)
                .await?;
        if recorded_generation != generation {
            return Err(ApplicationError::WorkspaceBusy);
        }
        let parsed = Manifest::parse(manifest_text)
            .map_err(|error| ApplicationError::InvalidManifest(error.message()))?;
        if parsed.exceeds_default_tier() {
            applications::require_approval(
                &self.pool,
                approval_id,
                application.project_id,
                "increase_resource_tier",
                &applications::ApprovalTarget {
                    application_id: Some(application.id),
                    ..Default::default()
                },
                actor_user_id,
            )
            .await?;
        }
        let manifest_hash = parsed.hash(manifest_text);
        let hash = Self::request_hash(
            workspace_id,
            generation,
            &manifest_hash,
            &parsed.runtime,
            &parsed.build_command,
            parsed.test_command.as_deref(),
            &parsed.build_output,
            build_intent_id,
        );
        let mut tx = self.pool.begin().await?;
        crate::Kernel::lock_user_row(&mut tx, actor_user_id).await?;
        applications::lock_project(&mut tx, application.project_id).await?;
        applications::require_live_application(&mut tx, application_id).await?;
        remember_existing_intents(&mut tx, application_id).await?;
        if let Some((class, release_id, stored_hash)) =
            load_intent(&mut tx, build_intent_id).await?
        {
            tx.commit().await?;
            if stored_hash.as_slice() != hash.as_slice() {
                let existing = load_by_intent(&self.pool, build_intent_id).await?;
                return Ok((BeginRelease::Conflict, existing));
            }
            let existing = load_by_intent(&self.pool, build_intent_id).await?;
            let id = existing
                .as_ref()
                .map(|row| row.id)
                .or(release_id)
                .unwrap_or(build_intent_id);
            return Ok((begin_from_class(&class, id), existing));
        }
        let hash_taken: bool = sqlx::query_scalar(
            "select exists(select 1 from application_release_intents \
             where application_id = $1 and request_hash = $2)",
        )
        .bind(application_id)
        .bind(hash.as_slice())
        .fetch_one(&mut *tx)
        .await?;
        if hash_taken {
            tx.commit().await?;
            return Ok((BeginRelease::Conflict, None));
        }
        trim_failed_objects(&mut tx, application_id).await?;
        let retained: i64 = sqlx::query_scalar(
            "select count(*) from application_releases \
             where application_id = $1 and state in ('ready', 'reserved', 'dispatched')",
        )
        .bind(application_id)
        .fetch_one(&mut *tx)
        .await?;
        if retained >= MAX_RELEASES_PER_APPLICATION {
            return Err(ApplicationError::Kernel(crate::KernelError::Quota));
        }
        let project_retained: i64 = sqlx::query_scalar(
            "select count(*) from application_releases r \
             join applications a on a.id = r.application_id \
             where a.project_id = $1 and r.state in ('ready', 'reserved', 'dispatched')",
        )
        .bind(application.project_id)
        .fetch_one(&mut *tx)
        .await?;
        if project_retained >= MAX_RELEASES_PER_PROJECT {
            return Err(ApplicationError::Kernel(crate::KernelError::Quota));
        }
        let bytes = reserved_artifact_bytes(&mut tx, Some(application_id), None).await?;
        if bytes + MAX_PACKED_ARTIFACT_BYTES > MAX_RELEASE_BYTES_PER_APPLICATION {
            return Err(ApplicationError::Kernel(crate::KernelError::Quota));
        }
        let project_bytes =
            reserved_artifact_bytes(&mut tx, None, Some(application.project_id)).await?;
        if project_bytes + MAX_PACKED_ARTIFACT_BYTES > MAX_RELEASE_BYTES_PER_PROJECT {
            return Err(ApplicationError::Kernel(crate::KernelError::Quota));
        }
        let inflight: i64 = sqlx::query_scalar(
            "select count(*) from application_releases \
             where application_id = $1 and state in ('reserved', 'dispatched')",
        )
        .bind(application_id)
        .fetch_one(&mut *tx)
        .await?;
        if inflight >= MAX_CONCURRENT_RELEASES_PER_APPLICATION {
            return Err(ApplicationError::Kernel(crate::KernelError::Quota));
        }
        let project_inflight: i64 = sqlx::query_scalar(
            "select count(*) from application_releases r \
             join applications a on a.id = r.application_id \
             where a.project_id = $1 and r.state in ('reserved', 'dispatched')",
        )
        .bind(application.project_id)
        .fetch_one(&mut *tx)
        .await?;
        if project_inflight >= MAX_CONCURRENT_RELEASES_PER_PROJECT {
            return Err(ApplicationError::Kernel(crate::KernelError::Quota));
        }
        let user_inflight: i64 = sqlx::query_scalar(
            "select count(*) from application_releases \
             where created_by_user_id = $1 and state in ('reserved', 'dispatched')",
        )
        .bind(actor_user_id)
        .fetch_one(&mut *tx)
        .await?;
        if user_inflight >= MAX_CONCURRENT_RELEASES_PER_USER {
            return Err(ApplicationError::Kernel(crate::KernelError::Quota));
        }
        let release_id = Uuid::new_v4();
        sqlx::query(
            "insert into application_release_intents \
             (build_intent_id, application_id, request_hash, class, release_id) \
             values ($1, $2, $3, 'dispatched', $4)",
        )
        .bind(build_intent_id)
        .bind(application_id)
        .bind(hash.as_slice())
        .bind(release_id)
        .execute(&mut *tx)
        .await?;
        let inserted = sqlx::query(
            "insert into application_releases \
             (id, application_id, build_intent_id, request_hash, source_workspace_id, \
              source_exec_generation, runtime_profile, manifest, manifest_hash, state, created_by_user_id) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'reserved', $10) \
             on conflict (build_intent_id) do nothing \
             returning id",
        )
        .bind(release_id)
        .bind(application_id)
        .bind(build_intent_id)
        .bind(hash.as_slice())
        .bind(workspace_id)
        .bind(generation)
        .bind(&parsed.runtime)
        .bind(parsed.to_json())
        .bind(manifest_hash.as_slice())
        .bind(actor_user_id)
        .fetch_optional(&mut *tx)
        .await?;
        if inserted.is_some() {
            sqlx::query("update application_releases set state = 'dispatched' where build_intent_id = $1 and state = 'reserved'")
                .bind(build_intent_id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            return Ok((BeginRelease::ReadyToDispatch, None));
        }
        tx.commit().await?;
        let existing = load_by_intent(&self.pool, build_intent_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        if existing.request_hash.as_slice() != hash.as_slice() {
            return Ok((BeginRelease::Conflict, Some(existing)));
        }
        let begin = match existing.state.as_str() {
            "ready" => BeginRelease::Ready { id: existing.id },
            "failed" => BeginRelease::Failed { id: existing.id },
            "dispatched" | "unknown" | "reserved" => BeginRelease::OutcomeUnknown,
            _ => BeginRelease::OutcomeUnknown,
        };
        Ok((begin, Some(existing)))
    }

    pub async fn complete(
        &self,
        build_intent_id: Uuid,
        artifact_key: &str,
        artifact_hash: &[u8; 32],
        artifact_bytes: i64,
        test_summary: &str,
    ) -> Result<Release, ApplicationError> {
        let pending = load_by_intent(&self.pool, build_intent_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let mut tx = self.pool.begin().await?;
        applications::require_live_application(&mut tx, pending.application_id).await?;
        let updated = sqlx::query(
            "update application_releases set state = 'ready', artifact_key = $2, \
                    artifact_hash = $3, artifact_bytes = $4, test_summary = $5 \
             where build_intent_id = $1 and state = 'dispatched' \
             returning id",
        )
        .bind(build_intent_id)
        .bind(artifact_key)
        .bind(artifact_hash.as_slice())
        .bind(artifact_bytes)
        .bind(test_summary)
        .fetch_optional(&mut *tx)
        .await?;
        if updated.is_none() {
            return Err(ApplicationError::NotFound);
        }
        tx.commit().await?;
        mark_intent_class(&self.pool, build_intent_id, "ready").await?;
        load_by_intent(&self.pool, build_intent_id)
            .await?
            .ok_or(ApplicationError::NotFound)
    }

    /// Writes immutable Release bytes to Blob without marking the row ready.
    /// Ready is committed after the Blob object and PostgreSQL metadata exist.
    /// Deployment materializes a private 1 GiB LV from Blob; there is no
    /// permanent Fabric Release volume.
    pub async fn stage_blob(
        &self,
        blob: &BlobStore,
        build_intent_id: Uuid,
        bytes: &[u8],
    ) -> Result<(Uuid, String, [u8; 32], i64), ApplicationError> {
        let pending = load_by_intent(&self.pool, build_intent_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        if pending.state != "dispatched" {
            return Err(ApplicationError::WorkspaceBusy);
        }
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let artifact_hash: [u8; 32] = hasher.finalize().into();
        let project_id: Uuid =
            sqlx::query_scalar("select project_id from applications where id = $1")
                .bind(pending.application_id)
                .fetch_one(&self.pool)
                .await?;
        let key = Self::artifact_key(project_id, pending.application_id, &artifact_hash);
        {
            let mut tx = self.pool.begin().await?;
            applications::require_live_application(&mut tx, pending.application_id).await?;
            let still_dispatched: bool = sqlx::query_scalar(
                "select exists(select 1 from application_releases \
                 where build_intent_id = $1 and state = 'dispatched')",
            )
            .bind(build_intent_id)
            .fetch_one(&mut *tx)
            .await?;
            if !still_dispatched {
                return Err(ApplicationError::WorkspaceBusy);
            }
            tx.commit().await?;
        }
        blob.put_artifact_if_absent(&key, bytes)
            .await
            .map_err(|_| ApplicationError::Kernel(crate::KernelError::Database))?;
        {
            let mut tx = self.pool.begin().await?;
            if let Err(error) =
                applications::require_live_application(&mut tx, pending.application_id).await
            {
                drop(tx);
                let _ = blob.delete(&key).await;
                return Err(error);
            }
            tx.commit().await?;
        }
        Ok((pending.id, key, artifact_hash, bytes.len() as i64))
    }

    /// Streams a packed Release from Fabric into Blob. The object key is the
    /// content hash Fabric already computed; control never assembles the pack.
    pub async fn stage_blob_stream<S, E>(
        &self,
        blob: &BlobStore,
        build_intent_id: Uuid,
        expected_hash_hex: &str,
        stream: S,
    ) -> Result<(Uuid, String, [u8; 32], i64), ApplicationError>
    where
        S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Unpin,
        E: std::error::Error + Send + Sync + 'static,
    {
        let pending = load_by_intent(&self.pool, build_intent_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        if pending.state != "dispatched" {
            return Err(ApplicationError::WorkspaceBusy);
        }
        let artifact_hash =
            parse_sha256_hex(expected_hash_hex).ok_or(ApplicationError::WorkspaceBusy)?;
        let project_id: Uuid =
            sqlx::query_scalar("select project_id from applications where id = $1")
                .bind(pending.application_id)
                .fetch_one(&self.pool)
                .await?;
        let key = Self::artifact_key(project_id, pending.application_id, &artifact_hash);
        {
            let mut tx = self.pool.begin().await?;
            applications::require_live_application(&mut tx, pending.application_id).await?;
            let still_dispatched: bool = sqlx::query_scalar(
                "select exists(select 1 from application_releases \
                 where build_intent_id = $1 and state = 'dispatched')",
            )
            .bind(build_intent_id)
            .fetch_one(&mut *tx)
            .await?;
            if !still_dispatched {
                return Err(ApplicationError::WorkspaceBusy);
            }
            tx.commit().await?;
        }
        let (digest, byte_length) = blob
            .put_stream_if_absent(&key, stream, Some(MAX_PACKED_ARTIFACT_BYTES as u64))
            .await
            .map_err(|_| ApplicationError::Kernel(crate::KernelError::Database))?;
        if digest != artifact_hash || byte_length == 0 {
            let _ = blob.delete(&key).await;
            return Err(ApplicationError::WorkspaceBusy);
        }
        {
            let mut tx = self.pool.begin().await?;
            if let Err(error) =
                applications::require_live_application(&mut tx, pending.application_id).await
            {
                drop(tx);
                let _ = blob.delete(&key).await;
                return Err(error);
            }
            tx.commit().await?;
        }
        Ok((pending.id, key, artifact_hash, byte_length as i64))
    }

    /// Writes immutable Release bytes to Blob and commits metadata. Same hash
    /// is identity. Fabric never receives the Blob credential.
    pub async fn commit_artifact(
        &self,
        blob: &BlobStore,
        build_intent_id: Uuid,
        bytes: &[u8],
        test_summary: &str,
    ) -> Result<Release, ApplicationError> {
        let pending = load_by_intent(&self.pool, build_intent_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        if pending.state == "ready" {
            return Ok(pending);
        }
        let (_, key, artifact_hash, artifact_bytes) =
            self.stage_blob(blob, build_intent_id, bytes).await?;
        self.complete(
            build_intent_id,
            &key,
            &artifact_hash,
            artifact_bytes,
            test_summary,
        )
        .await
    }

    pub async fn fail(
        &self,
        build_intent_id: Uuid,
        test_summary: &str,
    ) -> Result<(), ApplicationError> {
        sqlx::query(
            "update application_releases set state = 'failed', test_summary = $2 \
             where build_intent_id = $1 and state = 'dispatched'",
        )
        .bind(build_intent_id)
        .bind(test_summary)
        .execute(&self.pool)
        .await?;
        mark_intent_class(&self.pool, build_intent_id, "failed").await?;
        Ok(())
    }

    pub async fn unknown(&self, build_intent_id: Uuid) -> Result<(), ApplicationError> {
        sqlx::query(
            "update application_releases set state = 'unknown' \
             where build_intent_id = $1 and state in ('reserved', 'dispatched')",
        )
        .bind(build_intent_id)
        .execute(&self.pool)
        .await?;
        mark_intent_class(&self.pool, build_intent_id, "unknown").await?;
        Ok(())
    }

    pub async fn get(
        &self,
        actor_user_id: Uuid,
        release_id: Uuid,
    ) -> Result<Release, ApplicationError> {
        let release = load(&self.pool, release_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        applications::ApplicationStore::new(self.pool.clone(), String::new())
            .require_in_project(actor_user_id, release.application_id, Action::ReadProject)
            .await?;
        Ok(release)
    }

    pub async fn list(
        &self,
        actor_user_id: Uuid,
        application_id: Uuid,
    ) -> Result<Vec<Release>, ApplicationError> {
        applications::ApplicationStore::new(self.pool.clone(), String::new())
            .require_in_project(actor_user_id, application_id, Action::ReadProject)
            .await?;
        let rows = sqlx::query(&format!(
            "{RELEASE_SELECT} where application_id = $1 order by created_at, id"
        ))
        .bind(application_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_release).collect())
    }

    /// Dispatched pack journals the supervisor must finish. Not a GET path.
    pub async fn list_dispatched(&self) -> Result<Vec<Release>, ApplicationError> {
        let rows = sqlx::query(&format!(
            "{RELEASE_SELECT} where state = 'dispatched' order by created_at, id limit 32"
        ))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_release).collect())
    }

    pub fn artifact_key(project_id: Uuid, application_id: Uuid, sha256: &[u8; 32]) -> String {
        format!(
            "releases/{project_id}/{application_id}/{}.tar.zst",
            hex_sha(sha256)
        )
    }

    /// Drops an unreferenced Release object after its Blob is gone. The
    /// intent ledger stays, so the same `build_intent_id` cannot dispatch
    /// again. In-flight and referenced rows are refused. The bulky row is
    /// locked `FOR UPDATE` before the reference check and Blob delete so a
    /// concurrent Deployment insert is serialized by the FK. Content-addressed
    /// Blob keys are deleted only when no other Release still names them.
    pub async fn drop_unreferenced(
        &self,
        actor_user_id: Uuid,
        release_id: Uuid,
        blob: Option<&BlobStore>,
    ) -> Result<(), ApplicationError> {
        let preview = self.get(actor_user_id, release_id).await?;
        applications::ApplicationStore::new(self.pool.clone(), String::new())
            .require_in_project(
                actor_user_id,
                preview.application_id,
                Action::OperateSession,
            )
            .await?;
        let mut tx = self.pool.begin().await?;
        applications::lock_application(&mut tx, preview.application_id).await?;
        let locked = sqlx::query(&format!("{RELEASE_SELECT} where id = $1 for update"))
            .bind(release_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let release = row_release(locked);
        if !matches!(release.state.as_str(), "ready" | "failed" | "unknown") {
            return Err(ApplicationError::WorkspaceBusy);
        }
        remember_existing_intents(&mut tx, release.application_id).await?;
        let referenced: bool = sqlx::query_scalar(
            "select exists(select 1 from application_deployments where release_id = $1) \
             or exists(select 1 from approval_requests where release_id = $1) \
             or exists(select 1 from database_operations where release_id = $1)",
        )
        .bind(release_id)
        .fetch_one(&mut *tx)
        .await?;
        if referenced {
            return Err(ApplicationError::ReleaseInUse);
        }
        if let Some(key) = release.artifact_key.as_deref() {
            sqlx::query(
                "select id from application_releases \
                 where artifact_key = $1 order by id for update",
            )
            .bind(key)
            .fetch_all(&mut *tx)
            .await?;
            let shared: i64 = sqlx::query_scalar(
                "select count(*) from application_releases \
                 where artifact_key = $1 and id <> $2",
            )
            .bind(key)
            .bind(release_id)
            .fetch_one(&mut *tx)
            .await?;
            if shared == 0 {
                let Some(blob) = blob else {
                    return Err(ApplicationError::Kernel(crate::KernelError::Database));
                };
                blob.delete(key)
                    .await
                    .map_err(|_| ApplicationError::Kernel(crate::KernelError::Database))?;
            }
        }
        sqlx::query("delete from application_releases where id = $1")
            .bind(release_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Deletes every distinct Release Blob for one Application, then clears
    /// `artifact_key` / `artifact_bytes` so the storage quota is released.
    /// The intent ledger is left in place. Deployment rows may still
    /// reference the bulky Release identity.
    pub async fn reclaim_application_blobs(
        &self,
        application_id: Uuid,
        blob: Option<&BlobStore>,
    ) -> Result<(), ApplicationError> {
        let mut tx = self.pool.begin().await?;
        applications::lock_application(&mut tx, application_id).await?;
        sqlx::query(
            "select id from application_releases where application_id = $1 order by id for update",
        )
        .bind(application_id)
        .fetch_all(&mut *tx)
        .await?;
        remember_existing_intents(&mut tx, application_id).await?;
        let keys: Vec<String> = sqlx::query_scalar(
            "select distinct artifact_key from application_releases \
             where application_id = $1 and artifact_key is not null \
             order by artifact_key",
        )
        .bind(application_id)
        .fetch_all(&mut *tx)
        .await?;
        if !keys.is_empty() {
            let Some(blob) = blob else {
                return Err(ApplicationError::Kernel(crate::KernelError::Database));
            };
            for key in &keys {
                blob.delete(key)
                    .await
                    .map_err(|_| ApplicationError::Kernel(crate::KernelError::Database))?;
            }
        }
        sqlx::query(
            "update application_releases \
             set artifact_key = null, artifact_bytes = 0 \
             where application_id = $1",
        )
        .bind(application_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

fn hex_sha(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_sha256_hex(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

const RELEASE_SELECT: &str = "select id, application_id, build_intent_id, request_hash, \
     source_workspace_id, source_exec_generation, runtime_profile, manifest, manifest_hash, \
     artifact_key, artifact_hash, artifact_bytes, test_summary, state, created_by_user_id, \
     created_at::text as created_at from application_releases";

async fn load(pool: &PgPool, id: Uuid) -> Result<Option<Release>, sqlx::Error> {
    let row = sqlx::query(&format!("{RELEASE_SELECT} where id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(row_release))
}

async fn load_by_intent(pool: &PgPool, intent: Uuid) -> Result<Option<Release>, sqlx::Error> {
    let row = sqlx::query(&format!("{RELEASE_SELECT} where build_intent_id = $1"))
        .bind(intent)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(row_release))
}

fn row_release(row: sqlx::postgres::PgRow) -> Release {
    Release {
        id: row.get("id"),
        application_id: row.get("application_id"),
        build_intent_id: row.get("build_intent_id"),
        request_hash: row.get("request_hash"),
        source_workspace_id: row.get("source_workspace_id"),
        source_exec_generation: row.get("source_exec_generation"),
        runtime_profile: row.get("runtime_profile"),
        manifest: row.get("manifest"),
        manifest_hash: row.get("manifest_hash"),
        artifact_key: row.get("artifact_key"),
        artifact_hash: row.get("artifact_hash"),
        artifact_bytes: row.get("artifact_bytes"),
        test_summary: row.get("test_summary"),
        state: row.get("state"),
        created_by_user_id: row.get("created_by_user_id"),
        created_at: row.get("created_at"),
    }
}

impl ReleaseStore {
    pub async fn get_internal(&self, release_id: Uuid) -> Result<Release, ApplicationError> {
        load(&self.pool, release_id)
            .await?
            .ok_or(ApplicationError::NotFound)
    }

    pub async fn get_internal_by_intent(
        &self,
        build_intent_id: Uuid,
    ) -> Result<Release, ApplicationError> {
        load_by_intent(&self.pool, build_intent_id)
            .await?
            .ok_or(ApplicationError::NotFound)
    }
}

async fn reserved_artifact_bytes(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    application_id: Option<Uuid>,
    project_id: Option<Uuid>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "select coalesce(sum( \
            case when r.state in ('reserved', 'dispatched') then $1 \
                 else coalesce(r.artifact_bytes, 0) end \
         ), 0)::bigint \
         from application_releases r \
         join applications a on a.id = r.application_id \
         where r.state in ('ready', 'reserved', 'dispatched') \
           and ($2::uuid is null or r.application_id = $2) \
           and ($3::uuid is null or a.project_id = $3)",
    )
    .bind(MAX_PACKED_ARTIFACT_BYTES)
    .bind(application_id)
    .bind(project_id)
    .fetch_one(&mut **tx)
    .await
}

async fn remember_existing_intents(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    application_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into application_release_intents \
         (build_intent_id, application_id, request_hash, class, release_id, created_at) \
         select build_intent_id, application_id, request_hash, \
                case when state in ('reserved', 'dispatched') then 'dispatched' else state end, \
                id, created_at \
         from application_releases \
         where application_id = $1 \
         on conflict (build_intent_id) do nothing",
    )
    .bind(application_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn load_intent(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    build_intent_id: Uuid,
) -> Result<Option<(String, Option<Uuid>, Vec<u8>)>, sqlx::Error> {
    let row = sqlx::query(
        "select class, release_id, request_hash from application_release_intents \
         where build_intent_id = $1",
    )
    .bind(build_intent_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|row| {
        (
            row.get("class"),
            row.get("release_id"),
            row.get("request_hash"),
        )
    }))
}

async fn trim_failed_objects(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    application_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "delete from application_releases \
         where id in ( \
            select id from ( \
                select r.id, row_number() over (order by r.created_at desc, r.id desc) as rn \
                from application_releases r \
                where r.application_id = $1 \
                  and r.state = 'failed' \
                  and r.artifact_key is null \
                  and not exists ( \
                      select 1 from application_deployments d where d.release_id = r.id \
                  ) \
                  and not exists ( \
                      select 1 from approval_requests a where a.release_id = r.id \
                  ) \
                  and not exists ( \
                      select 1 from database_operations o where o.release_id = r.id \
                  ) \
            ) ranked \
            where rn > $2 \
         )",
    )
    .bind(application_id)
    .bind(MAX_FAILED_RELEASE_TOMBSTONES_PER_APPLICATION)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn mark_intent_class(
    pool: &PgPool,
    build_intent_id: Uuid,
    class: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into application_release_intents \
         (build_intent_id, application_id, request_hash, class, release_id, created_at) \
         select build_intent_id, application_id, request_hash, $2, id, created_at \
         from application_releases where build_intent_id = $1 \
         on conflict (build_intent_id) do update \
         set class = excluded.class \
         where application_release_intents.class = 'dispatched'",
    )
    .bind(build_intent_id)
    .bind(class)
    .execute(pool)
    .await?;
    sqlx::query(
        "update application_release_intents set class = $2 \
         where build_intent_id = $1 and class = 'dispatched'",
    )
    .bind(build_intent_id)
    .bind(class)
    .execute(pool)
    .await?;
    Ok(())
}

fn begin_from_class(class: &str, id: Uuid) -> BeginRelease {
    match class {
        "ready" => BeginRelease::Ready { id },
        "failed" => BeginRelease::Failed { id },
        _ => BeginRelease::OutcomeUnknown,
    }
}
