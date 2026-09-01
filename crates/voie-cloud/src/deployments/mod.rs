//! Deployment desired-state machine. Fabric realizes; PostgreSQL owns intent.

use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::applications::{self, ApplicationError, Environment};
use crate::auth::Action;
use crate::releases::ReleaseStore;

/// At most one active Deployment plus one candidate per Environment after
/// reservation. `unknown`, `superseded`, and definite `failed` still own
/// Fabric resources, so they consume the same budget until cleanup is
/// positively complete.
pub const MAX_LIVE_DEPLOYMENTS_PER_ENVIRONMENT: i64 = 2;
/// In-flight deploy operations per Project (`accepted`/`materializing`/
/// `starting`/`activating`). Active and unknown rows use the Environment cap.
pub const MAX_CONCURRENT_DEPLOYMENTS_PER_PROJECT: i64 = 2;
/// In-flight deploy operations per actor.
pub const MAX_CONCURRENT_DEPLOYMENTS_PER_USER: i64 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deployment {
    pub id: Uuid,
    pub environment_id: Uuid,
    pub release_id: Uuid,
    pub deployment_intent_id: Uuid,
    pub request_hash: Vec<u8>,
    pub state: String,
    pub desired_revision: i64,
    pub observed_revision: i64,
    pub previous_deployment_id: Option<Uuid>,
    pub created_by_user_id: Uuid,
    pub accepted_at: String,
    pub dispatched_at: Option<String>,
    pub active_at: Option<String>,
    pub terminal_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeginDeployment {
    ReadyToDispatch { id: Uuid },
    Active { id: Uuid },
    Failed { id: Uuid },
    OutcomeUnknown,
    Conflict,
}

#[derive(Clone)]
pub struct DeploymentStore {
    pool: PgPool,
}

impl DeploymentStore {
    pub fn new(pool: PgPool) -> Self {
        DeploymentStore { pool }
    }

    pub fn request_hash(
        environment_id: Uuid,
        release_id: Uuid,
        kind: &str,
        intent_id: Uuid,
    ) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(environment_id.as_bytes());
        hasher.update(release_id.as_bytes());
        hasher.update(kind.as_bytes());
        hasher.update(intent_id.as_bytes());
        hasher.finalize().into()
    }

    pub async fn deploy(
        &self,
        actor_user_id: Uuid,
        environment_id: Uuid,
        release_id: Uuid,
        intent_id: Uuid,
        approval_id: Option<Uuid>,
    ) -> Result<(BeginDeployment, Deployment), ApplicationError> {
        self.deploy_gated(
            actor_user_id,
            environment_id,
            release_id,
            intent_id,
            approval_id,
            false,
        )
        .await
    }

    pub async fn deploy_for_restore(
        &self,
        actor_user_id: Uuid,
        environment_id: Uuid,
        release_id: Uuid,
        intent_id: Uuid,
    ) -> Result<(BeginDeployment, Deployment), ApplicationError> {
        self.deploy_gated(
            actor_user_id,
            environment_id,
            release_id,
            intent_id,
            None,
            true,
        )
        .await
    }

    async fn deploy_gated(
        &self,
        actor_user_id: Uuid,
        environment_id: Uuid,
        release_id: Uuid,
        intent_id: Uuid,
        approval_id: Option<Uuid>,
        archive_restore: bool,
    ) -> Result<(BeginDeployment, Deployment), ApplicationError> {
        let environment = applications::load_environment(&self.pool, environment_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let action = if environment.kind == "prod" {
            Action::ManageProduction
        } else {
            Action::DeployDev
        };
        let application = applications::ApplicationStore::new(self.pool.clone(), String::new())
            .require_in_project(actor_user_id, environment.application_id, action)
            .await?
            .0;
        let release = ReleaseStore::new(self.pool.clone())
            .get(actor_user_id, release_id)
            .await?;
        if release.application_id != application.id || release.state != "ready" {
            return Err(ApplicationError::NotFound);
        }
        let postgres = release
            .manifest
            .get("database")
            .and_then(|database| database.get("postgres"))
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if postgres {
            match crate::databases::DatabaseStore::new(self.pool.clone())
                .by_environment(environment_id)
                .await?
            {
                Some(database) if database.state == "ready" => {}
                _ => return Err(ApplicationError::DatabaseRequired),
            }
        }
        if environment.kind == "prod" && !archive_restore {
            applications::require_approval(
                &self.pool,
                approval_id,
                application.project_id,
                "publish_production",
                &applications::ApprovalTarget {
                    application_id: Some(application.id),
                    environment_id: Some(environment.id),
                    release_id: Some(release.id),
                    ..Default::default()
                },
                actor_user_id,
            )
            .await?;
        }
        let hash = Self::request_hash(environment_id, release_id, &environment.kind, intent_id);
        let mut tx = self.pool.begin().await?;
        crate::Kernel::lock_user_row(&mut tx, actor_user_id).await?;
        applications::lock_project(&mut tx, application.project_id).await?;
        if archive_restore {
            applications::require_restoring_application(&mut tx, application.id).await?;
        } else {
            applications::require_live_application(&mut tx, application.id).await?;
        }
        let env_row = sqlx::query(
            "select revision, active_deployment_id from application_environments \
             where id = $1 for update",
        )
        .bind(environment_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApplicationError::NotFound)?;
        let revision: i64 = env_row.get("revision");
        let predecessor: Option<Uuid> = env_row.get("active_deployment_id");
        let live: i64 = sqlx::query_scalar(
            "select count(*) from application_deployments \
             where environment_id = $1 \
               and state in ('accepted', 'materializing', 'starting', 'healthy', \
                             'activating', 'active', 'unknown', 'superseded', 'failed')",
        )
        .bind(environment_id)
        .fetch_one(&mut *tx)
        .await?;
        if live >= MAX_LIVE_DEPLOYMENTS_PER_ENVIRONMENT {
            return Err(ApplicationError::Kernel(crate::KernelError::Quota));
        }
        let project_inflight: i64 = sqlx::query_scalar(
            "select count(*) from application_deployments d \
             join application_environments e on e.id = d.environment_id \
             join applications a on a.id = e.application_id \
             where a.project_id = $1 \
               and d.state in ('accepted', 'materializing', 'starting', 'activating')",
        )
        .bind(application.project_id)
        .fetch_one(&mut *tx)
        .await?;
        if project_inflight >= MAX_CONCURRENT_DEPLOYMENTS_PER_PROJECT {
            return Err(ApplicationError::Kernel(crate::KernelError::Quota));
        }
        let user_inflight: i64 = sqlx::query_scalar(
            "select count(*) from application_deployments \
             where created_by_user_id = $1 \
               and state in ('accepted', 'materializing', 'starting', 'activating')",
        )
        .bind(actor_user_id)
        .fetch_one(&mut *tx)
        .await?;
        if user_inflight >= MAX_CONCURRENT_DEPLOYMENTS_PER_USER {
            return Err(ApplicationError::Kernel(crate::KernelError::Quota));
        }
        let inserted = sqlx::query(
            "insert into application_deployments \
             (id, environment_id, release_id, deployment_intent_id, request_hash, state, \
              desired_revision, previous_deployment_id, created_by_user_id) \
             values ($1, $2, $3, $4, $5, 'accepted', $6, $7, $8) \
             on conflict (deployment_intent_id) do nothing \
             returning id",
        )
        .bind(Uuid::new_v4())
        .bind(environment_id)
        .bind(release_id)
        .bind(intent_id)
        .bind(hash.as_slice())
        .bind(revision + 1)
        .bind(predecessor)
        .bind(actor_user_id)
        .fetch_optional(&mut *tx)
        .await?;
        if inserted.is_some() {
            sqlx::query(
                "update application_deployments set state = 'materializing', dispatched_at = now() \
                 where deployment_intent_id = $1 and state = 'accepted'",
            )
            .bind(intent_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            let deployment = load_by_intent(&self.pool, intent_id)
                .await?
                .ok_or(ApplicationError::NotFound)?;
            return Ok((
                BeginDeployment::ReadyToDispatch { id: deployment.id },
                deployment,
            ));
        }
        tx.commit().await?;
        let existing = load_by_intent(&self.pool, intent_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        if existing.request_hash.as_slice() != hash.as_slice() {
            return Ok((BeginDeployment::Conflict, existing));
        }
        let begin = match existing.state.as_str() {
            "active" => BeginDeployment::Active { id: existing.id },
            "failed" | "stopped" | "superseded" => BeginDeployment::Failed { id: existing.id },
            _ => BeginDeployment::OutcomeUnknown,
        };
        Ok((begin, existing))
    }

    /// Records `active` only after the caller has proven health and the
    /// external edge. A candidate still materializing or starting cannot
    /// take traffic; the previous Deployment stays active.
    pub async fn activate(&self, deployment_id: Uuid) -> Result<Deployment, ApplicationError> {
        let current = load(&self.pool, deployment_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        if current.state == "active" {
            return Ok(current);
        }
        if current.state != "healthy" {
            return Err(ApplicationError::WorkspaceBusy);
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query("select id from application_environments where id = $1 for update")
            .bind(current.environment_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let switched = sqlx::query(
            "update application_environments set active_deployment_id = $1, revision = revision + 1 \
             where id = $2 and active_deployment_id is not distinct from $3 \
             returning id",
        )
        .bind(deployment_id)
        .bind(current.environment_id)
        .bind(current.previous_deployment_id)
        .fetch_optional(&mut *tx)
        .await?;
        if switched.is_none() {
            return Err(ApplicationError::WorkspaceBusy);
        }
        sqlx::query(
            "update application_deployments set state = 'active', active_at = now(), \
                    observed_revision = desired_revision \
             where id = $1",
        )
        .bind(deployment_id)
        .execute(&mut *tx)
        .await?;
        if let Some(previous) = current.previous_deployment_id {
            sqlx::query(
                "update application_deployments set state = 'superseded', terminal_at = now() \
                 where id = $1 and state = 'active'",
            )
            .bind(previous)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        load(&self.pool, deployment_id)
            .await?
            .ok_or(ApplicationError::NotFound)
    }

    /// Probe proof: readiness and internal HTTP succeeded. Does not switch
    /// the Environment Service selector; `activate` is the cutover.
    pub async fn mark_healthy(&self, deployment_id: Uuid) -> Result<Deployment, ApplicationError> {
        let current = load(&self.pool, deployment_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        if current.state == "healthy" || current.state == "active" {
            return Ok(current);
        }
        if !matches!(
            current.state.as_str(),
            "starting" | "materializing" | "unknown"
        ) {
            return Err(ApplicationError::WorkspaceBusy);
        }
        sqlx::query(
            "update application_deployments set state = 'healthy' \
             where id = $1 and state in ('starting', 'materializing', 'unknown')",
        )
        .bind(deployment_id)
        .execute(&self.pool)
        .await?;
        load(&self.pool, deployment_id)
            .await?
            .ok_or(ApplicationError::NotFound)
    }

    pub async fn advance(
        &self,
        deployment_id: Uuid,
        state: &str,
    ) -> Result<Deployment, ApplicationError> {
        sqlx::query("update application_deployments set state = $2 where id = $1")
            .bind(deployment_id)
            .bind(state)
            .execute(&self.pool)
            .await?;
        load(&self.pool, deployment_id)
            .await?
            .ok_or(ApplicationError::NotFound)
    }

    pub async fn unknown(&self, deployment_id: Uuid) -> Result<(), ApplicationError> {
        sqlx::query(
            "update application_deployments set state = 'unknown', terminal_at = now() \
             where id = $1 and state not in ('active', 'superseded', 'stopped', 'failed')",
        )
        .bind(deployment_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Definite materialization failure. Ambiguous Fabric outcomes stay
    /// `unknown` and are not auto-cleaned.
    pub async fn fail(&self, deployment_id: Uuid) -> Result<(), ApplicationError> {
        sqlx::query(
            "update application_deployments set state = 'failed', terminal_at = now() \
             where id = $1 and state in ('accepted', 'materializing', 'starting')",
        )
        .bind(deployment_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Ambiguous Fabric cutover or stop: hold the row as `unknown` and do
    /// not clear `active_deployment_id`. Control must not record a successful
    /// SQL transition when the edge effect is unobserved.
    pub async fn hold_unknown(&self, deployment_id: Uuid) -> Result<(), ApplicationError> {
        sqlx::query(
            "update application_deployments set state = 'unknown', terminal_at = now() \
             where id = $1 and state in \
             ('materializing', 'starting', 'healthy', 'activating', 'active')",
        )
        .bind(deployment_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn rollback(
        &self,
        actor_user_id: Uuid,
        deployment_id: Uuid,
        intent_id: Uuid,
        approval_id: Option<Uuid>,
    ) -> Result<(BeginDeployment, Deployment), ApplicationError> {
        let current = self.get(actor_user_id, deployment_id).await?;
        let environment = applications::load_environment(&self.pool, current.environment_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let previous = current
            .previous_deployment_id
            .ok_or(ApplicationError::NotFound)?;
        let previous_row = load(&self.pool, previous)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        self.deploy(
            actor_user_id,
            environment.id,
            previous_row.release_id,
            intent_id,
            approval_id,
        )
        .await
    }

    /// Recreates the same Deployment realization. Does not create a new Release.
    pub async fn restart(
        &self,
        actor_user_id: Uuid,
        deployment_id: Uuid,
    ) -> Result<Deployment, ApplicationError> {
        let deployment = self.get(actor_user_id, deployment_id).await?;
        if deployment.state != "active" && deployment.state != "healthy" {
            return Err(ApplicationError::WorkspaceBusy);
        }
        sqlx::query(
            "update application_deployments set state = 'starting', observed_revision = observed_revision + 1 \
             where id = $1",
        )
        .bind(deployment_id)
        .execute(&self.pool)
        .await?;
        load(&self.pool, deployment_id)
            .await?
            .ok_or(ApplicationError::NotFound)
    }

    pub async fn prepare_stop(
        &self,
        actor_user_id: Uuid,
        deployment_id: Uuid,
    ) -> Result<Deployment, ApplicationError> {
        let deployment = self.get(actor_user_id, deployment_id).await?;
        let environment = applications::load_environment(&self.pool, deployment.environment_id)
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
        Ok(deployment)
    }

    pub async fn commit_stop(&self, deployment_id: Uuid) -> Result<Deployment, ApplicationError> {
        let deployment = load(&self.pool, deployment_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("select id from application_environments where id = $1 for update")
            .bind(deployment.environment_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        sqlx::query(
            "update application_deployments set state = 'stopped', terminal_at = now() where id = $1",
        )
        .bind(deployment_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "update application_environments set active_deployment_id = null \
             where id = $1 and active_deployment_id = $2",
        )
        .bind(deployment.environment_id)
        .bind(deployment_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        load(&self.pool, deployment_id)
            .await?
            .ok_or(ApplicationError::NotFound)
    }

    pub async fn get(
        &self,
        actor_user_id: Uuid,
        deployment_id: Uuid,
    ) -> Result<Deployment, ApplicationError> {
        let deployment = load(&self.pool, deployment_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let environment = applications::load_environment(&self.pool, deployment.environment_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        applications::ApplicationStore::new(self.pool.clone(), String::new())
            .require_in_project(
                actor_user_id,
                environment.application_id,
                Action::ReadProject,
            )
            .await?;
        Ok(deployment)
    }

    pub async fn list(
        &self,
        actor_user_id: Uuid,
        environment_id: Uuid,
    ) -> Result<(Environment, Vec<Deployment>), ApplicationError> {
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
        let rows = sqlx::query(&format!(
            "{DEPLOY_SELECT} where environment_id = $1 order by accepted_at, id"
        ))
        .bind(environment_id)
        .fetch_all(&self.pool)
        .await?;
        Ok((environment, rows.into_iter().map(row_deployment).collect()))
    }

    /// Superseded and failed Deployments still own Fabric resources until
    /// `commit_stop`. Startup drains this queue. Ambiguous `unknown` rows
    /// are not included.
    pub async fn list_superseded(&self) -> Result<Vec<Deployment>, ApplicationError> {
        let rows = sqlx::query(&format!(
            "{DEPLOY_SELECT} where state in ('superseded', 'failed') order by terminal_at, id"
        ))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_deployment).collect())
    }

    pub async fn get_internal(&self, deployment_id: Uuid) -> Result<Deployment, ApplicationError> {
        load(&self.pool, deployment_id)
            .await?
            .ok_or(ApplicationError::NotFound)
    }
}

const DEPLOY_SELECT: &str = "select id, environment_id, release_id, deployment_intent_id, request_hash, \
     state, desired_revision, observed_revision, previous_deployment_id, created_by_user_id, \
     accepted_at::text as accepted_at, dispatched_at::text as dispatched_at, \
     active_at::text as active_at, terminal_at::text as terminal_at \
     from application_deployments";

async fn load(pool: &PgPool, id: Uuid) -> Result<Option<Deployment>, sqlx::Error> {
    let row = sqlx::query(&format!("{DEPLOY_SELECT} where id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(row_deployment))
}

async fn load_by_intent(pool: &PgPool, intent: Uuid) -> Result<Option<Deployment>, sqlx::Error> {
    let row = sqlx::query(&format!("{DEPLOY_SELECT} where deployment_intent_id = $1"))
        .bind(intent)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(row_deployment))
}

fn row_deployment(row: sqlx::postgres::PgRow) -> Deployment {
    Deployment {
        id: row.get("id"),
        environment_id: row.get("environment_id"),
        release_id: row.get("release_id"),
        deployment_intent_id: row.get("deployment_intent_id"),
        request_hash: row.get("request_hash"),
        state: row.get("state"),
        desired_revision: row.get("desired_revision"),
        observed_revision: row.get("observed_revision"),
        previous_deployment_id: row.get("previous_deployment_id"),
        created_by_user_id: row.get("created_by_user_id"),
        accepted_at: row.get("accepted_at"),
        dispatched_at: row.get("dispatched_at"),
        active_at: row.get("active_at"),
        terminal_at: row.get("terminal_at"),
    }
}
