//! Workspace desired-state wake. Fabric owns realization.

use std::time::Duration;

use sqlx::Row;
use uuid::Uuid;

use crate::fabric_client::FabricError;
use crate::http::Platform;
use crate::reconcile::{
    OBSERVE_AFTER_SECS, OBSERVE_RETRY_SECS, fabric_reported_revision, fabric_revision_caught_up,
    observed_satisfies_desired,
};

pub async fn reconcile_due(platform: &Platform) {
    let _ = platform
        .applications
        .retire_identities_on_deleted_workspaces()
        .await;
    let Ok(rows) = sqlx::query(
        "select id, desired_state, desired_revision, observed_revision, observed_state \
         from workspaces \
         where desired_revision > observed_revision \
            or (desired_state <> 'deleted' \
                and reconcile_after is not null \
                and reconcile_after <= now()) \
         order by case when desired_state = 'deleted' then 1 else 0 end, created_at, id \
         limit 32",
    )
    .fetch_all(platform.applications.pool())
    .await
    else {
        return;
    };
    for row in rows {
        let id: Uuid = row.get("id");
        let desired_revision: i64 = row.get("desired_revision");
        let observed_revision: i64 = row.get("observed_revision");
        let desired_state: String = row.get("desired_state");
        if desired_revision > observed_revision {
            put_workspace_spec(platform, id, desired_revision, &desired_state).await;
        } else {
            observe_workspace_status(platform, id).await;
        }
    }
}

/// First realization and later healing use the same PUT spec path.
pub async fn put_due_workspace(platform: &Platform, id: Uuid) {
    let Ok(row) = sqlx::query(
        "select desired_state, desired_revision, observed_revision from workspaces where id = $1",
    )
    .bind(id)
    .fetch_optional(platform.applications.pool())
    .await
    else {
        return;
    };
    let Some(row) = row else {
        return;
    };
    let desired: String = row.get("desired_state");
    let desired_revision: i64 = row.get("desired_revision");
    let observed_revision: i64 = row.get("observed_revision");
    if desired_revision > observed_revision {
        put_workspace_spec(platform, id, desired_revision, &desired).await;
    } else {
        observe_workspace_status(platform, id).await;
    }
}

async fn put_workspace_spec(platform: &Platform, id: Uuid, revision: i64, desired: &str) {
    let Some(runtime) = platform.runtime.as_ref() else {
        return;
    };
    let allocated: i64 = sqlx::query_scalar("select allocated_bytes from workspaces where id = $1")
        .bind(id)
        .fetch_one(platform.applications.pool())
        .await
        .unwrap_or(crate::storage::WORKSPACE_BYTES);
    let tier: String = sqlx::query_scalar("select storage_tier from workspaces where id = $1")
        .bind(id)
        .fetch_one(platform.applications.pool())
        .await
        .unwrap_or_else(|_| crate::storage::workspace_tier_for_bytes(allocated).to_owned());
    let body = serde_json::json!({
        "revision": revision,
        "desired": desired,
        "runtimeProfile": "workspace-v1",
        "storageTier": tier,
    });
    match runtime.fabric.put_workspace_spec(id, &body).await {
        Ok(outcome) => apply_workspace_outcome(platform, id, &outcome).await,
        Err(FabricError::Transport) => {
            record_workspace_observe_failure(
                platform,
                id,
                "fabric_unreachable",
                OBSERVE_RETRY_SECS,
            )
            .await;
        }
        Err(FabricError::Capacity) => {
            record_workspace_observe_failure(platform, id, "fabric_capacity", OBSERVE_RETRY_SECS)
                .await;
        }
        Err(_) => {
            record_workspace_observe_failure(platform, id, "fabric_put_failed", OBSERVE_RETRY_SECS)
                .await;
        }
    }
}

async fn observe_workspace_status(platform: &Platform, id: Uuid) {
    let Some(runtime) = platform.runtime.as_ref() else {
        return;
    };
    match runtime
        .fabric
        .product_get(&format!("/v1/workspaces/{id}"))
        .await
    {
        Ok(outcome) => {
            let desired_revision: i64 =
                sqlx::query_scalar("select desired_revision from workspaces where id = $1")
                    .bind(id)
                    .fetch_one(platform.applications.pool())
                    .await
                    .unwrap_or(0);
            apply_workspace_outcome(platform, id, &outcome).await;
            if outcome.state != "lost"
                && !fabric_revision_caught_up(outcome.observed_revision, desired_revision)
            {
                put_workspace_spec_now(platform, id).await;
            }
        }
        Err(FabricError::Transport) => {
            record_workspace_observe_failure(
                platform,
                id,
                "fabric_unreachable",
                OBSERVE_RETRY_SECS,
            )
            .await;
        }
        Err(_) => match runtime.fabric.get_workspace(id).await {
            Ok(None) => {
                settle_unbound_or_reput(platform, id).await;
            }
            Ok(Some(_)) => {
                // Legacy probe has a state and no spec revision. Re-PUT.
                record_workspace_observe_failure(
                    platform,
                    id,
                    "fabric_revision_unproven",
                    OBSERVE_RETRY_SECS,
                )
                .await;
                put_workspace_spec_now(platform, id).await;
            }
            Err(FabricError::Transport) => {
                record_workspace_observe_failure(
                    platform,
                    id,
                    "fabric_unreachable",
                    OBSERVE_RETRY_SECS,
                )
                .await;
            }
            Err(_) => {
                record_workspace_observe_failure(
                    platform,
                    id,
                    "fabric_observe_failed",
                    OBSERVE_RETRY_SECS,
                )
                .await;
            }
        },
    }
}

async fn settle_unbound_or_reput(platform: &Platform, id: Uuid) {
    let desired: String = sqlx::query_scalar("select desired_state from workspaces where id = $1")
        .bind(id)
        .fetch_one(platform.applications.pool())
        .await
        .unwrap_or_default();
    if unbound_active_settles_on_fabric_absent(&desired, "deleted") {
        let live: bool = sqlx::query_scalar(
            "select exists(select 1 from applications \
             where workspace_id = $1 and state not in ('deleted', 'deleting'))",
        )
        .bind(id)
        .fetch_one(platform.applications.pool())
        .await
        .unwrap_or(true);
        if !live {
            let _ = sqlx::query(
                "update workspaces set desired_state = 'deleted', \
                 desired_revision = case \
                     when desired_state = 'deleted' then desired_revision \
                     else desired_revision + 1 \
                 end, \
                 last_error_code = null, \
                 reconcile_after = now() \
                 where id = $1 and state <> 'fenced' \
                   and not exists ( \
                       select 1 from applications \
                       where workspace_id = $1 \
                         and state not in ('deleted', 'deleting') \
                   )",
            )
            .bind(id)
            .execute(platform.applications.pool())
            .await;
            return;
        }
    }
    if desired == "deleted" {
        put_workspace_spec_now(platform, id).await;
        return;
    }
    record_workspace_observe_failure(platform, id, "fabric_revision_unproven", OBSERVE_RETRY_SECS)
        .await;
}

async fn put_workspace_spec_now(platform: &Platform, id: Uuid) {
    let Ok(Some(row)) =
        sqlx::query("select desired_state, desired_revision from workspaces where id = $1")
            .bind(id)
            .fetch_optional(platform.applications.pool())
            .await
    else {
        return;
    };
    let desired: String = row.get("desired_state");
    let revision: i64 = row.get("desired_revision");
    Box::pin(put_workspace_spec(platform, id, revision, &desired)).await;
}

async fn apply_workspace_outcome(
    platform: &Platform,
    id: Uuid,
    outcome: &crate::fabric_client::ProductOutcome,
) {
    sync_workspace_bytes(platform, id, outcome.allocated_bytes).await;
    let desired_revision: i64 =
        sqlx::query_scalar("select desired_revision from workspaces where id = $1")
            .bind(id)
            .fetch_one(platform.applications.pool())
            .await
            .unwrap_or(0);
    let fabric_rev = fabric_reported_revision(outcome.observed_revision);
    if outcome.state == "lost" {
        let error = outcome
            .last_error_code
            .as_deref()
            .unwrap_or("durable_volume_missing");
        persist_workspace_observation(
            platform,
            id,
            "lost",
            Some(error),
            fabric_rev,
            OBSERVE_AFTER_SECS,
            false,
        )
        .await;
        return;
    }
    let desired: String = sqlx::query_scalar("select desired_state from workspaces where id = $1")
        .bind(id)
        .fetch_one(platform.applications.pool())
        .await
        .unwrap_or_default();
    if unbound_active_settles_on_fabric_absent(&desired, &outcome.state) {
        settle_unbound_or_reput(platform, id).await;
        return;
    }
    if observed_satisfies_desired(&desired, &outcome.state)
        && fabric_revision_caught_up(outcome.observed_revision, desired_revision)
    {
        let done_deleted = desired == "deleted" && outcome.state == "deleted";
        persist_workspace_observation(
            platform,
            id,
            &outcome.state,
            None,
            fabric_rev,
            OBSERVE_AFTER_SECS,
            done_deleted,
        )
        .await;
        return;
    }
    if observed_satisfies_desired(&desired, &outcome.state) {
        persist_workspace_observation(
            platform,
            id,
            &outcome.state,
            Some("fabric_revision_unproven"),
            fabric_rev,
            OBSERVE_RETRY_SECS,
            false,
        )
        .await;
        return;
    }
    let error = outcome
        .last_error_code
        .as_deref()
        .unwrap_or("observed_not_desired");
    persist_workspace_observation(
        platform,
        id,
        &outcome.state,
        Some(error),
        fabric_rev,
        OBSERVE_RETRY_SECS,
        false,
    )
    .await;
}

async fn persist_workspace_observation(
    platform: &Platform,
    id: Uuid,
    observed_state: &str,
    error: Option<&str>,
    fabric_revision: Option<i64>,
    after_secs: i64,
    clear_reconcile: bool,
) {
    let _ = sqlx::query(
        "update workspaces set observed_state = $2, last_error_code = $3, \
         observed_revision = coalesce($4, observed_revision), \
         reconcile_after = case when $6 then null \
             else now() + ($5 * interval '1 second') end \
         where id = $1",
    )
    .bind(id)
    .bind(observed_state)
    .bind(error)
    .bind(fabric_revision)
    .bind(after_secs)
    .bind(clear_reconcile)
    .execute(platform.applications.pool())
    .await;
}

async fn sync_workspace_bytes(platform: &Platform, id: Uuid, allocated_bytes: Option<u64>) {
    let Some(bytes) = allocated_bytes.filter(|bytes| *bytes > 0) else {
        return;
    };
    let _ = sqlx::query(
        "update workspaces set allocated_bytes = $2 where id = $1 and allocated_bytes <> $2",
    )
    .bind(id)
    .bind(bytes as i64)
    .execute(platform.applications.pool())
    .await;
}

async fn record_workspace_observe_failure(
    platform: &Platform,
    id: Uuid,
    code: &str,
    after_secs: i64,
) {
    // Unreachable Fabric is not Lost. Keep observed_state; retry later.
    let _ = sqlx::query(
        "update workspaces set last_error_code = $2, \
         reconcile_after = now() + ($3 * interval '1 second') \
         where id = $1 and observed_state <> 'lost'",
    )
    .bind(id)
    .bind(code)
    .bind(after_secs)
    .execute(platform.applications.pool())
    .await;
    let _ = sqlx::query(
        "update workspaces set reconcile_after = now() + ($2 * interval '1 second') \
         where id = $1 and observed_state = 'lost'",
    )
    .bind(id)
    .bind(after_secs)
    .execute(platform.applications.pool())
    .await;
}

pub fn spawn_loop(platform: Platform) {
    tokio::spawn(async move {
        loop {
            reconcile_due(&platform).await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

/// One-shot startup heal for rows written before migration 0027. Migration
/// 0030 already moved leftover process `deleted` onto desired deleted.
pub async fn persist_deleted_desired_for_tombstones(platform: &Platform) {
    let _ = sqlx::query(
        "update workspaces \
         set desired_state = 'deleted', \
             desired_revision = desired_revision + 1, \
             reconcile_after = now() \
         where state = 'deleted' and desired_state <> 'deleted'",
    )
    .execute(platform.applications.pool())
    .await;
}

pub fn put_outcome_matches(desired: &str, observed: &str) -> bool {
    observed_satisfies_desired(desired, observed)
}

/// Fabric-only teardown left a Control `ready`/`active` row with no volume
/// and no live Application. Do not PUT `active` (that remints empty bytes).
/// Settle the Control row so quota matches Fabric absence.
pub fn unbound_active_settles_on_fabric_absent(desired: &str, observed: &str) -> bool {
    desired == "active" && matches!(observed, "deleted" | "absent")
}

#[cfg(test)]
mod tests {
    use super::{put_outcome_matches, unbound_active_settles_on_fabric_absent};

    #[test]
    fn put_outcome_accepts_each_desired_name() {
        assert!(put_outcome_matches("active", "active"));
        assert!(put_outcome_matches("active", "ready"));
        assert!(!put_outcome_matches("active", "accepted"));
        assert!(put_outcome_matches("suspended", "suspended"));
        assert!(!put_outcome_matches("suspended", "ready"));
        assert!(put_outcome_matches("archived", "archived"));
        assert!(put_outcome_matches("deleted", "deleted"));
        assert!(!put_outcome_matches("deleted", "ready"));
        assert!(!put_outcome_matches("deleted", "deleting"));
        assert!(
            !put_outcome_matches("active", "lost"),
            "lost durable bytes are not a successful converge"
        );
        assert!(!put_outcome_matches("suspended", "lost"));
    }

    #[test]
    fn fabric_absent_settles_unbound_active_without_remint() {
        assert!(unbound_active_settles_on_fabric_absent("active", "deleted"));
        assert!(unbound_active_settles_on_fabric_absent("active", "absent"));
        assert!(!unbound_active_settles_on_fabric_absent("active", "lost"));
        assert!(!unbound_active_settles_on_fabric_absent("active", "ready"));
        assert!(!unbound_active_settles_on_fabric_absent(
            "deleted", "deleted"
        ));
        assert!(!unbound_active_settles_on_fabric_absent(
            "active", "deleting"
        ));
    }
}
