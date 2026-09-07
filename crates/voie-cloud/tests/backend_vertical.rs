use uuid::Uuid;
use voie_cloud::exec_journal::{BeginDispatch, ExecJournal};
use voie_cloud::session_store::{AppendEvent, BlobStore, SessionStore};
use voie_cloud::{Config, Kernel};

fn database_url() -> String {
    std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at a PostgreSQL database")
}

async fn seed_workspace(kernel: &Kernel) -> (Uuid, Uuid, Uuid, Uuid) {
    let owner = Uuid::new_v4();
    sqlx::query("insert into users (id, issuer, subject) values ($1, $2, $3)")
        .bind(owner)
        .bind("test-issuer")
        .bind(owner.to_string())
        .execute(kernel.pool())
        .await
        .expect("user inserts");
    let project = kernel
        .create_project(
            Uuid::new_v4(),
            owner,
            &format!("project-{owner}"),
            "personal",
        )
        .await
        .expect("project creates");
    let fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .expect("fabric inserts");
    let workspace = Uuid::new_v4();
    sqlx::query("insert into workspaces (id, project_id, fabric_id, observed_state) values ($1, $2, $3, 'ready')")
        .bind(workspace)
        .bind(project.id)
        .bind(fabric)
        .execute(kernel.pool())
        .await
        .expect("workspace inserts");
    let agent = Uuid::new_v4();
    sqlx::query("insert into agents (id, project_id, name) values ($1, $2, $3)")
        .bind(agent)
        .bind(project.id)
        .bind(format!("agent-{agent}"))
        .execute(kernel.pool())
        .await
        .expect("agent inserts");
    (project.id, agent, workspace, fabric)
}

#[tokio::test]
async fn backend_vertical_postgres() {
    let kernel = Kernel::connect(&Config::database_url(database_url()))
        .await
        .expect("PostgreSQL connection succeeds");
    kernel.migrate().await.expect("migrations apply");
    kernel.migrate().await.expect("repeat migration is safe");
    assert!(kernel.ready().await, "migrated PostgreSQL is ready");

    let exists: bool = sqlx::query_scalar(
        "select exists(select 1 from information_schema.tables \
         where table_schema = 'public' and table_name = 'session_events')",
    )
    .fetch_one(kernel.pool())
    .await
    .expect("session_events existence check");
    assert!(exists, "session_events exists");

    let (project_id, agent, workspace, _) = seed_workspace(&kernel).await;
    let session = kernel
        .create_session(Uuid::new_v4(), project_id, agent, workspace)
        .await
        .expect("session metadata creates");
    assert_eq!(session.head_revision, 0);

    let store = SessionStore::new(kernel.pool().clone());
    let writer = store.writer(session.id).await.expect("first writer pins");
    let first_generation = writer.writer_generation();
    assert!(first_generation > 0);
    drop(writer);
    let writer = store.writer(session.id).await.expect("second writer pins");
    assert!(writer.writer_generation() > first_generation);
    drop(writer);

    let journal = ExecJournal::new(kernel.pool().clone());
    let call_id = format!("call-{}", Uuid::new_v4());
    let hash_a = ExecJournal::request_hash("echo a");
    let hash_b = ExecJournal::request_hash("echo b");

    let first = journal
        .begin_dispatch(workspace, &call_id, &hash_a)
        .await
        .expect("first dispatch persists");
    assert_eq!(first, BeginDispatch::ReadyToDispatch);

    let unknown = journal
        .begin_dispatch(workspace, &call_id, &hash_a)
        .await
        .expect("dispatched call is unknown");
    assert_eq!(unknown, BeginDispatch::OutcomeUnknown);

    journal
        .complete(workspace, &call_id, &hash_a, "ok:a")
        .await
        .expect("terminal result stores");
    let retained = journal
        .begin_dispatch(workspace, &call_id, &hash_a)
        .await
        .expect("terminal lookup");
    assert_eq!(
        retained,
        BeginDispatch::Terminal {
            result: "ok:a".into()
        }
    );
    assert_eq!(retained.retained_result(), Some("ok:a"));

    let conflict = journal
        .begin_dispatch(workspace, &call_id, &hash_b)
        .await
        .expect("hash mismatch");
    assert_eq!(conflict, BeginDispatch::Conflict);

    let other = format!("call-{}", Uuid::new_v4());
    let dispatched = journal
        .begin_dispatch(workspace, &other, &hash_a)
        .await
        .expect("second call dispatches once");
    assert_eq!(dispatched, BeginDispatch::ReadyToDispatch);
    let still_unknown = journal
        .begin_dispatch(workspace, &other, &hash_a)
        .await
        .expect("no redispatch");
    assert_eq!(still_unknown, BeginDispatch::OutcomeUnknown);
    assert!(
        still_unknown.is_outcome_unknown(),
        "unknown outcome maps explicitly for aborted-effect recording"
    );

    // Fencing and revision expectations are checked before any payload write.
    let mut writer = store.writer(session.id).await.expect("writer pins");
    let generation = writer.writer_generation();
    let fenced = AppendEvent {
        append_id: Uuid::new_v4(),
        writer_generation: generation + 1,
        expected_revision: 1,
        bytes: br#"{"stale":"writer"}"#.to_vec(),
        model_usage: None,
    };
    assert!(matches!(
        writer.append(fenced).await,
        Err(voie_cloud::session_store::StoreError::Fenced)
    ));
    let stale = AppendEvent {
        append_id: Uuid::new_v4(),
        writer_generation: generation,
        expected_revision: 5,
        bytes: br#"{"stale":"revision"}"#.to_vec(),
        model_usage: None,
    };
    assert!(matches!(
        writer.append(stale).await,
        Err(voie_cloud::session_store::StoreError::Revision)
    ));
    drop(writer);
}

#[tokio::test]
#[ignore = "live C3: real PostgreSQL, Azure Blob, and optional fabric/model"]
async fn live_c3() {
    let database_url = std::env::var("VOIE_DATABASE_URL")
        .or_else(|_| std::env::var("VOIE_TEST_DATABASE_URL"))
        .expect("VOIE_DATABASE_URL or VOIE_TEST_DATABASE_URL is required");
    let kernel = Kernel::connect(&Config::database_url(database_url.clone()))
        .await
        .expect("PostgreSQL connection succeeds");
    kernel.migrate().await.expect("migrations apply");
    assert!(kernel.ready().await);

    let (project_id, agent, workspace, _fabric) = seed_workspace(&kernel).await;
    let store = SessionStore::new(kernel.pool().clone());
    let session = store
        .create_session(Uuid::new_v4(), project_id, agent, workspace)
        .await
        .expect("session metadata creates");
    assert_eq!(session.head_revision, 0);

    let append_id = Uuid::new_v4();
    let body = br#"{"type":"user","text":"ping"}"#.to_vec();
    let mut writer = store.writer(session.id).await.expect("writer pins");
    let revision = writer
        .append(AppendEvent {
            append_id,
            writer_generation: writer.writer_generation(),
            expected_revision: 1,
            bytes: body.clone(),
            model_usage: None,
        })
        .await
        .expect("canonical postgres event appends");
    assert_eq!(revision, 1);
    let retry = writer
        .append(AppendEvent {
            append_id,
            writer_generation: writer.writer_generation(),
            expected_revision: 1,
            bytes: body.clone(),
            model_usage: None,
        })
        .await
        .expect("identical append retries");
    assert_eq!(retry, 1);
    drop(writer);
    drop(store);
    drop(kernel);

    let kernel = Kernel::connect(&Config::database_url(database_url))
        .await
        .expect("control reconnects");
    kernel.migrate().await.expect("restarted control migrates");
    let store = SessionStore::new(kernel.pool().clone());
    let head = store.inspect_head(session.id).await.expect("head inspects");
    assert_eq!(head.head_revision, 1);
    let history = store
        .load_history(session.id)
        .await
        .expect("ordered history loads");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].reference.append_id, append_id);
    assert_eq!(history[0].bytes, body);
    let blob = BlobStore::from_env().expect("real Azure Blob configuration is required");
    assert!(
        blob.reachable().await,
        "release/snapshot blob remains reachable"
    );
    println!(
        "live-c3: session {} revision 1 survived control restart",
        session.id
    );

    let journal = ExecJournal::new(kernel.pool().clone());
    let call_id = format!("live-{}", Uuid::new_v4());
    let hash = ExecJournal::request_hash("uname -a");
    assert_eq!(
        journal
            .begin_dispatch(workspace, &call_id, &hash)
            .await
            .unwrap(),
        BeginDispatch::ReadyToDispatch
    );
    journal
        .complete(workspace, &call_id, &hash, "Linux")
        .await
        .unwrap();
    assert_eq!(
        journal
            .begin_dispatch(workspace, &call_id, &hash)
            .await
            .unwrap(),
        BeginDispatch::Terminal {
            result: "Linux".into()
        }
    );
    assert_eq!(
        journal
            .begin_dispatch(workspace, &call_id, &ExecJournal::request_hash("other"))
            .await
            .unwrap(),
        BeginDispatch::Conflict
    );
    println!("live-c3: exec journal retained terminal result and refused conflicting hash");

    match voie_cloud::model::ModelRelay::from_env() {
        Ok(relay) => {
            let response = relay
                .complete(voie_cloud::model::ModelRequest {
                    messages: vec![voie_cloud::model::ModelMessage::text(
                        "user",
                        "Reply with the single word pong.",
                    )],
                    tools: Vec::new(),
                    max_tokens: 16,
                })
                .await
                .expect("model provider request succeeds");
            assert!(!response.content.is_empty(), "model returned content");
            println!(
                "live-c3: model relay returned {} bytes; usage={:?}",
                response.content.len(),
                response.usage
            );
        }
        Err(error) => {
            println!("live-c3 remaining live dependency: model provider ({error})");
        }
    }

    match voie_cloud::fabric_client::FabricClient::from_env() {
        Ok(fabric) => {
            fabric.health().await.expect("fabric health succeeds");
            fabric
                .create_workspace(workspace, None, None)
                .await
                .expect("real workspace creates");
            let outcome = journal
                .execute(
                    &fabric,
                    workspace,
                    &format!("ws-{}", Uuid::new_v4()),
                    "true",
                )
                .await
                .expect("workspace command dispatches");
            println!("live-c3: fabric exec outcome={outcome:?}");
            let repeat = journal
                .execute(&fabric, workspace, "repeat-call", "true")
                .await
                .expect("first fabric call");
            let again = journal
                .execute(&fabric, workspace, "repeat-call", "true")
                .await
                .expect("repeat does not redispatch");
            assert_eq!(repeat, again);
        }
        Err(error) => {
            println!(
                "live-c3 remaining live dependency: real voie-fabricd mTLS workspace exec ({error})"
            );
        }
    }
}
