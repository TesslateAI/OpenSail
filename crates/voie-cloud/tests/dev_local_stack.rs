//! Focused checks for the local dev cloud stack: real PostgreSQL plus real
//! Azure Blob HTTP semantics exercised through the product clients.
//!
//! Ignored by default; `just dev-cloud-check` runs them against the stack
//! started by `just dev-cloud-up`.

use uuid::Uuid;
use voie_cloud::session_store::{BlobStore, BlobStoreError};

fn required_env(name: &str) -> String {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => panic!("required environment value {name} is missing; run 'just dev-cloud-up'"),
    }
}

#[tokio::test]
#[ignore = "requires the local dev cloud stack (just dev-cloud-up)"]
async fn postgres_accepts_connections() {
    let url = required_env("VOIE_DATABASE_URL");
    let pool = sqlx::PgPool::connect(&url)
        .await
        .expect("PostgreSQL accepts the configured connection");
    let one: i32 = sqlx::query_scalar("select 1")
        .fetch_one(&pool)
        .await
        .expect("PostgreSQL answers queries");
    assert_eq!(one, 1);
}

#[tokio::test]
#[ignore = "requires the local dev cloud stack (just dev-cloud-up)"]
async fn blob_put_if_absent_and_get_round_trip() {
    let store = BlobStore::from_env().expect("blob configuration loads");
    let object_key = format!("dev-local-stack/{}/event.json", Uuid::new_v4());
    let bytes: &[u8] = br#"{"kind":"dev-local-stack","revision":7}"#;

    store
        .put_if_absent(&object_key, bytes)
        .await
        .expect("absent blob creates");
    store
        .put_if_absent(&object_key, bytes)
        .await
        .expect("identical retry succeeds");

    let conflicting: &[u8] = br#"{"kind":"dev-local-stack","revision":8}"#;
    match store.put_if_absent(&object_key, conflicting).await {
        Err(BlobStoreError::UnexpectedStatus) => {}
        other => panic!("conflicting bytes must fail with UnexpectedStatus, got {other:?}"),
    }

    let loaded = store.get(&object_key).await.expect("existing blob reads");
    assert_eq!(loaded.as_slice(), bytes);

    let missing = format!("dev-local-stack/{}/missing.json", Uuid::new_v4());
    match store.get(&missing).await {
        Err(BlobStoreError::Missing) => {}
        other => panic!("absent blob read must fail with Missing, got {other:?}"),
    }
}
