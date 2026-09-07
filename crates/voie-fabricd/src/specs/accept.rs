//! Revision-monotonic acceptance for Control-authored Fabric desired specs.
//!
//! Routes are not this contract: Control and Fabric both mint route revisions.

use crate::FabricError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesiredSpecAcceptance {
    /// Incoming revision is newer. Persist and realize.
    Accept,
    /// Same revision and hash. Stored desired is unchanged.
    Idempotent,
    /// Same revision, different hash. One revision cannot name two specs.
    Conflict,
    /// Incoming revision is older. Stored desired and live realization stay.
    Stale,
}

/// Decide whether an incoming Control desired spec may replace stored truth.
pub fn desired_spec_acceptance(
    incoming_revision: i64,
    incoming_hash: &str,
    stored: Option<(i64, &str)>,
) -> DesiredSpecAcceptance {
    let Some((stored_revision, stored_hash)) = stored else {
        return DesiredSpecAcceptance::Accept;
    };
    if incoming_revision < stored_revision {
        DesiredSpecAcceptance::Stale
    } else if incoming_revision > stored_revision {
        DesiredSpecAcceptance::Accept
    } else if incoming_hash == stored_hash {
        DesiredSpecAcceptance::Idempotent
    } else {
        DesiredSpecAcceptance::Conflict
    }
}

/// Persist-path mapping: stale and conflict never write or realize.
pub fn require_spec_write(
    decision: DesiredSpecAcceptance,
) -> Result<DesiredSpecAcceptance, FabricError> {
    match decision {
        DesiredSpecAcceptance::Stale => Err(FabricError::Conflict("stale desired revision".into())),
        DesiredSpecAcceptance::Conflict => {
            Err(FabricError::Conflict("desired spec conflict".into()))
        }
        other => Ok(other),
    }
}

/// Live traffic mutation is allowed only for the currently accepted revision.
pub fn traffic_realize_applies(stored_revision: i64, applying_revision: i64) -> bool {
    stored_revision == applying_revision
}

/// One-shot Application Secret mutation is allowed only on Accept.
///
/// Environment bindings are not part of `DeploymentSpec`'s revision/hash.
/// An equal-revision retry, including one that carries different one-shot
/// bindings, is Idempotent and must not overwrite the Secret already
/// applied for that revision.
pub fn deployment_secret_bind_applies(decision: DesiredSpecAcceptance) -> bool {
    decision == DesiredSpecAcceptance::Accept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_older_revision_is_rejected() {
        assert_eq!(
            desired_spec_acceptance(4, "n", Some((5, "n-plus"))),
            DesiredSpecAcceptance::Stale
        );
    }

    #[test]
    fn equal_revision_equal_hash_is_idempotent() {
        assert_eq!(
            desired_spec_acceptance(5, "a", Some((5, "a"))),
            DesiredSpecAcceptance::Idempotent
        );
    }

    #[test]
    fn equal_revision_different_hash_is_conflict() {
        assert_eq!(
            desired_spec_acceptance(5, "b", Some((5, "a"))),
            DesiredSpecAcceptance::Conflict
        );
    }

    #[test]
    fn newer_revision_is_accepted() {
        assert_eq!(
            desired_spec_acceptance(6, "next", Some((5, "a"))),
            DesiredSpecAcceptance::Accept
        );
        assert_eq!(
            desired_spec_acceptance(1, "first", None),
            DesiredSpecAcceptance::Accept
        );
    }

    #[test]
    fn older_traffic_revision_must_not_mutate_live() {
        assert!(traffic_realize_applies(5, 5));
        assert!(!traffic_realize_applies(6, 5));
        assert!(!traffic_realize_applies(5, 4));
    }

    #[test]
    fn stale_or_idempotent_deployment_must_not_bind_secret() {
        assert!(!deployment_secret_bind_applies(
            DesiredSpecAcceptance::Stale
        ));
        assert!(!deployment_secret_bind_applies(
            DesiredSpecAcceptance::Conflict
        ));
        assert!(!deployment_secret_bind_applies(
            DesiredSpecAcceptance::Idempotent
        ));
        assert!(deployment_secret_bind_applies(
            DesiredSpecAcceptance::Accept
        ));
    }
}
