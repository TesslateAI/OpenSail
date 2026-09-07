//! Derived Application route map. PostgreSQL owns intent; Fabric realizes.

use std::time::Duration;

use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

use crate::http::Platform;

/// ClusterIP Service DNS name plus port, matching Fabric `app_service_name`.
pub fn edge_service(slug: &str, kind: &str, port: u16) -> String {
    format!("app-{slug}-{kind}:{port}")
}

fn manifest_port(manifest: &Value) -> Option<u16> {
    let port = manifest
        .get("run")
        .and_then(|run| run.get("port"))
        .and_then(Value::as_u64)
        .unwrap_or(8080);
    u16::try_from(port).ok().filter(|port| *port != 0)
}

pub async fn bump_and_put(platform: &Platform) {
    let Some(fabric_id) = platform.fabric_id else {
        return;
    };
    if platform.runtime.is_none() {
        return;
    }
    let Ok(Some(revision)) = sqlx::query_scalar::<_, i64>(
        "update fabrics set desired_route_revision = desired_route_revision + 1 \
         where id = $1 returning desired_route_revision",
    )
    .bind(fabric_id)
    .fetch_optional(platform.applications.pool())
    .await
    else {
        return;
    };
    put_revision(platform, fabric_id, revision).await;
}

/// Persist `consoleHost` on Fabric before activate so the journal body does
/// not carry the edge hostname. Does not bump the Control revision.
pub async fn ensure_console_host(platform: &Platform) {
    let Some(fabric_id) = platform.fabric_id else {
        return;
    };
    if platform.runtime.is_none() {
        return;
    }
    let desired =
        sqlx::query_scalar::<_, i64>("select desired_route_revision from fabrics where id = $1")
            .bind(fabric_id)
            .fetch_optional(platform.applications.pool())
            .await
            .ok()
            .flatten()
            .unwrap_or(0);
    put_revision(platform, fabric_id, desired.max(1)).await;
}

pub async fn reconcile_due(platform: &Platform) {
    let Some(fabric_id) = platform.fabric_id else {
        return;
    };
    let Ok(Some(row)) = sqlx::query(
        "select desired_route_revision, observed_route_revision from fabrics where id = $1",
    )
    .bind(fabric_id)
    .fetch_optional(platform.applications.pool())
    .await
    else {
        return;
    };
    let desired: i64 = row.get("desired_route_revision");
    let observed: i64 = row.get("observed_route_revision");
    if desired > observed {
        put_revision(platform, fabric_id, desired).await;
    }
}

pub fn spawn_loop(platform: Platform) {
    tokio::spawn(async move {
        loop {
            reconcile_due(&platform).await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

async fn put_revision(platform: &Platform, fabric_id: Uuid, revision: i64) {
    let Some(runtime) = platform.runtime.as_ref() else {
        return;
    };
    let Ok(routes) = list_desired_routes(platform).await else {
        return;
    };
    let body = json!({
        "revision": revision.max(1),
        "consoleHost": platform.applications.console_host(),
        "routes": routes,
    });
    match runtime.fabric.put_route_map(&body).await {
        Ok(outcome) if outcome.observed_revision.unwrap_or(0) >= revision.max(1) => {
            let _ = sqlx::query(
                "update fabrics set observed_route_revision = desired_route_revision \
                 where id = $1 and desired_route_revision = $2",
            )
            .bind(fabric_id)
            .bind(revision)
            .execute(platform.applications.pool())
            .await;
        }
        Ok(_) | Err(_) => {}
    }
}

async fn list_desired_routes(platform: &Platform) -> Result<Vec<Value>, sqlx::Error> {
    let rows = sqlx::query(
        "select a.slug, e.kind, r.manifest \
         from application_environments e \
         join applications a on a.id = e.application_id \
         join application_deployments d on d.id = e.active_deployment_id \
         join application_releases r on r.id = d.release_id \
         order by a.slug, e.kind",
    )
    .fetch_all(platform.applications.pool())
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let slug: String = row.get("slug");
            let kind: String = row.get("kind");
            let manifest: Value = row.get("manifest");
            let port = manifest_port(&manifest)?;
            Some(json!({
                "slug": slug,
                "kind": kind,
                "service": edge_service(&slug, &kind, port),
            }))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::edge_service;

    #[test]
    fn edge_service_is_the_fabric_service_name() {
        assert_eq!(
            edge_service("invoice-demo", "dev", 3000),
            "app-invoice-demo-dev:3000"
        );
        assert_eq!(
            edge_service("invoice-demo", "prod", 8080),
            "app-invoice-demo-prod:8080"
        );
    }

    #[test]
    fn omitted_manifest_port_defaults_to_8080() {
        let manifest = serde_json::json!({ "run": { "command": ["python3", "-m", "app"] } });
        assert_eq!(super::manifest_port(&manifest), Some(8080));
        let zero = serde_json::json!({ "run": { "port": 0 } });
        assert_eq!(super::manifest_port(&zero), None);
    }
}
