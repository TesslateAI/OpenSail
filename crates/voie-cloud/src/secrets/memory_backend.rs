//! In-memory material backend for tests and ephemeral processes.
//!
//! Values live in process memory only and disappear with the process, so this
//! backend is correct wherever durability is not a property of the deployment.
//! It never logs, serializes, or returns material; `put`/`delete` are the only
//! operations, matching the write-only [`SecretBackend`] boundary.

use std::collections::HashMap;
use std::sync::Mutex;

use super::{
    BackendError, BackendFuture, BackendKind, BackendWrite, SecretBackend, SecretReference,
    SecretValue,
};

/// Process-local material store. The map key is the opaque backend name.
#[derive(Debug, Default)]
pub struct InMemorySecretBackend {
    values: Mutex<HashMap<String, Vec<u8>>>,
}

impl InMemorySecretBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test-only observability: how many references currently hold material.
    pub fn stored_count(&self) -> usize {
        self.values.lock().expect("memory backend mutex").len()
    }

    pub(crate) async fn get_material(
        &self,
        reference: &SecretReference,
    ) -> Result<SecretValue, BackendError> {
        let name = reference.name().to_owned();
        let bytes = self
            .values
            .lock()
            .expect("memory backend mutex")
            .get(&name)
            .cloned()
            .ok_or(BackendError)?;
        SecretValue::from_bytes(bytes).map_err(|_| BackendError)
    }
}

impl SecretBackend for InMemorySecretBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::LocalEncrypted
    }

    fn put<'a>(&'a self, reference: &'a SecretReference, value: SecretValue) -> BackendFuture<'a> {
        Box::pin(async move {
            let name = reference.name().to_owned();
            self.values
                .lock()
                .expect("memory backend mutex")
                .insert(name, value.as_bytes().to_vec());
            Ok(BackendWrite::changed())
        })
    }

    fn delete<'a>(&'a self, reference: &'a SecretReference) -> BackendFuture<'a> {
        Box::pin(async move {
            self.values
                .lock()
                .expect("memory backend mutex")
                .remove(reference.name());
            Ok(BackendWrite::changed())
        })
    }
}

// Keep the material type imported for the documented signature contract even
// though values are stored as raw bytes here.
const _: fn(&SecretValue) -> &[u8] = |value| value.as_bytes();

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn reference() -> SecretReference {
        SecretReference::for_test(BackendKind::LocalEncrypted, Uuid::new_v4())
    }

    #[tokio::test]
    async fn put_then_delete_round_trips_without_reading_material() {
        let backend = InMemorySecretBackend::new();
        let reference = reference();
        let value = SecretValue::from_text("material").expect("non-empty value");
        let write = backend.put(&reference, value).await.expect("put succeeds");
        assert!(write.changed);
        assert_eq!(backend.stored_count(), 1);

        let delete = backend.delete(&reference).await.expect("delete succeeds");
        assert!(delete.changed);
        assert_eq!(backend.stored_count(), 0);
    }

    #[tokio::test]
    async fn put_overwrites_one_reference_in_place() {
        let backend = InMemorySecretBackend::new();
        let reference = reference();
        let first = SecretValue::from_text("first").expect("non-empty value");
        let second = SecretValue::from_text("second").expect("non-empty value");
        backend.put(&reference, first).await.expect("first put");
        backend.put(&reference, second).await.expect("second put");
        assert_eq!(backend.stored_count(), 1, "one reference, replaced value");
    }

    #[tokio::test]
    async fn platform_injection_can_read_stored_material() {
        let backend = InMemorySecretBackend::new();
        let reference = reference();
        backend
            .put(
                &reference,
                SecretValue::from_text("injected").expect("non-empty"),
            )
            .await
            .expect("put");
        let material = backend.get_material(&reference).await.expect("get");
        assert_eq!(material.as_bytes(), b"injected");
    }
}
