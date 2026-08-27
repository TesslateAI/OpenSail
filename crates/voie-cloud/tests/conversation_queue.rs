//! Conversation API and durable queue contract.
//!
//! Kernel-level contract of the product conversation API: the first message
//! atomically creates the Session and its first accepted Run, a replay of
//! the same intent returns the existing pair, conflicting intents are
//! refused, follow-ups queue per-Session with durable turn ordinals, and a
//! queued Run dispatches only after its predecessor settles (terminal or
//! cancelled). HTTP-level contracts run against the native-auth surface:
//! viewer/foreign/disabled actors are refused and actor attribution is
//! preserved in the durable rows and audit trail.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;
use voie_cloud::auth::{Auth, AuthConfig};
use voie_cloud::web_session;
use voie_cloud::{Config, Kernel, KernelError, RunState, serve_with_services};

// ------------------------------------------------------------ kernel fixtures

async fn fresh_kernel() -> Arc<Kernel> {
    let database_url = std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database");
    let kernel = Kernel::connect(&Config::database_url(database_url))
        .await
        .expect("PostgreSQL connection succeeds");
    kernel.migrate().await.expect("migration succeeds");
    Arc::new(kernel)
}

async fn insert_user(kernel: &Kernel, id: Uuid) {
    // Migration 0007 keeps the unique (issuer, subject) constraint while
    // making both columns nullable and provider-independent, so fixture
    // users never share a legacy pair: each gets a fresh random issuer and
    // its own id as subject.
    let issuer = Uuid::new_v4().to_string();
    let subject = id.to_string();
    sqlx::query("insert into users (id, issuer, subject) values ($1, $2, $3)")
        .bind(id)
        .bind(issuer)
        .bind(subject)
        .execute(kernel.pool())
        .await
        .expect("test user inserts");
}

/// One Project with a ready Workspace and an Agent owned by one User.
struct ConversationSeed {
    owner: Uuid,
    project_id: Uuid,
    agent_id: Uuid,
    workspace_id: Uuid,
}

async fn seed_project(kernel: &Kernel) -> ConversationSeed {
    let owner = Uuid::new_v4();
    insert_user(kernel, owner).await;
    let project_id = Uuid::new_v4();
    kernel
        .create_project(
            project_id,
            owner,
            &format!("conversation-project-{owner}"),
            "personal",
        )
        .await
        .expect("Project creates");
    let fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .expect("test Fabric inserts");
    let workspace_id = Uuid::new_v4();
    sqlx::query(
        "insert into workspaces (id, project_id, fabric_id, state, created_by_user_id) \
         values ($1, $2, $3, 'ready', $4)",
    )
    .bind(workspace_id)
    .bind(project_id)
    .bind(fabric)
    .bind(owner)
    .execute(kernel.pool())
    .await
    .expect("test Workspace inserts");
    let agent_id = Uuid::new_v4();
    sqlx::query("insert into agents (id, project_id, name) values ($1, $2, $3)")
        .bind(agent_id)
        .bind(project_id)
        .bind(format!("agent-{agent_id}"))
        .execute(kernel.pool())
        .await
        .expect("test Agent inserts");
    ConversationSeed {
        owner,
        project_id,
        agent_id,
        workspace_id,
    }
}

async fn run_count(kernel: &Kernel, session_id: Uuid) -> i64 {
    sqlx::query_scalar("select count(*) from runs where session_id = $1")
        .bind(session_id)
        .fetch_one(kernel.pool())
        .await
        .expect("Run count reads")
}

// --------------------------------------------------------------- kernel tests

#[tokio::test]
async fn first_message_creates_session_and_accepted_run_atomically() {
    let kernel = fresh_kernel().await;
    let seed = seed_project(&kernel).await;

    let session_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();
    let intent_id = Uuid::new_v4();
    let hash = [1u8; 32];
    let (session, run) = kernel
        .create_conversation(
            session_id,
            seed.project_id,
            seed.agent_id,
            seed.workspace_id,
            run_id,
            intent_id,
            &hash,
            "first message",
            seed.owner,
        )
        .await
        .expect("conversation creates atomically");

    assert_eq!(session.id, session_id);
    assert_eq!(session.project_id, seed.project_id);
    assert_eq!(session.agent_id, seed.agent_id);
    assert_eq!(session.workspace_id, seed.workspace_id);
    assert_eq!(
        session.last_actor_user_id,
        Some(seed.owner),
        "the creating human is the Session's first actor"
    );
    assert_eq!(run.id, run_id);
    assert_eq!(run.intent_id, intent_id);
    assert_eq!(run.session_id, session_id);
    assert_eq!(run.request_hash, hash.to_vec());
    assert_eq!(run.mode, "create", "the first message is always a create");
    assert_eq!(run.prompt, "first message");
    assert_eq!(run.state, RunState::Accepted);
    assert_eq!(run.actor_user_id, Some(seed.owner));
    assert_eq!(run.seq, 1, "the first Run owns turn ordinal 1");

    // A conversation never exists without its first message: one durable
    // Session row and exactly one accepted Run.
    assert_eq!(
        kernel.find_session(session_id).await.unwrap(),
        Some(session),
        "the Session row exists with the first Run"
    );
    assert_eq!(run_count(&kernel, session_id).await, 1);
    let persisted = kernel.list_runs(session_id).await.unwrap();
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].id, run_id);

    // A missing Agent or a Workspace outside the Project is refused with no
    // partial pair surviving.
    let other_seed = seed_project(&kernel).await;
    let refused_workspace = kernel
        .create_conversation(
            Uuid::new_v4(),
            seed.project_id,
            seed.agent_id,
            other_seed.workspace_id,
            Uuid::new_v4(),
            Uuid::new_v4(),
            &[9u8; 32],
            "x",
            seed.owner,
        )
        .await;
    assert!(matches!(
        refused_workspace,
        Err(KernelError::RelationRefused)
    ));
    let refused_agent = kernel
        .create_conversation(
            Uuid::new_v4(),
            seed.project_id,
            other_seed.agent_id,
            seed.workspace_id,
            Uuid::new_v4(),
            Uuid::new_v4(),
            &[9u8; 32],
            "x",
            seed.owner,
        )
        .await;
    assert!(matches!(refused_agent, Err(KernelError::RelationRefused)));

    // Only a `ready` Workspace binds to a conversation.
    let creating_fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(creating_fabric)
        .bind(format!("fabric-{creating_fabric}"))
        .execute(kernel.pool())
        .await
        .expect("creating Fabric inserts");
    let creating_workspace = Uuid::new_v4();
    sqlx::query(
        "insert into workspaces (id, project_id, fabric_id, state, created_by_user_id) \
         values ($1, $2, $3, 'creating', $4)",
    )
    .bind(creating_workspace)
    .bind(seed.project_id)
    .bind(creating_fabric)
    .bind(seed.owner)
    .execute(kernel.pool())
    .await
    .expect("creating Workspace inserts");
    let refused_creating = kernel
        .create_conversation(
            Uuid::new_v4(),
            seed.project_id,
            seed.agent_id,
            creating_workspace,
            Uuid::new_v4(),
            Uuid::new_v4(),
            &[9u8; 32],
            "x",
            seed.owner,
        )
        .await;
    assert!(matches!(
        refused_creating,
        Err(KernelError::RelationRefused)
    ));
}

#[tokio::test]
async fn same_intent_replay_returns_existing_pair() {
    let kernel = fresh_kernel().await;
    let seed = seed_project(&kernel).await;

    let session_id = Uuid::new_v4();
    let intent_id = Uuid::new_v4();
    let hash = [2u8; 32];
    let (first_session, first_run) = kernel
        .create_conversation(
            session_id,
            seed.project_id,
            seed.agent_id,
            seed.workspace_id,
            Uuid::new_v4(),
            intent_id,
            &hash,
            "hello",
            seed.owner,
        )
        .await
        .expect("conversation creates");
    let (replay_session, replay_run) = kernel
        .create_conversation(
            session_id,
            seed.project_id,
            seed.agent_id,
            seed.workspace_id,
            Uuid::new_v4(),
            intent_id,
            &hash,
            "hello",
            seed.owner,
        )
        .await
        .expect("same-intent replay returns the existing pair");
    assert_eq!(
        replay_session, first_session,
        "replay returns the same Session"
    );
    assert_eq!(
        replay_run, first_run,
        "replay never starts a second activation"
    );
    assert_eq!(run_count(&kernel, session_id).await, 1);

    // Same intent, different prompt: a conflict, never a silent overwrite.
    let changed_prompt = kernel
        .create_conversation(
            session_id,
            seed.project_id,
            seed.agent_id,
            seed.workspace_id,
            Uuid::new_v4(),
            intent_id,
            &[3u8; 32],
            "different",
            seed.owner,
        )
        .await;
    assert!(matches!(changed_prompt, Err(KernelError::Conflict)));

    // Same intent bound to a different conversation: a conflict.
    let other_session = Uuid::new_v4();
    let other_conversation = kernel
        .create_conversation(
            other_session,
            seed.project_id,
            seed.agent_id,
            seed.workspace_id,
            Uuid::new_v4(),
            intent_id,
            &hash,
            "hello",
            seed.owner,
        )
        .await;
    assert!(matches!(other_conversation, Err(KernelError::Conflict)));

    // A repeated conversation identity with a fresh intent is a conflict and
    // leaves no partial pair behind.
    let fresh_intent = kernel
        .create_conversation(
            session_id,
            seed.project_id,
            seed.agent_id,
            seed.workspace_id,
            Uuid::new_v4(),
            Uuid::new_v4(),
            &[4u8; 32],
            "hello again",
            seed.owner,
        )
        .await;
    assert!(matches!(fresh_intent, Err(KernelError::Conflict)));
    assert_eq!(run_count(&kernel, session_id).await, 1);
    assert_eq!(
        kernel.find_session(session_id).await.unwrap().map(|s| s.id),
        Some(session_id),
        "the original pair survives every conflicting attempt"
    );
}

#[tokio::test]
async fn follow_ups_sequence_per_session_and_dispatch_in_order() {
    let kernel = fresh_kernel().await;
    let seed = seed_project(&kernel).await;

    let session_id = Uuid::new_v4();
    let (_, first) = kernel
        .create_conversation(
            session_id,
            seed.project_id,
            seed.agent_id,
            seed.workspace_id,
            Uuid::new_v4(),
            Uuid::new_v4(),
            &[0u8; 32],
            "first",
            seed.owner,
        )
        .await
        .expect("conversation creates");
    assert_eq!(first.seq, 1);

    // Sequential follow-ups earn distinct durable turn ordinals.
    let mut follow_ids = Vec::new();
    for i in 0..3 {
        let run = kernel
            .accept_run(
                Uuid::new_v4(),
                Uuid::new_v4(),
                session_id,
                &[10 + i as u8; 32],
                "resume",
                &format!("follow-up {i}"),
                Some(seed.owner),
            )
            .await
            .expect("follow-up accepts");
        assert_eq!(
            run.seq,
            2 + i as i64,
            "follow-ups queue in acceptance order"
        );
        assert_eq!(run.mode, "resume", "follow-ups are always resume");
        follow_ids.push(run.id);
    }

    // The initial accepted head is eligible for the supervisor. Every
    // follow-up remains blocked behind it until it settles.
    let initial = kernel
        .next_dispatchable_run_for_session(session_id)
        .await
        .unwrap()
        .expect("initial head is dispatchable");
    assert_eq!(initial.id, first.id);
    assert!(kernel.session_has_pending_run(session_id).await.unwrap());

    // The first Run settles terminal; only the lowest queued turn becomes
    // eligible.
    kernel
        .dispatch_run(first.id)
        .await
        .expect("first Run dispatches");
    assert!(
        kernel
            .next_dispatchable_run_for_session(session_id)
            .await
            .unwrap()
            .is_none(),
        "a dispatched predecessor still blocks the queue"
    );
    kernel
        .complete_run(first.id, r#"{"accepted":true}"#)
        .await
        .expect("first Run completes");
    let next = kernel
        .next_dispatchable_run_for_session(session_id)
        .await
        .unwrap()
        .expect("queue head exists");
    assert_eq!(next.id, follow_ids[0]);
    assert_eq!(next.seq, 2);

    // A dispatched successor blocks its own tail until it settles terminal.
    kernel
        .dispatch_run(follow_ids[0])
        .await
        .expect("queue head dispatches");
    assert!(
        kernel
            .next_dispatchable_run_for_session(session_id)
            .await
            .unwrap()
            .is_none(),
        "a Session never runs two activations concurrently"
    );
    kernel
        .complete_run(follow_ids[0], r#"{"accepted":true}"#)
        .await
        .expect("queue head completes");
    let next = kernel
        .next_dispatchable_run_for_session(session_id)
        .await
        .unwrap()
        .expect("next queued run exists");
    assert_eq!(next.id, follow_ids[1]);
    assert_eq!(next.seq, 3);
}

#[tokio::test]
async fn queued_run_dispatches_after_predecessor_cancelled_or_terminal() {
    let kernel = fresh_kernel().await;
    let seed = seed_project(&kernel).await;

    let session_id = Uuid::new_v4();
    let (_, first) = kernel
        .create_conversation(
            session_id,
            seed.project_id,
            seed.agent_id,
            seed.workspace_id,
            Uuid::new_v4(),
            Uuid::new_v4(),
            &[0u8; 32],
            "first",
            seed.owner,
        )
        .await
        .expect("conversation creates");
    kernel
        .dispatch_run(first.id)
        .await
        .expect("first Run dispatches");
    kernel
        .complete_run(first.id, r#"{"accepted":true}"#)
        .await
        .expect("first Run completes");

    let head = kernel
        .accept_run(
            Uuid::new_v4(),
            Uuid::new_v4(),
            session_id,
            &[1u8; 32],
            "resume",
            "queued head",
            Some(seed.owner),
        )
        .await
        .expect("head accepts");
    let successor = kernel
        .accept_run(
            Uuid::new_v4(),
            Uuid::new_v4(),
            session_id,
            &[2u8; 32],
            "resume",
            "successor",
            Some(seed.owner),
        )
        .await
        .expect("successor accepts");
    let tail = kernel
        .accept_run(
            Uuid::new_v4(),
            Uuid::new_v4(),
            session_id,
            &[3u8; 32],
            "resume",
            "tail",
            Some(seed.owner),
        )
        .await
        .expect("tail accepts");
    assert_eq!((head.seq, successor.seq, tail.seq), (2, 3, 4));

    // A cancelled predecessor wakes its successor.
    let eligible = kernel
        .next_dispatchable_run_for_session(session_id)
        .await
        .unwrap()
        .expect("queue head exists");
    assert_eq!(eligible.id, head.id);
    let (state, kicked) = kernel
        .cancel_run(head.id)
        .await
        .expect("queued head cancels");
    assert_eq!(state, RunState::Cancelled);
    assert_eq!(
        kicked,
        Some(session_id),
        "cancel reports the Session to wake"
    );
    let eligible = kernel
        .next_dispatchable_run_for_session(session_id)
        .await
        .unwrap()
        .expect("successor becomes eligible");
    assert_eq!(
        eligible.id, successor.id,
        "a cancelled predecessor never strands the queue"
    );

    // A terminal predecessor releases the tail.
    kernel
        .dispatch_run(successor.id)
        .await
        .expect("successor dispatches");
    assert!(
        kernel
            .next_dispatchable_run_for_session(session_id)
            .await
            .unwrap()
            .is_none(),
        "a dispatched successor still blocks the tail"
    );
    kernel
        .complete_run(successor.id, r#"{"accepted":true}"#)
        .await
        .expect("successor completes");
    let eligible = kernel
        .next_dispatchable_run_for_session(session_id)
        .await
        .unwrap()
        .expect("tail becomes eligible");
    assert_eq!(eligible.id, tail.id, "queued Run dispatches after terminal");
    kernel.dispatch_run(tail.id).await.expect("tail dispatches");
    kernel
        .complete_run(tail.id, r#"{"accepted":true}"#)
        .await
        .expect("tail completes");
    assert!(
        kernel
            .next_dispatchable_run_for_session(session_id)
            .await
            .unwrap()
            .is_none(),
        "a settled queue has no dispatchable Run"
    );
    assert!(
        !kernel.session_has_pending_run(session_id).await.unwrap(),
        "a settled Session has no pending Run"
    );
}

#[tokio::test]
async fn actor_attribution_preserved() {
    let kernel = fresh_kernel().await;
    let seed = seed_project(&kernel).await;
    let member = Uuid::new_v4();
    insert_user(&kernel, member).await;
    sqlx::query(
        "insert into project_members (project_id, user_id, role) values ($1, $2, 'member')",
    )
    .bind(seed.project_id)
    .bind(member)
    .execute(kernel.pool())
    .await
    .expect("member membership inserts");

    let session_id = Uuid::new_v4();
    let (session, first) = kernel
        .create_conversation(
            session_id,
            seed.project_id,
            seed.agent_id,
            seed.workspace_id,
            Uuid::new_v4(),
            Uuid::new_v4(),
            &[5u8; 32],
            "first",
            seed.owner,
        )
        .await
        .expect("conversation creates");
    assert_eq!(first.actor_user_id, Some(seed.owner));
    assert_eq!(session.last_actor_user_id, Some(seed.owner));

    let follow = kernel
        .accept_run(
            Uuid::new_v4(),
            Uuid::new_v4(),
            session_id,
            &[6u8; 32],
            "resume",
            "from member",
            Some(member),
        )
        .await
        .expect("member follow-up accepts");
    assert_eq!(follow.actor_user_id, Some(member));
    let refreshed = kernel
        .find_session(session_id)
        .await
        .unwrap()
        .expect("Session exists");
    assert_eq!(
        refreshed.last_actor_user_id,
        Some(member),
        "Session attribution tracks the last queuer"
    );

    // Every Run keeps its own queuer even after another actor joins.
    let runs = kernel.list_runs(session_id).await.unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].actor_user_id, Some(seed.owner));
    assert_eq!(runs[1].actor_user_id, Some(member));
}

// ---------------------------------------------------------------- HTTP surface

const BLOB_ACCOUNT: &str = "voie-conversation-test";
const BLOB_CONTAINER: &str = "voie-test-container";
/// Arbitrary 32-byte key material; only shape matters to the local test.
const BLOB_KEY_BASE64: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

// Throwaway mTLS material for the Fabric client; no remote estate is touched.
const FABRIC_CA_PEM: &str = r#"-----BEGIN CERTIFICATE-----
MIIDDzCCAfegAwIBAgIUDMW0llgfZTo1gBKwAJmeG7lTfKMwDQYJKoZIhvcNAQEL
BQAwFzEVMBMGA1UEAwwMdm9pZS10ZXN0LWNhMB4XDTI2MDgyNjIzMzkxNVoXDTM2
MDgyMzIzMzkxNVowFzEVMBMGA1UEAwwMdm9pZS10ZXN0LWNhMIIBIjANBgkqhkiG
9w0BAQEFAAOCAQ8AMIIBCgKCAQEArErw3ChJaIf42VV3fC0/iBxzXroAzI19uUZ8
TYv1dL28riEyHj0824jEeehC10DFkrTr7wKJxqgAC4C7txtYL1zhwJpWLXt31eEC
UoIRzH+g6i4WWlEyL5QPr7+tapF068nmYsc4BRpk0gAglMn0UegOePmyIuOutnSo
SQ3ftojib02nDGv78mSRBcdr+WxWltNFpEbtpayjU28OYfPFuNlZQWiQt153Ywyy
66FgYTMMc/qf0jcmdnWGKDQluVCg2DX5LZdR2MXInCCoQ25BAaR1xFzJH9p9Wopc
Xprx5UDGUgl7D9/LJpBTwYwiN+gTxvklkTMzJ6pUuvqnh1wxnwIDAQABo1MwUTAd
BgNVHQ4EFgQUF3o3IA96ojZ5QJAqlhY+pgEJ8VMwHwYDVR0jBBgwFoAUF3o3IA96
ojZ5QJAqlhY+pgEJ8VMwDwYDVR0TAQH/BAUwAwEB/zANBgkqhkiG9w0BAQsFAAOC
AQEAD3AUiKercsgcGX++msfvi7lgFFnN6oWjwB8CyKB9R1Cc1Y/A4n10A+GVdbPu
pVLuUwMsItJXU1pp7utDMPh9otIkX3EKNy1/nmL/pe+URbasqX8onWKXu0K4/hwy
7nslcymW9GkqzFeNq0vzL5wa+NVzc2lQ9AnRHuV191aFXNTmqIlv7WfqvRMRPb98
PPxv7FIGw5IJcGAqxMSN+KzTXrQxYqx9YZxMaBtD+KriK7F36vtF5tlsQKDbFb8w
zU5tSTQ8Thk2mANl7kz9iAD0C0WR3k9suBmGZizJccutt3fNw+tE7bl5aHD9ghQi
vom7FvjID5tP95NXjSppYlUu2w==
-----END CERTIFICATE-----
"#;
const FABRIC_CLIENT_CERT_PEM: &str = r#"-----BEGIN CERTIFICATE-----
MIIDAjCCAeqgAwIBAgIUSdCkqDA8lZlkVy5V8bzIhSGvmFYwDQYJKoZIhvcNAQEL
BQAwFzEVMBMGA1UEAwwMdm9pZS10ZXN0LWNhMB4XDTI2MDgyNjIzMzkxNVoXDTM2
MDgyMzIzMzkxNVowGzEZMBcGA1UEAwwQdm9pZS10ZXN0LWNsaWVudDCCASIwDQYJ
KoZIhvcNAQEBBQADggEPADCCAQoCggEBAKcSXsGa4Sma9gH8Phxlf02b2326iKpP
CQGFDlEyelP3a3nGhKYHxdUMIe1+5yuZUZ8lO0cx0FZYJEetAx5OeNHIsFj7UqMs
SK68NMPejLQZHUGsFdSkFBTbooGBaxWUzI1qvH6LsHes1Ct1HPpgW3C7+MZaYI7Z
4s3QU68ikdrS94yn/B91pDlbzGjhFQ2E8eu5RnubndHK5lZXbStAMxj6WDlb31Lf
R/To1Ar/VDmUatvVz8lYI4A42menvG0HHZXnNn+4g6hptY9dttwfKNPE1Nen7a4B
YQk0dxlCGafdC6DLs6pxGhIKy3sumuILB8kG5H1JfjWI3dpMY9jNHicCAwEAAaNC
MEAwHQYDVR0OBBYEFGju+5IKfbgsia5dBBMzuDvTFqY/MB8GA1UdIwQYMBaAFBd6
NyAPeqI2eUCQKpYWPqYBCfFTMA0GCSqGSIb3DQEBCwUAA4IBAQCdngBNLaCGCXLq
7uwqtKC48AyoSqjdj/ION0KsP60oxN3TKJkRXEk0iN2eMlt54LlFurXvPszyT3KU
ZQ13y7OYJBr6LPYqLjPr23apiTpvJZMjuL06czHE6PLSmtRIMLS8y1qv/MbwWfp9
A1pAzSCK1XcZQkWFHE3k/c4RM4Y8tq4H7KUBvrH3nDlNIOZEIbyYKBnqBq64jIl6
HndzUHWJcoFq3ZQz3kntHOoA1m6qC9PtC82idbp/OizllegCP+VUZKe5h+nUJttu
KnKjLpoIsRJ8KiMPc87/yIWGOgz+Fzuh9SHHKamnzoiJE5HVK8zdcjNDX7UejE8O
W5LCSJ8M
-----END CERTIFICATE-----
"#;
const FABRIC_CLIENT_KEY_PEM: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQCnEl7BmuEpmvYB
/D4cZX9Nm9t9uoiqTwkBhQ5RMnpT92t5xoSmB8XVDCHtfucrmVGfJTtHMdBWWCRH
rQMeTnjRyLBY+1KjLEiuvDTD3oy0GR1BrBXUpBQU26KBgWsVlMyNarx+i7B3rNQr
dRz6YFtwu/jGWmCO2eLN0FOvIpHa0veMp/wfdaQ5W8xo4RUNhPHruUZ7m53RyuZW
V20rQDMY+lg5W99S30f06NQK/1Q5lGrb1c/JWCOAONpnp7xtBx2V5zZ/uIOoabWP
XbbcHyjTxNTXp+2uAWEJNHcZQhmn3Qugy7OqcRoSCst7LpriCwfJBuR9SX41iN3a
TGPYzR4nAgMBAAECggEAQGHE4Ci2EhlkKdmxebHnP9oK2EWUusSgPNSwcrvYBhl3
ckL9BRpDs1jsjh/0J4n6uTBYypO4rD1lJbXXWMt2pakHxBJ9guHi1Gs0jjJp2FFB
Q/hzpTDhiDQnSG69/GAN/4UdRErCYyvXyzNjSlztf+D/+jgDs6jlTNi2FuxkdoVr
2jS3AcqlirA/Ar2KGLLZO3oyYcADXm9X0DTuJZwEXu2zdMlxYuDwniJwhH1Qt6Kc
ukucSMcFMb09oEruYwy7vTMKZDeLpmzz83p30U62eM1/ERVSpb/n8h2OF6Ge3pmi
7CGl3sFCxv0f9cKyePa7NkoQrisJAnwRyvX3ziEhOQKBgQDh5gY3g1ecvPY9fpFl
kPTxt/23vJzKGmgmeex8x46s4C1GLmP3ILcqDlz24kvM4l74CC1Q6/ULacE6ji/n
OY2zCcUQHU+S5O/4xdtiocPCsc06b5SwBndTkHJE/nhEFTG4nbF1sXc3iyLUezHm
JvWPvjimxQEuHm99SgkBjV5FKwKBgQC9VZ1JFHud/akuv9qEowBGSIS66WxaEpFD
ikNctCf7G45kVkY15l9D2ez54wi5iaKTWJDhrCkfhTSOkkC2lB2PdzGC0+E2cWIY
ViuodI5ZB+00TgUnV9HDFs2wQ1FGR5UPGWWy703xPigcHHl3CF+YnGUYKU77HrNm
bqxFEeDE9QKBgEtwQ9c6F4ISYLE8mVWvyP0IEsTPShT8KJfg06cABZeZ7cSoLV4U
INb8oPMZs3KijlCKeoexpM3A7XSek0TGpZmKw7KT90T5C2KqwI75sqRMOFsxdBgs
sKDJdj+wM32ZDle24dKKB2QXJPSMh6dyj0MHpWecFr7ODzFqDgPkr/ytAoGBAIyV
Q4J9+QPo03Ro9EJEHfIR6qw2okOHQeFaioYNJxqm7WXHQb7H3bit2e36DAJoFhU+
T+WhRa+n4sxyACcRd5mNMXApDzKzodjcMvKUCRZGcnTB8cWyyYgIKJZWhcSfZiid
/QuN8NvOAU5OPkqKJyFUDySPl5uSwjauuq9WhQT5AoGBANK0zdLpTP0berN/8t+c
/vtVi+HPWn9EWuljN6CjAso/KzSPQVJPrdx+RFm4J7RnYwR/9sVuUUfb7gHjo+o1
C6CknfkeWPeQABVlQoIZThKPDN73gcu9sMm+5wlNyoiMXGGQLrwtk7+BbKIDp9vd
ENndDEaiQT/zd+roA+xn4E44
-----END PRIVATE KEY-----
"#;

/// Removes its directory on drop so repeated runs leave nothing behind.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn set_env(name: &str, value: &str) {
    // Process-global configuration consumed by Services::from_env in this
    // test process only; set before any reader is spawned.
    unsafe { std::env::set_var(name, value) };
}

struct Surface {
    kernel: Arc<Kernel>,
    auth: Arc<Auth>,
    port: u16,
    origin: String,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
    _pems: TempDir,
}

/// Full HTTP surface on one listener with the native-auth mode: no OIDC
/// discovery, no live dependencies. Accepted Runs settle to `unknown`
/// through the supervisor because no node activation child is provisioned;
/// the route responses and durable rows are unaffected.
async fn http_surface() -> Surface {
    let kernel = fresh_kernel().await;
    let dir = std::env::temp_dir().join(format!("voie-conversation-test-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("temp dir creates");
    let ca = dir.join("ca.pem");
    let cert = dir.join("client.pem");
    let key = dir.join("client.key");
    std::fs::write(&ca, FABRIC_CA_PEM).expect("CA pem writes");
    std::fs::write(&cert, FABRIC_CLIENT_CERT_PEM).expect("client pem writes");
    std::fs::write(&key, FABRIC_CLIENT_KEY_PEM).expect("client key writes");
    set_env("VOIE_AZURE_BLOB_ACCOUNT", BLOB_ACCOUNT);
    set_env("VOIE_AZURE_BLOB_KEY", BLOB_KEY_BASE64);
    set_env("VOIE_AZURE_BLOB_CONTAINER", BLOB_CONTAINER);
    set_env("VOIE_AZURE_BLOB_ENDPOINT", "http://127.0.0.1:9");
    set_env("VOIE_MODEL_BASE_URL", "https://127.0.0.1:9/v1");
    set_env("VOIE_MODEL_NAME", "test-model");
    set_env("VOIE_MODEL_API_KEY", "test-key");
    set_env("VOIE_FABRIC_ENDPOINT", "https://127.0.0.1:9/");
    set_env(
        "VOIE_FABRIC_CLIENT_CERT_PATH",
        cert.to_str().expect("cert path"),
    );
    set_env(
        "VOIE_FABRIC_CLIENT_KEY_PATH",
        key.to_str().expect("key path"),
    );
    set_env("VOIE_FABRIC_CA_CERT_PATH", ca.to_str().expect("ca path"));

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener binds");
    let port = listener.local_addr().expect("listener address").port();
    let origin = format!("http://127.0.0.1:{port}");
    let auth = Arc::new(
        Auth::connect(AuthConfig::native(origin.clone()), kernel.pool().clone())
            .await
            .expect("native auth builds without discovery"),
    );
    let services = voie_cloud::integration::Services::from_env(kernel.pool().clone())
        .expect("local service configuration resolves");
    let server = tokio::spawn(serve_with_services(
        listener,
        kernel.clone(),
        auth.clone(),
        services,
    ));
    Surface {
        kernel,
        auth,
        port,
        origin,
        server,
        _pems: TempDir(dir),
    }
}

/// Mints a server-side Web session for one User and returns the opaque
/// cookie secret, exactly like the login boot.
async fn mint_session(kernel: &Kernel, auth: &Auth, user_id: Uuid) -> String {
    let (_session, token) =
        web_session::create(kernel.pool(), user_id, auth.config().session_ttl())
            .await
            .expect("web session mints");
    token
}

fn cookie_for(token: &str) -> String {
    format!("voie_session={token}")
}

struct Exchange {
    status: u16,
    body: Vec<u8>,
}

impl Exchange {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("response body is JSON")
    }
}

fn request_text(
    method: &str,
    path: &str,
    port: u16,
    headers: &[(&str, String)],
    body: Option<&[u8]>,
) -> String {
    let mut text = format!("{method} {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\n");
    for (key, value) in headers {
        text.push_str(&format!("{key}: {value}\r\n"));
    }
    text.push_str("connection: close\r\n\r\n");
    if let Some(body) = body {
        text.push_str(&String::from_utf8_lossy(body));
    }
    text
}

async fn exchange(port: u16, request: &str) -> Exchange {
    tokio::time::timeout(Duration::from_secs(10), raw_exchange(port, request))
        .await
        .expect("HTTP exchange completes inside 10s")
}

async fn raw_exchange(port: u16, request: &str) -> Exchange {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("listener accepts connections");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("request writes");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("response reads");
    let text = String::from_utf8_lossy(&response);
    let (head, body) = text.split_once("\r\n\r\n").expect("header terminator");
    let status = head
        .lines()
        .next()
        .expect("status line")
        .split_whitespace()
        .nth(1)
        .expect("status code")
        .parse()
        .expect("numeric status");
    Exchange {
        status,
        body: body.as_bytes().to_vec(),
    }
}

async fn post_json(port: u16, path: &str, cookie: &str, origin: &str, body: Value) -> Exchange {
    let bytes = body.to_string().into_bytes();
    let headers: Vec<(&str, String)> = vec![
        ("cookie", cookie.to_string()),
        (
            "content-type",
            "application/json; charset=utf-8".to_string(),
        ),
        ("content-length", bytes.len().to_string()),
        ("origin", origin.to_string()),
        ("x-voie-intent", "mutate".to_string()),
    ];
    exchange(
        port,
        &request_text("POST", path, port, &headers, Some(&bytes)),
    )
    .await
}

// ------------------------------------------------------------------ HTTP tests

#[tokio::test]
async fn http_conversation_create_replay_and_conflict_contract() {
    let surface = http_surface().await;
    let seed = seed_project(&surface.kernel).await;
    let cookie = cookie_for(&mint_session(&surface.kernel, &surface.auth, seed.owner).await);

    let conversation_id = Uuid::new_v4();
    let intent_id = Uuid::new_v4();
    let body = json!({
        "conversationId": conversation_id,
        "projectId": seed.project_id,
        "agentId": seed.agent_id,
        "workspaceId": seed.workspace_id,
        "intentId": intent_id,
        "prompt": "hello world",
    });

    // The first message atomically creates the conversation and its first
    // accepted Run.
    let created = post_json(
        surface.port,
        "/api/conversations",
        &cookie,
        &surface.origin,
        body.clone(),
    )
    .await;
    assert_eq!(created.status, 200);
    let created_json = created.json();
    assert_eq!(
        created_json.get("conversationId").and_then(Value::as_str),
        Some(conversation_id.to_string().as_str())
    );
    let first_run = created_json
        .get("runId")
        .and_then(Value::as_str)
        .expect("runId")
        .to_string();
    assert_eq!(
        created_json.get("intentId").and_then(Value::as_str),
        Some(intent_id.to_string().as_str())
    );
    assert_eq!(created_json.get("accepted"), Some(&json!(true)));
    assert_eq!(created_json.get("state"), Some(&json!("accepted")));
    assert_eq!(run_count(&surface.kernel, conversation_id).await, 1);

    // A replay of the same intent returns the same pair idempotently: the
    // same conversation identity and the same Run, never a second
    // activation.
    let replay = post_json(
        surface.port,
        "/api/conversations",
        &cookie,
        &surface.origin,
        body.clone(),
    )
    .await;
    assert_eq!(replay.status, 200);
    let replay_json = replay.json();
    assert_eq!(
        replay_json.get("conversationId").and_then(Value::as_str),
        Some(conversation_id.to_string().as_str())
    );
    assert_eq!(
        replay_json.get("runId").and_then(Value::as_str),
        Some(first_run.as_str()),
        "replay returns the existing Run identity"
    );
    assert_eq!(replay_json.get("accepted"), Some(&json!(false)));
    assert_eq!(run_count(&surface.kernel, conversation_id).await, 1);

    // Same conversation, same intent, different prompt: a conflict.
    let changed_prompt = json!({
        "conversationId": conversation_id,
        "projectId": seed.project_id,
        "agentId": seed.agent_id,
        "workspaceId": seed.workspace_id,
        "intentId": intent_id,
        "prompt": "different message",
    });
    let conflict = post_json(
        surface.port,
        "/api/conversations",
        &cookie,
        &surface.origin,
        changed_prompt,
    )
    .await;
    assert_eq!(conflict.status, 409, "conflicting intent is refused");

    // Same conversation, fresh intent: the repeated Session identity is a
    // conflict and nothing is duplicated.
    let fresh_intent = json!({
        "conversationId": conversation_id,
        "projectId": seed.project_id,
        "agentId": seed.agent_id,
        "workspaceId": seed.workspace_id,
        "intentId": Uuid::new_v4(),
        "prompt": "hello again",
    });
    let conflict = post_json(
        surface.port,
        "/api/conversations",
        &cookie,
        &surface.origin,
        fresh_intent,
    )
    .await;
    assert_eq!(
        conflict.status, 409,
        "repeated conversation identity conflicts"
    );
    assert_eq!(run_count(&surface.kernel, conversation_id).await, 1);

    // Follow-up messages queue durable Runs; replaying the same message
    // intent returns the same Run without duplicating the queue.
    let message_intent = Uuid::new_v4();
    let message_body = json!({ "intentId": message_intent, "prompt": "follow up" });
    let first_msg = post_json(
        surface.port,
        &format!("/api/conversations/{conversation_id}/messages"),
        &cookie,
        &surface.origin,
        message_body.clone(),
    )
    .await;
    assert_eq!(first_msg.status, 200);
    let message_run = first_msg
        .json()
        .get("runId")
        .and_then(Value::as_str)
        .expect("message runId")
        .to_string();
    let replay_msg = post_json(
        surface.port,
        &format!("/api/conversations/{conversation_id}/messages"),
        &cookie,
        &surface.origin,
        message_body.clone(),
    )
    .await;
    assert_eq!(replay_msg.status, 200);
    assert_eq!(
        replay_msg.json().get("runId").and_then(Value::as_str),
        Some(message_run.as_str()),
        "message replay returns the existing Run identity"
    );
    assert_eq!(
        run_count(&surface.kernel, conversation_id).await,
        2,
        "first message plus one follow-up, replay never duplicates"
    );

    // Same message intent, different prompt: a conflict.
    let changed_message = json!({ "intentId": message_intent, "prompt": "changed follow up" });
    let conflict = post_json(
        surface.port,
        &format!("/api/conversations/{conversation_id}/messages"),
        &cookie,
        &surface.origin,
        changed_message,
    )
    .await;
    assert_eq!(
        conflict.status, 409,
        "conflicting message intent is refused"
    );
    assert_eq!(run_count(&surface.kernel, conversation_id).await, 2);

    // An unknown conversation is not found.
    let missing = post_json(
        surface.port,
        &format!("/api/conversations/{}/messages", Uuid::new_v4()),
        &cookie,
        &surface.origin,
        json!({ "intentId": Uuid::new_v4(), "prompt": "hi" }),
    )
    .await;
    assert_eq!(missing.status, 404, "unknown conversation is not found");

    // A malformed payload is refused before any store work.
    let bad = post_json(
        surface.port,
        "/api/conversations",
        &cookie,
        &surface.origin,
        json!({ "conversationId": conversation_id }),
    )
    .await;
    assert_eq!(bad.status, 400);

    surface.server.abort();
    let _ = surface.server.await;
}

#[tokio::test]
async fn http_actor_refusals_and_attribution() {
    let surface = http_surface().await;
    let seed = seed_project(&surface.kernel).await;

    // One actor per project role plus one disabled and one foreign user.
    let member = Uuid::new_v4();
    insert_user(&surface.kernel, member).await;
    let viewer = Uuid::new_v4();
    insert_user(&surface.kernel, viewer).await;
    let foreign = Uuid::new_v4();
    insert_user(&surface.kernel, foreign).await;
    let disabled = Uuid::new_v4();
    sqlx::query("insert into users (id, status, platform_role) values ($1, 'disabled', 'user')")
        .bind(disabled)
        .execute(surface.kernel.pool())
        .await
        .expect("disabled user inserts");
    sqlx::query(
        "insert into project_members (project_id, user_id, role) values ($1, $2, 'member')",
    )
    .bind(seed.project_id)
    .bind(member)
    .execute(surface.kernel.pool())
    .await
    .expect("member membership inserts");
    sqlx::query(
        "insert into project_members (project_id, user_id, role) values ($1, $2, 'viewer')",
    )
    .bind(seed.project_id)
    .bind(viewer)
    .execute(surface.kernel.pool())
    .await
    .expect("viewer membership inserts");

    let owner_cookie = cookie_for(&mint_session(&surface.kernel, &surface.auth, seed.owner).await);
    let member_cookie = cookie_for(&mint_session(&surface.kernel, &surface.auth, member).await);
    let viewer_cookie = cookie_for(&mint_session(&surface.kernel, &surface.auth, viewer).await);
    let foreign_cookie = cookie_for(&mint_session(&surface.kernel, &surface.auth, foreign).await);
    let disabled_token = mint_session(&surface.kernel, &surface.auth, disabled).await;
    let disabled_cookie = cookie_for(&disabled_token);

    let conversation_id = Uuid::new_v4();
    let body = json!({
        "conversationId": conversation_id,
        "projectId": seed.project_id,
        "agentId": seed.agent_id,
        "workspaceId": seed.workspace_id,
        "intentId": Uuid::new_v4(),
        "prompt": "hello",
    });
    let created = post_json(
        surface.port,
        "/api/conversations",
        &owner_cookie,
        &surface.origin,
        body.clone(),
    )
    .await;
    assert_eq!(created.status, 200, "the owner operates the conversation");

    // A viewer can read the Project but never operate a Session.
    let viewer_create = post_json(
        surface.port,
        "/api/conversations",
        &viewer_cookie,
        &surface.origin,
        json!({
            "conversationId": Uuid::new_v4(),
            "projectId": seed.project_id,
            "agentId": seed.agent_id,
            "workspaceId": seed.workspace_id,
            "intentId": Uuid::new_v4(),
            "prompt": "viewer create",
        }),
    )
    .await;
    assert_eq!(viewer_create.status, 403, "viewer is refused on create");

    // A foreign user with no membership is refused.
    let foreign_create = post_json(
        surface.port,
        "/api/conversations",
        &foreign_cookie,
        &surface.origin,
        json!({
            "conversationId": Uuid::new_v4(),
            "projectId": seed.project_id,
            "agentId": seed.agent_id,
            "workspaceId": seed.workspace_id,
            "intentId": Uuid::new_v4(),
            "prompt": "foreign create",
        }),
    )
    .await;
    assert_eq!(foreign_create.status, 403, "foreign actor is refused");

    // A disabled User cannot act at all: the Web session is revoked.
    let disabled_create = post_json(
        surface.port,
        "/api/conversations",
        &disabled_cookie,
        &surface.origin,
        json!({
            "conversationId": Uuid::new_v4(),
            "projectId": seed.project_id,
            "agentId": seed.agent_id,
            "workspaceId": seed.workspace_id,
            "intentId": Uuid::new_v4(),
            "prompt": "disabled create",
        }),
    )
    .await;
    assert_eq!(disabled_create.status, 401, "disabled actor is refused");
    assert!(
        web_session::lookup(
            surface.kernel.pool(),
            &disabled_token,
            surface.auth.config().session_ttl()
        )
        .await
        .expect("session lookup")
        .is_none(),
        "the disabled user's session is revoked"
    );

    // A member queues a follow-up on the owner's conversation; a viewer and
    // a foreign actor cannot message it.
    let follow = post_json(
        surface.port,
        &format!("/api/conversations/{conversation_id}/messages"),
        &member_cookie,
        &surface.origin,
        json!({ "intentId": Uuid::new_v4(), "prompt": "from member" }),
    )
    .await;
    assert_eq!(follow.status, 200, "a member operates the Session");
    let viewer_msg = post_json(
        surface.port,
        &format!("/api/conversations/{conversation_id}/messages"),
        &viewer_cookie,
        &surface.origin,
        json!({ "intentId": Uuid::new_v4(), "prompt": "viewer message" }),
    )
    .await;
    assert_eq!(viewer_msg.status, 403, "viewer is refused on follow-up");
    let foreign_msg = post_json(
        surface.port,
        &format!("/api/conversations/{conversation_id}/messages"),
        &foreign_cookie,
        &surface.origin,
        json!({ "intentId": Uuid::new_v4(), "prompt": "foreign message" }),
    )
    .await;
    assert_eq!(
        foreign_msg.status, 403,
        "foreign actor is refused on follow-up"
    );

    // Actor attribution: the conversation audit names the creator, the
    // message audit names the queuer, and the durable rows keep every
    // actor.
    let created_actor: Option<(Option<Uuid>,)> = sqlx::query_as(
        "select actor_user_id from audit_events \
         where kind = 'conversation.created' and resource_id = $1 \
         order by seq desc limit 1",
    )
    .bind(conversation_id)
    .fetch_optional(surface.kernel.pool())
    .await
    .expect("conversation audit reads");
    assert_eq!(
        created_actor,
        Some((Some(seed.owner),)),
        "conversation.created carries the creating actor"
    );
    let message_actor: Option<(Option<Uuid>,)> = sqlx::query_as(
        "select actor_user_id from audit_events \
         where kind = 'message.accepted' and session_id = $1 \
         order by seq desc limit 1",
    )
    .bind(conversation_id)
    .fetch_optional(surface.kernel.pool())
    .await
    .expect("message audit reads");
    assert_eq!(
        message_actor,
        Some((Some(member),)),
        "message.accepted carries the queuing actor"
    );

    let session = surface
        .kernel
        .find_session(conversation_id)
        .await
        .unwrap()
        .expect("Session exists");
    assert_eq!(
        session.last_actor_user_id,
        Some(member),
        "the Session tracks the last queuer"
    );
    let runs = surface.kernel.list_runs(conversation_id).await.unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].actor_user_id, Some(seed.owner));
    assert_eq!(runs[1].actor_user_id, Some(member));

    surface.server.abort();
    let _ = surface.server.await;
}
