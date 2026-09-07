//! Fabric product API refuses infrastructure objects and journals no-replay.

use serde_json::json;
use voie_fabricd::{JournalBody, reject_forbidden};

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
fn accepts_journal_body() {
    let body = json!({
        "operation_id": "11111111-1111-1111-1111-111111111111",
        "request_hash": "deadbeef",
        "desired_revision": 3,
        "migrate_argv": ["node", "dist/migrate.js"],
        "database_id": "22222222-2222-2222-2222-222222222222"
    });
    reject_forbidden(&body).expect("typed body is allowed");
    let parsed: JournalBody = serde_json::from_value(body).expect("parses");
    assert_eq!(parsed.desired_revision, 3);
    assert_eq!(parsed.migrate_argv.as_ref().map(Vec::len), Some(2));
    assert_eq!(
        parsed.database_id.as_deref(),
        Some("22222222-2222-2222-2222-222222222222")
    );
}

#[test]
fn realization_fields_are_not_on_the_journal_body() {
    let password = json!({
        "operation_id": "11111111-1111-1111-1111-111111111111",
        "request_hash": "deadbeef",
        "desired_revision": 1,
        "postgres_password": "once"
    });
    assert!(serde_json::from_value::<JournalBody>(password).is_err());
    let env = json!({
        "operation_id": "11111111-1111-1111-1111-111111111111",
        "request_hash": "deadbeef",
        "desired_revision": 1,
        "env_bindings": [{"name": "SESSION_SECRET", "value": "once"}]
    });
    assert!(serde_json::from_value::<JournalBody>(env).is_err());
    let run = json!({
        "operation_id": "11111111-1111-1111-1111-111111111111",
        "request_hash": "deadbeef",
        "desired_revision": 1,
        "run_argv": ["node", "dist/server.js"]
    });
    assert!(serde_json::from_value::<JournalBody>(run).is_err());
    for key in [
        "slug",
        "kind",
        "port",
        "health_path",
        "console_host",
        "previous_deployment_id",
        "previousDeploymentId",
    ] {
        let mut body = json!({
            "operation_id": "11111111-1111-1111-1111-111111111111",
            "request_hash": "deadbeef",
            "desired_revision": 1
        });
        body[key] = json!("x");
        assert!(
            serde_json::from_value::<JournalBody>(body).is_err(),
            "{key} belongs on the stored spec"
        );
    }
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
