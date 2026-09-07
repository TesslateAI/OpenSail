//! Focused contracts for project collaboration scopes.
//!
//! The 0007 migration is an upgrade, not a fresh-schema default: old projects
//! with only their owner remain `personal`, while a project that already has a
//! collaborator becomes `team`. The same migration gives legacy workspaces a
//! durable creator (the project owner). User provisioning must also be
//! idempotent: one canonical User has exactly one personal scope.

use std::path::{Path, PathBuf};

use sqlx::postgres::PgConnection;
use sqlx::{Connection, Executor};
use uuid::Uuid;
use voie_cloud::{Config, Kernel};

fn database_url() -> String {
    std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database")
}

fn migration_path(version: u32) -> PathBuf {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let prefix = format!("{version:04}_");
    std::fs::read_dir(&directory)
        .expect("migration directory exists")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".sql"))
        })
        .unwrap_or_else(|| panic!("migration {version:04} exists"))
}

fn migration_sql(version: u32) -> String {
    std::fs::read_to_string(migration_path(version)).expect("migration is readable")
}

/// Applies the pre-scope schema in one isolated transaction. This models a
/// database that was upgraded through 0006 before the scope migration runs,
/// without mutating the shared test database or relying on migration order in
/// another integration test.
async fn legacy_connection() -> (PgConnection, String) {
    let mut connection = PgConnection::connect(&database_url())
        .await
        .expect("PostgreSQL connection succeeds");
    let schema = format!("scope_contract_{}", Uuid::new_v4().simple());
    connection
        .execute(format!("create schema {schema}").as_str())
        .await
        .expect("isolated schema creates");
    connection
        .execute("begin")
        .await
        .expect("legacy transaction begins");
    connection
        .execute(format!("set local search_path to {schema}").as_str())
        .await
        .expect("isolated search path applies");
    connection
        .execute("create table schema_migrations (version bigint primary key)")
        .await
        .expect("migration ledger creates");

    for version in 1..=6 {
        sqlx::raw_sql(&migration_sql(version))
            .execute(&mut connection)
            .await
            .unwrap_or_else(|error| panic!("legacy migration {version} applies: {error}"));
        sqlx::query("insert into schema_migrations (version) values ($1)")
            .bind(i64::from(version))
            .execute(&mut connection)
            .await
            .unwrap_or_else(|error| panic!("legacy migration {version} records: {error}"));
    }
    (connection, schema)
}

#[tokio::test]
async fn legacy_projects_and_workspaces_are_classified_by_0007() {
    let (mut connection, schema) = legacy_connection().await;
    let owner = Uuid::new_v4();
    let collaborator = Uuid::new_v4();
    let personal_project = Uuid::new_v4();
    let team_project = Uuid::new_v4();
    let fabric = Uuid::new_v4();
    let legacy_workspace = Uuid::new_v4();

    for (user_id, subject) in [(owner, "owner"), (collaborator, "collaborator")] {
        sqlx::query("insert into users (id, issuer, subject) values ($1, $2, $3)")
            .bind(user_id)
            .bind("scope-contract")
            .bind(subject)
            .execute(&mut connection)
            .await
            .expect("legacy user inserts");
    }
    sqlx::query("insert into projects (id, owner_user_id, name) values ($1, $2, $3), ($4, $2, $5)")
        .bind(personal_project)
        .bind(owner)
        .bind("legacy-personal")
        .bind(team_project)
        .bind("legacy-team")
        .execute(&mut connection)
        .await
        .expect("legacy project inserts");
    sqlx::query(
        "insert into project_members (project_id, user_id, role) \
         values ($1, $2, 'owner'), ($3, $2, 'owner'), ($3, $4, 'member')",
    )
    .bind(personal_project)
    .bind(owner)
    .bind(team_project)
    .bind(collaborator)
    .execute(&mut connection)
    .await
    .expect("legacy membership inserts");
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind("legacy-fabric")
        .execute(&mut connection)
        .await
        .expect("legacy fabric inserts");
    sqlx::query("insert into workspaces (id, project_id, fabric_id, observed_state) values ($1, $2, $3, 'ready')")
        .bind(legacy_workspace)
        .bind(team_project)
        .bind(fabric)
        .execute(&mut connection)
        .await
        .expect("legacy workspace inserts");

    sqlx::raw_sql(&migration_sql(7))
        .execute(&mut connection)
        .await
        .expect("scope migration applies to the legacy schema");

    let personal_kind: String = sqlx::query_scalar("select kind from projects where id = $1")
        .bind(personal_project)
        .fetch_one(&mut connection)
        .await
        .expect("personal project kind query succeeds");
    let team_kind: String = sqlx::query_scalar("select kind from projects where id = $1")
        .bind(team_project)
        .fetch_one(&mut connection)
        .await
        .expect("team project kind query succeeds");
    assert_eq!(
        personal_kind, "personal",
        "one-member legacy projects stay personal"
    );
    assert_eq!(
        team_kind, "team",
        "collaborator legacy projects become team"
    );

    let creator: Uuid =
        sqlx::query_scalar("select created_by_user_id from workspaces where id = $1")
            .bind(legacy_workspace)
            .fetch_one(&mut connection)
            .await
            .expect("legacy workspace creator is queryable");
    assert_eq!(
        creator, owner,
        "legacy workspace ownership is attributed to its project owner"
    );

    connection
        .execute("rollback")
        .await
        .expect("legacy transaction rolls back");
    connection
        .execute(format!("drop schema {schema} cascade").as_str())
        .await
        .expect("isolated schema drops");
}

#[tokio::test]
async fn provisioning_one_user_is_idempotent_about_the_personal_scope() {
    let kernel = Kernel::connect(&Config::database_url(database_url()))
        .await
        .expect("PostgreSQL connection succeeds");
    kernel.migrate().await.expect("latest migration applies");

    let user_id = Uuid::new_v4();
    sqlx::query("insert into users (id, issuer, subject) values ($1, $2, $3)")
        .bind(user_id)
        .bind("scope-contract")
        .bind("scope-owner")
        .execute(kernel.pool())
        .await
        .expect("canonical User inserts");

    let first = kernel
        .ensure_personal_project(user_id)
        .await
        .expect("first personal scope is ensured");
    let second = kernel
        .ensure_personal_project(user_id)
        .await
        .expect("repeated personal scope ensure succeeds");
    assert_eq!(
        first.id, second.id,
        "one User resolves to one canonical personal scope"
    );

    let personal_scopes: i64 = sqlx::query_scalar(
        "select count(*) from projects where owner_user_id = $1 and kind = 'personal'",
    )
    .bind(user_id)
    .fetch_one(kernel.pool())
    .await
    .expect("personal scope count succeeds");
    assert_eq!(
        personal_scopes, 1,
        "each User receives exactly one personal project scope"
    );
    let owner_memberships: i64 = sqlx::query_scalar(
        "select count(*) from project_members where user_id = $1 and role = 'owner'",
    )
    .bind(user_id)
    .fetch_one(kernel.pool())
    .await
    .expect("personal owner membership count succeeds");
    assert_eq!(
        owner_memberships, 1,
        "the personal scope has one durable owner membership"
    );
}
