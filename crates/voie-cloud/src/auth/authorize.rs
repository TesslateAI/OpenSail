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

    pub fn permits(self, action: Action) -> bool {
        match (self, action) {
            (_, Action::ReadProject) => true,
            (Role::Viewer, _) => false,
            (Role::Member, Action::OperateSession) => true,
            (Role::Member, _) => false,
            (Role::Admin | Role::Owner, Action::OperateSession) => true,
            (Role::Admin | Role::Owner, Action::ManageMembership) => true,
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
}

/// Authorize `user_id` to perform `action` on the Project named by `project_id`.
///
/// Uses only `project_members(user_id, project_id)` and the fixed role permits.
pub async fn authorize(
    pool: &PgPool,
    user_id: Uuid,
    project_id: Uuid,
    action: Action,
) -> Result<Role, super::AuthError> {
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
        Err(super::AuthError::Denied)
    }
}
