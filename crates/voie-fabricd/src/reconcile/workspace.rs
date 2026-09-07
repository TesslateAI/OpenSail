//! Workspace desired-state planner. One next action; the reconciler loops.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceDesired {
    Active,
    Suspended,
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspacePod {
    Absent,
    Pending,
    Running,
    Ready,
    /// Substrate could not classify the Pod. `stale_ready` is a leftover Ready
    /// condition that must not be trusted as convergence.
    Unknown {
        stale_ready: bool,
    },
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorkspaceLocal {
    /// SQLite `volume_allocations.state = allocated` after a successful
    /// prepare. A reserved-but-unprepared row is still never-materialized
    /// so a crash during first `lvcreate` can retry.
    pub materialized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorkspaceObserved {
    pub lv: bool,
    pub mapper: bool,
    pub pv: bool,
    pub pod: WorkspacePod,
}

impl Default for WorkspacePod {
    fn default() -> Self {
        Self::Absent
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceAction {
    AllocateLv,
    EnsureMapper,
    CreatePv,
    CreatePod,
    ReplacePod,
    RemovePod,
    RemovePv,
    /// Planner must not emit these. `execute_workspace` refuses `RemoveLv`
    /// so a regression cannot skip residue-gated delete.
    #[allow(dead_code)]
    RemoveMapper,
    #[allow(dead_code)]
    RemoveLv,
    WaitPod,
    /// Durable bytes existed and the LV is gone. Never mint an empty
    /// replacement. Recovery is an explicit restore.
    Lost,
    GrowVolume,
    Converged,
}

pub fn plan_workspace(
    desired: WorkspaceDesired,
    local: WorkspaceLocal,
    observed: WorkspaceObserved,
) -> WorkspaceAction {
    match desired {
        WorkspaceDesired::Active => plan_active(local, observed),
        WorkspaceDesired::Suspended => plan_suspended(local, observed),
        WorkspaceDesired::Absent => plan_absent(observed),
    }
}

fn plan_active(local: WorkspaceLocal, observed: WorkspaceObserved) -> WorkspaceAction {
    if !observed.lv {
        if local.materialized {
            return WorkspaceAction::Lost;
        }
        return WorkspaceAction::AllocateLv;
    }
    if matches!(
        observed.pod,
        WorkspacePod::Unknown { stale_ready: true } | WorkspacePod::Failed
    ) {
        return WorkspaceAction::ReplacePod;
    }
    if !observed.mapper {
        return WorkspaceAction::EnsureMapper;
    }
    if !observed.pv {
        return WorkspaceAction::CreatePv;
    }
    match observed.pod {
        WorkspacePod::Ready => WorkspaceAction::Converged,
        WorkspacePod::Pending | WorkspacePod::Running => WorkspaceAction::WaitPod,
        WorkspacePod::Absent => WorkspaceAction::CreatePod,
        WorkspacePod::Unknown { stale_ready: false }
        | WorkspacePod::Unknown { stale_ready: true }
        | WorkspacePod::Failed => WorkspaceAction::ReplacePod,
    }
}

fn plan_suspended(local: WorkspaceLocal, observed: WorkspaceObserved) -> WorkspaceAction {
    if observed.pod != WorkspacePod::Absent {
        return WorkspaceAction::RemovePod;
    }
    if !observed.lv {
        if local.materialized {
            return WorkspaceAction::Lost;
        }
        return WorkspaceAction::AllocateLv;
    }
    if observed.pv {
        return WorkspaceAction::RemovePv;
    }
    WorkspaceAction::Converged
}

fn plan_absent(observed: WorkspaceObserved) -> WorkspaceAction {
    if observed.pod != WorkspacePod::Absent {
        return WorkspaceAction::RemovePod;
    }
    if observed.pv {
        return WorkspaceAction::RemovePv;
    }
    // The LV, mapper, jail, and VMM stay until residue-gated
    // `delete_workspace` proves absence. Archive and delete both map here;
    // neither may free capacity through the reconciler.
    WorkspaceAction::Converged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        desired: WorkspaceDesired,
        lv: bool,
        pv: bool,
        pod: WorkspacePod,
        want: WorkspaceAction,
    ) {
        let local = WorkspaceLocal { materialized: lv };
        let observed = WorkspaceObserved {
            lv,
            mapper: lv,
            pv,
            pod,
        };
        assert_eq!(
            plan_workspace(desired, local, observed),
            want,
            "desired={desired:?} lv={lv} pv={pv} pod={pod:?}"
        );
    }

    #[test]
    fn present_matrix() {
        row(
            WorkspaceDesired::Active,
            false,
            false,
            WorkspacePod::Absent,
            WorkspaceAction::AllocateLv,
        );
        row(
            WorkspaceDesired::Active,
            true,
            false,
            WorkspacePod::Absent,
            WorkspaceAction::CreatePv,
        );
        row(
            WorkspaceDesired::Active,
            true,
            true,
            WorkspacePod::Pending,
            WorkspaceAction::WaitPod,
        );
        row(
            WorkspaceDesired::Active,
            true,
            true,
            WorkspacePod::Running,
            WorkspaceAction::WaitPod,
        );
        row(
            WorkspaceDesired::Active,
            true,
            true,
            WorkspacePod::Absent,
            WorkspaceAction::CreatePod,
        );
        row(
            WorkspaceDesired::Active,
            true,
            true,
            WorkspacePod::Failed,
            WorkspaceAction::ReplacePod,
        );
        row(
            WorkspaceDesired::Active,
            true,
            true,
            WorkspacePod::Unknown { stale_ready: true },
            WorkspaceAction::ReplacePod,
        );
        row(
            WorkspaceDesired::Active,
            true,
            true,
            WorkspacePod::Ready,
            WorkspaceAction::Converged,
        );
    }

    #[test]
    fn absent_matrix() {
        row(
            WorkspaceDesired::Absent,
            true,
            true,
            WorkspacePod::Ready,
            WorkspaceAction::RemovePod,
        );
        row(
            WorkspaceDesired::Absent,
            true,
            true,
            WorkspacePod::Absent,
            WorkspaceAction::RemovePv,
        );
        row(
            WorkspaceDesired::Absent,
            true,
            false,
            WorkspacePod::Absent,
            WorkspaceAction::Converged,
        );
        row(
            WorkspaceDesired::Absent,
            false,
            false,
            WorkspacePod::Absent,
            WorkspaceAction::Converged,
        );
        let leftover = WorkspaceObserved {
            lv: true,
            mapper: true,
            pv: false,
            pod: WorkspacePod::Absent,
        };
        let action = plan_workspace(
            WorkspaceDesired::Absent,
            WorkspaceLocal { materialized: true },
            leftover,
        );
        assert_eq!(action, WorkspaceAction::Converged);
        assert_ne!(action, WorkspaceAction::RemoveLv);
        assert_ne!(action, WorkspaceAction::RemoveMapper);
    }

    #[test]
    fn suspended_keeps_lv_and_drops_pod() {
        let observed = WorkspaceObserved {
            lv: true,
            mapper: true,
            pv: true,
            pod: WorkspacePod::Ready,
        };
        assert_eq!(
            plan_workspace(
                WorkspaceDesired::Suspended,
                WorkspaceLocal { materialized: true },
                observed
            ),
            WorkspaceAction::RemovePod
        );
        let no_pod = WorkspaceObserved {
            lv: true,
            mapper: true,
            pv: false,
            pod: WorkspacePod::Absent,
        };
        assert_eq!(
            plan_workspace(
                WorkspaceDesired::Suspended,
                WorkspaceLocal { materialized: true },
                no_pod
            ),
            WorkspaceAction::Converged
        );
    }

    #[test]
    fn missing_mapper_before_pv() {
        let observed = WorkspaceObserved {
            lv: true,
            mapper: false,
            pv: false,
            pod: WorkspacePod::Absent,
        };
        assert_eq!(
            plan_workspace(
                WorkspaceDesired::Active,
                WorkspaceLocal { materialized: true },
                observed
            ),
            WorkspaceAction::EnsureMapper
        );
    }

    #[test]
    fn materialized_missing_lv_is_lost() {
        let missing = WorkspaceObserved {
            lv: false,
            mapper: false,
            pv: false,
            pod: WorkspacePod::Absent,
        };
        assert_eq!(
            plan_workspace(
                WorkspaceDesired::Active,
                WorkspaceLocal { materialized: true },
                missing
            ),
            WorkspaceAction::Lost
        );
        assert_eq!(
            plan_workspace(
                WorkspaceDesired::Suspended,
                WorkspaceLocal { materialized: true },
                missing
            ),
            WorkspaceAction::Lost
        );
        assert_eq!(
            plan_workspace(
                WorkspaceDesired::Active,
                WorkspaceLocal {
                    materialized: false
                },
                missing
            ),
            WorkspaceAction::AllocateLv
        );
        assert_ne!(
            plan_workspace(
                WorkspaceDesired::Active,
                WorkspaceLocal { materialized: true },
                missing
            ),
            WorkspaceAction::AllocateLv
        );
    }
}
