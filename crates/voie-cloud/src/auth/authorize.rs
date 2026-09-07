//! Fixed Project membership roles and typed resource actions.

use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Frozen Release 0 Project roles. `Admin` is the team-style management
/// role; the durable project owner stays `Owner` and remains protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Owner,
    Admin,
    Member,
    Viewer,
}

impl Role {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "owner" => Some(Role::Owner),
            "admin" => Some(Role::Admin),
            "member" => Some(Role::Member),
            "viewer" => Some(Role::Viewer),
            _ => None,
        }
    }

    /// Roles that ordinary and platform-admin membership mutation APIs may
    /// assign. `owner` is never writable through those routes.
    pub fn parse_writable(value: &str) -> Option<Self> {
        match value {
            "admin" => Some(Role::Admin),
            "member" => Some(Role::Member),
            "viewer" => Some(Role::Viewer),
            _ => None,
        }
    }

    /// Strict permission rank used for downgrade fencing. Owner is highest
    /// and is never changed by membership mutation APIs.
    pub fn rank(self) -> u8 {
        match self {
            Role::Owner => 3,
            Role::Admin => 2,
            Role::Member => 1,
            Role::Viewer => 0,
        }
    }

    pub fn permits(self, action: Action) -> bool {
        match (self, action) {
            (_, Action::ReadProject) => true,
            (Role::Viewer, _) => false,
            (Role::Member, Action::OperateSession | Action::DeployDev) => true,
            (Role::Member, _) => false,
            (
                Role::Admin | Role::Owner,
                Action::OperateSession
                | Action::DeployDev
                | Action::ManageMembership
                | Action::ManageProduction,
            ) => true,
            (Role::Admin, Action::DestroyApplication) => false,
            (Role::Owner, Action::DestroyApplication) => true,
        }
    }
}

/// Typed actions over a Project-owned resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Read a Project-owned resource.
    ReadProject,
    /// Operate a Session or Workspace in the Project.
    OperateSession,
    /// Manage Project membership.
    ManageMembership,
    /// Build Releases and deploy private development previews.
    DeployDev,
    /// Production publication, visibility, Databases, and production secrets.
    ManageProduction,
    /// Destructive Application or Database deletion.
    DestroyApplication,
}

impl Action {
    /// Stable product name for structured errors and capability text.
    pub fn name(self) -> &'static str {
        match self {
            Action::ReadProject => "ReadProject",
            Action::OperateSession => "OperateSession",
            Action::ManageMembership => "ManageMembership",
            Action::DeployDev => "DeployDev",
            Action::ManageProduction => "ManageProduction",
            Action::DestroyApplication => "DestroyApplication",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ReadProject" => Some(Action::ReadProject),
            "OperateSession" => Some(Action::OperateSession),
            "ManageMembership" => Some(Action::ManageMembership),
            "DeployDev" => Some(Action::DeployDev),
            "ManageProduction" => Some(Action::ManageProduction),
            "DestroyApplication" => Some(Action::DestroyApplication),
            _ => None,
        }
    }
}

/// Authorize `user_id` to perform `action` on the Project named by `project_id`.
///
/// Requires an active User and a current `project_members` row whose frozen
/// role permits the action. A disabled User is denied even if a membership
/// row remains.
pub async fn authorize(
    pool: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
    action: Action,
) -> Result<Role, super::AuthError> {
    let status: Option<String> = sqlx::query_scalar("select status from users where id = $1")
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map_err(|_| super::AuthError::Database)?;
    if status.as_deref() != Some("active") {
        return Err(super::AuthError::Denied);
    }
    let row =
        sqlx::query("select role from project_members where user_id = $1 and project_id = $2")
            .bind(user_id)
            .bind(project_id)
            .fetch_optional(pool)
            .await
            .map_err(|_| super::AuthError::Database)?;
    let Some(row) = row else {
        return Err(super::AuthError::Denied);
    };
    let role = Role::parse(row.get("role")).ok_or(super::AuthError::Denied)?;
    if role.permits(action) {
        Ok(role)
    } else {
        Err(super::AuthError::MissingAction(action))
    }
}
