//! Same-origin HTTP surface for Application platform resources.

mod orchestrate;
mod tools;

use bytes::Bytes;
use hyper::header::HeaderValue;
use hyper::{Method, Response, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::applications::{self, ApplicationError, ApplicationStore};
use crate::auth::Action;
use crate::databases::DatabaseStore;
use crate::deployments::{BeginDeployment, DeploymentStore};
use crate::environment_bindings::BindingStore;
use crate::preview_auth::{PREVIEW_COOKIE, PreviewAuth};
use crate::releases::{BeginRelease, ReleaseStore};

pub use orchestrate::ProductRuntime;

#[derive(Clone)]
pub struct Platform {
    pub applications: ApplicationStore,
    pub releases: ReleaseStore,
    pub deployments: DeploymentStore,
    pub databases: DatabaseStore,
    pub bindings: BindingStore,
    pub preview: PreviewAuth,
    pub fabric_id: Option<Uuid>,
    pub(crate) runtime: Option<ProductRuntime>,
}

impl Platform {
    pub fn new(pool: PgPool, console_host: String, fabric_id: Option<Uuid>) -> Self {
        Platform {
            applications: ApplicationStore::new(pool.clone(), console_host),
            releases: ReleaseStore::new(pool.clone()),
            deployments: DeploymentStore::new(pool.clone()),
            databases: DatabaseStore::new(pool.clone()),
            bindings: BindingStore::new(pool.clone()),
            preview: PreviewAuth::new(pool),
            fabric_id,
            runtime: None,
        }
    }

    /// The enrolled Workspace Fabric is the authority. `VOIE_FABRIC_ID` is
    /// only a bootstrap default; a missing env id must not block Database
    /// create on a Workspace that already has a fabric_id.
    async fn fabric_id_for_workspace(&self, workspace_id: Uuid) -> Result<Uuid, ApplicationError> {
        let from_workspace: Option<Uuid> =
            sqlx::query_scalar("select fabric_id from workspaces where id = $1")
                .bind(workspace_id)
                .fetch_optional(self.applications.pool())
                .await?;
        from_workspace
            .or(self.fabric_id)
            .ok_or(ApplicationError::WorkspaceMissing)
    }

    pub async fn route(
        &self,
        user_id: Uuid,
        method: &Method,
        segments: &[&str],
        body: &[u8],
        query: &str,
        host: Option<&str>,
        cookie_header: Option<&str>,
    ) -> Option<Response<http_body_util::Full<Bytes>>> {
        match (method, segments) {
            (&Method::POST, ["api", "projects", project_id, "applications"]) => {
                Some(self.create_application(user_id, project_id, body).await)
            }
            (&Method::GET, ["api", "projects", project_id, "applications"]) => {
                Some(self.list_applications(user_id, project_id).await)
            }
            (&Method::GET, ["api", "applications", application_id]) => {
                Some(self.get_application(user_id, application_id).await)
            }
            (&Method::PATCH, ["api", "applications", application_id]) => {
                Some(self.patch_application(user_id, application_id, body).await)
            }
            (&Method::DELETE, ["api", "applications", application_id]) => {
                Some(self.delete_application(user_id, application_id, body).await)
            }
            (&Method::GET, ["api", "applications", application_id, "approvals"]) => {
                Some(self.list_approvals(user_id, application_id).await)
            }
            (&Method::POST, ["api", "approvals", approval_id, "accept"]) => {
                Some(self.accept_approval(user_id, approval_id).await)
            }
            (&Method::POST, ["api", "applications", application_id, "releases"]) => {
                Some(self.create_release(user_id, application_id, body).await)
            }
            (&Method::GET, ["api", "applications", application_id, "releases"]) => {
                Some(self.list_releases(user_id, application_id).await)
            }
            (&Method::GET, ["api", "releases", release_id]) => {
                Some(self.get_release(user_id, release_id).await)
            }
            (&Method::DELETE, ["api", "releases", release_id]) => {
                Some(self.delete_release(user_id, release_id).await)
            }
            (&Method::GET, ["api", "applications", application_id, "environments"]) => {
                Some(self.list_environments(user_id, application_id).await)
            }
            (&Method::PATCH, ["api", "environments", environment_id]) => {
                Some(self.patch_environment(user_id, environment_id, body).await)
            }
            (&Method::POST, ["api", "environments", environment_id, "deployments"]) => {
                Some(self.create_deployment(user_id, environment_id, body).await)
            }
            (&Method::GET, ["api", "environments", environment_id, "deployments"]) => {
                Some(self.list_deployments(user_id, environment_id).await)
            }
            (&Method::GET, ["api", "deployments", deployment_id]) => {
                Some(self.get_deployment(user_id, deployment_id).await)
            }
            (&Method::POST, ["api", "deployments", deployment_id, "activate"]) => {
                Some(self.activate_http(user_id, deployment_id).await)
            }
            (&Method::POST, ["api", "deployments", deployment_id, "rollback"]) => {
                Some(self.rollback(user_id, deployment_id, body).await)
            }
            (&Method::POST, ["api", "deployments", deployment_id, "restart"]) => {
                Some(self.restart(user_id, deployment_id).await)
            }
            (&Method::POST, ["api", "deployments", deployment_id, "stop"]) => {
                Some(self.stop(user_id, deployment_id).await)
            }
            (&Method::GET, ["api", "deployments", deployment_id, "logs"]) => {
                Some(self.deployment_logs(user_id, deployment_id).await)
            }
            (&Method::POST, ["api", "environments", environment_id, "database"]) => {
                Some(self.create_database(user_id, environment_id, body).await)
            }
            (&Method::GET, ["api", "environments", environment_id, "database"]) => {
                Some(self.get_environment_database(user_id, environment_id).await)
            }
            (&Method::GET, ["api", "databases", database_id]) => {
                Some(self.get_database(user_id, database_id).await)
            }
            (&Method::DELETE, ["api", "databases", database_id]) => {
                Some(self.delete_database(user_id, database_id, body).await)
            }
            (&Method::POST, ["api", "databases", database_id, "backups"]) => {
                Some(self.create_backup(user_id, database_id).await)
            }
            (&Method::GET, ["api", "databases", database_id, "backups"]) => {
                Some(self.list_backups(user_id, database_id).await)
            }
            (&Method::POST, ["api", "databases", database_id, "restores"]) => {
                Some(self.restore_backup(user_id, database_id, body).await)
            }
            (&Method::GET, ["api", "applications", application_id, "metrics"]) => {
                Some(self.application_metrics(user_id, application_id).await)
            }
            (&Method::GET, ["api", "environments", environment_id, "secret-bindings"]) => {
                Some(self.list_bindings(user_id, environment_id).await)
            }
            (
                &Method::PUT,
                [
                    "api",
                    "environments",
                    environment_id,
                    "secret-bindings",
                    name,
                ],
            ) => Some(self.put_binding(user_id, environment_id, name, body).await),
            (
                &Method::DELETE,
                [
                    "api",
                    "environments",
                    environment_id,
                    "secret-bindings",
                    name,
                ],
            ) => Some(self.delete_binding(user_id, environment_id, name).await),
            (&Method::GET, ["api", "preview", "login"]) => {
                Some(self.preview_login(user_id, query).await)
            }
            (&Method::GET, [".voie", "auth", "callback"]) => {
                Some(self.preview_callback(query, host).await)
            }
            (
                &Method::GET | &Method::HEAD | &Method::POST,
                ["internal", "preview", "authorize"],
            ) => Some(self.preview_authorize(host, cookie_header).await),
            _ => None,
        }
    }

    async fn create_application(
        &self,
        user_id: Uuid,
        project_id: &str,
        body: &[u8],
    ) -> Response<http_body_util::Full<Bytes>> {
        let project_id = match Uuid::parse_str(project_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        #[derive(Deserialize)]
        struct Payload {
            name: String,
            slug: String,
            workspace_id: Uuid,
            root_path: Option<String>,
        }
        let payload: Payload = match serde_json::from_slice(body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid application payload"),
        };
        if let Err(error) = self.require_profile1_workspace(payload.workspace_id).await {
            return application_error(error);
        }
        match self
            .applications
            .create(
                user_id,
                project_id,
                payload.workspace_id,
                &payload.name,
                &payload.slug,
                payload.root_path.as_deref(),
            )
            .await
        {
            Ok(outcome) => {
                if let Some(handoff) = outcome.workspace_handoff {
                    if let Err(error) = self.realize_workspace_handoff(handoff).await {
                        if matches!(error, ApplicationError::WorkspaceMissing) {
                            self.abort_unrealized_handoff(outcome.application.id, handoff)
                                .await;
                        }
                        return application_error(error);
                    }
                }
                json_response(
                    StatusCode::CREATED,
                    json!({
                        "application": application_json(&outcome.application),
                        "environments": outcome.environments.iter().map(environment_json).collect::<Vec<_>>(),
                        "workspaceHandoff": outcome.workspace_handoff,
                    }),
                )
            }
            Err(error) => application_error(error),
        }
    }

    async fn list_applications(
        &self,
        user_id: Uuid,
        project_id: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let project_id = match Uuid::parse_str(project_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        match self.applications.list(user_id, project_id).await {
            Ok(items) => json_ok(json!({
                "items": items.iter().map(application_json).collect::<Vec<_>>()
            })),
            Err(error) => application_error(error),
        }
    }

    async fn get_application(
        &self,
        user_id: Uuid,
        application_id: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let application_id = match Uuid::parse_str(application_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        match self.applications.get(user_id, application_id).await {
            Ok(application) => json_ok(json!({ "application": application_json(&application) })),
            Err(error) => application_error(error),
        }
    }

    async fn patch_application(
        &self,
        user_id: Uuid,
        application_id: &str,
        body: &[u8],
    ) -> Response<http_body_util::Full<Bytes>> {
        let application_id = match Uuid::parse_str(application_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        #[derive(Deserialize)]
        struct Payload {
            state: String,
            #[serde(default, alias = "approvalId")]
            approval_id: Option<Uuid>,
        }
        let payload: Payload = match serde_json::from_slice(body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid application payload"),
        };
        if payload.state != "suspended" && payload.state != "archived" && payload.state != "ready" {
            return json_error(
                StatusCode::BAD_REQUEST,
                "application state is not suspendable",
            );
        }
        if payload.state == "ready" {
            match self
                .restore_application(user_id, application_id, payload.approval_id)
                .await
            {
                Ok(application) => {
                    json_ok(json!({ "application": application_json(&application) }))
                }
                Err(error) => application_error(error),
            }
        } else if payload.state == "archived" {
            match self.archive_application(user_id, application_id).await {
                Ok(application) => {
                    json_ok(json!({ "application": application_json(&application) }))
                }
                Err(error) => application_error(error),
            }
        } else {
            match self
                .applications
                .plan_suspend(user_id, application_id)
                .await
            {
                Ok(cleanup) => {
                    if let Err(error) = self.cleanup_application_fabric(&cleanup).await {
                        return application_error(error);
                    }
                    if let Err(error) = self
                        .applications
                        .commit_suspend(user_id, application_id)
                        .await
                    {
                        return application_error(error);
                    }
                    match self.applications.get(user_id, application_id).await {
                        Ok(application) => {
                            json_ok(json!({ "application": application_json(&application) }))
                        }
                        Err(error) => application_error(error),
                    }
                }
                Err(error) => application_error(error),
            }
        }
    }

    async fn list_approvals(
        &self,
        user_id: Uuid,
        application_id: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let application_id = match Uuid::parse_str(application_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        match self
            .applications
            .list_approvals(user_id, application_id)
            .await
        {
            Ok(items) => json_ok(json!({
                "items": items.iter().map(approval_json).collect::<Vec<_>>()
            })),
            Err(error) => application_error(error),
        }
    }

    async fn accept_approval(
        &self,
        user_id: Uuid,
        approval_id: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let approval_id = match Uuid::parse_str(approval_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        match self
            .applications
            .accept_pending_approval(user_id, approval_id)
            .await
        {
            Ok(approval) => json_ok(json!({ "approval": approval_json(&approval) })),
            Err(error) => application_error(error),
        }
    }

    async fn delete_application(
        &self,
        user_id: Uuid,
        application_id: &str,
        body: &[u8],
    ) -> Response<http_body_util::Full<Bytes>> {
        let application_id = match Uuid::parse_str(application_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        let approval_id = optional_uuid(body, "approvalId");
        match self
            .applications
            .plan_delete(user_id, application_id, approval_id)
            .await
        {
            Ok(cleanup) => {
                if let Err(error) = self.cleanup_application_fabric(&cleanup).await {
                    return application_error(error);
                }
                let blob = self.runtime.as_ref().map(|runtime| &runtime.blob);
                if let Err(error) = self
                    .releases
                    .reclaim_application_blobs(application_id, blob)
                    .await
                {
                    return application_error(error);
                }
                if let Err(error) = self
                    .databases
                    .reclaim_application_recovery_blobs(application_id, blob)
                    .await
                {
                    return application_error(error);
                }
                if let Err(error) = self.applications.commit_delete(application_id).await {
                    return application_error(error);
                }
                no_content()
            }
            Err(error) => application_error(error),
        }
    }

    async fn create_release(
        &self,
        user_id: Uuid,
        application_id: &str,
        body: &[u8],
    ) -> Response<http_body_util::Full<Bytes>> {
        let application_id = match Uuid::parse_str(application_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        #[derive(Deserialize)]
        struct Payload {
            build_intent_id: Uuid,
            workspace_id: Uuid,
            source_exec_generation: i64,
            manifest: String,
            #[serde(default)]
            approval_id: Option<Uuid>,
        }
        let payload: Payload = match serde_json::from_slice(body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid release payload"),
        };
        let root = self
            .applications
            .get_internal(application_id)
            .await
            .map(|application| application.root_path)
            .unwrap_or_else(|_| ".".into());
        let manifest = match self.read_guest_manifest(payload.workspace_id, &root).await {
            Ok(Some(text)) => text,
            Ok(None) => payload.manifest,
            Err(error) => return application_error(error),
        };
        match self
            .releases
            .begin(
                user_id,
                application_id,
                payload.build_intent_id,
                payload.workspace_id,
                payload.source_exec_generation,
                &manifest,
                payload.approval_id,
            )
            .await
        {
            Ok((BeginRelease::ReadyToDispatch, _)) => {
                self.kick_complete_release(payload.build_intent_id, payload.workspace_id, root);
                json_response(
                    StatusCode::ACCEPTED,
                    json!({ "state": "dispatched", "buildIntentId": payload.build_intent_id }),
                )
            }
            Ok((BeginRelease::Ready { id }, Some(release))) => json_ok(
                json!({ "state": "ready", "release": release_json(&release), "releaseId": id }),
            ),
            Ok((BeginRelease::Ready { id }, None)) => {
                json_ok(json!({ "state": "ready", "releaseId": id }))
            }
            Ok((BeginRelease::Failed { id }, Some(release))) => json_ok(
                json!({ "state": "failed", "release": release_json(&release), "releaseId": id }),
            ),
            Ok((BeginRelease::Failed { id }, None)) => {
                json_ok(json!({ "state": "failed", "releaseId": id }))
            }
            Ok((BeginRelease::OutcomeUnknown, _)) => json_error(
                StatusCode::CONFLICT,
                "release outcome unknown; the intent will not be dispatched again",
            ),
            Ok((BeginRelease::Conflict, _)) => {
                json_error(StatusCode::CONFLICT, "release intent hash conflict")
            }
            Err(error) => application_error(error),
        }
    }

    async fn list_releases(
        &self,
        user_id: Uuid,
        application_id: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let application_id = match Uuid::parse_str(application_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        match self.releases.list(user_id, application_id).await {
            Ok(items) => {
                for item in &items {
                    self.kick_resume_release(item);
                }
                json_ok(json!({ "items": items.iter().map(release_json).collect::<Vec<_>>() }))
            }
            Err(error) => application_error(error),
        }
    }

    async fn get_release(
        &self,
        user_id: Uuid,
        release_id: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let release_id = match Uuid::parse_str(release_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        match self.releases.get(user_id, release_id).await {
            Ok(release) => {
                self.kick_resume_release(&release);
                json_ok(json!({ "release": release_json(&release) }))
            }
            Err(error) => application_error(error),
        }
    }

    async fn delete_release(
        &self,
        user_id: Uuid,
        release_id: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let release_id = match Uuid::parse_str(release_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        let blob = self.runtime.as_ref().map(|runtime| &runtime.blob);
        match self
            .releases
            .drop_unreferenced(user_id, release_id, blob)
            .await
        {
            Ok(()) => no_content(),
            Err(error) => application_error(error),
        }
    }

    async fn list_environments(
        &self,
        user_id: Uuid,
        application_id: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let application_id = match Uuid::parse_str(application_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        match self
            .applications
            .environments(user_id, application_id)
            .await
        {
            Ok(items) => json_ok(json!({
                "items": items.iter().map(environment_json).collect::<Vec<_>>()
            })),
            Err(error) => application_error(error),
        }
    }

    async fn patch_environment(
        &self,
        user_id: Uuid,
        environment_id: &str,
        body: &[u8],
    ) -> Response<http_body_util::Full<Bytes>> {
        let environment_id = match Uuid::parse_str(environment_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        #[derive(Deserialize)]
        struct Payload {
            visibility: String,
            approval_id: Option<Uuid>,
        }
        let payload: Payload = match serde_json::from_slice(body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid environment payload"),
        };
        match self
            .applications
            .set_visibility(
                user_id,
                environment_id,
                &payload.visibility,
                payload.approval_id,
            )
            .await
        {
            Ok(environment) => json_ok(json!({ "environment": environment_json(&environment) })),
            Err(error) => application_error(error),
        }
    }

    async fn create_deployment(
        &self,
        user_id: Uuid,
        environment_id: &str,
        body: &[u8],
    ) -> Response<http_body_util::Full<Bytes>> {
        let environment_id = match Uuid::parse_str(environment_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        #[derive(Deserialize)]
        struct Payload {
            release_id: Uuid,
            deployment_intent_id: Uuid,
            approval_id: Option<Uuid>,
        }
        let payload: Payload = match serde_json::from_slice(body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid deployment payload"),
        };
        match self
            .deployments
            .deploy(
                user_id,
                environment_id,
                payload.release_id,
                payload.deployment_intent_id,
                payload.approval_id,
            )
            .await
        {
            Ok((BeginDeployment::ReadyToDispatch { id }, deployment)) => {
                self.kick_materialize_deployment(id);
                json_response(
                    StatusCode::ACCEPTED,
                    json!({ "state": "materializing", "deploymentId": id, "deployment": deployment_json(&deployment) }),
                )
            }
            Ok((BeginDeployment::Active { id }, deployment)) => json_ok(json!({
                "state": "active",
                "deploymentId": id,
                "deployment": deployment_json(&deployment),
            })),
            Ok((BeginDeployment::OutcomeUnknown, _)) => json_error(
                StatusCode::CONFLICT,
                "deployment outcome unknown; the intent will not be dispatched again",
            ),
            Ok((BeginDeployment::Conflict, _)) => {
                json_error(StatusCode::CONFLICT, "deployment intent hash conflict")
            }
            Ok((BeginDeployment::Failed { id }, deployment)) => json_ok(json!({
                "state": deployment.state,
                "deploymentId": id,
                "deployment": deployment_json(&deployment),
            })),
            Err(error) => application_error(error),
        }
    }

    async fn list_deployments(
        &self,
        user_id: Uuid,
        environment_id: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let environment_id = match Uuid::parse_str(environment_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        match self.deployments.list(user_id, environment_id).await {
            Ok((_, items)) => {
                for item in &items {
                    self.kick_resume_deployment(item);
                }
                json_ok(json!({
                    "items": items.iter().map(deployment_json).collect::<Vec<_>>()
                }))
            }
            Err(error) => application_error(error),
        }
    }

    async fn get_deployment(
        &self,
        user_id: Uuid,
        deployment_id: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let deployment_id = match Uuid::parse_str(deployment_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        match self.deployments.get(user_id, deployment_id).await {
            Ok(deployment) => {
                self.kick_resume_deployment(&deployment);
                json_ok(json!({ "deployment": deployment_json(&deployment) }))
            }
            Err(error) => application_error(error),
        }
    }

    async fn activate_http(
        &self,
        user_id: Uuid,
        deployment_id: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let deployment_id = match Uuid::parse_str(deployment_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        match self.activate_deployment(user_id, deployment_id).await {
            Ok(deployment) => json_ok(json!({ "deployment": deployment_json(&deployment) })),
            Err(error) => application_error(error),
        }
    }

    /// Health-gated cutover. SQL `active` is recorded only after Fabric
    /// applies the Environment Service switch and the public wildcard edge
    /// returns success, or when this process has no Fabric runtime (contract
    /// tests). A live transport or edge failure does not invent an active
    /// Deployment. A failed public probe re-applies the previous selector.
    /// After SQL cutover, predecessor `fabric_stop` must succeed before the
    /// caller treats activation as settled; failure keeps `superseded` and
    /// kicks one immediate retry.
    pub async fn activate_deployment(
        &self,
        user_id: Uuid,
        deployment_id: Uuid,
    ) -> Result<crate::deployments::Deployment, ApplicationError> {
        let deployment = self.deployments.get(user_id, deployment_id).await?;
        let environment = crate::applications::load_environment(
            self.applications.pool(),
            deployment.environment_id,
        )
        .await?
        .ok_or(ApplicationError::NotFound)?;
        let action = if environment.kind == "prod" {
            Action::ManageProduction
        } else {
            Action::DeployDev
        };
        crate::applications::ApplicationStore::new(self.applications.pool().clone(), String::new())
            .require_in_project(user_id, environment.application_id, action)
            .await?;
        if deployment.state == "active" {
            if let Some(predecessor) = deployment.previous_deployment_id {
                self.settle_superseded_predecessor(predecessor).await?;
            }
            return Ok(deployment);
        }
        if deployment.state != "healthy" {
            return Err(ApplicationError::DeploymentNotReady);
        }
        let switched = self.fabric_activate(deployment_id).await?;
        if switched {
            if let Err(error) = self.probe_wildcard_edge(user_id, deployment_id).await {
                if let Some(previous) = deployment.previous_deployment_id {
                    match self.fabric_activate(previous).await {
                        Ok(_) => {}
                        Err(_) => {
                            let _ = self.deployments.hold_unknown(deployment_id).await;
                            return Err(ApplicationError::WorkspaceBusy);
                        }
                    }
                } else {
                    let _ = self.deployments.hold_unknown(deployment_id).await;
                    return Err(ApplicationError::WorkspaceBusy);
                }
                return Err(error);
            }
        }
        let previous = deployment.previous_deployment_id;
        let activated = self.deployments.activate(deployment_id).await?;
        if let Some(predecessor) = previous {
            self.settle_superseded_predecessor(predecessor).await?;
        }
        Ok(activated)
    }

    async fn rollback(
        &self,
        user_id: Uuid,
        deployment_id: &str,
        body: &[u8],
    ) -> Response<http_body_util::Full<Bytes>> {
        let deployment_id = match Uuid::parse_str(deployment_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        #[derive(Deserialize)]
        struct Payload {
            deployment_intent_id: Uuid,
            approval_id: Option<Uuid>,
        }
        let payload: Payload = match serde_json::from_slice(body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid rollback payload"),
        };
        match self
            .deployments
            .rollback(
                user_id,
                deployment_id,
                payload.deployment_intent_id,
                payload.approval_id,
            )
            .await
        {
            Ok((BeginDeployment::ReadyToDispatch { id }, deployment)) => {
                self.kick_materialize_deployment(id);
                json_response(
                    StatusCode::ACCEPTED,
                    json!({
                        "state": "materializing",
                        "deploymentId": id,
                        "deployment": deployment_json(&deployment),
                    }),
                )
            }
            Ok((_, deployment)) => json_ok(json!({ "deployment": deployment_json(&deployment) })),
            Err(error) => application_error(error),
        }
    }

    async fn restart(
        &self,
        user_id: Uuid,
        deployment_id: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let deployment_id = match Uuid::parse_str(deployment_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        match self.restart_deployment(user_id, deployment_id).await {
            Ok(deployment) => json_ok(json!({ "deployment": deployment_json(&deployment) })),
            Err(error) => application_error(error),
        }
    }

    pub(crate) async fn restart_deployment(
        &self,
        user_id: Uuid,
        deployment_id: Uuid,
    ) -> Result<crate::deployments::Deployment, ApplicationError> {
        self.deployments.restart(user_id, deployment_id).await?;
        self.fabric_restart(deployment_id).await?;
        self.kick_continue_starting(deployment_id);
        self.deployments.get(user_id, deployment_id).await
    }

    async fn deployment_logs(
        &self,
        user_id: Uuid,
        deployment_id: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let deployment_id = match Uuid::parse_str(deployment_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        match crate::deployment_logs::DeploymentLogs::new(self.applications.pool().clone())
            .list(user_id, deployment_id)
            .await
        {
            Ok(items) => json_ok(json!({
                "items": items.iter().map(|chunk| json!({
                    "seq": chunk.seq,
                    "byteLength": chunk.byte_length,
                    "firstTimestamp": chunk.first_timestamp,
                    "lastTimestamp": chunk.last_timestamp,
                })).collect::<Vec<_>>()
            })),
            Err(error) => application_error(error),
        }
    }

    async fn stop(
        &self,
        user_id: Uuid,
        deployment_id: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let deployment_id = match Uuid::parse_str(deployment_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        match self.deployments.prepare_stop(user_id, deployment_id).await {
            Ok(deployment) => match self.fabric_stop(deployment_id).await {
                Ok(()) => match self.deployments.commit_stop(deployment_id).await {
                    Ok(stopped) => json_ok(json!({ "deployment": deployment_json(&stopped) })),
                    Err(error) => application_error(error),
                },
                Err(error) => {
                    if deployment.state == "active" {
                        let _ = self.deployments.hold_unknown(deployment_id).await;
                    }
                    application_error(error)
                }
            },
            Err(error) => application_error(error),
        }
    }

    async fn create_database(
        &self,
        user_id: Uuid,
        environment_id: &str,
        body: &[u8],
    ) -> Response<http_body_util::Full<Bytes>> {
        let environment_id = match Uuid::parse_str(environment_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        let environment =
            match crate::applications::load_environment(self.applications.pool(), environment_id)
                .await
            {
                Ok(Some(environment)) => environment,
                Ok(None) => return json_error(StatusCode::NOT_FOUND, "environment not found"),
                Err(_) => {
                    return json_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "environment lookup failed",
                    );
                }
            };
        let application = match self
            .applications
            .get_internal(environment.application_id)
            .await
        {
            Ok(application) => application,
            Err(error) => return application_error(error),
        };
        let fabric_id = match self.fabric_id_for_workspace(application.workspace_id).await {
            Ok(id) => id,
            Err(error) => return application_error(error),
        };
        #[derive(Deserialize)]
        struct Payload {
            operation_id: Uuid,
        }
        let payload: Payload = match serde_json::from_slice(body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid database payload"),
        };
        let hash = applications::request_hash(&[b"create", environment_id.as_bytes()]);
        match self
            .databases
            .create(
                user_id,
                environment_id,
                fabric_id,
                payload.operation_id,
                &hash,
            )
            .await
        {
            Ok(database) => {
                self.kick_resume_database(&database);
                json_response(
                    StatusCode::ACCEPTED,
                    json!({ "database": database_json(&database) }),
                )
            }
            Err(error) => application_error(error),
        }
    }

    async fn get_environment_database(
        &self,
        user_id: Uuid,
        environment_id: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let environment_id = match Uuid::parse_str(environment_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        let environment =
            match crate::applications::load_environment(self.applications.pool(), environment_id)
                .await
            {
                Ok(Some(environment)) => environment,
                Ok(None) => return application_error(ApplicationError::NotFound),
                Err(error) => return application_error(error.into()),
            };
        if let Err(error) = self
            .applications
            .require_in_project(user_id, environment.application_id, Action::ReadProject)
            .await
        {
            return application_error(error);
        }
        match self.databases.by_environment(environment_id).await {
            Ok(Some(database)) => {
                self.kick_resume_database(&database);
                json_ok(json!({ "database": database_json(&database) }))
            }
            Ok(None) => application_error(ApplicationError::NotFound),
            Err(error) => application_error(error),
        }
    }

    async fn get_database(
        &self,
        user_id: Uuid,
        database_id: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let database_id = match Uuid::parse_str(database_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        match self.databases.get(user_id, database_id).await {
            Ok(database) => {
                self.kick_resume_database(&database);
                json_ok(json!({ "database": database_json(&database) }))
            }
            Err(error) => application_error(error),
        }
    }

    async fn delete_database(
        &self,
        user_id: Uuid,
        database_id: &str,
        body: &[u8],
    ) -> Response<http_body_util::Full<Bytes>> {
        let database_id = match Uuid::parse_str(database_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        match self
            .databases
            .delete(user_id, database_id, optional_uuid(body, "approvalId"))
            .await
        {
            Ok(()) => no_content(),
            Err(error) => application_error(error),
        }
    }

    async fn create_backup(
        &self,
        user_id: Uuid,
        database_id: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let database_id = match Uuid::parse_str(database_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        match self.databases.get(user_id, database_id).await {
            Ok(_) => {
                let operation_id = Uuid::new_v4();
                let hash = applications::request_hash(&[
                    b"backup",
                    database_id.as_bytes(),
                    operation_id.as_bytes(),
                ]);
                match self
                    .databases
                    .begin_backup(user_id, database_id, operation_id, &hash)
                    .await
                {
                    Ok(()) => {
                        self.kick_complete_backup(database_id, operation_id);
                        json_response(
                            StatusCode::ACCEPTED,
                            json!({
                                "databaseId": database_id,
                                "operationId": operation_id,
                                "state": "dispatched",
                                "kind": "manual",
                            }),
                        )
                    }
                    Err(error) => application_error(error),
                }
            }
            Err(error) => application_error(error),
        }
    }

    async fn list_backups(
        &self,
        user_id: Uuid,
        database_id: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let database_id = match Uuid::parse_str(database_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        match self.databases.list_backups(user_id, database_id).await {
            Ok(items) => json_ok(json!({
                "items": items.iter().map(|backup| json!({
                    "id": backup.id,
                    "databaseId": backup.database_id,
                    "kind": backup.kind,
                    "byteLength": backup.byte_length,
                    "createdAt": backup.created_at,
                })).collect::<Vec<_>>()
            })),
            Err(error) => application_error(error),
        }
    }

    async fn restore_backup(
        &self,
        user_id: Uuid,
        database_id: &str,
        body: &[u8],
    ) -> Response<http_body_util::Full<Bytes>> {
        let database_id = match Uuid::parse_str(database_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        #[derive(Deserialize)]
        struct Payload {
            backup_id: Uuid,
            operation_id: Uuid,
            approval_id: Option<Uuid>,
        }
        let payload: Payload = match serde_json::from_slice(body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid restore payload"),
        };
        let hash = applications::request_hash(&[
            b"restore",
            database_id.as_bytes(),
            payload.backup_id.as_bytes(),
            payload.operation_id.as_bytes(),
        ]);
        match self
            .databases
            .begin_restore(
                user_id,
                database_id,
                payload.backup_id,
                payload.operation_id,
                payload.approval_id,
                &hash,
            )
            .await
        {
            Ok(_) => {
                self.kick_complete_restore(database_id, payload.backup_id, payload.operation_id);
                json_response(
                    StatusCode::ACCEPTED,
                    json!({
                        "databaseId": database_id,
                        "backupId": payload.backup_id,
                        "operationId": payload.operation_id,
                        "state": "dispatched",
                    }),
                )
            }
            Err(error) => application_error(error),
        }
    }

    async fn application_metrics(
        &self,
        user_id: Uuid,
        application_id: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let application_id = match Uuid::parse_str(application_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        match self.applications.get(user_id, application_id).await {
            Ok(application) => {
                let environments = match self
                    .applications
                    .environments(user_id, application_id)
                    .await
                {
                    Ok(items) => items,
                    Err(error) => return application_error(error),
                };
                let db_count: i64 = sqlx::query_scalar(
                    "select count(*) from application_databases \
                     where application_id = $1 and state <> 'deleted'",
                )
                .bind(application_id)
                .fetch_one(self.applications.pool())
                .await
                .unwrap_or(0);
                let backup_count: i64 = sqlx::query_scalar(
                    "select count(*) from database_backups b \
                     join application_databases d on d.id = b.database_id \
                     where d.application_id = $1",
                )
                .bind(application_id)
                .fetch_one(self.applications.pool())
                .await
                .unwrap_or(0);
                let log_chunks: i64 = sqlx::query_scalar(
                    "select count(*) from deployment_log_chunks c \
                     join application_deployments d on d.id = c.deployment_id \
                     join application_environments e on e.id = d.environment_id \
                     where e.application_id = $1",
                )
                .bind(application_id)
                .fetch_one(self.applications.pool())
                .await
                .unwrap_or(0);
                json_ok(json!({
                    "applicationId": application.id,
                    "state": application.state,
                    "environments": environments.iter().map(|environment| json!({
                        "kind": environment.kind,
                        "state": environment.state,
                        "visibility": environment.visibility,
                    })).collect::<Vec<_>>(),
                    "databases": db_count,
                    "backups": backup_count,
                    "logChunks": log_chunks,
                    "logChunkByteLimit": crate::deployment_logs::MAX_LOG_CHUNK_BYTES,
                    "logChunkCountLimit": crate::deployment_logs::MAX_LOG_CHUNKS_PER_DEPLOYMENT,
                    "applicationQuota": crate::applications::MAX_APPLICATIONS_PER_PROJECT,
                    "backupRetention": crate::databases::MAX_BACKUPS_PER_DATABASE,
                }))
            }
            Err(error) => application_error(error),
        }
    }

    async fn list_bindings(
        &self,
        user_id: Uuid,
        environment_id: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let environment_id = match Uuid::parse_str(environment_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        match self.bindings.list(user_id, environment_id).await {
            Ok(items) => json_ok(json!({
                "items": items.iter().map(|binding| json!({
                    "environmentId": binding.environment_id,
                    "secretId": binding.secret_id,
                    "name": binding.environment_name,
                    "revision": binding.binding_revision,
                })).collect::<Vec<_>>()
            })),
            Err(error) => application_error(error),
        }
    }

    async fn put_binding(
        &self,
        user_id: Uuid,
        environment_id: &str,
        name: &str,
        body: &[u8],
    ) -> Response<http_body_util::Full<Bytes>> {
        let environment_id = match Uuid::parse_str(environment_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        #[derive(Deserialize)]
        struct Payload {
            secret_id: Uuid,
            approval_id: Option<Uuid>,
        }
        let payload: Payload = match serde_json::from_slice(body) {
            Ok(payload) => payload,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid binding payload"),
        };
        match self
            .bindings
            .bind(
                user_id,
                environment_id,
                name,
                payload.secret_id,
                payload.approval_id,
            )
            .await
        {
            Ok(binding) => json_ok(json!({
                "name": binding.environment_name,
                "secretId": binding.secret_id,
                "revision": binding.binding_revision,
            })),
            Err(error) => application_error(error),
        }
    }

    async fn delete_binding(
        &self,
        user_id: Uuid,
        environment_id: &str,
        name: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let environment_id = match Uuid::parse_str(environment_id) {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid resource id"),
        };
        match self.bindings.unbind(user_id, environment_id, name).await {
            Ok(()) => no_content(),
            Err(error) => application_error(error),
        }
    }

    async fn preview_login(
        &self,
        user_id: Uuid,
        query: &str,
    ) -> Response<http_body_util::Full<Bytes>> {
        let application_id = query_uuid(query, "applicationId");
        let environment_id = query_uuid(query, "environmentId");
        let (Some(application_id), Some(environment_id)) = (application_id, environment_id) else {
            return json_error(StatusCode::BAD_REQUEST, "invalid resource id");
        };
        match self
            .preview
            .start_login(user_id, application_id, environment_id)
            .await
        {
            Ok(login) => json_ok(json!({
                "redirect": login.redirect,
                "hostname": login.hostname,
            })),
            Err(error) => application_error(error),
        }
    }

    async fn preview_callback(
        &self,
        query: &str,
        host: Option<&str>,
    ) -> Response<http_body_util::Full<Bytes>> {
        let Some(code) = query_value(query, "code") else {
            return json_error(StatusCode::BAD_REQUEST, "invalid resource id");
        };
        let Some(host) = host.map(host_without_port) else {
            return json_error(StatusCode::BAD_REQUEST, "invalid resource id");
        };
        match self.preview.exchange_code(&code, host).await {
            Ok((cookie, hostname)) => {
                let location = format!("https://{hostname}/");
                let mut builder = Response::builder()
                    .status(StatusCode::FOUND)
                    .header("location", location);
                if let Ok(value) = HeaderValue::from_str(&cookie) {
                    builder = builder.header("set-cookie", value);
                }
                builder
                    .body(http_body_util::Full::new(Bytes::new()))
                    .expect("preview redirect headers are valid")
            }
            Err(error) => application_error(error),
        }
    }

    async fn preview_authorize(
        &self,
        host: Option<&str>,
        cookie_header: Option<&str>,
    ) -> Response<http_body_util::Full<Bytes>> {
        let Some(host) = host.map(host_without_port) else {
            return json_error(StatusCode::BAD_REQUEST, "invalid resource id");
        };
        let cookie = cookie_header.and_then(|header| cookie_value(header, PREVIEW_COOKIE));
        match self.preview.authorize(host, cookie.as_deref()).await {
            Ok(true) => json_ok(json!({ "authorized": true })),
            Ok(false) => json_error(StatusCode::UNAUTHORIZED, "preview authorization failed"),
            Err(error) => application_error(error),
        }
    }
}

fn application_json(application: &applications::Application) -> Value {
    json!({
        "id": application.id,
        "projectId": application.project_id,
        "workspaceId": application.workspace_id,
        "name": application.name,
        "slug": application.slug,
        "rootPath": application.root_path,
        "runtimeProfile": application.runtime_profile,
        "state": application.state,
        "createdByUserId": application.created_by_user_id,
        "createdAt": application.created_at,
        "updatedAt": application.updated_at,
    })
}

fn environment_json(environment: &applications::Environment) -> Value {
    json!({
        "id": environment.id,
        "applicationId": environment.application_id,
        "kind": environment.kind,
        "visibility": environment.visibility,
        "hostname": environment.hostname,
        "revision": environment.revision,
        "activeDeploymentId": environment.active_deployment_id,
        "state": environment.state,
    })
}

fn release_json(release: &crate::releases::Release) -> Value {
    json!({
        "id": release.id,
        "applicationId": release.application_id,
        "buildIntentId": release.build_intent_id,
        "sourceWorkspaceId": release.source_workspace_id,
        "sourceExecGeneration": release.source_exec_generation,
        "runtimeProfile": release.runtime_profile,
        "state": release.state,
        "artifactBytes": release.artifact_bytes,
        "artifactHash": release.artifact_hash.as_ref().map(|bytes| {
            bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>()
        }),
        "testSummary": release.test_summary,
        "createdByUserId": release.created_by_user_id,
        "createdAt": release.created_at,
    })
}

fn deployment_json(deployment: &crate::deployments::Deployment) -> Value {
    json!({
        "id": deployment.id,
        "environmentId": deployment.environment_id,
        "releaseId": deployment.release_id,
        "deploymentIntentId": deployment.deployment_intent_id,
        "state": deployment.state,
        "desiredRevision": deployment.desired_revision,
        "observedRevision": deployment.observed_revision,
        "previousDeploymentId": deployment.previous_deployment_id,
        "createdByUserId": deployment.created_by_user_id,
        "acceptedAt": deployment.accepted_at,
        "activeAt": deployment.active_at,
    })
}

fn approval_json(approval: &applications::ApprovalRequest) -> Value {
    json!({
        "id": approval.id,
        "projectId": approval.project_id,
        "applicationId": approval.application_id,
        "environmentId": approval.environment_id,
        "releaseId": approval.release_id,
        "kind": approval.kind,
        "state": approval.state,
        "createdAt": approval.created_at,
    })
}

fn database_json(database: &crate::databases::Database) -> Value {
    json!({
        "id": database.id,
        "applicationId": database.application_id,
        "environmentId": database.environment_id,
        "engine": database.engine,
        "engineProfile": database.engine_profile,
        "state": database.state,
        "createdAt": database.created_at,
    })
}

fn application_error(error: ApplicationError) -> Response<http_body_util::Full<Bytes>> {
    let status = StatusCode::from_u16(error.status()).unwrap_or(StatusCode::BAD_REQUEST);
    if let ApplicationError::ApprovalRequired(id) = error {
        return json_response(
            status,
            json!({ "error": "approval required", "approvalId": id }),
        );
    }
    json_error(status, error.message())
}

fn optional_uuid(body: &[u8], field: &str) -> Option<Uuid> {
    let value: Value = serde_json::from_slice(body).ok()?;
    value
        .get(field)
        .and_then(Value::as_str)
        .and_then(|text| Uuid::parse_str(text).ok())
}

fn query_uuid(query: &str, key: &str) -> Option<Uuid> {
    query_value(query, key).and_then(|text| Uuid::parse_str(&text).ok())
}

fn query_value(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        if name == key {
            return Some(value.to_owned());
        }
    }
    None
}

fn host_without_port(host: &str) -> &str {
    match host.rsplit_once(':') {
        Some((name, port))
            if !name.is_empty()
                && !name.ends_with(']')
                && port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            name
        }
        _ => host,
    }
}

fn cookie_value(header: &str, name: &str) -> Option<String> {
    for part in header.split(';') {
        let part = part.trim();
        if let Some(value) = part
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('='))
        {
            return Some(value.to_owned());
        }
    }
    None
}

fn json_ok(value: Value) -> Response<http_body_util::Full<Bytes>> {
    json_response(StatusCode::OK, value)
}

fn json_error(status: StatusCode, message: &'static str) -> Response<http_body_util::Full<Bytes>> {
    json_response(status, json!({ "error": message }))
}

fn json_response(status: StatusCode, value: Value) -> Response<http_body_util::Full<Bytes>> {
    let body = serde_json::to_vec(&value).expect("JSON response serializes");
    Response::builder()
        .status(status)
        .header("content-type", "application/json; charset=utf-8")
        .body(http_body_util::Full::new(Bytes::from(body)))
        .expect("JSON response headers are valid")
}

fn no_content() -> Response<http_body_util::Full<Bytes>> {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(http_body_util::Full::new(Bytes::new()))
        .expect("204 response headers are valid")
}

pub fn product_tool_definitions() -> Vec<crate::model::ModelToolDefinition> {
    const TOOLS: &[(&str, &str)] = &[
        (
            "application.create",
            "Create or attach an Application on the current Workspace. Call this to start the software project the user asked you to build; then write voie.toml and source with bash under /workspace.",
        ),
        (
            "application.inspect",
            "Inspect the Application bound to this Workspace.",
        ),
        (
            "application.status",
            "Show Application Environments, Releases, Deployments, Databases, and pending approvals. Poll this after release.build or deploy until the Release is ready and the candidate Deployment is healthy, then call deployment.activate.",
        ),
        (
            "application.suspend",
            "Suspend the Application: stop Deployments without deleting Databases or Workspace volumes.",
        ),
        (
            "application.archive",
            "Archive the Application: keep Blob restore points and release local Workspace, Database, and Deployment volumes. Distinct from suspend (keeps volumes) and delete (no final backup).",
        ),
        (
            "application.restore",
            "Restore an archived Application from pinned Blob Workspace snapshot and Database backups onto candidate LVs after restore_application approval. Distinct from suspend.",
        ),
        (
            "application.delete",
            "Delete the Application after delete_application approval. Stops Deployments, Databases, routes, and Fabric journal rows.",
        ),
        (
            "release.build",
            "Pack the Workspace guest voie.toml and source into an immutable Release. Reads voie.toml from the guest. Resources above the default tier require increase_resource_tier approval.",
        ),
        ("release.inspect", "Inspect one Release."),
        (
            "environment.deploy_dev",
            "Materialize a ready Release in private dev. Omitting release_id uses the latest ready Release. Call database.create first and wait until database.status is ready when the Release declares postgres. Does not switch traffic; after healthy, call deployment.activate.",
        ),
        (
            "environment.set_visibility",
            "Set development Environment visibility.",
        ),
        (
            "environment.publish_prod",
            "Materialize an existing Release in production after human approval (approval_id). Omitting release_id uses the latest ready Release. Does not rebuild or switch traffic. Call database.create for prod and wait until ready when the Release declares postgres. After healthy, call deployment.activate.",
        ),
        (
            "deployment.status",
            "Show Deployment state. Omitting deployment_id lists Deployments for this Application.",
        ),
        (
            "deployment.activate",
            "Switch Environment traffic to a healthy Deployment. Omitting deployment_id uses the latest healthy Deployment. Required after deploy_dev or publish_prod. Production requires ManageProduction.",
        ),
        (
            "deployment.rollback",
            "Create a new Deployment of the previous Release. Does not mutate the old row back to active.",
        ),
        (
            "deployment.restart",
            "Recreate the same Deployment Pod without changing the Release.",
        ),
        ("deployment.logs", "List Deployment log chunk metadata."),
        (
            "database.create",
            "Create the dedicated PostgreSQL Database for one Environment kind. Call before deploying a Release that declares postgres. Poll database.status until ready. Elevated size requires increase_resource_tier approval.",
        ),
        (
            "database.status",
            "Show Database state. Omitting database_id lists Databases for this Application. Optional kind selects one Environment. Wait for ready before deploy_dev or publish_prod when the Release uses postgres.",
        ),
        (
            "database.backup",
            "Dispatch a manual Database backup. The dump is a Blob object; credentials never enter the result.",
        ),
        (
            "database.list_backups",
            "List Database backup metadata without dump bytes or credentials.",
        ),
        (
            "database.restore",
            "Restore one backup into the Database after restore_database approval. Always allocates a candidate LV and switches only after proof.",
        ),
        (
            "workspace.snapshot",
            "Capture a Blob Workspace snapshot including .git. Distinct from a Release pack. Retention drops unpinned snapshots beyond the platform bound.",
        ),
        (
            "workspace.grow",
            "Grow a 32 GiB Workspace to 64 GiB after increase_resource_tier approval. Workspaces never shrink. New Workspaces start at 16 GiB and may grow to 32 GiB automatically.",
        ),
        (
            "secret.list_metadata",
            "List Project secret metadata without values.",
        ),
        (
            "secret.request_binding",
            "Request that a named secret be bound to an Environment.",
        ),
    ];
    TOOLS
        .iter()
        .map(|(name, description)| crate::model::ModelToolDefinition {
            id: (*name).to_owned(),
            name: (*name).to_owned(),
            description: (*description).to_owned(),
            parameters: product_tool_parameters(name),
        })
        .collect()
}

fn product_tool_parameters(name: &str) -> serde_json::Value {
    fn uuid() -> serde_json::Value {
        serde_json::json!({ "type": "string" })
    }
    match name {
        "application.create" => serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "slug": { "type": "string" }
            },
            "required": ["name", "slug"],
            "additionalProperties": true
        }),
        "application.delete" => serde_json::json!({
            "type": "object",
            "properties": { "approval_id": uuid(), "approvalId": uuid() },
            "additionalProperties": true
        }),
        "application.restore" => serde_json::json!({
            "type": "object",
            "properties": { "approval_id": uuid(), "approvalId": uuid() },
            "additionalProperties": true
        }),
        "release.build" => serde_json::json!({
            "type": "object",
            "properties": {
                "build_intent_id": uuid(),
                "buildIntentId": uuid(),
                "approval_id": uuid(),
                "approvalId": uuid()
            },
            "additionalProperties": true
        }),
        "release.inspect" => serde_json::json!({
            "type": "object",
            "properties": { "release_id": uuid(), "releaseId": uuid() },
            "additionalProperties": true
        }),
        "environment.deploy_dev" | "environment.publish_prod" => serde_json::json!({
            "type": "object",
            "properties": {
                "release_id": uuid(),
                "releaseId": uuid(),
                "approval_id": uuid(),
                "approvalId": uuid()
            },
            "additionalProperties": true
        }),
        "environment.set_visibility" => serde_json::json!({
            "type": "object",
            "properties": {
                "kind": { "type": "string" },
                "visibility": { "type": "string" },
                "approval_id": uuid(),
                "approvalId": uuid()
            },
            "additionalProperties": true
        }),
        "deployment.status" | "deployment.activate" | "deployment.restart" | "deployment.logs" => {
            serde_json::json!({
                "type": "object",
                "properties": { "deployment_id": uuid(), "deploymentId": uuid() },
                "additionalProperties": true
            })
        }
        "deployment.rollback" => serde_json::json!({
            "type": "object",
            "properties": {
                "deployment_id": uuid(),
                "deploymentId": uuid(),
                "approval_id": uuid(),
                "approvalId": uuid()
            },
            "additionalProperties": true
        }),
        "database.create" => serde_json::json!({
            "type": "object",
            "properties": {
                "kind": { "type": "string" },
                "elevated": { "type": "boolean" },
                "approval_id": uuid(),
                "approvalId": uuid()
            },
            "required": ["kind"],
            "additionalProperties": true
        }),
        "database.status" => serde_json::json!({
            "type": "object",
            "properties": {
                "database_id": uuid(),
                "databaseId": uuid(),
                "kind": { "type": "string" }
            },
            "additionalProperties": true
        }),
        "database.backup" | "database.list_backups" => serde_json::json!({
            "type": "object",
            "properties": { "database_id": uuid(), "databaseId": uuid() },
            "additionalProperties": true
        }),
        "database.restore" => serde_json::json!({
            "type": "object",
            "properties": {
                "database_id": uuid(),
                "databaseId": uuid(),
                "backup_id": uuid(),
                "backupId": uuid(),
                "approval_id": uuid(),
                "approvalId": uuid()
            },
            "additionalProperties": true
        }),
        "workspace.grow" => serde_json::json!({
            "type": "object",
            "properties": { "approval_id": uuid(), "approvalId": uuid() },
            "additionalProperties": true
        }),
        "secret.request_binding" => serde_json::json!({
            "type": "object",
            "properties": {
                "kind": { "type": "string" },
                "name": { "type": "string" },
                "secret_id": uuid(),
                "secretId": uuid()
            },
            "additionalProperties": true
        }),
        _ => serde_json::json!({
            "type": "object",
            "additionalProperties": true
        }),
    }
}

/// Operating instructions used when the Agent prompt is empty. Project stays
/// the authorization scope; Application is the deployable the model creates.
pub const PROFILE1_AGENT_PREAMBLE: &str = "\
Project is the authorization scope. Application is the deployable: call application.create on this Workspace, write voie.toml and source under /workspace with bash, test there, then release.build. Packed runtime files are under /app; open relative paths, not /workspace. Private preview is environment.deploy_dev then deployment.activate after healthy. Production publishes that exact Release with environment.publish_prod after human approval, then deployment.activate. Dedicated PostgreSQL is database.create per Environment. Call one product tool per turn. Never print credentials, DATABASE_URL, or postgres URLs. Do not use Kubernetes, Dockerfiles, GitHub Actions, or another Project.";

/// Prefers a configured Agent prompt, then a child-supplied system string,
/// then the Profile 1 preamble so an empty Agent still runs the vertical.
pub fn resolve_agent_system_prompt(
    agent_prompt: &str,
    request_system: Option<String>,
) -> Option<String> {
    let agent = agent_prompt.trim();
    if !agent.is_empty() {
        return Some(agent.to_owned());
    }
    match request_system {
        Some(text) if !text.trim().is_empty() => Some(text),
        _ => Some(PROFILE1_AGENT_PREAMBLE.to_owned()),
    }
}

/// True when tool text would put database credentials into conversation.
pub fn product_text_leaks_secret(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("postgres://")
        || lower.contains("postgresql://")
        || lower.contains("database_url")
        || lower.contains("postgres_password")
        || lower.contains("postgres-password")
}
