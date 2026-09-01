//! Environment secret bindings. Values stay in the existing secret backend.

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::applications::{self, ApplicationError};
use crate::auth::Action;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub environment_id: Uuid,
    pub secret_id: Uuid,
    pub environment_name: String,
    pub binding_revision: i64,
}

#[derive(Clone)]
pub struct BindingStore {
    pool: PgPool,
}

impl BindingStore {
    pub fn new(pool: PgPool) -> Self {
        BindingStore { pool }
    }

    pub async fn list(
        &self,
        actor_user_id: Uuid,
        environment_id: Uuid,
    ) -> Result<Vec<Binding>, ApplicationError> {
        let environment = applications::load_environment(&self.pool, environment_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        applications::ApplicationStore::new(self.pool.clone(), String::new())
            .require_in_project(
                actor_user_id,
                environment.application_id,
                Action::ReadProject,
            )
            .await?;
        let rows = sqlx::query(
            "select environment_id, secret_id, environment_name, binding_revision \
             from environment_secret_bindings where environment_id = $1 \
             order by environment_name",
        )
        .bind(environment_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_binding).collect())
    }

    pub async fn bind(
        &self,
        actor_user_id: Uuid,
        environment_id: Uuid,
        name: &str,
        secret_id: Uuid,
        approval_id: Option<Uuid>,
    ) -> Result<Binding, ApplicationError> {
        let environment = applications::load_environment(&self.pool, environment_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let action = if environment.kind == "prod" {
            Action::ManageProduction
        } else {
            Action::DeployDev
        };
        let (application, _) =
            applications::ApplicationStore::new(self.pool.clone(), String::new())
                .require_in_project(actor_user_id, environment.application_id, action)
                .await?;
        let name = name.trim();
        if name.is_empty() || name.len() > 128 {
            return Err(ApplicationError::InvalidName);
        }
        let owned: bool = sqlx::query_scalar(
            "select exists(select 1 from user_secrets where id = $1 and scope_id = $2)",
        )
        .bind(secret_id)
        .bind(application.project_id)
        .fetch_one(&self.pool)
        .await?;
        if !owned {
            return Err(ApplicationError::NotFound);
        }
        if environment.kind == "prod" {
            applications::require_approval(
                &self.pool,
                approval_id,
                application.project_id,
                "bind_production_secret",
                &applications::ApprovalTarget {
                    application_id: Some(application.id),
                    environment_id: Some(environment.id),
                    secret_id: Some(secret_id),
                    environment_name: Some(name.to_owned()),
                    ..Default::default()
                },
                actor_user_id,
            )
            .await?;
        }
        sqlx::query(
            "insert into environment_secret_bindings \
             (environment_id, secret_id, environment_name, binding_revision) \
             values ($1, $2, $3, 1) \
             on conflict (environment_id, environment_name) do update \
             set secret_id = excluded.secret_id, binding_revision = environment_secret_bindings.binding_revision + 1",
        )
        .bind(environment_id)
        .bind(secret_id)
        .bind(name)
        .execute(&self.pool)
        .await?;
        sqlx::query("update application_environments set revision = revision + 1 where id = $1")
            .bind(environment_id)
            .execute(&self.pool)
            .await?;
        self.list(actor_user_id, environment_id)
            .await?
            .into_iter()
            .find(|binding| binding.environment_name == name)
            .ok_or(ApplicationError::NotFound)
    }

    pub async fn list_internal(
        &self,
        environment_id: Uuid,
    ) -> Result<Vec<Binding>, ApplicationError> {
        let rows = sqlx::query(
            "select environment_id, secret_id, environment_name, binding_revision \
             from environment_secret_bindings where environment_id = $1 \
             order by environment_name",
        )
        .bind(environment_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_binding).collect())
    }

    pub async fn unbind(
        &self,
        actor_user_id: Uuid,
        environment_id: Uuid,
        name: &str,
    ) -> Result<(), ApplicationError> {
        let environment = applications::load_environment(&self.pool, environment_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let action = if environment.kind == "prod" {
            Action::ManageProduction
        } else {
            Action::DeployDev
        };
        applications::ApplicationStore::new(self.pool.clone(), String::new())
            .require_in_project(actor_user_id, environment.application_id, action)
            .await?;
        sqlx::query(
            "delete from environment_secret_bindings where environment_id = $1 and environment_name = $2",
        )
        .bind(environment_id)
        .bind(name)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn row_binding(row: sqlx::postgres::PgRow) -> Binding {
    Binding {
        environment_id: row.get("environment_id"),
        secret_id: row.get("secret_id"),
        environment_name: row.get("environment_name"),
        binding_revision: row.get("binding_revision"),
    }
}
