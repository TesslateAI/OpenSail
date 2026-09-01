//! Narrow typed Application/Release/Database Fabric operations.
//! Kubernetes objects, images, host paths, and Caddy fragments are refused.

use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Digest;
use uuid::Uuid;

use crate::product_realize::{
    self, app_pod_name, app_service_name, compact_id, deployment_volume_name, postgres_pod_for_lv,
    postgres_pod_name, postgres_pvc_for_lv, postgres_restore_pod_name,
    postgres_restore_volume_name, postgres_service_name, postgres_volume_name, release_volume_name,
    AppIntent, DatabaseIntent,
};
use crate::{file_stream_response, full_body, json_response, FabricBody, FabricError};

fn abandon_or_error(
    fabric: &crate::Fabric,
    kind: &str,
    resource_id: &str,
    operation_id: &str,
    state: &str,
) -> Option<Response<FabricBody>> {
    fabric
        .abandon_staging_operation(kind, resource_id, operation_id, state)
        .err()
        .map(crate::error_response)
}

const FORBIDDEN_KEYS: &[&str] = &[
    "pod",
    "spec",
    "manifest",
    "yaml",
    "image",
    "hostPath",
    "host_path",
    "command",
    "namespace",
    "caddy",
    "networkPolicy",
    "network_policy",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MutatingBody {
    pub operation_id: Uuid,
    pub request_hash: String,
    pub desired_revision: i64,
    #[serde(default)]
    pub artifact_hash: Option<String>,
    #[serde(default)]
    pub byte_length: Option<i64>,
    #[serde(default)]
    pub release_id: Option<Uuid>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub health_path: Option<String>,
    #[serde(default)]
    pub run_argv: Option<Vec<String>>,
    #[serde(default)]
    pub console_host: Option<String>,
    /// One-time Database password. Never journaled; dropped after realization.
    #[serde(default)]
    pub postgres_password: Option<String>,
    /// Database identity whose Fabric-owned credential is copied into the
    /// Application env secret as `DATABASE_URL`. The password is never in this
    /// body on the Deployment path.
    #[serde(default)]
    pub database_id: Option<String>,
    /// One-time Environment binding values streamed from voie-cloud. Dropped
    /// after realization; never journaled or written into a Pod template.
    #[serde(default)]
    pub env_bindings: Option<Vec<EnvBinding>>,
    /// Predecessor Deployment identity. Activate demotes it so a later stop
    /// cannot tear down the new Environment edge.
    #[serde(default)]
    pub previous_deployment_id: Option<Uuid>,
    /// Typed `voie.toml` migration argv. Runs in the Application container.
    #[serde(default)]
    pub migrate_argv: Option<Vec<String>>,
    /// Platform CPU millicores from the Release manifest. Clamped to Profile 1 limits.
    #[serde(default)]
    pub cpu_millis: Option<u32>,
    /// Platform memory in MiB from the Release manifest. Clamped to Profile 1 limits.
    #[serde(default)]
    pub memory_mb: Option<u32>,
    #[serde(default)]
    pub allocated_bytes: Option<u64>,
    #[serde(default)]
    pub elevated: Option<bool>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct EnvBinding {
    pub name: String,
    pub value: String,
}

pub fn is_product_path(path: &str) -> bool {
    path.starts_with("/v1/releases/")
        || path == "/v1/deployments"
        || path.starts_with("/v1/deployments/")
        || path == "/v1/databases"
        || path.starts_with("/v1/databases/")
}

/// Deployment and Database create are keyed by the durable cloud UUID in the
/// path. Collection POST without that id is not a product route: health,
/// activate, stop, and delete must address the same journal row the Pod uses.
fn parse_product_route<'a>(
    method: &Method,
    parts: &[&'a str],
) -> Option<(&'static str, &'a str, &'static str)> {
    match (method, parts) {
        (&Method::POST, ["v1", "releases", id, "materialize"]) => {
            Some(("release", *id, "materialize"))
        }
        (&Method::GET, ["v1", "releases", id]) => Some(("release", *id, "get")),
        (&Method::DELETE, ["v1", "releases", id]) => Some(("release", *id, "delete")),
        (&Method::POST, ["v1", "releases", id, "delete"]) => Some(("release", *id, "delete")),
        (&Method::POST, ["v1", "deployments", id]) => Some(("deployment", *id, "create")),
        (&Method::GET, ["v1", "deployments", id]) => Some(("deployment", *id, "get")),
        (&Method::POST, ["v1", "deployments", id, "activate"]) => {
            Some(("deployment", *id, "activate"))
        }
        (&Method::POST, ["v1", "deployments", id, "restart"]) => {
            Some(("deployment", *id, "restart"))
        }
        (&Method::POST, ["v1", "deployments", id, "stop"]) => Some(("deployment", *id, "stop")),
        (&Method::POST, ["v1", "deployments", id, "migrate"]) => {
            Some(("deployment", *id, "migrate"))
        }
        (&Method::DELETE, ["v1", "deployments", id]) => Some(("deployment", *id, "delete")),
        (&Method::GET, ["v1", "deployments", id, "logs"]) => Some(("deployment", *id, "logs")),
        (&Method::POST, ["v1", "deployments", id, "health"]) => Some(("deployment", *id, "health")),
        (&Method::POST, ["v1", "databases", id]) => Some(("database", *id, "create")),
        (&Method::GET, ["v1", "databases", id]) => Some(("database", *id, "get")),
        (&Method::POST, ["v1", "databases", id, "backup"]) => Some(("database", *id, "backup")),
        (&Method::DELETE, ["v1", "databases", id, "backup"]) => {
            Some(("database", *id, "ack-backup"))
        }
        (&Method::POST, ["v1", "databases", id, "restore"]) => Some(("database", *id, "restore")),
        (&Method::PUT, ["v1", "databases", id, "restore-artifact"]) => {
            Some(("database", *id, "restore-artifact"))
        }
        (&Method::DELETE, ["v1", "databases", id]) => Some(("database", *id, "delete")),
        (&Method::POST, ["v1", "databases", id, "delete"]) => Some(("database", *id, "delete")),
        _ => None,
    }
}

pub async fn handle(
    fabric: &crate::Fabric,
    method: Method,
    path: &str,
    request: Request<Incoming>,
) -> Response<FabricBody> {
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    if let ["v1", "releases", id, "artifact"] = parts.as_slice() {
        if method == Method::PUT {
            return put_release_artifact(fabric, id, request).await;
        }
        if method == Method::GET {
            return get_release_artifact(fabric, id);
        }
    }
    let Some((kind, resource_id, action)) = parse_product_route(&method, &parts) else {
        return json_response(
            StatusCode::NOT_FOUND,
            json!({ "error": "not_found" }).to_string(),
        );
    };
    if action == "get" {
        if kind == "database" {
            return probe_database_ready(fabric, resource_id).await;
        }
        return match fabric.get_product_resource(kind, resource_id) {
            Ok(Some((_, _, state))) => json_response(
                StatusCode::OK,
                json!({ "id": resource_id, "kind": kind, "state": state }).to_string(),
            ),
            Ok(None) => crate::error_response(FabricError::NotFound),
            Err(error) => crate::error_response(error),
        };
    }
    if action == "logs" {
        return deployment_logs(fabric, resource_id).await;
    }
    if action == "health" {
        return probe_deployment_health(fabric, resource_id, request).await;
    }
    if action == "backup" {
        return backup_database(fabric, resource_id, request).await;
    }
    if action == "ack-backup" {
        return ack_database_backup(fabric, resource_id, request).await;
    }
    if action == "restore-artifact" {
        return put_restore_artifact(fabric, resource_id, request).await;
    }
    match read_and_validate(request).await {
        Err(error) => crate::error_response(error),
        Ok(mut body) => {
            let password = body.postgres_password.take();
            let env_bindings = body.env_bindings.take().unwrap_or_default();
            let resource = resource_id.to_owned();
            // App Ready and voie-gateway Ready must hold before the typed
            // activate/stop journal. A leftover dispatched row is treated
            // as unknown and is not replayed; Conflict leaves the selector
            // unchanged so cloud can retry.
            if kind == "deployment" && action == "activate" {
                match fabric
                    .live()
                    .wait_pod_ready(&app_pod_name(&resource), Duration::from_secs(30))
                    .await
                {
                    Ok(_) => {}
                    Err(error) => {
                        return crate::error_response(retryable_unready(
                            error,
                            "application pod is not Ready",
                        ));
                    }
                }
            }
            if kind == "deployment"
                && (action == "activate" || action == "stop" || action == "delete")
            {
                if let Err(error) = ensure_gateway_ready(fabric).await {
                    return crate::error_response(error);
                }
            }
            match fabric.begin_product_operation(
                kind,
                &resource,
                &body.operation_id.to_string(),
                &body.request_hash,
            ) {
                Ok(state) if should_realize_product_op(kind, action, &state) => {
                    let replay = state == "terminal";
                    match realize_desired(
                        fabric,
                        kind,
                        action,
                        &resource,
                        &body,
                        password.as_deref(),
                        &env_bindings,
                    )
                    .await
                    {
                        Ok(()) => {
                            if !replay {
                                let _ = fabric.complete_product_operation(
                                    kind,
                                    &resource,
                                    &body.operation_id.to_string(),
                                    "terminal",
                                );
                            }
                            operation_response(&state, &resource, body.operation_id)
                        }
                        Err(error) => {
                            if !replay {
                                let _ = fabric.complete_product_operation(
                                    kind,
                                    &resource,
                                    &body.operation_id.to_string(),
                                    replayable_journal_on_error(kind, action, &error),
                                );
                            }
                            crate::error_response(error)
                        }
                    }
                }
                Ok(state) => operation_response(&state, &resource, body.operation_id),
                Err(error) => crate::error_response(error),
            }
        }
    }
}

async fn realize_desired(
    fabric: &crate::Fabric,
    kind: &str,
    action: &str,
    resource: &str,
    body: &MutatingBody,
    postgres_password: Option<&str>,
    env_bindings: &[EnvBinding],
) -> Result<(), FabricError> {
    match (kind, action) {
        ("release", "materialize") => {
            let staged = fabric
                .release_root()
                .join(resource)
                .join("artifact.tar.zst");
            if let Some(expected) = body.artifact_hash.as_deref() {
                verify_file_hash(&staged, expected)?;
            } else if !staged.is_file() {
                return Err(FabricError::Realize(
                    "release artifact has not been staged".into(),
                ));
            }
            fabric.upsert_product_resource(
                kind,
                resource,
                None,
                None,
                body.artifact_hash.as_deref(),
                "ready",
            )?;
            Ok(())
        }
        ("release", "delete") => {
            delete_local_volume(fabric, &release_volume_name(resource)).await?;
            fabric
                .live()
                .release_block(&crate::BlockSlot {
                    device: String::new(),
                    lv_name: Some(crate::lv_name_for_release(resource)),
                    ..Default::default()
                })
                .await?;
            fabric.purge_product_resource(kind, resource)?;
            let _ = std::fs::remove_dir_all(fabric.release_root().join(resource));
            Ok(())
        }
        ("deployment", "create") => {
            // Candidate only: Pod + per-Deployment NetworkPolicy. Platform
            // egress is ensured separately so a Running voie-egress Pod is
            // not kubectl-applied (resource fields are immutable).
            let release_id = body
                .release_id
                .ok_or(FabricError::Config("deployment release is required"))?
                .to_string();
            materialize_deployment_volume(fabric, resource, &release_id, body.slug.as_deref())
                .await?;
            let env_secret = bind_application_env(
                fabric,
                resource,
                body.database_id.as_deref(),
                env_bindings,
                body.slug.as_deref(),
            )
            .await?;
            let yaml = application_pod_yaml(fabric, resource, body, env_secret.as_deref())?;
            ensure_egress_present(fabric).await?;
            ensure_application_policy_present(fabric).await?;
            let policies = deployment_network_yaml(fabric, resource, body)?;
            let combined = [yaml.as_str(), policies.as_str()].join("\n---\n");
            apply_or_unknown(fabric, &combined).await?;
            fabric.upsert_product_resource(
                kind,
                resource,
                Some(&app_pod_name(resource)),
                None,
                None,
                "starting",
            )?;
            fabric.set_product_desired_yaml(kind, resource, &yaml)?;
            Ok(())
        }
        ("deployment", "activate") => {
            let slug = body
                .slug
                .as_deref()
                .ok_or(FabricError::Config("deployment slug is required"))?;
            let env_kind = body
                .kind
                .as_deref()
                .ok_or(FabricError::Config("deployment kind is required"))?;
            let port = body.port.unwrap_or(3000);
            let host = body
                .console_host
                .as_deref()
                .ok_or(FabricError::Config("console host is required"))?;
            let switched = product_realize::app_service_selector_yaml(
                fabric.live(),
                slug,
                env_kind,
                resource,
                port,
            )?;
            refuse_user_infrastructure(&switched)?;
            apply_or_unknown(fabric, &switched)
                .await
                .map_err(|error| retryable_unready(error, "environment selector is not applied"))?;
            let service_name = app_service_name(slug, env_kind);
            fabric.upsert_product_resource(
                kind,
                resource,
                Some(&app_pod_name(resource)),
                Some(&service_name),
                None,
                "active",
            )?;
            if let Some(previous) = body.previous_deployment_id {
                let previous = previous.to_string();
                if previous != resource {
                    if let Ok(Some((pod, service, _))) =
                        fabric.get_product_resource(kind, &previous)
                    {
                        fabric.upsert_product_resource(
                            kind,
                            &previous,
                            pod.as_deref(),
                            service.as_deref(),
                            None,
                            "superseded",
                        )?;
                    }
                }
            }
            let cluster_ip = fabric
                .live()
                .service_cluster_ip(&service_name)
                .await
                .map_err(|error| retryable_unready(error, "environment selector is not applied"))?;
            fabric.upsert_gateway_route(slug, env_kind, &format!("{cluster_ip}:{port}"), host)?;
            // Selector is switched. A gateway reload timeout must not
            // journal unknown: leftover dispatched is remapped and never
            // replays. Conflict + terminal lets the next activate reload.
            if let Err(error) = realize_gateway_routes(fabric).await {
                return Err(retryable_unready(error, "voie-gateway is not Ready"));
            }
            Ok(())
        }
        ("deployment", "restart") => {
            if let Some(release_id) = body.release_id {
                materialize_deployment_volume(
                    fabric,
                    resource,
                    &release_id.to_string(),
                    body.slug.as_deref(),
                )
                .await?;
            }
            let previous = fabric.get_product_resource(kind, resource).ok().flatten();
            let keep_active = previous
                .as_ref()
                .is_some_and(|(_, _, state)| state == "active");
            let service_name = previous.and_then(|(_, service, _)| service);
            let _ = fabric
                .live()
                .delete_named("pod", &app_pod_name(resource), true, 60)
                .await;
            let env_secret = product_realize::app_env_secret_name(resource);
            let yaml = application_pod_yaml(fabric, resource, body, Some(&env_secret))?;
            // Per-Deployment policy only. Shared application/gateway
            // NetworkPolicies stay put: re-applying them with the candidate
            // Pod dropped the public Host matcher ("not found") after restart.
            let intent = app_intent(resource, body)?;
            let postgres =
                product_realize::application_postgres_policy_yaml(fabric.live(), &intent)?;
            refuse_user_infrastructure(&postgres)?;
            if postgres.contains("ipBlock") || postgres.contains("fromEntities") {
                return Err(FabricError::Realize(
                    "application postgres policy must not carry CIDR or host entities".into(),
                ));
            }
            apply_or_unknown(fabric, &format!("{yaml}\n---\n{postgres}")).await?;
            fabric.upsert_product_resource(
                kind,
                resource,
                Some(&app_pod_name(resource)),
                service_name.as_deref(),
                None,
                if keep_active { "active" } else { "starting" },
            )?;
            fabric.set_product_desired_yaml(kind, resource, &yaml)?;
            // Ready is observational. A 90s kubelet wait here would journal
            // unknown on a slow Firecracker boot; cloud already sets SQL
            // `starting` and GET/health resume waits Endpoints.
            Ok(())
        }
        ("deployment", "stop") | ("deployment", "delete") => {
            let owns_edge = fabric
                .get_product_resource(kind, resource)
                .ok()
                .flatten()
                .is_some_and(|(_, _, state)| state == "active");
            if owns_edge {
                if let (Some(slug), Some(env_kind)) = (body.slug.as_deref(), body.kind.as_deref()) {
                    fabric.delete_gateway_route(slug, env_kind)?;
                    // Route is gone from SQLite. Gateway reload Conflict is
                    // still a successful stop journal so C5 can replay.
                    if let Err(error) = realize_gateway_routes(fabric).await {
                        return Err(retryable_unready(error, "voie-gateway is not Ready"));
                    }
                }
                delete_named_retryable(
                    fabric,
                    "svc",
                    &app_service_name(
                        body.slug.as_deref().unwrap_or(""),
                        body.kind.as_deref().unwrap_or("dev"),
                    ),
                    true,
                    30,
                )
                .await?;
            }
            delete_named_retryable(fabric, "pod", &app_pod_name(resource), true, 60).await?;
            delete_named_retryable(
                fabric,
                "secret",
                &product_realize::app_env_secret_name(resource),
                true,
                30,
            )
            .await?;
            delete_named_retryable(
                fabric,
                "networkpolicy",
                &product_realize::application_postgres_policy_name(resource),
                true,
                30,
            )
            .await?;
            delete_local_volume(fabric, &deployment_volume_name(resource)).await?;
            fabric
                .free_volume(crate::VolumeKind::Deployment, resource)
                .await?;
            fabric.purge_product_resource(kind, resource)?;
            Ok(())
        }
        ("deployment", "migrate") => {
            migrate_application(fabric, resource, body).await?;
            Ok(())
        }
        ("database", "create") => {
            let password = postgres_password.ok_or(FabricError::Config(
                "database password is required once and is never journaled",
            ))?;
            let intent = DatabaseIntent {
                database_id: resource.to_owned(),
                slug: body.slug.clone().unwrap_or_default(),
                kind: body.kind.clone().unwrap_or_else(|| "dev".into()),
            };
            let _ = delete_local_volume(fabric, &live_postgres_volume(fabric, resource)).await;
            let prod = intent.kind == "prod";
            let bytes = body.allocated_bytes.unwrap_or_else(|| {
                fabric
                    .live()
                    .storage()
                    .database_size(prod, body.elevated.unwrap_or(false))
            });
            if !fabric
                .live()
                .storage()
                .matches_tier(crate::VolumeKind::Database, bytes, prod)
            {
                return Err(FabricError::Conflict(
                    "database size is not a platform storage tier".into(),
                ));
            }
            let slot = fabric
                .allocate_volume(
                    crate::VolumeKind::Database,
                    resource,
                    bytes,
                    Some(&body.operation_id.to_string()),
                )
                .await?;
            fabric.live().mkfs_ext4_if_needed(&slot.device).await?;
            let volume = postgres_volume_name(resource);
            let pv = product_realize::postgres_pv_yaml(
                fabric.live(),
                resource,
                &slot.device,
                body.slug.as_deref(),
                bytes,
            );
            let pvc = product_realize::postgres_pvc_yaml(
                fabric.live(),
                resource,
                body.slug.as_deref(),
                bytes,
            );
            crate::realize::require_stable_block_path(&slot.device)?;
            refuse_user_infrastructure(&pv)?;
            refuse_user_infrastructure(&pvc)?;
            apply_or_unknown(fabric, &format!("{pv}\n---\n{pvc}")).await?;
            let yaml =
                product_realize::postgres_pod_yaml(fabric.live(), &intent, &volume, resource);
            let service = product_realize::postgres_service_yaml(fabric.live(), &intent, resource);
            let policy = product_realize::postgres_network_policy_yaml(fabric.live(), &intent)?;
            refuse_user_infrastructure(&yaml)?;
            refuse_user_infrastructure(&service)?;
            refuse_user_infrastructure(&policy)?;
            if yaml.contains("POSTGRES_PASSWORD") || service.contains("POSTGRES_PASSWORD") {
                return Err(FabricError::Realize(
                    "postgres manifest must not embed credentials".into(),
                ));
            }
            if policy.contains("ipBlock") || policy.contains("fromEntities") {
                return Err(FabricError::Realize(
                    "postgres network policy must not carry CIDR or host entities".into(),
                ));
            }
            let mut pg_labels: Vec<(&str, &str)> =
                vec![("io.voie/kind", "postgres"), ("io.voie/database", resource)];
            if let Some(slug) = body.slug.as_deref().filter(|value| !value.is_empty()) {
                pg_labels.push(("io.voie/slug", slug));
            }
            fabric
                .live()
                .apply_opaque_secret(
                    &product_realize::postgres_secret_name(resource),
                    "postgres-password",
                    password.as_bytes(),
                    &pg_labels,
                )
                .await
                .map_err(|error| FabricError::Unknown(error.to_string()))?;
            let combined = format!("{yaml}\n---\n{service}\n---\n{policy}");
            apply_or_unknown(fabric, &combined).await?;
            // Ready is observational (GET). A 180s kubelet wait here would
            // journal unknown on a slow Firecracker initdb and begin would
            // never replay the typed create.
            fabric.upsert_product_resource(
                kind,
                resource,
                Some(&postgres_pod_name(resource)),
                Some(&postgres_service_name(resource)),
                Some(resource),
                "creating",
            )?;
            fabric.set_product_desired_yaml(kind, resource, &yaml)?;
            Ok(())
        }
        ("database", "delete") => {
            delete_named_retryable(
                fabric,
                "pod",
                &live_postgres_pod(fabric, resource),
                true,
                60,
            )
            .await?;
            delete_named_retryable(fabric, "svc", &postgres_service_name(resource), true, 30)
                .await?;
            delete_named_retryable(
                fabric,
                "secret",
                &product_realize::postgres_secret_name(resource),
                true,
                30,
            )
            .await?;
            delete_named_retryable(
                fabric,
                "networkpolicy",
                &product_realize::postgres_network_policy_name(resource),
                true,
                30,
            )
            .await?;
            delete_local_volume(fabric, &live_postgres_volume(fabric, resource)).await?;
            fabric
                .free_volume(crate::VolumeKind::Database, resource)
                .await?;
            let _ = std::fs::remove_dir_all(fabric.postgres_root().join(resource));
            fabric.purge_product_resource(kind, resource)?;
            Ok(())
        }
        ("database", "restore") => {
            let _lock = fabric
                .lifecycle_guard(&format!("database:{resource}"))
                .await;
            restore_database_dump(fabric, resource, body, postgres_password).await?;
            Ok(())
        }
        _ => Err(FabricError::Config("unsupported product operation")),
    }
}

/// Running platform egress must not be `kubectl apply`'d again. Resource
/// requests on the generated Pod are immutable once the live Pod exists;
/// a multi-doc apply that included the candidate Application then failed
/// the egress update and journaled typed unknown after the app Pod existed.
fn egress_pod_needs_replace(phase: &str, host_network: bool) -> bool {
    host_network || phase == "Failed" || phase == "Succeeded"
}

async fn ensure_egress_present(fabric: &crate::Fabric) -> Result<(), FabricError> {
    let egress_svc = product_realize::egress_service_yaml(fabric.live());
    refuse_user_infrastructure(&egress_svc)?;
    apply_or_unknown(fabric, &egress_svc).await?;
    let egress_policy = product_realize::egress_network_policy_yaml(fabric.live());
    refuse_user_infrastructure(&egress_policy)?;
    apply_or_unknown(fabric, &egress_policy).await?;
    let pod_yaml = product_realize::egress_pod_yaml(fabric.live());
    refuse_user_infrastructure(&pod_yaml)?;
    let pod_name = product_realize::egress_pod_name();
    match fabric.live().get_pod(&pod_name).await {
        Ok(None) => apply_or_unknown(fabric, &pod_yaml).await,
        Ok(Some(pod)) if egress_pod_needs_replace(&pod.phase, pod.host_network) => {
            fabric
                .live()
                .delete_named("pod", &pod_name, true, 30)
                .await
                .map_err(|error| FabricError::Unknown(error.to_string()))?;
            apply_or_unknown(fabric, &pod_yaml).await
        }
        Ok(Some(_)) => Ok(()),
        Err(error) => Err(FabricError::Unknown(error.to_string())),
    }
}

async fn apply_or_unknown(fabric: &crate::Fabric, yaml: &str) -> Result<(), FabricError> {
    match fabric.live().apply_yaml(yaml).await {
        Ok(()) => Ok(()),
        Err(FabricError::Unknown(message)) => Err(FabricError::Unknown(message)),
        Err(error) => Err(FabricError::Unknown(error.to_string())),
    }
}

/// kubectl delete --ignore-not-found is idempotent. Timeout/Unknown must
/// not journal typed unknown: leftover dispatched is remapped and C5
/// cannot purge residue.
async fn delete_named_retryable(
    fabric: &crate::Fabric,
    kind: &str,
    name: &str,
    namespaced: bool,
    timeout_secs: u64,
) -> Result<(), FabricError> {
    match fabric
        .live()
        .delete_named(kind, name, namespaced, timeout_secs)
        .await
    {
        Ok(()) => Ok(()),
        Err(error) => Err(retryable_unready(
            error,
            "product object delete is not settled",
        )),
    }
}

async fn bind_application_env(
    fabric: &crate::Fabric,
    deployment_id: &str,
    database_id: Option<&str>,
    env_bindings: &[EnvBinding],
    slug: Option<&str>,
) -> Result<Option<String>, FabricError> {
    let mut pairs: Vec<(String, Vec<u8>)> = vec![
        (
            "HTTP_PROXY".into(),
            product_realize::EGRESS_LISTEN.as_bytes().to_vec(),
        ),
        (
            "HTTPS_PROXY".into(),
            product_realize::EGRESS_LISTEN.as_bytes().to_vec(),
        ),
        (
            "NO_PROXY".into(),
            b"localhost,127.0.0.1,voie-egress,.svc,.cluster.local".to_vec(),
        ),
    ];
    if let Some(database_id) = database_id {
        if database_id.is_empty() || database_id.len() > 64 {
            return Err(FabricError::Config("database identity is invalid"));
        }
        let password = fabric
            .live()
            .read_opaque_secret(
                &product_realize::postgres_secret_name(database_id),
                "postgres-password",
            )
            .await
            .map_err(|error| FabricError::Unknown(error.to_string()))?;
        let password = String::from_utf8(password)
            .map_err(|_| FabricError::Realize("database password is unusable".into()))?;
        let service = product_realize::postgres_service_name(database_id);
        // CoreDNS is not the Application data plane. Gateway reverse_proxy
        // already uses ClusterIP; DATABASE_URL must too or healthz 503s.
        let host = fabric.live().service_cluster_ip(&service).await?;
        let url = format!("postgres://app:{password}@{host}:5432/app?sslmode=disable");
        pairs.push(("DATABASE_URL".into(), url.into_bytes()));
        if let Some((_, no_proxy)) = pairs.iter_mut().find(|(name, _)| name == "NO_PROXY") {
            no_proxy.extend(format!(",{host},{service}").as_bytes());
        }
    }
    let proxy_aliases: Vec<(String, Vec<u8>)> = pairs
        .iter()
        .filter_map(|(name, value)| {
            let alias = match name.as_str() {
                "HTTP_PROXY" => Some("http_proxy"),
                "HTTPS_PROXY" => Some("https_proxy"),
                "NO_PROXY" => Some("no_proxy"),
                _ => None,
            };
            alias.map(|name| (name.to_owned(), value.clone()))
        })
        .collect();
    pairs.extend(proxy_aliases);
    for binding in env_bindings {
        if !valid_env_name(&binding.name) {
            return Err(FabricError::Config("environment binding name is invalid"));
        }
        if binding.name == "DATABASE_URL"
            || binding.name.eq_ignore_ascii_case("HTTP_PROXY")
            || binding.name.eq_ignore_ascii_case("HTTPS_PROXY")
            || binding.name.eq_ignore_ascii_case("NO_PROXY")
        {
            return Err(FabricError::Config(
                "platform environment names cannot be bound by the caller",
            ));
        }
        if binding.value.is_empty() {
            return Err(FabricError::Config("environment binding value is empty"));
        }
        pairs.push((binding.name.clone(), binding.value.as_bytes().to_vec()));
    }
    let secret = product_realize::app_env_secret_name(deployment_id);
    let mut labels: Vec<(&str, &str)> = vec![
        ("io.voie/kind", "application"),
        ("io.voie/deployment", deployment_id),
    ];
    if let Some(slug) = slug.filter(|value| !value.is_empty()) {
        labels.push(("io.voie/slug", slug));
    }
    fabric
        .live()
        .apply_opaque_secret_pairs(&secret, &pairs, &labels)
        .await
        .map_err(|error| FabricError::Unknown(error.to_string()))?;
    Ok(Some(secret))
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('A'..='Z' | 'a'..='z' | '_'))
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && name.len() <= 128
}

async fn migrate_application(
    fabric: &crate::Fabric,
    deployment_id: &str,
    body: &MutatingBody,
) -> Result<(), FabricError> {
    let argv = body
        .migrate_argv
        .as_ref()
        .ok_or(FabricError::Config("migration argv is required"))?;
    if argv.is_empty() {
        return Err(FabricError::Config("migration argv is required"));
    }
    for part in argv {
        if part.is_empty() || part.contains('\n') || part.contains('\0') {
            return Err(FabricError::Config("migration argv is invalid"));
        }
    }
    let pod = fabric
        .get_product_resource("deployment", deployment_id)
        .ok()
        .flatten()
        .and_then(|(pod, _, _)| pod)
        .unwrap_or_else(|| app_pod_name(deployment_id));
    let borrowed: Vec<&str> = argv.iter().map(String::as_str).collect();
    // App Running and postgres Ready used to wait with `?` before this
    // loop. A 90s timeout journaled unknown on the typed migrate id.
    // Guest exec and ClusterIP retries stay inside one deadline.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(150);
    let mut last_exit = None;
    loop {
        let app_running = fabric
            .live()
            .get_pod(&pod)
            .await
            .ok()
            .flatten()
            .is_some_and(|info| info.phase == "Running" && info.uid != "");
        let postgres_ready = match body.database_id.as_deref().filter(|id| !id.is_empty()) {
            None => true,
            Some(database_id) => fabric
                .live()
                .get_pod(&postgres_pod_name(database_id))
                .await
                .ok()
                .flatten()
                .is_some_and(|info| info.ready),
        };
        if app_running && postgres_ready {
            let output = fabric
                .live()
                .exec_guest(&pod, "app", &borrowed, 180_000)
                .await?;
            if output.ambiguous {
                return Err(FabricError::Unknown("migration did not settle".into()));
            }
            if output.exit_code == 0 {
                return Ok(());
            }
            last_exit = Some(output.exit_code);
        }
        if tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        }
        return Err(FabricError::Realize(match last_exit {
            Some(code) => format!("migration exited {code}"),
            None => "application or postgres was not Running in time".into(),
        }));
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn kubectl_unready(stderr: &str) -> bool {
    let text = stderr.to_ascii_lowercase();
    text.contains("not found")
        || text.contains("is not running")
        || text.contains("container not found")
        || text.contains("unable to upgrade connection")
        || text.contains("containercreating")
        || text.contains("podsandboxnotready")
}

async fn deployment_logs(fabric: &crate::Fabric, deployment_id: &str) -> Response<FabricBody> {
    let pod = fabric
        .get_product_resource("deployment", deployment_id)
        .ok()
        .flatten()
        .and_then(|(pod, _, _)| pod)
        .unwrap_or_else(|| app_pod_name(deployment_id));
    match fabric.live().pod_logs(&pod, "app", 4000).await {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(hyper::header::CONTENT_TYPE, "application/octet-stream")
            .body(full_body(Bytes::from(bytes)))
            .expect("response parts are valid"),
        Err(error) => crate::error_response(error),
    }
}

fn application_pod_yaml(
    fabric: &crate::Fabric,
    resource: &str,
    body: &MutatingBody,
    env_secret: Option<&str>,
) -> Result<String, FabricError> {
    let intent = app_intent(resource, body)?;
    let yaml = product_realize::app_pod_yaml(
        fabric.live(),
        &intent,
        &deployment_volume_name(resource),
        env_secret,
    )?;
    refuse_user_infrastructure(&yaml)?;
    if yaml.contains("postgres://") || yaml.contains("POSTGRES_PASSWORD") {
        return Err(FabricError::Realize(
            "application pod must not embed credentials".into(),
        ));
    }
    if yaml.contains(&format!(
        "io.voie/kind: \"{}\"",
        product_realize::KIND_WORKSPACE
    )) {
        return Err(FabricError::Realize(
            "application pod must not use the Workspace identity".into(),
        ));
    }
    Ok(yaml)
}

fn deployment_network_yaml(
    fabric: &crate::Fabric,
    resource: &str,
    body: &MutatingBody,
) -> Result<String, FabricError> {
    // Per-Deployment postgres policy only. Shared application/gateway
    // NetworkPolicies stay put: re-applying them with a candidate Pod
    // dropped the public Host matcher ("not found").
    let intent = app_intent(resource, body)?;
    let postgres = product_realize::application_postgres_policy_yaml(fabric.live(), &intent)?;
    refuse_user_infrastructure(&postgres)?;
    if postgres.contains("ipBlock") || postgres.contains("fromEntities") {
        return Err(FabricError::Realize(
            "application postgres policy must not carry CIDR or host entities".into(),
        ));
    }
    Ok(postgres)
}

async fn ensure_application_policy_present(fabric: &crate::Fabric) -> Result<(), FabricError> {
    let yaml = product_realize::application_network_policy_yaml(fabric.live());
    refuse_user_infrastructure(&yaml)?;
    if yaml.contains("ipBlock") || yaml.contains("io.voie/kind: \"postgres\"") {
        return Err(FabricError::Realize(
            "shared application network policy must not carry CIDR or postgres egress".into(),
        ));
    }
    match fabric
        .live()
        .get_namespaced("networkpolicy", "voie-application")
        .await
    {
        Ok(Some(_)) => Ok(()),
        Ok(None) => apply_or_unknown(fabric, &yaml).await,
        Err(error) => Err(FabricError::Unknown(error.to_string())),
    }
}

fn app_intent(resource: &str, body: &MutatingBody) -> Result<AppIntent, FabricError> {
    let slug = body
        .slug
        .as_deref()
        .ok_or(FabricError::Config("deployment slug is required"))?;
    let kind = body
        .kind
        .as_deref()
        .ok_or(FabricError::Config("deployment kind is required"))?;
    Ok(AppIntent {
        deployment_id: resource.to_owned(),
        release_id: body.release_id.map(|id| id.to_string()).unwrap_or_default(),
        slug: slug.to_owned(),
        kind: kind.to_owned(),
        port: body.port.unwrap_or(3000),
        health_path: body
            .health_path
            .clone()
            .unwrap_or_else(|| "/healthz".into()),
        run_argv: body
            .run_argv
            .clone()
            .ok_or(FabricError::Config("application run argv is required"))?,
        cpu_millis: body.cpu_millis.unwrap_or(500).clamp(100, 2000),
        memory_mb: body.memory_mb.unwrap_or(512).clamp(128, 2048),
    })
}

fn refuse_user_infrastructure(yaml: &str) -> Result<(), FabricError> {
    for needle in ["hostPath", "LoadBalancer", "evil:latest"] {
        if yaml.contains(needle) {
            return Err(FabricError::Config(
                "fabric API does not accept infrastructure objects",
            ));
        }
    }
    Ok(())
}

/// Applies the current Caddyfile and waits until voie-gateway is Ready.
/// First-app Caddy boot happens here, before `begin_product_operation`.
/// A Pending/Running Pod is kept across retryable Conflict waits so a
/// 90s timeout cannot delete a guest that is still starting.
async fn ensure_gateway_ready(fabric: &crate::Fabric) -> Result<(), FabricError> {
    let (_caddyfile, pod_name) = match apply_gateway_config(fabric).await {
        Ok(applied) => applied,
        Err(error) => return Err(retryable_unready(error, "voie-gateway is not Ready")),
    };
    let pod_yaml = product_realize::gateway_pod_yaml(fabric.live());
    refuse_user_infrastructure(&pod_yaml)?;
    match fabric.live().get_pod(&pod_name).await {
        Ok(None) => {
            if let Err(error) = apply_or_unknown(fabric, &pod_yaml).await {
                return Err(retryable_unready(error, "voie-gateway is not Ready"));
            }
        }
        // hostNetwork cannot reverse-proxy to Firecracker Application
        // ClusterIPs: Cilium treats the guest as reserved:host, and the
        // Application policy only admits io.voie/kind=gateway.
        Ok(Some(pod)) if pod.phase == "Failed" || pod.phase == "Succeeded" || pod.host_network => {
            fabric
                .live()
                .delete_named("pod", &pod_name, true, 30)
                .await
                .map_err(|error| retryable_unready(error, "voie-gateway is not Ready"))?;
            if let Err(error) = apply_or_unknown(fabric, &pod_yaml).await {
                return Err(retryable_unready(error, "voie-gateway is not Ready"));
            }
        }
        Ok(Some(_)) => {}
        Err(error) => return Err(retryable_unready(error, "voie-gateway is not Ready")),
    }
    match fabric
        .live()
        .wait_pod_ready(&pod_name, Duration::from_secs(90))
        .await
    {
        Ok(_) => fabric
            .live()
            .ensure_gateway_host_edge()
            .await
            .map_err(|error| retryable_unready(error, "voie-gateway is not Ready")),
        Err(error) => Err(retryable_unready(error, "voie-gateway is not Ready")),
    }
}

async fn apply_gateway_config(fabric: &crate::Fabric) -> Result<(String, String), FabricError> {
    let caddyfile = fabric.rendered_caddyfile()?;
    let configmap = product_realize::gateway_caddy_configmap_yaml(fabric.live(), &caddyfile)?;
    refuse_user_infrastructure(&configmap)?;
    apply_or_unknown(fabric, &configmap).await?;
    let service = product_realize::gateway_service_yaml(fabric.live());
    refuse_user_infrastructure(&service)?;
    apply_or_unknown(fabric, &service).await?;
    let policy = product_realize::gateway_network_policy_yaml(fabric.live());
    refuse_user_infrastructure(&policy)?;
    apply_or_unknown(fabric, &policy).await?;
    let host_policy = product_realize::gateway_host_policy_yaml(fabric.live());
    refuse_user_infrastructure(&host_policy)?;
    apply_or_unknown(fabric, &host_policy).await?;
    Ok((caddyfile, product_realize::gateway_pod_name()))
}

async fn realize_gateway_routes(fabric: &crate::Fabric) -> Result<(), FabricError> {
    let (caddyfile, pod_name) = apply_gateway_config(fabric).await?;
    let pod_yaml = product_realize::gateway_pod_yaml(fabric.live());
    refuse_user_infrastructure(&pod_yaml)?;
    match fabric.live().get_pod(&pod_name).await? {
        Some(pod) if pod.phase == "Running" && !pod.host_network => {
            match fabric.live().reload_gateway_caddyfile(&caddyfile).await {
                Ok(()) => {}
                Err(FabricError::Unknown(message)) => return Err(FabricError::Unknown(message)),
                Err(_) => {
                    fabric
                        .live()
                        .delete_named("pod", &pod_name, true, 30)
                        .await?;
                    apply_or_unknown(fabric, &pod_yaml).await?;
                }
            }
        }
        Some(_) => {
            fabric
                .live()
                .delete_named("pod", &pod_name, true, 30)
                .await?;
            apply_or_unknown(fabric, &pod_yaml).await?;
        }
        None => apply_or_unknown(fabric, &pod_yaml).await?,
    }
    // Cutover already waited Ready before begin. This wait covers a
    // reload that dropped tcp/80; it should be a no-op on a Ready Pod.
    fabric
        .live()
        .wait_pod_ready(&pod_name, Duration::from_secs(90))
        .await?;
    fabric.live().ensure_gateway_host_edge().await?;
    Ok(())
}

async fn delete_local_volume(fabric: &crate::Fabric, name: &str) -> Result<(), FabricError> {
    fabric.live().delete_named("pvc", name, true, 30).await?;
    fabric.live().delete_named("pv", name, false, 30).await?;
    Ok(())
}

/// Copies the immutable Release artifact onto a private RWO drive for this
/// Deployment. Preview and production cannot share one Deployment drive.
async fn materialize_deployment_volume(
    fabric: &crate::Fabric,
    deployment_id: &str,
    release_id: &str,
    slug: Option<&str>,
) -> Result<(), FabricError> {
    let live = fabric.live();
    let volume = deployment_volume_name(deployment_id);
    if live.get_namespaced("pvc", &volume).await?.is_some() {
        // Already bound. Remounting would steal the Firecracker extra drive.
        return Ok(());
    }
    let slot = fabric
        .allocate_volume(
            crate::VolumeKind::Deployment,
            deployment_id,
            live.storage().deployment_bytes,
            None,
        )
        .await?;
    let staged = fabric
        .release_root()
        .join(release_id)
        .join("artifact.tar.zst");
    if !staged.is_file() {
        return Err(FabricError::Realize(
            "release artifact has not been staged".into(),
        ));
    }
    live.mkfs_ext4_if_needed(&slot.device).await?;
    let mount = fabric
        .release_root()
        .join(release_id)
        .join(format!("dep-{}", compact_id(deployment_id)));
    let mount_s = mount.to_string_lossy().into_owned();
    let _ = live.unmount(&mount_s).await;
    live.mount_ext4(&slot.device, &mount_s).await?;
    let extracted = product_realize::extract_archive_file(&staged, Path::new(&mount_s));
    live.unmount(&mount_s).await?;
    extracted?;
    let pv = product_realize::deployment_pv_yaml(live, deployment_id, &slot.device, slug);
    let pvc = product_realize::deployment_pvc_yaml(live, deployment_id, slug);
    crate::realize::require_stable_block_path(&slot.device)?;
    refuse_user_infrastructure(&pv)?;
    refuse_user_infrastructure(&pvc)?;
    apply_or_unknown(fabric, &format!("{pv}\n---\n{pvc}")).await?;
    let _ = std::fs::remove_file(&staged);
    Ok(())
}

async fn put_release_artifact(
    fabric: &crate::Fabric,
    release_id: &str,
    request: Request<Incoming>,
) -> Response<FabricBody> {
    if request
        .headers()
        .get("x-voie-artifact-hash")
        .and_then(|value| value.to_str().ok())
        .is_none()
    {
        return crate::error_response(FabricError::Config(
            "release artifact hash header is required",
        ));
    }
    let dir = fabric.release_root().join(release_id);
    if let Err(error) = std::fs::create_dir_all(&dir) {
        return crate::error_response(FabricError::Realize(format!(
            "cannot create release staging: {error}"
        )));
    }
    let path = dir.join("artifact.tar.zst");
    let tmp = dir.join(".artifact.tar.zst.part");
    let _ = std::fs::remove_file(&tmp);
    let (hash, total) =
        match crate::put_hashed_file_capped(&tmp, request, Some(512 * 1024 * 1024)).await {
            Ok(value) => value,
            Err(error) => {
                let _ = std::fs::remove_file(&tmp);
                return crate::error_response(error);
            }
        };
    if path.exists() {
        match hash_staged_file(&path) {
            Ok((existing, _)) if existing.eq_ignore_ascii_case(&hash) => {
                let _ = std::fs::remove_file(&tmp);
            }
            Ok(_) => {
                let _ = std::fs::remove_file(&tmp);
                return crate::error_response(FabricError::Conflict(
                    "release artifact already exists with different bytes".into(),
                ));
            }
            Err(error) => {
                let _ = std::fs::remove_file(&tmp);
                return crate::error_response(error);
            }
        }
    } else if let Err(error) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return crate::error_response(FabricError::Realize(format!(
            "cannot write release artifact: {error}"
        )));
    }
    let _ = fabric.upsert_product_resource("release", release_id, None, None, Some(&hash), "ready");
    json_response(
        StatusCode::CREATED,
        json!({
            "state": "ready",
            "resourceId": release_id,
            "artifactHash": hash,
            "byteLength": total,
        })
        .to_string(),
    )
}

fn get_release_artifact(fabric: &crate::Fabric, release_id: &str) -> Response<FabricBody> {
    let path = fabric
        .release_root()
        .join(release_id)
        .join("artifact.tar.zst");
    if !path.exists() {
        return crate::error_response(FabricError::NotFound);
    }
    match hash_staged_file(&path) {
        Ok((digest, length)) => {
            match file_stream_response(&path, "x-voie-artifact-hash", &digest, length) {
                Ok(response) => response,
                Err(error) => crate::error_response(error),
            }
        }
        Err(error) => crate::error_response(error),
    }
}

async fn probe_deployment_health(
    fabric: &crate::Fabric,
    deployment_id: &str,
    request: Request<Incoming>,
) -> Response<FabricBody> {
    let body = match read_and_validate(request).await {
        Ok(body) => body,
        Err(error) => return crate::error_response(error),
    };
    let health_path = body.health_path.as_deref().unwrap_or("/healthz");
    if !health_path.starts_with('/') || health_path.contains('\n') || health_path.contains("..") {
        return crate::error_response(FabricError::Config("application health path is invalid"));
    }
    let port = body.port.unwrap_or(3000);
    let pod = fabric
        .get_product_resource("deployment", deployment_id)
        .ok()
        .flatten()
        .and_then(|(pod, _, _)| pod)
        .unwrap_or_else(|| app_pod_name(deployment_id));
    let url = format!("http://127.0.0.1:{port}{health_path}");
    let wget = format!(
        "HTTP_PROXY= HTTPS_PROXY= http_proxy= https_proxy= exec /bin/wget -q -O /dev/null {url}"
    );
    let wget_ok = match fabric
        .live()
        .exec_guest(&pod, "app", &["/bin/busybox", "sh", "-c", &wget], 15_000)
        .await
    {
        Ok(output) if !output.ambiguous && output.exit_code == 0 => true,
        // Health is observational. Unsettled kubectl exec and a missed
        // wget are "still starting", not a journaled unknown mutate.
        _ => false,
    };
    let pod_ready = fabric
        .live()
        .get_pod(&pod)
        .await
        .ok()
        .flatten()
        .is_some_and(|info| info.ready);
    if observational_healthy(wget_ok, pod_ready) {
        json_response(
            StatusCode::OK,
            json!({ "state": "healthy", "resourceId": deployment_id }).to_string(),
        )
    } else {
        json_response(
            StatusCode::CONFLICT,
            json!({ "state": "starting", "resourceId": deployment_id }).to_string(),
        )
    }
}

/// Activate cutover and delete/stop are re-applied when the typed journal
/// is already terminal. Create/migrate/pack stay at-most-once: those
/// effects are not idempotent.
fn should_realize_product_op(kind: &str, action: &str, state: &str) -> bool {
    state == "dispatched" || (state == "terminal" && replayable_product_op(kind, action))
}

fn replayable_product_op(kind: &str, action: &str) -> bool {
    matches!(
        (kind, action),
        ("deployment", "activate")
            | ("deployment", "stop")
            | ("deployment", "delete")
            | ("database", "delete")
            | ("release", "delete")
    )
}

/// Conflict after an idempotent switch or delete is still a successful
/// journal. Unknown would refuse replay and leave routes or residue.
fn replayable_journal_on_error(kind: &str, action: &str, error: &FabricError) -> &'static str {
    if replayable_product_op(kind, action) {
        if let FabricError::Conflict(_) = error {
            return "terminal";
        }
    }
    "unknown"
}

/// Guest wget and kubelet Ready are both required. Either alone is still
/// starting: Endpoints stay empty until Ready, so activate would 409.
fn observational_healthy(wget_ok: bool, pod_ready: bool) -> bool {
    wget_ok && pod_ready
}

/// Database create journals apply only. GET reports kubelet Ready so a
/// slow Firecracker initdb stays `creating` instead of typed unknown.
fn observed_database_state(pod_ready: bool) -> &'static str {
    if pod_ready {
        "ready"
    } else {
        "creating"
    }
}

async fn probe_database_ready(fabric: &crate::Fabric, database_id: &str) -> Response<FabricBody> {
    match fabric.get_product_resource("database", database_id) {
        Ok(None) => crate::error_response(FabricError::NotFound),
        Err(error) => crate::error_response(error),
        Ok(Some(_)) => {
            let ready = fabric
                .live()
                .get_pod(&live_postgres_pod(fabric, database_id))
                .await
                .ok()
                .flatten()
                .is_some_and(|info| info.ready);
            json_response(
                StatusCode::OK,
                json!({
                    "id": database_id,
                    "kind": "database",
                    "state": observed_database_state(ready),
                })
                .to_string(),
            )
        }
    }
}

/// Maps a lagging Ready wait to HTTP 409. Unknown/Realize must not open
/// the typed journal; Conflict is the retryable activate contract.
fn retryable_unready(error: FabricError, message: &str) -> FabricError {
    match error {
        FabricError::Unknown(_) | FabricError::Realize(_) => FabricError::Conflict(message.into()),
        other => other,
    }
}

async fn backup_database(
    fabric: &crate::Fabric,
    database_id: &str,
    request: Request<Incoming>,
) -> Response<FabricBody> {
    let body = match read_and_validate(request).await {
        Ok(body) => body,
        Err(error) => return crate::error_response(error),
    };
    match fabric.begin_product_operation(
        "database-backup",
        database_id,
        &body.operation_id.to_string(),
        &body.request_hash,
    ) {
        Ok(state) if state != "dispatched" => {
            if state == "unknown" {
                return crate::error_response(FabricError::Unknown(
                    "database backup outcome unknown; the intent will not be dispatched again"
                        .into(),
                ));
            }
            if state == "acked" {
                return crate::error_response(FabricError::Conflict(
                    "database backup already acked".into(),
                ));
            }
            let staged = backup_stage_path(fabric, database_id, &body.operation_id.to_string());
            return match file_backup_response(&staged) {
                Ok(response) => response,
                Err(_) => crate::error_response(FabricError::NotFound),
            };
        }
        Ok(_) => {}
        Err(error) => return crate::error_response(error),
    }
    let pod = live_postgres_pod(fabric, database_id);
    const BACKUP_TIMEOUT_MS: u64 = crate::storage::PRODUCT_VOLUME_IO_TIMEOUT_MS;
    let dump_cmd = postgres_client_command("pg_dump -U app -d app -Fc");
    let dump_argv: Vec<&str> = dump_cmd.iter().map(String::as_str).collect();
    let staged = backup_stage_path(fabric, database_id, &body.operation_id.to_string());
    if let Some(parent) = staged.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            if let Some(response) = abandon_or_error(
                fabric,
                "database-backup",
                database_id,
                &body.operation_id.to_string(),
                "unknown",
            ) {
                return response;
            }
            return crate::error_response(FabricError::Realize(format!(
                "cannot stage database backup: {error}"
            )));
        }
    }
    let dump = match fabric
        .live()
        .exec_guest_stdout_file(&pod, "postgres", &dump_argv, &staged, BACKUP_TIMEOUT_MS)
        .await
    {
        Ok(output) if !output.ambiguous && output.exit_code == 0 => output,
        Ok(output) if !output.ambiguous => {
            if let Some(response) = abandon_or_error(
                fabric,
                "database-backup",
                database_id,
                &body.operation_id.to_string(),
                "failed",
            ) {
                return response;
            }
            return crate::error_response(FabricError::Realize(format!(
                "pg_dump exited {}",
                output.exit_code
            )));
        }
        _ => {
            if let Some(response) = abandon_or_error(
                fabric,
                "database-backup",
                database_id,
                &body.operation_id.to_string(),
                "unknown",
            ) {
                return response;
            }
            return crate::error_response(FabricError::Unknown(
                "database backup did not settle".into(),
            ));
        }
    };
    let _ = dump;
    match file_backup_response(&staged) {
        Ok(response) => {
            let _ = fabric.complete_product_operation(
                "database-backup",
                database_id,
                &body.operation_id.to_string(),
                "terminal",
            );
            response
        }
        Err(error) => {
            if let Some(response) = abandon_or_error(
                fabric,
                "database-backup",
                database_id,
                &body.operation_id.to_string(),
                "unknown",
            ) {
                return response;
            }
            crate::error_response(error)
        }
    }
}

async fn ack_database_backup(
    fabric: &crate::Fabric,
    database_id: &str,
    request: Request<Incoming>,
) -> Response<FabricBody> {
    let body = match read_and_validate(request).await {
        Ok(body) => body,
        Err(error) => return crate::error_response(error),
    };
    if let Err(error) = fabric.ack_database_backup(database_id, &body.operation_id.to_string()) {
        return crate::error_response(error);
    }
    json_response(StatusCode::OK, json!({ "state": "acked" }).to_string())
}

fn backup_stage_path(
    fabric: &crate::Fabric,
    database_id: &str,
    operation_id: &str,
) -> std::path::PathBuf {
    fabric
        .stage_root()
        .join("backups")
        .join(database_id)
        .join(format!("{operation_id}.pgdump"))
}

fn restore_stage_path(fabric: &crate::Fabric, database_id: &str) -> std::path::PathBuf {
    fabric
        .stage_root()
        .join("backups")
        .join(database_id)
        .join("restore.pgdump")
}

async fn put_restore_artifact(
    fabric: &crate::Fabric,
    database_id: &str,
    request: Request<Incoming>,
) -> Response<FabricBody> {
    let state = match fabric.begin_restore_artifact("database-restore-artifact", database_id) {
        Ok(state) if state == "unknown" => {
            return crate::error_response(FabricError::Unknown(
                "database restore artifact outcome unknown; the intent will not be dispatched again"
                    .into(),
            ));
        }
        Ok(state) => state,
        Err(error) => return crate::error_response(error),
    };
    let path = restore_stage_path(fabric, database_id);
    match crate::put_hashed_file_capped(
        &path,
        request,
        Some(crate::storage::DATABASE_PROD_ELEVATED_BYTES),
    )
    .await
    {
        Ok((hash, total)) => {
            if let Err(error) =
                fabric.finish_restore_artifact("database-restore-artifact", database_id)
            {
                return crate::error_response(error);
            }
            json_response(
                StatusCode::CREATED,
                json!({
                    "state": "ready",
                    "resourceId": database_id,
                    "artifactHash": hash,
                    "byteLength": total,
                })
                .to_string(),
            )
        }
        Err(error) => {
            if state == "dispatched" {
                if let Some(response) = abandon_or_error(
                    fabric,
                    "database-restore-artifact",
                    database_id,
                    "artifact",
                    "failed",
                ) {
                    return response;
                }
            }
            crate::error_response(error)
        }
    }
}

async fn restore_database_dump(
    fabric: &crate::Fabric,
    database_id: &str,
    body: &MutatingBody,
    postgres_password: Option<&str>,
) -> Result<(), FabricError> {
    let path = restore_stage_path(fabric, database_id);
    if !path.exists() {
        return Err(FabricError::Realize(
            "restore artifact has not been staged".into(),
        ));
    }
    if let Some(expected) = body.artifact_hash.as_deref() {
        verify_file_hash(&path, expected)?;
    }
    teardown_restore_candidate(fabric, database_id).await;
    let current = fabric.get_allocation(crate::VolumeKind::Database, database_id)?;
    let prod = body.kind.as_deref() == Some("prod");
    let bytes = current
        .as_ref()
        .map(|row| row.allocated_bytes)
        .or(body.allocated_bytes)
        .unwrap_or_else(|| {
            fabric
                .live()
                .storage()
                .database_size(prod, body.elevated.unwrap_or(false))
        });
    let old_pod = live_postgres_pod(fabric, database_id);
    let old_pvc = live_postgres_volume(fabric, database_id);
    let operation = body.operation_id.to_string();
    let slot = fabric
        .allocate_volume(
            crate::VolumeKind::DatabaseRestore,
            database_id,
            bytes,
            Some(&operation),
        )
        .await?;
    let restore_result = restore_onto_candidate(
        fabric,
        database_id,
        body,
        &operation,
        &slot.device,
        bytes,
        &path,
        &old_pod,
        &old_pvc,
        postgres_password,
    )
    .await;
    if matches!(&restore_result, Err(FabricError::Realize(_))) {
        teardown_named_restore_candidate(fabric, database_id, &operation).await;
    }
    restore_result
}

async fn restore_onto_candidate(
    fabric: &crate::Fabric,
    database_id: &str,
    body: &MutatingBody,
    operation: &str,
    device: &str,
    bytes: u64,
    path: &std::path::Path,
    old_pod: &str,
    old_pvc: &str,
    postgres_password: Option<&str>,
) -> Result<(), FabricError> {
    crate::realize::require_stable_block_path(device)?;
    fabric.live().mkfs_ext4_if_needed(device).await?;
    let pv = product_realize::postgres_restore_pv_yaml(
        fabric.live(),
        database_id,
        operation,
        device,
        body.slug.as_deref(),
        bytes,
    );
    let pvc = product_realize::postgres_restore_pvc_yaml(
        fabric.live(),
        database_id,
        operation,
        body.slug.as_deref(),
        bytes,
    );
    apply_or_unknown(fabric, &format!("{pv}\n---\n{pvc}")).await?;
    let intent = DatabaseIntent {
        database_id: database_id.to_owned(),
        slug: body.slug.clone().unwrap_or_default(),
        kind: body.kind.clone().unwrap_or_else(|| "dev".into()),
    };
    if let Some(password) = postgres_password {
        let mut pg_labels: Vec<(&str, &str)> = vec![
            ("io.voie/kind", "postgres"),
            ("io.voie/database", database_id),
        ];
        if let Some(slug) = body.slug.as_deref().filter(|value| !value.is_empty()) {
            pg_labels.push(("io.voie/slug", slug));
        }
        fabric
            .live()
            .apply_opaque_secret(
                &product_realize::postgres_secret_name(database_id),
                "postgres-password",
                password.as_bytes(),
                &pg_labels,
            )
            .await
            .map_err(|error| FabricError::Unknown(error.to_string()))?;
    }
    let candidate = postgres_restore_pod_name(operation);
    let yaml = product_realize::postgres_restore_pod_yaml(
        fabric.live(),
        &intent,
        &postgres_restore_volume_name(operation),
        operation,
    );
    apply_or_unknown(fabric, &yaml).await?;
    fabric.set_product_desired_yaml("database", database_id, &yaml)?;
    fabric
        .live()
        .wait_pod_ready(&candidate, Duration::from_secs(180))
        .await?;
    let restore_cmd =
        postgres_client_command("pg_restore -U app -d app --clean --if-exists --no-owner -Fc");
    let restore_argv: Vec<&str> = restore_cmd.iter().map(String::as_str).collect();
    let output = fabric
        .live()
        .exec_guest_stdin_file(
            &candidate,
            "postgres",
            &restore_argv,
            path,
            crate::storage::PRODUCT_VOLUME_IO_TIMEOUT_MS,
        )
        .await?;
    if output.ambiguous {
        return Err(FabricError::Unknown(
            "database restore did not settle".into(),
        ));
    }
    if output.exit_code != 0 {
        return Err(FabricError::Realize(format!(
            "pg_restore exited {}",
            output.exit_code
        )));
    }
    let probe = fabric
        .live()
        .exec_guest(
            &candidate,
            "postgres",
            &["/bin/pg_isready", "-U", "app", "-h", "127.0.0.1"],
            30_000,
        )
        .await?;
    if probe.ambiguous || probe.exit_code != 0 {
        return Err(FabricError::Realize(
            "restore candidate did not answer pg_isready after pg_restore".into(),
        ));
    }
    let service = product_realize::postgres_service_yaml(fabric.live(), &intent, operation);
    refuse_user_infrastructure(&service)?;
    apply_or_unknown(fabric, &service).await?;
    fabric
        .live()
        .wait_endpoints_exactly(
            &postgres_service_name(database_id),
            &candidate,
            Duration::from_secs(60),
        )
        .await?;
    if !intent.slug.is_empty() && (intent.kind == "dev" || intent.kind == "prod") {
        let policy = product_realize::postgres_network_policy_yaml(fabric.live(), &intent)?;
        refuse_user_infrastructure(&policy)?;
        apply_or_unknown(fabric, &policy).await?;
    }
    let old_lv = fabric
        .get_allocation(crate::VolumeKind::Database, database_id)?
        .map(|row| row.lv_name);
    fabric.promote_restore_to_database(database_id).await?;
    if old_pod != candidate {
        delete_named_retryable(fabric, "pod", old_pod, true, 60).await?;
    }
    if old_pvc != postgres_restore_volume_name(operation) {
        delete_local_volume(fabric, old_pvc).await?;
    }
    if let Some(lv) = old_lv {
        let promoted = fabric.get_allocation(crate::VolumeKind::Database, database_id)?;
        if promoted.as_ref().map(|row| row.lv_name.as_str()) != Some(lv.as_str()) {
            fabric
                .live()
                .release_block(&crate::BlockSlot {
                    device: String::new(),
                    lv_name: Some(lv),
                    mapper_name: None,
                })
                .await?;
        }
    }
    fabric.upsert_product_resource(
        "database",
        database_id,
        Some(&candidate),
        Some(&postgres_service_name(database_id)),
        Some(operation),
        "ready",
    )?;
    fabric.set_product_desired_yaml("database", database_id, &yaml)?;
    fabric.ack_restore_artifact("database-restore-artifact", database_id, path)?;
    Ok(())
}

async fn teardown_restore_candidate(fabric: &crate::Fabric, database_id: &str) {
    let Some(row) = fabric
        .get_allocation(crate::VolumeKind::DatabaseRestore, database_id)
        .ok()
        .flatten()
    else {
        return;
    };
    let pod = postgres_pod_for_lv(&row.lv_name, database_id);
    let pvc = postgres_pvc_for_lv(&row.lv_name, database_id);
    let _ = delete_named_retryable(fabric, "pod", &pod, true, 60).await;
    let _ = delete_local_volume(fabric, &pvc).await;
    let _ = fabric
        .free_volume(crate::VolumeKind::DatabaseRestore, database_id)
        .await;
}

async fn teardown_named_restore_candidate(
    fabric: &crate::Fabric,
    database_id: &str,
    operation: &str,
) {
    let _ = delete_named_retryable(
        fabric,
        "pod",
        &postgres_restore_pod_name(operation),
        true,
        60,
    )
    .await;
    let _ = delete_local_volume(fabric, &postgres_restore_volume_name(operation)).await;
    let _ = fabric
        .free_volume(crate::VolumeKind::DatabaseRestore, database_id)
        .await;
}

fn live_postgres_pod(fabric: &crate::Fabric, database_id: &str) -> String {
    if let Some(pod) = fabric
        .get_product_resource("database", database_id)
        .ok()
        .flatten()
        .and_then(|(pod, _, _)| pod)
    {
        return pod;
    }
    fabric
        .get_allocation(crate::VolumeKind::Database, database_id)
        .ok()
        .flatten()
        .map(|row| postgres_pod_for_lv(&row.lv_name, database_id))
        .unwrap_or_else(|| postgres_pod_name(database_id))
}

fn live_postgres_volume(fabric: &crate::Fabric, database_id: &str) -> String {
    fabric
        .get_allocation(crate::VolumeKind::Database, database_id)
        .ok()
        .flatten()
        .map(|row| postgres_pvc_for_lv(&row.lv_name, database_id))
        .unwrap_or_else(|| postgres_volume_name(database_id))
}

fn file_backup_response(path: &std::path::Path) -> Result<Response<FabricBody>, FabricError> {
    let (digest, length) = hash_staged_file(path)?;
    if length == 0 {
        return Err(FabricError::Unknown(
            "database backup was empty after copy".into(),
        ));
    }
    file_stream_response(path, "x-voie-backup-hash", &digest, length)
}

pub(crate) fn hash_staged_file(path: &std::path::Path) -> Result<(String, u64), FabricError> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)
        .map_err(|_| FabricError::Realize("artifact is unreadable".into()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut total = 0u64;
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|_| FabricError::Realize("artifact is unreadable".into()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total = total.saturating_add(n as u64);
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok((digest, total))
}

pub(crate) fn verify_file_hash(
    path: &std::path::Path,
    expected_hex: &str,
) -> Result<(), FabricError> {
    let (digest, _) = hash_staged_file(path)?;
    if digest != expected_hex.to_ascii_lowercase() {
        return Err(FabricError::Realize(
            "artifact hash did not match the immutable digest".into(),
        ));
    }
    Ok(())
}

/// Local sockets use SCRAM. Read the guest password file inside the
/// postgres container; never put the secret on kubectl argv.
fn postgres_client_command(argv: &str) -> [String; 3] {
    [
        "sh".into(),
        "-c".into(),
        format!(
            "set -eu; PGPASSWORD=$(cat /run/voie/postgres-password); export PGPASSWORD; exec {argv}"
        ),
    ]
}

async fn read_and_validate(request: Request<Incoming>) -> Result<MutatingBody, FabricError> {
    let bytes = request
        .into_body()
        .collect()
        .await
        .map_err(|_| FabricError::Config("request body is unreadable"))?
        .to_bytes();
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| FabricError::Config("JSON is unusable"))?;
    reject_forbidden(&value)?;
    serde_json::from_value(value).map_err(|_| FabricError::Config("JSON is unusable"))
}

pub fn reject_forbidden(value: &Value) -> Result<(), FabricError> {
    if let Some(object) = value.as_object() {
        for key in object.keys() {
            if FORBIDDEN_KEYS
                .iter()
                .any(|forbidden| key.eq_ignore_ascii_case(forbidden))
            {
                return Err(FabricError::Config(
                    "fabric API does not accept infrastructure objects",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{kubectl_unready, parse_product_route};
    use crate::FabricError;
    use hyper::Method;

    #[test]
    fn kubectl_unready_detects_container_startup() {
        assert!(kubectl_unready(
            "Error from server (BadRequest): container is not running"
        ));
        assert!(!kubectl_unready("Traceback (most recent call last)"));
    }

    #[test]
    fn create_is_keyed_by_path_id_not_collection_post() {
        let deployment = "11111111-1111-1111-1111-111111111111";
        let database = "22222222-2222-2222-2222-222222222222";
        assert_eq!(
            parse_product_route(&Method::POST, &["v1", "deployments", deployment]),
            Some(("deployment", deployment, "create"))
        );
        assert_eq!(
            parse_product_route(&Method::POST, &["v1", "deployments", deployment, "health"]),
            Some(("deployment", deployment, "health"))
        );
        assert_eq!(
            parse_product_route(
                &Method::POST,
                &["v1", "deployments", deployment, "activate"]
            ),
            Some(("deployment", deployment, "activate"))
        );
        assert_eq!(
            parse_product_route(&Method::POST, &["v1", "databases", database]),
            Some(("database", database, "create"))
        );
        assert_eq!(
            parse_product_route(&Method::GET, &["v1", "databases", database]),
            Some(("database", database, "get"))
        );
        assert!(parse_product_route(&Method::POST, &["v1", "deployments"]).is_none());
        assert!(parse_product_route(&Method::POST, &["v1", "databases"]).is_none());
    }

    #[test]
    fn running_egress_pod_is_not_replaced() {
        assert!(!super::egress_pod_needs_replace("Running", false));
        assert!(!super::egress_pod_needs_replace("Pending", false));
        assert!(super::egress_pod_needs_replace("Failed", false));
        assert!(super::egress_pod_needs_replace("Succeeded", false));
        assert!(super::egress_pod_needs_replace("Running", true));
    }

    #[test]
    fn observational_health_requires_wget_and_ready() {
        assert!(super::observational_healthy(true, true));
        assert!(!super::observational_healthy(true, false));
        assert!(!super::observational_healthy(false, true));
        assert!(!super::observational_healthy(false, false));
    }

    #[test]
    fn activate_replays_terminal_cutover_other_ops_do_not() {
        assert!(super::should_realize_product_op(
            "deployment",
            "activate",
            "dispatched"
        ));
        assert!(super::should_realize_product_op(
            "deployment",
            "activate",
            "terminal"
        ));
        assert!(super::should_realize_product_op(
            "deployment",
            "stop",
            "terminal"
        ));
        assert!(super::should_realize_product_op(
            "deployment",
            "delete",
            "terminal"
        ));
        assert!(super::should_realize_product_op(
            "database", "delete", "terminal"
        ));
        assert!(!super::should_realize_product_op(
            "deployment",
            "activate",
            "unknown"
        ));
        assert!(!super::should_realize_product_op(
            "deployment",
            "create",
            "terminal"
        ));
        assert!(!super::should_realize_product_op(
            "deployment",
            "migrate",
            "terminal"
        ));
        assert!(super::should_realize_product_op(
            "deployment",
            "create",
            "dispatched"
        ));
        assert_eq!(
            super::replayable_journal_on_error(
                "deployment",
                "activate",
                &FabricError::Conflict("voie-gateway is not Ready".into()),
            ),
            "terminal"
        );
        assert_eq!(
            super::replayable_journal_on_error(
                "deployment",
                "stop",
                &FabricError::Conflict("voie-gateway is not Ready".into()),
            ),
            "terminal"
        );
        assert_eq!(
            super::replayable_journal_on_error(
                "database",
                "delete",
                &FabricError::Conflict("product object delete is not settled".into()),
            ),
            "terminal"
        );
        assert_eq!(
            super::replayable_journal_on_error(
                "deployment",
                "activate",
                &FabricError::Unknown("gateway reload did not settle".into()),
            ),
            "unknown"
        );
        assert_eq!(
            super::replayable_journal_on_error(
                "deployment",
                "create",
                &FabricError::Conflict("voie-gateway is not Ready".into()),
            ),
            "unknown"
        );
    }

    #[test]
    fn activate_unready_is_conflict_not_unknown() {
        match super::retryable_unready(
            FabricError::Unknown("pod voie-gateway did not become Ready".into()),
            "voie-gateway is not Ready",
        ) {
            FabricError::Conflict(message) => assert_eq!(message, "voie-gateway is not Ready"),
            other => panic!("expected conflict, got {other}"),
        }
        match super::retryable_unready(
            FabricError::Realize("pod voie-gateway reached Failed before Running".into()),
            "application pod is not Ready",
        ) {
            FabricError::Conflict(message) => assert_eq!(message, "application pod is not Ready"),
            other => panic!("expected conflict, got {other}"),
        }
        match super::retryable_unready(
            FabricError::Config("deployment slug is required"),
            "voie-gateway is not Ready",
        ) {
            FabricError::Config("deployment slug is required") => {}
            other => panic!("config must stay config, got {other}"),
        }
    }

    #[test]
    fn database_ready_is_observational_kubelet() {
        assert_eq!(super::observed_database_state(true), "ready");
        assert_eq!(super::observed_database_state(false), "creating");
    }

    #[test]
    fn application_database_url_uses_cluster_ip() {
        let src = include_str!("product.rs");
        assert!(
            src.contains("service_cluster_ip(&service)"),
            "DATABASE_URL must use the postgres Service ClusterIP"
        );
    }

    #[test]
    fn backup_and_restore_use_the_guest_password_file() {
        let src = include_str!("product.rs");
        assert!(
            src.contains("PGPASSWORD=$(cat /run/voie/postgres-password)"),
            "pg_dump/pg_restore must authenticate with the guest password file"
        );
        let cmd = super::postgres_client_command("pg_dump -U app -d app");
        assert_eq!(cmd[0], "sh");
        assert!(cmd[2].contains("/run/voie/postgres-password"), "{}", cmd[2]);
        assert!(
            !cmd[2].contains("secret"),
            "client wrapper must not embed a password literal"
        );
    }

    #[test]
    fn application_pod_attaches_a_per_deployment_volume() {
        let src = include_str!("product.rs");
        assert!(
            src.contains("deployment_volume_name(resource)"),
            "Application pods must attach a per-Deployment copy of the Release"
        );
        assert!(
            src.contains("materialize_deployment_volume"),
            "Deployment create must copy the Release onto a private RWO drive"
        );
    }
}

fn operation_response(state: &str, resource_id: &str, operation_id: Uuid) -> Response<FabricBody> {
    json_response(
        if state == "dispatched" {
            StatusCode::ACCEPTED
        } else {
            StatusCode::OK
        },
        json!({
            "state": state,
            "resourceId": resource_id,
            "operationId": operation_id,
        })
        .to_string(),
    )
}
