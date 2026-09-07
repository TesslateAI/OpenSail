//! Backup admission: User-row claim, inflight caps, Blob byte budget, and
//! Application-delete recovery-key fencing.

use tokio::sync::Mutex;
use uuid::Uuid;
use voie_cloud::applications::{ApplicationError, ApplicationStore};
use voie_cloud::databases::DatabaseStore;
use voie_cloud::storage::GIB;
use voie_cloud::{Config, Kernel};

static MIGRATE_LOCK: Mutex<()> = Mutex::const_new(());

async fn kernel() -> Kernel {
    let _lock = MIGRATE_LOCK.lock().await;
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

struct Fixture {
    owner: Uuid,
    application_id: Uuid,
    workspace: Uuid,
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
        .create(owner, project.id, workspace, "Backup App", None)
        .await
        .expect("application");
    Fixture {
        owner,
        application_id: created.application.id,
        workspace,
        databases: DatabaseStore::new(kernel.pool().clone()),
    }
}

async fn insert_database(kernel: &Kernel, fixture: &Fixture, env_kind: &str) -> Uuid {
    let env: Uuid = sqlx::query_scalar(
        "select id from application_environments where application_id = $1 and kind = $2",
    )
    .bind(fixture.application_id)
    .bind(env_kind)
    .fetch_one(kernel.pool())
    .await
    .expect("environment");
    let fabric: Uuid = sqlx::query_scalar("select fabric_id from workspaces where id = $1")
        .bind(fixture.workspace)
        .fetch_one(kernel.pool())
        .await
        .expect("fabric");
    let id = Uuid::new_v4();
    sqlx::query(
        "insert into application_databases \
         (id, application_id, environment_id, engine_profile, fabric_id, storage_bytes) \
         values ($1, $2, $3, 'voie-postgres:v1', $4, 8589934592)",
    )
    .bind(id)
    .bind(fixture.application_id)
    .bind(env)
    .bind(fabric)
    .execute(kernel.pool())
    .await
    .expect("database");
    id
}

fn hash(label: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let bytes = label.as_bytes();
    out[..bytes.len().min(32)].copy_from_slice(&bytes[..bytes.len().min(32)]);
    out
}

#[tokio::test]
async fn disabled_user_cannot_begin_backup() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "bak-disabled").await;
    let database_id = insert_database(&kernel, &fixture, "dev").await;
    sqlx::query("update users set status = 'disabled' where id = $1")
        .bind(fixture.owner)
        .execute(kernel.pool())
        .await
        .expect("disable");
    let error = fixture
        .databases
        .begin_backup(fixture.owner, database_id, Uuid::new_v4(), &hash("op"))
        .await
        .expect_err("disabled actor");
    assert!(matches!(error, ApplicationError::Auth));
}

#[tokio::test]
async fn second_inflight_backup_on_same_database_is_refused() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "bak-db-cap").await;
    let database_id = insert_database(&kernel, &fixture, "dev").await;
    fixture
        .databases
        .begin_backup(fixture.owner, database_id, Uuid::new_v4(), &hash("a"))
        .await
        .expect("first backup");
    let error = fixture
        .databases
        .begin_backup(fixture.owner, database_id, Uuid::new_v4(), &hash("b"))
        .await
        .expect_err("second backup");
    assert!(matches!(error, ApplicationError::WorkspaceBusy));
}

#[tokio::test]
async fn second_inflight_backup_on_same_project_is_refused() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "bak-proj-cap").await;
    let dev = insert_database(&kernel, &fixture, "dev").await;
    let prod = insert_database(&kernel, &fixture, "prod").await;
    fixture
        .databases
        .begin_backup(fixture.owner, dev, Uuid::new_v4(), &hash("dev"))
        .await
        .expect("first project backup");
    let error = fixture
        .databases
        .begin_backup(fixture.owner, prod, Uuid::new_v4(), &hash("prod"))
        .await
        .expect_err("second project backup");
    assert!(matches!(error, ApplicationError::WorkspaceBusy));
}

#[tokio::test]
async fn archive_backup_is_not_blocked_by_project_inflight_cap() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "bak-arch-cap").await;
    let dev = insert_database(&kernel, &fixture, "dev").await;
    let prod = insert_database(&kernel, &fixture, "prod").await;
    fixture
        .databases
        .begin_backup(fixture.owner, dev, Uuid::new_v4(), &hash("stale"))
        .await
        .expect("stale dispatched backup");
    sqlx::query("update applications set state = 'archiving' where id = $1")
        .bind(fixture.application_id)
        .execute(kernel.pool())
        .await
        .expect("archiving");
    fixture
        .databases
        .begin_backup(fixture.owner, prod, Uuid::new_v4(), &hash("archive"))
        .await
        .expect("archive capture backup must proceed while Application is archiving");
}

#[tokio::test]
async fn expired_backups_honor_byte_budget() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "bak-bytes").await;
    let database_id = insert_database(&kernel, &fixture, "dev").await;
    for (label, size) in [("new", 20 * GIB), ("mid", 20 * GIB), ("old", 8 * GIB)] {
        let id = Uuid::new_v4();
        sqlx::query(
            "insert into database_backups \
             (id, database_id, object_key, content_hash, byte_length, kind, pinned, created_at) \
             values ($1, $2, $3, $4, $5, 'manual', false, now() - ($6 || ' minutes')::interval)",
        )
        .bind(id)
        .bind(database_id)
        .bind(format!("backups/databases/{database_id}/{id}.pgdump"))
        .bind(hash(label).as_slice())
        .bind(size)
        .bind(match label {
            "new" => "1",
            "mid" => "2",
            _ => "3",
        })
        .execute(kernel.pool())
        .await
        .expect("backup row");
    }
    let expired = fixture
        .databases
        .expired_backups(database_id)
        .await
        .expect("expire");
    assert_eq!(expired.len(), 2);
    assert_eq!(expired[0].byte_length, 20 * GIB);
    assert_eq!(expired[1].byte_length, 8 * GIB);
}

#[tokio::test]
async fn delete_fails_closed_while_recoverable_backup_keys_remain() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "bak-reclaim").await;
    let database_id = insert_database(&kernel, &fixture, "dev").await;
    let backup_id = Uuid::new_v4();
    sqlx::query(
        "insert into database_backups \
         (id, database_id, object_key, content_hash, byte_length, kind, pinned) \
         values ($1, $2, $3, $4, 32, 'manual', false)",
    )
    .bind(backup_id)
    .bind(database_id)
    .bind(format!(
        "backups/databases/{database_id}/{backup_id}.pgdump"
    ))
    .bind(hash("live").as_slice())
    .execute(kernel.pool())
    .await
    .expect("backup");
    let keys = fixture
        .databases
        .list_application_recovery_keys(fixture.application_id)
        .await
        .expect("keys");
    assert_eq!(keys.len(), 1);
    let error = fixture
        .databases
        .reclaim_application_recovery_blobs(fixture.application_id, None)
        .await
        .expect_err("blob required");
    assert!(matches!(
        error,
        ApplicationError::Kernel(voie_cloud::KernelError::Database)
    ));
    let remaining = fixture
        .databases
        .list_application_recovery_keys(fixture.application_id)
        .await
        .expect("still recoverable");
    assert_eq!(remaining, keys);
}

#[tokio::test]
async fn successful_backup_releases_admission_for_the_next_backup() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "bak-ready").await;
    let database_id = insert_database(&kernel, &fixture, "dev").await;
    let first = Uuid::new_v4();
    fixture
        .databases
        .begin_backup(fixture.owner, database_id, first, &hash("a"))
        .await
        .expect("first backup");
    fixture
        .databases
        .complete_backup(database_id, first)
        .await
        .expect("settle first");
    fixture
        .databases
        .begin_backup(fixture.owner, database_id, Uuid::new_v4(), &hash("b"))
        .await
        .expect("second backup after ready");
}

#[tokio::test]
async fn archive_can_backup_dev_then_prod_after_the_first_settles() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "bak-archive-pair").await;
    let dev = insert_database(&kernel, &fixture, "dev").await;
    let prod = insert_database(&kernel, &fixture, "prod").await;
    let first = Uuid::new_v4();
    fixture
        .databases
        .begin_backup(fixture.owner, dev, first, &hash("dev"))
        .await
        .expect("dev backup");
    fixture
        .databases
        .complete_backup(dev, first)
        .await
        .expect("settle dev");
    fixture
        .databases
        .begin_backup(fixture.owner, prod, Uuid::new_v4(), &hash("prod"))
        .await
        .expect("prod backup after dev settled");
}

#[tokio::test]
async fn fabric_unknown_backup_releases_admission_for_a_new_operation() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "bak-unknown").await;
    let database_id = insert_database(&kernel, &fixture, "dev").await;
    let first = Uuid::new_v4();
    fixture
        .databases
        .begin_backup(fixture.owner, database_id, first, &hash("a"))
        .await
        .expect("first backup");
    fixture
        .databases
        .unknown_backup(database_id, first)
        .await
        .expect("settle unknown");
    fixture
        .databases
        .begin_backup(fixture.owner, database_id, Uuid::new_v4(), &hash("b"))
        .await
        .expect("second backup after unknown");
}

#[tokio::test]
async fn fabric_unknown_snapshot_releases_the_workspace_operation() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "snap-unknown").await;
    let first = fixture
        .databases
        .begin_workspace_snapshot(fixture.workspace, "manual", None)
        .await
        .expect("first snapshot");
    fixture
        .databases
        .unknown_workspace_snapshot(fixture.workspace, first)
        .await
        .expect("settle unknown");
    let second = fixture
        .databases
        .begin_workspace_snapshot(fixture.workspace, "manual", None)
        .await
        .expect("second snapshot after unknown");
    assert_ne!(first, second);
}

#[tokio::test]
async fn application_delete_reclaims_only_archive_workspace_snapshots() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "bak-ws-own").await;
    let archive_snapshot = Uuid::new_v4();
    sqlx::query(
        "insert into workspace_snapshots \
         (id, workspace_id, object_key, content_hash, byte_length, kind, pinned) \
         values ($1, $2, $3, $4, 16, 'archive', true)",
    )
    .bind(archive_snapshot)
    .bind(fixture.workspace)
    .bind(format!(
        "backups/workspaces/{}/{}.tar.zst",
        fixture.workspace, archive_snapshot
    ))
    .bind(hash("archive-a").as_slice())
    .execute(kernel.pool())
    .await
    .expect("archive snapshot");
    sqlx::query(
        "insert into application_archives \
         (id, application_id, generation, state, workspace_snapshot_id) \
         values ($1, $2, 1, 'complete', $3)",
    )
    .bind(Uuid::new_v4())
    .bind(fixture.application_id)
    .bind(archive_snapshot)
    .execute(kernel.pool())
    .await
    .expect("archive generation");
    sqlx::query("update applications set state = 'deleting' where id = $1")
        .bind(fixture.application_id)
        .execute(kernel.pool())
        .await
        .expect("fence deleting");
    let live_snapshot = Uuid::new_v4();
    sqlx::query(
        "insert into workspace_snapshots \
         (id, workspace_id, object_key, content_hash, byte_length, kind, pinned) \
         values ($1, $2, $3, $4, 16, 'manual', false)",
    )
    .bind(live_snapshot)
    .bind(fixture.workspace)
    .bind(format!(
        "backups/workspaces/{}/{}.tar.zst",
        fixture.workspace, live_snapshot
    ))
    .bind(hash("manual-b").as_slice())
    .execute(kernel.pool())
    .await
    .expect("replacement snapshot");
    let keys = fixture
        .databases
        .list_application_recovery_keys(fixture.application_id)
        .await
        .expect("keys");
    assert_eq!(keys.len(), 1);
    assert!(keys[0].contains(&archive_snapshot.to_string()));
    assert!(
        !keys
            .iter()
            .any(|key| key.contains(&live_snapshot.to_string()))
    );
}

#[tokio::test]
async fn member_can_create_a_normal_dev_database_without_manage_production() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "db-member-dev").await;
    let member = insert_user(&kernel, "db-member").await;
    let project_id: Uuid = sqlx::query_scalar("select project_id from applications where id = $1")
        .bind(fixture.application_id)
        .fetch_one(kernel.pool())
        .await
        .expect("project");
    sqlx::query(
        "insert into project_members (project_id, user_id, role) values ($1, $2, 'member')",
    )
    .bind(project_id)
    .bind(member)
    .execute(kernel.pool())
    .await
    .expect("member");
    let dev: Uuid = sqlx::query_scalar(
        "select id from application_environments where application_id = $1 and kind = 'dev'",
    )
    .bind(fixture.application_id)
    .fetch_one(kernel.pool())
    .await
    .expect("dev env");
    let prod: Uuid = sqlx::query_scalar(
        "select id from application_environments where application_id = $1 and kind = 'prod'",
    )
    .bind(fixture.application_id)
    .fetch_one(kernel.pool())
    .await
    .expect("prod env");
    let fabric: Uuid = sqlx::query_scalar("select fabric_id from workspaces where id = $1")
        .bind(fixture.workspace)
        .fetch_one(kernel.pool())
        .await
        .expect("fabric");
    fixture
        .databases
        .create_with_tier(
            member,
            dev,
            fabric,
            Uuid::new_v4(),
            &hash("dev-create"),
            false,
            None,
        )
        .await
        .expect("member DeployDev creates a normal dev database");
    let prod_err = fixture
        .databases
        .create_with_tier(
            member,
            prod,
            fabric,
            Uuid::new_v4(),
            &hash("prod-create"),
            false,
            None,
        )
        .await
        .expect_err("member cannot create prod database");
    assert!(matches!(prod_err, ApplicationError::Auth));
}

#[tokio::test]
async fn concurrent_disable_wins_user_lock_before_snapshot_claim() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "snap-race").await;
    let mut hold = kernel.pool().begin().await.expect("hold");
    let locked: Uuid = sqlx::query_scalar("select id from users where id = $1 for update")
        .bind(fixture.owner)
        .fetch_one(&mut *hold)
        .await
        .expect("lock user");
    assert_eq!(locked, fixture.owner);
    let owner = fixture.owner;
    let workspace = fixture.workspace;
    let application_id = fixture.application_id;
    let pool = kernel.pool().clone();
    let task = tokio::spawn(async move {
        DatabaseStore::new(pool)
            .accept_manual_workspace_snapshot(owner, workspace, application_id)
            .await
    });
    sqlx::query("update users set status = 'disabled' where id = $1")
        .bind(fixture.owner)
        .execute(&mut *hold)
        .await
        .expect("disable");
    hold.commit().await.expect("commit disable");
    let error = task
        .await
        .expect("join")
        .expect_err("snapshot claim must lose to disable");
    assert!(matches!(error, ApplicationError::Auth));
}

#[tokio::test]
async fn concurrent_disable_wins_user_lock_before_grow_claim() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "grow-race").await;
    sqlx::query("update workspaces set allocated_bytes = $2 where id = $1")
        .bind(fixture.workspace)
        .bind(voie_cloud::storage::WORKSPACE_LARGE_BYTES)
        .execute(kernel.pool())
        .await
        .expect("large workspace");
    let project_id: Uuid = sqlx::query_scalar("select project_id from applications where id = $1")
        .bind(fixture.application_id)
        .fetch_one(kernel.pool())
        .await
        .expect("project");
    let target = voie_cloud::applications::ApprovalTarget {
        application_id: Some(fixture.application_id),
        ..Default::default()
    };
    let pending = match voie_cloud::applications::require_approval(
        kernel.pool(),
        None,
        project_id,
        "increase_resource_tier",
        &target,
        fixture.owner,
    )
    .await
    {
        Err(ApplicationError::ApprovalRequired(id)) => id,
        other => panic!("expected pending approval, got {other:?}"),
    };
    voie_cloud::applications::accept_approval(kernel.pool(), pending, fixture.owner)
        .await
        .expect("accept grow approval");
    let mut hold = kernel.pool().begin().await.expect("hold");
    sqlx::query_scalar::<_, Uuid>("select id from users where id = $1 for update")
        .bind(fixture.owner)
        .fetch_one(&mut *hold)
        .await
        .expect("lock user");
    let owner = fixture.owner;
    let workspace = fixture.workspace;
    let pool = kernel.pool().clone();
    let task = tokio::spawn(async move {
        ApplicationStore::new(pool, "console.test".into())
            .accept_elevated_workspace_grow(
                owner,
                workspace,
                pending,
                voie_cloud::storage::WORKSPACE_ELEVATED_BYTES,
            )
            .await
    });
    sqlx::query("update users set status = 'disabled' where id = $1")
        .bind(fixture.owner)
        .execute(&mut *hold)
        .await
        .expect("disable");
    hold.commit().await.expect("commit disable");
    let error = task
        .await
        .expect("join")
        .expect_err("grow claim must lose to disable");
    assert!(matches!(error, ApplicationError::Auth));
}

#[tokio::test]
async fn concurrent_disable_wins_user_lock_before_delete_claim() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "del-race").await;
    let project_id: Uuid = sqlx::query_scalar("select project_id from applications where id = $1")
        .bind(fixture.application_id)
        .fetch_one(kernel.pool())
        .await
        .expect("project");
    let target = voie_cloud::applications::ApprovalTarget {
        application_id: Some(fixture.application_id),
        ..Default::default()
    };
    let pending = match voie_cloud::applications::require_approval(
        kernel.pool(),
        None,
        project_id,
        "delete_application",
        &target,
        fixture.owner,
    )
    .await
    {
        Err(ApplicationError::ApprovalRequired(id)) => id,
        other => panic!("expected pending approval, got {other:?}"),
    };
    voie_cloud::applications::accept_approval(kernel.pool(), pending, fixture.owner)
        .await
        .expect("accept delete approval");
    let mut hold = kernel.pool().begin().await.expect("hold");
    sqlx::query_scalar::<_, Uuid>("select id from users where id = $1 for update")
        .bind(fixture.owner)
        .fetch_one(&mut *hold)
        .await
        .expect("lock user");
    let owner = fixture.owner;
    let application_id = fixture.application_id;
    let pool = kernel.pool().clone();
    let task = tokio::spawn(async move {
        ApplicationStore::new(pool, "console.test".into())
            .plan_delete(owner, application_id, Some(pending))
            .await
    });
    sqlx::query("update users set status = 'disabled' where id = $1")
        .bind(fixture.owner)
        .execute(&mut *hold)
        .await
        .expect("disable");
    hold.commit().await.expect("commit disable");
    let error = task
        .await
        .expect("join")
        .expect_err("delete claim must lose to disable");
    assert!(matches!(error, ApplicationError::Auth));
    let state: String = sqlx::query_scalar("select state from applications where id = $1")
        .bind(fixture.application_id)
        .fetch_one(kernel.pool())
        .await
        .expect("state");
    assert_ne!(state, "deleting");
}
