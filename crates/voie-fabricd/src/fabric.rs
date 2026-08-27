//! Workspace, exec, replace, and cleanup orchestration.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::FabricError;
use crate::realize::{
    BlockSlot, ExecVerdict, Live, NETWORK_POLICY_NAME, Residue, classify_exec, is_daemon_lv_name,
    lv_name_for, managed, object_names, spec_sha,
};
use crate::store::{BeginDispatch, CleanupRow, GenerationRow, ReservationRow, Store, WorkspaceRow};

/// What startup reconciliation found and did. Every list names workspaces
/// or logical volumes so the operator log stays truthful; nothing here is
/// inferred success. Deleting workspaces are retried before this report
/// is returned; only rows that remain transient are listed.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct StartupReport {
    /// Orphaned reservations (no workspace row) released after positive
    /// absence of every realization surface.
    pub orphan_reservations_released: Vec<String>,
    /// Orphaned reservations kept held because any surface was unknown;
    /// unknown means held.
    pub orphan_reservations_held: Vec<String>,
    /// Prepared logical volumes removed because no store row claimed them.
    pub orphan_lvs_removed: Vec<String>,
    /// Logical volumes that could not be enumerated or removed; they stay.
    pub orphan_lv_failures: Vec<String>,
    /// Workspaces that remain in creating, replacing, or deleting after the
    /// bounded startup retry; unknown outcomes remain listed and held.
    pub transient_workspaces: Vec<String>,
}

/// The whole-run guest deadline of the Bash contract: every user command is
/// dispatched through the runner shell under `/workspace` with this fixed
/// bound; there is no per-request override.
const EXEC_TIMEOUT_MS: u64 = voie_runner::DEFAULT_TIMEOUT_MS;

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceView {
    pub id: String,
    pub state: String,
    pub generation: i64,
    pub pod_name: String,
    pub pod_uid: String,
    pub sandbox_id: Option<String>,
    pub pv_name: String,
    pub pvc_name: String,
    pub device: String,
    pub node: String,
    pub runtime_class: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecView {
    pub call_id: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CleanupView {
    pub id: String,
    pub state: String,
    pub cleaned: CleanupFlags,
}

#[derive(Debug, Clone, Serialize)]
pub struct CleanupFlags {
    pub pod: bool,
    pub reservation: bool,
    pub jail: bool,
    pub vmm: bool,
    pub children: bool,
}

pub struct Fabric {
    store: Store,
    live: Live,
    /// One lifecycle key per workspace id. Create, replace, delete, and exec
    /// hold their workspace's key for the whole operation so concurrent
    /// requests can never interleave two lifecycles on one workspace while
    /// different workspaces proceed independently.
    lifecycles: Mutex<BTreeMap<String, std::sync::Arc<AsyncMutex<()>>>>,
}

impl Fabric {
    pub fn open(config: crate::Config, live: Live) -> Result<Self, FabricError> {
        let store = Store::open(&config.sqlite)?;
        Ok(Fabric {
            store,
            live,
            lifecycles: Mutex::new(BTreeMap::new()),
        })
    }

    async fn lifecycle_guard(&self, workspace_id: &str) -> OwnedMutexGuard<()> {
        let key = {
            let mut keys = self
                .lifecycles
                .lock()
                .expect("lifecycle key table cannot be poisoned");
            keys.entry(workspace_id.to_owned()).or_default().clone()
        };
        key.lock_owned().await
    }

    /// Classifies crash leftovers before the API serves any request.
    ///
    /// Two orphan shapes exist after a daemon death between write steps:
    /// a reservation whose workspace row was never written (crash between
    /// `reserve_volume` and `upsert_workspace`), and a prepared logical
    /// volume no row claims at all (crash between `lvcreate` and
    /// `reserve_volume`, or after a release that lost its final removal).
    /// The first is released only when every realization surface is
    /// positively absent; anything else stays held. The second is removed
    /// only when its name is one this daemon mints and no store row claims
    /// it. A `deleting` workspace is re-driven through the same idempotent
    /// cleanup path before unresolved transient rows are reported.
    pub async fn reconcile_startup(&self) -> Result<StartupReport, FabricError> {
        let mut report = StartupReport::default();
        let reserved = self.store.list_reserved_reservations()?;
        let workspaces = self.store.list_workspaces()?;
        let realized: HashSet<&str> = workspaces.iter().map(|row| row.id.as_str()).collect();

        for reservation in &reserved {
            if realized.contains(reservation.workspace_id.as_str()) {
                continue;
            }
            match self.resolve_orphaned_reservation(reservation).await {
                Ok(true) => report
                    .orphan_reservations_released
                    .push(reservation.workspace_id.clone()),
                Ok(false) => report
                    .orphan_reservations_held
                    .push(reservation.workspace_id.clone()),
                Err(error) => {
                    eprintln!(
                        "voie-fabricd: reservation {} stays held: {error}",
                        reservation.workspace_id
                    );
                    report
                        .orphan_reservations_held
                        .push(reservation.workspace_id.clone());
                }
            }
        }

        // A claimed slot is protected by either its workspace row or an
        // active reservation. Everything else carrying a daemon-minted name
        // is an unclaimed leftover of a crashed prepare.
        let mut protected: HashSet<String> = workspaces
            .iter()
            .filter_map(|row| row.lv_name.clone())
            .collect();
        for reservation in &reserved {
            protected.insert(lv_name_for(&reservation.workspace_id));
        }
        match self.live.list_lv_names().await {
            Ok(names) => {
                for name in names {
                    if !is_daemon_lv_name(&name) || protected.contains(&name) {
                        continue;
                    }
                    let slot = BlockSlot {
                        device: String::new(),
                        lv_name: Some(name.clone()),
                    };
                    match self.live.release_block(&slot).await {
                        Ok(()) => report.orphan_lvs_removed.push(name),
                        Err(error) => {
                            eprintln!("voie-fabricd: unclaimed LV {name} stays: {error}");
                            report.orphan_lv_failures.push(name);
                        }
                    }
                }
            }
            Err(error) => {
                eprintln!(
                    "voie-fabricd: cannot enumerate pool {}; no unclaimed LV is removed: {error}",
                    self.live.vg_name()
                );
            }
        }

        let mut transient_workspaces = Vec::new();
        for workspace in &workspaces {
            match workspace.state.as_str() {
                // Creation and replacement have distinct forward-only
                // realization protocols; startup observes and reports them
                // without guessing a rollback or replaying an effect.
                "creating" | "replacing" => transient_workspaces.push(workspace.id.clone()),
                // Deletion is the one safe retry path: every operation is
                // idempotent and the reservation is released only after a
                // fresh positive absence check.
                "deleting" => match self.delete_workspace(&workspace.id).await {
                    Ok(cleanup) if cleanup.state == "deleted" => {}
                    Ok(_) => transient_workspaces.push(workspace.id.clone()),
                    Err(error) => {
                        eprintln!(
                            "voie-fabricd: deleting workspace {} remains held: {error}",
                            workspace.id
                        );
                        transient_workspaces.push(workspace.id.clone());
                    }
                },
                _ => {}
            }
        }
        report.transient_workspaces = transient_workspaces;
        Ok(report)
    }

    /// Releases one orphaned reservation only on positive absence of every
    /// surface where guest bytes could ever have appeared. Returns `Ok(false)`
    /// when any surface is present or unknowable: the reservation then stays
    /// held exactly as the cleanup contract requires.
    async fn resolve_orphaned_reservation(
        &self,
        reservation: &ReservationRow,
    ) -> Result<bool, FabricError> {
        let workspace_id = &reservation.workspace_id;
        let (_, _, pod_name) = object_names(workspace_id, 1);

        if self.live.get_pv(&reservation.pv_name).await?.is_some() {
            return Ok(false);
        }
        if self
            .live
            .get_namespaced("pvc", &reservation.pv_name)
            .await?
            .is_some()
        {
            return Ok(false);
        }
        if self.live.get_namespaced("pod", &pod_name).await?.is_some() {
            return Ok(false);
        }
        let residue = self
            .live
            .wait_residue_gone(&pod_name, None, self.live.residue_wait())
            .await?;
        if !residue.runtime_clean() {
            return Ok(false);
        }
        if !self.live.sandbox_absent(&pod_name).await? {
            return Ok(false);
        }
        if self.live.device_mounted(&reservation.device).await? {
            return Ok(false);
        }

        // Every surface is positively absent, so the prepared slot holds no
        // workspace bytes this daemon ever exposed to a guest. Remove the LV
        // first; the reservation row is released only once removal succeeded
        // or the LV was already gone.
        let slot = BlockSlot {
            device: reservation.device.clone(),
            lv_name: Some(lv_name_for(workspace_id)),
        };
        self.live.release_block(&slot).await?;
        self.store
            .release_reservation(workspace_id, "startup-unrealized")?;
        Ok(true)
    }

    pub async fn create_workspace(&self, id: &str) -> Result<WorkspaceView, FabricError> {
        let _lifecycle = self.lifecycle_guard(id).await;
        if let Some(existing) = self.store.get_workspace(id)? {
            match existing.state.as_str() {
                "ready" => return self.view_from_row(&existing),
                "deleting" => {
                    return Err(FabricError::Realize(format!("workspace {id} is deleting")));
                }
                _ => {}
            }
        }

        // First realization step, deliberately ahead of any side effect: a
        // RuntimeClass that is absent or wrongly handled means admission can
        // never accept this workspace's pod, so the gate fails before the
        // pool loses an LV, the store records a reservation, or any device
        // is touched. There is no path that carves bytes for a workspace no
        // pod may ever join.
        self.ensure_runtime_class().await?;

        let slot = self.live.prepare_block(id).await?;
        let (pv_name, pvc_name, pod_name) = object_names(id, 1);
        self.store
            .reserve_volume(id, &slot.device, self.live.node_name(), &pv_name)?;
        if self.live.device_mounted(&slot.device).await? {
            return Err(FabricError::Foreign(format!(
                "reserved device {} is already mounted",
                slot.device
            )));
        }
        self.live.mkfs_ext4_if_needed(&slot.device).await?;
        self.live.ensure_namespace().await?;
        self.live.ensure_storage_class().await?;
        self.live.ensure_workspace_service_account().await?;
        self.ensure_network_policy().await?;
        self.refuse_foreign(id, &pv_name, &pvc_name, &pod_name)
            .await?;

        self.store.upsert_workspace(&WorkspaceRow {
            id: id.to_owned(),
            state: "creating".into(),
            device: slot.device.clone(),
            node: self.live.node_name().to_owned(),
            pv_name: pv_name.clone(),
            pvc_name: pvc_name.clone(),
            lv_name: slot.lv_name.clone(),
        })?;

        self.live
            .apply_yaml(&self.live.pv_yaml(id, &pv_name, &slot.device))
            .await?;
        let Some(pv) = self.live.get_pv(&pv_name).await? else {
            return Err(FabricError::Unknown(format!(
                "PV {pv_name} missing after apply"
            )));
        };
        let canonical = self
            .live
            .canonical_device(&pv.path)
            .await
            .unwrap_or(pv.path.clone());
        let mut pv = pv;
        pv.path = canonical;
        self.live.verify_pv(&pv, id, &slot.device)?;
        self.live
            .apply_yaml(&self.live.pvc_yaml(id, &pvc_name, &pv_name))
            .await?;
        self.live
            .apply_yaml(&self.live.pod_yaml(id, &pod_name, &pvc_name, 1))
            .await?;

        // Readiness is Kubernetes' own verdict: the generated Pod's
        // mount-validating readinessProbe drives the `Ready` condition, and
        // only a positively Ready pod may mark the workspace ready. The pod
        // could only have been applied because the RuntimeClass gate above
        // already confirmed admission's precondition. On an unresolved wait
        // the row stays `creating` and no generation is recorded, so a retry
        // re-realizes from scratch.
        let pod = self
            .live
            .wait_pod_ready(&pod_name, Duration::from_secs(180))
            .await?;
        if pod.runtime_class != self.live.runtime_class() {
            return Err(FabricError::Realize(format!(
                "pod {} runtimeClass is {}, want {}",
                pod.name,
                pod.runtime_class,
                self.live.runtime_class()
            )));
        }
        if self.store.latest_generation(id)?.is_some() {
            self.store.update_generation_runtime(
                id,
                1,
                &pod.uid,
                pod.sandbox_id.as_deref(),
                "running",
            )?;
        } else {
            self.store.insert_generation(&GenerationRow {
                workspace_id: id.to_owned(),
                generation: 1,
                pod_name: pod_name.clone(),
                pod_uid: Some(pod.uid.clone()),
                sandbox_id: pod.sandbox_id.clone(),
                state: "running".into(),
            })?;
        }
        self.store.set_workspace_state(id, "ready")?;
        self.view(id)
    }

    pub fn get_workspace(&self, id: &str) -> Result<WorkspaceView, FabricError> {
        self.view(id)
    }

    pub fn list_workspaces(&self) -> Result<Vec<WorkspaceView>, FabricError> {
        let rows = self.store.list_workspaces()?;
        rows.iter().map(|row| self.view_from_row(row)).collect()
    }

    pub async fn exec(
        &self,
        workspace_id: &str,
        call_id: &str,
        command: &str,
    ) -> Result<ExecView, FabricError> {
        validate_call_id(call_id)?;
        let _lifecycle = self.lifecycle_guard(workspace_id).await;
        let workspace = self
            .store
            .get_workspace(workspace_id)?
            .ok_or(FabricError::NotFound)?;
        if workspace.state != "ready" && workspace.state != "replacing" {
            return Err(FabricError::Realize(format!(
                "workspace {workspace_id} is {}",
                workspace.state
            )));
        }
        let generation = self
            .store
            .latest_generation(workspace_id)?
            .ok_or(FabricError::Realize("workspace has no execution".into()))?;
        let hash = Store::request_hash(command);
        match self.store.begin_dispatch(workspace_id, call_id, &hash)? {
            BeginDispatch::Conflict => {
                return Err(FabricError::Conflict(format!(
                    "call {call_id} was recorded with a different command"
                )));
            }
            BeginDispatch::Terminal {
                exit_code,
                stdout,
                stderr,
            } => {
                return Ok(ExecView {
                    call_id: call_id.to_owned(),
                    state: "terminal".into(),
                    exit_code,
                    stdout: Some(stdout),
                    stderr: Some(stderr),
                });
            }
            BeginDispatch::OutcomeUnknown => {
                return Ok(ExecView {
                    call_id: call_id.to_owned(),
                    state: "unknown".into(),
                    exit_code: None,
                    stdout: None,
                    stderr: None,
                });
            }
            BeginDispatch::ReadyToDispatch => {}
        }

        let result = self
            .live
            .exec_runner(&generation.pod_name, command, EXEC_TIMEOUT_MS)
            .await;
        match result {
            Ok(output) if !output.ambiguous => {
                match classify_exec(output.exit_code, &output.stderr) {
                    ExecVerdict::Terminal(exit_code) => {
                        self.store.complete_exec(
                            workspace_id,
                            call_id,
                            exit_code,
                            &output.stdout,
                            &output.stderr,
                        )?;
                        Ok(ExecView {
                            call_id: call_id.to_owned(),
                            state: "terminal".into(),
                            exit_code: Some(exit_code),
                            stdout: Some(output.stdout),
                            stderr: Some(output.stderr),
                        })
                    }
                    ExecVerdict::Unknown => {
                        self.finish_unknown(
                            workspace_id,
                            call_id,
                            Some(output.exit_code),
                            &output.stdout,
                            &output.stderr,
                        )
                        .await
                    }
                }
            }
            Ok(_) | Err(_) => {
                self.finish_unknown(workspace_id, call_id, None, "", "")
                    .await
            }
        }
    }

    /// Records a durably unknown attempt. The captured streams stay on the
    /// row for diagnosis, but the row can never become terminal and never
    /// dispatches again under the same call id.
    async fn finish_unknown(
        &self,
        workspace_id: &str,
        call_id: &str,
        exit_code: Option<i32>,
        stdout: &str,
        stderr: &str,
    ) -> Result<ExecView, FabricError> {
        self.store
            .mark_unknown(workspace_id, call_id, exit_code, stdout, stderr)?;
        Ok(ExecView {
            call_id: call_id.to_owned(),
            state: "unknown".into(),
            exit_code: None,
            stdout: None,
            stderr: None,
        })
    }

    pub fn get_exec(&self, workspace_id: &str, call_id: &str) -> Result<ExecView, FabricError> {
        let row = self
            .store
            .get_exec(workspace_id, call_id)?
            .ok_or(FabricError::NotFound)?;
        Ok(ExecView {
            call_id: row.call_id,
            state: row.state,
            exit_code: row.exit_code,
            stdout: row.stdout,
            stderr: row.stderr,
        })
    }

    pub async fn replace(&self, workspace_id: &str) -> Result<WorkspaceView, FabricError> {
        let _lifecycle = self.lifecycle_guard(workspace_id).await;
        let workspace = self
            .store
            .get_workspace(workspace_id)?
            .ok_or(FabricError::NotFound)?;
        if workspace.state != "ready" && workspace.state != "replacing" {
            return Err(FabricError::Realize(format!(
                "workspace {workspace_id} is {}",
                workspace.state
            )));
        }
        let previous = self
            .store
            .latest_generation(workspace_id)?
            .ok_or(FabricError::Realize("workspace has no execution".into()))?;
        self.store.set_workspace_state(workspace_id, "replacing")?;
        self.ensure_network_policy().await?;
        self.live.ensure_workspace_service_account().await?;
        // Before any teardown: if the estate RuntimeClass is not ready to
        // admit a replacement pod, the still-running previous generation is
        // left untouched rather than destroyed in front of a gate that
        // cannot pass. A `replaced` generation resuming here simply waits
        // like creation would.
        self.ensure_runtime_class().await?;

        // A `replacing` workspace whose latest generation was already marked
        // `replaced` crashed after passing the teardown gate; resuming skips
        // straight to realizing the next generation. Otherwise the previous
        // generation must be positively absent before anything new appears.
        if previous.state != "replaced" {
            self.live
                .delete_named(
                    "pod",
                    &previous.pod_name,
                    true,
                    self.live.residue_wait().as_secs(),
                )
                .await?;
            let residue = self
                .live
                .wait_residue_gone(
                    &previous.pod_name,
                    previous.sandbox_id.as_deref(),
                    self.live.residue_wait(),
                )
                .await?;
            if !residue.runtime_clean() {
                return Err(FabricError::Unknown(
                    "previous execution residue was not positively absent".into(),
                ));
            }
            self.store
                .stop_generation(workspace_id, previous.generation, "replaced")?;
        }

        let next = previous.generation + 1;
        let (_pv, pvc_name, pod_name) = object_names(workspace_id, next);
        self.live
            .apply_yaml(&self.live.pod_yaml(workspace_id, &pod_name, &pvc_name, next))
            .await?;
        // Same readiness rule as creation: the new generation counts only
        // once Kubernetes reports the pod Ready, which the generated Pod's
        // mount-validating readinessProbe ties to the live `/workspace`
        // mount inside the guest.
        let pod = self
            .live
            .wait_pod_ready(&pod_name, Duration::from_secs(180))
            .await?;
        self.store.insert_generation(&GenerationRow {
            workspace_id: workspace_id.to_owned(),
            generation: next,
            pod_name,
            pod_uid: Some(pod.uid),
            sandbox_id: pod.sandbox_id,
            state: "running".into(),
        })?;
        self.store.set_workspace_state(workspace_id, "ready")?;
        self.view(workspace_id)
    }

    pub async fn delete_workspace(&self, workspace_id: &str) -> Result<CleanupView, FabricError> {
        let _lifecycle = self.lifecycle_guard(workspace_id).await;
        let workspace = self
            .store
            .get_workspace(workspace_id)?
            .ok_or(FabricError::NotFound)?;
        if workspace.state == "deleted" {
            let cleanup = self.store.get_cleanup(workspace_id)?.ok_or_else(|| {
                FabricError::Unknown(format!(
                    "workspace {workspace_id} is deleted without a cleanup record"
                ))
            })?;
            if !cleanup.reservation_released
                || !cleanup.pod_absent
                || !cleanup.jail_absent
                || !cleanup.vmm_absent
                || !cleanup.children_absent
            {
                return Err(FabricError::Unknown(format!(
                    "workspace {workspace_id} is deleted without positive cleanup evidence"
                )));
            }
            return Ok(cleanup_view(workspace_id, "deleted", &cleanup));
        }
        self.store.set_workspace_state(workspace_id, "deleting")?;
        let generation = self.store.latest_generation(workspace_id)?;
        let pod_name = generation
            .as_ref()
            .map(|row| row.pod_name.clone())
            .unwrap_or_else(|| object_names(workspace_id, 1).2);
        // A lost sandbox identity must not silently widen cleanup: without
        // it, jail/VMM presence is unprovable. Try to recover the identity
        // from the local CRI first; if it stays unknown, residue observation
        // falls back to host-wide checks and holds the reservation unless
        // absence is positive there too.
        let mut sandbox_id = generation.as_ref().and_then(|row| row.sandbox_id.clone());
        if sandbox_id.is_none() {
            sandbox_id = self.live.discover_sandbox_id(&pod_name).await;
        }

        let mut delete_unknown = false;
        if let Err(error) = self
            .live
            .delete_named("pod", &pod_name, true, self.live.residue_wait().as_secs())
            .await
        {
            if matches!(error, FabricError::Unknown(_)) {
                delete_unknown = true;
            } else {
                return Err(error);
            }
        }
        for (kind, name, namespaced) in [
            ("pvc", workspace.pvc_name.as_str(), true),
            ("pv", workspace.pv_name.as_str(), false),
        ] {
            match self.live.delete_named(kind, name, namespaced, 60).await {
                Ok(()) => {}
                Err(FabricError::Unknown(_)) => delete_unknown = true,
                Err(error) => return Err(error),
            }
        }

        let residue = self
            .live
            .wait_residue_gone(&pod_name, sandbox_id.as_deref(), self.live.residue_wait())
            .await?;

        let pv = self.live.get_pv(&workspace.pv_name).await?;
        let pvc = self.live.get_namespaced("pvc", &workspace.pvc_name).await?;
        let sandbox_absent = self.live.sandbox_absent(&pod_name).await?;
        let device_mounted = self.live.device_mounted(&workspace.device).await?;
        let reservation_ok = residue.runtime_clean()
            && pv.is_none()
            && pvc.is_none()
            && sandbox_absent
            && !device_mounted
            && !delete_unknown;

        // The reservation is released only after a fresh positive absence
        // observation for every realization surface: Pod, PV, PVC, CRI
        // sandbox, jailer, VMM, child processes, and mounted device. Any
        // unknown outcome keeps it reserved forever; bytes whose fate is
        // unknown must never be handed out again.
        if reservation_ok {
            let slot = BlockSlot {
                device: workspace.device.clone(),
                // The daemon's LV name is deterministic. Recover it when a
                // crash left the workspace row without its optional copy;
                // a missing name must never turn release into a no-op.
                lv_name: Some(
                    workspace
                        .lv_name
                        .clone()
                        .unwrap_or_else(|| lv_name_for(workspace_id)),
                ),
            };
            if let Err(error) = self.live.release_block(&slot).await {
                if matches!(error, FabricError::Unknown(_)) {
                    self.store
                        .put_cleanup(&cleanup_row(workspace_id, &residue, false))?;
                }
                return Err(error);
            }
            self.store
                .release_reservation(workspace_id, "positive-absence")?;
        }

        let cleanup = cleanup_row(workspace_id, &residue, reservation_ok);
        self.store.put_cleanup(&cleanup)?;
        if reservation_ok {
            self.store.set_workspace_state(workspace_id, "deleted")?;
        }
        Ok(cleanup_view(
            workspace_id,
            if reservation_ok {
                "deleted"
            } else {
                "deleting"
            },
            &cleanup,
        ))
    }

    /// Converges and confirms the daemon-owned guest-egress NetworkPolicy
    /// before any guest pod may be realized. Desired state is recorded
    /// first, then the live object is observed, normalized by the single
    /// published stored-shape equivalence ([`canonicalize_observed_spec`]),
    /// and compared field-exactly against the desired spec. A
    /// managed-but-drifted object is converged; an unmanaged object is
    /// foreign and refused; an absent object is applied once and
    /// re-observed. Nothing proceeds on anything less than positive
    /// confirmation. Idempotent; also usable standalone to re-check the
    /// gate without touching any workspace.
    ///
    /// [`ensure_runtime_class`] runs before this in both pod-realizing
    /// paths: admission refuses a pod whose RuntimeClass is absent, so the
    /// class must be positively ready before any policy or pod work.
    pub async fn ensure_runtime_class(&self) -> Result<(), FabricError> {
        self.live
            .wait_runtime_class_ready(self.live.runtime_class_wait())
            .await
    }

    pub async fn ensure_network_policy(&self) -> Result<(), FabricError> {
        let desired_spec = self.live.desired_network_policy_spec();
        let desired_yaml = self.live.network_policy_yaml();
        let desired_sha = spec_sha(&desired_spec);
        self.store.put_policy_desired(
            NETWORK_POLICY_NAME,
            self.live.namespace(),
            &desired_yaml,
            &desired_sha,
        )?;

        match self.live.observe_network_policy().await? {
            None => {
                self.store
                    .set_policy_observed(NETWORK_POLICY_NAME, "missing", None)?;
                self.live.apply_yaml(&desired_yaml).await?;
                let applied = self.live.observe_network_policy().await?.ok_or_else(|| {
                    FabricError::Unknown("guest-egress NetworkPolicy absent after apply".into())
                })?;
                self.confirm_network_policy(applied, &desired_spec, &desired_sha)
                    .await
            }
            Some(live) => {
                if !managed(&live) {
                    // An existing unmanaged object was observed, so its spec
                    // digest is durable evidence of what actually sits in
                    // the namespace; recording it costs nothing and keeps
                    // the foreign refusal auditable.
                    let foreign_spec = live.get("spec").cloned().unwrap_or(Value::Null);
                    self.store.set_policy_observed(
                        NETWORK_POLICY_NAME,
                        "foreign",
                        Some(&spec_sha(&foreign_spec)),
                    )?;
                    return Err(FabricError::Foreign(format!(
                        "NetworkPolicy {NETWORK_POLICY_NAME} exists and is not managed by this Fabric"
                    )));
                }
                self.live.apply_yaml(&desired_yaml).await?;
                let converged = self.live.observe_network_policy().await?.ok_or_else(|| {
                    FabricError::Unknown("guest-egress NetworkPolicy vanished after apply".into())
                })?;
                self.confirm_network_policy(converged, &desired_spec, &desired_sha)
                    .await
            }
        }
    }

    async fn confirm_network_policy(
        &self,
        live: Value,
        desired_spec: &Value,
        desired_sha: &str,
    ) -> Result<(), FabricError> {
        let live_spec = live.get("spec").cloned().unwrap_or(Value::Null);
        let observed_spec = canonicalize_observed_spec(&live_spec);
        if *observed_spec == *desired_spec {
            self.store
                .set_policy_observed(NETWORK_POLICY_NAME, "present", Some(desired_sha))?;
            Ok(())
        } else {
            // After the one published canonicalization the comparison is
            // exact: any remaining difference between the desired spec and
            // what the API server stores fails closed as Unknown. The
            // evidence below describes desired vs canonicalized observation,
            // so its digests are reproducible by applying the same
            // published function to `kubectl get -o json` output. The store
            // schema keeps digests only, so the excerpt evidence travels
            // solely in this message rather than being written anywhere
            // unbounded.
            let drift = spec_drift(desired_spec, &observed_spec);
            self.store.set_policy_observed(
                NETWORK_POLICY_NAME,
                "drifted",
                Some(&drift.observed_spec_sha),
            )?;
            Err(FabricError::Unknown(format!(
                "guest-egress NetworkPolicy did not converge to the desired spec ({drift})"
            )))
        }
    }

    async fn refuse_foreign(
        &self,
        workspace_id: &str,
        pv_name: &str,
        pvc_name: &str,
        pod_name: &str,
    ) -> Result<(), FabricError> {
        if self
            .live
            .object_is_foreign("pv", pv_name, false, workspace_id)
            .await?
        {
            return Err(FabricError::Foreign(format!(
                "PV {pv_name} exists and is not owned by this workspace"
            )));
        }
        if self
            .live
            .object_is_foreign("pvc", pvc_name, true, workspace_id)
            .await?
        {
            return Err(FabricError::Foreign(format!(
                "PVC {pvc_name} exists and is not owned by this workspace"
            )));
        }
        if self
            .live
            .object_is_foreign("pod", pod_name, true, workspace_id)
            .await?
        {
            return Err(FabricError::Foreign(format!(
                "Pod {pod_name} exists and is not owned by this workspace"
            )));
        }
        Ok(())
    }

    fn view(&self, id: &str) -> Result<WorkspaceView, FabricError> {
        let row = self.store.get_workspace(id)?.ok_or(FabricError::NotFound)?;
        self.view_from_row(&row)
    }

    fn view_from_row(&self, row: &WorkspaceRow) -> Result<WorkspaceView, FabricError> {
        let generation = self.store.latest_generation(&row.id)?;
        Ok(WorkspaceView {
            id: row.id.clone(),
            state: row.state.clone(),
            generation: generation.as_ref().map(|g| g.generation).unwrap_or(0),
            pod_name: generation
                .as_ref()
                .map(|g| g.pod_name.clone())
                .unwrap_or_default(),
            pod_uid: generation
                .as_ref()
                .and_then(|g| g.pod_uid.clone())
                .unwrap_or_default(),
            sandbox_id: generation.and_then(|g| g.sandbox_id),
            pv_name: row.pv_name.clone(),
            pvc_name: row.pvc_name.clone(),
            device: row.device.clone(),
            node: row.node.clone(),
            runtime_class: self.live.runtime_class().to_owned(),
        })
    }
}

fn cleanup_view(workspace_id: &str, state: &str, cleanup: &CleanupRow) -> CleanupView {
    CleanupView {
        id: workspace_id.to_owned(),
        state: state.to_owned(),
        cleaned: CleanupFlags {
            pod: cleanup.pod_absent,
            reservation: cleanup.reservation_released,
            jail: cleanup.jail_absent,
            vmm: cleanup.vmm_absent,
            children: cleanup.children_absent,
        },
    }
}

fn cleanup_row(workspace_id: &str, residue: &Residue, reservation_released: bool) -> CleanupRow {
    CleanupRow {
        workspace_id: workspace_id.to_owned(),
        pod_absent: !residue.pod_present,
        reservation_released,
        jail_absent: !residue.jail_present,
        vmm_absent: !residue.vmm_present,
        children_absent: !residue.children_present,
    }
}

fn validate_call_id(call_id: &str) -> Result<(), FabricError> {
    if call_id.is_empty() || call_id.len() > 128 {
        return Err(FabricError::Config("call_id must be 1..=128 characters"));
    }
    if !call_id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(FabricError::Config(
            "call_id must be alphanumeric, hyphen, or underscore",
        ));
    }
    Ok(())
}

/// The single canonicalization applied to an observed NetworkPolicy spec
/// before it is compared with the desired spec. The API server prunes empty
/// lists when storing objects (`omitempty`), so a desired `ingress: []`
/// round-trips back with no `ingress` key at all — exactly what the local
/// API-server capture showed. Under Kubernetes semantics the two shapes are
/// identical: with `Ingress` among the declared policyTypes, both isolate
/// ingress for every selected pod, i.e. default-deny. Nothing else is
/// normalized; any other absence, addition, or value difference stays real
/// drift and fails closed.
fn canonicalize_observed_spec(observed: &Value) -> Cow<'_, Value> {
    let Some(map) = observed.as_object() else {
        return Cow::Borrowed(observed);
    };
    // The equivalence only holds while the object itself declares ingress
    // isolation. Without that declaration an omitted list no longer means
    // what the desired default-deny means, so it must stay visible drift.
    let isolates_ingress = map
        .get("policyTypes")
        .and_then(Value::as_array)
        .is_some_and(|types| types.iter().any(|entry| entry.as_str() == Some("Ingress")));
    if !isolates_ingress || map.contains_key("ingress") {
        return Cow::Borrowed(observed);
    }
    let mut canonical = map.clone();
    canonical.insert("ingress".into(), Value::Array(Vec::new()));
    Cow::Owned(Value::Object(canonical))
}

/// Upper bounds that keep NetworkPolicy drift evidence diagnostic-sized no
/// matter how large or hostile the live Kubernetes object is. The error
/// message built from this evidence is the only place excerpts appear; the
/// store keeps digests only.
const DRIFT_MAX_FIELDS: usize = 8;
const DRIFT_EXCERPT_BYTES: usize = 192;

/// How one top-level spec field differs between desired and observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriftKind {
    /// Desired declares the field; the observed spec does not have it.
    Missing,
    /// The observed spec carries a field the desired spec never declares.
    Unexpected,
    /// Both sides declare the field with different values.
    Changed,
}

impl DriftKind {
    fn label(self) -> &'static str {
        match self {
            DriftKind::Missing => "missing-from-observed",
            DriftKind::Unexpected => "not-in-desired",
            DriftKind::Changed => "changed",
        }
    }
}

/// One differing field with bounded value excerpts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SpecFieldDrift {
    field: String,
    kind: DriftKind,
    desired_excerpt: Option<String>,
    observed_excerpt: Option<String>,
}

/// Bounded desired-vs-observed evidence for one drifted NetworkPolicy spec.
/// Every excerpt is size-capped and every unshown difference is counted, so
/// the evidence names exactly what drifted without ever carrying an
/// unbounded Kubernetes payload into an error message.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SpecDrift {
    desired_spec_sha: String,
    observed_spec_sha: String,
    fields: Vec<SpecFieldDrift>,
    omitted_fields: usize,
}

impl fmt::Display for SpecDrift {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "desired_spec_sha={} observed_spec_sha={} differing_fields={}",
            self.desired_spec_sha,
            self.observed_spec_sha,
            self.fields.len() + self.omitted_fields
        )?;
        for field in &self.fields {
            write!(f, "; [{}] {}", field.field, field.kind.label())?;
            if let Some(excerpt) = &field.desired_excerpt {
                write!(f, " desired={excerpt}")?;
            }
            if let Some(excerpt) = &field.observed_excerpt {
                write!(f, " observed={excerpt}")?;
            }
        }
        if self.omitted_fields > 0 {
            write!(
                f,
                "; [+{} differing field(s) not shown]",
                self.omitted_fields
            )?;
        }
        Ok(())
    }
}

/// Renders one JSON value as a compact excerpt capped at
/// [`DRIFT_EXCERPT_BYTES`], cutting on a UTF-8 boundary and stating how many
/// bytes were withheld instead of silently truncating.
fn bounded_excerpt(value: &Value) -> String {
    let raw = serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".into());
    bounded_text(&raw)
}

/// Caps any diagnostic text at [`DRIFT_EXCERPT_BYTES`] on a UTF-8 boundary,
/// stating how many bytes were withheld instead of silently truncating.
/// Applied to value excerpts and to top-level spec key names alike: an
/// observed object controls both. Also bounds the last failed RuntimeClass
/// read's stderr so a hung API server cannot bloat the readiness failure.
pub(crate) fn bounded_text(text: &str) -> String {
    if text.len() <= DRIFT_EXCERPT_BYTES {
        return text.to_owned();
    }
    let mut cut = DRIFT_EXCERPT_BYTES;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…(+{} bytes)", &text[..cut], text.len() - cut)
}

/// Compares two specs field by field at the top level, which is where
/// NetworkPolicy specs carry their meaning (`podSelector`, `policyTypes`,
/// `ingress`, `egress`). Keys are visited in sorted order so the rendered
/// evidence is deterministic. Non-object shapes (including a missing spec)
/// collapse to one whole-spec entry rather than panicking or lying.
fn spec_drift(desired: &Value, observed: &Value) -> SpecDrift {
    let mut fields = Vec::new();
    let mut omitted_fields = 0usize;
    match (desired.as_object(), observed.as_object()) {
        (Some(desired_map), Some(observed_map)) => {
            let mut keys: BTreeSet<&String> = desired_map.keys().collect();
            keys.extend(observed_map.keys());
            for key in keys {
                let kind = match (desired_map.get(key), observed_map.get(key)) {
                    (Some(_), None) => DriftKind::Missing,
                    (None, Some(_)) => DriftKind::Unexpected,
                    (Some(desired_value), Some(observed_value)) => {
                        if desired_value == observed_value {
                            continue;
                        }
                        DriftKind::Changed
                    }
                    (None, None) => continue,
                };
                if fields.len() >= DRIFT_MAX_FIELDS {
                    omitted_fields += 1;
                    continue;
                }
                fields.push(SpecFieldDrift {
                    field: bounded_text(key),
                    kind,
                    desired_excerpt: desired_map.get(key).map(bounded_excerpt),
                    observed_excerpt: observed_map.get(key).map(bounded_excerpt),
                });
            }
        }
        _ => {
            fields.push(SpecFieldDrift {
                field: "<spec>".into(),
                kind: DriftKind::Changed,
                desired_excerpt: Some(bounded_excerpt(desired)),
                observed_excerpt: Some(bounded_excerpt(observed)),
            });
        }
    }
    SpecDrift {
        desired_spec_sha: spec_sha(desired),
        observed_spec_sha: spec_sha(observed),
        fields,
        omitted_fields,
    }
}

#[cfg(test)]
mod drift_tests {
    use super::*;

    #[test]
    fn equal_specs_report_no_drift() {
        let spec = serde_json::json!({"podSelector": {}, "ingress": []});
        let drift = spec_drift(&spec, &spec);
        assert_eq!(drift.fields, vec![]);
        assert_eq!(drift.omitted_fields, 0);
        assert_eq!(drift.desired_spec_sha, drift.observed_spec_sha);
    }

    #[test]
    fn missing_unexpected_and_changed_are_named_deterministically() {
        let desired = serde_json::json!({
            "policyTypes": ["Ingress", "Egress"],
            "ingress": [],
            "egress": [{"ports": [{"port": 53}]}],
        });
        let observed = serde_json::json!({
            "policyTypes": ["Ingress"],
            "egress": [{"ports": [{"port": 5353}]}],
            "extra": true,
        });
        let rendered = spec_drift(&desired, &observed).to_string();
        // Sorted key order, exact kinds, one entry per differing field.
        assert_eq!(
            rendered.matches(';').count(),
            4,
            "exactly four entries expected: {rendered}"
        );
        assert!(rendered.contains("[egress] changed"), "{rendered}");
        assert!(
            rendered.contains("[extra] not-in-desired observed=true"),
            "{rendered}"
        );
        assert!(
            rendered.contains("[ingress] missing-from-observed desired=[]"),
            "{rendered}"
        );
        assert!(rendered.contains("[policyTypes] changed"), "{rendered}");
        assert!(rendered.starts_with("desired_spec_sha="), "{rendered}");
    }

    #[test]
    fn excerpts_are_capped_and_state_the_withheld_remainder() {
        let big = serde_json::json!("x".repeat(1000));
        let excerpt = bounded_excerpt(&big);
        // The quoted 1000-byte string serializes to 1002 bytes; 192 are
        // kept, so 810 are withheld and named.
        assert_eq!(excerpt.len(), DRIFT_EXCERPT_BYTES + "…(+810 bytes)".len());
        assert!(excerpt.ends_with("(+810 bytes)"), "{excerpt}");
        assert!(excerpt.starts_with('"'));
    }

    #[test]
    fn non_object_specs_collapse_to_one_whole_spec_entry() {
        let drift = spec_drift(&serde_json::json!({}), &Value::Null);
        assert_eq!(drift.fields.len(), 1);
        assert_eq!(drift.fields[0].field, "<spec>");
        assert_eq!(drift.fields[0].kind, DriftKind::Changed);
        assert_eq!(drift.fields[0].observed_excerpt.as_deref(), Some("null"));
    }

    #[test]
    fn more_than_the_field_cap_are_counted_not_rendered() {
        let desired = serde_json::json!({});
        let observed = serde_json::Value::Object(
            (0..12)
                .map(|index| (format!("f{index:02}"), serde_json::json!(index)))
                .collect(),
        );
        let drift = spec_drift(&desired, &observed);
        assert_eq!(drift.fields.len(), DRIFT_MAX_FIELDS);
        assert_eq!(drift.omitted_fields, 4);
        assert!(
            drift
                .to_string()
                .contains("[+4 differing field(s) not shown]")
        );
    }

    #[test]
    fn overlong_observed_key_names_share_the_excerpt_bound() {
        let long_key = "k".repeat(500);
        let mut observed = serde_json::Map::new();
        observed.insert(long_key.clone(), serde_json::json!(1));
        let drift = spec_drift(&serde_json::json!({}), &Value::Object(observed));
        assert_eq!(drift.fields.len(), 1);
        // The key is capped by the same UTF-8-safe bound as value excerpts.
        assert_eq!(
            drift.fields[0].field,
            format!("{}…(+308 bytes)", "k".repeat(DRIFT_EXCERPT_BYTES))
        );
    }

    #[test]
    fn omitted_ingress_with_ingress_isolation_canonicalizes_to_empty() {
        let observed = serde_json::json!({
            "podSelector": {},
            "policyTypes": ["Ingress", "Egress"],
            // The stored shape from the local API-server capture: the key
            // pruned, meaning default-deny just like the desired `[]`.
            "egress": [{"ports": [{"protocol": "UDP", "port": 53}]}],
        });
        let desired = serde_json::json!({
            "podSelector": {},
            "policyTypes": ["Ingress", "Egress"],
            "ingress": [],
            "egress": [{"ports": [{"protocol": "UDP", "port": 53}]}],
        });
        assert_eq!(*canonicalize_observed_spec(&observed), desired);
    }

    #[test]
    fn omitted_ingress_without_ingress_policy_type_stays_real_drift() {
        let observed = serde_json::json!({"policyTypes": ["Egress"]});
        assert!(matches!(
            canonicalize_observed_spec(&observed),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn missing_policy_types_never_earns_the_ingress_equivalence() {
        let observed = serde_json::json!({"podSelector": {}});
        assert!(matches!(
            canonicalize_observed_spec(&observed),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn present_ingress_key_is_never_rewritten() {
        let observed = serde_json::json!({
            "policyTypes": ["Ingress"],
            "ingress": [{"from": [{"podSelector": {"matchLabels": {"app": "x"}}}]}],
        });
        assert!(matches!(
            canonicalize_observed_spec(&observed),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn non_object_specs_pass_through_untouched() {
        assert!(matches!(
            canonicalize_observed_spec(&Value::Null),
            Cow::Borrowed(_)
        ));
    }
}
