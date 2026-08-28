use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::Row;
use uuid::Uuid;
use voie_cloud::session_store::{BlobStore, SessionStore};
use voie_cloud::{Config, Kernel, KernelError, RunState};

const UNUSED_BLOB_KEY: &str = "bm90LWEtcmVhbC1rZXk=";

/// Shared fixture: one Project with a ready Workspace and an Agent, plus a
/// conversation whose first Run is already accepted.
async fn conversation_fixture(kernel: &Kernel) -> (Uuid, Uuid, Uuid, Uuid, Uuid) {
    let owner = Uuid::new_v4();
    sqlx::query("insert into users (id, issuer, subject) values ($1, $2, $3)")
        .bind(owner)
        .bind("queue-test-issuer")
        .bind(owner.to_string())
        .execute(kernel.pool())
        .await
        .expect("test owner inserts");
    let project = kernel
        .create_project(Uuid::new_v4(), owner, "queue-project", "personal")
        .await
        .expect("Project creates");
    let fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .expect("test Fabric inserts");
    let workspace = Uuid::new_v4();
    sqlx::query(
        "insert into workspaces (id, project_id, fabric_id, state, created_by_user_id) \
         values ($1, $2, $3, 'ready', $4)",
    )
    .bind(workspace)
    .bind(project.id)
    .bind(fabric)
    .bind(owner)
    .execute(kernel.pool())
    .await
    .expect("test Workspace inserts");
    let agent = Uuid::new_v4();
    sqlx::query("insert into agents (id, project_id, name) values ($1, $2, $3)")
        .bind(agent)
        .bind(project.id)
        .bind(format!("agent-{agent}"))
        .execute(kernel.pool())
        .await
        .expect("test Agent inserts");
    let session = Uuid::new_v4();
    let (_, first) = kernel
        .create_conversation(
            session,
            project.id,
            agent,
            workspace,
            Uuid::new_v4(),
            Uuid::new_v4(),
            &[0u8; 32],
            "first message",
            owner,
        )
        .await
        .expect("conversation creates atomically");
    assert_eq!(first.seq, 1, "first Run owns turn ordinal 1");
    // Settle the first message so follow-ups are the queue head.
    kernel
        .dispatch_run(first.id)
        .await
        .expect("first Run dispatches");
    kernel
        .complete_run(first.id, r#"{"accepted":true}"#)
        .await
        .expect("first Run completes");
    (owner, project.id, session, agent, workspace)
}

#[tokio::test]
async fn concurrent_follow_ups_never_collide_on_seq() {
    let database_url = std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database");
    let kernel = Arc::new(
        Kernel::connect(&Config::database_url(database_url))
            .await
            .expect("PostgreSQL connection succeeds"),
    );
    kernel.migrate().await.expect("fresh migration succeeds");
    let (owner, _project, session, _agent, _workspace) = conversation_fixture(&kernel).await;

    // Concurrent follow-ups on one Session: the per-session advisory lock
    // serializes max(seq)+1 allocation, so every Run gets a distinct
    // durable turn ordinal and no unique (session_id, seq) violation.
    let mut handles = Vec::new();
    for i in 0..16 {
        let kernel = kernel.clone();
        handles.push(tokio::spawn(async move {
            let run = kernel
                .accept_run(
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                    session,
                    &[i as u8; 32],
                    "resume",
                    &format!("follow-up {i}"),
                    Some(owner),
                )
                .await
                .expect("concurrent follow-up accepts");
            run.seq
        }));
    }
    let mut seqs = Vec::new();
    for handle in handles {
        seqs.push(handle.await.expect("follow-up task completes"));
    }
    seqs.sort_unstable();
    let expected: Vec<i64> = (2..=17).collect();
    assert_eq!(
        seqs, expected,
        "every follow-up owns a distinct turn ordinal"
    );

    // The queue is strictly ordered: dispatch eligibility follows seq.
    let head = kernel
        .next_dispatchable_run_for_session(session)
        .await
        .expect("queue head reads")
        .expect("queue head exists");
    assert_eq!(head.seq, 2, "lowest unsettled turn dispatches first");
    assert!(
        kernel.session_has_pending_run(session).await.unwrap(),
        "queued follow-ups keep the Session busy"
    );
}

#[tokio::test]
async fn cancelled_queue_head_wakes_successor() {
    let database_url = std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database");
    let kernel = Arc::new(
        Kernel::connect(&Config::database_url(database_url))
            .await
            .expect("PostgreSQL connection succeeds"),
    );
    kernel.migrate().await.expect("fresh migration succeeds");
    let (owner, _project, session, _agent, _workspace) = conversation_fixture(&kernel).await;

    let head = kernel
        .accept_run(
            Uuid::new_v4(),
            Uuid::new_v4(),
            session,
            &[1u8; 32],
            "resume",
            "queued head",
            Some(owner),
        )
        .await
        .expect("queued head accepts");
    let successor = kernel
        .accept_run(
            Uuid::new_v4(),
            Uuid::new_v4(),
            session,
            &[2u8; 32],
            "resume",
            "successor",
            Some(owner),
        )
        .await
        .expect("successor accepts");
    assert_eq!(head.seq, 2);
    assert_eq!(successor.seq, 3);

    // The successor is not dispatchable while the head is queued.
    let eligible = kernel
        .next_dispatchable_run_for_session(session)
        .await
        .expect("eligibility reads");
    assert_eq!(eligible.as_ref().map(|run| run.id), Some(head.id));

    // Cancelling the queued head reports its Session so the caller can wake
    // the queue, and the successor becomes dispatchable immediately.
    let (state, kicked) = kernel
        .cancel_run(head.id)
        .await
        .expect("queued head cancels");
    assert_eq!(state, RunState::Cancelled);
    assert_eq!(kicked, Some(session), "cancel reports the Session to wake");
    let eligible = kernel
        .next_dispatchable_run_for_session(session)
        .await
        .expect("eligibility after cancel reads");
    assert_eq!(
        eligible.as_ref().map(|run| run.id),
        Some(successor.id),
        "cancelled head never strands the queue"
    );

    // A dispatched Run is never silently cancelled: it stays dispatched and
    // reports no Session to wake.
    let dispatched = kernel
        .accept_run(
            Uuid::new_v4(),
            Uuid::new_v4(),
            session,
            &[3u8; 32],
            "resume",
            "in flight",
            Some(owner),
        )
        .await
        .expect("in-flight Run accepts");
    kernel
        .dispatch_run(dispatched.id)
        .await
        .expect("Run dispatches");
    let (state, kicked) = kernel
        .cancel_run(dispatched.id)
        .await
        .expect("in-flight cancel request records");
    assert_eq!(state, RunState::Dispatched);
    assert_eq!(kicked, None, "in-flight cancel never wakes the queue");
    let row = sqlx::query("select state from runs where id = $1")
        .bind(dispatched.id)
        .fetch_one(kernel.pool())
        .await
        .expect("Run state reads");
    assert_eq!(
        row.get::<String, _>("state"),
        "dispatched",
        "in-flight effect is never hidden as cancelled"
    );
}

#[tokio::test]
async fn idempotent_replay_returns_existing_run() {
    let database_url = std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database");
    let kernel = Arc::new(
        Kernel::connect(&Config::database_url(database_url))
            .await
            .expect("PostgreSQL connection succeeds"),
    );
    kernel.migrate().await.expect("fresh migration succeeds");
    let (owner, _project, session, _agent, _workspace) = conversation_fixture(&kernel).await;

    let run_id = Uuid::new_v4();
    let intent_id = Uuid::new_v4();
    let hash = [7u8; 32];
    let first = kernel
        .accept_run(
            run_id,
            intent_id,
            session,
            &hash,
            "resume",
            "same message",
            Some(owner),
        )
        .await
        .expect("first accept succeeds");
    let replay = kernel
        .accept_run(
            run_id,
            intent_id,
            session,
            &hash,
            "resume",
            "same message",
            Some(owner),
        )
        .await
        .expect("idempotent replay returns the existing Run");
    assert_eq!(replay, first, "replay never starts a second activation");
    let count: i64 = sqlx::query_scalar("select count(*) from runs where session_id = $1")
        .bind(session)
        .fetch_one(kernel.pool())
        .await
        .expect("Run count reads");
    assert_eq!(count, 2, "first message plus one follow-up, no duplicates");

    // Same intent, different prompt: a conflict, never a silent overwrite.
    let conflict = kernel
        .accept_run(
            Uuid::new_v4(),
            intent_id,
            session,
            &[8u8; 32],
            "resume",
            "different message",
            Some(owner),
        )
        .await;
    assert!(matches!(conflict, Err(KernelError::Conflict)));
}

#[tokio::test]
async fn follow_up_accept_does_not_wait_for_session_writer() {
    let database_url = std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database");
    let kernel = Kernel::connect(&Config::database_url(database_url))
        .await
        .expect("PostgreSQL connection succeeds");
    kernel.migrate().await.expect("fresh migration succeeds");
    let (owner, _project, session, _agent, _workspace) = conversation_fixture(&kernel).await;

    let store = SessionStore::new(
        kernel.pool().clone(),
        BlobStore::new(
            "unused".into(),
            UNUSED_BLOB_KEY,
            "unused".into(),
            "https://example.invalid".into(),
        )
        .expect("blob store type constructs"),
    );
    // Hold the live activation writer fence. A follow-up must still accept
    // immediately and remain queued; sharing this lock would stall the HTTP
    // admission path until the in-flight turn finished appending events.
    let writer = store
        .writer(session)
        .await
        .expect("session writer pins for the in-flight turn");
    let started = Instant::now();
    let follow = kernel
        .accept_run(
            Uuid::new_v4(),
            Uuid::new_v4(),
            session,
            &[9u8; 32],
            "resume",
            "queued while writer is held",
            Some(owner),
        )
        .await
        .expect("follow-up accepts while the session writer is held");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "follow-up accept waited on the session writer fence ({elapsed:?})"
    );
    assert_eq!(follow.seq, 2, "follow-up is the next turn ordinal");
    assert_eq!(follow.state, RunState::Accepted);
    drop(writer);
}
