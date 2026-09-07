//! Focused authorization contracts for project roles and Workspace ownership.
//!
//! Roles are fixed project membership, not a platform capability graph. A
//! member may read and operate Sessions (including creating and using a
//! Workspace), but only an owner or admin may manage membership or another
//! member's Workspace. A viewer can read only. Workspace creator identity is
//! durable so the mutation route can enforce the creator boundary after a
//! restart.

use sqlx::Row;
use uuid::Uuid;
use voie_cloud::auth::{authorize, Action, AuthError, Role};
use voie_cloud::{Config, Kernel};

fn database_url() -> String {
    std::env::var("VOIE_TEST_DATABASE_URL")
        .expect("VOIE_TEST_DATABASE_URL points at an ephemeral PostgreSQL database")
}

#[test]
fn fixed_project_roles_expose_only_their_contractual_actions() {
    let expectations = [
        (
            Role::Owner,
            [true, true, true],
            "owner manages membership and all project resources",
        ),
        (
            Role::Admin,
            [true, true, true],
            "admin manages membership and all project resources",
        ),
        (
            Role::Member,
            [true, true, false],
            "member operates sessions but cannot manage membership",
        ),
        (Role::Viewer, [true, false, false], "viewer is read-only"),
    ];
    for (role, [read, operate, manage], reason) in expectations {
        assert_eq!(role.permits(Action::ReadProject), read, "{reason}");
        assert_eq!(role.permits(Action::OperateSession), operate, "{reason}");
        assert_eq!(role.permits(Action::ManageMembership), manage, "{reason}");
    }
}

#[tokio::test]
async fn workspace_creator_is_durable_and_member_mutations_are_creator_scoped() {
    let kernel = Kernel::connect(&Config::database_url(database_url()))
        .await
        .expect("PostgreSQL connection succeeds");
    kernel.migrate().await.expect("latest migration applies");

    let owner = Uuid::new_v4();
    let admin = Uuid::new_v4();
    let member = Uuid::new_v4();
    let other_member = Uuid::new_v4();
    let viewer = Uuid::new_v4();
    let project = Uuid::new_v4();
    let fabric = Uuid::new_v4();
    let member_workspace = Uuid::new_v4();
    let other_workspace = Uuid::new_v4();

    for (user_id, subject) in [
        (owner, "owner"),
        (admin, "admin"),
        (member, "member"),
        (other_member, "other-member"),
        (viewer, "viewer"),
    ] {
        sqlx::query("insert into users (id, issuer, subject) values ($1, $2, $3)")
            .bind(user_id)
            .bind("roles-contract")
            .bind(subject)
            .execute(kernel.pool())
            .await
            .expect("role-contract user inserts");
    }
    sqlx::query("insert into projects (id, owner_user_id, name, kind) values ($1, $2, $3, 'team')")
        .bind(project)
        .bind(owner)
        .bind("workspace-role-contract")
        .execute(kernel.pool())
        .await
        .expect("team project inserts");
    for (user_id, role) in [
        (owner, "owner"),
        (admin, "admin"),
        (member, "member"),
        (other_member, "member"),
        (viewer, "viewer"),
    ] {
        sqlx::query("insert into project_members (project_id, user_id, role) values ($1, $2, $3)")
            .bind(project)
            .bind(user_id)
            .bind(role)
            .execute(kernel.pool())
            .await
            .expect("project membership inserts");
    }
    sqlx::query("insert into fabrics (id, name) values ($1, $2)")
        .bind(fabric)
        .bind("roles-contract-fabric")
        .execute(kernel.pool())
        .await
        .expect("fabric inserts");
    sqlx::query(
        "insert into workspaces \
         (id, project_id, fabric_id, created_by_user_id, observed_state) \
         values ($1, $2, $3, $4, 'ready'), ($5, $2, $3, $6, 'ready')",
    )
    .bind(member_workspace)
    .bind(project)
    .bind(fabric)
    .bind(member)
    .bind(other_workspace)
    .bind(other_member)
    .execute(kernel.pool())
    .await
    .expect("creator-attributed workspaces insert");

    let creators = sqlx::query(
        "select id, created_by_user_id from workspaces \
         where id in ($1, $2) order by id",
    )
    .bind(member_workspace)
    .bind(other_workspace)
    .fetch_all(kernel.pool())
    .await
    .expect("workspace creator query succeeds")
    .into_iter()
    .map(|row| {
        (
            row.get::<Uuid, _>("id"),
            row.get::<Uuid, _>("created_by_user_id"),
        )
    })
    .collect::<Vec<_>>();
    assert_eq!(creators.len(), 2, "both workspaces remain addressable");
    assert!(
        creators.contains(&(member_workspace, member)),
        "the member-created Workspace records its creator"
    );
    assert!(
        creators.contains(&(other_workspace, other_member)),
        "another member's Workspace records its distinct creator"
    );

    // Membership authorization is project-scoped. Member can use Sessions and
    // create/use a Workspace, while viewer can inspect but not operate.
    assert!(matches!(
        authorize(kernel.pool(), member, project, Action::ReadProject).await,
        Ok(Role::Member)
    ));
    assert!(matches!(
        authorize(kernel.pool(), member, project, Action::OperateSession).await,
        Ok(Role::Member)
    ));
    assert!(matches!(
        authorize(kernel.pool(), member, project, Action::ManageMembership).await,
        Err(voie_cloud::auth::AuthError::MissingAction(
            Action::ManageMembership
        ))
    ));
    assert!(matches!(
        authorize(kernel.pool(), viewer, project, Action::ReadProject).await,
        Ok(Role::Viewer)
    ));
    assert!(matches!(
        authorize(kernel.pool(), viewer, project, Action::OperateSession).await,
        Err(voie_cloud::auth::AuthError::MissingAction(
            Action::OperateSession
        ))
    ));

    // The route's creator check must distinguish the member's own row from a
    // different member's row. Owners/admins are not creator-bound.
    for (actor, workspace, expected) in [
        (member, member_workspace, true),
        (member, other_workspace, false),
        (owner, other_workspace, true),
        (admin, member_workspace, true),
        (viewer, member_workspace, false),
    ] {
        let creator: Uuid = sqlx::query_scalar(
            "select created_by_user_id from workspaces where id = $1 and project_id = $2",
        )
        .bind(workspace)
        .bind(project)
        .fetch_one(kernel.pool())
        .await
        .expect("workspace creator lookup succeeds");
        let role: Role = authorize(kernel.pool(), actor, project, Action::ReadProject)
            .await
            .expect("workspace actor remains a project member");
        let can_manage = match role {
            Role::Owner | Role::Admin => true,
            Role::Member => creator == actor,
            Role::Viewer => false,
        };
        assert_eq!(
            can_manage, expected,
            "workspace mutation policy is role plus durable creator, not UUID-only ownership"
        );
    }
}

#[tokio::test]
async fn disabled_user_is_denied_even_with_a_membership_row() {
    let kernel = Kernel::connect(&Config::database_url(database_url()))
        .await
        .expect("PostgreSQL connection succeeds");
    kernel.migrate().await.expect("latest migration applies");

    let owner = Uuid::new_v4();
    let member = Uuid::new_v4();
    let project = Uuid::new_v4();
    for (user_id, subject) in [(owner, "owner"), (member, "member")] {
        sqlx::query("insert into users (id, issuer, subject) values ($1, $2, $3)")
            .bind(user_id)
            .bind("disabled-contract")
            .bind(subject)
            .execute(kernel.pool())
            .await
            .expect("user inserts");
    }
    sqlx::query("insert into projects (id, owner_user_id, name, kind) values ($1, $2, $3, 'team')")
        .bind(project)
        .bind(owner)
        .bind("disabled-project")
        .execute(kernel.pool())
        .await
        .expect("project inserts");
    for (user_id, role) in [(owner, "owner"), (member, "member")] {
        sqlx::query("insert into project_members (project_id, user_id, role) values ($1, $2, $3)")
            .bind(project)
            .bind(user_id)
            .bind(role)
            .execute(kernel.pool())
            .await
            .expect("membership inserts");
    }

    authorize(kernel.pool(), member, project, Action::OperateSession)
        .await
        .expect("active member may operate");

    sqlx::query("update users set status = 'disabled' where id = $1")
        .bind(member)
        .execute(kernel.pool())
        .await
        .expect("disable");
    let denied = authorize(kernel.pool(), member, project, Action::OperateSession).await;
    assert!(
        matches!(denied, Err(AuthError::Denied)),
        "a disabled User cannot operate even while the membership row remains: {denied:?}"
    );
}
