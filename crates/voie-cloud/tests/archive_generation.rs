//! Archive generations: a second archive after restore must capture new
//! Workspace, Database, and Release restore points, never the first
//! generation's pointers.

use tokio::sync::Mutex;
use uuid::Uuid;
use voie_cloud::applications::{accept_approval, ApplicationError, ApplicationStore};
use voie_cloud::databases::DatabaseStore;
use voie_cloud::http::Platform;
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

fn marker_hash(label: &str) -> [u8; 32] {
    let mut hash = [0u8; 32];
    let bytes = label.as_bytes();
    hash[..bytes.len().min(32)].copy_from_slice(&bytes[..bytes.len().min(32)]);
    hash
}

struct Fixture {
    owner: Uuid,
    application_id: Uuid,
    workspace: Uuid,
    store: ApplicationStore,
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
        "insert into workspaces (id, project_id, fabric_id, state, created_by_user_id, exec_generation) \
         values ($1, $2, $3, 'ready', $4, 1)",
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
        .create(
            owner,
            project.id,
            workspace,
            "Archive App",
            &format!("arc-{}", Uuid::new_v4().simple()),
            None,
        )
        .await
        .expect("application");
    Fixture {
        owner,
        application_id: created.application.id,
        workspace,
        store,
    }
}

async fn insert_snapshot(kernel: &Kernel, workspace: Uuid, marker: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "insert into workspace_snapshots \
         (id, workspace_id, object_key, content_hash, byte_length, kind, pinned) \
         values ($1, $2, $3, $4, 16, 'archive', true)",
    )
    .bind(id)
    .bind(workspace)
    .bind(format!("backups/workspaces/{workspace}/{id}.tar.zst"))
    .bind(marker_hash(marker).as_slice())
    .execute(kernel.pool())
    .await
    .expect("snapshot");
    id
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
         (id, application_id, environment_id, engine_profile, fabric_id, state, storage_bytes) \
         values ($1, $2, $3, 'voie-postgres:v1', $4, 'ready', 8589934592)",
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

async fn insert_backup(kernel: &Kernel, database_id: Uuid, marker: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "insert into database_backups \
         (id, database_id, object_key, content_hash, byte_length, kind, pinned) \
         values ($1, $2, $3, $4, 32, 'archive', true)",
    )
    .bind(id)
    .bind(database_id)
    .bind(format!("backups/databases/{database_id}/{id}.pgdump"))
    .bind(marker_hash(marker).as_slice())
    .execute(kernel.pool())
    .await
    .expect("backup");
    id
}

async fn insert_release(kernel: &Kernel, fixture: &Fixture, marker: &str) -> Uuid {
    let id = Uuid::new_v4();
    let mut hash = marker_hash(marker);
    hash[31] = id.as_bytes()[15];
    sqlx::query(
        "insert into application_releases (
            id, application_id, build_intent_id, request_hash, source_workspace_id,
            source_exec_generation, runtime_profile, manifest, manifest_hash,
            state, created_by_user_id
         ) values (
            $1, $2, $3, $4, $5, 1, 'universal-v1', '{}'::jsonb, $6, 'ready', $7
         )",
    )
    .bind(id)
    .bind(fixture.application_id)
    .bind(Uuid::new_v4())
    .bind(hash.as_slice())
    .bind(fixture.workspace)
    .bind(hash.as_slice())
    .bind(fixture.owner)
    .execute(kernel.pool())
    .await
    .expect("release");
    id
}

async fn archive_generation(
    store: &ApplicationStore,
    owner: Uuid,
    application_id: Uuid,
    snapshot: Uuid,
    dev_backup: Uuid,
    prod_backup: Uuid,
    dev_release: Uuid,
    prod_release: Uuid,
) {
    let phase = store
        .begin_archive(owner, application_id)
        .await
        .expect("begin archive");
    assert_eq!(phase, "archiving");
    store
        .persist_archive_restore_points(
            application_id,
            Some(snapshot),
            Some(dev_backup),
            Some(prod_backup),
            Some(dev_release),
            Some(prod_release),
        )
        .await
        .expect("persist capturing generation");
    store
        .commit_archive(
            owner,
            application_id,
            Some(snapshot),
            Some(dev_backup),
            Some(prod_backup),
            Some(dev_release),
            Some(prod_release),
        )
        .await
        .expect("promote archive generation");
}

async fn restore_ready(store: &ApplicationStore, owner: Uuid, application_id: Uuid) {
    let phase = store
        .begin_restore(owner, application_id)
        .await
        .expect("begin restore");
    assert_eq!(phase, "restoring");
    store
        .commit_restore(owner, application_id)
        .await
        .expect("commit restore");
}

#[tokio::test]
async fn rearchive_after_restore_keeps_second_generation_restore_points() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "arch-gen").await;
    let dev_db = insert_database(&kernel, &fixture, "dev").await;
    let prod_db = insert_database(&kernel, &fixture, "prod").await;

    let ws1 = insert_snapshot(&kernel, fixture.workspace, "ws-gen1").await;
    let dev_b1 = insert_backup(&kernel, dev_db, "db-dev-gen1").await;
    let prod_b1 = insert_backup(&kernel, prod_db, "db-prod-gen1").await;
    let dev_r1 = insert_release(&kernel, &fixture, "rel-dev-gen1").await;
    let prod_r1 = insert_release(&kernel, &fixture, "rel-prod-gen1").await;
    archive_generation(
        &fixture.store,
        fixture.owner,
        fixture.application_id,
        ws1,
        dev_b1,
        prod_b1,
        dev_r1,
        prod_r1,
    )
    .await;
    let first = fixture
        .store
        .get_archive(fixture.application_id)
        .await
        .expect("first complete")
        .expect("generation 1");
    assert_eq!(first.generation, 1);
    assert_eq!(first.state, "complete");
    assert_eq!(first.workspace_snapshot_id, Some(ws1));
    assert_eq!(first.dev_database_backup_id, Some(dev_b1));
    assert_eq!(first.prod_database_backup_id, Some(prod_b1));
    assert_eq!(first.dev_release_id, Some(dev_r1));
    assert_eq!(first.prod_release_id, Some(prod_r1));

    restore_ready(&fixture.store, fixture.owner, fixture.application_id).await;

    let ws2 = insert_snapshot(&kernel, fixture.workspace, "ws-gen2").await;
    let dev_b2 = insert_backup(&kernel, dev_db, "db-dev-gen2").await;
    let prod_b2 = insert_backup(&kernel, prod_db, "db-prod-gen2").await;
    let dev_r2 = insert_release(&kernel, &fixture, "rel-dev-gen2").await;
    let prod_r2 = insert_release(&kernel, &fixture, "rel-prod-gen2").await;

    let phase = fixture
        .store
        .begin_archive(fixture.owner, fixture.application_id)
        .await
        .expect("second archive");
    assert_eq!(phase, "archiving");
    let capturing = fixture
        .store
        .capturing_archive(fixture.application_id)
        .await
        .expect("capturing load")
        .expect("generation 2 capturing");
    assert_eq!(capturing.generation, 2);
    assert_eq!(capturing.state, "capturing");
    assert_eq!(capturing.workspace_snapshot_id, None);
    assert_eq!(capturing.dev_database_backup_id, None);
    assert_eq!(capturing.prod_database_backup_id, None);
    assert_eq!(capturing.dev_release_id, None);
    assert_eq!(capturing.prod_release_id, None);
    let still_first = fixture
        .store
        .get_archive(fixture.application_id)
        .await
        .expect("complete during capture")
        .expect("generation 1 still current");
    assert_eq!(still_first.generation, 1);
    assert_eq!(still_first.workspace_snapshot_id, Some(ws1));

    fixture
        .store
        .persist_archive_restore_points(
            fixture.application_id,
            Some(ws2),
            Some(dev_b2),
            Some(prod_b2),
            Some(dev_r2),
            Some(prod_r2),
        )
        .await
        .expect("persist second generation");
    fixture
        .store
        .commit_archive(
            fixture.owner,
            fixture.application_id,
            Some(ws2),
            Some(dev_b2),
            Some(prod_b2),
            Some(dev_r2),
            Some(prod_r2),
        )
        .await
        .expect("promote second generation");
    let second = fixture
        .store
        .get_archive(fixture.application_id)
        .await
        .expect("second complete")
        .expect("generation 2");
    assert_eq!(second.generation, 2);
    assert_eq!(second.state, "complete");
    assert_eq!(second.workspace_snapshot_id, Some(ws2));
    assert_eq!(second.dev_database_backup_id, Some(dev_b2));
    assert_eq!(second.prod_database_backup_id, Some(prod_b2));
    assert_eq!(second.dev_release_id, Some(dev_r2));
    assert_eq!(second.prod_release_id, Some(prod_r2));

    let superseded: i64 = sqlx::query_scalar(
        "select count(*) from application_archives \
         where application_id = $1 and generation = 1 and state = 'superseded'",
    )
    .bind(fixture.application_id)
    .fetch_one(kernel.pool())
    .await
    .expect("superseded count");
    assert_eq!(superseded, 1);

    let gen1_snapshot_pinned: bool =
        sqlx::query_scalar("select pinned from workspace_snapshots where id = $1")
            .bind(ws1)
            .fetch_one(kernel.pool())
            .await
            .expect("gen1 snapshot pin");
    let gen1_dev_pinned: bool =
        sqlx::query_scalar("select pinned from database_backups where id = $1")
            .bind(dev_b1)
            .fetch_one(kernel.pool())
            .await
            .expect("gen1 dev pin");
    let gen2_snapshot_pinned: bool =
        sqlx::query_scalar("select pinned from workspace_snapshots where id = $1")
            .bind(ws2)
            .fetch_one(kernel.pool())
            .await
            .expect("gen2 snapshot pin");
    assert!(
        !gen1_snapshot_pinned,
        "superseded archive snapshot must unpin"
    );
    assert!(!gen1_dev_pinned, "superseded archive backup must unpin");
    assert!(
        gen2_snapshot_pinned,
        "current archive snapshot stays pinned"
    );

    restore_ready(&fixture.store, fixture.owner, fixture.application_id).await;
    let restored = fixture
        .store
        .get_archive(fixture.application_id)
        .await
        .expect("restore target")
        .expect("still generation 2");
    assert_eq!(restored.generation, 2);
    assert_eq!(restored.workspace_snapshot_id, Some(ws2));
    assert_eq!(restored.dev_database_backup_id, Some(dev_b2));
    assert_eq!(restored.prod_database_backup_id, Some(prod_b2));
    assert_eq!(restored.dev_release_id, Some(dev_r2));
    assert_eq!(restored.prod_release_id, Some(prod_r2));
}

#[tokio::test]
async fn archive_retry_reuses_the_same_capturing_generation() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "arch-retry").await;
    let ws1 = insert_snapshot(&kernel, fixture.workspace, "ws-retry").await;
    assert_eq!(
        fixture
            .store
            .begin_archive(fixture.owner, fixture.application_id)
            .await
            .expect("first begin"),
        "archiving"
    );
    fixture
        .store
        .persist_archive_restore_points(fixture.application_id, Some(ws1), None, None, None, None)
        .await
        .expect("partial capture");
    assert_eq!(
        fixture
            .store
            .begin_archive(fixture.owner, fixture.application_id)
            .await
            .expect("retry begin"),
        "archiving"
    );
    let capturing = fixture
        .store
        .capturing_archive(fixture.application_id)
        .await
        .expect("capturing")
        .expect("in progress");
    assert_eq!(capturing.generation, 1);
    assert_eq!(capturing.workspace_snapshot_id, Some(ws1));
    assert_eq!(capturing.dev_database_backup_id, None);
}

#[tokio::test]
async fn disabled_user_cannot_begin_archive() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "arch-disabled").await;
    sqlx::query("update users set status = 'disabled' where id = $1")
        .bind(fixture.owner)
        .execute(kernel.pool())
        .await
        .expect("disable");
    let error = fixture
        .store
        .begin_archive(fixture.owner, fixture.application_id)
        .await
        .expect_err("disabled actor is refused");
    assert!(matches!(error, ApplicationError::Auth));
}

#[tokio::test]
async fn application_restore_requires_restore_application_approval() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "arch-approve").await;
    let ws1 = insert_snapshot(&kernel, fixture.workspace, "ws-approve").await;
    assert_eq!(
        fixture
            .store
            .begin_archive(fixture.owner, fixture.application_id)
            .await
            .expect("begin archive"),
        "archiving"
    );
    fixture
        .store
        .persist_archive_restore_points(fixture.application_id, Some(ws1), None, None, None, None)
        .await
        .expect("persist");
    fixture
        .store
        .commit_archive(
            fixture.owner,
            fixture.application_id,
            Some(ws1),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("commit archive");

    let platform = Platform::new(kernel.pool().clone(), "console.test".into(), None);
    let denied = platform
        .restore_application(fixture.owner, fixture.application_id, None)
        .await
        .expect_err("restore without approval");
    let ApplicationError::ApprovalRequired(pending) = denied else {
        panic!("expected restore_application approval, got {denied:?}");
    };
    let state: String = sqlx::query_scalar("select state from applications where id = $1")
        .bind(fixture.application_id)
        .fetch_one(kernel.pool())
        .await
        .expect("state");
    assert_eq!(state, "archived");
    accept_approval(kernel.pool(), pending, fixture.owner)
        .await
        .expect("accept");
    let restored = platform
        .restore_application(fixture.owner, fixture.application_id, Some(pending))
        .await
        .expect("restore after approval");
    assert_eq!(restored.state, "ready");
}

#[tokio::test]
async fn manual_snapshot_cannot_satisfy_an_archive_snapshot_operation() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "snap-purpose").await;
    let databases = DatabaseStore::new(kernel.pool().clone());
    databases
        .begin_workspace_snapshot(fixture.workspace, "manual", None)
        .await
        .expect("manual in flight");
    let error = databases
        .begin_workspace_snapshot(fixture.workspace, "archive", Some(1))
        .await
        .expect_err("archive must not reuse a manual operation");
    assert!(matches!(error, ApplicationError::WorkspaceBusy));
}

#[tokio::test]
async fn archive_snapshot_cannot_satisfy_a_manual_snapshot_operation() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "snap-purpose-rev").await;
    let databases = DatabaseStore::new(kernel.pool().clone());
    databases
        .begin_workspace_snapshot(fixture.workspace, "archive", Some(1))
        .await
        .expect("archive in flight");
    let error = databases
        .begin_workspace_snapshot(fixture.workspace, "manual", None)
        .await
        .expect_err("manual must not reuse an archive operation");
    assert!(matches!(error, ApplicationError::WorkspaceBusy));
}

#[tokio::test]
async fn archive_retry_reuses_attached_points_and_pins_them() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "arch-incr").await;
    let ws1 = insert_snapshot(&kernel, fixture.workspace, "ws-incr").await;
    sqlx::query("update workspace_snapshots set pinned = false where id = $1")
        .bind(ws1)
        .execute(kernel.pool())
        .await
        .expect("start unpinned");
    fixture
        .store
        .begin_archive(fixture.owner, fixture.application_id)
        .await
        .expect("begin");
    fixture
        .store
        .persist_archive_restore_points(fixture.application_id, Some(ws1), None, None, None, None)
        .await
        .expect("attach snapshot");
    fixture
        .store
        .persist_archive_restore_points(fixture.application_id, None, None, None, None, None)
        .await
        .expect("later persist must not wipe attached snapshot");
    let capturing = fixture
        .store
        .capturing_archive(fixture.application_id)
        .await
        .expect("capturing")
        .expect("in progress");
    assert_eq!(capturing.workspace_snapshot_id, Some(ws1));
    let pinned: bool = sqlx::query_scalar("select pinned from workspace_snapshots where id = $1")
        .bind(ws1)
        .fetch_one(kernel.pool())
        .await
        .expect("pin");
    assert!(
        pinned,
        "archive restore point must be pinned while referenced"
    );
}

#[tokio::test]
async fn archive_retry_reuses_same_generation_snapshot_operation() {
    let kernel = kernel().await;
    let fixture = fixture(&kernel, "snap-arch-reuse").await;
    let databases = DatabaseStore::new(kernel.pool().clone());
    let first = databases
        .begin_workspace_snapshot(fixture.workspace, "archive", Some(2))
        .await
        .expect("first archive op");
    let second = databases
        .begin_workspace_snapshot(fixture.workspace, "archive", Some(2))
        .await
        .expect("retry same generation");
    assert_eq!(first, second);
    let other = databases
        .begin_workspace_snapshot(fixture.workspace, "archive", Some(3))
        .await
        .expect_err("different generation must not alias");
    assert!(matches!(other, ApplicationError::WorkspaceBusy));
}
