//! Observe, plan, and execute one Workspace until WaitPod, Converged, or bound.

use std::path::Path;

use crate::observe::classify_workspace_pod;
use crate::realize::{encrypted_mapper_device, lv_name_for, object_names};
use crate::reconcile::workspace::{
    WorkspaceAction, WorkspaceDesired, WorkspaceLocal, WorkspaceObserved, WorkspacePod,
    plan_workspace,
};
use crate::specs::workspace::{WorkspaceDesiredName, WorkspaceSpec};
use crate::{Fabric, FabricError, GenerationRow, VolumeKind, WorkspaceRow};

const MAX_STEPS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceStatus {
    pub desired_revision: i64,
    pub observed_revision: i64,
    pub desired_state: String,
    pub observed_state: String,
    pub last_error: Option<String>,
}

pub fn persist_workspace_spec_for(
    fabric: &Fabric,
    workspace_id: &str,
    spec: &WorkspaceSpec,
) -> Result<crate::specs::accept::DesiredSpecAcceptance, FabricError> {
    let typed = serde_json::to_string(spec)
        .map_err(|_| FabricError::Store("cannot encode workspace spec".into()))?;
    crate::specs::accept::require_spec_write(fabric.store.accept_resource_spec(
        "workspace",
        workspace_id,
        spec.revision,
        &spec.hash_bytes(),
        &typed,
    )?)
}

pub async fn reconcile_workspace(
    fabric: &Fabric,
    workspace_id: &str,
) -> Result<WorkspaceStatus, FabricError> {
    let _lock = fabric.lifecycle_guard(workspace_id).await;
    reconcile_workspace_locked(fabric, workspace_id).await
}

/// GET/observe must not wait on restore or exec. A contended lifecycle
/// lock means those callers should read sqlite + the live view instead.
pub async fn try_reconcile_workspace(
    fabric: &Fabric,
    workspace_id: &str,
) -> Result<Option<WorkspaceStatus>, FabricError> {
    let Some(_lock) = fabric.try_lifecycle_guard(workspace_id) else {
        return Ok(None);
    };
    Ok(Some(
        reconcile_workspace_locked(fabric, workspace_id).await?,
    ))
}

pub fn status_from_spec_row(row: &crate::store::ResourceSpecRow) -> WorkspaceStatus {
    let desired = serde_json::from_str::<WorkspaceSpec>(&row.typed_spec)
        .map(|spec| spec.desired.as_str().to_owned())
        .unwrap_or_default();
    WorkspaceStatus {
        desired_revision: row.desired_revision,
        observed_revision: row.observed_revision,
        desired_state: desired,
        observed_state: row.state.clone(),
        last_error: row.last_error.clone(),
    }
}

async fn reconcile_workspace_locked(
    fabric: &Fabric,
    workspace_id: &str,
) -> Result<WorkspaceStatus, FabricError> {
    let Some(row) = fabric.store.get_resource_spec("workspace", workspace_id)? else {
        return Err(FabricError::NotFound);
    };
    let spec: WorkspaceSpec = serde_json::from_str(&row.typed_spec)
        .map_err(|_| FabricError::Store("workspace spec is unusable".into()))?;
    if spec.planner_desired() == WorkspaceDesired::Active {
        if let Some(workspace) = fabric.store.get_workspace(workspace_id)? {
            if workspace.state == "deleting" || workspace.state == "deleted" {
                return Ok(WorkspaceStatus {
                    desired_revision: spec.revision,
                    observed_revision: row.observed_revision,
                    desired_state: spec.desired.as_str().into(),
                    observed_state: workspace.state,
                    last_error: None,
                });
            }
        }
    }
    for _ in 0..MAX_STEPS {
        let observed = observe_workspace(fabric, workspace_id).await?;
        let local = WorkspaceLocal {
            materialized: fabric
                .get_allocation(VolumeKind::Workspace, workspace_id)?
                .is_some_and(|row| row.state == "allocated"),
        };
        let want = spec.volume_bytes_for(fabric.live().storage());
        let have = fabric
            .get_allocation(VolumeKind::Workspace, workspace_id)?
            .map(|row| row.allocated_bytes)
            .unwrap_or(0);
        let action = if spec.planner_desired() == WorkspaceDesired::Active
            && observed.lv
            && want > have
            && want > 0
        {
            WorkspaceAction::GrowVolume
        } else {
            plan_workspace(spec.planner_desired(), local, observed)
        };
        match action {
            WorkspaceAction::Converged => {
                let observed_state =
                    record_workspace_converged(fabric, workspace_id, &spec).await?;
                fabric.store.set_resource_spec_observed(
                    "workspace",
                    workspace_id,
                    spec.revision,
                    &observed_state,
                    None,
                )?;
                return Ok(WorkspaceStatus {
                    desired_revision: spec.revision,
                    observed_revision: spec.revision,
                    desired_state: spec.desired.as_str().into(),
                    observed_state,
                    last_error: None,
                });
            }
            WorkspaceAction::WaitPod => {
                return Ok(WorkspaceStatus {
                    desired_revision: spec.revision,
                    observed_revision: row.observed_revision,
                    desired_state: spec.desired.as_str().into(),
                    observed_state: row.state.clone(),
                    last_error: None,
                });
            }
            WorkspaceAction::Lost => {
                fabric.store.set_resource_spec_observed(
                    "workspace",
                    workspace_id,
                    row.observed_revision,
                    "lost",
                    Some("durable_volume_missing"),
                )?;
                return Ok(WorkspaceStatus {
                    desired_revision: spec.revision,
                    observed_revision: row.observed_revision,
                    desired_state: spec.desired.as_str().into(),
                    observed_state: "lost".into(),
                    last_error: Some("durable_volume_missing".into()),
                });
            }
            other => {
                if let Err(error) = execute_workspace(fabric, workspace_id, &spec, other).await {
                    let code = last_error_code(&error);
                    fabric.store.set_resource_spec_observed(
                        "workspace",
                        workspace_id,
                        row.observed_revision,
                        "failed",
                        Some(code),
                    )?;
                    return Err(error);
                }
            }
        }
    }
    Err(FabricError::Realize(
        "workspace reconcile did not settle".into(),
    ))
}

/// Residue-gated volume release is idempotent. Startup and a runtime tick
/// both retry `deleting` / incomplete `deleted` rows so a PUT that returned
/// `deleting` is not stuck until the next daemon restart.
pub async fn retry_held_workspace_releases(fabric: &Fabric) -> Result<(), FabricError> {
    for workspace in fabric.store.list_workspaces()? {
        match fabric.workspace_needs_volume_release(&workspace) {
            Ok(true) => {
                if let Err(error) = fabric.delete_workspace(&workspace.id).await {
                    eprintln!(
                        "voie-fabricd: workspace {} residue release: {error}",
                        workspace.id
                    );
                }
            }
            Ok(false) => {}
            Err(error) => eprintln!(
                "voie-fabricd: workspace {} release check: {error}",
                workspace.id
            ),
        }
    }
    Ok(())
}

pub async fn reconcile_accepted_workspaces(fabric: &Fabric) -> Result<(), FabricError> {
    // Residue release can wait out a leftover delete. WaitPod observation
    // must not sit behind that wait or a Ready workspace stays `accepted`.
    let release = retry_held_workspace_releases(fabric);
    let heal = heal_accepted_workspace_specs(fabric);
    let (release, heal) = tokio::join!(release, heal);
    release?;
    heal
}

async fn heal_accepted_workspace_specs(fabric: &Fabric) -> Result<(), FabricError> {
    let mut ids: Vec<String> = fabric
        .store
        .list_resource_specs("workspace")?
        .into_iter()
        .map(|row| row.resource_id)
        .collect();
    for workspace in fabric.store.list_workspaces()? {
        if workspace.state == "deleting" || workspace.state == "deleted" {
            continue;
        }
        if let Err(error) = ensure_realized_active_spec(fabric, &workspace.id) {
            eprintln!("voie-fabricd: workspace {} spec: {error}", workspace.id);
            continue;
        }
        if !ids.iter().any(|id| id == &workspace.id) {
            ids.push(workspace.id);
        }
    }
    for id in ids {
        if let Ok(Some(workspace)) = fabric.store.get_workspace(&id) {
            if workspace.state == "deleted" {
                // Residue-gated delete already finished. Re-entering
                // `delete_workspace` can lvremove leftover names. Align a
                // leftover Active spec to Deleted and observe it so GET
                // matches sqlite without reminting empty capacity.
                if let Err(error) = settle_retired_workspace_spec(fabric, &id) {
                    eprintln!("voie-fabricd: workspace {id} retired spec: {error}");
                }
                continue;
            }
            if workspace.state == "deleting" {
                continue;
            }
        }
        let Some(_lock) = fabric.try_lifecycle_guard(&id) else {
            continue;
        };
        if let Err(error) = reconcile_workspace_locked(fabric, &id).await {
            eprintln!("voie-fabricd: workspace {id} reconcile: {error}");
        }
    }
    Ok(())
}

/// Sqlite is already `deleted`. Do not remint. A leftover Active spec is
/// teardown that Control already settled; record Deleted and observe it.
fn settle_retired_workspace_spec(fabric: &Fabric, workspace_id: &str) -> Result<(), FabricError> {
    persist_deleted_spec_if_needed(fabric, workspace_id)?;
    if let Some(row) = fabric.store.get_resource_spec("workspace", workspace_id)? {
        fabric.store.set_resource_spec_observed(
            "workspace",
            workspace_id,
            row.desired_revision,
            "deleted",
            None,
        )?;
    }
    Ok(())
}

/// Keep-list and other pre-spec Workspaces hold an LV and a ready sqlite
/// row but no typed spec. Control will not PUT while revisions stay 0, so
/// GET and startup must persist Active and heal the guest.
pub fn ensure_realized_active_spec(fabric: &Fabric, workspace_id: &str) -> Result<(), FabricError> {
    if fabric
        .store
        .get_resource_spec("workspace", workspace_id)?
        .is_some()
    {
        return Ok(());
    }
    let Some(workspace) = fabric.store.get_workspace(workspace_id)? else {
        return Ok(());
    };
    if workspace.state == "deleting" || workspace.state == "deleted" {
        return Ok(());
    }
    let Some(allocation) = fabric.get_allocation(VolumeKind::Workspace, workspace_id)? else {
        return Ok(());
    };
    if allocation.state != "allocated" {
        return Ok(());
    }
    persist_from_fields(
        fabric,
        workspace_id,
        1,
        WorkspaceDesiredName::Active,
        allocation.allocated_bytes,
    )
}

async fn observe_workspace(
    fabric: &Fabric,
    workspace_id: &str,
) -> Result<WorkspaceObserved, FabricError> {
    let allocation = fabric.get_allocation(VolumeKind::Workspace, workspace_id)?;
    let lv_name = allocation
        .as_ref()
        .map(|row| row.lv_name.clone())
        .unwrap_or_else(|| lv_name_for(workspace_id));
    let lv_path = format!("/dev/{}/{}", fabric.live().vg_name(), lv_name);
    let lv = Path::new(&lv_path).exists();
    let mapper = Path::new(&encrypted_mapper_device(&lv_name)).exists();
    let (pv_name, _, pod_name) = workspace_object_names(fabric, workspace_id);
    let pv = fabric.live().get_pv(&pv_name).await?.is_some();
    let pod = match fabric.live().get_pod(&pod_name).await? {
        Some(info) => {
            classify_workspace_pod(&info.phase, info.ready, info.waiting_reason.as_deref())
        }
        None => WorkspacePod::Absent,
    };
    Ok(WorkspaceObserved {
        lv,
        mapper,
        pv,
        pod,
    })
}

fn workspace_object_names(fabric: &Fabric, workspace_id: &str) -> (String, String, String) {
    if let Ok(Some(row)) = fabric.store.get_workspace(workspace_id) {
        let generation = fabric
            .store
            .latest_generation(workspace_id)
            .ok()
            .flatten()
            .map(|row| row.generation)
            .unwrap_or(1);
        let pod = fabric
            .store
            .latest_generation(workspace_id)
            .ok()
            .flatten()
            .map(|row| row.pod_name)
            .unwrap_or_else(|| object_names(workspace_id, generation).2);
        return (row.pv_name, row.pvc_name, pod);
    }
    object_names(workspace_id, 1)
}

async fn execute_workspace(
    fabric: &Fabric,
    workspace_id: &str,
    spec: &WorkspaceSpec,
    action: WorkspaceAction,
) -> Result<(), FabricError> {
    match action {
        WorkspaceAction::AllocateLv => {
            refuse_workspace_capacity_while_tearing_down(fabric, workspace_id)?;
            fabric.ensure_runtime_class().await?;
            let bytes = spec.volume_bytes_for(fabric.live().storage());
            if bytes == 0 {
                return Err(FabricError::Config(
                    "storageTier or volumeBytes is required",
                ));
            }
            let slot = fabric
                .allocate_volume(VolumeKind::Workspace, workspace_id, bytes, None)
                .await?;
            if fabric.live().device_mounted(&slot.device).await? {
                return Err(FabricError::Foreign(format!(
                    "reserved device {} is already mounted",
                    slot.device
                )));
            }
            fabric.live().mkfs_ext4_if_needed(&slot.device).await?;
            Ok(())
        }
        WorkspaceAction::GrowVolume => {
            let bytes = spec.volume_bytes_for(fabric.live().storage());
            if bytes == 0 {
                return Err(FabricError::Config(
                    "storageTier or volumeBytes is required",
                ));
            }
            fabric
                .grow_workspace_while_lifecycle_held(workspace_id, bytes)
                .await?;
            Ok(())
        }
        WorkspaceAction::EnsureMapper => {
            refuse_workspace_capacity_while_tearing_down(fabric, workspace_id)?;
            let Some(row) = fabric.get_allocation(VolumeKind::Workspace, workspace_id)? else {
                return Err(FabricError::Realize("workspace LV is not allocated".into()));
            };
            fabric.live().reopen_encrypted_lv(&row.lv_name).await?;
            Ok(())
        }
        WorkspaceAction::CreatePv => {
            refuse_workspace_capacity_while_tearing_down(fabric, workspace_id)?;
            let Some(slot) = fabric.get_allocation(VolumeKind::Workspace, workspace_id)? else {
                return Err(FabricError::Realize("workspace LV is not allocated".into()));
            };
            let device = encrypted_mapper_device(&slot.lv_name);
            crate::realize::require_stable_block_path(&device)?;
            fabric.live().mkfs_ext4_if_needed(&device).await?;
            let (pv_name, pvc_name, _) = object_names(workspace_id, 1);
            fabric.store.reserve_volume(
                workspace_id,
                &device,
                fabric.live().node_name(),
                &pv_name,
            )?;
            fabric.live().ensure_namespace().await?;
            fabric.live().ensure_storage_class().await?;
            fabric
                .live()
                .apply_yaml(&fabric.live().pv_yaml(
                    workspace_id,
                    &pv_name,
                    &device,
                    slot.allocated_bytes,
                ))
                .await?;
            fabric
                .live()
                .apply_yaml(&fabric.live().pvc_yaml(
                    workspace_id,
                    &pvc_name,
                    &pv_name,
                    slot.allocated_bytes,
                ))
                .await?;
            fabric.store.upsert_workspace(&WorkspaceRow {
                id: workspace_id.to_owned(),
                state: "creating".into(),
                device,
                node: fabric.live().node_name().to_owned(),
                pv_name,
                pvc_name,
                lv_name: Some(slot.lv_name),
            })?;
            Ok(())
        }
        WorkspaceAction::CreatePod | WorkspaceAction::ReplacePod => {
            refuse_workspace_capacity_while_tearing_down(fabric, workspace_id)?;
            fabric.ensure_runtime_class().await?;
            let generation = fabric
                .store
                .latest_generation(workspace_id)?
                .map(|row| row.generation)
                .unwrap_or(1);
            let (pv_name, pvc_name, pod_name) = object_names(workspace_id, generation);
            if action == WorkspaceAction::ReplacePod {
                crate::product::delete_named_retryable(fabric, "pod", &pod_name, true, 60).await?;
            }
            fabric.live().ensure_workspace_service_account().await?;
            fabric.ensure_network_policy().await?;
            fabric
                .live()
                .apply_yaml(
                    &fabric
                        .live()
                        .pod_yaml(workspace_id, &pod_name, &pvc_name, generation),
                )
                .await?;
            if fabric.store.latest_generation(workspace_id)?.is_none() {
                fabric.store.insert_generation(&GenerationRow {
                    workspace_id: workspace_id.to_owned(),
                    generation,
                    pod_name,
                    pod_uid: None,
                    sandbox_id: None,
                    state: "creating".into(),
                })?;
            }
            let _ = pv_name;
            Ok(())
        }
        WorkspaceAction::RemovePod => {
            let (_, _, pod_name) = workspace_object_names(fabric, workspace_id);
            crate::product::delete_named_retryable(fabric, "pod", &pod_name, true, 60).await
        }
        WorkspaceAction::RemovePv => {
            let (pv_name, pvc_name, _) = workspace_object_names(fabric, workspace_id);
            crate::product::delete_named_retryable(fabric, "pvc", &pvc_name, true, 30).await?;
            crate::product::delete_named_retryable(fabric, "pv", &pv_name, false, 30).await
        }
        WorkspaceAction::RemoveMapper => Ok(()),
        WorkspaceAction::RemoveLv => Err(FabricError::Realize(
            "workspace volume release requires residue-gated delete".into(),
        )),
        WorkspaceAction::Lost => Err(FabricError::Realize(
            "workspace volume is lost; recovery is an explicit restore".into(),
        )),
        WorkspaceAction::WaitPod | WorkspaceAction::Converged => Ok(()),
    }
}

fn refuse_workspace_capacity_while_tearing_down(
    fabric: &Fabric,
    workspace_id: &str,
) -> Result<(), FabricError> {
    if let Some(workspace) = fabric.store.get_workspace(workspace_id)? {
        if workspace.state == "deleting" || workspace.state == "deleted" {
            return Err(FabricError::Realize(
                "workspace volume is residue-gated".into(),
            ));
        }
    }
    Ok(())
}

/// Record Deleted before residue teardown so reboot does not recreate
/// the guest from a leftover Active spec. A Control PUT that already
/// stored Deleted must not bump the revision.
pub fn persist_deleted_spec_if_needed(
    fabric: &Fabric,
    workspace_id: &str,
) -> Result<(), FabricError> {
    if let Some(row) = fabric.store.get_resource_spec("workspace", workspace_id)? {
        if let Ok(spec) = serde_json::from_str::<WorkspaceSpec>(&row.typed_spec) {
            if spec.desired == WorkspaceDesiredName::Deleted {
                return Ok(());
            }
        }
    }
    persist_deleted_spec(fabric, workspace_id)
}

pub fn persist_deleted_spec(fabric: &Fabric, workspace_id: &str) -> Result<(), FabricError> {
    let revision = fabric
        .store
        .get_resource_spec("workspace", workspace_id)?
        .map(|row| row.desired_revision.max(1) + 1)
        .unwrap_or(1);
    let volume_bytes = fabric
        .get_allocation(VolumeKind::Workspace, workspace_id)?
        .map(|row| row.allocated_bytes)
        .unwrap_or(0);
    persist_from_fields(
        fabric,
        workspace_id,
        revision,
        WorkspaceDesiredName::Deleted,
        volume_bytes,
    )
}

/// Persist an accepted spec so reboot reconcilers have typed truth.
pub fn persist_from_fields(
    fabric: &Fabric,
    workspace_id: &str,
    revision: i64,
    desired: WorkspaceDesiredName,
    volume_bytes: u64,
) -> Result<(), FabricError> {
    persist_workspace_spec_for(
        fabric,
        workspace_id,
        &WorkspaceSpec {
            revision: revision.max(1),
            desired,
            runtime_profile: "workspace-v1".into(),
            storage_tier: String::new(),
            volume_bytes,
        },
    )
    .map(|_| ())
}

fn last_error_code(error: &FabricError) -> &'static str {
    let text = error.to_string();
    if text.contains("RuntimeClass") || text.contains("runtime_class") {
        return "runtime_class_unavailable";
    }
    match error {
        FabricError::Config(_) => "invalid_spec",
        FabricError::NotFound => "not_found",
        FabricError::Unknown(_) => "unknown",
        _ => "realize_failed",
    }
}

async fn record_workspace_converged(
    fabric: &Fabric,
    workspace_id: &str,
    spec: &WorkspaceSpec,
) -> Result<String, FabricError> {
    if spec.desired == WorkspaceDesiredName::Deleted {
        let view = fabric.delete_workspace_locked(workspace_id).await?;
        return Ok(view.state);
    }
    if spec.planner_desired() != WorkspaceDesired::Active {
        let state = match spec.desired {
            WorkspaceDesiredName::Suspended => "suspended",
            WorkspaceDesiredName::Archived => "archived",
            WorkspaceDesiredName::Deleted => "deleted",
            WorkspaceDesiredName::Active => "ready",
        };
        if fabric.store.get_workspace(workspace_id)?.is_some() {
            fabric.store.set_workspace_state(workspace_id, state)?;
        }
        return Ok(state.to_owned());
    }
    let generation = fabric
        .store
        .latest_generation(workspace_id)?
        .map(|row| row.generation)
        .unwrap_or(1);
    let (_, _, pod_name) = object_names(workspace_id, generation);
    if let Ok(Some(pod)) = fabric.live().get_pod(&pod_name).await {
        if fabric.store.latest_generation(workspace_id)?.is_some() {
            fabric.store.update_generation_runtime(
                workspace_id,
                generation,
                &pod.uid,
                pod.sandbox_id.as_deref(),
                "running",
            )?;
        } else {
            fabric.store.insert_generation(&GenerationRow {
                workspace_id: workspace_id.to_owned(),
                generation,
                pod_name: pod_name.clone(),
                pod_uid: Some(pod.uid.clone()),
                sandbox_id: pod.sandbox_id.clone(),
                state: "running".into(),
            })?;
        }
    }
    if fabric.store.get_workspace(workspace_id)?.is_some() {
        fabric.store.set_workspace_state(workspace_id, "ready")?;
    }
    Ok("ready".into())
}
