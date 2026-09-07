//! Database desired-state planner. Security profile is desired state, not a
//! special `database/secure` operation. Roles come from PostgreSQL observation,
//! never from a guest marker.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseDesired {
    Present { security_profile: u32 },
    Suspended,
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabasePod {
    Absent,
    Pending,
    Running,
    Ready,
    Unknown { stale_ready: bool },
    Failed,
}

impl Default for DatabasePod {
    fn default() -> Self {
        Self::Absent
    }
}

/// Authoritative role contract observed inside the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseRoles {
    Unobserved,
    Matches { security_profile: u32 },
    Mismatch { security_profile: u32 },
}

impl Default for DatabaseRoles {
    fn default() -> Self {
        Self::Unobserved
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RestoreCandidate {
    pub present: bool,
    pub verified: bool,
    pub ambiguous: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DatabaseLocal {
    pub materialized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DatabaseObserved {
    pub lv: bool,
    pub mapper: bool,
    pub pv: bool,
    pub pod: DatabasePod,
    pub roles: DatabaseRoles,
    pub candidate: RestoreCandidate,
    /// Positive blkid "no signature". Unknown/foreign is false so we never
    /// format tenant bytes we could not classify.
    pub unformatted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseAction {
    AllocateLv,
    EnsureMapper,
    CreatePv,
    CreatePod,
    ReplacePod,
    /// Materialized LV exists and carries no filesystem. Format before
    /// attaching; never treat an empty device as PostgreSQL bytes.
    EnsureFilesystem,
    ObserveRoles,
    RestartWithCurrentPostgresProfile,
    MaterializeCandidate,
    DiscardCandidate,
    PromoteCandidate,
    RemovePod,
    RemovePv,
    RemoveMapper,
    RemoveLv,
    /// Pod exists and is progressing. The executor does not mutate; the
    /// loop returns reconciling until kubelet Ready.
    WaitPod,
    /// Durable Database bytes existed and the LV is gone. Never mint an
    /// empty replacement. Recovery is an explicit restore.
    Lost,
    Converged,
}

pub fn plan_database(
    desired: DatabaseDesired,
    local: DatabaseLocal,
    observed: DatabaseObserved,
) -> DatabaseAction {
    if observed.candidate.ambiguous {
        return DatabaseAction::DiscardCandidate;
    }
    match desired {
        DatabaseDesired::Present { security_profile } => {
            plan_present(security_profile, local, observed)
        }
        DatabaseDesired::Suspended => plan_suspended(local, observed),
        DatabaseDesired::Absent => plan_absent(local, observed),
    }
}

fn plan_present(
    security_profile: u32,
    local: DatabaseLocal,
    observed: DatabaseObserved,
) -> DatabaseAction {
    if observed.candidate.present && observed.candidate.verified {
        return DatabaseAction::PromoteCandidate;
    }
    if observed.candidate.present {
        return DatabaseAction::MaterializeCandidate;
    }
    if !observed.lv {
        if local.materialized {
            return DatabaseAction::Lost;
        }
        return DatabaseAction::AllocateLv;
    }
    if observed.unformatted {
        return DatabaseAction::EnsureFilesystem;
    }
    if matches!(
        observed.pod,
        DatabasePod::Unknown { stale_ready: true } | DatabasePod::Failed
    ) {
        return DatabaseAction::ReplacePod;
    }
    if !observed.mapper {
        return DatabaseAction::EnsureMapper;
    }
    if !observed.pv {
        return DatabaseAction::CreatePv;
    }
    match observed.pod {
        DatabasePod::Ready => plan_ready_roles(security_profile, observed.roles),
        DatabasePod::Unknown { stale_ready: false } => DatabaseAction::ReplacePod,
        DatabasePod::Pending | DatabasePod::Running => DatabaseAction::WaitPod,
        DatabasePod::Absent => DatabaseAction::CreatePod,
        DatabasePod::Failed | DatabasePod::Unknown { stale_ready: true } => {
            DatabaseAction::ReplacePod
        }
    }
}

fn plan_ready_roles(security_profile: u32, roles: DatabaseRoles) -> DatabaseAction {
    match roles {
        DatabaseRoles::Unobserved => DatabaseAction::ObserveRoles,
        DatabaseRoles::Matches {
            security_profile: got,
        } if got == security_profile => DatabaseAction::Converged,
        DatabaseRoles::Matches { .. } | DatabaseRoles::Mismatch { .. } => {
            DatabaseAction::RestartWithCurrentPostgresProfile
        }
    }
}

fn plan_suspended(local: DatabaseLocal, observed: DatabaseObserved) -> DatabaseAction {
    if observed.candidate.present {
        return DatabaseAction::DiscardCandidate;
    }
    if observed.pod != DatabasePod::Absent {
        return DatabaseAction::RemovePod;
    }
    if !observed.lv {
        if local.materialized {
            return DatabaseAction::Lost;
        }
        return DatabaseAction::AllocateLv;
    }
    if observed.pv {
        return DatabaseAction::RemovePv;
    }
    DatabaseAction::Converged
}

fn plan_absent(local: DatabaseLocal, observed: DatabaseObserved) -> DatabaseAction {
    if observed.candidate.present {
        return DatabaseAction::DiscardCandidate;
    }
    if observed.pod != DatabasePod::Absent {
        return DatabaseAction::RemovePod;
    }
    if observed.pv {
        return DatabaseAction::RemovePv;
    }
    if observed.lv || local.materialized {
        return DatabaseAction::RemoveLv;
    }
    if observed.mapper {
        return DatabaseAction::RemoveMapper;
    }
    DatabaseAction::Converged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present(profile: u32) -> DatabaseDesired {
        DatabaseDesired::Present {
            security_profile: profile,
        }
    }

    fn obs(
        lv: bool,
        pv: bool,
        pod: DatabasePod,
        roles: DatabaseRoles,
        candidate: RestoreCandidate,
    ) -> (DatabaseLocal, DatabaseObserved) {
        (
            DatabaseLocal { materialized: lv },
            DatabaseObserved {
                lv,
                mapper: lv,
                pv,
                pod,
                roles,
                candidate,
                unformatted: false,
            },
        )
    }

    fn assert_plan(
        desired: DatabaseDesired,
        lv: bool,
        pv: bool,
        pod: DatabasePod,
        roles: DatabaseRoles,
        candidate: RestoreCandidate,
        want: DatabaseAction,
    ) {
        let (local, observed) = obs(lv, pv, pod, roles, candidate);
        assert_eq!(
            plan_database(desired, local, observed),
            want,
            "desired={desired:?} lv={lv} pv={pv} pod={pod:?} roles={roles:?} candidate={candidate:?}"
        );
    }

    #[test]
    fn present_runtime_matrix() {
        let none = RestoreCandidate::default();
        assert_plan(
            present(2),
            false,
            false,
            DatabasePod::Absent,
            DatabaseRoles::Unobserved,
            none,
            DatabaseAction::AllocateLv,
        );
        assert_plan(
            present(2),
            true,
            false,
            DatabasePod::Absent,
            DatabaseRoles::Unobserved,
            none,
            DatabaseAction::CreatePv,
        );
        assert_plan(
            present(2),
            true,
            true,
            DatabasePod::Absent,
            DatabaseRoles::Unobserved,
            none,
            DatabaseAction::CreatePod,
        );
        assert_plan(
            present(2),
            true,
            true,
            DatabasePod::Pending,
            DatabaseRoles::Unobserved,
            none,
            DatabaseAction::WaitPod,
        );
        assert_plan(
            present(2),
            true,
            true,
            DatabasePod::Running,
            DatabaseRoles::Unobserved,
            none,
            DatabaseAction::WaitPod,
        );
        assert_plan(
            present(2),
            true,
            true,
            DatabasePod::Failed,
            DatabaseRoles::Unobserved,
            none,
            DatabaseAction::ReplacePod,
        );
        assert_plan(
            present(2),
            true,
            true,
            DatabasePod::Unknown { stale_ready: true },
            DatabaseRoles::Unobserved,
            none,
            DatabaseAction::ReplacePod,
        );
        assert_plan(
            present(2),
            true,
            true,
            DatabasePod::Ready,
            DatabaseRoles::Unobserved,
            none,
            DatabaseAction::ObserveRoles,
        );
        assert_plan(
            present(2),
            true,
            true,
            DatabasePod::Ready,
            DatabaseRoles::Matches {
                security_profile: 2,
            },
            none,
            DatabaseAction::Converged,
        );
    }

    #[test]
    fn security_profile_is_reconciled_not_journaled() {
        let none = RestoreCandidate::default();
        assert_plan(
            present(2),
            true,
            true,
            DatabasePod::Ready,
            DatabaseRoles::Mismatch {
                security_profile: 1,
            },
            none,
            DatabaseAction::RestartWithCurrentPostgresProfile,
        );
        assert_plan(
            present(2),
            true,
            true,
            DatabasePod::Ready,
            DatabaseRoles::Matches {
                security_profile: 1,
            },
            none,
            DatabaseAction::RestartWithCurrentPostgresProfile,
        );
    }

    #[test]
    fn reboot_pod_absent_lv_present_creates_pod() {
        let none = RestoreCandidate::default();
        assert_plan(
            present(2),
            true,
            true,
            DatabasePod::Absent,
            DatabaseRoles::Unobserved,
            none,
            DatabaseAction::CreatePod,
        );
    }

    #[test]
    fn ambiguous_candidate_is_discarded() {
        let candidate = RestoreCandidate {
            present: true,
            verified: false,
            ambiguous: true,
        };
        assert_plan(
            present(2),
            true,
            true,
            DatabasePod::Ready,
            DatabaseRoles::Matches {
                security_profile: 2,
            },
            candidate,
            DatabaseAction::DiscardCandidate,
        );
    }

    #[test]
    fn verified_candidate_is_promoted() {
        let candidate = RestoreCandidate {
            present: true,
            verified: true,
            ambiguous: false,
        };
        assert_plan(
            present(2),
            true,
            true,
            DatabasePod::Ready,
            DatabaseRoles::Matches {
                security_profile: 2,
            },
            candidate,
            DatabaseAction::PromoteCandidate,
        );
    }

    #[test]
    fn unverified_candidate_keeps_materializing() {
        let candidate = RestoreCandidate {
            present: true,
            verified: false,
            ambiguous: false,
        };
        assert_plan(
            present(2),
            true,
            true,
            DatabasePod::Ready,
            DatabaseRoles::Matches {
                security_profile: 2,
            },
            candidate,
            DatabaseAction::MaterializeCandidate,
        );
    }

    #[test]
    fn suspended_keeps_lv() {
        let none = RestoreCandidate::default();
        assert_plan(
            DatabaseDesired::Suspended,
            true,
            true,
            DatabasePod::Ready,
            DatabaseRoles::Unobserved,
            none,
            DatabaseAction::RemovePod,
        );
        assert_plan(
            DatabaseDesired::Suspended,
            true,
            false,
            DatabasePod::Absent,
            DatabaseRoles::Unobserved,
            none,
            DatabaseAction::Converged,
        );
    }

    #[test]
    fn reboot_loop_creates_missing_pod_then_waits() {
        let mut pod = DatabasePod::Absent;
        let mut actions = Vec::new();
        for _ in 0..4 {
            let (local, mut observed) = obs(
                true,
                true,
                pod,
                DatabaseRoles::Unobserved,
                RestoreCandidate::default(),
            );
            observed.mapper = true;
            let action = plan_database(present(2), local, observed);
            actions.push(action);
            match action {
                DatabaseAction::CreatePod => pod = DatabasePod::Pending,
                DatabaseAction::WaitPod | DatabaseAction::Converged => break,
                other => panic!("unexpected reboot action {other:?}"),
            }
        }
        assert_eq!(
            actions,
            [DatabaseAction::CreatePod, DatabaseAction::WaitPod,]
        );
    }

    #[test]
    fn absent_matrix() {
        let none = RestoreCandidate::default();
        assert_plan(
            DatabaseDesired::Absent,
            true,
            true,
            DatabasePod::Ready,
            DatabaseRoles::Unobserved,
            none,
            DatabaseAction::RemovePod,
        );
        assert_plan(
            DatabaseDesired::Absent,
            false,
            false,
            DatabasePod::Absent,
            DatabaseRoles::Unobserved,
            none,
            DatabaseAction::Converged,
        );
        assert_plan(
            DatabaseDesired::Absent,
            true,
            false,
            DatabasePod::Absent,
            DatabaseRoles::Unobserved,
            none,
            DatabaseAction::RemoveLv,
        );
        let leftover = DatabaseObserved {
            lv: false,
            mapper: false,
            pv: false,
            pod: DatabasePod::Absent,
            roles: DatabaseRoles::Unobserved,
            candidate: none,
            unformatted: false,
        };
        assert_eq!(
            plan_database(
                DatabaseDesired::Absent,
                DatabaseLocal { materialized: true },
                leftover
            ),
            DatabaseAction::RemoveLv,
            "a leftover allocation without an LV is still Fabric residue"
        );
    }

    #[test]
    fn materialized_missing_lv_is_lost() {
        let none = RestoreCandidate::default();
        let (local, observed) = (
            DatabaseLocal { materialized: true },
            DatabaseObserved {
                lv: false,
                mapper: false,
                pv: false,
                pod: DatabasePod::Absent,
                roles: DatabaseRoles::Unobserved,
                candidate: none,
                unformatted: false,
            },
        );
        assert_eq!(
            plan_database(present(2), local, observed),
            DatabaseAction::Lost
        );
        assert_eq!(
            plan_database(DatabaseDesired::Suspended, local, observed),
            DatabaseAction::Lost
        );
        assert_ne!(
            plan_database(present(2), local, observed),
            DatabaseAction::AllocateLv
        );
        let never = DatabaseLocal {
            materialized: false,
        };
        assert_eq!(
            plan_database(present(2), never, observed),
            DatabaseAction::AllocateLv
        );
    }

    #[test]
    fn unformatted_materialized_lv_is_formatted_not_waited() {
        let none = RestoreCandidate::default();
        let observed = DatabaseObserved {
            lv: true,
            mapper: true,
            pv: true,
            pod: DatabasePod::Running,
            roles: DatabaseRoles::Unobserved,
            candidate: none,
            unformatted: true,
        };
        assert_eq!(
            plan_database(present(2), DatabaseLocal { materialized: true }, observed),
            DatabaseAction::EnsureFilesystem
        );
        let formatted = DatabaseObserved {
            unformatted: false,
            ..observed
        };
        assert_eq!(
            plan_database(present(2), DatabaseLocal { materialized: true }, formatted),
            DatabaseAction::WaitPod
        );
    }
}
