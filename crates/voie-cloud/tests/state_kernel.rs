use std::sync::Arc;

use sqlx::Row;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;
use voie_cloud::{Config, Kernel, KernelError, serve};

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
    sqlx::query("insert into workspaces (id, project_id, fabric_id) values ($1, $2, $3)")
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
