//! Deployment desired-state wake. Fabric owns realization.

use std::future::Future;
use std::time::Duration;

use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::deployments::Deployment;
use crate::fabric_client::FabricError;
use crate::http::Platform;
use crate::http::orchestrate::{
    MigrateFabric, classify_migrate_fabric, hex_sha, manifest_migrate_argv, migrate_operation_id,
};
use crate::reconcile::{
    OBSERVE_AFTER_SECS, OBSERVE_RETRY_SECS, fabric_revision_caught_up, observed_satisfies_desired,
};

pub async fn reconcile_due(platform: &Platform) {
    let _ = platform
        .deployments
        .persist_absent_desired_for_removing_applications()
        .await;
    let Ok(rows) = sqlx::query(
        "select id from application_deployments \
         where desired_revision > observed_revision \
            or (desired_state = 'absent' \
                and coalesce(nullif(observed_state, ''), '') <> 'absent') \
            or (desired_state = 'stopped' \
                and coalesce(nullif(observed_state, ''), '') not in ('stopped', 'absent')) \
            or (desired_state = 'running' \
                and (reconcile_after <= now() \
                     or (reconcile_after is null \
                         and observed_state not in ('running', 'ready')))) \
         order by accepted_at, id \
         limit 32",
    )
    .fetch_all(platform.applications.pool())
    .await
    else {
        return;
    };
    for row in rows {
        let id: Uuid = row.get("id");
        if let Ok(deployment) = platform.deployments.get_internal(id).await {
            reconcile_one(platform, &deployment).await;
        }
    }
}

/// Mutation-path wake after PostgreSQL records desired state.
pub async fn put_due_deployment(platform: &Platform, id: Uuid) {
    let Ok(deployment) = platform.deployments.get_internal(id).await else {
        return;
    };
    reconcile_one(platform, &deployment).await;
}

pub fn spawn_loop(platform: Platform) {
    tokio::spawn(async move {
        loop {
            reconcile_due(&platform).await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

/// Leftover process is not a wake predicate. Desired running or desired
/// ahead of observed occupies the reconciler.
pub fn wakes_deployment_reconcile(desired: &str, desired_ahead: bool) -> bool {
    desired_ahead || desired == "running"
}

/// Heal the volume only after Control has already recorded
/// `needs_release_stream`. A same-turn stream can complete before the next
/// 1s Lost poll and hide the observation behind `running`.
pub fn rematerialize_after_stream_observation(
    previous_observed: &str,
    already_streamed: bool,
) -> bool {
    !already_streamed
        && previous_observed != "running"
        && previous_observed != "starting"
        && previous_observed != "healthy"
}

async fn reconcile_one(platform: &Platform, deployment: &Deployment) {
    let desired_ahead = deployment.desired_revision > deployment.observed_revision
        || (deployment.desired_state == "absent"
            && !observed_satisfies_desired(&deployment.desired_state, &deployment.observed_state));
    if desired_ahead {
        put_deployment_spec(
            platform,
            deployment.id,
            deployment.desired_revision,
            &deployment.desired_state,
            false,
        )
        .await;
        return;
    }
    observe_deployment_status(
        platform,
        deployment.id,
        deployment.desired_revision,
        &deployment.desired_state,
    )
    .await;
}

async fn observe_deployment_status(platform: &Platform, id: Uuid, revision: i64, desired: &str) {
    let Some(runtime) = platform.runtime.as_ref() else {
        settle_without_fabric(platform, id, desired).await;
        return;
    };
    match runtime
        .fabric
        .product_get(&format!("/v1/deployments/{id}"))
        .await
    {
        Ok(outcome) if outcome.state == "needs_release_stream" => {
            apply_needs_release_stream(platform, id, revision, desired, false).await;
        }
        Ok(outcome)
            if desired == "absent" && !observed_satisfies_desired(desired, &outcome.state) =>
        {
            put_deployment_spec(platform, id, revision, desired, false).await;
        }
        Ok(outcome) => {
            apply_deployment_outcome(platform, id, revision, desired, &outcome).await;
            continue_running_observe(platform, id, desired, &outcome.state).await;
        }
        Err(FabricError::Transport) => {
            record_deployment_observe_failure(platform, id, "fabric_unreachable").await;
        }
        Err(_) if desired == "absent" || desired == "stopped" || desired == "running" => {
            put_deployment_spec(platform, id, revision, desired, false).await;
        }
        Err(_) => {
            record_deployment_observe_failure(platform, id, "fabric_observe_failed").await;
        }
    }
}

async fn put_deployment_spec(
    platform: &Platform,
    id: Uuid,
    revision: i64,
    desired: &str,
    already_streamed: bool,
) {
    let Some(runtime) = platform.runtime.as_ref() else {
        settle_without_fabric(platform, id, desired).await;
        return;
    };
    let Ok(deployment) = platform.deployments.get_internal(id).await else {
        return;
    };
    let release = platform
        .releases
        .get_internal(deployment.release_id)
        .await
        .ok();
    if release.is_none() && desired != "absent" && desired != "stopped" {
        return;
    }
    if desired == "running" {
        let artifact_ready = release.as_ref().is_some_and(|item| {
            item.artifact_key.is_some()
                && item.artifact_hash.is_some()
                && item.artifact_bytes.is_some()
        });
        if !artifact_ready {
            fail_and_teardown(platform, id).await;
            return;
        }
    }
    let Ok(Some(environment)) = crate::applications::load_environment(
        platform.applications.pool(),
        deployment.environment_id,
    )
    .await
    else {
        return;
    };
    let Ok(application) = crate::applications::ApplicationStore::new(
        platform.applications.pool().clone(),
        String::new(),
    )
    .get_internal(environment.application_id)
    .await
    else {
        return;
    };
    let typed = release.as_ref().and_then(typed_running_fields);
    if desired == "running" && typed.is_none() {
        fail_and_teardown(platform, id).await;
        return;
    }
    let fields = typed.unwrap_or(TypedRunningFields {
        release_id: deployment.release_id,
        ..TypedRunningFields::empty()
    });
    let hash = fields.hash;
    let run_argv = fields.run_argv;
    let health_path = fields.health_path;
    let port = fields.port;
    let cpu_millis = fields.cpu_millis;
    let memory_mb = fields.memory_mb;
    let runtime_profile = fields.runtime_profile;
    let release_id = fields.release_id;
    let mut body = json!({
        "revision": revision,
        "desired": desired,
        "releaseId": release_id,
        "releaseHash": hash,
        "runtimeProfile": runtime_profile,
        "slug": application.slug,
        "kind": environment.kind,
        "port": port,
        "runArgv": run_argv,
        "healthPath": health_path,
        "cpuMillis": cpu_millis,
        "memoryMb": memory_mb,
    });
    if let Some(previous) = deployment.previous_deployment_id {
        body["previousDeploymentId"] = json!(previous);
    }
    body["podGeneration"] = json!(deployment.desired_pod_generation);
    let mut database_id = None;
    let mut injected_secrets = false;
    if desired == "running" {
        match stream_env_bindings(platform, environment.id).await {
            Some(bindings) => {
                if !bindings.is_empty() {
                    injected_secrets = true;
                }
                body["envBindings"] = json!(bindings);
            }
            None => return,
        }
        if let Ok(Some(database)) = platform.databases.by_environment(environment.id).await {
            if environment.kind == "prod" && database.environment_id != environment.id {
                return;
            }
            let id = database.id.to_string();
            body["databaseId"] = json!(id);
            database_id = Some(id);
            injected_secrets = true;
        }
    }
    let put = match after_logs_sensitive_fence(
        injected_secrets,
        platform.deployments.mark_logs_sensitive(id),
        || runtime.fabric.put_deployment_spec(id, &body),
    )
    .await
    {
        Ok(put) => put,
        Err(_) => {
            record_deployment_observe_failure(platform, id, "logs_sensitive_unproven").await;
            return;
        }
    };
    match put {
        Ok(outcome) if outcome.state == "needs_release_stream" => {
            apply_needs_release_stream(platform, id, revision, desired, already_streamed).await;
        }
        Ok(outcome) => {
            if desired == "running"
                && !dispatch_declared_migrate(
                    platform,
                    &deployment,
                    release.as_ref(),
                    database_id.as_deref(),
                )
                .await
            {
                return;
            }
            apply_deployment_outcome(platform, id, revision, desired, &outcome).await;
            continue_running_observe(platform, id, desired, &outcome.state).await;
        }
        Err(FabricError::Transport) => {
            record_deployment_observe_failure(platform, id, "fabric_unreachable").await;
        }
        Err(FabricError::Config(_)) if desired == "running" => {
            fail_and_teardown(platform, id).await;
        }
        Err(_) => {
            record_deployment_observe_failure(platform, id, "fabric_put_failed").await;
        }
    }
}

async fn stream_env_bindings(
    platform: &Platform,
    environment_id: Uuid,
) -> Option<Vec<serde_json::Value>> {
    let runtime = platform.runtime.as_ref()?;
    let Ok(bindings) = platform.bindings.list_internal(environment_id).await else {
        return None;
    };
    let mut streamed = Vec::new();
    for binding in bindings {
        match runtime
            .secrets
            .get_platform_material(binding.secret_id)
            .await
        {
            Ok(material) => match std::str::from_utf8(material.as_bytes()) {
                Ok(text) if !text.is_empty() => {
                    streamed.push(json!({
                        "name": binding.environment_name,
                        "value": text,
                    }));
                }
                _ => return None,
            },
            Err(_) => return None,
        }
    }
    Some(streamed)
}

/// Tenant migrate is an at-most-once journal. `false` means this tick
/// should not mark the Deployment observed-caught-up.
async fn dispatch_declared_migrate(
    platform: &Platform,
    deployment: &Deployment,
    release: Option<&crate::releases::Release>,
    database_id: Option<&str>,
) -> bool {
    let Some(runtime) = platform.runtime.as_ref() else {
        return true;
    };
    let Some(release) = release else {
        return true;
    };
    let Some(migrate) = manifest_migrate_argv(&release.manifest) else {
        return true;
    };
    let mut migrate_body = json!({
        "operation_id": migrate_operation_id(deployment.id),
        "request_hash": format!("migrate:{}", hex_sha(&deployment.request_hash)),
        "desired_revision": deployment.desired_revision,
        "migrate_argv": migrate,
    });
    if let Some(id) = database_id {
        migrate_body["database_id"] = json!(id);
    }
    match classify_migrate_fabric(
        &runtime
            .fabric
            .product_mutate(
                &format!("/v1/deployments/{}/migrate", deployment.id),
                &migrate_body,
            )
            .await,
    ) {
        MigrateFabric::Succeeded => true,
        MigrateFabric::Retry => {
            record_deployment_observe_failure(platform, deployment.id, "migrate_retry").await;
            false
        }
        MigrateFabric::OutcomeUnknown => {
            let _ = platform.deployments.unknown(deployment.id).await;
            false
        }
        MigrateFabric::DefiniteFailure => {
            fail_and_teardown(platform, deployment.id).await;
            false
        }
    }
}

async fn continue_running_observe(platform: &Platform, id: Uuid, desired: &str, observed: &str) {
    if desired != "running" {
        return;
    }
    if observed == "starting" || observed == "running" || observed == "healthy" {
        platform.probe_and_mark_healthy(id).await;
        platform.ship_deployment_logs(id).await;
    }
}

async fn apply_needs_release_stream(
    platform: &Platform,
    id: Uuid,
    revision: i64,
    desired: &str,
    already_streamed: bool,
) {
    let row = sqlx::query(
        "select coalesce(nullif(observed_state, ''), '') as observed, \
                coalesce(nullif(last_error_code, ''), '') as last_error \
         from application_deployments where id = $1",
    )
    .bind(id)
    .fetch_one(platform.applications.pool())
    .await;
    let (previous, last_error) = match row {
        Ok(row) => (
            row.get::<String, _>("observed"),
            row.get::<String, _>("last_error"),
        ),
        Err(_) => (String::new(), String::new()),
    };
    if last_error == "materialize_failed" {
        fail_and_teardown(platform, id).await;
        return;
    }
    // Persist needs_release_stream, then stream in this same turn so a
    // first deploy does not wait for the next reconciler tick. A 500 from
    // Fabric is retried; only missing artifact config tears the candidate
    // down.
    let rematerialize = rematerialize_after_stream_observation(&previous, already_streamed);
    let wake_secs = if rematerialize { OBSERVE_RETRY_SECS } else { 0 };
    let _ = sqlx::query(
        "update application_deployments set observed_state = 'needs_release_stream', \
         last_error_code = 'needs_release_stream', \
         reconcile_after = now() + ($2 * interval '1 second') where id = $1",
    )
    .bind(id)
    .bind(wake_secs)
    .execute(platform.applications.pool())
    .await;
    if !rematerialize {
        return;
    }
    match platform.rematerialize_deployment_from_release(id).await {
        Ok(()) => {
            Box::pin(put_deployment_spec(platform, id, revision, desired, true)).await;
        }
        Err(error) => {
            eprintln!("voie-cloud: rematerialize deployment {id}: {error}");
            match error {
                FabricError::Transport => {
                    record_deployment_observe_failure(platform, id, "fabric_unreachable").await;
                }
                FabricError::Config(_) => {
                    record_deployment_observe_failure(platform, id, "release_stream_failed").await;
                    fail_and_teardown(platform, id).await;
                }
                _ => {
                    record_deployment_observe_failure(platform, id, "release_stream_failed").await;
                }
            }
        }
    }
}

async fn apply_deployment_outcome(
    platform: &Platform,
    id: Uuid,
    revision: i64,
    desired: &str,
    outcome: &crate::fabric_client::ProductOutcome,
) {
    if observed_satisfies_desired(desired, &outcome.state)
        && fabric_revision_caught_up(outcome.observed_revision, revision)
    {
        let _ = sqlx::query(
            "update application_deployments \
             set observed_revision = $2, observed_state = $3, \
                 observed_pod_generation = coalesce($4, observed_pod_generation), \
                 last_error_code = case \
                     when $6 in ('absent', 'stopped') then last_error_code \
                     else null \
                 end, \
                 reconcile_after = now() + ($5 * interval '1 second') \
             where id = $1",
        )
        .bind(id)
        .bind(outcome.observed_revision)
        .bind(&outcome.state)
        .bind(outcome.observed_pod_generation)
        .bind(OBSERVE_AFTER_SECS)
        .bind(desired)
        .execute(platform.applications.pool())
        .await;
        settle_observed_teardown(platform, id, desired).await;
        return;
    }
    if observed_satisfies_desired(desired, &outcome.state) {
        let _ = sqlx::query(
            "update application_deployments \
             set observed_state = $2, last_error_code = 'fabric_revision_unproven', \
                 observed_revision = coalesce($3, observed_revision), \
                 observed_pod_generation = coalesce($4, observed_pod_generation), \
                 reconcile_after = now() + ($5 * interval '1 second') \
             where id = $1",
        )
        .bind(id)
        .bind(&outcome.state)
        .bind(outcome.observed_revision)
        .bind(outcome.observed_pod_generation)
        .bind(OBSERVE_RETRY_SECS)
        .execute(platform.applications.pool())
        .await;
        settle_observed_teardown(platform, id, desired).await;
        return;
    }
    if desired == "running" && outcome.state == "starting" {
        let _ = sqlx::query(
            "update application_deployments set observed_state = 'starting', \
             last_error_code = null, \
             reconcile_after = now() + ($2 * interval '1 second') where id = $1",
        )
        .bind(id)
        .bind(OBSERVE_RETRY_SECS)
        .execute(platform.applications.pool())
        .await;
        return;
    }
    let error = outcome
        .last_error_code
        .as_deref()
        .unwrap_or("observed_not_desired");
    let _ = sqlx::query(
        "update application_deployments set observed_state = $2, last_error_code = $3, \
         reconcile_after = now() + ($4 * interval '1 second') where id = $1",
    )
    .bind(id)
    .bind(&outcome.state)
    .bind(error)
    .bind(OBSERVE_RETRY_SECS)
    .execute(platform.applications.pool())
    .await;
}

async fn settle_observed_teardown(platform: &Platform, id: Uuid, desired: &str) {
    match desired {
        "absent" => {
            let _ = platform.deployments.commit_absent(id).await;
            platform.kick_route_map();
        }
        "stopped" => {
            let _ = platform.deployments.commit_stop(id).await;
            platform.kick_route_map();
        }
        _ => {}
    }
}

async fn settle_without_fabric(platform: &Platform, id: Uuid, desired: &str) {
    settle_observed_teardown(platform, id, desired).await;
}

async fn fail_and_teardown(platform: &Platform, id: Uuid) {
    let _ = platform.deployments.fail(id).await;
    if let Ok(deployment) = platform.deployments.get_internal(id).await {
        if deployment.desired_state == "absent" {
            Box::pin(put_deployment_spec(
                platform,
                id,
                deployment.desired_revision,
                "absent",
                false,
            ))
            .await;
        }
    }
}

async fn record_deployment_observe_failure(platform: &Platform, id: Uuid, code: &str) {
    let _ = sqlx::query(
        "update application_deployments set last_error_code = $2, \
         reconcile_after = now() + ($3 * interval '1 second') where id = $1",
    )
    .bind(id)
    .bind(code)
    .bind(OBSERVE_RETRY_SECS)
    .execute(platform.applications.pool())
    .await;
}

struct TypedRunningFields {
    hash: String,
    run_argv: Vec<String>,
    health_path: String,
    port: u16,
    cpu_millis: u32,
    memory_mb: u32,
    runtime_profile: String,
    release_id: Uuid,
}

impl TypedRunningFields {
    fn empty() -> Self {
        TypedRunningFields {
            hash: String::new(),
            run_argv: Vec::new(),
            health_path: String::new(),
            port: 0,
            cpu_millis: 0,
            memory_mb: 0,
            runtime_profile: String::new(),
            release_id: Uuid::nil(),
        }
    }
}

/// Secret material may reach Fabric only after `logs_sensitive` is durable.
/// `put` is not called when the marker cannot be persisted.
pub(crate) async fn after_logs_sensitive_fence<Mark, Put, PutFut, T, E>(
    injected_secrets: bool,
    mark: Mark,
    put: Put,
) -> Result<T, E>
where
    Mark: Future<Output = Result<(), E>>,
    Put: FnOnce() -> PutFut,
    PutFut: Future<Output = T>,
{
    if injected_secrets {
        mark.await?;
    }
    Ok(put().await)
}

fn json_argv(value: &serde_json::Value) -> Option<Vec<String>> {
    let items = value.as_array()?;
    let mut command = Vec::with_capacity(items.len());
    for item in items {
        let part = item.as_str()?;
        if part.is_empty() {
            return None;
        }
        command.push(part.to_owned());
    }
    if command.is_empty() {
        None
    } else {
        Some(command)
    }
}

fn typed_running_fields(release: &crate::releases::Release) -> Option<TypedRunningFields> {
    let hash = release
        .artifact_hash
        .as_ref()
        .filter(|bytes| !bytes.is_empty())?;
    let run = release.manifest.get("run")?;
    let run_argv = json_argv(run.get("command")?)?;
    let health_path = run.get("healthPath")?.as_str()?.to_owned();
    if !health_path.starts_with('/') || health_path.contains("..") {
        return None;
    }
    let port = u16::try_from(run.get("port")?.as_u64()?).ok()?;
    if port == 0 {
        return None;
    }
    let resources = release.manifest.get("resources")?;
    let cpu_millis = u32::try_from(resources.get("cpuMillis")?.as_u64()?).ok()?;
    let memory_mb = u32::try_from(resources.get("memoryMb")?.as_u64()?).ok()?;
    if cpu_millis == 0 || memory_mb == 0 || release.runtime_profile.is_empty() {
        return None;
    }
    Some(TypedRunningFields {
        hash: hash.iter().map(|b| format!("{b:02x}")).collect(),
        run_argv,
        health_path,
        port,
        cpu_millis,
        memory_mb,
        runtime_profile: release.runtime_profile.clone(),
        release_id: release.id,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        after_logs_sensitive_fence, rematerialize_after_stream_observation,
        wakes_deployment_reconcile,
    };

    #[test]
    fn stopped_history_does_not_starve_live_observe() {
        assert!(wakes_deployment_reconcile("running", false));
        assert!(wakes_deployment_reconcile("absent", true));
        assert!(!wakes_deployment_reconcile("absent", false));
        assert!(!wakes_deployment_reconcile("stopped", false));
    }

    #[test]
    fn streams_on_first_needs_release_sighting() {
        assert!(!rematerialize_after_stream_observation("running", false));
        assert!(!rematerialize_after_stream_observation("starting", false));
        assert!(rematerialize_after_stream_observation("accepted", false));
        assert!(rematerialize_after_stream_observation("", false));
        assert!(rematerialize_after_stream_observation(
            "needs_release_stream",
            false
        ));
        assert!(!rematerialize_after_stream_observation(
            "needs_release_stream",
            true
        ));
    }

    #[tokio::test]
    async fn fabric_put_is_never_reached_if_sensitivity_write_fails() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let reached = AtomicBool::new(false);
        let result: Result<&str, &str> =
            after_logs_sensitive_fence(true, async { Err("postgres unavailable") }, || async {
                reached.store(true, Ordering::SeqCst);
                "fabric-put"
            })
            .await;
        assert_eq!(result, Err("postgres unavailable"));
        assert!(
            !reached.load(Ordering::SeqCst),
            "Fabric PUT must not run when logs_sensitive cannot persist"
        );
    }

    #[tokio::test]
    async fn fabric_put_runs_after_sensitivity_write() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let reached = AtomicBool::new(false);
        let result: Result<&str, &str> =
            after_logs_sensitive_fence(true, async { Ok(()) }, || async {
                reached.store(true, Ordering::SeqCst);
                "fabric-put"
            })
            .await;
        assert_eq!(result, Ok("fabric-put"));
        assert!(reached.load(Ordering::SeqCst));
    }
}
