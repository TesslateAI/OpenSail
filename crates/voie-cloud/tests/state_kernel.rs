use std::sync::Arc;

use sqlx::Row;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;
use voie_cloud::{Config, Kernel, KernelError, WorkspaceState, serve};

async fn http_status(port: u16, path: &str) -> u16 {
    let mut stream = TcpStream::connect(("localhost", port))
        .await
        .expect("HTTP listener accepts connections");
    let request = format!("GET {path} HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("HTTP request writes");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("HTTP response reads");
    String::from_utf8_lossy(&response)
        .split_whitespace()
        .nth(1)
        .expect("HTTP status exists")
        .parse()
        .expect("HTTP status is numeric")
}

#[tokio::test]
async fn state_kernel_contract() {
    let database_url = std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database");
    let kernel = Kernel::connect(&Config::database_url(database_url))
        .await
        .expect("PostgreSQL connection succeeds");

    kernel.migrate().await.expect("fresh migration succeeds");
    kernel.migrate().await.expect("repeat migration is safe");

    let tables = sqlx::query(
        "select table_name from information_schema.tables \
         where table_schema = 'public' order by table_name",
    )
    .fetch_all(kernel.pool())
    .await
    .expect("table listing succeeds")
    .into_iter()
    .map(|row| row.get::<String, _>("table_name"))
    .collect::<Vec<_>>();
    for expected in [
        "users",
        "web_sessions",
        "projects",
        "project_members",
        "agents",
        "sessions",
        "runs",
        "fabrics",
        "workspaces",
        "exec_calls",
        "audit_events",
    ] {
        assert!(
            tables.iter().any(|table| table == expected),
            "missing {expected}"
        );
    }
    assert!(kernel.ready().await, "migrated PostgreSQL is ready");

    let owner = Uuid::new_v4();
    sqlx::query("insert into users (id, issuer, subject) values ($1, $2, $3)")
        .bind(owner)
        .bind("test-issuer")
        .bind(owner.to_string())
        .execute(kernel.pool())
        .await
        .expect("test owner inserts");

    let project = kernel
        .create_project(
            Uuid::new_v4(),
            owner,
            &format!("project-{owner}"),
            "personal",
        )
        .await
        .expect("Project creates");
    let owner_members: i64 = sqlx::query_scalar(
        "select count(*) from project_members \
         where project_id = $1 and user_id = $2 and role = 'owner'",
    )
    .bind(project.id)
    .bind(owner)
    .fetch_one(kernel.pool())
    .await
    .expect("membership count reads");
    assert_eq!(
        owner_members, 1,
        "project creation commits its owner membership atomically"
    );

    // A conflicting retry must not disturb the committed pair.
    let duplicate = kernel
        .create_project(project.id, owner, "different-name", "personal")
        .await;
    assert!(matches!(duplicate, Err(KernelError::Conflict)));
    let members_after: i64 =
        sqlx::query_scalar("select count(*) from project_members where project_id = $1")
            .bind(project.id)
            .fetch_one(kernel.pool())
            .await
            .expect("membership count after conflict");
    assert_eq!(members_after, 1);
    assert_eq!(
        kernel.find_project(project.id).await.unwrap(),
        Some(project.clone())
    );

    let fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .expect("test Fabric inserts");
    let workspace = Uuid::new_v4();
    sqlx::query("insert into workspaces (id, project_id, fabric_id, observed_state) values ($1, $2, $3, 'ready')")
        .bind(workspace)
        .bind(project.id)
        .bind(fabric)
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

    let session = kernel
        .create_session(Uuid::new_v4(), project.id, agent, workspace)
        .await
        .expect("Session creates under its Project");
    assert_eq!(
        kernel.find_session(session.id).await.unwrap(),
        Some(session)
    );

    let other_project = kernel
        .create_project(
            Uuid::new_v4(),
            owner,
            &format!("other-project-{owner}"),
            "personal",
        )
        .await
        .expect("second Project creates");
    let invalid = kernel
        .create_session(Uuid::new_v4(), other_project.id, agent, workspace)
        .await;
    assert!(matches!(invalid, Err(KernelError::RelationRefused)));

    let listener = TcpListener::bind("localhost:0")
        .await
        .expect("HTTP listener binds");
    let port = listener
        .local_addr()
        .expect("listener address exists")
        .port();
    let pool = kernel.pool().clone();
    let server = tokio::spawn(serve(listener, Arc::new(kernel)));
    assert_eq!(http_status(port, "/healthz").await, 200);
    assert_eq!(http_status(port, "/readyz").await, 200);

    sqlx::query("delete from schema_migrations where version = $1")
        .bind(1_i64)
        .execute(&pool)
        .await
        .expect("migration marker can be removed for readiness check");
    assert_eq!(http_status(port, "/readyz").await, 503);

    sqlx::query("insert into schema_migrations (version) values ($1)")
        .bind(1_i64)
        .execute(&pool)
        .await
        .expect("migration marker restores readiness");
    assert_eq!(http_status(port, "/readyz").await, 200);

    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn deleted_workspace_identity_is_a_permanent_tombstone() {
    let database_url = std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database");
    let kernel = Kernel::connect(&Config::database_url(database_url))
        .await
        .expect("PostgreSQL connection succeeds");
    kernel.migrate().await.expect("fresh migration succeeds");

    let owner = Uuid::new_v4();
    sqlx::query("insert into users (id, issuer, subject) values ($1, $2, $3)")
        .bind(owner)
        .bind("tombstone-issuer")
        .bind(owner.to_string())
        .execute(kernel.pool())
        .await
        .expect("test owner inserts");
    let project = kernel
        .create_project(
            Uuid::new_v4(),
            owner,
            &format!("tombstone-{owner}"),
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
    let workspace = Uuid::new_v4();
    sqlx::query(
        "insert into workspaces (id, project_id, fabric_id, state, created_by_user_id, observed_state) \
         values ($1, $2, $3, 'creating', $4, 'ready')",
    )
    .bind(workspace)
    .bind(project.id)
    .bind(fabric)
    .bind(owner)
    .execute(kernel.pool())
    .await
    .expect("ready Workspace inserts");

    assert!(
        kernel
            .begin_workspace_delete(workspace)
            .await
            .expect("fence claims"),
        "ready Workspace can be fenced"
    );
    assert!(
        kernel
            .finish_workspace_delete(workspace)
            .await
            .expect("tombstone writes"),
        "fenced Workspace becomes a tombstone"
    );
    let row = kernel
        .find_workspace(workspace)
        .await
        .expect("lookup")
        .expect("tombstone remains");
    assert_eq!(row.state, WorkspaceState::Creating);
    let tombstone = sqlx::query(
        "select desired_state, desired_revision, observed_revision from workspaces where id = $1",
    )
    .bind(workspace)
    .fetch_one(kernel.pool())
    .await
    .expect("tombstone revisions");
    let desired: String = sqlx::Row::get(&tombstone, "desired_state");
    let desired_revision: i64 = sqlx::Row::get(&tombstone, "desired_revision");
    let observed_revision: i64 = sqlx::Row::get(&tombstone, "observed_revision");
    assert_eq!(desired, "deleted");
    assert!(
        desired_revision > observed_revision,
        "tombstone must PUT Deleted before claiming Fabric observed: desired={desired_revision} observed={observed_revision}"
    );

    let recreate = kernel
        .reserve_workspace(workspace, project.id, fabric, owner)
        .await;
    assert!(
        matches!(recreate, Err(KernelError::Conflict)),
        "the same UUID must never start a second lifecycle: {recreate:?}"
    );
    let after = kernel
        .find_workspace(workspace)
        .await
        .expect("lookup")
        .expect("tombstone remains after refused recreate");
    assert_eq!(after.state, WorkspaceState::Creating);
}

#[tokio::test]
async fn creating_workspace_can_be_fenced_and_tombstoned() {
    let database_url = std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database");
    let kernel = Arc::new(
        Kernel::connect(&Config::database_url(database_url))
            .await
            .expect("PostgreSQL connection succeeds"),
    );
    kernel.migrate().await.expect("fresh migration succeeds");

    let owner = Uuid::new_v4();
    sqlx::query("insert into users (id, issuer, subject) values ($1, $2, $3)")
        .bind(owner)
        .bind("creating-delete-issuer")
        .bind(owner.to_string())
        .execute(kernel.pool())
        .await
        .expect("test owner inserts");
    let project = kernel
        .create_project(Uuid::new_v4(), owner, "creating-delete", "personal")
        .await
        .expect("project");
    let fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("creating-delete-fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .expect("test Fabric inserts");
    let workspace = Uuid::new_v4();
    sqlx::query(
        "insert into workspaces (id, project_id, fabric_id, state, created_by_user_id) \
         values ($1, $2, $3, 'creating', $4)",
    )
    .bind(workspace)
    .bind(project.id)
    .bind(fabric)
    .bind(owner)
    .execute(kernel.pool())
    .await
    .expect("creating Workspace inserts");

    assert!(
        kernel
            .begin_workspace_delete(workspace)
            .await
            .expect("fence claims"),
        "creating Workspace can be fenced"
    );
    assert!(
        kernel
            .finish_workspace_delete(workspace)
            .await
            .expect("tombstone writes"),
        "fenced creating Workspace becomes a tombstone"
    );
}

#[tokio::test]
async fn user_workspace_quota_serializes_across_projects() {
    let database_url = std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database");
    let kernel = Arc::new(
        Kernel::connect(&Config::database_url(database_url))
            .await
            .expect("PostgreSQL connection succeeds"),
    );
    kernel.migrate().await.expect("fresh migration succeeds");

    let owner = Uuid::new_v4();
    sqlx::query("insert into users (id, issuer, subject) values ($1, $2, $3)")
        .bind(owner)
        .bind("quota-issuer")
        .bind(owner.to_string())
        .execute(kernel.pool())
        .await
        .expect("test owner inserts");
    let project_a = kernel
        .create_project(Uuid::new_v4(), owner, "quota-a", "personal")
        .await
        .expect("project A");
    let project_b = kernel
        .create_project(Uuid::new_v4(), owner, "quota-b", "personal")
        .await
        .expect("project B");
    let fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .expect("test Fabric inserts");
    for _ in 0..(voie_cloud::MAX_WORKSPACES_PER_USER - 1) {
        kernel
            .reserve_workspace(Uuid::new_v4(), project_a.id, fabric, owner)
            .await
            .expect("fill user quota minus one");
    }

    let kernel_a = kernel.clone();
    let kernel_b = kernel.clone();
    let left = tokio::spawn(async move {
        kernel_a
            .reserve_workspace(Uuid::new_v4(), project_a.id, fabric, owner)
            .await
    });
    let right = tokio::spawn(async move {
        kernel_b
            .reserve_workspace(Uuid::new_v4(), project_b.id, fabric, owner)
            .await
    });
    let results = [left.await.expect("task A"), right.await.expect("task B")];
    let ok = results.iter().filter(|result| result.is_ok()).count();
    let quota = results
        .iter()
        .filter(|result| matches!(result, Err(KernelError::Quota)))
        .count();
    assert_eq!(
        (ok, quota),
        (1, 1),
        "exactly one of two cross-Project creates may take the last user slot: {results:?}"
    );
}

#[tokio::test]
async fn failed_unrealized_workspace_is_reclaimed_on_create() {
    let database_url = std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database");
    let kernel = Arc::new(
        Kernel::connect(&Config::database_url(database_url))
            .await
            .expect("PostgreSQL connection succeeds"),
    );
    kernel.migrate().await.expect("fresh migration succeeds");

    let owner = Uuid::new_v4();
    sqlx::query("insert into users (id, issuer, subject) values ($1, $2, $3)")
        .bind(owner)
        .bind("reclaim-issuer")
        .bind(owner.to_string())
        .execute(kernel.pool())
        .await
        .expect("test owner inserts");
    let project = kernel
        .create_project(Uuid::new_v4(), owner, "reclaim", "personal")
        .await
        .expect("project");
    let fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("reclaim-fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .expect("test Fabric inserts");
    let failed = Uuid::new_v4();
    sqlx::query(
        "insert into workspaces \
         (id, project_id, fabric_id, created_by_user_id, desired_state, observed_state, last_error_code) \
         values ($1, $2, $3, $4, 'active', 'creating', 'fabric_create_failed')",
    )
    .bind(failed)
    .bind(project.id)
    .bind(fabric)
    .bind(owner)
    .execute(kernel.pool())
    .await
    .expect("failed unrealized workspace inserts");

    kernel
        .reserve_workspace(Uuid::new_v4(), project.id, fabric, owner)
        .await
        .expect("failed unrealized rows do not consume occupancy");
    let leftover: Option<Uuid> = sqlx::query_scalar("select id from workspaces where id = $1")
        .bind(failed)
        .fetch_optional(kernel.pool())
        .await
        .expect("leftover lookup");
    assert_eq!(leftover, None, "failed unrealized Workspace is reclaimed");
}

#[tokio::test]
async fn live_guest_still_occupies_after_desired_delete() {
    let database_url = std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database");
    let kernel = Arc::new(
        Kernel::connect(&Config::database_url(database_url))
            .await
            .expect("PostgreSQL connection succeeds"),
    );
    kernel.migrate().await.expect("fresh migration succeeds");

    let owner = Uuid::new_v4();
    sqlx::query("insert into users (id, issuer, subject) values ($1, $2, $3)")
        .bind(owner)
        .bind("teardown-issuer")
        .bind(owner.to_string())
        .execute(kernel.pool())
        .await
        .expect("test owner inserts");
    let project = kernel
        .create_project(Uuid::new_v4(), owner, "teardown", "personal")
        .await
        .expect("project");
    let fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("teardown-fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .expect("test Fabric inserts");
    let live = Uuid::new_v4();
    sqlx::query(
        "insert into workspaces \
         (id, project_id, fabric_id, created_by_user_id, desired_state, observed_state) \
         values ($1, $2, $3, $4, 'deleted', 'ready')",
    )
    .bind(live)
    .bind(project.id)
    .bind(fabric)
    .bind(owner)
    .execute(kernel.pool())
    .await
    .expect("live tearing-down workspace inserts");
    for _ in 0..(voie_cloud::MAX_WORKSPACES_PER_USER - 1) {
        sqlx::query(
            "insert into workspaces \
             (id, project_id, fabric_id, created_by_user_id, desired_state, observed_state) \
             values ($1, $2, $3, $4, 'deleted', 'ready')",
        )
        .bind(Uuid::new_v4())
        .bind(project.id)
        .bind(fabric)
        .bind(owner)
        .execute(kernel.pool())
        .await
        .expect("additional live teardown inserts");
    }
    let refused = kernel
        .reserve_workspace(Uuid::new_v4(), project.id, fabric, owner)
        .await;
    assert!(
        matches!(refused, Err(KernelError::Quota)),
        "observed-ready guests occupy capacity during teardown: {refused:?}"
    );
}

#[tokio::test]
async fn session_attaches_from_observed_active_not_process_creating() {
    let database_url = std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database");
    let kernel = Arc::new(
        Kernel::connect(&Config::database_url(database_url))
            .await
            .expect("PostgreSQL connection succeeds"),
    );
    kernel.migrate().await.expect("fresh migration succeeds");

    let owner = Uuid::new_v4();
    sqlx::query("insert into users (id, issuer, subject) values ($1, $2, $3)")
        .bind(owner)
        .bind("observed-attach-issuer")
        .bind(owner.to_string())
        .execute(kernel.pool())
        .await
        .expect("test owner inserts");
    let project = kernel
        .create_project(Uuid::new_v4(), owner, "observed-attach", "personal")
        .await
        .expect("project");
    let fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("observed-attach-fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .expect("test Fabric inserts");
    let creating = Uuid::new_v4();
    kernel
        .reserve_workspace(creating, project.id, fabric, owner)
        .await
        .expect("reserve");
    let agent = Uuid::new_v4();
    sqlx::query("insert into agents (id, project_id, name) values ($1, $2, $3)")
        .bind(agent)
        .bind(project.id)
        .bind("observed-attach-agent")
        .execute(kernel.pool())
        .await
        .expect("test Agent inserts");
    let refused = kernel
        .create_session(Uuid::new_v4(), project.id, agent, creating)
        .await;
    assert!(
        matches!(refused, Err(KernelError::RelationRefused)),
        "unobserved creating Workspace must not accept Sessions: {refused:?}"
    );

    let observed = Uuid::new_v4();
    kernel
        .reserve_workspace(observed, project.id, fabric, owner)
        .await
        .expect("reserve observed");
    sqlx::query("update workspaces set observed_state = 'active' where id = $1")
        .bind(observed)
        .execute(kernel.pool())
        .await
        .expect("observe without leftover process ready");
    let session = kernel
        .create_session(Uuid::new_v4(), project.id, agent, observed)
        .await
        .expect("observed active Workspace accepts Sessions without process ready");
    assert_eq!(session.workspace_id, observed);
}

#[tokio::test]
async fn observed_live_creating_workspace_is_not_discarded() {
    let database_url = std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database");
    let kernel = Arc::new(
        Kernel::connect(&Config::database_url(database_url))
            .await
            .expect("PostgreSQL connection succeeds"),
    );
    kernel.migrate().await.expect("fresh migration succeeds");

    let owner = Uuid::new_v4();
    sqlx::query("insert into users (id, issuer, subject) values ($1, $2, $3)")
        .bind(owner)
        .bind("unrealized-discard-issuer")
        .bind(owner.to_string())
        .execute(kernel.pool())
        .await
        .expect("test owner inserts");
    let project = kernel
        .create_project(Uuid::new_v4(), owner, "unrealized-discard", "personal")
        .await
        .expect("project");
    let fabric = Uuid::new_v4();
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind(format!("unrealized-discard-fabric-{fabric}"))
        .execute(kernel.pool())
        .await
        .expect("test Fabric inserts");

    let unrealized = Uuid::new_v4();
    kernel
        .reserve_workspace(unrealized, project.id, fabric, owner)
        .await
        .expect("reserve unrealized");
    assert!(
        kernel
            .delete_workspace(unrealized)
            .await
            .expect("discard unrealized"),
        "never-observed leftover creating may be discarded"
    );

    let live = Uuid::new_v4();
    kernel
        .reserve_workspace(live, project.id, fabric, owner)
        .await
        .expect("reserve live");
    sqlx::query("update workspaces set observed_state = 'active' where id = $1")
        .bind(live)
        .execute(kernel.pool())
        .await
        .expect("observe live");
    assert!(
        !kernel
            .delete_workspace(live)
            .await
            .expect("refuse realized discard"),
        "observed live must not be discarded because leftover process is creating"
    );
    let kept = kernel
        .find_workspace(live)
        .await
        .expect("lookup")
        .expect("realized row remains");
    assert_eq!(kept.state, WorkspaceState::Creating);
}
