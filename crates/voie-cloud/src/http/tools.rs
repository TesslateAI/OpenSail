//! Server-side product tools. Authority is derived from activation context.

use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

use crate::applications::{ApplicationError, Manifest};
use crate::auth::Action;
use crate::databases::BeginDatabaseOp;
use crate::deployments::BeginDeployment;
use crate::releases::BeginRelease;

use super::Platform;

impl Platform {
    /// Executes one typed product tool. Project and Workspace come from the
    /// activation, never from the model.
    pub async fn execute_tool(
        &self,
        actor_user_id: Uuid,
        project_id: Uuid,
        workspace_id: Uuid,
        name: &str,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        match name {
            "application.create" => {
                self.tool_application_create(actor_user_id, project_id, workspace_id, arguments)
                    .await
            }
            "application.inspect" => {
                self.tool_application_inspect(actor_user_id, workspace_id)
                    .await
            }
            "application.status" => {
                self.tool_application_status(actor_user_id, workspace_id)
                    .await
            }
            "application.suspend" => {
                self.tool_application_suspend(actor_user_id, workspace_id)
                    .await
            }
            "application.archive" => {
                self.tool_application_archive(actor_user_id, workspace_id)
                    .await
            }
            "application.restore" => {
                self.tool_application_restore(actor_user_id, workspace_id, arguments)
                    .await
            }
            "application.delete" => {
                self.tool_application_delete(actor_user_id, workspace_id, arguments)
                    .await
            }
            "release.build" => {
                self.tool_release_build(actor_user_id, workspace_id, arguments)
                    .await
            }
            "release.inspect" => {
                let release_id = required_uuid(arguments, "release_id")?;
                let release = self.releases.get(actor_user_id, release_id).await?;
                Ok(json!({ "release": super::release_json(&release) }))
            }
            "environment.deploy_dev" => {
                self.tool_deploy(actor_user_id, workspace_id, "dev", arguments)
                    .await
            }
            "environment.set_visibility" => {
                self.tool_set_visibility(actor_user_id, workspace_id, arguments)
                    .await
            }
            "environment.publish_prod" => {
                self.tool_deploy(actor_user_id, workspace_id, "prod", arguments)
                    .await
            }
            "deployment.status" => {
                self.tool_deployment_status(actor_user_id, workspace_id, arguments)
                    .await
            }
            "deployment.activate" => {
                self.tool_deployment_activate(actor_user_id, workspace_id, arguments)
                    .await
            }
            "deployment.rollback" => {
                self.tool_deployment_rollback(actor_user_id, workspace_id, arguments)
                    .await
            }
            "deployment.restart" => {
                self.tool_deployment_restart(actor_user_id, workspace_id, arguments)
                    .await
            }
            "deployment.logs" => {
                self.tool_deployment_logs(actor_user_id, workspace_id, arguments)
                    .await
            }
            "deployment.stop" => {
                self.tool_deployment_stop(actor_user_id, workspace_id, arguments)
                    .await
            }
            "database.create" => {
                self.tool_database_create(actor_user_id, workspace_id, arguments)
                    .await
            }
            "database.status" => {
                self.tool_database_status(actor_user_id, workspace_id, arguments)
                    .await
            }
            "database.backup" => {
                self.tool_database_backup(actor_user_id, workspace_id, arguments)
                    .await
            }
            "database.list_backups" => {
                self.tool_database_list_backups(actor_user_id, workspace_id, arguments)
                    .await
            }
            "workspace.snapshot" => {
                self.tool_workspace_snapshot(actor_user_id, workspace_id)
                    .await
            }
            "workspace.restore" => {
                self.tool_workspace_restore(actor_user_id, workspace_id, arguments)
                    .await
            }
            "workspace.grow" => {
                self.tool_workspace_grow(actor_user_id, workspace_id, arguments)
                    .await
            }
            "database.restore" => {
                self.tool_database_restore(actor_user_id, workspace_id, arguments)
                    .await
            }
            "database.set_security_profile" => {
                self.tool_database_set_security_profile(actor_user_id, workspace_id, arguments)
                    .await
            }
            "secret.list_metadata" => self.tool_secret_metadata(actor_user_id, project_id).await,
            "secret.request_binding" => {
                self.tool_request_binding(actor_user_id, workspace_id, arguments)
                    .await
            }
            _ => Err(ApplicationError::InvalidName),
        }
    }

    async fn require_bound_deployment(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        deployment_id: Uuid,
    ) -> Result<(), ApplicationError> {
        let application = self
            .applications
            .by_workspace(workspace_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let deployment = self.deployments.get(actor_user_id, deployment_id).await?;
        let environment = crate::applications::load_environment(
            self.applications.pool(),
            deployment.environment_id,
        )
        .await?
        .ok_or(ApplicationError::NotFound)?;
        if environment.application_id != application.id {
            return Err(ApplicationError::NotFound);
        }
        Ok(())
    }

    async fn tool_deployment_status(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        if let Some(deployment_id) = optional_uuid_arg(arguments, "deployment_id")? {
            self.require_bound_deployment(actor_user_id, workspace_id, deployment_id)
                .await?;
            let deployment = self.deployments.get(actor_user_id, deployment_id).await?;
            return Ok(json!({ "deployment": super::deployment_json(&deployment) }));
        }
        let application = self
            .applications
            .by_workspace(workspace_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let environments = self
            .applications
            .environments(actor_user_id, application.id)
            .await?;
        let mut deployments = Vec::new();
        for environment in &environments {
            if let Ok((_, items)) = self.deployments.list(actor_user_id, environment.id).await {
                for deployment in items {
                    deployments.push(deployment);
                }
            }
        }
        deployments.sort_by(|left, right| {
            left.accepted_at
                .cmp(&right.accepted_at)
                .then(left.id.cmp(&right.id))
        });
        let items: Vec<_> = deployments.iter().map(super::deployment_json).collect();
        let mut body = json!({ "items": items });
        if let Some(latest) = deployments.last() {
            body["deployment"] = super::deployment_json(latest);
        }
        Ok(body)
    }

    async fn tool_deployment_activate(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        let deployment_id = self
            .resolve_activate_deployment_id(actor_user_id, workspace_id, arguments)
            .await?;
        self.require_bound_deployment(actor_user_id, workspace_id, deployment_id)
            .await?;
        let deployment = self
            .activate_deployment(actor_user_id, deployment_id)
            .await?;
        Ok(json!({ "deployment": super::deployment_json(&deployment) }))
    }

    async fn tool_deployment_rollback(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        let deployment_id = required_uuid(arguments, "deployment_id")?;
        self.require_bound_deployment(actor_user_id, workspace_id, deployment_id)
            .await?;
        let intent = match optional_uuid_arg(arguments, "deployment_intent_id")? {
            Some(id) => id,
            None => {
                let current = self.deployments.get(actor_user_id, deployment_id).await?;
                let previous = current
                    .previous_deployment_id
                    .ok_or(ApplicationError::NotFound)?;
                let previous_row = self.deployments.get(actor_user_id, previous).await?;
                self.deployments
                    .latest_inflight_intent_for_release(
                        current.environment_id,
                        previous_row.release_id,
                    )
                    .await?
                    .unwrap_or_else(Uuid::new_v4)
            }
        };
        let approval_id = optional_uuid_arg(arguments, "approval_id")?;
        match self
            .deployments
            .rollback(actor_user_id, deployment_id, intent, approval_id)
            .await?
        {
            (BeginDeployment::ReadyToDispatch { id }, deployment) => {
                self.wake_deployment(id);
                Ok(json!({
                    "state": deployment.wire_state(),
                    "desiredState": deployment.desired_state,
                    "deploymentId": id,
                    "deploymentIntentId": deployment.deployment_intent_id,
                    "deployment": super::deployment_json(&deployment),
                }))
            }
            (BeginDeployment::OutcomeUnknown, deployment) => {
                Err(ApplicationError::outcome_unknown_at(
                    "deployment",
                    deployment.id,
                    deployment.deployment_intent_id,
                ))
            }
            (_, deployment) => Ok(json!({ "deployment": super::deployment_json(&deployment) })),
        }
    }

    async fn tool_deployment_restart(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        let deployment_id = required_uuid(arguments, "deployment_id")?;
        self.require_bound_deployment(actor_user_id, workspace_id, deployment_id)
            .await?;
        let deployment = self
            .restart_deployment(actor_user_id, deployment_id)
            .await?;
        Ok(json!({ "deployment": super::deployment_json(&deployment) }))
    }

    async fn tool_deployment_stop(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        let deployment_id = required_uuid(arguments, "deployment_id")?;
        self.require_bound_deployment(actor_user_id, workspace_id, deployment_id)
            .await?;
        self.deployments
            .prepare_stop(actor_user_id, deployment_id)
            .await?;
        self.deployments
            .request_desired(deployment_id, "stopped")
            .await?;
        crate::reconcile::deployment::put_due_deployment(self, deployment_id).await;
        if let Ok(deployment) = self.deployments.get_internal(deployment_id).await {
            crate::reconcile::traffic::put_due_environment(self, deployment.environment_id).await;
        }
        self.kick_route_map();
        let stopped = self.deployments.get(actor_user_id, deployment_id).await?;
        Ok(json!({ "deployment": super::deployment_json(&stopped) }))
    }

    async fn tool_deployment_logs(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        let deployment_id = required_uuid(arguments, "deployment_id")?;
        self.require_bound_deployment(actor_user_id, workspace_id, deployment_id)
            .await?;
        let deployment = self.deployments.get(actor_user_id, deployment_id).await?;
        if deployment.logs_sensitive {
            return Ok(json!({
                "deploymentId": deployment_id,
                "unavailable": true,
                "reason": "sensitive",
                "truncated": true,
            }));
        }
        let items = crate::deployment_logs::DeploymentLogs::new(self.applications.pool().clone())
            .list(actor_user_id, deployment_id)
            .await?;
        if items.is_empty() {
            return Ok(json!({
                "deploymentId": deployment_id,
                "text": "",
                "firstSeq": 0,
                "lastSeq": 0,
                "nextSeq": 1,
                "truncated": false,
            }));
        }
        let Some(runtime) = self.runtime.as_ref() else {
            return Err(ApplicationError::LogUnavailable);
        };
        const LOG_TAIL_BYTES: usize = 48 * 1024;
        let mut parts: Vec<(i64, Vec<u8>, String, String)> = Vec::new();
        let mut total = 0usize;
        for chunk in items.iter().rev() {
            let bytes = runtime
                .blob
                .get_artifact(&chunk.object_key)
                .await
                .map_err(|_| ApplicationError::LogUnavailable)?;
            total = total.saturating_add(bytes.len());
            parts.push((
                chunk.seq,
                bytes,
                chunk.first_timestamp.clone(),
                chunk.last_timestamp.clone(),
            ));
            if total >= LOG_TAIL_BYTES {
                break;
            }
        }
        parts.reverse();
        let (text, truncated) = crate::deployment_logs::bounded_log_text(&parts, LOG_TAIL_BYTES);
        let last_seq = parts.last().map(|part| part.0).unwrap_or(0);
        let first_seq = parts.first().map(|part| part.0).unwrap_or(0);
        let body = json!({
            "deploymentId": deployment_id,
            "text": text,
            "firstSeq": first_seq,
            "lastSeq": last_seq,
            "nextSeq": last_seq.saturating_add(1),
            "truncated": truncated,
        });
        if super::product_text_leaks_secret(&body.to_string()) {
            return Ok(json!({
                "deploymentId": deployment_id,
                "text": "log content withheld",
                "truncated": true,
            }));
        }
        Ok(body)
    }

    async fn tool_application_create(
        &self,
        actor_user_id: Uuid,
        project_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        let name = required_str(arguments, "name")?;
        self.authorize_workspace_for_application_create(actor_user_id, project_id, workspace_id)
            .await?;
        self.require_profile1_workspace(workspace_id).await?;
        let outcome = self
            .applications
            .create(actor_user_id, project_id, workspace_id, name, None)
            .await?;
        if let Some(handoff) = outcome.workspace_handoff {
            if let Err(error) = self.realize_workspace_handoff(handoff).await {
                if matches!(error, ApplicationError::WorkspaceMissing) {
                    self.abort_unrealized_handoff(outcome.application.id, handoff)
                        .await;
                }
                return Err(error);
            }
        }
        let mut body = json!({
            "application": super::application_json(&outcome.application),
            "environments": outcome.environments.iter().map(super::environment_json).collect::<Vec<_>>(),
        });
        if let Some(handoff) = outcome.workspace_handoff {
            body["workspaceHandoff"] = json!(handoff);
            body["message"] = json!(
                "Application created. Workspace provisioning. Open a new conversation in this Application."
            );
        }
        Ok(body)
    }

    async fn tool_application_inspect(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
    ) -> Result<Value, ApplicationError> {
        let application = self
            .applications
            .by_workspace(workspace_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let loaded = self.applications.get(actor_user_id, application.id).await?;
        Ok(json!({ "application": super::application_json(&loaded) }))
    }

    async fn tool_application_status(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
    ) -> Result<Value, ApplicationError> {
        let application = self
            .applications
            .by_workspace(workspace_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let environments = self
            .applications
            .environments(actor_user_id, application.id)
            .await?;
        let releases = self.releases.list(actor_user_id, application.id).await?;
        let latest_release = releases.into_iter().max_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.id.cmp(&right.id))
        });
        let mut environment_views = Vec::new();
        for environment in &environments {
            let items = self
                .deployments
                .list(actor_user_id, environment.id)
                .await
                .map(|(_, items)| items)
                .unwrap_or_default();
            let candidate = environment
                .desired_deployment_id
                .and_then(|id| items.iter().find(|item| item.id == id).cloned())
                .or_else(|| {
                    items
                        .iter()
                        .filter(|item| !item.traffic)
                        .max_by_key(|item| item.accepted_at.clone())
                        .cloned()
                });
            let active = environment
                .active_deployment_id
                .and_then(|id| items.iter().find(|item| item.id == id).cloned())
                .or_else(|| items.iter().find(|item| item.traffic).cloned());
            let database = self
                .databases
                .by_environment(environment.id)
                .await
                .ok()
                .flatten();
            environment_views.push(json!({
                "environment": super::environment_json(environment),
                "candidateDeployment": candidate.as_ref().map(super::deployment_json),
                "activeDeployment": active.as_ref().map(super::deployment_json),
                "database": database.as_ref().map(super::database_json),
            }));
        }
        let approvals: Vec<_> = self
            .applications
            .list_approvals(actor_user_id, application.id)
            .await?
            .into_iter()
            .filter(|item| item.state == "pending")
            .collect();
        Ok(json!({
            "application": super::application_json(&application),
            "environmentViews": environment_views,
            "latestRelease": latest_release.as_ref().map(super::release_json),
            "approvals": approvals.iter().map(super::approval_json).collect::<Vec<_>>(),
        }))
    }

    async fn tool_application_suspend(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
    ) -> Result<Value, ApplicationError> {
        let application = self
            .applications
            .by_workspace(workspace_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let cleanup = self
            .applications
            .plan_suspend(actor_user_id, application.id)
            .await?;
        self.applications
            .commit_suspend(actor_user_id, application.id)
            .await?;
        self.wake_cleanup_reconcilers(&cleanup).await;
        self.kick_route_map();
        let loaded = self.applications.get(actor_user_id, application.id).await?;
        Ok(json!({ "application": super::application_json(&loaded) }))
    }

    async fn tool_application_archive(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
    ) -> Result<Value, ApplicationError> {
        let application = self
            .applications
            .by_workspace(workspace_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let loaded = self
            .archive_application(actor_user_id, application.id)
            .await?;
        Ok(json!({ "application": super::application_json(&loaded) }))
    }

    async fn tool_application_restore(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        let application = self
            .applications
            .by_workspace(workspace_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let approval_id = optional_uuid_arg(arguments, "approval_id")?;
        let loaded = self
            .restore_application(actor_user_id, application.id, approval_id)
            .await?;
        Ok(json!({ "application": super::application_json(&loaded) }))
    }

    async fn tool_workspace_snapshot(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
    ) -> Result<Value, ApplicationError> {
        let application = self
            .applications
            .by_workspace(workspace_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let _ = self
            .applications
            .require_in_project(actor_user_id, application.id, Action::ManageProduction)
            .await?;
        if matches!(
            application.state.as_str(),
            "archiving" | "archived" | "restoring" | "deleting"
        ) {
            return Err(ApplicationError::WorkspaceBusy);
        }
        self.databases
            .accept_manual_workspace_snapshot(actor_user_id, workspace_id, application.id)
            .await?;
        let snapshot_id = self
            .snapshot_workspace_to_blob(workspace_id, "manual", None)
            .await?;
        Ok(json!({
            "snapshotId": snapshot_id,
            "workspaceId": workspace_id,
            "kind": "manual",
        }))
    }

    async fn tool_workspace_restore(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        let application = self
            .applications
            .by_workspace(workspace_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let _ = self
            .applications
            .require_in_project(actor_user_id, application.id, Action::ManageProduction)
            .await?;
        let snapshot_id = required_uuid(arguments, "snapshot_id")?;
        self.restore_workspace_from_snapshot(workspace_id, snapshot_id)
            .await?;
        Ok(json!({
            "workspaceId": workspace_id,
            "snapshotId": snapshot_id,
            "state": "ready",
        }))
    }

    async fn tool_workspace_grow(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        let approval_id = optional_uuid_arg(arguments, "approval_id")?;
        let allocated_bytes = self
            .grow_workspace_elevated(actor_user_id, workspace_id, approval_id)
            .await?;
        Ok(json!({
            "workspaceId": workspace_id,
            "allocatedBytes": allocated_bytes,
            "storageTier": "elevated",
        }))
    }

    async fn tool_application_delete(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        let application = self
            .applications
            .by_workspace_for_cleanup(workspace_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let approval_id = optional_uuid_arg(arguments, "approval_id")?;
        let cleanup = self
            .applications
            .plan_delete(actor_user_id, application.id, approval_id)
            .await?;
        self.applications.commit_delete(application.id).await?;
        self.wake_cleanup_reconcilers(&cleanup).await;
        self.cleanup_application_fabric(&cleanup).await?;
        let blob = self.runtime.as_ref().map(|runtime| &runtime.blob);
        self.releases
            .reclaim_application_blobs(application.id, blob)
            .await?;
        self.databases
            .reclaim_application_recovery_blobs(application.id, blob)
            .await?;
        self.kick_route_map();
        Ok(json!({
            "state": "deleting",
            "applicationId": application.id,
        }))
    }

    async fn tool_release_build(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        let application = self
            .applications
            .by_workspace(workspace_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let root_path = self
            .authorize_release_manifest_read(actor_user_id, application.id, workspace_id)
            .await?;
        let explicit_intent = optional_uuid_arg(arguments, "build_intent_id")?;
        let generation = match field(arguments, "source_exec_generation").and_then(Value::as_i64) {
            Some(generation) if generation > 0 => generation,
            _ => {
                sqlx::query_scalar("select exec_generation from workspaces where id = $1")
                    .bind(workspace_id)
                    .fetch_one(self.applications.pool())
                    .await?
            }
        };
        let mut manifest = None;
        for attempt in 0..2 {
            match self.read_guest_manifest(workspace_id, &root_path).await? {
                Some(text) => {
                    manifest = Some(text);
                    break;
                }
                None if attempt + 1 < 2 => {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
                None => break,
            }
        }
        let manifest = match manifest {
            Some(text) => text,
            None => match field(arguments, "manifest").and_then(Value::as_str) {
                Some(text) if !text.is_empty() => text.to_owned(),
                _ => {
                    return Ok(json!({
                        "state": "waiting",
                        "message": "guest voie.toml is missing; write it with bash, then call release.build again",
                    }));
                }
            },
        };
        let parsed = Manifest::parse(&manifest)
            .map_err(|error| ApplicationError::InvalidManifest(error.message()))?;
        let intent = match explicit_intent {
            Some(id) => id,
            None => self
                .releases
                .matching_inflight_intent(
                    application.id,
                    workspace_id,
                    generation,
                    parsed.hash(&manifest).as_slice(),
                )
                .await?
                .unwrap_or_else(Uuid::new_v4),
        };
        let approval_id = optional_uuid_arg(arguments, "approval_id")?;
        let began = self
            .releases
            .begin(
                actor_user_id,
                application.id,
                intent,
                workspace_id,
                generation,
                &manifest,
                approval_id,
            )
            .await?;
        match began {
            (BeginRelease::ReadyToDispatch { id }, _) => {
                self.kick_complete_release(intent, workspace_id, root_path.clone());
                Ok(json!({ "state": "dispatched", "buildIntentId": intent, "releaseId": id }))
            }
            (BeginRelease::Ready { id }, Some(release)) => Ok(json!({
                "state": "ready",
                "releaseId": id,
                "release": super::release_json(&release),
            })),
            (BeginRelease::Ready { id }, None) => Ok(json!({
                "state": "ready",
                "releaseId": id,
            })),
            (BeginRelease::Failed { id }, Some(release)) => Ok(json!({
                "state": "failed",
                "releaseId": id,
                "release": super::release_json(&release),
            })),
            (BeginRelease::Failed { id }, None) => Ok(json!({
                "state": "failed",
                "releaseId": id,
            })),
            (BeginRelease::OutcomeUnknown, existing) => {
                let resource = existing
                    .as_ref()
                    .map(|release| release.id)
                    .unwrap_or(intent);
                Err(ApplicationError::outcome_unknown_at(
                    "release", resource, intent,
                ))
            }
            (BeginRelease::Conflict, _) => Ok(json!({
                "state": "conflict",
                "message": "this build intent conflicts; omit build_intent_id and call release.build again. Do not call application.create.",
            })),
        }
    }

    async fn tool_deploy(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        kind: &str,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        let application = self
            .applications
            .by_workspace(workspace_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let environments = self
            .applications
            .environments(actor_user_id, application.id)
            .await?;
        let environment = environments
            .iter()
            .find(|item| item.kind == kind)
            .ok_or(ApplicationError::NotFound)?;
        let release_id = self
            .resolve_deploy_release_id(actor_user_id, application.id, arguments)
            .await?;
        let intent = match optional_uuid_arg(arguments, "deployment_intent_id")? {
            Some(id) => id,
            None => self
                .deployments
                .latest_inflight_intent_for_release(environment.id, release_id)
                .await?
                .unwrap_or_else(Uuid::new_v4),
        };
        let approval_id = optional_uuid_arg(arguments, "approval_id")?;
        match self
            .deployments
            .deploy(
                actor_user_id,
                environment.id,
                release_id,
                intent,
                approval_id,
            )
            .await?
        {
            (BeginDeployment::ReadyToDispatch { id }, deployment) => {
                self.wake_deployment(id);
                Ok(json!({
                    "state": deployment.wire_state(),
                    "desiredState": deployment.desired_state,
                    "deploymentId": id,
                    "deploymentIntentId": deployment.deployment_intent_id,
                    "deployment": super::deployment_json(&deployment),
                }))
            }
            (BeginDeployment::Active { id }, deployment) => Ok(json!({
                "state": "active",
                "deploymentId": id,
                "deploymentIntentId": deployment.deployment_intent_id,
                "deployment": super::deployment_json(&deployment),
            })),
            (BeginDeployment::Failed { id }, deployment) => Ok(json!({
                "state": deployment.wire_state(),
                "deploymentId": id,
                "deploymentIntentId": deployment.deployment_intent_id,
                "deployment": super::deployment_json(&deployment),
            })),
            (BeginDeployment::OutcomeUnknown, deployment) => {
                Err(ApplicationError::outcome_unknown_at(
                    "deployment",
                    deployment.id,
                    deployment.deployment_intent_id,
                ))
            }
            (BeginDeployment::Conflict, _) => Ok(json!({
                "state": "conflict",
                "message": "this deploy intent conflicts; omit deployment_intent_id and call environment.deploy_dev again. Do not call application.create.",
            })),
        }
    }

    async fn tool_set_visibility(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        let application = self
            .applications
            .by_workspace(workspace_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let kind = required_str(arguments, "kind")?;
        let visibility = required_str(arguments, "visibility")?;
        let environments = self
            .applications
            .environments(actor_user_id, application.id)
            .await?;
        let environment = environments
            .iter()
            .find(|item| item.kind == kind)
            .ok_or(ApplicationError::NotFound)?;
        let approval_id = optional_uuid_arg(arguments, "approval_id")?;
        let updated = self
            .applications
            .set_visibility(actor_user_id, environment.id, visibility, approval_id)
            .await?;
        Ok(json!({ "environment": super::environment_json(&updated) }))
    }

    async fn tool_database_create(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        let application = self
            .applications
            .by_workspace(workspace_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let fabric_id = self.fabric_id_for_workspace(workspace_id).await?;
        let kind = required_str(arguments, "kind")?;
        let environments = self
            .applications
            .environments(actor_user_id, application.id)
            .await?;
        let environment = environments
            .iter()
            .find(|item| item.kind == kind)
            .ok_or(ApplicationError::NotFound)?;
        let hash = crate::applications::request_hash(&[b"create", environment.id.as_bytes()]);
        let elevated = arguments
            .get("elevated")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let approval_id = optional_uuid_arg(arguments, "approval_id")?;
        let database = self
            .databases
            .create_with_tier(
                actor_user_id,
                environment.id,
                fabric_id,
                Uuid::new_v4(),
                &hash,
                elevated,
                approval_id,
            )
            .await?;
        self.wake_database(database.id);
        Ok(json!({ "database": super::database_json(&database) }))
    }

    /// Explicit `database_id` wins. Otherwise Databases on this Application
    /// are listed. Optional `kind` limits the list to that Environment.
    async fn tool_database_status(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        if let Some(database_id) = optional_uuid_arg(arguments, "database_id")? {
            let database = self
                .require_bound_database(actor_user_id, workspace_id, database_id)
                .await?;
            return Ok(json!({ "database": super::database_json(&database) }));
        }
        let application = self
            .applications
            .by_workspace(workspace_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let environments = self
            .applications
            .environments(actor_user_id, application.id)
            .await?;
        let kind = field(arguments, "kind").and_then(Value::as_str);
        let mut databases = Vec::new();
        for environment in &environments {
            if let Some(want) = kind {
                if environment.kind != want {
                    continue;
                }
            }
            if let Ok(Some(database)) = self.databases.by_environment(environment.id).await {
                databases.push(database);
            }
        }
        let items: Vec<_> = databases.iter().map(super::database_json).collect();
        let mut body = json!({ "items": items });
        if databases.len() == 1 {
            body["database"] = super::database_json(&databases[0]);
        }
        Ok(body)
    }

    async fn require_bound_database(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        database_id: Uuid,
    ) -> Result<crate::databases::Database, ApplicationError> {
        let application = self
            .applications
            .by_workspace(workspace_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let database = self.databases.get(actor_user_id, database_id).await?;
        if database.application_id != application.id {
            return Err(ApplicationError::NotFound);
        }
        Ok(database)
    }

    async fn tool_database_backup(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        let database_id = required_uuid(arguments, "database_id")?;
        self.require_bound_database(actor_user_id, workspace_id, database_id)
            .await?;
        let operation_id = match optional_uuid_arg(arguments, "operation_id")? {
            Some(id) => id,
            None => self
                .databases
                .matching_backup_operation(database_id)
                .await?
                .unwrap_or_else(Uuid::new_v4),
        };
        let hash = crate::applications::request_hash(&[
            b"backup",
            database_id.as_bytes(),
            operation_id.as_bytes(),
        ]);
        let began = self
            .databases
            .begin_backup(actor_user_id, database_id, operation_id, &hash)
            .await?;
        match began {
            BeginDatabaseOp::ReadyToDispatch => {
                self.kick_complete_backup(database_id, operation_id);
                Ok(json!({
                    "databaseId": database_id,
                    "operationId": operation_id,
                    "state": "dispatched",
                    "kind": "manual",
                }))
            }
            BeginDatabaseOp::Ready => Ok(json!({
                "databaseId": database_id,
                "operationId": operation_id,
                "state": "ready",
                "kind": "manual",
            })),
            BeginDatabaseOp::Failed => Ok(json!({
                "databaseId": database_id,
                "operationId": operation_id,
                "state": "failed",
                "kind": "manual",
            })),
            BeginDatabaseOp::OutcomeUnknown => Err(ApplicationError::outcome_unknown_at(
                "database",
                database_id,
                operation_id,
            )),
            BeginDatabaseOp::Conflict => Ok(json!({
                "state": "conflict",
                "message": "this backup operation conflicts; omit operation_id and call database.backup again",
            })),
        }
    }

    async fn tool_database_list_backups(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        let database_id = required_uuid(arguments, "database_id")?;
        self.require_bound_database(actor_user_id, workspace_id, database_id)
            .await?;
        let items = self
            .databases
            .list_backups(actor_user_id, database_id)
            .await?;
        Ok(json!({
            "items": items.iter().map(|backup| json!({
                "id": backup.id,
                "databaseId": backup.database_id,
                "kind": backup.kind,
                "byteLength": backup.byte_length,
                "createdAt": backup.created_at,
            })).collect::<Vec<_>>()
        }))
    }

    async fn tool_database_restore(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        let database_id = required_uuid(arguments, "database_id")?;
        self.require_bound_database(actor_user_id, workspace_id, database_id)
            .await?;
        let backup_id = required_uuid(arguments, "backup_id")?;
        let hash = crate::databases::restore_request_hash(database_id, backup_id);
        let operation_id = match optional_uuid_arg(arguments, "operation_id")? {
            Some(id) => id,
            None => self
                .databases
                .matching_restore_operation(database_id, &hash)
                .await?
                .unwrap_or_else(Uuid::new_v4),
        };
        let approval_id = optional_uuid_arg(arguments, "approval_id")?;
        let began = self
            .databases
            .begin_restore(
                actor_user_id,
                database_id,
                backup_id,
                operation_id,
                approval_id,
                &hash,
            )
            .await?;
        match began {
            BeginDatabaseOp::ReadyToDispatch => {
                self.kick_complete_restore(database_id, backup_id, operation_id);
                Ok(json!({
                    "databaseId": database_id,
                    "backupId": backup_id,
                    "operationId": operation_id,
                    "state": "dispatched",
                }))
            }
            BeginDatabaseOp::Ready => Ok(json!({
                "databaseId": database_id,
                "backupId": backup_id,
                "operationId": operation_id,
                "state": "ready",
            })),
            BeginDatabaseOp::Failed => Ok(json!({
                "databaseId": database_id,
                "backupId": backup_id,
                "operationId": operation_id,
                "state": "failed",
            })),
            BeginDatabaseOp::OutcomeUnknown => Err(ApplicationError::outcome_unknown_at(
                "database",
                database_id,
                operation_id,
            )),
            BeginDatabaseOp::Conflict => Ok(json!({
                "state": "conflict",
                "message": "this restore operation conflicts; pass a fresh operation_id to restore this backup again",
            })),
        }
    }

    async fn tool_database_set_security_profile(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        let database_id = required_uuid(arguments, "database_id")?;
        self.require_bound_database(actor_user_id, workspace_id, database_id)
            .await?;
        let security_profile = required_i32(arguments, "security_profile")?;
        let database = self
            .databases
            .set_security_profile(actor_user_id, database_id, security_profile)
            .await?;
        self.wake_database(database.id);
        Ok(json!({ "database": super::database_json(&database) }))
    }

    /// Explicit `release_id` wins. Otherwise the newest ready
    /// Release on this Application is used so live deploy_dev can omit the id.
    async fn resolve_deploy_release_id(
        &self,
        actor_user_id: Uuid,
        application_id: Uuid,
        arguments: &Value,
    ) -> Result<Uuid, ApplicationError> {
        if let Some(id) = optional_uuid_arg(arguments, "release_id")? {
            return Ok(id);
        }
        let releases = self.releases.list(actor_user_id, application_id).await?;
        releases
            .into_iter()
            .rev()
            .find(|release| release.state == "ready")
            .map(|release| release.id)
            .ok_or(ApplicationError::NotFound)
    }

    /// Explicit `deployment_id` wins. Otherwise the newest
    /// healthy Deployment on this Application is used; if none is healthy,
    /// the newest candidate is used so activate still fail-closes as not ready.
    async fn resolve_activate_deployment_id(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Uuid, ApplicationError> {
        if let Some(id) = optional_uuid_arg(arguments, "deployment_id")? {
            return Ok(id);
        }
        let application = self
            .applications
            .by_workspace(workspace_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let environments = self
            .applications
            .environments(actor_user_id, application.id)
            .await?;
        let mut deployments = Vec::new();
        for environment in &environments {
            if let Ok((_, items)) = self.deployments.list(actor_user_id, environment.id).await {
                deployments.extend(items);
            }
        }
        deployments.sort_by(|left, right| {
            left.accepted_at
                .cmp(&right.accepted_at)
                .then(left.id.cmp(&right.id))
        });
        if let Some(owner) = deployments
            .iter()
            .rev()
            .find(|deployment| deployment.traffic)
        {
            return Ok(owner.id);
        }
        if let Some(proven) = deployments
            .iter()
            .rev()
            .find(|deployment| deployment.is_proven())
        {
            return Ok(proven.id);
        }
        deployments
            .into_iter()
            .rev()
            .next()
            .map(|deployment| deployment.id)
            .ok_or(ApplicationError::NotFound)
    }

    async fn tool_secret_metadata(
        &self,
        actor_user_id: Uuid,
        project_id: Uuid,
    ) -> Result<Value, ApplicationError> {
        crate::auth::authorize(
            self.applications.pool(),
            actor_user_id,
            project_id,
            Action::ReadProject,
        )
        .await?;
        let rows = sqlx::query(
            "select id, name, version from user_secrets where project_id = $1 order by name",
        )
        .bind(project_id)
        .fetch_all(self.applications.pool())
        .await
        .map_err(ApplicationError::from)?;
        let items: Vec<Value> = rows
            .into_iter()
            .map(|row| {
                json!({
                    "id": row.get::<Uuid, _>("id"),
                    "name": row.get::<String, _>("name"),
                    "version": row.get::<i64, _>("version"),
                })
            })
            .collect();
        Ok(json!({ "items": items }))
    }

    async fn tool_request_binding(
        &self,
        actor_user_id: Uuid,
        workspace_id: Uuid,
        arguments: &Value,
    ) -> Result<Value, ApplicationError> {
        let application = self
            .applications
            .by_workspace(workspace_id)
            .await?
            .ok_or(ApplicationError::NotFound)?;
        let kind = required_str(arguments, "kind")?;
        let name = required_str(arguments, "name")?;
        let secret_id = required_uuid(arguments, "secret_id")?;
        let environments = self
            .applications
            .environments(actor_user_id, application.id)
            .await?;
        let environment = environments
            .iter()
            .find(|item| item.kind == kind)
            .ok_or(ApplicationError::NotFound)?;
        let approval_id = optional_uuid_arg(arguments, "approval_id")?;
        let binding = self
            .bindings
            .bind(actor_user_id, environment.id, name, secret_id, approval_id)
            .await?;
        Ok(json!({
            "name": binding.environment_name,
            "secretId": binding.secret_id,
            "revision": binding.binding_revision,
        }))
    }
}

fn field<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key)
}

fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str, ApplicationError> {
    field(value, key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or(ApplicationError::InvalidName)
}

fn required_uuid(value: &Value, key: &str) -> Result<Uuid, ApplicationError> {
    let Some(text) = field(value, key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    else {
        return Err(ApplicationError::InvalidArgument {
            field: key.to_owned(),
            expected: "UUID",
        });
    };
    text.parse().map_err(|_| ApplicationError::InvalidArgument {
        field: key.to_owned(),
        expected: "UUID",
    })
}

fn required_i32(value: &Value, key: &str) -> Result<i32, ApplicationError> {
    let raw = field(value, key).ok_or(ApplicationError::InvalidName)?;
    if let Some(n) = raw.as_i64() {
        return i32::try_from(n).map_err(|_| ApplicationError::InvalidName);
    }
    if let Some(text) = raw.as_str() {
        return text.parse().map_err(|_| ApplicationError::InvalidName);
    }
    Err(ApplicationError::InvalidName)
}

fn optional_uuid_arg(value: &Value, key: &str) -> Result<Option<Uuid>, ApplicationError> {
    let Some(raw) = field(value, key) else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let Some(text) = raw.as_str() else {
        return Err(ApplicationError::InvalidArgument {
            field: key.to_owned(),
            expected: "UUID",
        });
    };
    if text.is_empty() {
        return Ok(None);
    }
    if text.eq_ignore_ascii_case("latest") {
        return Ok(None);
    }
    text.parse()
        .map(Some)
        .map_err(|_| ApplicationError::InvalidArgument {
            field: key.to_owned(),
            expected: "UUID",
        })
}

#[cfg(test)]
mod optional_uuid_tests {
    use super::optional_uuid_arg;
    use serde_json::json;

    #[test]
    fn optional_uuid_omits_blank_and_latest() {
        assert_eq!(
            optional_uuid_arg(&json!({"deployment_id": "latest"}), "deployment_id").unwrap(),
            None
        );
        assert_eq!(
            optional_uuid_arg(&json!({"deployment_id": ""}), "deployment_id").unwrap(),
            None
        );
        assert_eq!(
            optional_uuid_arg(&json!({}), "deployment_id").unwrap(),
            None
        );
        let id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        assert_eq!(
            optional_uuid_arg(&json!({"deployment_id": id}), "deployment_id")
                .unwrap()
                .map(|value| value.to_string())
                .as_deref(),
            Some(id)
        );
        assert!(matches!(
            optional_uuid_arg(&json!({"build_intent_id": "not-a-uuid"}), "build_intent_id"),
            Err(crate::applications::ApplicationError::InvalidArgument { .. })
        ));
    }
}
