//! Deployment desired-state machine. Fabric realizes; PostgreSQL owns intent.

use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::applications::{self, ApplicationError, Environment};
use crate::auth::Action;
use crate::releases::ReleaseStore;

/// At most one active Deployment plus one candidate per Environment after
/// reservation. Occupancy is the complement of [`fully_absent`].
pub const MAX_LIVE_DEPLOYMENTS_PER_ENVIRONMENT: i64 = 2;
/// In-flight deploy operations per Project (desired running, not yet
/// health-proven or traffic-owning). Traffic-owning rows use the
/// Environment cap.
pub const MAX_CONCURRENT_DEPLOYMENTS_PER_PROJECT: i64 = 2;
/// In-flight deploy operations per actor.
pub const MAX_CONCURRENT_DEPLOYMENTS_PER_USER: i64 = 2;

/// SQL form of [`occupies_environment`]. Keep in lockstep with the Rust
/// predicate; quota SQL must not special-case teardown lag.
macro_rules! occupies_environment_sql {
    () => {
        "not ( \
            desired_state = 'absent' \
            and coalesce(nullif(observed_state, ''), '') in ('absent', '') \
            and observed_revision >= desired_revision \
        )"
    };
}

pub const OCCUPIES_ENVIRONMENT_SQL: &str = occupies_environment_sql!();

/// Terminal empty Environment slot: desired absent, observed absent or
/// never-set, and Fabric caught up. Occupancy is the complement, not a
/// teardown-lag OR on `desired_state = 'absent'`.
pub fn fully_absent(
    desired_state: &str,
    observed_state: &str,
    desired_revision: i64,
    observed_revision: i64,
) -> bool {
    desired_state == "absent"
        && matches!(observed_state, "absent" | "")
        && observed_revision >= desired_revision
}

/// A Deployment occupies its Environment until [`fully_absent`].
pub fn occupies_environment(
    desired_state: &str,
    observed_state: &str,
    desired_revision: i64,
    observed_revision: i64,
) -> bool {
    !fully_absent(
        desired_state,
        observed_state,
        desired_revision,
        observed_revision,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deployment {
    pub id: Uuid,
    pub environment_id: Uuid,
    pub release_id: Uuid,
    pub deployment_intent_id: Uuid,
    pub request_hash: Vec<u8>,
    pub state: String,
    pub desired_state: String,
    pub observed_state: String,
    pub last_error_code: Option<String>,
    pub desired_revision: i64,
    pub observed_revision: i64,
    pub previous_deployment_id: Option<Uuid>,
    pub created_by_user_id: Uuid,
    pub accepted_at: String,
    pub dispatched_at: Option<String>,
    pub active_at: Option<String>,
    pub terminal_at: Option<String>,
    /// Environment traffic pointer. Process `state` is not this fact.
    pub traffic: bool,
    /// Prove-then-switch bit. Leftover process `healthy` is not this fact.
    pub proven: bool,
    pub desired_pod_generation: i64,
    pub observed_pod_generation: i64,
}

impl Deployment {
    /// Wire projection: traffic owner presents as `active`. Prove-then-switch
    /// proof stays `healthy`. Desired teardown presents as `stopped`. Leftover
    /// process `accepted` is not a product state.
    pub fn wire_state(&self) -> &str {
        if self.traffic {
            return "active";
        }
        match self.desired_state.as_str() {
            "absent" | "stopped" => "stopped",
            _ if self.proven => "healthy",
            _ => "creating",
        }
    }

    pub(crate) fn is_proven(&self) -> bool {
        self.traffic || self.proven
    }
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
                Some(database) if database.wire_state() == "ready" => {}
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
        let live: i64 = sqlx::query_scalar(concat!(
            "select count(*) from application_deployments \
             where environment_id = $1 \
               and ",
            occupies_environment_sql!(),
        ))
        .bind(environment_id)
        .fetch_one(&mut *tx)
        .await?;
        if live >= MAX_LIVE_DEPLOYMENTS_PER_ENVIRONMENT {
            return Err(ApplicationError::InFlightQuota);
        }
        // Definite stream/materialize failures are not unknown: they do not
        // occupy the in-flight cap. fabric_unknown remains counted.
        let project_inflight: i64 = sqlx::query_scalar(
            "select count(*) from application_deployments d \
             join application_environments e on e.id = d.environment_id \
             join applications a on a.id = e.application_id \
             where a.project_id = $1 \
               and d.desired_state = 'running' \
               and not d.proven \
               and coalesce(d.last_error_code, '') not in ('release_stream_failed', 'materialize_failed') \
               and not exists ( \
                    select 1 from application_environments e2 \
                    where e2.active_deployment_id = d.id \
               )",
        )
        .bind(application.project_id)
        .fetch_one(&mut *tx)
        .await?;
        if project_inflight >= MAX_CONCURRENT_DEPLOYMENTS_PER_PROJECT {
            return Err(ApplicationError::InFlightQuota);
        }
        let user_inflight: i64 = sqlx::query_scalar(
            "select count(*) from application_deployments \
             where created_by_user_id = $1 \
               and desired_state = 'running' \
               and not proven \
               and coalesce(last_error_code, '') not in ('release_stream_failed', 'materialize_failed') \
               and not exists ( \
                    select 1 from application_environments e \
                    where e.active_deployment_id = application_deployments.id \
               )",
        )
        .bind(actor_user_id)
        .fetch_one(&mut *tx)
        .await?;
        if user_inflight >= MAX_CONCURRENT_DEPLOYMENTS_PER_USER {
            return Err(ApplicationError::InFlightQuota);
        }
        // Schema default leftover `accepted` satisfies CHECK. Realization is
        // desired `running`; prove-then-switch sets `proven`.
        let inserted = sqlx::query(
            "insert into application_deployments \
             (id, environment_id, release_id, deployment_intent_id, request_hash, \
              desired_revision, desired_state, previous_deployment_id, created_by_user_id, \
              dispatched_at) \
             values ($1, $2, $3, $4, $5, $6, 'running', $7, $8, now()) \
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
        let begin = if existing.traffic {
            BeginDeployment::Active { id: existing.id }
        } else if existing.desired_state == "absent" || existing.desired_state == "stopped" {
            BeginDeployment::Failed { id: existing.id }
        } else {
            BeginDeployment::ReadyToDispatch { id: existing.id }
        };
        Ok((begin, existing))
    }

    /// PostgreSQL commits the desired traffic target first. Fabric
    /// reconciles the Environment Service selector. Observation then
    /// advances `observed_deployment_id` and the settled `active` pointer.
    pub async fn set_desired_traffic(
        &self,
        deployment_id: Uuid,
    ) -> Result<Deployment, ApplicationError> {
        let current = load(&self.pool, deployment_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        if current.traffic {
            return Ok(current);
        }
        if !current.proven {
            return Err(ApplicationError::WorkspaceBusy);
        }
        let mut tx = self.pool.begin().await?;
        let env = sqlx::query(
            "select desired_deployment_id, active_deployment_id \
             from application_environments where id = $1 for update",
        )
        .bind(current.environment_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApplicationError::NotFound)?;
        let desired: Option<Uuid> = env.get("desired_deployment_id");
        if desired == Some(deployment_id) {
            tx.commit().await?;
            return load(&self.pool, deployment_id)
                .await?
                .ok_or(ApplicationError::NotFound);
        }
        let active: Option<Uuid> = env.get("active_deployment_id");
        let expected = desired.or(active).or(current.previous_deployment_id);
        let switched = sqlx::query(
            "update application_environments set desired_deployment_id = $1, \
                    revision = revision + 1 \
             where id = $2 and desired_deployment_id is not distinct from $3 \
             returning id",
        )
        .bind(deployment_id)
        .bind(current.environment_id)
        .bind(expected)
        .fetch_optional(&mut *tx)
        .await?;
        if switched.is_none() {
            return Err(ApplicationError::WorkspaceBusy);
        }
        tx.commit().await?;
        load(&self.pool, deployment_id)
            .await?
            .ok_or(ApplicationError::NotFound)
    }

    /// Settled traffic: desired, observed, and active name the same Deployment.
    pub async fn settle_observed_traffic(
        &self,
        deployment_id: Uuid,
    ) -> Result<Deployment, ApplicationError> {
        self.settle_observed_traffic_at(deployment_id, None).await
    }

    pub async fn settle_observed_traffic_at(
        &self,
        deployment_id: Uuid,
        traffic_observed_revision: Option<i64>,
    ) -> Result<Deployment, ApplicationError> {
        let current = load(&self.pool, deployment_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        // Matching IDs without Fabric revision proof are not settled.
        if current.traffic {
            return Ok(current);
        }
        if !current.proven {
            return Err(ApplicationError::WorkspaceBusy);
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query("select id from application_environments where id = $1 for update")
            .bind(current.environment_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let switched = sqlx::query(
            "update application_environments \
             set observed_deployment_id = $1, active_deployment_id = $1, \
                 traffic_observed_revision = greatest( \
                    traffic_observed_revision, coalesce($3, greatest(revision, 1))) \
             where id = $2 and desired_deployment_id = $1 \
             returning id",
        )
        .bind(deployment_id)
        .bind(current.environment_id)
        .bind(traffic_observed_revision)
        .fetch_optional(&mut *tx)
        .await?;
        if switched.is_none() {
            return Err(ApplicationError::WorkspaceBusy);
        }
        sqlx::query(
            "update application_deployments set active_at = now(), \
                    reconcile_after = now() + ($2 * interval '1 second') \
             where id = $1",
        )
        .bind(deployment_id)
        .bind(crate::reconcile::OBSERVE_AFTER_SECS)
        .execute(&mut *tx)
        .await?;
        if let Some(previous) = current.previous_deployment_id {
            sqlx::query(
                "update application_deployments set \
                        desired_state = 'absent', \
                        desired_revision = case \
                            when desired_state = 'absent' then desired_revision \
                            else desired_revision + 1 \
                        end, \
                        terminal_at = now() \
                 where id = $1",
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

    /// Settled absent traffic: desired is None and Fabric observed None.
    pub async fn settle_observed_absent(
        &self,
        environment_id: Uuid,
        traffic_observed_revision: i64,
    ) -> Result<Environment, ApplicationError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("select id from application_environments where id = $1 for update")
            .bind(environment_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let switched = sqlx::query(
            "update application_environments \
             set observed_deployment_id = null, active_deployment_id = null, \
                 traffic_observed_revision = greatest(traffic_observed_revision, $2) \
             where id = $1 and desired_deployment_id is null \
             returning id",
        )
        .bind(environment_id)
        .bind(traffic_observed_revision)
        .fetch_optional(&mut *tx)
        .await?;
        if switched.is_none() {
            return Err(ApplicationError::WorkspaceBusy);
        }
        tx.commit().await?;
        applications::load_environment(&self.pool, environment_id)
            .await?
            .ok_or(ApplicationError::NotFound)
    }

    /// Compatibility wrapper for callers that settle without Fabric.
    pub async fn activate(&self, deployment_id: Uuid) -> Result<Deployment, ApplicationError> {
        self.set_desired_traffic(deployment_id).await?;
        self.settle_observed_traffic(deployment_id).await
    }

    /// Probe proof: in-guest HTTP succeeded. Observed `running` belongs to
    /// Fabric GET after the Pod is Ready. This writes only the prove bit.
    pub async fn mark_healthy(&self, deployment_id: Uuid) -> Result<Deployment, ApplicationError> {
        let current = load(&self.pool, deployment_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        if current.is_proven() {
            return Ok(current);
        }
        if current.desired_state != "running" {
            return Err(ApplicationError::WorkspaceBusy);
        }
        sqlx::query(
            "update application_deployments set proven = true, \
                    reconcile_after = now() + ($2 * interval '1 second') \
             where id = $1 and desired_state = 'running' \
               and not proven \
               and not exists ( \
                    select 1 from application_environments e \
                    where e.active_deployment_id = $1 \
               )",
        )
        .bind(deployment_id)
        .bind(crate::reconcile::OBSERVE_RETRY_SECS)
        .execute(&self.pool)
        .await?;
        load(&self.pool, deployment_id)
            .await?
            .ok_or(ApplicationError::NotFound)
    }

    /// Ambiguous Fabric observation on a candidate. Realization is a
    /// reconciler: desired stays running so PUT/GET retry. Not a leftover
    /// process journal.
    pub async fn unknown(&self, deployment_id: Uuid) -> Result<(), ApplicationError> {
        sqlx::query(
            "update application_deployments set last_error_code = 'fabric_unknown', \
                    reconcile_after = now() + ($2 * interval '1 second') \
             where id = $1 and desired_state = 'running' \
               and not proven \
               and not exists ( \
                    select 1 from application_environments e \
                    where e.active_deployment_id = $1 \
               )",
        )
        .bind(deployment_id)
        .bind(crate::reconcile::OBSERVE_RETRY_SECS)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Definite materialization failure. Discard the candidate (desired
    /// absent). Leftover process stays the CHECK dummy.
    pub async fn fail(&self, deployment_id: Uuid) -> Result<(), ApplicationError> {
        sqlx::query(
            "update application_deployments set proven = false, desired_state = 'absent', \
                    desired_revision = case \
                        when desired_state = 'absent' then desired_revision \
                        else desired_revision + 1 \
                    end, \
                    last_error_code = coalesce(last_error_code, 'materialize_failed'), \
                    terminal_at = now(), reconcile_after = now() \
             where id = $1 and desired_state = 'running' \
               and not proven \
               and not exists ( \
                    select 1 from application_environments e \
                    where e.active_deployment_id = $1 \
               )",
        )
        .bind(deployment_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Ambiguous Fabric cutover or stop: do not record traffic. Leftover
    /// process is not a journal. The SQL traffic pointer stays unchanged.
    pub async fn hold_unknown(&self, deployment_id: Uuid) -> Result<(), ApplicationError> {
        sqlx::query(
            "update application_deployments set last_error_code = 'fabric_unknown' \
             where id = $1 and proven \
               and not exists ( \
                    select 1 from application_environments e \
                    where e.active_deployment_id = $1 \
               )",
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
    /// Authorization is owned here: `ReadProject` via `get()` never mutates.
    pub async fn restart(
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
        let action = if environment.kind == "prod" {
            Action::ManageProduction
        } else {
            Action::DeployDev
        };
        applications::ApplicationStore::new(self.pool.clone(), String::new())
            .require_in_project(actor_user_id, environment.application_id, action)
            .await?;
        if !deployment.is_proven() {
            return Err(ApplicationError::WorkspaceBusy);
        }
        sqlx::query(
            "update application_deployments set desired_state = 'running', \
                    desired_revision = desired_revision + 1, \
                    desired_pod_generation = desired_pod_generation + 1, \
                    reconcile_after = now() \
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

    /// Mutation authority: persist the desired spec and bump revision.
    /// Realization is the Deployment reconciler's PUT. Traffic intent
    /// drops here when desired is no longer running.
    pub async fn request_desired(
        &self,
        deployment_id: Uuid,
        desired_state: &str,
    ) -> Result<Deployment, ApplicationError> {
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
            "update application_deployments \
             set desired_state = $2, \
                 desired_revision = desired_revision + 1, \
                 reconcile_after = now() \
             where id = $1",
        )
        .bind(deployment_id)
        .bind(desired_state)
        .execute(&mut *tx)
        .await?;
        if desired_state == "stopped" || desired_state == "absent" {
            sqlx::query(
                "update application_environments set \
                        desired_deployment_id = case \
                            when desired_deployment_id = $2 then null \
                            else desired_deployment_id end, \
                        revision = case \
                            when desired_deployment_id = $2 then revision + 1 \
                            else revision end \
                 where id = $1",
            )
            .bind(deployment.environment_id)
            .bind(deployment_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        load(&self.pool, deployment_id)
            .await?
            .ok_or(ApplicationError::NotFound)
    }

    pub async fn commit_stop(&self, deployment_id: Uuid) -> Result<Deployment, ApplicationError> {
        self.commit_teardown(deployment_id, "stopped").await
    }

    pub async fn commit_absent(&self, deployment_id: Uuid) -> Result<Deployment, ApplicationError> {
        self.commit_teardown(deployment_id, "absent").await
    }

    /// Observed settlement after Fabric matches desired teardown. Does not
    /// rewrite desired; the mutation path already owns that revision.
    async fn commit_teardown(
        &self,
        deployment_id: Uuid,
        observed_state: &str,
    ) -> Result<Deployment, ApplicationError> {
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
            "update application_deployments set \
             observed_state = $2, \
             last_error_code = null, reconcile_after = null, \
             terminal_at = coalesce(terminal_at, now()) where id = $1",
        )
        .bind(deployment_id)
        .bind(observed_state)
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
            "{DEPLOY_SELECT} where d.environment_id = $1 order by d.accepted_at, d.id"
        ))
        .bind(environment_id)
        .fetch_all(&self.pool)
        .await?;
        Ok((environment, rows.into_iter().map(row_deployment).collect()))
    }

    pub async fn get_internal(&self, deployment_id: Uuid) -> Result<Deployment, ApplicationError> {
        load(&self.pool, deployment_id)
            .await?
            .ok_or(ApplicationError::NotFound)
    }

    /// Application delete used to skip already-`stopped` rows still desired
    /// `running`, so Fabric never received Absent. Heal those onto the wake.
    pub async fn persist_absent_desired_for_removing_applications(
        &self,
    ) -> Result<(), ApplicationError> {
        sqlx::query(
            "update application_deployments d \
             set desired_state = 'absent', \
                 desired_revision = case \
                     when d.desired_state = 'absent' then d.desired_revision \
                     else d.desired_revision + 1 \
                 end, \
                 reconcile_after = now() \
             from application_environments e \
             join applications a on a.id = e.application_id \
             where d.environment_id = e.id \
               and a.state in ('deleting', 'deleted') \
               and d.desired_state <> 'absent'",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

const DEPLOY_SELECT: &str = "select d.id, d.environment_id, d.release_id, d.deployment_intent_id, \
     d.request_hash, d.state, d.proven, \
     coalesce(nullif(d.desired_state, ''), 'running') as desired_state, \
     coalesce(d.observed_state, '') as observed_state, d.last_error_code, \
     d.desired_revision, d.observed_revision, d.previous_deployment_id, d.created_by_user_id, \
     d.accepted_at::text as accepted_at, d.dispatched_at::text as dispatched_at, \
     d.active_at::text as active_at, d.terminal_at::text as terminal_at, \
     coalesce(d.desired_pod_generation, 0) as desired_pod_generation, \
     coalesce(d.observed_pod_generation, 0) as observed_pod_generation, \
     (e.active_deployment_id is not distinct from d.id \
        and e.desired_deployment_id is not distinct from d.id \
        and e.observed_deployment_id is not distinct from d.id \
        and e.traffic_observed_revision >= greatest(e.revision, 1)) as traffic \
     from application_deployments d \
     join application_environments e on e.id = d.environment_id";

async fn load(pool: &PgPool, id: Uuid) -> Result<Option<Deployment>, sqlx::Error> {
    let row = sqlx::query(&format!("{DEPLOY_SELECT} where d.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(row_deployment))
}

async fn load_by_intent(pool: &PgPool, intent: Uuid) -> Result<Option<Deployment>, sqlx::Error> {
    let row = sqlx::query(&format!(
        "{DEPLOY_SELECT} where d.deployment_intent_id = $1"
    ))
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
        desired_state: row.get("desired_state"),
        observed_state: row.get("observed_state"),
        last_error_code: row.get("last_error_code"),
        desired_revision: row.get("desired_revision"),
        observed_revision: row.get("observed_revision"),
        previous_deployment_id: row.get("previous_deployment_id"),
        created_by_user_id: row.get("created_by_user_id"),
        accepted_at: row.get("accepted_at"),
        dispatched_at: row.get("dispatched_at"),
        active_at: row.get("active_at"),
        terminal_at: row.get("terminal_at"),
        traffic: row.get("traffic"),
        proven: row.get("proven"),
        desired_pod_generation: row.get("desired_pod_generation"),
        observed_pod_generation: row.get("observed_pod_generation"),
    }
}

#[cfg(test)]
mod tests {
    use super::{Deployment, OCCUPIES_ENVIRONMENT_SQL, fully_absent, occupies_environment};
    use uuid::Uuid;

    fn sample(state: &str, traffic: bool) -> Deployment {
        sample_full(state, "running", traffic, false)
    }

    fn sample_desired(state: &str, desired: &str, traffic: bool) -> Deployment {
        sample_full(state, desired, traffic, false)
    }

    fn sample_full(state: &str, desired: &str, traffic: bool, proven: bool) -> Deployment {
        Deployment {
            id: Uuid::nil(),
            environment_id: Uuid::nil(),
            release_id: Uuid::nil(),
            deployment_intent_id: Uuid::nil(),
            request_hash: Vec::new(),
            state: state.into(),
            desired_state: desired.into(),
            observed_state: String::new(),
            last_error_code: None,
            desired_revision: 1,
            observed_revision: 0,
            previous_deployment_id: None,
            created_by_user_id: Uuid::nil(),
            accepted_at: String::new(),
            dispatched_at: None,
            active_at: None,
            terminal_at: None,
            traffic,
            proven,
            desired_pod_generation: 0,
            observed_pod_generation: 0,
        }
    }

    #[test]
    fn wire_state_hides_leftover_process_adjectives() {
        assert_eq!(sample("accepted", false).wire_state(), "creating");
        assert_eq!(
            sample_full("accepted", "running", false, true).wire_state(),
            "healthy"
        );
        assert_eq!(
            sample_full("accepted", "running", true, true).wire_state(),
            "active"
        );
        assert_eq!(
            sample_full("accepted", "absent", false, true).wire_state(),
            "stopped"
        );
        assert_eq!(
            sample_desired("accepted", "running", false).wire_state(),
            "creating",
            "leftover process accepted is not product stopped while desired is running"
        );
        assert_eq!(
            sample_desired("accepted", "absent", false).wire_state(),
            "stopped"
        );
    }

    #[test]
    fn occupancy_is_the_complement_of_fully_absent() {
        assert!(occupies_environment("running", "", 1, 0));
        assert!(occupies_environment("stopped", "stopped", 2, 2));
        assert!(occupies_environment("absent", "running", 3, 3));
        assert!(occupies_environment("absent", "absent", 4, 3));
        assert!(!occupies_environment("absent", "absent", 4, 4));
        assert!(!occupies_environment("absent", "", 4, 4));
        assert!(fully_absent("absent", "absent", 4, 4));
        assert!(!fully_absent("absent", "absent", 4, 3));
        assert!(
            OCCUPIES_ENVIRONMENT_SQL.contains("observed_revision >= desired_revision"),
            "quota SQL must require Fabric catch-up, not a teardown-lag special case"
        );
    }
}
