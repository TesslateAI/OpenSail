//! Server-side user-secret metadata and storage boundary.
//!
//! `SecretsStore` persists only ownership, scope, version, and the opaque
//! Key Vault/local-backend name.  The backend receives material for writes and
//! deletion, but this module has no value-read operation.  Metadata, audit
//! events, and errors therefore cannot carry secret material.

pub mod file_backend;
pub mod keyvault_backend;
pub mod memory_backend;

pub use file_backend::{DEFAULT_SECRETS_DIR, FileSecretBackend, SECRETS_DIR_ENV, SECRETS_KEY_ENV};
pub use keyvault_backend::AzureSecretBackend;
pub use memory_backend::InMemorySecretBackend;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// A boxed asynchronous backend operation.
pub type BackendFuture<'a> =
    Pin<Box<dyn Future<Output = Result<BackendWrite, BackendError>> + Send + 'a>>;

/// A boxed asynchronous scope-authorization operation.
pub type ScopeCapabilityFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ScopeCapability, ScopeAuthorizationError>> + Send + 'a>>;

/// The configured material backend.  Selection belongs to deployment
/// configuration; the vault never branches on an estate/provider at runtime.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// Azure Key Vault in the production profile.
    KeyVault,
    /// Encrypted local files for development-only storage.
    LocalEncrypted,
}

impl BackendKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            BackendKind::KeyVault => "azure-keyvault",
            BackendKind::LocalEncrypted => "local-encrypted",
        }
    }
}

impl fmt::Debug for BackendKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Opaque backend reference for one secret.  Its name identifies material in
/// the backend but is never part of a metadata or audit response.
pub struct SecretReference {
    backend: BackendKind,
    name: String,
}

impl SecretReference {
    fn for_secret(backend: BackendKind, secret_id: Uuid) -> Self {
        // UUIDs contain only characters accepted by the Key Vault name rules.
        SecretReference {
            backend,
            name: format!("us-{secret_id}"),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(backend: BackendKind, secret_id: Uuid) -> Self {
        Self::for_secret(backend, secret_id)
    }

    /// Backend selected for this reference.
    pub const fn backend(&self) -> BackendKind {
        self.backend
    }

    /// Opaque backend name consumed by a material backend implementation.
    ///
    /// This is a reference, not secret material.  It is intentionally absent
    /// from `SecretMetadata` and `SecretAuditEvent`.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Debug for SecretReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecretReference")
            .field("backend", &self.backend)
            .field("name", &"<opaque>")
            .finish()
    }
}

/// Secret material supplied for a write.
///
/// This type deliberately does not implement `Debug`, `Display`, `Clone`, or
/// serialization.  It is consumed by `SecretBackend::put` and is never
/// present in metadata, audit events, or errors.
pub struct SecretValue(Vec<u8>);

impl SecretValue {
    /// Creates material from bytes, rejecting an empty value.
    pub fn from_bytes(value: impl Into<Vec<u8>>) -> Result<Self, SecretsError> {
        let value = value.into();
        if value.is_empty() {
            return Err(SecretsError::EmptyValue);
        }
        Ok(Self(value))
    }

    /// Creates material from text, rejecting an empty value.
    pub fn from_text(value: impl Into<String>) -> Result<Self, SecretsError> {
        Self::from_bytes(value.into().into_bytes())
    }

    /// Length for bounded request handling and tests.  Material itself is not
    /// returned by any vault operation.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Gives a backend implementation access to the bytes it owns.
    ///
    /// Callers must not copy this into metadata, events, logs, or responses.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            *byte = 0;
        }
    }
}

/// Result of a backend write.  `changed = false` lets a backend make an
/// identical-value retry idempotent without exposing material to this module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackendWrite {
    pub changed: bool,
}

impl BackendWrite {
    pub const fn changed() -> Self {
        Self { changed: true }
    }

    pub const fn unchanged() -> Self {
        Self { changed: false }
    }
}

/// Typed backend failure.  Implementations must not put provider responses or
/// material in this error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackendError;

impl fmt::Display for BackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("secret backend operation failed")
    }
}

impl Error for BackendError {}

/// Material ownership boundary for Key Vault and local encrypted storage.
///
/// There is intentionally no `get`, `read`, or `fetch` method.  The control
/// plane writes and deletes material, while API responses expose metadata only.
/// Deployment-selected material backend. The concrete enum keeps
/// [`SecretsStore`] (and therefore `Services`) free of trait objects while
/// configuration still decides where material lives. Both arms share the
/// local `local-encrypted` backend kind: neither is a Key Vault.
#[derive(Debug)]
pub enum MaterialBackend {
    /// Process-local storage for tests and ephemeral processes.
    Memory(InMemorySecretBackend),
    /// Durable encrypted files for single-node deployments.
    File(FileSecretBackend),
    /// Production Azure Key Vault write-only material backend.
    KeyVault(AzureSecretBackend),
}

impl SecretBackend for MaterialBackend {
    fn kind(&self) -> BackendKind {
        match self {
            MaterialBackend::Memory(_) | MaterialBackend::File(_) => BackendKind::LocalEncrypted,
            MaterialBackend::KeyVault(inner) => inner.kind(),
        }
    }

    fn put<'a>(&'a self, reference: &'a SecretReference, value: SecretValue) -> BackendFuture<'a> {
        match self {
            MaterialBackend::Memory(inner) => inner.put(reference, value),
            MaterialBackend::File(inner) => inner.put(reference, value),
            MaterialBackend::KeyVault(inner) => inner.put(reference, value),
        }
    }

    fn delete<'a>(&'a self, reference: &'a SecretReference) -> BackendFuture<'a> {
        match self {
            MaterialBackend::Memory(inner) => inner.delete(reference),
            MaterialBackend::File(inner) => inner.delete(reference),
            MaterialBackend::KeyVault(inner) => inner.delete(reference),
        }
    }
}

impl MaterialBackend {
    /// Environment variable naming the deployment-selected material backend.
    pub const SELECTION_ENV: &'static str = "VOIE_USER_SECRETS_BACKEND";

    /// Resolves a selection value read from [`Self::SELECTION_ENV`] into
    /// the concrete backend, owning every arm including production Key
    /// Vault selection.
    ///
    /// * absent / `local-encrypted` — durable encrypted files;
    /// * `memory` — process-local storage for tests;
    /// * `key-vault` — [`AzureSecretBackend`] via
    ///   [`keyvault_backend::KEY_VAULT_URI_ENV`]; a missing vault URI is
    ///   refused explicitly instead of degrading to weaker storage;
    /// * anything else is refused explicitly.
    pub fn from_selection(selected: &str, database_url: &str) -> Result<Self, String> {
        match selected.trim() {
            "" | "local-encrypted" => {
                FileSecretBackend::from_env(database_url).map(MaterialBackend::File)
            }
            "memory" => Ok(MaterialBackend::Memory(InMemorySecretBackend::new())),
            "key-vault" => AzureSecretBackend::from_env().map(MaterialBackend::KeyVault),
            other => Err(format!(
                "VOIE_USER_SECRETS_BACKEND={other} is unavailable; \
                 configure local-encrypted, memory, or key-vault"
            )),
        }
    }
}

pub trait SecretBackend: Send + Sync {
    fn kind(&self) -> BackendKind;

    fn put<'a>(&'a self, reference: &'a SecretReference, value: SecretValue) -> BackendFuture<'a>;

    fn delete<'a>(&'a self, reference: &'a SecretReference) -> BackendFuture<'a>;
}

/// Scope capability returned by the authorization boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeCapability {
    /// The actor has no access to this project scope.
    None,
    /// The actor may inspect metadata and audit events.
    Read,
    /// The actor may inspect metadata and mutate material/metadata.
    Write,
}

impl ScopeCapability {
    pub const fn can_read(self) -> bool {
        matches!(self, ScopeCapability::Read | ScopeCapability::Write)
    }

    pub const fn can_write(self) -> bool {
        matches!(self, ScopeCapability::Write)
    }
}

/// Typed authorization-boundary failure with no identity or policy details.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScopeAuthorizationError;

impl fmt::Display for ScopeAuthorizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("scope authorization failed")
    }
}

impl Error for ScopeAuthorizationError {}

/// Fixed project-membership authorization interface.
///
/// The caller supplies the authenticated user and project (`scope_id` is the
/// project id).  Implementations map owner/admin/member to `Write`, viewer to
/// `Read`, and no membership to `None`; platform-admin handling remains in the
/// implementation that owns authentication.
pub trait ScopeAuthorizer: Send + Sync {
    fn scope_capability<'a>(
        &'a self,
        actor_user_id: Uuid,
        scope_id: Uuid,
    ) -> ScopeCapabilityFuture<'a>;
}

/// Metadata returned to API callers.  It has no backend reference or value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretMetadata {
    pub id: Uuid,
    /// The project id used as the API's `scopeId`.
    pub scope_id: Uuid,
    pub name: String,
    pub version: i64,
    pub created_by: Uuid,
    /// RFC3339 text emitted by PostgreSQL's timestamptz text representation.
    pub created_at: String,
    pub updated_at: String,
    pub can_write: bool,
}

/// Metadata list plus the server-derived scope capability used by the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretMetadataList {
    pub secrets: Vec<SecretMetadata>,
    pub can_write: bool,
}

/// Audit action persisted in the shared PostgreSQL audit index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretAuditAction {
    Created,
    Updated,
    Rotated,
    Deleted,
}

impl SecretAuditAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            SecretAuditAction::Created => "secret.created",
            SecretAuditAction::Updated => "secret.updated",
            SecretAuditAction::Rotated => "secret.rotated",
            SecretAuditAction::Deleted => "secret.deleted",
        }
    }

    fn from_str(kind: &str) -> Option<Self> {
        match kind {
            "secret.created" => Some(Self::Created),
            "secret.updated" => Some(Self::Updated),
            "secret.rotated" => Some(Self::Rotated),
            "secret.deleted" => Some(Self::Deleted),
            _ => None,
        }
    }

    /// Short wire action used by the browser API.
    pub const fn wire_name(self) -> &'static str {
        match self {
            SecretAuditAction::Created => "created",
            SecretAuditAction::Updated => "updated",
            SecretAuditAction::Rotated => "rotated",
            SecretAuditAction::Deleted => "deleted",
        }
    }
}

/// Metadata-only audit projection.  It deliberately has no value, backend
/// material, or request body fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretAuditEvent {
    pub secret_id: Uuid,
    pub action: SecretAuditAction,
    pub actor: Uuid,
    pub at: String,
    pub version: Option<i64>,
}

/// Stable errors at the later router boundary.  Display text is fixed and
/// never includes SQL/provider errors, names, identities, or material.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretsError {
    AccessDenied,
    AuthorizationUnavailable,
    InvalidName,
    EmptyValue,
    Backend,
    Database,
    RelationRefused,
    NotFound,
    Conflict,
}

impl fmt::Display for SecretsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            SecretsError::AccessDenied => "secret scope access denied",
            SecretsError::AuthorizationUnavailable => "secret scope authorization unavailable",
            SecretsError::InvalidName => "secret name is invalid",
            SecretsError::EmptyValue => "secret value is empty",
            SecretsError::Backend => "secret backend operation failed",
            SecretsError::Database => "secret metadata operation failed",
            SecretsError::RelationRefused => "secret scope reference was refused",
            SecretsError::NotFound => "secret was not found",
            SecretsError::Conflict => "secret name already exists in this scope",
        };
        formatter.write_str(message)
    }
}

impl Error for SecretsError {}

impl From<BackendError> for SecretsError {
    fn from(_: BackendError) -> Self {
        SecretsError::Backend
    }
}

fn map_database_error(error: sqlx::Error) -> SecretsError {
    let code = error
        .as_database_error()
        .and_then(|database_error| database_error.code());
    match code.as_deref() {
        Some("23503") => SecretsError::RelationRefused,
        Some("23505") => SecretsError::Conflict,
        _ => SecretsError::Database,
    }
}

fn validate_name(name: &str) -> Result<(), SecretsError> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed != name
        || name.chars().count() > 128
        || name.chars().any(char::is_control)
    {
        return Err(SecretsError::InvalidName);
    }
    Ok(())
}

struct SecretRecord {
    metadata: SecretMetadata,
    reference: SecretReference,
}

fn metadata_from_row(row: PgRow, can_write: bool) -> SecretMetadata {
    SecretMetadata {
        id: row.get("id"),
        scope_id: row.get("scope_id"),
        name: row.get("name"),
        version: row.get("version"),
        created_by: row.get("created_by"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        can_write,
    }
}

fn record_from_row(row: PgRow, backend: BackendKind, can_write: bool) -> SecretRecord {
    let reference = SecretReference {
        backend,
        name: row.get("kv_name"),
    };
    SecretRecord {
        metadata: metadata_from_row(row, can_write),
        reference,
    }
}

const FIND_RECORD_SQL: &str = "select id, scope_id, name, kv_name, version, created_by, created_at::text as created_at, updated_at::text as updated_at from user_secrets where id = $1";
const FIND_RECORD_FOR_UPDATE_SQL: &str = "select id, scope_id, name, kv_name, version, created_by, created_at::text as created_at, updated_at::text as updated_at from user_secrets where id = $1 for update";
const LIST_METADATA_SQL: &str = "select id, scope_id, name, version, created_by, created_at::text as created_at, updated_at::text as updated_at from user_secrets where scope_id = $1 order by name, id";
const CREATE_METADATA_SQL: &str = "insert into user_secrets (id, scope_id, name, kv_name, version, created_by) values ($1, $2, $3, $4, 1, $5) returning id, scope_id, name, version, created_by, created_at::text as created_at, updated_at::text as updated_at";
const UPDATE_VERSION_SQL: &str = "update user_secrets set version = version + 1, updated_at = now() where id = $1 and version = $2 returning id, scope_id, name, version, created_by, created_at::text as created_at, updated_at::text as updated_at";
const DELETE_METADATA_SQL: &str = "delete from user_secrets where id = $1";

// The shared audit index owns event ordering.  `metadata` contains only the
// scope, display name, and numeric version; it never contains material.
const INSERT_AUDIT_SQL: &str = "insert into audit_events (kind, resource_type, resource_id, actor_user_id, metadata, outcome) values ($1, 'secret', $2, $3, jsonb_build_object('scopeId', $4::text, 'name', $5::text, 'version', $6::bigint), 'ok')";
const FIND_SCOPE_FROM_AUDIT_SQL: &str = "select metadata->>'scopeId' as scope_id from audit_events where resource_type = 'secret' and resource_id = $1 order by seq desc limit 1";
const LIST_AUDIT_SQL: &str = "select kind, actor_user_id, occurred_at::text as at, nullif(metadata->>'version', '')::bigint as version from audit_events where resource_type = 'secret' and resource_id = $1 and kind in ('secret.created', 'secret.updated', 'secret.rotated', 'secret.deleted') order by seq";

/// PostgreSQL-backed metadata store with an injected material backend and
/// project-scope authorizer.
pub struct SecretsStore<B, A> {
    pool: PgPool,
    backend: B,
    authorizer: A,
}

impl<B, A> SecretsStore<B, A>
where
    B: SecretBackend,
    A: ScopeAuthorizer,
{
    pub fn new(pool: PgPool, backend: B, authorizer: A) -> Self {
        Self {
            pool,
            backend,
            authorizer,
        }
    }

    pub fn from_pool(pool: &PgPool, backend: B, authorizer: A) -> Self {
        Self::new(pool.clone(), backend, authorizer)
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    async fn capability(
        &self,
        actor_user_id: Uuid,
        scope_id: Uuid,
    ) -> Result<ScopeCapability, SecretsError> {
        self.authorizer
            .scope_capability(actor_user_id, scope_id)
            .await
            .map_err(|_| SecretsError::AuthorizationUnavailable)
    }

    async fn require_read(
        &self,
        actor_user_id: Uuid,
        scope_id: Uuid,
    ) -> Result<ScopeCapability, SecretsError> {
        let capability = self.capability(actor_user_id, scope_id).await?;
        if !capability.can_read() {
            return Err(SecretsError::AccessDenied);
        }
        Ok(capability)
    }

    async fn require_write(&self, actor_user_id: Uuid, scope_id: Uuid) -> Result<(), SecretsError> {
        let capability = self.capability(actor_user_id, scope_id).await?;
        if !capability.can_write() {
            return Err(SecretsError::AccessDenied);
        }
        Ok(())
    }

    /// Lists metadata in one project scope.  `scope_id` is the project id;
    /// deleted rows are absent, while their audit rows remain queryable.
    pub async fn list_metadata(
        &self,
        actor_user_id: Uuid,
        scope_id: Uuid,
    ) -> Result<SecretMetadataList, SecretsError> {
        let capability = self.require_read(actor_user_id, scope_id).await?;
        let rows = sqlx::query(LIST_METADATA_SQL)
            .bind(scope_id)
            .fetch_all(&self.pool)
            .await
            .map_err(map_database_error)?;
        let can_write = capability.can_write();
        let secrets = rows
            .into_iter()
            .map(|row| metadata_from_row(row, can_write))
            .collect();
        Ok(SecretMetadataList { secrets, can_write })
    }

    /// Creates one metadata row and writes material to the configured backend.
    /// The initial material set is version one; the database has a zero
    /// default so an independently inserted row remains representable.
    pub async fn create(
        &self,
        actor_user_id: Uuid,
        scope_id: Uuid,
        name: impl Into<String>,
        value: SecretValue,
    ) -> Result<SecretMetadata, SecretsError> {
        self.require_write(actor_user_id, scope_id).await?;
        let name = name.into();
        validate_name(&name)?;

        let secret_id = Uuid::new_v4();
        let reference = SecretReference::for_secret(self.backend.kind(), secret_id);
        let _write = self.backend.put(&reference, value).await?;

        let row = match sqlx::query(CREATE_METADATA_SQL)
            .bind(secret_id)
            .bind(scope_id)
            .bind(&name)
            .bind(reference.name())
            .bind(actor_user_id)
            .fetch_one(&self.pool)
            .await
        {
            Ok(row) => row,
            Err(error) => {
                // A metadata conflict must not leave an owned backend object.
                let _ = self.backend.delete(&reference).await;
                return Err(map_database_error(error));
            }
        };
        let metadata = metadata_from_row(row, true);
        self.append_audit(SecretAuditAction::Created, &metadata, actor_user_id)
            .await;
        Ok(metadata)
    }

    async fn set_value(
        &self,
        actor_user_id: Uuid,
        secret_id: Uuid,
        value: SecretValue,
        action: SecretAuditAction,
    ) -> Result<SecretMetadata, SecretsError> {
        // Lock the row before handing the new material to the backend.  This
        // serializes concurrent update/rotate/delete metadata transitions.
        let mut transaction = self.pool.begin().await.map_err(map_database_error)?;
        let row = sqlx::query(FIND_RECORD_FOR_UPDATE_SQL)
            .bind(secret_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_database_error)?
            .ok_or(SecretsError::NotFound)?;
        let record = record_from_row(row, self.backend.kind(), true);
        self.require_write(actor_user_id, record.metadata.scope_id)
            .await?;

        let write = self.backend.put(&record.reference, value).await?;
        if !write.changed {
            transaction.commit().await.map_err(map_database_error)?;
            return Ok(record.metadata);
        }

        let row = sqlx::query(UPDATE_VERSION_SQL)
            .bind(secret_id)
            .bind(record.metadata.version)
            .fetch_one(&mut *transaction)
            .await
            .map_err(map_database_error)?;
        let metadata = metadata_from_row(row, true);
        transaction.commit().await.map_err(map_database_error)?;
        self.append_audit(action, &metadata, actor_user_id).await;
        Ok(metadata)
    }

    /// Replaces material and advances its metadata version.
    pub async fn replace_value(
        &self,
        actor_user_id: Uuid,
        secret_id: Uuid,
        value: SecretValue,
    ) -> Result<SecretMetadata, SecretsError> {
        self.set_value(actor_user_id, secret_id, value, SecretAuditAction::Updated)
            .await
    }

    /// Rotates material and advances its metadata version.
    pub async fn rotate(
        &self,
        actor_user_id: Uuid,
        secret_id: Uuid,
        value: SecretValue,
    ) -> Result<SecretMetadata, SecretsError> {
        self.set_value(actor_user_id, secret_id, value, SecretAuditAction::Rotated)
            .await
    }

    /// Deletes backend material and then its metadata row.  Audit rows use no
    /// foreign key to the secret, so the deletion audit remains available.
    pub async fn delete(&self, actor_user_id: Uuid, secret_id: Uuid) -> Result<(), SecretsError> {
        let mut transaction = self.pool.begin().await.map_err(map_database_error)?;
        let row = sqlx::query(FIND_RECORD_FOR_UPDATE_SQL)
            .bind(secret_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(map_database_error)?
            .ok_or(SecretsError::NotFound)?;
        let record = record_from_row(row, self.backend.kind(), true);
        self.require_write(actor_user_id, record.metadata.scope_id)
            .await?;

        self.backend.delete(&record.reference).await?;
        let result = sqlx::query(DELETE_METADATA_SQL)
            .bind(secret_id)
            .execute(&mut *transaction)
            .await
            .map_err(map_database_error)?;
        if result.rows_affected() != 1 {
            return Err(SecretsError::NotFound);
        }
        transaction.commit().await.map_err(map_database_error)?;
        self.append_audit(SecretAuditAction::Deleted, &record.metadata, actor_user_id)
            .await;
        Ok(())
    }

    /// Lists metadata-only audit events for a secret.  Deleted secrets resolve
    /// their scope from the latest retained audit row before authorization.
    pub async fn audit(
        &self,
        actor_user_id: Uuid,
        secret_id: Uuid,
    ) -> Result<Vec<SecretAuditEvent>, SecretsError> {
        let scope_id =
            match sqlx::query_scalar::<_, Uuid>("select scope_id from user_secrets where id = $1")
                .bind(secret_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(map_database_error)?
            {
                Some(scope_id) => scope_id,
                None => {
                    let scope_text = sqlx::query_scalar::<_, String>(FIND_SCOPE_FROM_AUDIT_SQL)
                        .bind(secret_id)
                        .fetch_optional(&self.pool)
                        .await
                        .map_err(map_database_error)?
                        .ok_or(SecretsError::NotFound)?;
                    Uuid::parse_str(&scope_text).map_err(|_| SecretsError::Database)?
                }
            };
        self.require_read(actor_user_id, scope_id).await?;

        let rows = sqlx::query(LIST_AUDIT_SQL)
            .bind(secret_id)
            .fetch_all(&self.pool)
            .await
            .map_err(map_database_error)?;
        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            let Some(action) = SecretAuditAction::from_str(row.get::<String, _>("kind").as_str())
            else {
                continue;
            };
            events.push(SecretAuditEvent {
                secret_id,
                action,
                actor: row.get("actor_user_id"),
                at: row.get("at"),
                version: row.get("version"),
            });
        }
        Ok(events)
    }

    async fn append_audit(
        &self,
        action: SecretAuditAction,
        metadata: &SecretMetadata,
        actor_user_id: Uuid,
    ) {
        // Audit is append-only and best effort by design.  A failed audit
        // write never rolls back a successful metadata/backend operation.
        let _ = sqlx::query(INSERT_AUDIT_SQL)
            .bind(action.as_str())
            .bind(metadata.id)
            .bind(actor_user_id)
            .bind(metadata.scope_id)
            .bind(&metadata.name)
            .bind(metadata.version)
            .execute(&self.pool)
            .await;
    }
}
