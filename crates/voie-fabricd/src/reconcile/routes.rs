//! Route-map planner. Control sends one revision; Fabric applies atomically.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteAction {
    ApplyAtomic,
    Converged,
}

pub fn plan_routes(
    desired_revision: i64,
    observed_revision: i64,
    live_present: bool,
) -> RouteAction {
    if desired_revision > observed_revision || !live_present {
        RouteAction::ApplyAtomic
    } else {
        RouteAction::Converged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_drift_applies_the_whole_map() {
        assert_eq!(plan_routes(15, 14, true), RouteAction::ApplyAtomic);
        assert_eq!(plan_routes(41, 0, true), RouteAction::ApplyAtomic);
    }

    #[test]
    fn matching_revision_is_converged_only_when_live() {
        assert_eq!(plan_routes(15, 15, true), RouteAction::Converged);
        assert_eq!(plan_routes(0, 0, true), RouteAction::Converged);
        assert_eq!(plan_routes(15, 15, false), RouteAction::ApplyAtomic);
    }
}
