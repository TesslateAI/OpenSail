#[allow(dead_code)]
#[path = "../src/secrets/mod.rs"]
mod secrets;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use secrets::{
    BackendFuture, BackendKind, BackendWrite, ScopeAuthorizationError, ScopeAuthorizer,
    ScopeCapability, ScopeCapabilityFuture, SecretBackend, SecretReference, SecretValue,
    SecretsError,
};
use uuid::Uuid;

struct StaticAuthorizer(ScopeCapability);

impl ScopeAuthorizer for StaticAuthorizer {
    fn scope_capability(&self, _actor_user_id: Uuid, _scope_id: Uuid) -> ScopeCapabilityFuture<'_> {
        let capability = self.0;
        Box::pin(async move { Ok(capability) })
    }
}

struct RecordingBackend {
    writes: Arc<AtomicUsize>,
}

impl SecretBackend for RecordingBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::LocalEncrypted
    }

    fn put<'a>(&'a self, reference: &'a SecretReference, value: SecretValue) -> BackendFuture<'a> {
        let writes = Arc::clone(&self.writes);
        let name = reference.name().to_string();
        let length = value.len();
        Box::pin(async move {
            assert!(name.starts_with("us-"));
            assert!(length > 0);
            writes.fetch_add(1, Ordering::Relaxed);
            Ok(BackendWrite::changed())
        })
    }

    fn delete<'a>(&'a self, _reference: &'a SecretReference) -> BackendFuture<'a> {
        Box::pin(async { Ok(BackendWrite::changed()) })
    }
}

#[test]
fn scope_capabilities_are_narrow_and_ordered() {
    assert!(!ScopeCapability::None.can_read());
    assert!(!ScopeCapability::None.can_write());
    assert!(ScopeCapability::Read.can_read());
    assert!(!ScopeCapability::Read.can_write());
    assert!(ScopeCapability::Write.can_read());
    assert!(ScopeCapability::Write.can_write());
}

#[test]
fn empty_material_is_rejected_without_a_debug_or_display_shape() {
    assert!(matches!(
        SecretValue::from_bytes(Vec::<u8>::new()),
        Err(SecretsError::EmptyValue)
    ));
    let material = SecretValue::from_text("secret-value").expect("non-empty material accepted");
    assert_eq!(material.len(), 12);
    assert!(!material.is_empty());
}

#[tokio::test]
async fn backend_receives_material_only_on_write() {
    let backend = RecordingBackend {
        writes: Arc::new(AtomicUsize::new(0)),
    };
    let reference = SecretReference::for_test(BackendKind::LocalEncrypted, Uuid::new_v4());
    let result = backend
        .put(
            &reference,
            SecretValue::from_text("write-only").expect("material accepted"),
        )
        .await
        .expect("backend write succeeds");
    assert!(result.changed);
    assert_eq!(backend.writes.load(Ordering::Relaxed), 1);
}

#[test]
fn references_are_redacted_and_metadata_events_have_no_value_field() {
    let id = Uuid::new_v4();
    let reference = SecretReference::for_test(BackendKind::KeyVault, id);
    let debug = format!("{reference:?}");
    assert!(!debug.contains(&id.to_string()));
    assert!(debug.contains("<opaque>"));

    let source = include_str!("../src/secrets/mod.rs");
    let metadata = source
        .split("pub struct SecretMetadata {")
        .nth(1)
        .expect("metadata type exists")
        .split("pub struct SecretMetadataList {")
        .next()
        .unwrap();
    let event = source
        .split("pub struct SecretAuditEvent {")
        .nth(1)
        .expect("audit type exists")
        .split("pub enum SecretsError {")
        .next()
        .unwrap();
    assert!(
        !metadata
            .lines()
            .any(|line| line.trim_start().starts_with("pub value"))
    );
    assert!(
        !event
            .lines()
            .any(|line| line.trim_start().starts_with("pub value"))
    );
}

#[test]
fn migration_has_metadata_reference_and_version_but_no_material_column() {
    let migration = include_str!("../migrations/0008_user_secrets.sql");
    let table = migration
        .split("create table user_secrets (")
        .nth(1)
        .expect("user_secrets table exists")
        .split(");")
        .next()
        .unwrap();
    for column in ["id", "scope_id", "name", "kv_name", "version", "created_by"] {
        assert!(table.contains(column), "missing metadata column {column}");
    }
    assert!(!table.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("value ") || line.starts_with("secret_value ")
    }));
    assert!(!table.contains("secret_versions"));
}

#[test]
fn authorization_failure_type_has_no_provider_detail() {
    let error = ScopeAuthorizationError;
    assert_eq!(error.to_string(), "scope authorization failed");
    let _ = StaticAuthorizer(ScopeCapability::Read);
}
