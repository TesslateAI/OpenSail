//! SQL-backed desired-state contracts: migrations, fail-closed Key Vault
//! emptiness, Lost observation, and post-converge reconcile_after.

use tokio::sync::Mutex;
use uuid::Uuid;
use voie_cloud::applications::ApplicationStore;
use voie_cloud::databases::DatabaseStore;
use voie_cloud::{Config, Kernel};

static MIGRATE_LOCK: Mutex<()> = Mutex::const_new(());

async fn kernel() -> Kernel {
    let _lock = MIGRATE_LOCK.lock().await;
    let kernel = Kernel::connect(&Config::database_url(
        std::env::var("VOIE_TEST_DATABASE_URL").expect("VOIE_TEST_DATABASE_URL"),
    ))
    .await
    .expect("postgres");
    kernel.migrate().await.expect("desired-state migrations");
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

struct Fixture {
    owner: Uuid,
    environment_id: Uuid,
    fabric: Uuid,
    databases: DatabaseStore,
}

async fn fixture(kernel: &Kernel, label: &str) -> Fixture {
    let owner = insert_user(kernel, label).await;
    let project = kernel
        .create_project(Uuid::new_v4(), owner, &format!("{label}-proj"), "team")
        .await
        .expect("project");
    let fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("{label}-fabric-{fabric}"))
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
    let store = ApplicationStore::new(kernel.pool().clone(), "console.test".into());
    let created = store
        .create(owner, project.id, workspace, "Desired State App", None)
        .await
        .expect("application");
    let environment_id: Uuid = sqlx::query_scalar(
        "select id from application_environments where application_id = $1 and kind = 'dev'",
    )
    .bind(created.application.id)
    .fetch_one(kernel.pool())
    .await
    .expect("dev environment");
    Fixture {
        owner,
        environment_id,
        fabric,
        databases: DatabaseStore::new(kernel.pool().clone()),
    }
}

#[tokio::test]
async fn unusable_secret_keeps_desired_present_without_operation_row() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "ds-secret").await;
    let created = fixture
        .databases
        .create(
            fixture.owner,
            fixture.environment_id,
            fixture.fabric,
            Uuid::new_v4(),
            &[0u8; 32],
        )
        .await
        .expect("database row");
    assert_eq!(created.state, "creating");
    assert_eq!(created.desired_state, "present");
    assert_eq!(created.desired_revision, 1);
    assert_eq!(created.observed_revision, 0);
    fixture
        .databases
        .fail_closed_creating(created.id)
        .await
        .expect("fail closed");
    let after = fixture
        .databases
        .get(fixture.owner, created.id)
        .await
        .expect("reload");
    assert_eq!(after.desired_state, "present");
    assert_eq!(after.state, "creating");
    assert_eq!(after.observed_state, "failed");
    assert_eq!(
        after.last_error_code.as_deref(),
        Some("secret_material_unavailable")
    );
    assert_eq!(after.desired_revision, 1);
    assert_eq!(after.observed_revision, 0);
    let ops: i64 =
        sqlx::query_scalar("select count(*) from database_operations where database_id = $1")
            .bind(created.id)
            .fetch_one(kernel.pool())
            .await
            .expect("operations");
    assert_eq!(ops, 0, "empty Key Vault must not invent a create operation");
}

#[tokio::test]
async fn lost_observation_persists_without_bumping_desired_revision() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "ds-lost").await;
    let created = fixture
        .databases
        .create(
            fixture.owner,
            fixture.environment_id,
            fixture.fabric,
            Uuid::new_v4(),
            &[1u8; 32],
        )
        .await
        .expect("database row");
    sqlx::query(
        "update application_databases \
         set observed_state = 'ready', observed_revision = 0, \
             reconcile_after = now() + interval '15 seconds' \
         where id = $1",
    )
    .bind(created.id)
    .execute(kernel.pool())
    .await
    .expect("converge");
    fixture
        .databases
        .record_lost(created.id, "durable_volume_missing", None)
        .await
        .expect("lost");
    let after = fixture
        .databases
        .get(fixture.owner, created.id)
        .await
        .expect("reload");
    assert_eq!(after.desired_state, "present");
    assert_eq!(after.desired_revision, 1);
    assert_eq!(after.observed_revision, 0);
    assert_eq!(after.observed_state, "lost");
    assert_eq!(
        after.last_error_code.as_deref(),
        Some("durable_volume_missing")
    );
    let due: Option<String> =
        sqlx::query_scalar("select reconcile_after::text from application_databases where id = $1")
            .bind(created.id)
            .fetch_one(kernel.pool())
            .await
            .expect("reconcile_after");
    assert!(
        due.as_deref().is_some_and(|value| !value.is_empty()),
        "Lost must remain on the observation cadence"
    );
}

#[tokio::test]
async fn workspace_post_converge_observation_is_scheduled() {
    let kernel = kernel().await;
    let owner = insert_user(&kernel, "ds-ws").await;
    let project = kernel
        .create_project(Uuid::new_v4(), owner, "ds-ws-proj", "team")
        .await
        .expect("project");
    let fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("ds-ws-fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .expect("fabric");
    let workspace = Uuid::new_v4();
    kernel
        .reserve_workspace(workspace, project.id, fabric, owner)
        .await
        .expect("reserve");
    assert!(
        kernel
            .activate_workspace(workspace, 1)
            .await
            .expect("activate")
    );
    let row = sqlx::query(
        "select state, desired_state, observed_state, desired_revision, observed_revision, \
                last_error_code, reconcile_after is not null as armed \
         from workspaces where id = $1",
    )
    .bind(workspace)
    .fetch_one(kernel.pool())
    .await
    .expect("workspace");
    let state: String = sqlx::Row::get(&row, "state");
    let desired: String = sqlx::Row::get(&row, "desired_state");
    let observed: String = sqlx::Row::get(&row, "observed_state");
    let desired_revision: i64 = sqlx::Row::get(&row, "desired_revision");
    let observed_revision: i64 = sqlx::Row::get(&row, "observed_revision");
    let armed: bool = sqlx::Row::get(&row, "armed");
    assert_eq!(state, "creating");
    assert_eq!(desired, "active");
    assert_eq!(observed, "active");
    assert_eq!(desired_revision, 1);
    assert_eq!(observed_revision, 1);
    assert!(armed, "converged Workspace must stay on reconcile_after");
}

#[tokio::test]
async fn release0_security_profile_advances_without_inventing_an_operation() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "ds-sec").await;
    let created = fixture
        .databases
        .create(
            fixture.owner,
            fixture.environment_id,
            fixture.fabric,
            Uuid::new_v4(),
            &[2u8; 32],
        )
        .await
        .expect("database row");
    assert_eq!(created.security_profile, 1);
    assert_eq!(created.desired_revision, 1);
    let bumped = fixture
        .databases
        .advance_release0_security_profile(created.id)
        .await
        .expect("advance");
    assert_eq!(bumped.security_profile, 2);
    assert_eq!(bumped.desired_revision, 2);
    assert_eq!(bumped.desired_state, "present");
    let again = fixture
        .databases
        .advance_release0_security_profile(created.id)
        .await
        .expect("idempotent at 2");
    assert_eq!(again.security_profile, 2);
    assert_eq!(again.desired_revision, 2);
    let ops: i64 =
        sqlx::query_scalar("select count(*) from database_operations where database_id = $1")
            .bind(created.id)
            .fetch_one(kernel.pool())
            .await
            .expect("operations");
    assert_eq!(ops, 0, "security_profile is desired state, not a journal");
}

#[tokio::test]
async fn live_census_excludes_teardown_application_databases() {
    let kernel = kernel().await;
    let live = fixture(&kernel, "census-live").await;
    let teardown = fixture(&kernel, "census-tear").await;
    let live_db = live
        .databases
        .create(
            live.owner,
            live.environment_id,
            live.fabric,
            Uuid::new_v4(),
            &[3u8; 32],
        )
        .await
        .expect("live database");
    let tear_db = teardown
        .databases
        .create(
            teardown.owner,
            teardown.environment_id,
            teardown.fabric,
            Uuid::new_v4(),
            &[4u8; 32],
        )
        .await
        .expect("teardown database");
    sqlx::query("update applications set state = 'deleting' where id = $1")
        .bind(tear_db.application_id)
        .execute(kernel.pool())
        .await
        .expect("mark application deleting");
    let after_teardown = live
        .databases
        .list_live_census()
        .await
        .expect("census after teardown");
    assert!(
        after_teardown.iter().any(|row| row.id == live_db.id),
        "live Application Database must remain in the census: {after_teardown:?}"
    );
    assert!(
        after_teardown.iter().all(|row| row.id != tear_db.id),
        "deleting Application Database must not remain in the census: {after_teardown:?}"
    );
    sqlx::query("update application_databases set desired_state = 'absent' where id = $1")
        .bind(live_db.id)
        .execute(kernel.pool())
        .await
        .expect("absent desired");
    let after_absent = live
        .databases
        .list_live_census()
        .await
        .expect("census after absent");
    assert!(
        after_absent.iter().all(|row| row.id != live_db.id),
        "absent desired is not live estate: {after_absent:?}"
    );
}

#[tokio::test]
async fn application_delete_puts_database_desired_absent() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "ds-del-absent").await;
    let created = fixture
        .databases
        .create(
            fixture.owner,
            fixture.environment_id,
            fixture.fabric,
            Uuid::new_v4(),
            &[5u8; 32],
        )
        .await
        .expect("database row");
    assert_eq!(created.desired_state, "present");
    assert_eq!(created.desired_revision, 1);
    ApplicationStore::new(kernel.pool().clone(), "console.test".into())
        .commit_delete(created.application_id)
        .await
        .expect("commit Application delete");
    let row = sqlx::query(
        "select state, desired_state, desired_revision, observed_revision, \
                reconcile_after is not null as armed \
         from application_databases where id = $1",
    )
    .bind(created.id)
    .fetch_one(kernel.pool())
    .await
    .expect("database after delete");
    let state: String = sqlx::Row::get(&row, "state");
    let desired: String = sqlx::Row::get(&row, "desired_state");
    let desired_revision: i64 = sqlx::Row::get(&row, "desired_revision");
    let observed_revision: i64 = sqlx::Row::get(&row, "observed_revision");
    let armed: bool = sqlx::Row::get(&row, "armed");
    assert_eq!(state, "creating");
    assert_eq!(desired, "absent");
    assert_eq!(desired_revision, 2);
    assert_eq!(observed_revision, 0);
    assert!(armed, "teardown must wake reconcile to PUT absent");
    let due = fixture.databases.list_due().await.expect("due");
    assert!(
        due.iter()
            .any(|row| row.id == created.id && row.desired_state == "absent"),
        "deleted Application Database must stay on list_due until Fabric absent: {due:?}"
    );
}

#[tokio::test]
async fn deleting_application_heals_present_database_to_absent_without_touching_live() {
    let kernel = kernel().await;
    let live = fixture(&kernel, "ds-heal-live").await;
    let leftover = fixture(&kernel, "ds-heal-left").await;
    let live_db = live
        .databases
        .create(
            live.owner,
            live.environment_id,
            live.fabric,
            Uuid::new_v4(),
            &[6u8; 32],
        )
        .await
        .expect("live database");
    let leftover_db = leftover
        .databases
        .create(
            leftover.owner,
            leftover.environment_id,
            leftover.fabric,
            Uuid::new_v4(),
            &[7u8; 32],
        )
        .await
        .expect("leftover database");
    sqlx::query("update applications set state = 'deleting' where id = $1")
        .bind(leftover_db.application_id)
        .execute(kernel.pool())
        .await
        .expect("mark application deleting");
    sqlx::query(
        "update application_databases \
         set desired_state = 'present', reconcile_after = null \
         where id = $1",
    )
    .bind(leftover_db.id)
    .execute(kernel.pool())
    .await
    .expect("historical desired present while Application deleting");
    leftover
        .databases
        .persist_absent_desired_for_removing_applications()
        .await
        .expect("heal leftover present");
    let healed = sqlx::query(
        "select state, desired_state, desired_revision from application_databases where id = $1",
    )
    .bind(leftover_db.id)
    .fetch_one(kernel.pool())
    .await
    .expect("healed leftover");
    let state: String = sqlx::Row::get(&healed, "state");
    let desired: String = sqlx::Row::get(&healed, "desired_state");
    let desired_revision: i64 = sqlx::Row::get(&healed, "desired_revision");
    assert_eq!(state, "creating");
    assert_eq!(desired, "absent");
    assert_eq!(desired_revision, leftover_db.desired_revision + 1);
    let live_after = live
        .databases
        .get(live.owner, live_db.id)
        .await
        .expect("live reload");
    assert_eq!(live_after.state, "creating");
    assert_eq!(live_after.desired_state, "present");
    assert_eq!(live_after.desired_revision, live_db.desired_revision);
}

#[test]
fn leftover_failed_proven_clears_before_desired_absent() {
    let sql = include_str!("../migrations/0032_deployment_leftover_accepted_only.sql");
    let proven = sql
        .find("set proven = false")
        .expect("clears proven on leftover failed");
    let failed = sql[proven..]
        .find("where state = 'failed';")
        .expect("clears every leftover failed row");
    let absent = sql
        .find("desired_state = 'absent'")
        .expect("then convert leftover failed to desired absent");
    assert!(
        proven < absent,
        "proven must clear before 0031 leftover failed becomes absent"
    );
    assert!(
        !sql[proven..proven + failed].contains("desired_state"),
        "proven clear must not skip leftover failed rows already converted to absent"
    );
}

#[tokio::test]
async fn mark_ready_keeps_the_established_credential_identity() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "ds-ready-cred").await;
    let created = fixture
        .databases
        .create(
            fixture.owner,
            fixture.environment_id,
            fixture.fabric,
            Uuid::new_v4(),
            &[3u8; 32],
        )
        .await
        .expect("database row");
    let established = Uuid::new_v4();
    let winner = fixture
        .databases
        .attach_credential(created.id, established)
        .await
        .expect("claim");
    assert_eq!(winner, established);
    let other = Uuid::new_v4();
    fixture
        .databases
        .mark_ready(created.id, other)
        .await
        .expect("ready");
    let after = fixture
        .databases
        .get_internal(created.id)
        .await
        .expect("reload");
    assert_eq!(after.credential_secret_id, Some(established));
    assert_eq!(after.observed_state, "ready");
}

#[tokio::test]
async fn traffic_target_and_pod_generation_columns_exist() {
    let kernel = kernel().await;
    sqlx::query(
        "select desired_deployment_id, observed_deployment_id, traffic_observed_revision \
         from application_environments limit 0",
    )
    .execute(kernel.pool())
    .await
    .expect("traffic target columns");
    sqlx::query(
        "select desired_pod_generation, observed_pod_generation from application_deployments limit 0",
    )
    .execute(kernel.pool())
    .await
    .expect("pod generation columns");
}
