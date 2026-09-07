//! Deployment desired-state planner. Candidate promotion is a separate
//! Control transaction; this planner only converges one Deployment spec.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentDesired {
    Running,
    Stopped,
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentPod {
    Absent,
    Pending,
    Running,
    Ready,
    Unknown { stale_ready: bool },
    Failed,
}

impl Default for DeploymentPod {
    fn default() -> Self {
        Self::Absent
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeploymentLocal {
    pub allocation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeploymentObserved {
    pub lv: bool,
    pub pv: bool,
    pub pod: DeploymentPod,
    /// Environment ClusterIP exists. Running does not create or steal it;
    /// traffic desired owns the selector and `None` retires it.
    pub service_present: bool,
    /// Selector points at this Deployment. Observation only: this planner
    /// never deletes the shared Environment Service.
    pub service_owned: bool,
    /// Live Pod `io.voie/pod-generation`. Missing label is 0.
    pub pod_generation: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeploymentAction {
    /// Planner must not emit this. `execute_deployment` refuses empty LV
    /// minting; missing bytes are `NeedsReleaseStream`.
    #[allow(dead_code)]
    AllocateLv,
    CreatePv,
    CreatePod,
    ReplacePod,
    RemovePod,
    RemovePv,
    RemoveLv,
    WaitPod,
    /// Empty LV is never the recovery path. Stream the exact Release from
    /// Blob, verify the hash, then CreatePv/CreatePod.
    NeedsReleaseStream,
    Converged,
}

pub fn plan_deployment(
    desired: DeploymentDesired,
    local: DeploymentLocal,
    observed: DeploymentObserved,
    desired_pod_generation: i64,
) -> DeploymentAction {
    match desired {
        DeploymentDesired::Running => plan_running(local, observed, desired_pod_generation),
        DeploymentDesired::Stopped => plan_stopped(local, observed),
        DeploymentDesired::Absent => plan_absent(local, observed),
    }
}

fn plan_running(
    _local: DeploymentLocal,
    observed: DeploymentObserved,
    desired_pod_generation: i64,
) -> DeploymentAction {
    if !observed.lv {
        return DeploymentAction::NeedsReleaseStream;
    }
    if desired_pod_generation > observed.pod_generation
        && !matches!(observed.pod, DeploymentPod::Absent)
    {
        return DeploymentAction::ReplacePod;
    }
    if matches!(
        observed.pod,
        DeploymentPod::Unknown { stale_ready: true } | DeploymentPod::Failed
    ) {
        return DeploymentAction::ReplacePod;
    }
    if !observed.pv {
        return DeploymentAction::CreatePv;
    }
    match observed.pod {
        DeploymentPod::Ready => DeploymentAction::Converged,
        DeploymentPod::Pending | DeploymentPod::Running => DeploymentAction::WaitPod,
        DeploymentPod::Absent => DeploymentAction::CreatePod,
        DeploymentPod::Unknown { stale_ready: false }
        | DeploymentPod::Unknown { stale_ready: true }
        | DeploymentPod::Failed => DeploymentAction::ReplacePod,
    }
}

fn plan_stopped(_local: DeploymentLocal, observed: DeploymentObserved) -> DeploymentAction {
    if observed.pod != DeploymentPod::Absent {
        return DeploymentAction::RemovePod;
    }
    if !observed.lv {
        return DeploymentAction::NeedsReleaseStream;
    }
    DeploymentAction::Converged
}

fn plan_absent(local: DeploymentLocal, observed: DeploymentObserved) -> DeploymentAction {
    if observed.pod != DeploymentPod::Absent {
        return DeploymentAction::RemovePod;
    }
    if observed.pv {
        return DeploymentAction::RemovePv;
    }
    if observed.lv || local.allocation {
        return DeploymentAction::RemoveLv;
    }
    DeploymentAction::Converged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        desired: DeploymentDesired,
        lv: bool,
        pv: bool,
        pod: DeploymentPod,
        service: bool,
        want: DeploymentAction,
    ) {
        row_services(desired, lv, pv, pod, service, service, want);
    }

    fn row_services(
        desired: DeploymentDesired,
        lv: bool,
        pv: bool,
        pod: DeploymentPod,
        service_present: bool,
        service_owned: bool,
        want: DeploymentAction,
    ) {
        let local = DeploymentLocal { allocation: lv };
        let observed = DeploymentObserved {
            lv,
            pv,
            pod,
            service_present,
            service_owned,
            pod_generation: 0,
        };
        assert_eq!(
            plan_deployment(desired, local, observed, 0),
            want,
            "desired={desired:?} lv={lv} pv={pv} pod={pod:?} present={service_present} owned={service_owned}"
        );
    }

    #[test]
    fn running_matrix() {
        row(
            DeploymentDesired::Running,
            false,
            false,
            DeploymentPod::Absent,
            false,
            DeploymentAction::NeedsReleaseStream,
        );
        row(
            DeploymentDesired::Running,
            true,
            false,
            DeploymentPod::Absent,
            false,
            DeploymentAction::CreatePv,
        );
        row(
            DeploymentDesired::Running,
            true,
            true,
            DeploymentPod::Absent,
            false,
            DeploymentAction::CreatePod,
        );
        row(
            DeploymentDesired::Running,
            true,
            true,
            DeploymentPod::Running,
            false,
            DeploymentAction::WaitPod,
        );
        row(
            DeploymentDesired::Running,
            true,
            true,
            DeploymentPod::Unknown { stale_ready: false },
            false,
            DeploymentAction::ReplacePod,
        );
        row(
            DeploymentDesired::Running,
            true,
            true,
            DeploymentPod::Ready,
            false,
            DeploymentAction::Converged,
        );
        row(
            DeploymentDesired::Running,
            true,
            true,
            DeploymentPod::Ready,
            true,
            DeploymentAction::Converged,
        );
        row(
            DeploymentDesired::Running,
            true,
            true,
            DeploymentPod::Failed,
            true,
            DeploymentAction::ReplacePod,
        );
    }

    #[test]
    fn reboot_pod_absent_creates_pod() {
        row(
            DeploymentDesired::Running,
            true,
            true,
            DeploymentPod::Absent,
            true,
            DeploymentAction::CreatePod,
        );
    }

    #[test]
    fn absent_matrix() {
        row(
            DeploymentDesired::Absent,
            true,
            true,
            DeploymentPod::Ready,
            true,
            DeploymentAction::RemovePod,
        );
        row(
            DeploymentDesired::Absent,
            false,
            false,
            DeploymentPod::Absent,
            false,
            DeploymentAction::Converged,
        );
        let leftover = DeploymentObserved {
            lv: false,
            pv: false,
            pod: DeploymentPod::Absent,
            service_present: false,
            service_owned: false,
            pod_generation: 0,
        };
        assert_eq!(
            plan_deployment(
                DeploymentDesired::Absent,
                DeploymentLocal { allocation: true },
                leftover,
                0,
            ),
            DeploymentAction::RemoveLv,
            "an allocation without an LV is still Fabric residue"
        );
    }

    #[test]
    fn stopped_keeps_lv() {
        row(
            DeploymentDesired::Stopped,
            true,
            true,
            DeploymentPod::Ready,
            true,
            DeploymentAction::RemovePod,
        );
        row(
            DeploymentDesired::Stopped,
            true,
            true,
            DeploymentPod::Absent,
            false,
            DeploymentAction::Converged,
        );
        row(
            DeploymentDesired::Stopped,
            true,
            true,
            DeploymentPod::Absent,
            true,
            DeploymentAction::Converged,
        );
        row(
            DeploymentDesired::Stopped,
            false,
            false,
            DeploymentPod::Absent,
            false,
            DeploymentAction::NeedsReleaseStream,
        );
    }

    #[test]
    fn shared_environment_service_is_not_stolen() {
        row_services(
            DeploymentDesired::Running,
            true,
            true,
            DeploymentPod::Ready,
            true,
            false,
            DeploymentAction::Converged,
        );
        row_services(
            DeploymentDesired::Stopped,
            true,
            true,
            DeploymentPod::Absent,
            true,
            false,
            DeploymentAction::Converged,
        );
        row_services(
            DeploymentDesired::Absent,
            true,
            true,
            DeploymentPod::Absent,
            true,
            false,
            DeploymentAction::RemovePv,
        );
        row_services(
            DeploymentDesired::Stopped,
            true,
            true,
            DeploymentPod::Absent,
            true,
            true,
            DeploymentAction::Converged,
        );
        row_services(
            DeploymentDesired::Absent,
            true,
            true,
            DeploymentPod::Absent,
            true,
            true,
            DeploymentAction::RemovePv,
        );
    }

    #[test]
    fn missing_lv_never_mints_an_empty_volume() {
        let missing = DeploymentObserved {
            lv: false,
            pv: false,
            pod: DeploymentPod::Absent,
            service_present: false,
            service_owned: false,
            pod_generation: 0,
        };
        assert_eq!(
            plan_deployment(
                DeploymentDesired::Running,
                DeploymentLocal { allocation: true },
                missing,
                0,
            ),
            DeploymentAction::NeedsReleaseStream
        );
        assert_eq!(
            plan_deployment(
                DeploymentDesired::Running,
                DeploymentLocal { allocation: false },
                missing,
                0,
            ),
            DeploymentAction::NeedsReleaseStream
        );
        assert_ne!(
            plan_deployment(
                DeploymentDesired::Running,
                DeploymentLocal { allocation: true },
                missing,
                0,
            ),
            DeploymentAction::AllocateLv
        );
    }

    #[test]
    fn running_replaces_pod_when_generation_lags() {
        let observed = DeploymentObserved {
            lv: true,
            pv: true,
            pod: DeploymentPod::Ready,
            service_present: true,
            service_owned: true,
            pod_generation: 0,
        };
        assert_eq!(
            plan_deployment(
                DeploymentDesired::Running,
                DeploymentLocal { allocation: true },
                observed,
                1,
            ),
            DeploymentAction::ReplacePod
        );
        assert_eq!(
            plan_deployment(
                DeploymentDesired::Running,
                DeploymentLocal { allocation: true },
                observed,
                0,
            ),
            DeploymentAction::Converged
        );
    }
}
