//! Control-plane reconciliation kernel. PostgreSQL rows are the durable queue.

pub mod database;
pub mod deployment;
pub mod release;
pub mod routes;
pub mod traffic;
pub mod workspace;

/// Release 0 estate is small. Converged resources are re-observed on this
/// interval. Desired-ahead work stays on `desired_revision > observed_revision`.
pub const OBSERVE_AFTER_SECS: i64 = 15;
/// Fabric unreachable is retried sooner than a healthy observe cadence.
pub const OBSERVE_RETRY_SECS: i64 = 5;

/// Revision equality is not health. Lost, stream-needed, and failed observed
/// states never satisfy a desired present/active/running resource.
pub fn observed_satisfies_desired(desired: &str, observed: &str) -> bool {
    if observed == "lost" || observed == "needs_release_stream" || observed == "failed" {
        return false;
    }
    match desired {
        "active" => observed == "active" || observed == "ready",
        "present" => observed == "present" || observed == "ready",
        "running" => observed == "running",
        "suspended" | "archived" | "deleted" | "stopped" | "absent" => observed == desired,
        _ => false,
    }
}

/// Persist only a revision Fabric named. Never copy Control's desired revision.
pub fn fabric_reported_revision(reported: Option<i64>) -> Option<i64> {
    reported.filter(|&got| got >= 0)
}

/// Convergence requires Fabric to name a revision at least as new as Control desired.
pub fn fabric_revision_caught_up(reported: Option<i64>, desired: i64) -> bool {
    reported.is_some_and(|got| got >= desired)
}

/// GET 404 heals a live Database by putting current desired. Teardown must
/// not remint: Application delete purges the Fabric journal, and a heal PUT
/// of `present` recreates it.
pub fn should_heal_missing_database_spec(
    desired: &str,
    observed: &str,
    application_state: &str,
) -> bool {
    observed != "lost"
        && desired != "absent"
        && !matches!(
            application_state,
            "deleting" | "deleted" | "archiving" | "archived"
        )
}

#[cfg(test)]
mod tests {
    use super::{
        fabric_reported_revision, fabric_revision_caught_up, observed_satisfies_desired,
        should_heal_missing_database_spec,
    };

    #[test]
    fn revision_equal_lost_is_not_healthy() {
        assert!(!observed_satisfies_desired("active", "lost"));
        assert!(!observed_satisfies_desired("present", "lost"));
        assert!(!observed_satisfies_desired(
            "running",
            "needs_release_stream"
        ));
        assert!(observed_satisfies_desired("active", "ready"));
        assert!(observed_satisfies_desired("active", "active"));
        assert!(observed_satisfies_desired("present", "ready"));
        assert!(observed_satisfies_desired("running", "running"));
        assert!(observed_satisfies_desired("suspended", "suspended"));
        assert!(!observed_satisfies_desired("active", "accepted"));
        assert!(!observed_satisfies_desired("running", "lost"));
    }

    #[test]
    fn control_does_not_invent_an_observed_revision() {
        assert_eq!(fabric_reported_revision(None), None);
        assert_eq!(fabric_reported_revision(Some(6)), Some(6));
        assert!(!fabric_revision_caught_up(None, 7));
        assert!(!fabric_revision_caught_up(Some(6), 7));
        assert!(fabric_revision_caught_up(Some(7), 7));
        assert!(fabric_revision_caught_up(Some(8), 7));
    }

    #[test]
    fn missing_spec_is_not_healed_during_application_teardown() {
        assert!(should_heal_missing_database_spec(
            "present", "accepted", "ready"
        ));
        assert!(!should_heal_missing_database_spec(
            "present", "accepted", "deleting"
        ));
        assert!(!should_heal_missing_database_spec(
            "present", "accepted", "archived"
        ));
        assert!(!should_heal_missing_database_spec(
            "absent", "accepted", "ready"
        ));
        assert!(!should_heal_missing_database_spec(
            "present", "lost", "ready"
        ));
    }
}
