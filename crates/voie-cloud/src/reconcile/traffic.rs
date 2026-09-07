//! Environment traffic target. PostgreSQL desired first; Fabric realizes.

use std::time::Duration;

use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

use crate::applications::ApplicationStore;
use crate::fabric_client::ProductOutcome;
use crate::http::Platform;
use crate::reconcile::fabric_revision_caught_up;

/// Fabric observation is the only traffic proof. `resourceId` is the
/// Environment, never a substitute for the live selector.
pub fn fabric_traffic_settled(
    outcome: &ProductOutcome,
    desired: Option<Uuid>,
    environment_revision: i64,
) -> bool {
    outcome.observed_deployment_id == desired
        && fabric_revision_caught_up(outcome.observed_revision, environment_revision.max(1))
}

pub async fn reconcile_due(platform: &Platform) {
    let Ok(rows) = sqlx::query(
        "select id from application_environments \
         where desired_deployment_id is distinct from observed_deployment_id \
            or (desired_deployment_id is not null \
                and traffic_observed_revision < greatest(revision, 1)) \
            or (desired_deployment_id is null \
                and revision >= 1 \
                and traffic_observed_revision < revision) \
         order by revision, id \
         limit 32",
    )
    .fetch_all(platform.applications.pool())
    .await
    else {
        return;
    };
    for row in rows {
        let id: Uuid = row.get("id");
        put_due_environment(platform, id).await;
    }
}

pub async fn put_due_environment(platform: &Platform, environment_id: Uuid) {
    let Ok(Some(environment)) =
        crate::applications::load_environment(platform.applications.pool(), environment_id).await
    else {
        return;
    };
    if environment.revision < 1
        && environment.desired_deployment_id.is_none()
        && environment.observed_deployment_id.is_none()
    {
        return;
    }
    let Some(runtime) = platform.runtime.as_ref() else {
        settle_without_fabric(platform, &environment).await;
        return;
    };
    let Some(body) = traffic_spec_body(platform, &environment).await else {
        return;
    };
    if let Some(desired) = environment.desired_deployment_id {
        let Ok(deployment) = platform.deployments.get_internal(desired).await else {
            return;
        };
        if !deployment.proven {
            return;
        }
    }
    match runtime.fabric.put_traffic_spec(environment_id, &body).await {
        Ok(outcome)
            if fabric_traffic_settled(
                &outcome,
                environment.desired_deployment_id,
                environment.revision,
            ) =>
        {
            settle_from_outcome(platform, &environment, &outcome).await;
        }
        Ok(_) | Err(_) => {}
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

pub async fn traffic_spec_body(
    platform: &Platform,
    environment: &crate::applications::Environment,
) -> Option<Value> {
    let application = ApplicationStore::new(platform.applications.pool().clone(), String::new())
        .get_internal(environment.application_id)
        .await
        .ok()?;
    Some(json!({
        "revision": environment.revision.max(1),
        "slug": application.slug,
        "kind": environment.kind,
        "desiredDeploymentId": environment.desired_deployment_id,
    }))
}

async fn settle_from_outcome(
    platform: &Platform,
    environment: &crate::applications::Environment,
    outcome: &ProductOutcome,
) {
    let revision = outcome
        .observed_revision
        .unwrap_or(environment.revision.max(1));
    match environment.desired_deployment_id {
        Some(desired) => {
            let _ = platform
                .deployments
                .settle_observed_traffic_at(desired, Some(revision))
                .await;
            platform.kick_route_map();
        }
        None => {
            let _ = platform
                .deployments
                .settle_observed_absent(environment.id, revision)
                .await;
            platform.kick_route_map();
        }
    }
}

async fn settle_without_fabric(
    platform: &Platform,
    environment: &crate::applications::Environment,
) {
    match environment.desired_deployment_id {
        Some(desired) => {
            let _ = platform.deployments.settle_observed_traffic(desired).await;
        }
        None if environment.revision >= 1 => {
            let _ = platform
                .deployments
                .settle_observed_absent(environment.id, environment.revision)
                .await;
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::fabric_traffic_settled;
    use crate::fabric_client::ProductOutcome;
    use uuid::Uuid;

    fn outcome(
        resource_id: &str,
        observed: Option<Uuid>,
        observed_revision: Option<i64>,
    ) -> ProductOutcome {
        ProductOutcome {
            state: "pending".into(),
            resource_id: resource_id.into(),
            operation_id: None,
            desired_revision: Some(4),
            observed_revision,
            last_error_code: None,
            allocated_bytes: None,
            observed_pod_generation: None,
            observed_deployment_id: observed,
        }
    }

    #[test]
    fn resource_id_is_not_selector_proof() {
        let desired = Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
        let pending = outcome(&desired.to_string(), None, Some(0));
        assert!(
            !fabric_traffic_settled(&pending, Some(desired), 4),
            "Fabric resourceId is the Environment, not observed traffic"
        );
        let live = outcome(
            "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
            Some(desired),
            Some(4),
        );
        assert!(fabric_traffic_settled(&live, Some(desired), 4));
        assert!(!fabric_traffic_settled(&live, Some(desired), 5));
    }

    #[test]
    fn absent_settles_only_when_observed_is_none() {
        let leftover = Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
        let still_live = outcome("env", Some(leftover), Some(0));
        assert!(!fabric_traffic_settled(&still_live, None, 5));
        let gone = outcome("env", None, Some(5));
        assert!(fabric_traffic_settled(&gone, None, 5));
    }
}
