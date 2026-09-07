//! Narrow typed Application/Release/Database Fabric operations.
//! Kubernetes objects, images, host paths, and Caddy fragments are refused.

use std::path::Path;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::product_realize::{
    self, DatabaseIntent, app_pod_name, app_service_name, compact_id, deployment_volume_for_lv,
    deployment_volume_name, postgres_pod_for_lv, postgres_pod_name, postgres_pvc_for_lv,
    postgres_restore_pod_name, postgres_restore_volume_name, postgres_service_name,
    postgres_volume_name, release_volume_name,
};
use crate::{FabricBody, FabricError, full_body, json_response};

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

/// At-most-once journal body. Repeatable present/absent realization
/// uses typed PUT specs, not this struct. Activate and health are
/// observational: slug, kind, port, health path, console host, and
/// predecessor come from the stored Deployment and route specs.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalBody {
    pub operation_id: Uuid,
    pub request_hash: String,
    pub desired_revision: i64,
    /// Database identity for tenant migrate. The password is never in this
    /// body; PUT Database/Deployment specs stream one-shot secrets.
    #[serde(default)]
    pub database_id: Option<String>,
    /// Typed `voie.toml` migration argv. Runs in the Application container.
    #[serde(default)]
    pub migrate_argv: Option<Vec<String>>,
}

/// Restore candidate inputs. Slug, kind, and security profile come from the
/// stored Database spec; headers are a leftover fallback when no spec exists.
struct RestorePlan {
    operation_id: Uuid,
    desired_revision: i64,
    slug: String,
    kind: String,
    allocated_bytes: Option<u64>,
    elevated: bool,
    security_profile: u32,
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
/// migrate, backup, restore, and release-delete address the same journal
/// row the Pod uses. Activate is observational like health.
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
        (&Method::PUT, ["v1", "deployments", id]) => Some(("deployment", *id, "put-spec")),
        (&Method::GET, ["v1", "deployments", id]) => Some(("deployment", *id, "get")),
        (&Method::POST, ["v1", "deployments", id, "activate"]) => {
            Some(("deployment", *id, "activate"))
        }
        (&Method::POST, ["v1", "deployments", id, "migrate"]) => {
            Some(("deployment", *id, "migrate"))
        }
        (&Method::GET, ["v1", "deployments", id, "logs"]) => Some(("deployment", *id, "logs")),
        (&Method::POST, ["v1", "deployments", id, "health"]) => Some(("deployment", *id, "health")),
        (&Method::PUT, ["v1", "deployments", id, "artifact"]) => {
            Some(("deployment", *id, "artifact"))
        }
        (&Method::PUT, ["v1", "databases", id]) => Some(("database", *id, "put-spec")),
        (&Method::GET, ["v1", "databases", id]) => Some(("database", *id, "get")),
        (&Method::POST, ["v1", "databases", id, "backup"]) => Some(("database", *id, "backup")),
        (&Method::DELETE, ["v1", "databases", id, "backup"]) => {
            Some(("database", *id, "ack-backup"))
        }
        (&Method::POST, ["v1", "databases", id, "restore"]) => Some(("database", *id, "restore")),
        (&Method::PUT, ["v1", "databases", id, "restore-artifact"]) => {
            Some(("database", *id, "restore-artifact"))
        }
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
    if let ["v1", "releases", _, "artifact"] = parts.as_slice() {
        if method == Method::PUT || method == Method::GET {
            return crate::error_response(FabricError::Config(
                "release artifact must be streamed onto the deployment volume",
            ));
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
            return probe_database_status(fabric, resource_id).await;
        }
        if kind == "deployment" {
            return probe_deployment_status(fabric, resource_id).await;
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
    if action == "activate" {
        return activate_environment_selector(fabric, resource_id, request).await;
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
    if action == "artifact" && kind == "deployment" {
        return put_deployment_artifact(fabric, resource_id, request).await;
    }
    if action == "put-spec" {
        if kind == "database" {
            return put_database_spec(fabric, resource_id, request).await;
        }
        if kind == "deployment" {
            return put_deployment_spec(fabric, resource_id, request).await;
        }
    }
    if kind == "release" && action == "materialize" {
        return crate::error_response(FabricError::Config(
            "release artifact must be streamed onto the deployment volume",
        ));
    }
    if action == "restore" {
        return crate::error_response(FabricError::Config(
            "restore dump must be streamed onto the candidate",
        ));
    }
    match read_and_validate(request).await {
        Err(error) => crate::error_response(error),
        Ok(body) => {
            let resource = resource_id.to_owned();
            // App Running and postgres Ready must hold before the typed
            // migrate journal. Init/Pending is retryable Conflict; opening
            // the journal would turn a later timeout into unknown.
            if kind == "deployment" && action == "migrate" {
                if let Err(error) = ensure_migrate_ready(fabric, &resource, &body).await {
                    return crate::error_response(error);
                }
            }
            let operation_id = body.operation_id.to_string();
            let started = match fabric.begin_product_operation(
                kind,
                &resource,
                &operation_id,
                &body.request_hash,
            ) {
                Ok(state) => match redispatch_migrate_if_failed(
                    fabric,
                    kind,
                    action,
                    &resource,
                    &operation_id,
                    state,
                ) {
                    Ok(state) => state,
                    Err(error) => return crate::error_response(error),
                },
                Err(error) => return crate::error_response(error),
            };
            if should_realize_product_op(kind, action, &started) {
                let replay = started == "terminal";
                match realize_desired(fabric, kind, action, &resource, &body).await {
                    Ok(()) => {
                        if !replay {
                            let _ = fabric.complete_product_operation(
                                kind,
                                &resource,
                                &operation_id,
                                "terminal",
                            );
                        }
                        operation_response(&started, &resource, body.operation_id)
                    }
                    Err(error) => {
                        if !replay {
                            let _ = fabric.complete_product_operation(
                                kind,
                                &resource,
                                &operation_id,
                                replayable_journal_on_error(kind, action, &error),
                            );
                        }
                        crate::error_response(error)
                    }
                }
            } else {
                operation_response(&started, &resource, body.operation_id)
            }
        }
    }
}

async fn realize_desired(
    fabric: &crate::Fabric,
    kind: &str,
    action: &str,
    resource: &str,
    body: &JournalBody,
) -> Result<(), FabricError> {
    match (kind, action) {
        ("release", "materialize") => Err(FabricError::Config(
            "release artifact must be streamed onto the deployment volume",
        )),
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
        ("deployment", "activate") => Err(FabricError::Config(
            "environment selector switch is observational, not a journal",
        )),
        ("deployment", "migrate") => {
            migrate_application(fabric, resource, body).await?;
            Ok(())
        }
        ("database", "restore") => Err(FabricError::Config(
            "restore dump must be streamed onto the candidate",
        )),
        _ => Err(FabricError::Config("unsupported product operation")),
    }
}

/// Repeatable Environment Service switch. kubectl apply is idempotent;
/// ambiguous apply is Conflict so Control retries without a no-replay journal.
async fn switch_environment_selector(
    fabric: &crate::Fabric,
    resource: &str,
) -> Result<(), FabricError> {
    let spec = load_deployment_spec(fabric, resource)?;
    let slug = spec.slug.as_str();
    let env_kind = spec.kind.as_str();
    let port = spec.port;
    let host = console_host_from_specs(fabric)?;
    let switched =
        product_realize::app_service_selector_yaml(fabric.live(), slug, env_kind, resource, port)?;
    refuse_user_infrastructure(&switched)?;
    apply_or_unknown(fabric, &switched)
        .await
        .map_err(|error| retryable_unready(error, "environment selector is not applied"))?;
    let service_name = app_service_name(slug, env_kind);
    fabric.upsert_product_resource(
        "deployment",
        resource,
        Some(&app_pod_name(resource)),
        Some(&service_name),
        None,
        "active",
    )?;
    if let Some(previous) = spec.previous_deployment_id {
        let previous = previous.to_string();
        if previous != resource {
            if let Ok(Some((pod, service, _))) =
                fabric.get_product_resource("deployment", &previous)
            {
                fabric.upsert_product_resource(
                    "deployment",
                    &previous,
                    pod.as_deref(),
                    service.as_deref(),
                    None,
                    "superseded",
                )?;
            }
        }
    }
    fabric.upsert_gateway_route(slug, env_kind, &format!("{service_name}:{port}"), &host)?;
    let next = fabric
        .store
        .get_resource_spec("routes", "fabric")
        .ok()
        .flatten()
        .map(|row| row.desired_revision.max(row.observed_revision) + 1)
        .unwrap_or(1);
    let map = crate::specs::routes::RouteMapSpec {
        revision: next,
        console_host: host.to_owned(),
        routes: fabric
            .list_gateway_routes()?
            .into_iter()
            .map(|item| crate::specs::routes::RouteEntry {
                slug: item.slug,
                kind: item.kind,
                service: item.service,
            })
            .collect(),
    };
    let typed = serde_json::to_string(&map)
        .map_err(|_| FabricError::Store("cannot encode route map".into()))?;
    fabric
        .store
        .upsert_resource_spec("routes", "fabric", next, &map.hash_bytes(), &typed)?;
    // Selector is switched. A gateway reload timeout is retryable
    // Conflict; the next activate reloads without a no-replay journal.
    if let Err(error) = realize_gateway_routes(fabric).await {
        return Err(retryable_unready(error, "voie-gateway is not Ready"));
    }
    let _ = fabric
        .store
        .set_resource_spec_observed("routes", "fabric", next, "ready", None);
    Ok(())
}

/// Retire the Environment Service and Fabric gateway edge. Traffic desired
/// `None` owns this; Deployment stop must not guess the shared selector.
async fn clear_environment_edge(
    fabric: &crate::Fabric,
    slug: &str,
    kind: &str,
) -> Result<(), FabricError> {
    let name = app_service_name(slug, kind);
    if fabric.live().get_namespaced("svc", &name).await?.is_some() {
        delete_named_retryable(fabric, "svc", &name, true, 30).await?;
    }
    fabric.delete_gateway_route(slug, kind)?;
    if let Err(error) = realize_gateway_routes(fabric).await {
        return Err(retryable_unready(error, "voie-gateway is not Ready"));
    }
    Ok(())
}

/// Running platform egress must not be `kubectl apply`'d again. Resource
/// requests on the generated Pod are immutable once the live Pod exists;
/// a multi-doc apply that included the candidate Application then failed
/// the egress update and journaled typed unknown after the app Pod existed.
fn egress_pod_needs_replace(phase: &str, host_network: bool, dns_policy: &str) -> bool {
    host_network || phase == "Failed" || phase == "Succeeded" || dns_policy != "Default"
}

pub(crate) async fn ensure_egress_present(fabric: &crate::Fabric) -> Result<(), FabricError> {
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
        Ok(Some(pod))
            if egress_pod_needs_replace(&pod.phase, pod.host_network, &pod.dns_policy) =>
        {
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

pub(crate) async fn apply_or_unknown(
    fabric: &crate::Fabric,
    yaml: &str,
) -> Result<(), FabricError> {
    match fabric.live().apply_yaml(yaml).await {
        Ok(()) => Ok(()),
        Err(FabricError::Unknown(message)) => Err(FabricError::Unknown(message)),
        Err(FabricError::Config(message)) => Err(FabricError::Config(message)),
        Err(FabricError::Foreign(message)) => Err(FabricError::Foreign(message)),
        Err(FabricError::Realize(message)) => Err(FabricError::Realize(message)),
        Err(error) => Err(FabricError::Unknown(error.to_string())),
    }
}

/// kubectl delete --ignore-not-found is idempotent. Timeout/Unknown must
/// not journal typed unknown: leftover dispatched is remapped and C5
/// cannot purge residue.
pub(crate) async fn delete_named_retryable(
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
            .map_err(|error| match error {
                FabricError::Realize(message) => FabricError::Realize(message),
                FabricError::Config(message) => FabricError::Config(message),
                other => FabricError::Unknown(other.to_string()),
            })?;
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
    body: &JournalBody,
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
            .get_pod(&live_postgres_pod(fabric, database_id))
            .await
            .ok()
            .flatten()
            .is_some_and(|info| info.ready),
    };
    if !app_running || !postgres_ready {
        return Err(FabricError::Conflict(
            "application or postgres is not Ready".into(),
        ));
    }
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
    Err(FabricError::Realize(format!(
        "migration exited {}",
        output.exit_code
    )))
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

pub(crate) async fn ensure_application_policy_present(
    fabric: &crate::Fabric,
) -> Result<(), FabricError> {
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

pub(crate) fn refuse_user_infrastructure(yaml: &str) -> Result<(), FabricError> {
    for needle in ["hostPath", "LoadBalancer", "evil:latest"] {
        if yaml.contains(needle) {
            return Err(FabricError::Config(
                "fabric API does not accept infrastructure objects",
            ));
        }
    }
    Ok(())
}

fn load_deployment_spec(
    fabric: &crate::Fabric,
    deployment_id: &str,
) -> Result<crate::specs::deployment::DeploymentSpec, FabricError> {
    let row = fabric
        .store
        .get_resource_spec("deployment", deployment_id)?
        .ok_or(FabricError::Config("deployment spec is required"))?;
    let spec: crate::specs::deployment::DeploymentSpec = serde_json::from_str(&row.typed_spec)
        .map_err(|_| FabricError::Config("deployment spec is unusable"))?;
    spec.validate().map_err(FabricError::Config)?;
    Ok(spec)
}

fn load_database_spec(
    fabric: &crate::Fabric,
    database_id: &str,
) -> Result<crate::specs::database::DatabaseSpec, FabricError> {
    let row = fabric
        .store
        .get_resource_spec("database", database_id)?
        .ok_or(FabricError::Config("database spec is required"))?;
    let spec: crate::specs::database::DatabaseSpec = serde_json::from_str(&row.typed_spec)
        .map_err(|_| FabricError::Config("database spec is unusable"))?;
    spec.validate().map_err(FabricError::Config)?;
    Ok(spec)
}

fn console_host_from_specs(fabric: &crate::Fabric) -> Result<String, FabricError> {
    if let Some(row) = fabric.store.get_resource_spec("routes", "fabric")? {
        if let Ok(map) = serde_json::from_str::<crate::specs::routes::RouteMapSpec>(&row.typed_spec)
        {
            if !map.console_host.is_empty() && !map.console_host.contains('\n') {
                return Ok(map.console_host);
            }
        }
    }
    fabric
        .store
        .gateway_console_host()?
        .filter(|host| !host.is_empty() && !host.contains('\n'))
        .ok_or(FabricError::Config("console host is required"))
}

fn restore_plan(
    fabric: &crate::Fabric,
    database_id: &str,
    operation_id: Uuid,
    desired_revision: i64,
    header_slug: Option<String>,
    header_kind: Option<String>,
    allocated_bytes: Option<u64>,
    header_security_profile: Option<u32>,
) -> RestorePlan {
    let spec = load_database_spec(fabric, database_id).ok();
    RestorePlan {
        operation_id,
        desired_revision,
        slug: spec
            .as_ref()
            .map(|item| item.slug.clone())
            .or(header_slug)
            .unwrap_or_default(),
        kind: spec
            .as_ref()
            .map(|item| item.kind.clone())
            .or(header_kind)
            .unwrap_or_else(|| "dev".into()),
        allocated_bytes,
        elevated: spec
            .as_ref()
            .is_some_and(|item| item.storage_tier == "elevated"),
        security_profile: spec
            .as_ref()
            .map(|item| item.security_profile)
            .or(header_security_profile)
            .unwrap_or(1),
    }
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
    let caddyfile = fabric.dataplane_caddyfile().await?;
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

pub(crate) async fn realize_gateway_routes(fabric: &crate::Fabric) -> Result<(), FabricError> {
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

fn gateway_pod_is_realized(phase: &str, ready: bool, host_network: bool) -> bool {
    phase == "Running" && ready && !host_network
}

async fn gateway_realization_present(fabric: &crate::Fabric) -> bool {
    match fabric
        .live()
        .get_pod(&product_realize::gateway_pod_name())
        .await
    {
        Ok(Some(pod)) if gateway_pod_is_realized(&pod.phase, pod.ready, pod.host_network) => {}
        _ => return false,
    }
    for (kind, name) in [
        ("svc", "voie-gateway"),
        ("configmap", "voie-gateway-caddy"),
        ("networkpolicy", "voie-gateway"),
        ("networkpolicy", "voie-gateway-host"),
    ] {
        match fabric.live().get_namespaced(kind, name).await {
            Ok(Some(_)) => {}
            _ => return false,
        }
    }
    true
}

/// Steady-state heal: live gateway objects and traffic selectors can vanish
/// while fabricd stays up. Startup already runs these once. Route and
/// traffic heals are independent so a gateway apply failure does not skip
/// selector retirement.
pub async fn reconcile_runtime_edge(fabric: &crate::Fabric) -> Result<(), FabricError> {
    let routes = reconcile_accepted_routes(fabric).await;
    let traffic = reconcile_accepted_traffic(fabric).await;
    routes.and(traffic)
}

/// Workspace / Database / Deployment typed specs can return WaitPod from a
/// PUT. The 15s loop must keep reconciling them or GET stays `accepted`
/// while the live Pod is already Ready. Each kind is independent so a
/// Workspace miss does not skip Database or Deployment.
pub async fn reconcile_accepted_specs(fabric: &crate::Fabric) -> Result<(), FabricError> {
    let workspaces = crate::reconcile::workspace_run::reconcile_accepted_workspaces(fabric).await;
    let databases = crate::reconcile::database_run::reconcile_accepted_databases(fabric).await;
    let deployments =
        crate::reconcile::deployment_run::reconcile_accepted_deployments(fabric).await;
    workspaces.and(databases).and(deployments)
}

pub(crate) async fn reconcile_accepted_routes(fabric: &crate::Fabric) -> Result<(), FabricError> {
    let routes = fabric.list_gateway_routes()?;
    let spec = fabric.store.get_resource_spec("routes", "fabric")?;
    let desired = spec.as_ref().map(|row| row.desired_revision).unwrap_or(0);
    let observed = spec.as_ref().map(|row| row.observed_revision).unwrap_or(0);
    if routes.is_empty() && desired == 0 {
        return Ok(());
    }
    let desired = desired.max(1);
    let live_present = gateway_realization_present(fabric).await;
    if crate::reconcile::routes::plan_routes(desired, observed, live_present)
        == crate::reconcile::routes::RouteAction::Converged
        && spec.is_some()
    {
        return Ok(());
    }
    realize_gateway_routes(fabric).await?;
    let typed = spec
        .as_ref()
        .map(|row| row.typed_spec.clone())
        .unwrap_or_else(|| "{}".into());
    let hash = spec
        .as_ref()
        .map(|row| row.spec_hash.clone())
        .unwrap_or_else(|| "boot".into());
    fabric
        .store
        .upsert_resource_spec("routes", "fabric", desired, &hash, &typed)?;
    fabric
        .store
        .set_resource_spec_observed("routes", "fabric", desired, "ready", None)?;
    Ok(())
}

pub(crate) async fn delete_local_volume(
    fabric: &crate::Fabric,
    name: &str,
) -> Result<(), FabricError> {
    fabric.live().delete_named("pvc", name, true, 30).await?;
    fabric.live().delete_named("pv", name, false, 30).await?;
    Ok(())
}

pub(crate) async fn delete_deployment_volumes(
    fabric: &crate::Fabric,
    deployment_id: &str,
) -> Result<(), FabricError> {
    let lv_name = fabric
        .get_allocation(crate::VolumeKind::Deployment, deployment_id)
        .ok()
        .flatten()
        .map(|row| row.lv_name);
    for name in crate::product_realize::deployment_volume_aliases(lv_name.as_deref(), deployment_id)
    {
        delete_local_volume(fabric, &name).await?;
    }
    Ok(())
}

fn spec_holds_volume(typed_spec: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(typed_spec) else {
        return false;
    };
    matches!(
        value.get("desired").and_then(|item| item.as_str()),
        Some("present" | "running" | "active" | "ready")
    )
}

/// Control may already have settled `absent` and purged the Fabric spec while
/// compact PVC/LV aliases remained. Reap those leftovers on startup.
pub(crate) async fn reap_unowned_product_volumes(
    fabric: &crate::Fabric,
    report: &mut crate::StartupReport,
) {
    let Ok(allocations) = fabric.store.list_allocations() else {
        return;
    };
    for allocation in allocations {
        let spec_kind = match allocation.kind {
            crate::VolumeKind::Database | crate::VolumeKind::DatabaseRestore => "database",
            crate::VolumeKind::Deployment => "deployment",
            crate::VolumeKind::Workspace | crate::VolumeKind::WorkspaceRestore => continue,
        };
        let Some(spec) = fabric
            .store
            .get_resource_spec(spec_kind, &allocation.resource_id)
            .ok()
            .flatten()
        else {
            // No spec: keep-list or Lost. Never lvremove a live volume.
            continue;
        };
        if spec_holds_volume(&spec.typed_spec) {
            continue;
        }
        match allocation.kind {
            crate::VolumeKind::Deployment => {
                let _ = delete_deployment_volumes(fabric, &allocation.resource_id).await;
                let _ = fabric.purge_product_resource("deployment", &allocation.resource_id);
            }
            crate::VolumeKind::Database | crate::VolumeKind::DatabaseRestore => {
                let hyphen = crate::product_realize::postgres_volume_name(&allocation.resource_id);
                let compact = format!(
                    "voie-pgdata-{}",
                    crate::product_realize::compact_id(&allocation.resource_id)
                );
                let from_lv = crate::product_realize::postgres_pvc_for_lv(
                    &allocation.lv_name,
                    &allocation.resource_id,
                );
                for name in [hyphen, compact, from_lv] {
                    let _ = delete_local_volume(fabric, &name).await;
                }
            }
            crate::VolumeKind::Workspace | crate::VolumeKind::WorkspaceRestore => continue,
        }
        match fabric
            .free_volume(allocation.kind, &allocation.resource_id)
            .await
        {
            Ok(()) => {
                eprintln!(
                    "voie-fabricd: reaped leftover {} allocation {}",
                    allocation.kind.as_str(),
                    allocation.resource_id
                );
                report
                    .orphan_allocations_released
                    .push(allocation.resource_id);
            }
            Err(error) => eprintln!(
                "voie-fabricd: leftover {} allocation {} stays: {error}",
                allocation.kind.as_str(),
                allocation.resource_id
            ),
        }
    }
}

/// Copies the immutable Release artifact onto a private RWO drive for this
/// Deployment. Preview and production cannot share one Deployment drive.
/// The archive is hashed while it is unpacked; a mismatch discards the LV.
const RELEASE_PACK_MAX_BYTES: u64 = 512 * 1024 * 1024;

enum DeploymentArchive {
    Body {
        body: Incoming,
        expected_hash: String,
    },
}

fn deployment_lv_path(fabric: &crate::Fabric, deployment_id: &str) -> String {
    let lv_name = fabric
        .get_allocation(crate::VolumeKind::Deployment, deployment_id)
        .ok()
        .flatten()
        .map(|row| row.lv_name)
        .unwrap_or_else(|| crate::lv_name_for_deployment(deployment_id));
    format!("/dev/{}/{}", fabric.live().vg_name(), lv_name)
}

fn realize_step(step: &str, error: FabricError) -> FabricError {
    match error {
        FabricError::Realize(message) => FabricError::Realize(format!("{step}: {message}")),
        FabricError::Conflict(message) => FabricError::Conflict(format!("{step}: {message}")),
        FabricError::Unknown(message) => FabricError::Unknown(format!("{step}: {message}")),
        FabricError::Foreign(message) => FabricError::Foreign(format!("{step}: {message}")),
        FabricError::Store(message) => FabricError::Store(format!("{step}: {message}")),
        other => other,
    }
}

async fn materialize_deployment_volume(
    fabric: &crate::Fabric,
    deployment_id: &str,
    release_id: &str,
    slug: Option<&str>,
    archive: Option<DeploymentArchive>,
) -> Result<(), FabricError> {
    let live = fabric.live();
    let volume = fabric
        .get_allocation(crate::VolumeKind::Deployment, deployment_id)?
        .map(|row| deployment_volume_for_lv(&row.lv_name, deployment_id))
        .unwrap_or_else(|| deployment_volume_name(deployment_id));
    let lv_present = Path::new(&deployment_lv_path(fabric, deployment_id)).exists();
    if live
        .get_namespaced("pvc", &volume)
        .await
        .map_err(|error| realize_step("pvc lookup", error))?
        .is_some()
    {
        if lv_present {
            // Already bound. Remounting would steal the Firecracker extra drive.
            return Ok(());
        }
        // PVC without the LV is leftover Kubernetes residue after durable
        // bytes disappeared. Drop it so the Release can be streamed again.
        delete_named_retryable(fabric, "pvc", &volume, true, 30)
            .await
            .map_err(|error| realize_step("pvc residue", error))?;
        delete_named_retryable(fabric, "pv", &volume, false, 30)
            .await
            .map_err(|error| realize_step("pv residue", error))?;
    }
    let archive = archive.ok_or(FabricError::Realize(
        "release artifact must be streamed onto the deployment volume".into(),
    ))?;
    let DeploymentArchive::Body {
        body,
        expected_hash,
    } = archive;
    let tmp = std::env::temp_dir().join(format!("voie-dep-{deployment_id}.tar.zst"));
    if let Err(error) =
        product_realize::recv_incoming_file(body, &tmp, &expected_hash, RELEASE_PACK_MAX_BYTES)
            .await
    {
        let _ = std::fs::remove_file(&tmp);
        return Err(realize_step("recv", error));
    }
    let slot = fabric
        .allocate_volume(
            crate::VolumeKind::Deployment,
            deployment_id,
            live.storage().deployment_bytes,
            None,
        )
        .await
        .map_err(|error| realize_step("allocate", error))?;
    if let Err(error) = live.mkfs_ext4_if_needed(&slot.device).await {
        let _ = fabric
            .free_volume(crate::VolumeKind::Deployment, deployment_id)
            .await;
        return Err(realize_step("mkfs", error));
    }
    let mount = fabric
        .release_root()
        .join(release_id)
        .join(format!("dep-{}", compact_id(deployment_id)));
    let mount_s = mount.to_string_lossy().into_owned();
    let _ = live.unmount(&mount_s).await;
    if let Err(error) = live.mount_ext4(&slot.device, &mount_s).await {
        let _ = fabric
            .free_volume(crate::VolumeKind::Deployment, deployment_id)
            .await;
        return Err(realize_step("mount", error));
    }
    let tmp_path = tmp.clone();
    let mount_path = Path::new(&mount_s).to_path_buf();
    // recv_incoming_file already checked the compressed digest. Re-hashing
    // through the zstd decoder can miss the frame trailer and refuse a valid pack.
    let extracted = tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&tmp_path).map_err(|error| {
            FabricError::Realize(format!("buffered release artifact is unreadable: {error}"))
        })?;
        product_realize::extract_archive_hashed(file, &mount_path, None, RELEASE_PACK_MAX_BYTES)
            .map(|_| ())
    })
    .await
    .map_err(|error| FabricError::Unknown(error.to_string()))
    .and_then(|result| result);
    let _ = std::fs::remove_file(&tmp);
    if let Err(error) = extracted {
        let _ = live.unmount(&mount_s).await;
        let _ = fabric
            .free_volume(crate::VolumeKind::Deployment, deployment_id)
            .await;
        return Err(realize_step("extract", error));
    }
    if let Err(error) = live.unmount(&mount_s).await {
        let _ = fabric
            .free_volume(crate::VolumeKind::Deployment, deployment_id)
            .await;
        return Err(realize_step("unmount", error));
    }
    let pv = product_realize::deployment_pv_yaml(live, deployment_id, &slot.device, slug);
    let pvc = product_realize::deployment_pvc_yaml(live, deployment_id, slug);
    crate::realize::require_stable_block_path(&slot.device)
        .map_err(|error| realize_step("block path", error))?;
    refuse_user_infrastructure(&pv).map_err(|error| realize_step("pv yaml", error))?;
    refuse_user_infrastructure(&pvc).map_err(|error| realize_step("pvc yaml", error))?;
    if let Err(error) = apply_or_unknown(fabric, &format!("{pv}\n---\n{pvc}")).await {
        let _ = fabric
            .free_volume(crate::VolumeKind::Deployment, deployment_id)
            .await;
        return Err(realize_step("apply pv", error));
    }
    Ok(())
}

async fn put_deployment_artifact(
    fabric: &crate::Fabric,
    deployment_id: &str,
    request: Request<Incoming>,
) -> Response<FabricBody> {
    let expected = match request_header(&request, "x-voie-artifact-hash") {
        Some(value) => value.to_ascii_lowercase(),
        None => {
            return crate::error_response(FabricError::Config(
                "release artifact hash header is required",
            ));
        }
    };
    let operation_id = match request_header(&request, "x-voie-operation-id")
        .and_then(|value| Uuid::parse_str(value).ok())
    {
        Some(value) => value.to_string(),
        None => {
            return crate::error_response(FabricError::Config(
                "release operation id header is required",
            ));
        }
    };
    let request_hash = request_header(&request, "x-voie-request-hash")
        .unwrap_or(expected.as_str())
        .to_owned();
    let release_id = match request_header(&request, "x-voie-release-id") {
        Some(value) => value.to_owned(),
        None => {
            return crate::error_response(FabricError::Config("release id header is required"));
        }
    };
    let slug = request_header(&request, "x-voie-slug").map(ToOwned::to_owned);
    let state = match fabric.begin_product_operation(
        "deployment-artifact",
        deployment_id,
        &operation_id,
        &request_hash,
    ) {
        Ok(state) if state == "failed" => {
            match fabric.redispatch_failed_product_operation(
                "deployment-artifact",
                deployment_id,
                &operation_id,
            ) {
                Ok(true) => "dispatched".to_owned(),
                Ok(false) => state,
                Err(error) => return crate::error_response(error),
            }
        }
        Ok(state) => state,
        Err(error) => return crate::error_response(error),
    };
    let _lock = fabric
        .lifecycle_guard(&format!("deployment:{deployment_id}"))
        .await;
    let lv_present = Path::new(&deployment_lv_path(fabric, deployment_id)).exists();
    if lv_present {
        if state == "unknown" {
            return crate::error_response(FabricError::Unknown(
                "deployment artifact outcome unknown; the intent will not be dispatched again"
                    .into(),
            ));
        }
        if state != "dispatched" {
            return json_response(
                StatusCode::OK,
                json!({
                    "state": "ready",
                    "resourceId": deployment_id,
                })
                .to_string(),
            );
        }
    }
    // Missing LV is desired-state rematerialization from the immutable
    // Release, including after a previous terminal or unknown journal row.
    let archive = DeploymentArchive::Body {
        body: request.into_body(),
        expected_hash: expected.clone(),
    };
    match materialize_deployment_volume(
        fabric,
        deployment_id,
        &release_id,
        slug.as_deref(),
        Some(archive),
    )
    .await
    {
        Ok(()) => {
            let _ = fabric.complete_product_operation(
                "deployment-artifact",
                deployment_id,
                &operation_id,
                "terminal",
            );
            json_response(
                StatusCode::CREATED,
                json!({
                    "state": "ready",
                    "resourceId": deployment_id,
                    "artifactHash": expected,
                })
                .to_string(),
            )
        }
        Err(error) => {
            eprintln!("voie-fabricd: deployment {deployment_id} artifact: {error}");
            let journal = if matches!(error, FabricError::Unknown(_)) {
                "unknown"
            } else {
                "failed"
            };
            let _ = fabric.complete_product_operation(
                "deployment-artifact",
                deployment_id,
                &operation_id,
                journal,
            );
            crate::error_response(error)
        }
    }
}

async fn probe_deployment_health(
    fabric: &crate::Fabric,
    deployment_id: &str,
    request: Request<Incoming>,
) -> Response<FabricBody> {
    if let Err(error) = drain_observational(request).await {
        return crate::error_response(error);
    }
    let spec = match load_deployment_spec(fabric, deployment_id) {
        Ok(spec) => spec,
        Err(FabricError::Config(_)) => {
            return crate::error_response(FabricError::Conflict("application is not Ready".into()));
        }
        Err(error) => return crate::error_response(error),
    };
    let health_path = spec.health_path.as_str();
    if spec.port == 0
        || !health_path.starts_with('/')
        || health_path.contains('\n')
        || health_path.contains("..")
    {
        return crate::error_response(FabricError::Conflict("application is not Ready".into()));
    }
    let port = spec.port;
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
    let pod_info = fabric.live().get_pod(&pod).await.ok().flatten();
    let pod_ready = pod_info.as_ref().is_some_and(|info| info.ready);
    // Environment ClusterIP exists only after traffic cutover. Proven
    // reaches the candidate Pod IP from the gateway netns so a localhost
    // bind fails here instead of 502 after activate.
    let edge_ok = match candidate_edge_url(
        pod_info.as_ref().and_then(|info| info.pod_ip.as_deref()),
        port,
        health_path,
    ) {
        Some(url) => fabric.live().probe_http_via_gateway(&url).await,
        None => false,
    };
    if observational_healthy(wget_ok, pod_ready, edge_ok) {
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

/// Leftover POST. Traffic intent is PUT `/v1/traffic/{environmentId}`.
/// This path only reports whether a stored spec already names the
/// Deployment; it must not realize or switch the selector.
async fn activate_environment_selector(
    fabric: &crate::Fabric,
    deployment_id: &str,
    request: Request<Incoming>,
) -> Response<FabricBody> {
    if let Err(error) = drain_observational(request).await {
        return crate::error_response(error);
    }
    let Ok(rows) = fabric.store.list_resource_specs("traffic") else {
        return crate::error_response(FabricError::Store("traffic specs unreadable".into()));
    };
    let Ok(wanted) = Uuid::parse_str(deployment_id) else {
        return crate::error_response(FabricError::Config("deployment id is unusable"));
    };
    let matched = rows.into_iter().find_map(|row| {
        let spec: crate::specs::traffic::TrafficSpec =
            serde_json::from_str(&row.typed_spec).ok()?;
        (spec.desired_deployment_id == Some(wanted)).then_some((row.resource_id, spec))
    });
    let Some((environment_id, spec)) = matched else {
        return crate::error_response(FabricError::Config(
            "traffic target is PUT /v1/traffic/{environmentId}",
        ));
    };
    match live_selector_deployment(fabric, &spec).await {
        Ok(observed) if spec.matches_observed(observed) => json_response(
            StatusCode::OK,
            traffic_wire(&environment_id, &spec, observed).to_string(),
        ),
        Ok(_) => crate::error_response(FabricError::Conflict(
            "environment selector is not on the desired Deployment".into(),
        )),
        Err(error) => crate::error_response(error),
    }
}

/// Release delete stays at-most-once. Repeatable present/absent is PUT spec.
/// Activate is observational like health.
fn should_realize_product_op(kind: &str, action: &str, state: &str) -> bool {
    state == "dispatched" || (state == "terminal" && replayable_product_op(kind, action))
}

fn replayable_product_op(kind: &str, action: &str) -> bool {
    matches!((kind, action), ("release", "delete"))
}

/// Conflict after an idempotent delete is still a successful journal.
/// Unknown would refuse replay and leave residue. Activate is not journaled.
/// Migrate Conflict is "not ready yet": journal failed so the same
/// operation id can redispatch after the Pod is Running.
fn replayable_journal_on_error(kind: &str, action: &str, error: &FabricError) -> &'static str {
    if replayable_product_op(kind, action) {
        if let FabricError::Conflict(_) = error {
            return "terminal";
        }
    }
    if kind == "deployment" && action == "migrate" {
        if let FabricError::Conflict(_) = error {
            return "failed";
        }
    }
    "unknown"
}

fn redispatch_migrate_if_failed(
    fabric: &crate::Fabric,
    kind: &str,
    action: &str,
    resource: &str,
    operation_id: &str,
    state: String,
) -> Result<String, FabricError> {
    if state == "failed" && kind == "deployment" && action == "migrate" {
        if fabric.redispatch_failed_product_operation(kind, resource, operation_id)? {
            return Ok("dispatched".into());
        }
    }
    Ok(state)
}

/// Guest wget, kubelet Ready, and gateway-netns GET to the Pod IP.
/// Localhost-only bind passes in-guest wget and then fails the edge GET.
/// The Environment Service is created at traffic cutover, so proven must
/// not wait for a ClusterIP.
fn observational_healthy(wget_ok: bool, pod_ready: bool, edge_ok: bool) -> bool {
    wget_ok && pod_ready && edge_ok
}

fn candidate_edge_url(pod_ip: Option<&str>, port: u16, health_path: &str) -> Option<String> {
    let ip = crate::realize::cluster_ipv4(pod_ip?)?;
    Some(format!("http://{ip}:{port}{health_path}"))
}

/// Database GET reports kubelet Ready so a slow Firecracker initdb stays
/// `creating` instead of typed unknown.
fn observed_database_state(pod_ready: bool) -> &'static str {
    if pod_ready { "ready" } else { "creating" }
}

async fn probe_database_status(fabric: &crate::Fabric, database_id: &str) -> Response<FabricBody> {
    match crate::reconcile::database_run::reconcile_database(fabric, database_id, None).await {
        Ok(status) => json_response(
            StatusCode::OK,
            json!({
                "id": database_id,
                "kind": "database",
                "state": status.observed_state,
                "desiredRevision": status.desired_revision,
                "observedRevision": status.observed_revision,
                "desiredState": status.desired_state,
                "observedState": status.observed_state,
                "lastErrorCode": status.last_error,
            })
            .to_string(),
        ),
        Err(FabricError::NotFound) => crate::error_response(FabricError::NotFound),
        Err(error) => crate::error_response(error),
    }
}

async fn probe_deployment_status(
    fabric: &crate::Fabric,
    deployment_id: &str,
) -> Response<FabricBody> {
    match crate::reconcile::deployment_run::reconcile_deployment(fabric, deployment_id).await {
        Ok(status) => json_response(
            StatusCode::OK,
            json!({
                "id": deployment_id,
                "kind": "deployment",
                "state": status.observed_state,
                "desiredRevision": status.desired_revision,
                "observedRevision": status.observed_revision,
                "desiredState": status.desired_state,
                "lastErrorCode": status.last_error,
            })
            .to_string(),
        ),
        Err(FabricError::NotFound) => crate::error_response(FabricError::NotFound),
        Err(error) => crate::error_response(error),
    }
}

/// Maps a lagging Ready wait to HTTP 409. Unknown/Realize must not be
/// treated as a no-replay unknown; Conflict is the retryable contract.
fn retryable_unready(error: FabricError, message: &str) -> FabricError {
    match error {
        FabricError::Unknown(_) | FabricError::Realize(_) => FabricError::Conflict(message.into()),
        other => other,
    }
}

async fn ensure_migrate_ready(
    fabric: &crate::Fabric,
    deployment_id: &str,
    body: &JournalBody,
) -> Result<(), FabricError> {
    let pod = fabric
        .get_product_resource("deployment", deployment_id)
        .ok()
        .flatten()
        .and_then(|(pod, _, _)| pod)
        .unwrap_or_else(|| app_pod_name(deployment_id));
    fabric
        .live()
        .wait_pod_running(&pod, Duration::from_secs(30))
        .await
        .map_err(|error| retryable_unready(error, "application pod is not Running"))?;
    let Some(database_id) = body.database_id.as_deref().filter(|id| !id.is_empty()) else {
        return Ok(());
    };
    fabric
        .live()
        .wait_pod_ready(
            &live_postgres_pod(fabric, database_id),
            Duration::from_secs(30),
        )
        .await
        .map_err(|error| retryable_unready(error, "postgres is not Ready"))?;
    Ok(())
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
            // Terminal dump was streamed once. Fabric does not retain the
            // file; Control reads Blob or starts a new backup operation.
            return crate::error_response(FabricError::NotFound);
        }
        Ok(_) => {}
        Err(error) => return crate::error_response(error),
    }
    let pod = live_postgres_pod(fabric, database_id);
    const BACKUP_TIMEOUT_MS: u64 = crate::storage::PRODUCT_VOLUME_IO_TIMEOUT_MS;
    let dump_cmd = postgres_client_command("pg_dump -U app -d app -Fc");
    stream_database_backup(
        fabric.live().clone(),
        fabric.store.clone(),
        database_id,
        &body.operation_id.to_string(),
        &pod,
        dump_cmd,
        BACKUP_TIMEOUT_MS,
    )
}

fn stream_database_backup(
    live: crate::Live,
    store: crate::Store,
    database_id: &str,
    operation_id: &str,
    pod: &str,
    dump_cmd: [String; 3],
    timeout_ms: u64,
) -> Response<FabricBody> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let database_id = database_id.to_owned();
    let operation_id = operation_id.to_owned();
    let pod = pod.to_owned();
    tokio::spawn(async move {
        let argv: Vec<&str> = dump_cmd.iter().map(String::as_str).collect();
        let dump = live
            .exec_guest_stdout_chunks(&pod, "postgres", &argv, tx.clone(), timeout_ms)
            .await;
        match dump {
            Ok(output) if !output.ambiguous && output.exit_code == 0 => {
                drop(tx);
                let _ = store.complete_product_operation(
                    "database-backup",
                    &database_id,
                    &operation_id,
                    "terminal",
                );
            }
            Ok(output) if !output.ambiguous => {
                let _ = tx
                    .send(Err(std::io::Error::other(format!(
                        "pg_dump exited {}",
                        output.exit_code
                    ))))
                    .await;
                let _ = store.complete_product_operation(
                    "database-backup",
                    &database_id,
                    &operation_id,
                    "failed",
                );
            }
            _ => {
                let _ = tx
                    .send(Err(std::io::Error::other("database backup did not settle")))
                    .await;
                let _ = store.complete_product_operation(
                    "database-backup",
                    &database_id,
                    &operation_id,
                    "unknown",
                );
            }
        }
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, "application/octet-stream")
        .body(http_body_util::StreamBody::new(BackupBody { rx }).boxed())
        .expect("response parts are valid")
}

const WORKSPACE_SNAPSHOT_GUEST: &str = "/workspace/.voie/tmp/workspace-snapshot.tar.zst";

pub(crate) fn stream_workspace_snapshot(
    live: crate::Live,
    store: crate::Store,
    workspace_id: &str,
    operation_id: &str,
    pod: &str,
) -> Response<FabricBody> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let workspace_id = workspace_id.to_owned();
    let operation_id = operation_id.to_owned();
    let pod = pod.to_owned();
    tokio::spawn(async move {
        const SNAPSHOT_TIMEOUT_MS: u64 = crate::storage::PRODUCT_VOLUME_IO_TIMEOUT_MS;
        let dump = live
            .exec_guest_stdout_chunks(
                &pod,
                "runner",
                &["/bin/cat", WORKSPACE_SNAPSHOT_GUEST],
                tx.clone(),
                SNAPSHOT_TIMEOUT_MS,
            )
            .await;
        match dump {
            Ok(output) if !output.ambiguous && output.exit_code == 0 => {
                drop(tx);
                let _ = store.complete_product_operation(
                    "workspace-snapshot",
                    &workspace_id,
                    &operation_id,
                    "terminal",
                );
                let _ = live
                    .exec_guest(
                        &pod,
                        "runner",
                        &["/sbin/fstrim", "-v", "/workspace"],
                        60_000,
                    )
                    .await;
            }
            Ok(output) if !output.ambiguous => {
                let _ = tx
                    .send(Err(std::io::Error::other(format!(
                        "workspace snapshot cat exited {}",
                        output.exit_code
                    ))))
                    .await;
                let _ = store.complete_product_operation(
                    "workspace-snapshot",
                    &workspace_id,
                    &operation_id,
                    "failed",
                );
            }
            _ => {
                let _ = tx
                    .send(Err(std::io::Error::other(
                        "workspace snapshot stream did not settle",
                    )))
                    .await;
                let _ = store.complete_product_operation(
                    "workspace-snapshot",
                    &workspace_id,
                    &operation_id,
                    "unknown",
                );
            }
        }
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, "application/octet-stream")
        .body(http_body_util::StreamBody::new(BackupBody { rx }).boxed())
        .expect("response parts are valid")
}

pub(crate) fn stream_workspace_pack(
    live: crate::Live,
    pod: &str,
    remote: &str,
    hash: &str,
) -> Response<FabricBody> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let pod = pod.to_owned();
    let remote = remote.to_owned();
    tokio::spawn(async move {
        const PACK_TIMEOUT_MS: u64 = crate::storage::PRODUCT_VOLUME_IO_TIMEOUT_MS;
        let dump = live
            .exec_guest_stdout_chunks(
                &pod,
                "runner",
                &["/bin/cat", &remote],
                tx.clone(),
                PACK_TIMEOUT_MS,
            )
            .await;
        match dump {
            Ok(output) if !output.ambiguous && output.exit_code == 0 => {
                drop(tx);
            }
            Ok(output) if !output.ambiguous => {
                let _ = tx
                    .send(Err(std::io::Error::other(format!(
                        "workspace pack cat exited {}",
                        output.exit_code
                    ))))
                    .await;
            }
            _ => {
                let _ = tx
                    .send(Err(std::io::Error::other(
                        "workspace pack stream did not settle",
                    )))
                    .await;
            }
        }
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(hyper::header::CONTENT_TYPE, "application/octet-stream")
        .header("x-voie-artifact-hash", hash)
        .body(http_body_util::StreamBody::new(BackupBody { rx }).boxed())
        .expect("response parts are valid")
}

struct BackupBody {
    rx: tokio::sync::mpsc::Receiver<Result<Bytes, std::io::Error>>,
}

impl futures_util::Stream for BackupBody {
    type Item = Result<hyper::body::Frame<Bytes>, std::io::Error>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.rx.poll_recv(cx) {
            std::task::Poll::Ready(Some(Ok(bytes))) => {
                std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(bytes))))
            }
            std::task::Poll::Ready(Some(Err(error))) => std::task::Poll::Ready(Some(Err(error))),
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
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

fn request_header<'a>(request: &'a Request<Incoming>, name: &str) -> Option<&'a str> {
    request
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
}

enum RestoreDump {
    Body {
        body: Incoming,
        expected_hash: String,
    },
}

async fn put_restore_artifact(
    fabric: &crate::Fabric,
    database_id: &str,
    request: Request<Incoming>,
) -> Response<FabricBody> {
    let expected = match request_header(&request, "x-voie-artifact-hash") {
        Some(value) => value.to_ascii_lowercase(),
        None => {
            return crate::error_response(FabricError::Config(
                "restore artifact hash header is required",
            ));
        }
    };
    let operation_id = match request_header(&request, "x-voie-operation-id")
        .and_then(|value| Uuid::parse_str(value).ok())
    {
        Some(value) => value,
        None => {
            return crate::error_response(FabricError::Config(
                "restore operation id header is required",
            ));
        }
    };
    let request_hash = request_header(&request, "x-voie-request-hash")
        .unwrap_or(expected.as_str())
        .to_owned();
    let slug = request_header(&request, "x-voie-slug").map(ToOwned::to_owned);
    let kind = request_header(&request, "x-voie-kind").map(ToOwned::to_owned);
    let allocated_bytes =
        request_header(&request, "x-voie-allocated-bytes").and_then(|value| value.parse().ok());
    let desired_revision = request_header(&request, "x-voie-desired-revision")
        .and_then(|value| value.parse().ok())
        .unwrap_or(1);
    let security_profile =
        request_header(&request, "x-voie-security-profile").and_then(|value| value.parse().ok());
    let postgres_password =
        request_header(&request, "x-voie-postgres-password").map(ToOwned::to_owned);
    let op = operation_id.to_string();
    let state = match fabric.begin_product_operation("database", database_id, &op, &request_hash) {
        Ok(state) if state == "failed" => {
            match fabric.redispatch_failed_product_operation("database", database_id, &op) {
                Ok(true) => "dispatched".to_owned(),
                Ok(false) => state,
                Err(error) => return crate::error_response(error),
            }
        }
        Ok(state) => state,
        Err(error) => return crate::error_response(error),
    };
    if state == "unknown" {
        return crate::error_response(FabricError::Unknown(
            "database restore outcome unknown; the intent will not be dispatched again".into(),
        ));
    }
    if state != "dispatched" {
        return json_response(
            StatusCode::OK,
            json!({
                "state": "ready",
                "resourceId": database_id,
            })
            .to_string(),
        );
    }
    let plan = restore_plan(
        fabric,
        database_id,
        operation_id,
        desired_revision,
        slug,
        kind,
        allocated_bytes,
        security_profile,
    );
    let dump = RestoreDump::Body {
        body: request.into_body(),
        expected_hash: expected.clone(),
    };
    let _lock = fabric
        .lifecycle_guard(&format!("database:{database_id}"))
        .await;
    match restore_database_dump(
        fabric,
        database_id,
        &plan,
        postgres_password.as_deref(),
        dump,
    )
    .await
    {
        Ok(()) => {
            let _ = fabric.complete_product_operation("database", database_id, &op, "terminal");
            json_response(
                StatusCode::CREATED,
                json!({
                    "state": "ready",
                    "resourceId": database_id,
                    "artifactHash": expected,
                })
                .to_string(),
            )
        }
        Err(error) => {
            let journal = if matches!(error, FabricError::Unknown(_)) {
                "unknown"
            } else {
                "failed"
            };
            let _ = fabric.complete_product_operation("database", database_id, &op, journal);
            crate::error_response(error)
        }
    }
}

async fn restore_database_dump(
    fabric: &crate::Fabric,
    database_id: &str,
    plan: &RestorePlan,
    postgres_password: Option<&str>,
    dump: RestoreDump,
) -> Result<(), FabricError> {
    teardown_restore_candidate(fabric, database_id).await;
    let current = fabric.get_allocation(crate::VolumeKind::Database, database_id)?;
    let prod = plan.kind == "prod";
    let bytes = current
        .as_ref()
        .map(|row| row.allocated_bytes)
        .or(plan.allocated_bytes)
        .unwrap_or_else(|| fabric.live().storage().database_size(prod, plan.elevated));
    let old_pod = live_postgres_pod(fabric, database_id);
    let old_pvc = live_postgres_volume(fabric, database_id);
    let operation = plan.operation_id.to_string();
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
        plan,
        &operation,
        &slot.device,
        bytes,
        dump,
        &old_pod,
        &old_pvc,
        postgres_password,
    )
    .await;
    if matches!(
        &restore_result,
        Err(FabricError::Realize(_)) | Err(FabricError::Unknown(_)) | Err(FabricError::Config(_))
    ) {
        teardown_named_restore_candidate(fabric, database_id, &operation).await;
    }
    restore_result
}

async fn restore_onto_candidate(
    fabric: &crate::Fabric,
    database_id: &str,
    plan: &RestorePlan,
    operation: &str,
    device: &str,
    bytes: u64,
    dump: RestoreDump,
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
        Some(plan.slug.as_str()).filter(|value| !value.is_empty()),
        bytes,
    );
    let pvc = product_realize::postgres_restore_pvc_yaml(
        fabric.live(),
        database_id,
        operation,
        Some(plan.slug.as_str()).filter(|value| !value.is_empty()),
        bytes,
    );
    apply_or_unknown(fabric, &format!("{pv}\n---\n{pvc}")).await?;
    let intent = DatabaseIntent {
        database_id: database_id.to_owned(),
        slug: plan.slug.clone(),
        kind: plan.kind.clone(),
        security_profile: plan.security_profile,
        revision: plan.desired_revision.max(1),
    };
    if let Some(password) = postgres_password {
        let mut pg_labels: Vec<(&str, &str)> = vec![
            ("io.voie/kind", "postgres"),
            ("io.voie/database", database_id),
        ];
        if !plan.slug.is_empty() {
            pg_labels.push(("io.voie/slug", plan.slug.as_str()));
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
    fabric
        .live()
        .wait_pod_ready(&candidate, Duration::from_secs(180))
        .await?;
    let restore_cmd =
        postgres_client_command("pg_restore -U app -d app --clean --if-exists --no-owner -Fc");
    let restore_argv: Vec<&str> = restore_cmd.iter().map(String::as_str).collect();
    let RestoreDump::Body {
        body,
        expected_hash,
    } = dump;
    let output = fabric
        .live()
        .exec_guest_stdin_body(
            &candidate,
            "postgres",
            &restore_argv,
            body,
            &expected_hash,
            crate::storage::DATABASE_PROD_ELEVATED_BYTES,
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

pub(crate) fn live_postgres_pod(fabric: &crate::Fabric, database_id: &str) -> String {
    let lv_name = fabric
        .get_allocation(crate::VolumeKind::Database, database_id)
        .ok()
        .flatten()
        .map(|row| row.lv_name);
    let recorded = fabric
        .get_product_resource("database", database_id)
        .ok()
        .flatten()
        .and_then(|(pod, _, _)| pod);
    postgres_pod_from_allocation(lv_name.as_deref(), recorded.as_deref(), database_id)
}

fn postgres_pod_from_allocation(
    lv_name: Option<&str>,
    recorded_pod: Option<&str>,
    database_id: &str,
) -> String {
    if let Some(lv) = lv_name.filter(|name| !name.is_empty()) {
        return postgres_pod_for_lv(lv, database_id);
    }
    recorded_pod
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| postgres_pod_name(database_id))
}

pub(crate) fn live_postgres_volume(fabric: &crate::Fabric, database_id: &str) -> String {
    fabric
        .get_allocation(crate::VolumeKind::Database, database_id)
        .ok()
        .flatten()
        .map(|row| postgres_pvc_for_lv(&row.lv_name, database_id))
        .unwrap_or_else(|| postgres_volume_name(database_id))
}

async fn put_database_spec(
    fabric: &crate::Fabric,
    database_id: &str,
    request: Request<Incoming>,
) -> Response<FabricBody> {
    let bytes = match request.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => return crate::error_response(FabricError::Config("request body is unreadable")),
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return crate::error_response(FabricError::Config("JSON is unusable")),
    };
    if let Err(error) = reject_forbidden(&value) {
        return crate::error_response(error);
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct PutBody {
        revision: i64,
        desired: String,
        #[serde(default)]
        runtime_profile: Option<String>,
        #[serde(default)]
        security_profile: Option<u32>,
        #[serde(default)]
        storage_tier: String,
        #[serde(default)]
        volume_bytes: u64,
        #[serde(default)]
        credential_version: Option<i64>,
        slug: String,
        kind: String,
        #[serde(default)]
        postgres_password: Option<String>,
    }
    let body: PutBody = match serde_json::from_value(value) {
        Ok(body) => body,
        Err(_) => return crate::error_response(FabricError::Config("JSON is unusable")),
    };
    let Some(desired) = crate::specs::database::DatabaseDesiredName::parse(&body.desired) else {
        return crate::error_response(FabricError::Config(
            "desired must be present, suspended, or absent",
        ));
    };
    let spec = crate::specs::database::DatabaseSpec {
        revision: body.revision,
        desired,
        runtime_profile: body
            .runtime_profile
            .filter(|profile| !profile.is_empty())
            .unwrap_or_default(),
        security_profile: body.security_profile.unwrap_or(0),
        storage_tier: body.storage_tier,
        volume_bytes: body.volume_bytes,
        credential_version: body.credential_version.unwrap_or(1),
        slug: body.slug,
        kind: body.kind,
    };
    if let Err(message) = spec.validate() {
        return crate::error_response(FabricError::Config(message));
    }
    if let Err(error) =
        crate::reconcile::database_run::persist_database_spec_for(fabric, database_id, &spec)
    {
        return crate::error_response(error);
    }
    match crate::reconcile::database_run::reconcile_database(
        fabric,
        database_id,
        body.postgres_password.as_deref(),
    )
    .await
    {
        Ok(status) => json_response(
            StatusCode::OK,
            json!({
                "desiredRevision": status.desired_revision,
                "observedRevision": status.observed_revision,
                "state": status.observed_state,
                "desiredState": status.desired_state,
                "runtimeProfile": spec.runtime_profile,
                "securityProfile": spec.security_profile,
                "lastErrorCode": status.last_error,
            })
            .to_string(),
        ),
        Err(error) => crate::error_response(error),
    }
}

async fn put_deployment_spec(
    fabric: &crate::Fabric,
    deployment_id: &str,
    request: Request<Incoming>,
) -> Response<FabricBody> {
    let bytes = match request.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => return crate::error_response(FabricError::Config("request body is unreadable")),
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return crate::error_response(FabricError::Config("JSON is unusable")),
    };
    if let Err(error) = reject_forbidden(&value) {
        return crate::error_response(error);
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct PutBody {
        revision: i64,
        desired: String,
        release_id: Uuid,
        release_hash: String,
        #[serde(default)]
        runtime_profile: Option<String>,
        slug: String,
        kind: String,
        #[serde(default)]
        port: Option<std::num::NonZeroU16>,
        #[serde(default)]
        run_argv: Vec<String>,
        #[serde(default)]
        health_path: Option<String>,
        #[serde(default)]
        cpu_millis: Option<u32>,
        #[serde(default)]
        memory_mb: Option<u32>,
        /// One-shot Database identity. Copied into the env secret; never stored
        /// on the typed spec.
        #[serde(default)]
        database_id: Option<String>,
        /// One-shot Environment bindings. Dropped after the Secret apply.
        #[serde(default)]
        env_bindings: Option<Vec<EnvBinding>>,
        /// Predecessor Deployment. Stored on the typed spec for activate.
        #[serde(default)]
        previous_deployment_id: Option<Uuid>,
        #[serde(default)]
        pod_generation: Option<i64>,
    }
    let body: PutBody = match serde_json::from_value(value) {
        Ok(body) => body,
        Err(_) => return crate::error_response(FabricError::Config("JSON is unusable")),
    };
    let Some(desired) = crate::specs::deployment::DeploymentDesiredName::parse(&body.desired)
    else {
        return crate::error_response(FabricError::Config(
            "desired must be running, stopped, or absent",
        ));
    };
    let spec = crate::specs::deployment::DeploymentSpec {
        revision: body.revision,
        desired,
        release_id: body.release_id,
        release_hash: body.release_hash,
        runtime_profile: body
            .runtime_profile
            .filter(|profile| !profile.is_empty())
            .unwrap_or_default(),
        slug: body.slug,
        kind: body.kind,
        port: body.port.map(|port| port.get()).unwrap_or(0),
        run_argv: body.run_argv,
        health_path: body
            .health_path
            .filter(|path| !path.is_empty())
            .unwrap_or_default(),
        cpu_millis: body.cpu_millis.unwrap_or(0),
        memory_mb: body.memory_mb.unwrap_or(0),
        previous_deployment_id: body.previous_deployment_id,
        pod_generation: body.pod_generation.unwrap_or(0),
    };
    if let Err(message) = spec.validate() {
        return crate::error_response(FabricError::Config(message));
    }
    let _lock = fabric
        .lifecycle_guard(&format!("deployment:{deployment_id}"))
        .await;
    let hash = spec.hash_bytes();
    let decision =
        match fabric
            .store
            .evaluate_resource_spec("deployment", deployment_id, spec.revision, &hash)
        {
            Ok(decision) => decision,
            Err(error) => return crate::error_response(error),
        };
    let decision = match crate::specs::accept::require_spec_write(decision) {
        Ok(decision) => decision,
        Err(error) => return crate::error_response(error),
    };
    if crate::specs::accept::deployment_secret_bind_applies(decision)
        && spec.desired == crate::specs::deployment::DeploymentDesiredName::Running
        && (body.database_id.is_some() || body.env_bindings.is_some())
    {
        let slug = if spec.slug.is_empty() {
            None
        } else {
            Some(spec.slug.as_str())
        };
        if let Err(error) = bind_application_env(
            fabric,
            deployment_id,
            body.database_id.as_deref(),
            body.env_bindings.as_deref().unwrap_or(&[]),
            slug,
        )
        .await
        {
            return crate::error_response(error);
        }
    }
    if decision == crate::specs::accept::DesiredSpecAcceptance::Accept {
        if let Err(error) = crate::reconcile::deployment_run::persist_deployment_spec_for(
            fabric,
            deployment_id,
            &spec,
        ) {
            return crate::error_response(error);
        }
    }
    match crate::reconcile::deployment_run::reconcile_deployment_held(fabric, deployment_id).await {
        Ok(status) => json_response(
            StatusCode::OK,
            json!({
                "desiredRevision": status.desired_revision,
                "observedRevision": status.observed_revision,
                "observedPodGeneration": status.observed_pod_generation,
                "state": status.observed_state,
                "desiredState": status.desired_state,
                "runtimeProfile": spec.runtime_profile,
                "lastErrorCode": status.last_error,
            })
            .to_string(),
        ),
        Err(error) => crate::error_response(error),
    }
}

/// Local sockets use SCRAM. Read the guest password file inside the
/// postgres container; never put the secret on kubectl argv.
pub(crate) fn postgres_client_command(argv: &str) -> [String; 3] {
    [
        "sh".into(),
        "-c".into(),
        format!(
            "set -eu; PGPASSWORD=$(cat /run/voie/postgres-password); export PGPASSWORD; exec {argv}"
        ),
    ]
}

pub(crate) async fn put_traffic_spec(
    fabric: &crate::Fabric,
    environment_id: &str,
    request: Request<Incoming>,
) -> Response<FabricBody> {
    let bytes = match request.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(_) => return crate::error_response(FabricError::Config("request body is unreadable")),
    };
    let spec: crate::specs::traffic::TrafficSpec = match serde_json::from_slice(&bytes) {
        Ok(spec) => spec,
        Err(_) => return crate::error_response(FabricError::Config("JSON is unusable")),
    };
    if let Err(message) = spec.validate() {
        return crate::error_response(FabricError::Config(message));
    }
    let typed = match serde_json::to_string(&spec) {
        Ok(typed) => typed,
        Err(_) => {
            return crate::error_response(FabricError::Store("cannot encode traffic spec".into()));
        }
    };
    let _lock = fabric
        .lifecycle_guard(&format!("traffic:{environment_id}"))
        .await;
    match fabric.store.accept_resource_spec(
        "traffic",
        environment_id,
        spec.revision,
        &spec.hash_bytes(),
        &typed,
    ) {
        Ok(crate::specs::accept::DesiredSpecAcceptance::Stale) => {
            return crate::error_response(FabricError::Conflict("stale desired revision".into()));
        }
        Ok(crate::specs::accept::DesiredSpecAcceptance::Conflict) => {
            return crate::error_response(FabricError::Conflict("desired spec conflict".into()));
        }
        Ok(crate::specs::accept::DesiredSpecAcceptance::Idempotent) => {
            return match live_selector_deployment(fabric, &spec).await {
                Ok(observed) => json_response(
                    StatusCode::OK,
                    traffic_wire(environment_id, &spec, observed).to_string(),
                ),
                Err(error) => crate::error_response(error),
            };
        }
        Ok(crate::specs::accept::DesiredSpecAcceptance::Accept) => {}
        Err(error) => return crate::error_response(error),
    }
    match realize_traffic(fabric, environment_id, &spec).await {
        Ok(observed) => json_response(
            StatusCode::OK,
            traffic_wire(environment_id, &spec, observed).to_string(),
        ),
        Err(error) => crate::error_response(error),
    }
}

pub(crate) async fn get_traffic_spec(
    fabric: &crate::Fabric,
    environment_id: &str,
) -> Response<FabricBody> {
    let Some(row) = (match fabric.store.get_resource_spec("traffic", environment_id) {
        Ok(row) => row,
        Err(error) => return crate::error_response(error),
    }) else {
        return crate::error_response(FabricError::NotFound);
    };
    let spec: crate::specs::traffic::TrafficSpec = match serde_json::from_str(&row.typed_spec) {
        Ok(spec) => spec,
        Err(_) => {
            return crate::error_response(FabricError::Store("traffic spec is unusable".into()));
        }
    };
    match live_selector_deployment(fabric, &spec).await {
        Ok(observed) => json_response(
            StatusCode::OK,
            traffic_wire(environment_id, &spec, observed).to_string(),
        ),
        Err(error) => crate::error_response(error),
    }
}

pub(crate) async fn reconcile_accepted_traffic(fabric: &crate::Fabric) -> Result<(), FabricError> {
    let rows = fabric.store.list_resource_specs("traffic")?;
    for row in rows {
        let _lock = fabric
            .lifecycle_guard(&format!("traffic:{}", row.resource_id))
            .await;
        let Some(fresh) = fabric
            .store
            .get_resource_spec("traffic", &row.resource_id)?
        else {
            continue;
        };
        let spec: crate::specs::traffic::TrafficSpec = match serde_json::from_str(&fresh.typed_spec)
        {
            Ok(spec) => spec,
            Err(_) => continue,
        };
        match realize_traffic(fabric, &fresh.resource_id, &spec).await {
            Ok(observed) if spec.matches_observed(observed) => {
                let _ = fabric.store.set_resource_spec_observed(
                    "traffic",
                    &fresh.resource_id,
                    spec.revision,
                    spec.observed_state(observed),
                    None,
                );
            }
            Ok(_) => {}
            Err(error) => eprintln!(
                "voie-fabricd: traffic {} reconcile: {error}",
                fresh.resource_id
            ),
        }
    }
    Ok(())
}

fn traffic_wire(
    environment_id: &str,
    spec: &crate::specs::traffic::TrafficSpec,
    observed: Option<Uuid>,
) -> Value {
    json!({
        "desiredRevision": spec.revision,
        "observedRevision": spec.observed_revision(observed),
        "state": spec.observed_state(observed),
        "resourceId": environment_id,
        "observedDeploymentId": observed,
    })
}

async fn realize_traffic(
    fabric: &crate::Fabric,
    environment_id: &str,
    spec: &crate::specs::traffic::TrafficSpec,
) -> Result<Option<Uuid>, FabricError> {
    let stored_revision = fabric
        .store
        .get_resource_spec("traffic", environment_id)?
        .map(|row| row.desired_revision)
        .unwrap_or(0);
    if !crate::specs::accept::traffic_realize_applies(stored_revision, spec.revision) {
        return live_selector_deployment(fabric, spec).await;
    }
    match spec.desired_deployment_id {
        None => {
            if spec.slug.is_empty() {
                return Ok(live_selector_deployment(fabric, spec).await?);
            }
            clear_environment_edge(fabric, &spec.slug, &spec.kind).await?;
            live_selector_deployment(fabric, spec).await
        }
        Some(desired) => {
            let deployment = load_deployment_spec(fabric, &desired.to_string())?;
            if !spec.slug.is_empty()
                && (deployment.slug != spec.slug || deployment.kind != spec.kind)
            {
                return Err(FabricError::Config(
                    "traffic slug/kind must match the Deployment",
                ));
            }
            let live = live_selector_deployment(fabric, spec).await?;
            if live != Some(desired) {
                if !crate::specs::accept::traffic_realize_applies(
                    fabric
                        .store
                        .get_resource_spec("traffic", environment_id)?
                        .map(|row| row.desired_revision)
                        .unwrap_or(0),
                    spec.revision,
                ) {
                    return live_selector_deployment(fabric, spec).await;
                }
                ensure_gateway_ready(fabric).await?;
                switch_environment_selector(fabric, &desired.to_string()).await?;
            }
            live_selector_deployment(fabric, spec).await
        }
    }
}

async fn live_selector_deployment(
    fabric: &crate::Fabric,
    spec: &crate::specs::traffic::TrafficSpec,
) -> Result<Option<Uuid>, FabricError> {
    let (slug, kind) = if !spec.slug.is_empty() {
        (spec.slug.clone(), spec.kind.clone())
    } else if let Some(desired) = spec.desired_deployment_id {
        let deployment = load_deployment_spec(fabric, &desired.to_string())?;
        (deployment.slug, deployment.kind)
    } else {
        return Ok(None);
    };
    if slug.is_empty() {
        return Ok(None);
    }
    let name = app_service_name(&slug, &kind);
    let Some(value) = fabric.live().get_namespaced("svc", &name).await? else {
        return Ok(None);
    };
    let Some(id) = value
        .pointer("/spec/selector")
        .and_then(|selector| selector.get("io.voie/deployment"))
        .and_then(|item| item.as_str())
    else {
        return Ok(None);
    };
    Ok(Uuid::parse_str(id).ok())
}

async fn drain_observational(request: Request<Incoming>) -> Result<(), FabricError> {
    let bytes = request
        .into_body()
        .collect()
        .await
        .map_err(|_| FabricError::Config("request body is unreadable"))?
        .to_bytes();
    if bytes.is_empty() {
        return Ok(());
    }
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return Err(FabricError::Config("JSON is unusable")),
    };
    reject_forbidden(&value)
}

async fn read_and_validate(request: Request<Incoming>) -> Result<JournalBody, FabricError> {
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
    use uuid::Uuid;

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
            parse_product_route(&Method::PUT, &["v1", "deployments", deployment]),
            Some(("deployment", deployment, "put-spec"))
        );
        assert_eq!(
            parse_product_route(&Method::PUT, &["v1", "deployments", deployment, "artifact"]),
            Some(("deployment", deployment, "artifact"))
        );
        assert_eq!(
            parse_product_route(
                &Method::POST,
                &["v1", "deployments", deployment, "activate"]
            ),
            Some(("deployment", deployment, "activate"))
        );
        assert!(
            parse_product_route(&Method::POST, &["v1", "deployments", deployment, "delete"])
                .is_none(),
            "deployment delete is a PUT spec, not a journal POST"
        );
        assert!(
            parse_product_route(&Method::POST, &["v1", "deployments", deployment, "stop"])
                .is_none(),
            "deployment stop is a PUT spec, not a journal POST"
        );
        assert!(parse_product_route(&Method::POST, &["v1", "databases", database]).is_none());
        assert_eq!(
            parse_product_route(&Method::PUT, &["v1", "databases", database]),
            Some(("database", database, "put-spec"))
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
        assert!(!super::egress_pod_needs_replace(
            "Running", false, "Default"
        ));
        assert!(!super::egress_pod_needs_replace(
            "Pending", false, "Default"
        ));
        assert!(super::egress_pod_needs_replace("Failed", false, "Default"));
        assert!(super::egress_pod_needs_replace(
            "Succeeded",
            false,
            "Default"
        ));
        assert!(super::egress_pod_needs_replace("Running", true, "Default"));
        assert!(super::egress_pod_needs_replace(
            "Running",
            false,
            "ClusterFirst"
        ));
    }

    #[test]
    fn observational_health_requires_wget_ready_and_edge() {
        assert!(super::observational_healthy(true, true, true));
        assert!(!super::observational_healthy(true, true, false));
        assert!(!super::observational_healthy(true, false, true));
        assert!(!super::observational_healthy(false, true, true));
    }

    #[test]
    fn candidate_edge_url_uses_pod_ip_not_loopback() {
        assert_eq!(
            super::candidate_edge_url(Some("10.42.1.17"), 8080, "/healthz").as_deref(),
            Some("http://10.42.1.17:8080/healthz")
        );
        assert_eq!(
            super::candidate_edge_url(Some("127.0.0.1"), 8080, "/healthz"),
            None
        );
        assert_eq!(super::candidate_edge_url(None, 8080, "/healthz"), None);
        let health = include_str!("product.rs")
            .split("async fn probe_deployment_health")
            .nth(1)
            .unwrap_or("");
        let health = health
            .split("async fn activate_environment_selector")
            .next()
            .unwrap_or("");
        assert!(
            health.contains("candidate_edge_url"),
            "proven must GET the candidate Pod IP before traffic creates the Service"
        );
        assert!(
            !health.contains("service_cluster_ip"),
            "Environment ClusterIP exists only after activate"
        );
    }

    #[test]
    fn release_delete_replays_terminal_other_ops_do_not() {
        assert!(super::should_realize_product_op(
            "release",
            "delete",
            "dispatched"
        ));
        assert!(super::should_realize_product_op(
            "release", "delete", "terminal"
        ));
        assert!(!super::should_realize_product_op(
            "deployment",
            "activate",
            "terminal"
        ));
        assert!(!super::should_realize_product_op(
            "deployment",
            "stop",
            "terminal"
        ));
        assert!(!super::should_realize_product_op(
            "deployment",
            "delete",
            "terminal"
        ));
        assert!(!super::should_realize_product_op(
            "database", "delete", "terminal"
        ));
        assert!(!super::should_realize_product_op(
            "deployment",
            "migrate",
            "terminal"
        ));
        assert!(!super::should_realize_product_op(
            "database", "restore", "terminal"
        ));
        assert!(super::should_realize_product_op(
            "database",
            "restore",
            "dispatched"
        ));
        assert_eq!(
            super::replayable_journal_on_error(
                "deployment",
                "activate",
                &FabricError::Conflict("voie-gateway is not Ready".into()),
            ),
            "unknown"
        );
        assert_eq!(
            super::replayable_journal_on_error(
                "deployment",
                "stop",
                &FabricError::Conflict("voie-gateway is not Ready".into()),
            ),
            "unknown"
        );
        assert_eq!(
            super::replayable_journal_on_error(
                "database",
                "delete",
                &FabricError::Conflict("product object delete is not settled".into()),
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
        assert_eq!(
            super::replayable_journal_on_error(
                "deployment",
                "migrate",
                &FabricError::Conflict("application pod is not Running".into()),
            ),
            "failed"
        );
        assert_eq!(
            super::replayable_journal_on_error(
                "deployment",
                "migrate",
                &FabricError::Unknown("migration did not settle".into()),
            ),
            "unknown"
        );
        assert_eq!(
            super::replayable_journal_on_error(
                "deployment",
                "migrate",
                &FabricError::Realize("migration exited 1".into()),
            ),
            "unknown"
        );
    }

    #[test]
    fn migrate_targets_restore_postgres_pod_not_canonical_name() {
        let db = "e86b6b50-71e7-4749-a2c6-f147e05e5d64";
        let op = "75b11ad2-5d53-48fc-a954-f3079239d75a";
        let restore_lv = format!("rst{}", super::compact_id(op));
        let restore_pod = super::postgres_restore_pod_name(op);
        assert_ne!(restore_pod, super::postgres_pod_name(db));
        assert_eq!(
            super::postgres_pod_from_allocation(
                Some(&restore_lv),
                Some(&super::postgres_pod_name(db)),
                db
            ),
            restore_pod
        );
        assert_eq!(
            super::postgres_pod_from_allocation(None, Some(&restore_pod), db),
            restore_pod
        );
        assert_eq!(
            super::postgres_pod_from_allocation(None, None, db),
            super::postgres_pod_name(db)
        );
    }

    #[test]
    fn journals_are_migrate_and_release_delete() {
        assert!(!super::replayable_product_op("deployment", "activate"));
        assert!(super::replayable_product_op("release", "delete"));
        assert!(!super::replayable_product_op("deployment", "restart"));
        assert!(!super::replayable_product_op("deployment", "stop"));
        assert!(!super::replayable_product_op("deployment", "delete"));
        assert!(!super::replayable_product_op("database", "delete"));
        assert!(!super::replayable_product_op("deployment", "migrate"));
        assert!(!super::replayable_product_op("database", "restore"));
        assert!(!super::replayable_product_op("release", "materialize"));
    }

    #[test]
    fn restore_and_materialize_return_before_journal_parse() {
        let src = include_str!("product.rs");
        let handle = src.split("pub async fn handle(").nth(1).unwrap_or("");
        let handle = handle
            .split("async fn realize_desired")
            .next()
            .unwrap_or("");
        let before_validate = handle.split("match read_and_validate").next().unwrap_or("");
        assert!(
            before_validate.contains("action == \"materialize\""),
            "materialize must refuse before JournalBody parse"
        );
        assert!(
            before_validate.contains("action == \"restore\""),
            "restore must refuse before JournalBody parse"
        );
        assert!(
            before_validate
                .contains("release artifact must be streamed onto the deployment volume"),
            "materialize must tell the caller to stream the artifact"
        );
        assert!(
            before_validate.contains("restore dump must be streamed onto the candidate"),
            "restore must tell the caller to stream the dump"
        );
    }

    #[test]
    fn activate_is_observational_not_a_journal() {
        let src = include_str!("product.rs");
        let activate = src
            .split("async fn activate_environment_selector")
            .nth(1)
            .unwrap_or("");
        let activate = activate
            .split("fn should_realize_product_op")
            .next()
            .unwrap_or("");
        assert!(
            activate.contains("drain_observational"),
            "activate must drain an observational body like health"
        );
        assert!(
            !activate.contains("begin_product_operation"),
            "activate must not open a product journal"
        );
        assert!(
            !activate.contains("realize_traffic"),
            "leftover POST must not realize traffic; PUT /v1/traffic owns the selector"
        );
        assert!(
            activate.contains("live_selector_deployment"),
            "leftover POST may only observe a stored traffic spec"
        );
        assert!(
            activate.contains("PUT /v1/traffic"),
            "activate without a stored traffic spec must refuse"
        );
    }

    #[test]
    fn deployment_put_one_shot_bindings_are_not_infrastructure() {
        let value = serde_json::json!({
            "revision": 1,
            "desired": "running",
            "databaseId": "22222222-2222-2222-2222-222222222222",
            "envBindings": [{"name": "SESSION_SECRET", "value": "once"}],
        });
        super::reject_forbidden(&value).expect("one-shot bindings are typed fields");
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
    fn gateway_route_uses_fabric_service_name() {
        let src = include_str!("product.rs");
        let activate = src
            .split("async fn switch_environment_selector")
            .nth(1)
            .unwrap_or("");
        let activate = activate
            .split("fn egress_pod_needs_replace")
            .next()
            .unwrap_or("");
        assert!(
            activate.contains("load_deployment_spec(fabric, resource)"),
            "cutover must load slug/kind/port from the stored Deployment spec"
        );
        assert!(
            activate.contains("console_host_from_specs(fabric)"),
            "cutover must load console host from the stored route spec"
        );
        assert!(
            !activate.contains("body.slug")
                && !activate.contains("body.kind")
                && !activate.contains("body.port")
                && !activate.contains("body.console_host"),
            "activate must not carry realization fields on a journal body"
        );
        assert!(
            activate.contains("spec.previous_deployment_id"),
            "cutover must load the predecessor from the stored Deployment spec"
        );
        assert!(
            !activate.contains("body.previous_deployment_id"),
            "activate must not carry previous_deployment_id on a journal body"
        );
        assert!(
            activate.contains("format!(\"{service_name}:{port}\")"),
            "cutover must bind the Fabric Service name, not a ClusterIP"
        );
        assert!(
            !activate.contains("service_cluster_ip"),
            "sqlite gateway_routes must keep the Service name"
        );
    }

    #[test]
    fn gateway_caddyfile_dials_cluster_ip() {
        let src = include_str!("product.rs");
        let apply = src
            .split("async fn apply_gateway_config")
            .nth(1)
            .unwrap_or("");
        let apply = apply
            .split("async fn realize_gateway_routes")
            .next()
            .unwrap_or("");
        assert!(
            apply.contains("dataplane_caddyfile().await"),
            "Caddy reverse_proxy must dial Service ClusterIP"
        );
        assert!(
            !apply.contains("rendered_caddyfile()"),
            "on-disk Caddyfile must not dial CoreDNS Service names"
        );
    }

    #[test]
    fn gateway_reload_uses_mounted_caddyfile() {
        let src = include_str!("realize.rs");
        assert!(
            src.contains("/etc/caddy/Caddyfile"),
            "reload must use the ConfigMap mount, not stdin"
        );
        assert!(
            src.contains("caddy_proxy_dials"),
            "reload must wait for every ClusterIP dial, not the first leftover route"
        );
        assert!(
            !src.contains("\"--config\",\n            \"-\""),
            "stdin --config - can succeed without replacing the live listener"
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

    #[test]
    fn traffic_wire_names_the_environment_not_the_target() {
        use crate::specs::traffic::TrafficSpec;
        let environment = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
        let desired = Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
        let spec = TrafficSpec {
            revision: 4,
            slug: "invoice-demo".into(),
            kind: "dev".into(),
            desired_deployment_id: Some(desired),
        };
        let pending = super::traffic_wire(environment, &spec, None);
        assert_eq!(pending["resourceId"], environment);
        assert!(pending["observedDeploymentId"].is_null());
        assert_eq!(pending["observedRevision"], 0);
        assert_eq!(pending["state"], "pending");
        let live = super::traffic_wire(environment, &spec, Some(desired));
        assert_eq!(live["resourceId"], environment);
        assert_eq!(live["observedDeploymentId"], desired.to_string());
        assert_eq!(live["observedRevision"], 4);
        assert_eq!(live["state"], "active");
        let absent = TrafficSpec {
            desired_deployment_id: None,
            ..spec
        };
        let retired = super::traffic_wire(environment, &absent, None);
        assert_eq!(retired["resourceId"], environment);
        assert!(retired["observedDeploymentId"].is_null());
        assert_eq!(retired["state"], "absent");
        assert_eq!(retired["observedRevision"], 4);
    }

    #[test]
    fn equal_revision_route_heal_requires_gateway_ready() {
        assert!(
            super::gateway_pod_is_realized("Running", true, false),
            "Ready Running gateway counts as present"
        );
        assert!(
            !super::gateway_pod_is_realized("Running", false, false),
            "Running but not Ready is not equal-revision convergence"
        );
        assert!(!super::gateway_pod_is_realized("Pending", true, false));
        assert!(!super::gateway_pod_is_realized("Running", true, true));
    }

    #[test]
    fn runtime_loop_heals_routes_and_traffic() {
        let src = include_str!("main.rs");
        assert!(
            src.contains("reconcile_runtime_edge"),
            "the 15s loop must realize stored route and traffic specs, not only the host edge"
        );
        assert!(
            src.contains("reconcile_accepted_specs"),
            "WaitPod typed specs must keep reconciling after PUT returns"
        );
        assert!(
            src.contains("tokio::join!"),
            "route heal must not delay typed-spec WaitPod convergence"
        );
        assert!(
            include_str!("lib.rs").contains("try_reconcile_workspace"),
            "GET must record WaitPod convergence when the lifecycle lock is free"
        );
        let edge = include_str!("product.rs");
        let edge = edge
            .split("pub async fn reconcile_runtime_edge")
            .nth(1)
            .unwrap_or("");
        let edge = edge
            .split("pub(crate) async fn reconcile_accepted_routes")
            .next()
            .unwrap_or("");
        assert!(
            edge.contains("routes.and(traffic)"),
            "a route heal failure must still run traffic spec reconcile"
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
