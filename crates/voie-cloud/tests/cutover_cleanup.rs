//! Cutover must not report success while a superseded predecessor still runs.

use uuid::Uuid;
use voie_cloud::applications::ApplicationError;
use voie_cloud::deployments::DeploymentStore;
use voie_cloud::http::Platform;
use voie_cloud::{Config, Kernel};

async fn kernel() -> Kernel {
    let kernel = Kernel::connect(&Config::database_url(
        std::env::var("VOIE_TEST_DATABASE_URL").expect("VOIE_TEST_DATABASE_URL"),
    ))
    .await
    .expect("postgres");
    kernel.migrate().await.expect("migrate");
    kernel
}

async fn insert_user(kernel: &Kernel, label: &str) -> Uuid {
    let user_id = Uuid::new_v4();
    sqlx::query("insert into users (id, issuer, subject) values ($1, $2, $3)")
        .bind(user_id)
        .bind(format!("{label}-{}", Uuid::new_v4()))
        .bind(label)
        .execute(kernel.pool())
        .await
        .expect("user");
    user_id
}

async fn insert_ready_release(
    kernel: &Kernel,
    application_id: Uuid,
    workspace_id: Uuid,
    actor: Uuid,
) -> Uuid {
    let release_id = Uuid::new_v4();
    sqlx::query(
        "insert into application_releases (
            id, application_id, build_intent_id, request_hash, source_workspace_id,
            source_exec_generation, runtime_profile, manifest, manifest_hash,
            state, created_by_user_id
         ) values ($1, $2, $3, $4, $5, 1, 'universal-v1', $6::jsonb, $7, 'ready', $8)",
    )
    .bind(release_id)
    .bind(application_id)
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4().as_bytes().as_slice())
    .bind(workspace_id)
    .bind(r#"{"runtime":"universal-v1"}"#)
    .bind(&[7u8; 32].as_slice())
    .bind(actor)
    .execute(kernel.pool())
    .await
    .expect("release");
    release_id
}

#[tokio::test]
async fn successful_cutover_stops_the_superseded_predecessor() {
    let kernel = kernel().await;
    let owner = insert_user(&kernel, "cutover-owner").await;
    let project = kernel
        .create_project(Uuid::new_v4(), owner, &format!("cutover-{owner}"), "team")
        .await
        .expect("project");
    let fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("cutover-fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .expect("fabric");
    let workspace = Uuid::new_v4();
    sqlx::query(
        "insert into workspaces (id, project_id, fabric_id, state, created_by_user_id, exec_generation, observed_state) \
         values ($1, $2, $3, 'creating', $4, 1, 'ready')",
    )
    .bind(workspace)
    .bind(project.id)
    .bind(fabric)
    .bind(owner)
    .execute(kernel.pool())
    .await
    .expect("workspace");

    let apps = voie_cloud::applications::ApplicationStore::new(
        kernel.pool().clone(),
        "console.test".into(),
    );
    let created = apps
        .create(owner, project.id, workspace, "Cutover", None)
        .await
        .expect("application");
    let dev = created
        .environments
        .iter()
        .find(|environment| environment.kind == "dev")
        .expect("dev");
    let first_release =
        insert_ready_release(&kernel, created.application.id, workspace, owner).await;
    let second_release =
        insert_ready_release(&kernel, created.application.id, workspace, owner).await;

    let platform = Platform::new(kernel.pool().clone(), "console.test".into(), None);
    let store = DeploymentStore::new(kernel.pool().clone());
    let (_, first) = store
        .deploy(owner, dev.id, first_release, Uuid::new_v4(), None)
        .await
        .expect("first deploy");
    store.mark_healthy(first.id).await.expect("first healthy");
    let first_active = platform
        .activate_deployment(owner, first.id)
        .await
        .expect("first activate");
    assert_eq!(first_active.wire_state(), "active");
    assert!(first_active.proven);
    assert!(first_active.traffic);

    let (_, second) = store
        .deploy(owner, dev.id, second_release, Uuid::new_v4(), None)
        .await
        .expect("second deploy");
    store.mark_healthy(second.id).await.expect("second healthy");
    let activated = platform
        .activate_deployment(owner, second.id)
        .await
        .expect("settled cutover");
    assert_eq!(activated.wire_state(), "active");
    assert!(activated.proven);
    assert!(activated.traffic);
    let predecessor = store.get_internal(first.id).await.expect("predecessor");
    assert_eq!(predecessor.desired_state, "absent");
    assert_eq!(predecessor.wire_state(), "stopped");
    assert_eq!(
        predecessor.observed_state, "absent",
        "a settled cutover must observe the predecessor absent"
    );

    sqlx::query(
        "update application_deployments \
         set observed_state = '', observed_revision = 0 \
         where id = $1",
    )
    .bind(first.id)
    .execute(kernel.pool())
    .await
    .expect("simulate desired-absent predecessor with Fabric still present");
    let again = platform
        .activate_deployment(owner, second.id)
        .await
        .expect("already-active still settles leftover predecessor");
    assert_eq!(again.wire_state(), "active");
    assert!(again.traffic);
    let cleaned = store.get_internal(first.id).await.expect("cleaned");
    assert_eq!(cleaned.desired_state, "absent");
    assert_eq!(cleaned.wire_state(), "stopped");
    assert_eq!(
        cleaned.observed_state, "absent",
        "activating an already-active Deployment must notice an unsettled predecessor"
    );
    assert!(
        !matches!(
            platform.activate_deployment(owner, second.id).await,
            Err(ApplicationError::PredecessorCleanupPending)
        ),
        "stopped predecessor is already settled"
    );
}

#[tokio::test]
async fn definite_materialize_failure_stops_and_unknown_is_not_auto_cleaned() {
    let kernel = kernel().await;
    let owner = insert_user(&kernel, "fail-gc-owner").await;
    let project = kernel
        .create_project(Uuid::new_v4(), owner, &format!("fail-gc-{owner}"), "team")
        .await
        .expect("project");
    let fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("fail-gc-fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .expect("fabric");
    let workspace = Uuid::new_v4();
    sqlx::query(
        "insert into workspaces (id, project_id, fabric_id, state, created_by_user_id, exec_generation, observed_state) \
         values ($1, $2, $3, 'creating', $4, 1, 'ready')",
    )
    .bind(workspace)
    .bind(project.id)
    .bind(fabric)
    .bind(owner)
    .execute(kernel.pool())
    .await
    .expect("workspace");
    let apps = voie_cloud::applications::ApplicationStore::new(
        kernel.pool().clone(),
        "console.test".into(),
    );
    let created = apps
        .create(owner, project.id, workspace, "FailGc", None)
        .await
        .expect("application");
    let dev = created
        .environments
        .iter()
        .find(|environment| environment.kind == "dev")
        .expect("dev");
    let release = insert_ready_release(&kernel, created.application.id, workspace, owner).await;
    let platform = Platform::new(kernel.pool().clone(), "console.test".into(), None);
    let store = DeploymentStore::new(kernel.pool().clone());
    let (_, failed) = store
        .deploy(owner, dev.id, release, Uuid::new_v4(), None)
        .await
        .expect("candidate");
    store.fail(failed.id).await.expect("definite failure");
    let queued = store.get_internal(failed.id).await.expect("failed row");
    assert_eq!(queued.state, "accepted");
    assert_eq!(queued.desired_state, "absent");
    let leftover_journal =
        sqlx::query("update application_deployments set state = 'failed' where id = $1")
            .bind(failed.id)
            .execute(kernel.pool())
            .await;
    assert!(
        leftover_journal.is_err(),
        "leftover process cannot be a failed journal"
    );
    platform.resume_dispatched_deployment(&queued).await;
    let stopped = store.get_internal(failed.id).await.expect("cleaned");
    assert_eq!(stopped.desired_state, "absent");
    assert_eq!(stopped.wire_state(), "stopped");
    assert_eq!(
        stopped.observed_state, "absent",
        "definite materialize failure uses the desired-absent teardown path"
    );

    let second_release =
        insert_ready_release(&kernel, created.application.id, workspace, owner).await;
    let (_, unknown) = store
        .deploy(owner, dev.id, second_release, Uuid::new_v4(), None)
        .await
        .expect("ambiguous candidate");
    store.unknown(unknown.id).await.expect("hold unknown");
    let held = store.get_internal(unknown.id).await.expect("unknown row");
    assert_eq!(held.state, "accepted");
    assert_eq!(held.desired_state, "running");
    assert_eq!(held.last_error_code.as_deref(), Some("fabric_unknown"));
    platform.resume_dispatched_deployment(&held).await;
    let still = store
        .get_internal(unknown.id)
        .await
        .expect("unknown remains");
    assert_eq!(still.desired_state, "running");
    assert_eq!(
        still.state, "accepted",
        "ambiguous Fabric observation must not discard desired running"
    );
}

#[tokio::test]
async fn proven_health_can_settle_an_unknown_restore_deployment() {
    let kernel = kernel().await;
    let owner = insert_user(&kernel, "restore-unknown-owner").await;
    let project = kernel
        .create_project(
            Uuid::new_v4(),
            owner,
            &format!("restore-unknown-{owner}"),
            "team",
        )
        .await
        .expect("project");
    let fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("restore-unknown-fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .expect("fabric");
    let workspace = Uuid::new_v4();
    sqlx::query(
        "insert into workspaces (id, project_id, fabric_id, state, created_by_user_id, exec_generation, observed_state) \
         values ($1, $2, $3, 'creating', $4, 1, 'ready')",
    )
    .bind(workspace)
    .bind(project.id)
    .bind(fabric)
    .bind(owner)
    .execute(kernel.pool())
    .await
    .expect("workspace");
    let apps = voie_cloud::applications::ApplicationStore::new(
        kernel.pool().clone(),
        "console.test".into(),
    );
    let created = apps
        .create(owner, project.id, workspace, "RestoreUnknown", None)
        .await
        .expect("application");
    let dev = created
        .environments
        .iter()
        .find(|environment| environment.kind == "dev")
        .expect("dev");
    let release = insert_ready_release(&kernel, created.application.id, workspace, owner).await;
    let store = DeploymentStore::new(kernel.pool().clone());
    sqlx::query("update applications set state = 'restoring' where id = $1")
        .bind(created.application.id)
        .execute(kernel.pool())
        .await
        .expect("restoring");
    let intent = Uuid::new_v4();
    let (begin, first) = store
        .deploy_for_restore(owner, dev.id, release, intent)
        .await
        .expect("restore deploy");
    assert!(matches!(
        begin,
        voie_cloud::deployments::BeginDeployment::ReadyToDispatch { id } if id == first.id
    ));
    store.unknown(first.id).await.expect("materialize unknown");
    let (retry, existing) = store
        .deploy_for_restore(owner, dev.id, release, intent)
        .await
        .expect("restore retry");
    assert!(matches!(
        retry,
        voie_cloud::deployments::BeginDeployment::ReadyToDispatch { id } if id == existing.id
    ));
    assert_eq!(existing.id, first.id);
    assert_eq!(existing.state, "accepted");
    assert_eq!(existing.desired_state, "running");
    let healthy = store
        .mark_healthy(first.id)
        .await
        .expect("proven health settles a running candidate");
    assert!(healthy.proven);
    assert_eq!(healthy.state, "accepted");
    store
        .fail(first.id)
        .await
        .expect("fail is a no-op on proven");
    assert!(
        store
            .get_internal(first.id)
            .await
            .expect("still proven")
            .proven
    );
    let refused_id = first.id;
    sqlx::query(
        "update application_deployments set desired_state = 'absent', proven = false where id = $1",
    )
    .bind(refused_id)
    .execute(kernel.pool())
    .await
    .expect("force discarded");
    assert!(
        store.mark_healthy(refused_id).await.is_err(),
        "discarded candidate must not become healthy"
    );
}

#[tokio::test]
async fn matching_traffic_ids_are_unsettled_until_fabric_revision_proof() {
    let kernel = kernel().await;
    let owner = insert_user(&kernel, "traffic-proof-owner").await;
    let project = kernel
        .create_project(
            Uuid::new_v4(),
            owner,
            &format!("traffic-proof-{owner}"),
            "team",
        )
        .await
        .expect("project");
    let fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("traffic-proof-fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .expect("fabric");
    let workspace = Uuid::new_v4();
    sqlx::query(
        "insert into workspaces (id, project_id, fabric_id, state, created_by_user_id, exec_generation, observed_state) \
         values ($1, $2, $3, 'creating', $4, 1, 'ready')",
    )
    .bind(workspace)
    .bind(project.id)
    .bind(fabric)
    .bind(owner)
    .execute(kernel.pool())
    .await
    .expect("workspace");
    let apps = voie_cloud::applications::ApplicationStore::new(
        kernel.pool().clone(),
        "console.test".into(),
    );
    let created = apps
        .create(owner, project.id, workspace, "TrafficProof", None)
        .await
        .expect("application");
    let dev = created
        .environments
        .iter()
        .find(|environment| environment.kind == "dev")
        .expect("dev");
    let release = insert_ready_release(&kernel, created.application.id, workspace, owner).await;
    let store = DeploymentStore::new(kernel.pool().clone());
    let (_, deployment) = store
        .deploy(owner, dev.id, release, Uuid::new_v4(), None)
        .await
        .expect("deploy");
    store.mark_healthy(deployment.id).await.expect("proven");
    sqlx::query(
        "update application_environments \
         set desired_deployment_id = $1, \
             observed_deployment_id = $1, \
             active_deployment_id = $1, \
             revision = 3, \
             traffic_observed_revision = 0 \
         where id = $2",
    )
    .bind(deployment.id)
    .bind(dev.id)
    .execute(kernel.pool())
    .await
    .expect("migrated active IDs with no Fabric revision");
    let behind = store
        .get_internal(deployment.id)
        .await
        .expect("behind proof");
    assert!(
        !behind.traffic,
        "IDs equal with traffic_observed_revision behind is not settled"
    );
    let settled = store
        .settle_observed_traffic_at(deployment.id, Some(3))
        .await
        .expect("Fabric revision proof settles");
    assert!(
        settled.traffic,
        "IDs equal with Fabric revision caught up is settled"
    );
    let observed: i64 = sqlx::query_scalar(
        "select traffic_observed_revision from application_environments where id = $1",
    )
    .bind(dev.id)
    .fetch_one(kernel.pool())
    .await
    .expect("observed revision");
    assert!(observed >= 3);
}
