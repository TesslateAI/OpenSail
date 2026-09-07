//! Application resource: Project-owned deployable, distinct from `projects`.

mod manifest;
mod slug;

use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

pub use manifest::{
    DEFAULT_CPU_MILLIS, DEFAULT_MEMORY_MB, MAX_CPU_MILLIS, MAX_MEMORY_MB, MIN_CPU_MILLIS,
    MIN_MEMORY_MB, Manifest, ManifestError, ManifestV1,
};
pub use slug::{SlugError, allocate as allocate_slug, reserved_names, validate as validate_slug};

use crate::KernelError;
use crate::auth::{self, Action, Role};

const DEFAULT_RUNTIME: &str = "universal-v1";
/// Small explicit Application quota per Project. Exhaustion is a 429, not a
/// silent extra row.
pub const MAX_APPLICATIONS_PER_PROJECT: i64 = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Application {
    pub id: Uuid,
    pub project_id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub slug: String,
    pub root_path: String,
    pub runtime_profile: String,
    pub state: String,
    pub created_by_user_id: Uuid,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Environment {
    pub id: Uuid,
    pub application_id: Uuid,
    pub kind: String,
    pub visibility: String,
    pub hostname: String,
    pub revision: i64,
    pub active_deployment_id: Option<Uuid>,
    pub desired_deployment_id: Option<Uuid>,
    pub observed_deployment_id: Option<Uuid>,
    pub traffic_observed_revision: i64,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationCleanup {
    pub slug: String,
    pub deployments: Vec<CleanupDeployment>,
    pub databases: Vec<Uuid>,
    pub releases: Vec<Uuid>,
    pub workspace_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationArchive {
    pub id: Uuid,
    pub application_id: Uuid,
    pub generation: i64,
    pub state: String,
    pub workspace_snapshot_id: Option<Uuid>,
    pub dev_database_backup_id: Option<Uuid>,
    pub prod_database_backup_id: Option<Uuid>,
    pub dev_release_id: Option<Uuid>,
    pub prod_release_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupDeployment {
    pub id: Uuid,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateOutcome {
    pub application: Application,
    pub environments: Vec<Environment>,
    /// Set when the current Workspace already had an Application; the caller
    /// must open a new conversation rather than silently switch context.
    pub workspace_handoff: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub id: Uuid,
    pub project_id: Uuid,
    pub application_id: Option<Uuid>,
    pub environment_id: Option<Uuid>,
    pub release_id: Option<Uuid>,
    pub kind: String,
    pub state: String,
    pub created_at: String,
}

#[derive(Debug)]
pub enum ApplicationError {
    Kernel(KernelError),
    Auth,
    InvalidName,
    InvalidSlug,
    ReservedSlug,
    InvalidRoot,
    InvalidManifest(String),
    InvalidArgument {
        field: String,
        expected: &'static str,
    },
    WorkspaceMissing,
    WorkspaceBusy,
    NotFound,
    ApprovalRequired(Uuid),
    /// Release declares postgres but this Environment has no ready Database.
    DatabaseRequired,
    /// Observed Workspace guest is not `voie-workspace:v1`.
    WorkspaceImage,
    /// Candidate Deployment exists but is not healthy, so it must not take traffic.
    DeploymentNotReady,
    /// SQL cutover committed, but the superseded predecessor is still running.
    PredecessorCleanupPending,
    /// Release is still referenced, so its object and Blob cannot be dropped.
    ReleaseInUse,
    /// Database security_profile only advances from 1 to 2.
    InvalidSecurityProfile,
    /// In-flight Deployment cap. Not an Application-create refusal.
    InFlightQuota,
}

impl From<KernelError> for ApplicationError {
    fn from(error: KernelError) -> Self {
        ApplicationError::Kernel(error)
    }
}

impl From<sqlx::Error> for ApplicationError {
    fn from(error: sqlx::Error) -> Self {
        ApplicationError::Kernel(KernelError::from(error))
    }
}

impl From<auth::AuthError> for ApplicationError {
    fn from(_: auth::AuthError) -> Self {
        ApplicationError::Auth
    }
}

impl ApplicationError {
    pub fn message(&self) -> &'static str {
        match self {
            ApplicationError::Kernel(KernelError::Conflict) => {
                "resource request conflicts; poll application.status. Do not create another Application"
            }
            ApplicationError::Kernel(KernelError::Quota) => "application quota reached",
            ApplicationError::Kernel(_) => "application operation failed",
            ApplicationError::Auth => "application access denied",
            ApplicationError::InvalidName => "application name is invalid",
            ApplicationError::InvalidSlug => "application slug is invalid",
            ApplicationError::ReservedSlug => "application slug is reserved",
            ApplicationError::InvalidRoot => "application root path is invalid",
            ApplicationError::InvalidManifest(_) => "application manifest is invalid",
            ApplicationError::InvalidArgument { .. } => "invalid argument",
            ApplicationError::WorkspaceMissing => "workspace was not found in this project",
            ApplicationError::WorkspaceBusy => "workspace already has an application",
            ApplicationError::NotFound => "application was not found",
            ApplicationError::ApprovalRequired(_) => "approval required",
            ApplicationError::DatabaseRequired => {
                "dedicated database must be ready before deploying a postgres Release"
            }
            ApplicationError::WorkspaceImage => "workspace guest is not voie-workspace:v1",
            ApplicationError::DeploymentNotReady => {
                "deployment is not healthy yet; poll application.status then retry deployment.activate"
            }
            ApplicationError::PredecessorCleanupPending => {
                "cutover committed; predecessor cleanup is still pending"
            }
            ApplicationError::ReleaseInUse => "release is still referenced and cannot be deleted",
            ApplicationError::InvalidSecurityProfile => {
                "database security profile only advances from 1 to 2"
            }
            ApplicationError::InFlightQuota => {
                "too many in-flight deployments; poll application.status and retry environment.deploy_dev after one is healthy or failed. Do not call application.create"
            }
        }
    }

    /// Text returned to the activation child. Approval refusals include the
    /// request id so the model can wait for the console accept and retry.
    pub fn product_text(&self) -> String {
        match self {
            ApplicationError::ApprovalRequired(id) => json!({
                "error": self.message(),
                "approvalId": id,
            })
            .to_string(),
            ApplicationError::InvalidManifest(detail) => detail.clone(),
            ApplicationError::InvalidArgument { field, expected } => {
                format!("INVALID_ARGUMENT field={field} expected={expected}")
            }
            _ => self.message().to_owned(),
        }
    }

    pub fn status(&self) -> u16 {
        match self {
            ApplicationError::Auth => 403,
            ApplicationError::ApprovalRequired(_) => 409,
            ApplicationError::Kernel(KernelError::Conflict)
            | ApplicationError::WorkspaceBusy
            | ApplicationError::DatabaseRequired
            | ApplicationError::WorkspaceImage
            | ApplicationError::DeploymentNotReady
            | ApplicationError::PredecessorCleanupPending
            | ApplicationError::ReleaseInUse => 409,
            ApplicationError::NotFound | ApplicationError::WorkspaceMissing => 404,
            ApplicationError::Kernel(KernelError::Quota) | ApplicationError::InFlightQuota => 429,
            ApplicationError::Kernel(_) => 500,
            _ => 400,
        }
    }
}

#[derive(Clone)]
pub struct ApplicationStore {
    pool: PgPool,
    console_host: String,
}

impl ApplicationStore {
    pub fn new(pool: PgPool, console_host: String) -> Self {
        ApplicationStore { pool, console_host }
    }

    pub fn console_host(&self) -> &str {
        &self.console_host
    }

    pub(crate) fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Creates an Application on the current activation Workspace.
    /// Occupied Workspaces attach the existing Application. The store
    /// allocates the globally unique slug.
    pub async fn create(
        &self,
        actor_user_id: Uuid,
        project_id: Uuid,
        workspace_id: Uuid,
        name: &str,
        root_path: Option<&str>,
    ) -> Result<CreateOutcome, ApplicationError> {
        auth::authorize(
            &self.pool,
            actor_user_id,
            project_id,
            Action::OperateSession,
        )
        .await?;
        let name = validate_name(name)?;
        let root_path = validate_root(root_path.unwrap_or("."))?;

        let mut tx = self.pool.begin().await?;
        let workspace_row = sqlx::query(
            "select id, project_id, fabric_id, state, desired_state, observed_state, exec_generation \
             from workspaces where id = $1 for update",
        )
        .bind(workspace_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApplicationError::WorkspaceMissing)?;
        let workspace_project: Uuid = workspace_row.get("project_id");
        if workspace_project != project_id {
            return Err(ApplicationError::WorkspaceMissing);
        }
        let workspace_state: String = workspace_row.get("state");
        let workspace_desired: String = workspace_row.get("desired_state");
        let workspace_observed: String = workspace_row.get("observed_state");
        if crate::workspace_wire_state(&workspace_desired, &workspace_observed, &workspace_state)
            != "ready"
        {
            return Err(ApplicationError::WorkspaceBusy);
        }

        let attached = sqlx::query(
            "select id, slug, state from applications \
             where workspace_id = $1 and state <> 'deleted' \
             order by case when state = 'deleting' then 1 else 0 end, created_at, id \
             limit 1",
        )
        .bind(workspace_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(existing) = attached {
            let existing_id: Uuid = existing.get("id");
            let existing_state: String = existing.get("state");
            if existing_state == "deleting" {
                return Err(ApplicationError::WorkspaceBusy);
            }
            tx.commit().await?;
            let application = self.get_internal(existing_id).await?;
            let environments = load_environments(&self.pool, existing_id).await?;
            return Ok(CreateOutcome {
                application,
                environments,
                workspace_handoff: None,
            });
        }

        let application_id = Uuid::new_v4();
        let now_row = insert_application_allocated(
            &mut tx,
            application_id,
            project_id,
            workspace_id,
            &name,
            &root_path,
            actor_user_id,
        )
        .await?;
        let environments =
            insert_environments(&mut tx, application_id, &now_row.slug, &self.console_host).await?;
        tx.commit().await?;
        Ok(CreateOutcome {
            application: now_row,
            environments,
            workspace_handoff: None,
        })
    }

    /// Creates an Application on a newly reserved Workspace identity when the
    /// current Workspace is occupied. The new Workspace row must already exist
    /// in `creating` state; this method does not realize it.
    pub async fn create_with_new_workspace(
        &self,
        actor_user_id: Uuid,
        project_id: Uuid,
        current_workspace_id: Uuid,
        new_workspace_id: Uuid,
        name: &str,
    ) -> Result<CreateOutcome, ApplicationError> {
        auth::authorize(
            &self.pool,
            actor_user_id,
            project_id,
            Action::OperateSession,
        )
        .await?;
        let name = validate_name(name)?;
        let mut tx = self.pool.begin().await?;
        let occupied: bool = sqlx::query_scalar(
            "select exists(select 1 from applications \
             where workspace_id = $1 and state not in ('deleted', 'deleting'))",
        )
        .bind(current_workspace_id)
        .fetch_one(&mut *tx)
        .await?;
        if !occupied {
            return Err(ApplicationError::WorkspaceMissing);
        }
        let application_id = Uuid::new_v4();
        let application = insert_application_allocated(
            &mut tx,
            application_id,
            project_id,
            new_workspace_id,
            &name,
            ".",
            actor_user_id,
        )
        .await?;
        let environments = insert_environments(
            &mut tx,
            application_id,
            &application.slug,
            &self.console_host,
        )
        .await?;
        tx.commit().await?;
        Ok(CreateOutcome {
            application,
            environments,
            workspace_handoff: Some(new_workspace_id),
        })
    }

    pub async fn get(
        &self,
        actor_user_id: Uuid,
        application_id: Uuid,
    ) -> Result<Application, ApplicationError> {
        let application = load(&self.pool, application_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        auth::authorize(
            &self.pool,
            actor_user_id,
            application.project_id,
            Action::ReadProject,
        )
        .await?;
        Ok(application)
    }

    pub async fn get_internal(
        &self,
        application_id: Uuid,
    ) -> Result<Application, ApplicationError> {
        load(&self.pool, application_id)
            .await?
            .ok_or(ApplicationError::NotFound)
    }

    pub async fn list(
        &self,
        actor_user_id: Uuid,
        project_id: Uuid,
    ) -> Result<Vec<Application>, ApplicationError> {
        auth::authorize(&self.pool, actor_user_id, project_id, Action::ReadProject).await?;
        // Terminal `deleting` after the Workspace desired is already
        // `deleted` is reclaimed history. Occupancy follows desired, not
        // process adjectives: create refuses a still-charged Workspace, so
        // list must show that Application until desired drops.
        let rows = sqlx::query(
            "select a.id, a.project_id, a.workspace_id, a.name, a.slug, a.root_path, \
                    a.runtime_profile, a.state, a.created_by_user_id, \
                    a.created_at::text as created_at, a.updated_at::text as updated_at \
             from applications a \
             join workspaces w on w.id = a.workspace_id \
             where a.project_id = $1 \
               and not (a.state = 'deleting' and w.desired_state = 'deleted') \
             order by a.created_at, a.id",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_application).collect())
    }

    pub async fn environments(
        &self,
        actor_user_id: Uuid,
        application_id: Uuid,
    ) -> Result<Vec<Environment>, ApplicationError> {
        let application = self.get(actor_user_id, application_id).await?;
        load_environments(&self.pool, application.id)
            .await
            .map_err(Into::into)
    }

    pub async fn by_workspace(
        &self,
        workspace_id: Uuid,
    ) -> Result<Option<Application>, ApplicationError> {
        let row = sqlx::query(
            "select id, project_id, workspace_id, name, slug, root_path, runtime_profile, \
                    state, created_by_user_id, created_at::text as created_at, \
                    updated_at::text as updated_at \
             from applications where workspace_id = $1 and state <> 'deleting'",
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(row_application))
    }

    /// Live Application on the Workspace, or the latest `deleting` row when
    /// none is live. Delete retry uses this after the durable fence.
    pub async fn by_workspace_for_cleanup(
        &self,
        workspace_id: Uuid,
    ) -> Result<Option<Application>, ApplicationError> {
        if let Some(live) = self.by_workspace(workspace_id).await? {
            return Ok(Some(live));
        }
        let row = sqlx::query(
            "select id, project_id, workspace_id, name, slug, root_path, runtime_profile, \
                    state, created_by_user_id, created_at::text as created_at, \
                    updated_at::text as updated_at \
             from applications where workspace_id = $1 and state = 'deleting' \
             order by updated_at desc, id desc limit 1",
        )
        .bind(workspace_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(row_application))
    }

    /// Workspace desired `deleted` retires slug/hostname occupancy only for
    /// Applications already fenced `deleting` by approved application.delete.
    /// It never transitions a live Application into teardown.
    pub async fn retire_identities_on_deleted_workspaces(&self) -> Result<(), ApplicationError> {
        sqlx::query(
            "update applications a \
             set slug = left('x' || replace(a.id::text, '-', ''), 48), \
                 updated_at = now() \
             from workspaces w \
             where a.workspace_id = w.id \
               and w.desired_state = 'deleted' \
               and a.state = 'deleting' \
               and a.slug not like 'x%'",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "update application_environments e \
             set hostname = 'retired-' || replace(e.id::text, '-', '') \
             from applications a \
             join workspaces w on w.id = a.workspace_id \
             where e.application_id = a.id \
               and w.desired_state = 'deleted' \
               and a.state = 'deleting' \
               and e.hostname not like 'retired-%'",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn require_in_project(
        &self,
        actor_user_id: Uuid,
        application_id: Uuid,
        action: Action,
    ) -> Result<(Application, Role), ApplicationError> {
        let application = load(&self.pool, application_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let role =
            auth::authorize(&self.pool, actor_user_id, application.project_id, action).await?;
        Ok((application, role))
    }

    pub async fn set_visibility(
        &self,
        actor_user_id: Uuid,
        environment_id: Uuid,
        visibility: &str,
        approval_id: Option<Uuid>,
    ) -> Result<Environment, ApplicationError> {
        if visibility != "private" && visibility != "public" {
            return Err(ApplicationError::InvalidName);
        }
        let environment = load_environment(&self.pool, environment_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let application = load(&self.pool, environment.application_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let action = if visibility == "public" {
            Action::ManageProduction
        } else {
            Action::DeployDev
        };
        auth::authorize(&self.pool, actor_user_id, application.project_id, action).await?;
        if visibility == "public" {
            require_approval(
                &self.pool,
                approval_id,
                application.project_id,
                "make_environment_public",
                &ApprovalTarget {
                    application_id: Some(application.id),
                    environment_id: Some(environment.id),
                    ..Default::default()
                },
                actor_user_id,
            )
            .await?;
        }
        sqlx::query(
            "update application_environments set visibility = $2, revision = revision + 1 \
             where id = $1",
        )
        .bind(environment_id)
        .bind(visibility)
        .execute(&self.pool)
        .await?;
        load_environment(&self.pool, environment_id)
            .await?
            .ok_or(ApplicationError::NotFound)
    }

    /// Stops every Deployment and marks Environments suspended. Databases
    /// stay; this is not deletion. Fabric cleanup must succeed first so a
    /// dummy or failed stop cannot leave SQL suspended while Pods still run.
    pub async fn plan_suspend(
        &self,
        actor_user_id: Uuid,
        application_id: Uuid,
    ) -> Result<ApplicationCleanup, ApplicationError> {
        let (application, _) = self
            .require_in_project(actor_user_id, application_id, Action::ManageProduction)
            .await?;
        if application.state == "deleting" {
            return Err(ApplicationError::WorkspaceBusy);
        }
        self.collect_cleanup(&application, false).await
    }

    pub async fn commit_suspend(
        &self,
        actor_user_id: Uuid,
        application_id: Uuid,
    ) -> Result<(), ApplicationError> {
        let _ = self
            .require_in_project(actor_user_id, application_id, Action::ManageProduction)
            .await?;
        sqlx::query(
            "update applications set state = 'suspended', updated_at = now() where id = $1",
        )
        .bind(application_id)
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "update application_deployments \
             set desired_state = 'stopped', \
                 desired_revision = case \
                     when desired_state = 'stopped' then desired_revision \
                     else desired_revision + 1 \
                 end, \
                 reconcile_after = now() \
             where environment_id in (select id from application_environments where application_id = $1) \
               and desired_state <> 'stopped' and desired_state <> 'absent'",
        )
        .bind(application_id)
        .execute(&self.pool)
        .await?;
        retire_traffic_desired(&self.pool, application_id).await?;
        Ok(())
    }

    pub async fn list_approvals(
        &self,
        actor_user_id: Uuid,
        application_id: Uuid,
    ) -> Result<Vec<ApprovalRequest>, ApplicationError> {
        let _ = self
            .require_in_project(actor_user_id, application_id, Action::ReadProject)
            .await?;
        let rows = sqlx::query(
            "select id, project_id, application_id, environment_id, release_id, kind, state, \
                    created_at::text as created_at \
             from approval_requests where application_id = $1 \
             order by created_at desc limit 32",
        )
        .bind(application_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| ApprovalRequest {
                id: row.get("id"),
                project_id: row.get("project_id"),
                application_id: row.get("application_id"),
                environment_id: row.get("environment_id"),
                release_id: row.get("release_id"),
                kind: row.get("kind"),
                state: row.get("state"),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    pub async fn accept_pending_approval(
        &self,
        actor_user_id: Uuid,
        approval_id: Uuid,
    ) -> Result<ApprovalRequest, ApplicationError> {
        let row = sqlx::query(
            "select id, project_id, application_id, environment_id, release_id, kind, state, \
                    created_at::text as created_at \
             from approval_requests where id = $1",
        )
        .bind(approval_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(ApplicationError::NotFound)?;
        let project_id: Uuid = row.get("project_id");
        auth::authorize(
            &self.pool,
            actor_user_id,
            project_id,
            Action::ManageProduction,
        )
        .await?;
        accept_approval(&self.pool, approval_id, actor_user_id).await?;
        let updated = sqlx::query(
            "select id, project_id, application_id, environment_id, release_id, kind, state, \
                    created_at::text as created_at \
             from approval_requests where id = $1",
        )
        .bind(approval_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(ApprovalRequest {
            id: updated.get("id"),
            project_id: updated.get("project_id"),
            application_id: updated.get("application_id"),
            environment_id: updated.get("environment_id"),
            release_id: updated.get("release_id"),
            kind: updated.get("kind"),
            state: updated.get("state"),
            created_at: updated.get("created_at"),
        })
    }

    /// Approves deletion and durable-fences the Application first: User-row
    /// serialization, membership, approval, and `deleting` are one
    /// transaction before Fabric or Blob cleanup. Retry is allowed on an
    /// already-fenced row.
    pub async fn plan_delete(
        &self,
        actor_user_id: Uuid,
        application_id: Uuid,
        approval_id: Option<Uuid>,
    ) -> Result<ApplicationCleanup, ApplicationError> {
        let application = load(&self.pool, application_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let target = ApprovalTarget {
            application_id: Some(application.id),
            ..Default::default()
        };
        let Some(approval_id) = approval_id else {
            require_approval(
                &self.pool,
                None,
                application.project_id,
                "delete_application",
                &target,
                actor_user_id,
            )
            .await?;
            return Err(ApplicationError::Auth);
        };
        let mut tx = self.pool.begin().await?;
        let state = claim_actor(
            &mut tx,
            actor_user_id,
            application_id,
            Action::DestroyApplication,
        )
        .await?;
        require_approval_tx(
            &mut tx,
            approval_id,
            application.project_id,
            "delete_application",
            &target,
            actor_user_id,
        )
        .await?;
        if state != "deleting" {
            let prod_active: bool = sqlx::query_scalar(
                "select exists( \
                    select 1 from application_environments e \
                    join application_deployments d on d.id = e.active_deployment_id \
                    where e.application_id = $1 and e.kind = 'prod' \
                 )",
            )
            .bind(application_id)
            .fetch_one(&mut *tx)
            .await?;
            if prod_active {
                return Err(ApplicationError::WorkspaceBusy);
            }
            sqlx::query(
                "update applications set state = 'deleting', updated_at = now() where id = $1",
            )
            .bind(application.id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        let application = load(&self.pool, application_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let mut cleanup = self.collect_cleanup(&application, true).await?;
        let others: i64 = sqlx::query_scalar(
            "select count(*) from applications \
             where workspace_id = $1 and id <> $2 \
               and state not in ('deleted', 'deleting')",
        )
        .bind(application.workspace_id)
        .bind(application.id)
        .fetch_one(&self.pool)
        .await?;
        if others == 0 {
            cleanup.workspace_id = Some(application.workspace_id);
        }
        Ok(cleanup)
    }

    /// Settles Deployments, Environments, and Databases after the Application
    /// is already fenced `deleting`. Mutations persist desired `absent` /
    /// `deleted` only; Fabric realization belongs to the reconcilers.
    /// Traffic desired becomes `None`; observed/active wait for Fabric.
    /// Idempotent after cleanup.
    pub async fn commit_delete(&self, application_id: Uuid) -> Result<(), ApplicationError> {
        sqlx::query("update applications set state = 'deleting', updated_at = now() where id = $1")
            .bind(application_id)
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "update application_deployments \
             set desired_state = 'absent', \
                 desired_revision = case \
                     when desired_state = 'absent' then desired_revision \
                     else desired_revision + 1 \
                 end, \
                 reconcile_after = now() \
             where environment_id in (select id from application_environments where application_id = $1) \
               and desired_state <> 'absent'",
        )
        .bind(application_id)
        .execute(&self.pool)
        .await?;
        retire_traffic_desired(&self.pool, application_id).await?;
        sqlx::query(
            "update application_databases \
             set desired_state = 'absent', \
                 desired_revision = case \
                     when desired_state = 'absent' then desired_revision \
                     else desired_revision + 1 \
                 end, \
                 reconcile_after = now() \
             where application_id = $1 \
               and desired_state <> 'absent'",
        )
        .bind(application_id)
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "delete from environment_secret_bindings \
             where environment_id in (select id from application_environments where application_id = $1)",
        )
        .bind(application_id)
        .execute(&self.pool)
        .await?;
        sqlx::query("delete from preview_sessions where application_id = $1")
            .bind(application_id)
            .execute(&self.pool)
            .await?;
        sqlx::query("delete from preview_codes where application_id = $1")
            .bind(application_id)
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "update workspaces set desired_state = 'deleted', \
             desired_revision = case \
                 when desired_state = 'deleted' then desired_revision \
                 else desired_revision + 1 \
             end, \
             reconcile_after = now() \
             from applications \
             where workspaces.id = applications.workspace_id \
               and applications.id = $1 \
               and workspaces.desired_state <> 'deleted' \
               and not exists ( \
                   select 1 from applications remaining \
                   where remaining.workspace_id = workspaces.id \
                     and remaining.id <> applications.id \
                     and remaining.state not in ('deleted', 'deleting') \
               )",
        )
        .bind(application_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn collect_cleanup(
        &self,
        application: &Application,
        include_data: bool,
    ) -> Result<ApplicationCleanup, ApplicationError> {
        let deploy_rows = sqlx::query(
            "select d.id, e.kind from application_deployments d \
             join application_environments e on e.id = d.environment_id \
             where e.application_id = $1 \
               and d.desired_state <> 'absent'",
        )
        .bind(application.id)
        .fetch_all(&self.pool)
        .await?;
        let deployments = deploy_rows
            .into_iter()
            .map(|row| CleanupDeployment {
                id: row.get("id"),
                kind: row.get("kind"),
            })
            .collect();
        let (databases, releases) = if include_data {
            let databases = sqlx::query_scalar(
                "select id from application_databases where application_id = $1 and desired_state <> 'absent'",
            )
            .bind(application.id)
            .fetch_all(&self.pool)
            .await?;
            let releases =
                sqlx::query_scalar("select id from application_releases where application_id = $1")
                    .bind(application.id)
                    .fetch_all(&self.pool)
                    .await?;
            (databases, releases)
        } else {
            (Vec::new(), Vec::new())
        };
        Ok(ApplicationCleanup {
            slug: application.slug.clone(),
            deployments,
            databases,
            releases,
            workspace_id: None,
        })
    }

    /// Archive keeps Blob restore points and releases local capacity.
    /// Deployments stop; Database and Workspace LVs drop only after this
    /// cleanup runs. Release Blob objects stay.
    pub async fn plan_archive(
        &self,
        actor_user_id: Uuid,
        application_id: Uuid,
    ) -> Result<ApplicationCleanup, ApplicationError> {
        let (application, _) = self
            .require_in_project(actor_user_id, application_id, Action::ManageProduction)
            .await?;
        if application.state == "deleting" {
            return Err(ApplicationError::WorkspaceBusy);
        }
        let mut cleanup = self.collect_cleanup(&application, true).await?;
        cleanup.releases.clear();
        let others: i64 = sqlx::query_scalar(
            "select count(*) from applications \
             where workspace_id = $1 and id <> $2 and state <> 'deleted'",
        )
        .bind(application.workspace_id)
        .bind(application.id)
        .fetch_one(&self.pool)
        .await?;
        if others == 0 {
            cleanup.workspace_id = Some(application.workspace_id);
        }
        Ok(cleanup)
    }

    pub async fn begin_archive(
        &self,
        actor_user_id: Uuid,
        application_id: Uuid,
    ) -> Result<String, ApplicationError> {
        let mut tx = self.pool.begin().await?;
        let state = claim_actor(
            &mut tx,
            actor_user_id,
            application_id,
            Action::ManageProduction,
        )
        .await?;
        match state.as_str() {
            "archived" => {
                tx.commit().await?;
                return Ok("archived".into());
            }
            "archiving" => {
                tx.commit().await?;
                return Ok("archiving".into());
            }
            "restoring" | "deleting" => return Err(ApplicationError::WorkspaceBusy),
            "ready" | "suspended" | "creating" => {}
            _ => return Err(ApplicationError::WorkspaceBusy),
        }
        let next_generation: i64 = sqlx::query_scalar(
            "select coalesce(max(generation), 0) + 1 from application_archives \
             where application_id = $1",
        )
        .bind(application_id)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "insert into application_archives \
             (id, application_id, generation, state) values ($1, $2, $3, 'capturing')",
        )
        .bind(Uuid::new_v4())
        .bind(application_id)
        .bind(next_generation)
        .execute(&mut *tx)
        .await?;
        let updated = sqlx::query(
            "update applications set state = 'archiving', updated_at = now() \
             where id = $1 and state in ('ready', 'suspended', 'creating')",
        )
        .bind(application_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if updated == 0 {
            return Err(ApplicationError::WorkspaceBusy);
        }
        tx.commit().await?;
        Ok("archiving".into())
    }

    pub async fn persist_archive_restore_points(
        &self,
        application_id: Uuid,
        workspace_snapshot_id: Option<Uuid>,
        dev_database_backup_id: Option<Uuid>,
        prod_database_backup_id: Option<Uuid>,
        dev_release_id: Option<Uuid>,
        prod_release_id: Option<Uuid>,
    ) -> Result<(), ApplicationError> {
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query(
            "update application_archives set \
                workspace_snapshot_id = coalesce($2, workspace_snapshot_id), \
                dev_database_backup_id = coalesce($3, dev_database_backup_id), \
                prod_database_backup_id = coalesce($4, prod_database_backup_id), \
                dev_release_id = coalesce($5, dev_release_id), \
                prod_release_id = coalesce($6, prod_release_id) \
             where application_id = $1 and state = 'capturing'",
        )
        .bind(application_id)
        .bind(workspace_snapshot_id)
        .bind(dev_database_backup_id)
        .bind(prod_database_backup_id)
        .bind(dev_release_id)
        .bind(prod_release_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if updated == 0 {
            return Err(ApplicationError::WorkspaceBusy);
        }
        if let Some(snapshot_id) = workspace_snapshot_id {
            sqlx::query("update workspace_snapshots set pinned = true where id = $1")
                .bind(snapshot_id)
                .execute(&mut *tx)
                .await?;
        }
        for backup_id in [dev_database_backup_id, prod_database_backup_id]
            .into_iter()
            .flatten()
        {
            sqlx::query("update database_backups set pinned = true where id = $1")
                .bind(backup_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn begin_workspace_grow_tx(
        tx: &mut Transaction<'_, Postgres>,
        workspace_id: Uuid,
        target_bytes: i64,
    ) -> Result<Uuid, ApplicationError> {
        let existing = sqlx::query(
            "select operation_id, target_bytes from workspace_grow_operations \
             where workspace_id = $1 and state = 'dispatched' for update",
        )
        .bind(workspace_id)
        .fetch_optional(&mut **tx)
        .await?;
        if let Some(row) = existing {
            let operation_id: Uuid = row.get("operation_id");
            let stored: i64 = row.get("target_bytes");
            if stored != target_bytes {
                return Err(ApplicationError::WorkspaceBusy);
            }
            return Ok(operation_id);
        }
        let operation_id = Uuid::new_v4();
        sqlx::query(
            "insert into workspace_grow_operations \
             (workspace_id, operation_id, target_bytes, state) \
             values ($1, $2, $3, 'dispatched')",
        )
        .bind(workspace_id)
        .bind(operation_id)
        .bind(target_bytes)
        .execute(&mut **tx)
        .await?;
        Ok(operation_id)
    }

    /// User-row serialization, membership, approval, and the durable grow
    /// claim in one transaction. Fabric I/O happens after commit.
    pub async fn accept_elevated_workspace_grow(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        approval_id: Uuid,
        target_bytes: i64,
    ) -> Result<Uuid, ApplicationError> {
        let mut tx = self.pool.begin().await?;
        crate::Kernel::lock_user_row(&mut tx, actor_user_id).await?;
        let status: Option<String> = sqlx::query_scalar("select status from users where id = $1")
            .bind(actor_user_id)
            .fetch_optional(&mut *tx)
            .await?;
        if status.as_deref() != Some("active") {
            return Err(ApplicationError::Auth);
        }
        let workspace = sqlx::query(
            "select project_id, allocated_bytes from workspaces where id = $1 for update",
        )
        .bind(workspace_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApplicationError::NotFound)?;
        let project_id: Uuid = workspace.get("project_id");
        let allocated: i64 = workspace.get("allocated_bytes");
        let role_text: Option<String> = sqlx::query_scalar(
            "select role from project_members where user_id = $1 and project_id = $2",
        )
        .bind(actor_user_id)
        .bind(project_id)
        .fetch_optional(&mut *tx)
        .await?;
        let permitted = role_text
            .as_deref()
            .and_then(crate::auth::Role::parse)
            .is_some_and(|role| role.permits(crate::auth::Action::OperateSession));
        if !permitted {
            return Err(ApplicationError::Auth);
        }
        lock_project(&mut tx, project_id).await?;
        if allocated != crate::storage::WORKSPACE_LARGE_BYTES {
            return Err(ApplicationError::WorkspaceBusy);
        }
        let application_id: Option<Uuid> =
            sqlx::query_scalar("select id from applications where workspace_id = $1")
                .bind(workspace_id)
                .fetch_optional(&mut *tx)
                .await?;
        let target = ApprovalTarget {
            application_id,
            ..Default::default()
        };
        require_approval_tx(
            &mut tx,
            approval_id,
            project_id,
            "increase_resource_tier",
            &target,
            actor_user_id,
        )
        .await?;
        let operation_id =
            Self::begin_workspace_grow_tx(&mut tx, workspace_id, target_bytes).await?;
        tx.commit().await?;
        Ok(operation_id)
    }

    pub async fn complete_workspace_grow(
        &self,
        workspace_id: Uuid,
        operation_id: Uuid,
    ) -> Result<(), ApplicationError> {
        let updated = sqlx::query(
            "update workspace_grow_operations set state = 'ready' \
             where workspace_id = $1 and operation_id = $2 and state = 'dispatched'",
        )
        .bind(workspace_id)
        .bind(operation_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if updated == 0 {
            return Err(ApplicationError::WorkspaceBusy);
        }
        Ok(())
    }

    pub async fn commit_archive(
        &self,
        actor_user_id: Uuid,
        application_id: Uuid,
        workspace_snapshot_id: Option<Uuid>,
        dev_database_backup_id: Option<Uuid>,
        prod_database_backup_id: Option<Uuid>,
        dev_release_id: Option<Uuid>,
        prod_release_id: Option<Uuid>,
    ) -> Result<(), ApplicationError> {
        let mut tx = self.pool.begin().await?;
        let state = claim_actor(
            &mut tx,
            actor_user_id,
            application_id,
            Action::ManageProduction,
        )
        .await?;
        if state == "archived" {
            tx.commit().await?;
            return Ok(());
        }
        if state != "archiving" {
            return Err(ApplicationError::WorkspaceBusy);
        }
        let capturing = sqlx::query(
            "update application_archives set \
                workspace_snapshot_id = $2, \
                dev_database_backup_id = $3, \
                prod_database_backup_id = $4, \
                dev_release_id = $5, \
                prod_release_id = $6 \
             where application_id = $1 and state = 'capturing'",
        )
        .bind(application_id)
        .bind(workspace_snapshot_id)
        .bind(dev_database_backup_id)
        .bind(prod_database_backup_id)
        .bind(dev_release_id)
        .bind(prod_release_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if capturing == 0 {
            return Err(ApplicationError::WorkspaceBusy);
        }
        sqlx::query(
            "update application_archives set state = 'superseded' \
             where application_id = $1 and state = 'complete'",
        )
        .bind(application_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "update application_archives set state = 'complete' \
             where application_id = $1 and state = 'capturing'",
        )
        .bind(application_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "update application_deployments \
             set desired_state = 'absent', \
                 desired_revision = case \
                     when desired_state = 'absent' then desired_revision \
                     else desired_revision + 1 \
                 end, \
                 reconcile_after = now() \
             where environment_id in (select id from application_environments where application_id = $1) \
               and desired_state <> 'absent'",
        )
        .bind(application_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "update application_environments \
             set desired_deployment_id = null, \
                 revision = case \
                     when desired_deployment_id is null then revision \
                     else revision + 1 \
                 end \
             where application_id = $1",
        )
        .bind(application_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "update application_databases \
             set desired_state = 'absent', \
                 desired_revision = case \
                     when desired_state = 'absent' then desired_revision \
                     else desired_revision + 1 \
                 end, \
                 reconcile_after = now() \
             where application_id = $1 and desired_state <> 'absent'",
        )
        .bind(application_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "update workspaces set desired_state = 'archived', \
             desired_revision = case \
                 when desired_state = 'archived' then desired_revision \
                 else desired_revision + 1 \
             end, \
             reconcile_after = now() \
             from applications \
             where workspaces.id = applications.workspace_id \
               and applications.id = $1 \
               and workspaces.desired_state <> 'deleted'",
        )
        .bind(application_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "update applications set state = 'archived', updated_at = now() \
             where id = $1 and state in ('archiving', 'archived')",
        )
        .bind(application_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "update workspace_snapshots set pinned = false \
             where id in ( \
                 select workspace_snapshot_id from application_archives \
                 where application_id = $1 and state = 'superseded' \
                   and workspace_snapshot_id is not null \
             ) \
             and id not in ( \
                 select workspace_snapshot_id from application_archives \
                 where application_id = $1 and state = 'complete' \
                   and workspace_snapshot_id is not null \
             )",
        )
        .bind(application_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "update database_backups set pinned = false \
             where id in ( \
                 select dev_database_backup_id from application_archives \
                 where application_id = $1 and state = 'superseded' \
                   and dev_database_backup_id is not null \
                 union \
                 select prod_database_backup_id from application_archives \
                 where application_id = $1 and state = 'superseded' \
                   and prod_database_backup_id is not null \
             ) \
             and id not in ( \
                 select dev_database_backup_id from application_archives \
                 where application_id = $1 and state = 'complete' \
                   and dev_database_backup_id is not null \
                 union \
                 select prod_database_backup_id from application_archives \
                 where application_id = $1 and state = 'complete' \
                   and prod_database_backup_id is not null \
             )",
        )
        .bind(application_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn get_archive(
        &self,
        application_id: Uuid,
    ) -> Result<Option<ApplicationArchive>, ApplicationError> {
        self.archive_by_state(application_id, "complete").await
    }

    pub async fn capturing_archive(
        &self,
        application_id: Uuid,
    ) -> Result<Option<ApplicationArchive>, ApplicationError> {
        self.archive_by_state(application_id, "capturing").await
    }

    async fn archive_by_state(
        &self,
        application_id: Uuid,
        state: &str,
    ) -> Result<Option<ApplicationArchive>, ApplicationError> {
        let row = sqlx::query(
            "select id, application_id, generation, state, workspace_snapshot_id, \
                    dev_database_backup_id, prod_database_backup_id, \
                    dev_release_id, prod_release_id \
             from application_archives where application_id = $1 and state = $2",
        )
        .bind(application_id)
        .bind(state)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(row_archive))
    }

    pub async fn begin_restore(
        &self,
        actor_user_id: Uuid,
        application_id: Uuid,
    ) -> Result<String, ApplicationError> {
        let mut tx = self.pool.begin().await?;
        let state = claim_actor(
            &mut tx,
            actor_user_id,
            application_id,
            Action::ManageProduction,
        )
        .await?;
        match state.as_str() {
            "ready" => {
                let has_complete: bool = sqlx::query_scalar(
                    "select exists(select 1 from application_archives \
                     where application_id = $1 and state = 'complete')",
                )
                .bind(application_id)
                .fetch_one(&mut *tx)
                .await?;
                tx.commit().await?;
                if has_complete {
                    return Ok("ready".into());
                }
                return Err(ApplicationError::WorkspaceBusy);
            }
            "restoring" => {
                tx.commit().await?;
                return Ok("restoring".into());
            }
            "archived" => {}
            _ => return Err(ApplicationError::WorkspaceBusy),
        }
        let updated = sqlx::query(
            "update applications set state = 'restoring', updated_at = now() \
             where id = $1 and state = 'archived'",
        )
        .bind(application_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if updated == 0 {
            return Err(ApplicationError::WorkspaceBusy);
        }
        tx.commit().await?;
        Ok("restoring".into())
    }

    pub async fn commit_restore(
        &self,
        actor_user_id: Uuid,
        application_id: Uuid,
    ) -> Result<(), ApplicationError> {
        let (application, _) = self
            .require_in_project(actor_user_id, application_id, Action::ManageProduction)
            .await?;
        if application.state == "ready" {
            return Ok(());
        }
        if application.state != "restoring" {
            return Err(ApplicationError::WorkspaceBusy);
        }
        sqlx::query("update applications set state = 'ready', updated_at = now() where id = $1")
            .bind(application_id)
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "update application_environments set state = 'ready' where application_id = $1",
        )
        .bind(application_id)
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "update workspaces set desired_state = 'active', \
             desired_revision = case \
                 when desired_state = 'active' then desired_revision \
                 else desired_revision + 1 \
             end, \
             reconcile_after = now() \
             from applications \
             where workspaces.id = applications.workspace_id \
               and applications.id = $1 \
               and workspaces.desired_state <> 'deleted'",
        )
        .bind(application_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn validate_name(name: &str) -> Result<String, ApplicationError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > 80 {
        return Err(ApplicationError::InvalidName);
    }
    Ok(trimmed.to_owned())
}

fn validate_root(path: &str) -> Result<String, ApplicationError> {
    if path == "." {
        return Ok(".".to_owned());
    }
    if path.starts_with('/') || path.contains("..") || path.contains('\0') {
        return Err(ApplicationError::InvalidRoot);
    }
    Ok(path.to_owned())
}

pub(crate) fn application_lock_key(application_id: Uuid) -> i64 {
    (application_id.as_u128() as u64) as i64
}

pub(crate) async fn lock_application(
    tx: &mut Transaction<'_, Postgres>,
    application_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_advisory_xact_lock($1)")
        .bind(application_lock_key(application_id))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Serialize privileged Application effects on the same User row that
/// disable and membership removal lock. Re-reads active User + membership
/// after that lock, then locks Project and Application before the caller
/// persists the durable claim.
pub(crate) async fn claim_actor(
    tx: &mut Transaction<'_, Postgres>,
    actor_user_id: Uuid,
    application_id: Uuid,
    action: Action,
) -> Result<String, ApplicationError> {
    crate::Kernel::lock_user_row(tx, actor_user_id).await?;
    let status: Option<String> = sqlx::query_scalar("select status from users where id = $1")
        .bind(actor_user_id)
        .fetch_optional(&mut **tx)
        .await?;
    if status.as_deref() != Some("active") {
        return Err(ApplicationError::Auth);
    }
    let application =
        sqlx::query("select project_id, state from applications where id = $1 for update")
            .bind(application_id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or(ApplicationError::NotFound)?;
    let project_id: Uuid = application.get("project_id");
    let state: String = application.get("state");
    let role_text: Option<String> = sqlx::query_scalar(
        "select role from project_members where user_id = $1 and project_id = $2",
    )
    .bind(actor_user_id)
    .bind(project_id)
    .fetch_optional(&mut **tx)
    .await?;
    let permitted = role_text
        .as_deref()
        .and_then(Role::parse)
        .is_some_and(|role| role.permits(action));
    if !permitted {
        return Err(ApplicationError::Auth);
    }
    lock_project(tx, project_id).await?;
    lock_application(tx, application_id).await?;
    Ok(state)
}

pub(crate) async fn lock_project(
    tx: &mut Transaction<'_, Postgres>,
    project_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("select pg_advisory_xact_lock($1)")
        .bind(application_lock_key(project_id))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Application advisory lock plus a live-state check. `deleting` rows are
/// gone from the mutation surface; cleanup paths must use `lock_application`
/// instead so they can still reclaim.
pub(crate) async fn require_live_application(
    tx: &mut Transaction<'_, Postgres>,
    application_id: Uuid,
) -> Result<(), ApplicationError> {
    lock_application(tx, application_id).await?;
    let state: Option<String> = sqlx::query_scalar("select state from applications where id = $1")
        .bind(application_id)
        .fetch_optional(&mut **tx)
        .await?;
    match state.as_deref() {
        Some("deleting") | None => Err(ApplicationError::NotFound),
        Some("archiving" | "archived" | "restoring") => Err(ApplicationError::WorkspaceBusy),
        Some(_) => Ok(()),
    }
}

pub(crate) async fn require_restoring_application(
    tx: &mut Transaction<'_, Postgres>,
    application_id: Uuid,
) -> Result<(), ApplicationError> {
    lock_application(tx, application_id).await?;
    let state: Option<String> = sqlx::query_scalar("select state from applications where id = $1")
        .bind(application_id)
        .fetch_optional(&mut **tx)
        .await?;
    match state.as_deref() {
        Some("restoring") => Ok(()),
        Some("deleting") | None => Err(ApplicationError::NotFound),
        Some(_) => Err(ApplicationError::WorkspaceBusy),
    }
}

async fn insert_application(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    project_id: Uuid,
    workspace_id: Uuid,
    name: &str,
    slug: &str,
    root_path: &str,
    actor: Uuid,
) -> Result<Application, ApplicationError> {
    crate::Kernel::lock_user_row(tx, actor).await?;
    let lock_key: i64 = (project_id.as_u128() as u64) as i64;
    sqlx::query("select pg_advisory_xact_lock($1)")
        .bind(lock_key)
        .execute(&mut **tx)
        .await?;
    let count: i64 = sqlx::query_scalar(
        "select count(*) from applications where project_id = $1 and state <> 'deleting'",
    )
    .bind(project_id)
    .fetch_one(&mut **tx)
    .await?;
    if count >= MAX_APPLICATIONS_PER_PROJECT {
        return Err(ApplicationError::Kernel(KernelError::Quota));
    }
    let created: i64 = sqlx::query_scalar(
        "select count(*) from applications \
         where created_by_user_id = $1 and state <> 'deleting'",
    )
    .bind(actor)
    .fetch_one(&mut **tx)
    .await?;
    if created >= crate::MAX_APPLICATIONS_PER_USER {
        return Err(ApplicationError::Kernel(KernelError::Quota));
    }
    let row = sqlx::query(
        "insert into applications \
         (id, project_id, workspace_id, name, slug, root_path, runtime_profile, state, created_by_user_id) \
         values ($1, $2, $3, $4, $5, $6, $7, 'ready', $8) \
         returning id, project_id, workspace_id, name, slug, root_path, runtime_profile, \
                   state, created_by_user_id, created_at::text as created_at, \
                   updated_at::text as updated_at",
    )
    .bind(id)
    .bind(project_id)
    .bind(workspace_id)
    .bind(name)
    .bind(slug)
    .bind(root_path)
    .bind(DEFAULT_RUNTIME)
    .bind(actor)
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| match &error {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => {
            ApplicationError::Kernel(KernelError::Conflict)
        }
        _ => ApplicationError::from(error),
    })?;
    Ok(row_application(row))
}

async fn insert_application_allocated(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    project_id: Uuid,
    workspace_id: Uuid,
    name: &str,
    root_path: &str,
    actor: Uuid,
) -> Result<Application, ApplicationError> {
    for _ in 0..8 {
        let slug = slug::allocate(name);
        match insert_application(
            tx,
            id,
            project_id,
            workspace_id,
            name,
            &slug,
            root_path,
            actor,
        )
        .await
        {
            Ok(row) => return Ok(row),
            Err(ApplicationError::Kernel(KernelError::Conflict)) => continue,
            Err(error) => return Err(error),
        }
    }
    Err(ApplicationError::Kernel(KernelError::Conflict))
}

async fn retire_traffic_desired(pool: &PgPool, application_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update application_environments \
         set desired_deployment_id = null, \
             revision = case \
                 when desired_deployment_id is null then revision \
                 else revision + 1 \
             end \
         where application_id = $1",
    )
    .bind(application_id)
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_environments(
    tx: &mut Transaction<'_, Postgres>,
    application_id: Uuid,
    slug: &str,
    console_host: &str,
) -> Result<Vec<Environment>, ApplicationError> {
    let mut environments = Vec::new();
    for (kind, visibility) in [("dev", "private"), ("prod", "public")] {
        let hostname = format!("{slug}.{kind}.{console_host}");
        let row = sqlx::query(
            "insert into application_environments \
             (id, application_id, kind, visibility, hostname, state) \
             values ($1, $2, $3, $4, $5, 'ready') \
             returning id, application_id, kind, visibility, hostname, revision, \
                       active_deployment_id, desired_deployment_id, observed_deployment_id, \
                       traffic_observed_revision, state",
        )
        .bind(Uuid::new_v4())
        .bind(application_id)
        .bind(kind)
        .bind(visibility)
        .bind(hostname)
        .fetch_one(&mut **tx)
        .await?;
        environments.push(row_environment(row));
    }
    Ok(environments)
}

async fn load(pool: &PgPool, id: Uuid) -> Result<Option<Application>, sqlx::Error> {
    let row = sqlx::query(
        "select id, project_id, workspace_id, name, slug, root_path, runtime_profile, \
                state, created_by_user_id, created_at::text as created_at, \
                updated_at::text as updated_at \
         from applications where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(row_application))
}

async fn load_environments(
    pool: &PgPool,
    application_id: Uuid,
) -> Result<Vec<Environment>, sqlx::Error> {
    let rows = sqlx::query(
        "select id, application_id, kind, visibility, hostname, revision, \
                active_deployment_id, desired_deployment_id, observed_deployment_id, \
                traffic_observed_revision, state \
         from application_environments where application_id = $1 order by kind",
    )
    .bind(application_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(row_environment).collect())
}

pub async fn load_environment(pool: &PgPool, id: Uuid) -> Result<Option<Environment>, sqlx::Error> {
    let row = sqlx::query(
        "select id, application_id, kind, visibility, hostname, revision, \
                active_deployment_id, desired_deployment_id, observed_deployment_id, \
                traffic_observed_revision, state \
         from application_environments where id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(row_environment))
}

pub fn request_hash(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
        hasher.update([0]);
    }
    hasher.finalize().into()
}

/// Material targets of one approval. The canonical hash is immutable after
/// insert; acceptance never rewrites it.
#[derive(Debug, Clone, Default)]
pub struct ApprovalTarget {
    pub application_id: Option<Uuid>,
    pub environment_id: Option<Uuid>,
    pub release_id: Option<Uuid>,
    pub secret_id: Option<Uuid>,
    pub environment_name: Option<String>,
    pub database_id: Option<Uuid>,
    pub backup_id: Option<Uuid>,
    pub archive_generation: Option<i64>,
    pub workspace_snapshot_id: Option<Uuid>,
    pub dev_database_id: Option<Uuid>,
    pub dev_backup_id: Option<Uuid>,
    pub prod_database_id: Option<Uuid>,
    pub prod_backup_id: Option<Uuid>,
    pub dev_release_id: Option<Uuid>,
    pub prod_release_id: Option<Uuid>,
}

fn field(hasher: &mut Sha256, name: &str, value: &[u8]) {
    hasher.update(name.as_bytes());
    hasher.update([0]);
    hasher.update((value.len() as u32).to_be_bytes());
    hasher.update(value);
    hasher.update([0]);
}

fn uuid_bytes(id: Option<Uuid>) -> [u8; 16] {
    id.map(|value| *value.as_bytes()).unwrap_or([0; 16])
}

pub fn canonical_action_hash(kind: &str, target: &ApprovalTarget) -> [u8; 32] {
    let application = uuid_bytes(target.application_id);
    let environment = uuid_bytes(target.environment_id);
    let release = uuid_bytes(target.release_id);
    let secret = uuid_bytes(target.secret_id);
    let database = uuid_bytes(target.database_id);
    let backup = uuid_bytes(target.backup_id);
    let mut hasher = Sha256::new();
    field(&mut hasher, "kind", kind.as_bytes());
    field(
        &mut hasher,
        "application_id",
        if target.application_id.is_some() {
            application.as_slice()
        } else {
            &[]
        },
    );
    field(
        &mut hasher,
        "environment_id",
        if target.environment_id.is_some() {
            environment.as_slice()
        } else {
            &[]
        },
    );
    field(
        &mut hasher,
        "release_id",
        if target.release_id.is_some() {
            release.as_slice()
        } else {
            &[]
        },
    );
    field(
        &mut hasher,
        "secret_id",
        if target.secret_id.is_some() {
            secret.as_slice()
        } else {
            &[]
        },
    );
    field(
        &mut hasher,
        "environment_name",
        target.environment_name.as_deref().unwrap_or("").as_bytes(),
    );
    field(
        &mut hasher,
        "database_id",
        if target.database_id.is_some() {
            database.as_slice()
        } else {
            &[]
        },
    );
    field(
        &mut hasher,
        "backup_id",
        if target.backup_id.is_some() {
            backup.as_slice()
        } else {
            &[]
        },
    );
    if let Some(generation) = target.archive_generation {
        field(&mut hasher, "archive_generation", &generation.to_be_bytes());
    }
    if let Some(id) = target.workspace_snapshot_id {
        field(&mut hasher, "workspace_snapshot_id", id.as_bytes());
    }
    if let Some(id) = target.dev_database_id {
        field(&mut hasher, "dev_database_id", id.as_bytes());
    }
    if let Some(id) = target.dev_backup_id {
        field(&mut hasher, "dev_backup_id", id.as_bytes());
    }
    if let Some(id) = target.prod_database_id {
        field(&mut hasher, "prod_database_id", id.as_bytes());
    }
    if let Some(id) = target.prod_backup_id {
        field(&mut hasher, "prod_backup_id", id.as_bytes());
    }
    if let Some(id) = target.dev_release_id {
        field(&mut hasher, "dev_release_id", id.as_bytes());
    }
    if let Some(id) = target.prod_release_id {
        field(&mut hasher, "prod_release_id", id.as_bytes());
    }
    hasher.finalize().into()
}

pub async fn require_approval(
    pool: &PgPool,
    approval_id: Option<Uuid>,
    project_id: Uuid,
    kind: &str,
    target: &ApprovalTarget,
    actor_user_id: Uuid,
) -> Result<Uuid, ApplicationError> {
    let action_hash = canonical_action_hash(kind, target);
    let Some(approval_id) = approval_id else {
        let pending = Uuid::new_v4();
        sqlx::query(
            "insert into approval_requests \
             (id, project_id, application_id, environment_id, release_id, kind, action_hash, state, requested_by) \
             values ($1, $2, $3, $4, $5, $6, $7, 'pending', $8)",
        )
        .bind(pending)
        .bind(project_id)
        .bind(target.application_id)
        .bind(target.environment_id)
        .bind(target.release_id)
        .bind(kind)
        .bind(action_hash.as_slice())
        .bind(actor_user_id)
        .execute(pool)
        .await?;
        return Err(ApplicationError::ApprovalRequired(pending));
    };
    let accepted: Option<Uuid> = sqlx::query_scalar(
        "select id from approval_requests \
         where id = $1 and project_id = $2 and kind = $3 and state = 'accepted' \
           and action_hash = $4 \
           and ($5::uuid is null or application_id is null or application_id = $5) \
           and ($6::uuid is null or environment_id is null or environment_id = $6) \
           and ($7::uuid is null or release_id is null or release_id = $7)",
    )
    .bind(approval_id)
    .bind(project_id)
    .bind(kind)
    .bind(action_hash.as_slice())
    .bind(target.application_id)
    .bind(target.environment_id)
    .bind(target.release_id)
    .fetch_optional(pool)
    .await?;
    accepted.ok_or(ApplicationError::Auth)
}

pub async fn require_approval_tx(
    tx: &mut Transaction<'_, Postgres>,
    approval_id: Uuid,
    project_id: Uuid,
    kind: &str,
    target: &ApprovalTarget,
    _actor_user_id: Uuid,
) -> Result<Uuid, ApplicationError> {
    let action_hash = canonical_action_hash(kind, target);
    let accepted: Option<Uuid> = sqlx::query_scalar(
        "select id from approval_requests \
         where id = $1 and project_id = $2 and kind = $3 and state = 'accepted' \
           and action_hash = $4 \
           and ($5::uuid is null or application_id is null or application_id = $5) \
           and ($6::uuid is null or environment_id is null or environment_id = $6) \
           and ($7::uuid is null or release_id is null or release_id = $7)",
    )
    .bind(approval_id)
    .bind(project_id)
    .bind(kind)
    .bind(action_hash.as_slice())
    .bind(target.application_id)
    .bind(target.environment_id)
    .bind(target.release_id)
    .fetch_optional(&mut **tx)
    .await?;
    accepted.ok_or(ApplicationError::Auth)
}

pub async fn accept_approval(
    pool: &PgPool,
    approval_id: Uuid,
    actor_user_id: Uuid,
) -> Result<(), ApplicationError> {
    let updated = sqlx::query(
        "update approval_requests set state = 'accepted', accepted_by = $2, resolved_at = now() \
         where id = $1 and state = 'pending'",
    )
    .bind(approval_id)
    .bind(actor_user_id)
    .execute(pool)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApplicationError::NotFound);
    }
    Ok(())
}

fn row_application(row: sqlx::postgres::PgRow) -> Application {
    Application {
        id: row.get("id"),
        project_id: row.get("project_id"),
        workspace_id: row.get("workspace_id"),
        name: row.get("name"),
        slug: row.get("slug"),
        root_path: row.get("root_path"),
        runtime_profile: row.get("runtime_profile"),
        state: row.get("state"),
        created_by_user_id: row.get("created_by_user_id"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

fn row_archive(row: sqlx::postgres::PgRow) -> ApplicationArchive {
    ApplicationArchive {
        id: row.get("id"),
        application_id: row.get("application_id"),
        generation: row.get("generation"),
        state: row.get("state"),
        workspace_snapshot_id: row.get("workspace_snapshot_id"),
        dev_database_backup_id: row.get("dev_database_backup_id"),
        prod_database_backup_id: row.get("prod_database_backup_id"),
        dev_release_id: row.get("dev_release_id"),
        prod_release_id: row.get("prod_release_id"),
    }
}

fn row_environment(row: sqlx::postgres::PgRow) -> Environment {
    Environment {
        id: row.get("id"),
        application_id: row.get("application_id"),
        kind: row.get("kind"),
        visibility: row.get("visibility"),
        hostname: row.get("hostname"),
        revision: row.get("revision"),
        active_deployment_id: row.get("active_deployment_id"),
        desired_deployment_id: row.get("desired_deployment_id"),
        observed_deployment_id: row.get("observed_deployment_id"),
        traffic_observed_revision: row.get("traffic_observed_revision"),
        state: row.get("state"),
    }
}
