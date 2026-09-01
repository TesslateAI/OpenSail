//! Fabric product API refuses infrastructure objects and journals no-replay.

use serde_json::json;
use voie_fabricd::{MutatingBody, reject_forbidden};

#[test]
fn refuses_kubernetes_and_caddy_fragments() {
    let forbidden = json!({
        "operation_id": "00000000-0000-0000-0000-000000000001",
        "request_hash": "abc",
        "desired_revision": 1,
        "image": "evil:latest"
    });
    assert!(reject_forbidden(&forbidden).is_err());
    let yaml = json!({
        "operation_id": "00000000-0000-0000-0000-000000000001",
        "request_hash": "abc",
        "desired_revision": 1,
        "yaml": "apiVersion: v1"
    });
    assert!(reject_forbidden(&yaml).is_err());
}

#[test]
fn accepts_typed_mutating_body() {
    let body = json!({
        "operation_id": "11111111-1111-1111-1111-111111111111",
        "request_hash": "deadbeef",
        "desired_revision": 3,
        "slug": "invoice-demo",
        "kind": "dev",
        "port": 3000,
        "run_argv": ["node", "dist/server.js"]
    });
    reject_forbidden(&body).expect("typed body is allowed");
    let parsed: MutatingBody = serde_json::from_value(body).expect("parses");
    assert_eq!(parsed.desired_revision, 3);
    assert_eq!(parsed.run_argv.as_ref().map(Vec::len), Some(2));
}

#[test]
fn postgres_password_is_allowed_once_and_not_infrastructure() {
    let body = json!({
        "operation_id": "11111111-1111-1111-1111-111111111111",
        "request_hash": "deadbeef",
        "desired_revision": 1,
        "slug": "invoice-demo",
        "kind": "dev",
        "postgres_password": "once"
    });
    reject_forbidden(&body).expect("password is not an infrastructure object");
    let parsed: MutatingBody = serde_json::from_value(body).expect("parses");
    assert_eq!(parsed.postgres_password.as_deref(), Some("once"));
}

#[test]
fn env_bindings_and_database_id_are_typed_not_infrastructure() {
    let body = json!({
        "operation_id": "11111111-1111-1111-1111-111111111111",
        "request_hash": "deadbeef",
        "desired_revision": 1,
        "slug": "invoice-demo",
        "kind": "dev",
        "database_id": "22222222-2222-2222-2222-222222222222",
        "env_bindings": [{"name": "SESSION_SECRET", "value": "once"}],
        "migrate_argv": ["node", "dist/migrate.js"]
    });
    reject_forbidden(&body).expect("typed env bindings are allowed");
    let parsed: MutatingBody = serde_json::from_value(body).expect("parses");
    assert_eq!(
        parsed.database_id.as_deref(),
        Some("22222222-2222-2222-2222-222222222222")
    );
    assert_eq!(parsed.env_bindings.as_ref().map(Vec::len), Some(1));
    assert_eq!(parsed.migrate_argv.as_ref().map(Vec::len), Some(2));
}

#[test]
fn run_argv_is_not_a_kubernetes_command() {
    let forbidden = json!({
        "operation_id": "00000000-0000-0000-0000-000000000001",
        "request_hash": "abc",
        "desired_revision": 1,
        "command": ["sh", "-c", "evil"]
    });
    assert!(reject_forbidden(&forbidden).is_err());
}
