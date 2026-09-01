//! Workspace, exec, replace, and cleanup orchestration.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::FabricError;
use crate::realize::{
    BlockSlot, ExecVerdict, Live, NETWORK_POLICY_NAME, Residue, classify_exec, encrypted_mapper_device,
    is_daemon_lv_name, lv_name_for, managed, object_names, require_stable_block_path,
    restore_object_names, spec_sha,
};
use crate::product_realize::{
    app_pod_name, deployment_volume_name, postgres_network_policy_name, postgres_pod_for_lv,
    postgres_pvc_for_lv, postgres_runtime_pod_yaml,
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
    /// Ready sqlite rows whose LV is gone (for example after a VG DESTROY).
    /// Listed only; leftover capacity is never minted to recreate them.
    pub ready_without_volume: Vec<String>,
    /// Allocation rows released because the LV is gone or the workspace
    /// claim has no live workspace row and no held reservation.
    pub orphan_allocations_released: Vec<String>,
    /// Claimed LVs whose crypt mapping was reopened after reboot.
    pub encrypted_volumes_reopened: Vec<String>,
    /// Claimed LVs that could not be reopened (missing key or cryptsetup).
    pub encrypted_reopen_failures: Vec<String>,
    /// PersistentVolumes whose recycled `/dev/dm-N` path was replaced.
    pub stale_pvs_replaced: Vec<String>,
    /// Guest pods re-applied because they were not Ready after reboot.
    pub pods_rebound: Vec<String>,
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
    #[serde(default, rename = "allocatedBytes")]
    pub allocated_bytes: u64,
    /// Observed guest image when Fabric could read the running Pod. Empty
    /// when the Pod is absent or kubectl observation failed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub image: String,
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
    release_root: PathBuf,
    stage_root: PathBuf,
    /// One lifecycle key per workspace id. Create, replace, delete, and exec
    /// hold their workspace's key for the whole operation so concurrent
    /// requests can never interleave two lifecycles on one workspace while
    /// different workspaces proceed independently.
    lifecycles: Mutex<BTreeMap<String, std::sync::Arc<AsyncMutex<()>>>>,
    /// One Fabric-wide lock around observe → admit → reserve → lvcreate so
    /// concurrent Workspaces, Databases, and Deployments cannot both pass
    /// against the same observed total.
    storage_alloc: AsyncMutex<()>,
}

fn usable_volume_group(name: &str) -> Result<&str, FabricError> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+'))
    {
        return Err(FabricError::Config(
            "VOIE_WORKSPACE_VG is not a usable volume group name",
        ));
    }
    Ok(name)
}

/// Mount the configured staging LV. Production uses
/// `VOIE_FABRICD_STAGE_MODE=lvm` with `VOIE_FABRICD_STAGE_VOLUME=vg/lv`.
/// Local VMs must set `VOIE_FABRICD_STAGE_MODE=dev-directory` explicitly.
fn ensure_stage_volume_mounted(stage_root: &Path) -> Result<(), FabricError> {
    std::fs::create_dir_all(stage_root)
        .map_err(|error| FabricError::Realize(format!("cannot create staging root: {error}")))?;
    let mode = std::env::var("VOIE_FABRICD_STAGE_MODE").ok();
    #[cfg(test)]
    let mode = mode.or_else(|| Some("dev-directory".into()));
    let volume = std::env::var("VOIE_FABRICD_STAGE_VOLUME").ok();
    let Some((vg, lv)) = require_stage_mode(mode.as_deref(), volume.as_deref())? else {
        return Ok(());
    };
    let vg = usable_volume_group(&vg)?;
    let lv = usable_volume_group(&lv)?;
    let spec = format!("{vg}/{lv}");
    let listed = Command::new("lvs")
        .arg(&spec)
        .output()
        .map_err(|error| FabricError::Realize(format!("lvs staging volume: {error}")))?;
    if !listed.status.success() {
        return Err(FabricError::Realize(
            "configured staging volume is absent; refusing OS-disk fallback".into(),
        ));
    }
    // Thin stage LVs need the workspace pool active first. `--noudevsync`
    // skips udev node creation and lvchange then fails with tmeta missing.
    let pool = Command::new("lvs")
        .args(["--noheadings", "-o", "pool_lv", &spec])
        .output()
        .map_err(|error| FabricError::Realize(format!("lvs staging pool: {error}")))?;
    if pool.status.success() {
        let pool_lv = String::from_utf8_lossy(&pool.stdout).trim().to_string();
        if !pool_lv.is_empty() {
            let pool_spec = format!("{vg}/{pool_lv}");
            let activated_pool = Command::new("lvchange")
                .args(["--activate", "y", &pool_spec])
                .output()
                .map_err(|error| FabricError::Realize(format!("lvchange staging pool: {error}")))?;
            if !activated_pool.status.success() {
                return Err(FabricError::Realize(format!(
                    "cannot activate staging pool: {}",
                    String::from_utf8_lossy(&activated_pool.stderr).trim()
                )));
            }
        }
    }
    let activated = Command::new("lvchange")
        .args(["--activate", "y", &spec])
        .output()
        .map_err(|error| FabricError::Realize(format!("lvchange staging volume: {error}")))?;
    if !activated.status.success() {
        return Err(FabricError::Realize(format!(
            "cannot activate staging volume: {}",
            String::from_utf8_lossy(&activated.stderr).trim()
        )));
    }
    let device = format!("/dev/{spec}");
    for _ in 0..40 {
        if Path::new(&device).exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if !Path::new(&device).exists() {
        return Err(FabricError::Realize(
            "staging volume did not appear after activation".into(),
        ));
    }
    let source_ok = |stdout: &[u8]| -> bool {
        staging_source_from_vg(&String::from_utf8_lossy(stdout), vg, lv)
    };
    let current = Command::new("findmnt")
        .args(["-n", "-o", "SOURCE"])
        .arg(stage_root)
        .output()
        .map_err(|error| FabricError::Realize(format!("findmnt staging root: {error}")))?;
    if current.status.success() {
        if source_ok(&current.stdout) {
            return Ok(());
        }
        return Err(FabricError::Realize(format!(
            "staging root {} is not mounted from the Fabric volume group",
            stage_root.display()
        )));
    }
    let mounted = Command::new("mount")
        .args(["-o", "noatime", &device])
        .arg(stage_root)
        .output()
        .map_err(|error| FabricError::Realize(format!("mount staging volume: {error}")))?;
    if !mounted.status.success() {
        return Err(FabricError::Realize(format!(
            "mount staging volume failed: {}",
            String::from_utf8_lossy(&mounted.stderr).trim()
        )));
    }
    let verify = Command::new("findmnt")
        .args(["-n", "-o", "SOURCE"])
        .arg(stage_root)
        .output()
        .map_err(|error| FabricError::Realize(format!("findmnt staging root: {error}")))?;
    if !verify.status.success() || !source_ok(&verify.stdout) {
        return Err(FabricError::Realize(
            "staging volume did not mount from the Fabric data disk".into(),
        ));
    }
    Ok(())
}

fn staging_source_from_vg(source: &str, vg: &str, lv: &str) -> bool {
    let source = source.trim();
    let mapper_vg = vg.replace('-', "--");
    let mapper_lv = lv.replace('-', "--");
    source == format!("/dev/{vg}/{lv}")
        || source == format!("/dev/mapper/{mapper_vg}-{mapper_lv}")
}

fn slug_kind_from_object(value: &Value) -> (String, String) {
    let labels = value.pointer("/metadata/labels");
    let slug = labels
        .and_then(|item| item.get("io.voie/slug"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let kind = match labels
        .and_then(|item| item.get("io.voie/environment"))
        .and_then(Value::as_str)
    {
        Some("prod") => "prod".to_owned(),
        _ => "dev".to_owned(),
    };
    (slug, kind)
}

fn unlink_staged_path(path: &Path) -> Result<(), FabricError> {
    if path.exists() {
        std::fs::remove_file(path)
            .map_err(|error| FabricError::Realize(format!("cannot unlink staged file: {error}")))?;
    }
    if path.exists() {
        return Err(FabricError::Realize(
            "staged file remains after unlink; staging slot is not released".into(),
        ));
    }
    Ok(())
}

fn require_stage_mode(
    mode: Option<&str>,
    volume: Option<&str>,
) -> Result<Option<(String, String)>, FabricError> {
    match mode {
        Some("dev-directory") => Ok(None),
        Some("lvm") => {
            let required = volume
                .filter(|value| !value.is_empty())
                .ok_or(FabricError::Config(
                    "VOIE_FABRICD_STAGE_VOLUME is required when STAGE_MODE=lvm",
                ))?;
            let (vg, lv) = required.split_once('/').ok_or(FabricError::Config(
                "VOIE_FABRICD_STAGE_VOLUME must be vg/lv",
            ))?;
            Ok(Some((vg.to_owned(), lv.to_owned())))
        }
        Some(_) => Err(FabricError::Config(
            "VOIE_FABRICD_STAGE_MODE must be lvm or dev-directory",
        )),
        None => Err(FabricError::Config(
            "VOIE_FABRICD_STAGE_MODE must be lvm or dev-directory",
        )),
    }
}

impl Fabric {
    pub fn open(config: crate::Config, live: Live) -> Result<Self, FabricError> {
        let store = Store::open(&config.sqlite)?;
        let sqlite_parent = config
            .sqlite
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let release_root = sqlite_parent.join("releases");
        let stage_root = match std::env::var("VOIE_FABRICD_STAGE_ROOT") {
            Ok(value) if !value.is_empty() => PathBuf::from(value),
            _ => sqlite_parent.join("stage"),
        };
        ensure_stage_volume_mounted(&stage_root)?;
        let fabric = Fabric {
            store,
            live,
            release_root,
            stage_root,
            lifecycles: Mutex::new(BTreeMap::new()),
            storage_alloc: AsyncMutex::new(()),
        };
        fabric.reconcile_staging()?;
        Ok(fabric)
    }

    pub fn live(&self) -> &Live {
        &self.live
    }

    pub fn release_root(&self) -> &std::path::Path {
        &self.release_root
    }

    pub fn stage_root(&self) -> &Path {
        &self.stage_root
    }

    pub fn postgres_root(&self) -> PathBuf {
        self.release_root
            .parent()
            .map(|parent| parent.join("postgres"))
            .unwrap_or_else(|| PathBuf::from("postgres"))
    }

    pub fn begin_product_operation(
        &self,
        kind: &str,
        resource_id: &str,
        operation_id: &str,
        request_hash: &str,
    ) -> Result<String, FabricError> {
        self.store
            .begin_product_operation(kind, resource_id, operation_id, request_hash)
    }

    pub fn complete_product_operation(
        &self,
        kind: &str,
        resource_id: &str,
        operation_id: &str,
        state: &str,
    ) -> Result<(), FabricError> {
        self.store
            .complete_product_operation(kind, resource_id, operation_id, state)
    }

    pub fn upsert_gateway_route(
        &self,
        slug: &str,
        kind: &str,
        service: &str,
        console_host: &str,
    ) -> Result<(), FabricError> {
        self.store
            .upsert_gateway_route(slug, kind, service, console_host)
    }

    pub fn delete_gateway_route(&self, slug: &str, kind: &str) -> Result<(), FabricError> {
        self.store.delete_gateway_route(slug, kind)
    }

    pub fn delete_gateway_routes_for_slug(&self, slug: &str) -> Result<(), FabricError> {
        self.store.delete_gateway_routes_for_slug(slug)
    }

    pub fn list_gateway_routes(&self) -> Result<Vec<crate::routes::RouteIntent>, FabricError> {
        self.store.list_gateway_routes()
    }

    pub fn rendered_caddyfile(&self) -> Result<String, FabricError> {
        let intents = self.store.list_gateway_routes()?;
        let host = self
            .store
            .gateway_console_host()?
            .unwrap_or_else(|| "console.invalid".to_owned());
        crate::routes::render_map(&intents, &host)
    }

    pub fn upsert_product_resource(
        &self,
        kind: &str,
        resource_id: &str,
        pod_name: Option<&str>,
        service_name: Option<&str>,
        artifact_hash: Option<&str>,
        state: &str,
    ) -> Result<(), FabricError> {
        self.store.upsert_product_resource(
            kind,
            resource_id,
            pod_name,
            service_name,
            artifact_hash,
            state,
        )
    }

    pub fn delete_product_resource(
        &self,
        kind: &str,
        resource_id: &str,
    ) -> Result<(), FabricError> {
        self.store.delete_product_resource(kind, resource_id)
    }

    pub fn get_product_resource(
        &self,
        kind: &str,
        resource_id: &str,
    ) -> Result<Option<(Option<String>, Option<String>, String)>, FabricError> {
        self.store.get_product_resource(kind, resource_id)
    }

    pub fn set_product_desired_yaml(
        &self,
        kind: &str,
        resource_id: &str,
        yaml: &str,
    ) -> Result<(), FabricError> {
        self.store
            .set_product_desired_yaml(kind, resource_id, yaml)
    }

    pub fn purge_product_resource(&self, kind: &str, resource_id: &str) -> Result<(), FabricError> {
        self.store.delete_product_operations(kind, resource_id)?;
        self.store.delete_product_resource(kind, resource_id)
    }

    pub async fn allocate_volume(
        &self,
        kind: crate::VolumeKind,
        resource_id: &str,
        bytes: u64,
        operation_id: Option<&str>,
    ) -> Result<BlockSlot, FabricError> {
        let _alloc = self.storage_alloc.lock().await;
        let policy = self.live.storage();
        match kind {
            crate::VolumeKind::Workspace => {
                crate::storage::admit_workspace(
                    self.store.workspace_allocated_bytes()?,
                    bytes,
                    policy.workspace_normal_budget_bytes,
                )?;
            }
            crate::VolumeKind::WorkspaceRestore => {
                crate::storage::admit_workspace_restore(
                    self.store.workspace_restore_allocated_bytes()?,
                    bytes,
                    policy.workspace_restore_headroom_bytes,
                )?;
            }
            crate::VolumeKind::Database | crate::VolumeKind::Deployment => {
                let vg = self.live.observe_vg().await?;
                crate::storage::admit_linear(
                    self.store.linear_allocated_bytes()?,
                    bytes,
                    policy.linear_normal_budget_bytes,
                    vg.physical_free_bytes,
                    policy.recovery_reserve_bytes,
                )?;
            }
            crate::VolumeKind::DatabaseRestore => {
                let vg = self.live.observe_vg().await?;
                crate::storage::admit_database_restore(
                    self.store.database_restore_allocated_bytes()?,
                    bytes,
                    policy.database_restore_budget_bytes,
                    vg.physical_free_bytes,
                    policy.emergency_floor_bytes,
                )?;
            }
        }
        let lv_name = match kind {
            crate::VolumeKind::Workspace => lv_name_for(resource_id),
            crate::VolumeKind::Database => crate::lv_name_for_postgres(resource_id),
            crate::VolumeKind::Deployment => crate::lv_name_for_deployment(resource_id),
            crate::VolumeKind::WorkspaceRestore | crate::VolumeKind::DatabaseRestore => {
                let operation = operation_id.ok_or(FabricError::Config(
                    "restore allocation requires an operation id",
                ))?;
                crate::lv_name_for_restore(operation)
            }
        };
        self.store
            .reserve_allocation(kind, resource_id, &lv_name, bytes, operation_id)?;
        let prepared = match kind {
            crate::VolumeKind::Workspace => self.live.prepare_block(resource_id, bytes).await,
            crate::VolumeKind::Database => {
                self.live.prepare_postgres_block(resource_id, bytes).await
            }
            crate::VolumeKind::Deployment => self.live.prepare_deployment_block(resource_id).await,
            crate::VolumeKind::WorkspaceRestore => {
                self.live.prepare_thin_named_block(lv_name, bytes).await
            }
            crate::VolumeKind::DatabaseRestore => {
                self.live.prepare_named_block(lv_name, bytes).await
            }
        };
        match prepared {
            Ok(slot) => Ok(slot),
            Err(error) => {
                let _ = self.store.delete_allocation(kind, resource_id);
                Err(error)
            }
        }
    }

    pub async fn free_volume(
        &self,
        kind: crate::VolumeKind,
        resource_id: &str,
    ) -> Result<(), FabricError> {
        let Some(row) = self.store.get_allocation(kind, resource_id)? else {
            return Ok(());
        };
        self.live
            .release_block(&BlockSlot {
                device: String::new(),
                lv_name: Some(row.lv_name),
                mapper_name: None,
            })
            .await?;
        self.store.delete_allocation(kind, resource_id)
    }

    pub fn get_allocation(
        &self,
        kind: crate::VolumeKind,
        resource_id: &str,
    ) -> Result<Option<crate::storage::VolumeAllocation>, FabricError> {
        self.store.get_allocation(kind, resource_id)
    }

    pub async fn promote_restore_to_database(&self, resource_id: &str) -> Result<(), FabricError> {
        self.promote_restore(resource_id, crate::VolumeKind::Database)
            .await
    }

    pub async fn promote_restore(
        &self,
        resource_id: &str,
        kind: crate::VolumeKind,
    ) -> Result<(), FabricError> {
        let _alloc = self.storage_alloc.lock().await;
        self.admit_restore_promotion(resource_id, kind)?;
        self.store.promote_restore(resource_id, kind)
    }

    fn admit_restore_promotion(
        &self,
        resource_id: &str,
        kind: crate::VolumeKind,
    ) -> Result<(), FabricError> {
        let Some(source) = kind.restore_source() else {
            return Err(FabricError::Config(
                "only workspace or database restore candidates can be promoted",
            ));
        };
        let candidate = self
            .store
            .get_allocation(source, resource_id)?
            .ok_or(FabricError::NotFound)?;
        let existing_live = self
            .store
            .get_allocation(kind, resource_id)?
            .map(|row| row.allocated_bytes)
            .unwrap_or(0);
        let policy = self.live.storage();
        match kind {
            crate::VolumeKind::Workspace => crate::storage::admit_permanent_promotion(
                self.store.workspace_allocated_bytes()?,
                existing_live,
                candidate.allocated_bytes,
                policy.workspace_normal_budget_bytes,
                "workspace",
            ),
            crate::VolumeKind::Database => crate::storage::admit_permanent_promotion(
                self.store.linear_allocated_bytes()?,
                existing_live,
                candidate.allocated_bytes,
                policy.linear_normal_budget_bytes,
                "linear",
            ),
            _ => Err(FabricError::Config(
                "only workspace or database restore candidates can be promoted",
            )),
        }
    }

    pub async fn capacity(&self) -> Result<crate::CapacityReport, FabricError> {
        let vg = self.live.observe_vg().await?;
        let policy = self.live.storage();
        let workspaces_bytes = self
            .store
            .allocated_bytes_by_kind(crate::VolumeKind::Workspace)?;
        let workspace_restore_bytes = self.store.workspace_restore_allocated_bytes()?;
        let databases_bytes = self
            .store
            .allocated_bytes_by_kind(crate::VolumeKind::Database)?;
        let deployments_bytes = self
            .store
            .allocated_bytes_by_kind(crate::VolumeKind::Deployment)?;
        let linear_allocated = databases_bytes.saturating_add(deployments_bytes);
        let health = crate::storage::capacity_health(
            vg.physical_free_bytes,
            policy.emergency_floor_bytes,
            vg.workspace_pool_bytes,
            vg.workspace_pool_used_bytes,
            policy.workspace_pool_slack_bytes(),
            vg.workspace_pool_metadata_percent.map(|value| value as f64),
            workspaces_bytes,
            policy.workspace_normal_budget_bytes,
            linear_allocated,
            policy.linear_normal_budget_bytes,
            vg.runtime_pool_used_bytes,
            vg.runtime_pool_bytes,
        );
        Ok(crate::CapacityReport {
            device_bytes: vg.device_bytes,
            health,
            runtime: crate::storage::RuntimeCapacity {
                pool_bytes: vg.runtime_pool_bytes,
                used_bytes: vg.runtime_pool_used_bytes,
            },
            workspaces: crate::storage::WorkspaceCapacity {
                pool_bytes: vg.workspace_pool_bytes,
                pool_used_bytes: vg.workspace_pool_used_bytes,
                logical_budget_bytes: policy.workspace_normal_budget_bytes,
                logical_allocated_bytes: workspaces_bytes,
                restore_headroom_bytes: policy.workspace_restore_headroom_bytes,
                restore_allocated_bytes: workspace_restore_bytes,
            },
            linear: crate::storage::LinearCapacity {
                budget_bytes: policy.linear_normal_budget_bytes,
                allocated_bytes: linear_allocated,
                allocatable_now_bytes: crate::storage::linear_allocatable_now(
                    policy.linear_normal_budget_bytes,
                    linear_allocated,
                    vg.physical_free_bytes,
                    policy.recovery_reserve_bytes,
                ),
                databases_bytes,
                deployments_bytes,
            },
            recovery: crate::storage::RecoveryCapacity {
                reserve_bytes: policy.recovery_reserve_bytes,
                emergency_floor_bytes: policy.emergency_floor_bytes,
                physical_free_bytes: vg.physical_free_bytes,
            },
        })
    }

    pub(crate) async fn lifecycle_guard(&self, workspace_id: &str) -> OwnedMutexGuard<()> {
        let key = {
            let mut keys = self
                .lifecycles
                .lock()
                .expect("lifecycle key table cannot be poisoned");
            keys.entry(workspace_id.to_owned()).or_default().clone()
        };
        key.lock_owned().await
    }

    /// Waits for the current Workspace lifecycle/exec holder, then sets
    /// Fabric realization state to `fenced`. New exec requires `ready`.
    pub async fn fence_workspace(&self, workspace_id: &str) -> Result<WorkspaceView, FabricError> {
        let _lifecycle = self.lifecycle_guard(workspace_id).await;
        let workspace = self
            .store
            .get_workspace(workspace_id)?
            .ok_or(FabricError::NotFound)?;
        match workspace.state.as_str() {
            "fenced" => return self.view(workspace_id),
            "ready" | "replacing" => {}
            other => {
                return Err(FabricError::Realize(format!(
                    "workspace {workspace_id} is {other}"
                )));
            }
        }
        self.store.set_workspace_state(workspace_id, "fenced")?;
        self.view(workspace_id)
    }

    pub async fn grow_workspace(
        &self,
        workspace_id: &str,
        target_bytes: u64,
    ) -> Result<WorkspaceView, FabricError> {
        let _lifecycle = self.lifecycle_guard(workspace_id).await;
        let _alloc = self.storage_alloc.lock().await;
        self.grow_workspace_locked(workspace_id, target_bytes).await
    }

    async fn maybe_grow_workspace_for_pressure(
        &self,
        workspace_id: &str,
    ) -> Result<(), FabricError> {
        let Some(current) = self.get_allocation(crate::VolumeKind::Workspace, workspace_id)? else {
            return Ok(());
        };
        let policy = self.live.storage();
        if current.allocated_bytes != policy.workspace_bytes {
            return Ok(());
        }
        let workspace = self
            .store
            .get_workspace(workspace_id)?
            .ok_or(FabricError::NotFound)?;
        if workspace.state != "ready" {
            return Ok(());
        }
        let generation = self
            .store
            .latest_generation(workspace_id)?
            .ok_or(FabricError::Realize("workspace has no execution".into()))?;
        let output = self
            .live
            .exec_guest(
                &generation.pod_name,
                "runner",
                &["/bin/df", "-P", "/workspace"],
                15_000,
            )
            .await?;
        if output.ambiguous || output.exit_code != 0 {
            return Ok(());
        }
        let Some(percent) = parse_df_use_percent(&output.stdout) else {
            return Ok(());
        };
        if percent < crate::storage::WORKSPACE_GROW_PRESSURE_PERCENT {
            return Ok(());
        }
        let target = policy.workspace_large_bytes;
        match crate::storage::admit_workspace(
            self.store
                .workspace_allocated_bytes()?
                .saturating_sub(current.allocated_bytes),
            target,
            policy.workspace_normal_budget_bytes,
        ) {
            Ok(()) => {}
            Err(_) => return Ok(()),
        }
        let _alloc = self.storage_alloc.lock().await;
        self.grow_workspace_locked(workspace_id, target).await?;
        Ok(())
    }

    async fn grow_workspace_locked(
        &self,
        workspace_id: &str,
        target_bytes: u64,
    ) -> Result<WorkspaceView, FabricError> {
        let workspace = self
            .store
            .get_workspace(workspace_id)?
            .ok_or(FabricError::NotFound)?;
        if workspace.state != "ready" {
            return Err(FabricError::Realize(format!(
                "workspace {workspace_id} is {}",
                workspace.state
            )));
        }
        let current = self
            .get_allocation(crate::VolumeKind::Workspace, workspace_id)?
            .ok_or(FabricError::NotFound)?;
        if target_bytes < current.allocated_bytes {
            return Err(FabricError::Conflict("workspaces never shrink".into()));
        }
        if current.allocated_bytes != target_bytes {
            let policy = self.live.storage();
            let expected =
                policy.next_extension(crate::VolumeKind::Workspace, current.allocated_bytes, false);
            if expected != Some(target_bytes) {
                return Err(FabricError::Conflict(
                    "workspace size is not the next platform storage tier".into(),
                ));
            }
            crate::storage::admit_workspace(
                self.store
                    .workspace_allocated_bytes()?
                    .saturating_sub(current.allocated_bytes),
                target_bytes,
                policy.workspace_normal_budget_bytes,
            )?;
            let lv_name = current.lv_name.clone();
            self.live.extend_thin_lv(&lv_name, target_bytes).await?;
            self.store.update_allocation_bytes(
                crate::VolumeKind::Workspace,
                workspace_id,
                target_bytes,
            )?;
        }
        let generation = self
            .store
            .latest_generation(workspace_id)?
            .ok_or(FabricError::Realize("workspace has no execution".into()))?;
        let output = self
            .live
            .exec_guest(
                &generation.pod_name,
                "runner",
                &["/sbin/resize2fs", "/dev/workspace"],
                120_000,
            )
            .await?;
        if output.ambiguous {
            return Err(FabricError::Unknown(
                "workspace filesystem resize did not settle".into(),
            ));
        }
        if output.exit_code != 0 {
            return Err(FabricError::Realize(format!(
                "resize2fs exited {}",
                output.exit_code
            )));
        }
        self.live
            .apply_yaml(&self.live.pv_yaml(
                workspace_id,
                &workspace.pv_name,
                &workspace.device,
                target_bytes,
            ))
            .await?;
        self.live
            .apply_yaml(&self.live.pvc_yaml(
                workspace_id,
                &workspace.pvc_name,
                &workspace.pv_name,
                target_bytes,
            ))
            .await?;
        self.view(workspace_id)
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

        for workspace in &workspaces {
            if workspace.state == "deleted" {
                continue;
            }
            let Some(lv_name) = workspace.lv_name.as_deref() else {
                continue;
            };
            let mapper_dev = encrypted_mapper_device(lv_name);
            if !Path::new(&mapper_dev).exists() {
                continue;
            }
            if workspace.device.starts_with("/dev/dm-") && workspace.device != mapper_dev {
                if let Err(error) = self
                    .store
                    .retarget_workspace_device(&workspace.id, &mapper_dev)
                {
                    eprintln!(
                        "voie-fabricd: workspace {} device stays {}: {error}",
                        workspace.id, workspace.device
                    );
                }
            }
            if let Ok(Some(reservation)) = self.store.get_reservation(&workspace.id) {
                if reservation.state == "reserved"
                    && reservation.device.starts_with("/dev/dm-")
                    && reservation.device != mapper_dev
                {
                    if let Err(error) = self
                        .store
                        .retarget_reservation_device(&workspace.id, &mapper_dev)
                    {
                        eprintln!(
                            "voie-fabricd: reservation {} device stays {}: {error}",
                            workspace.id, reservation.device
                        );
                    }
                }
            }
        }

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

        // Workspace allocations whose resource is gone still occupy the
        // logical budget and protect the LV from the unclaimed walk.
        // A live workspace row or a held reservation can still own the
        // claim; a deleted or missing workspace cannot. Restore and
        // linear allocations are not released here.
        let live_workspace_ids: HashSet<&str> = workspaces
            .iter()
            .filter(|row| row.state != "deleted")
            .map(|row| row.id.as_str())
            .collect();
        let reserved_ids: HashSet<&str> = reserved
            .iter()
            .map(|row| row.workspace_id.as_str())
            .collect();
        for allocation in self.store.list_allocations()? {
            if allocation.kind != crate::VolumeKind::Workspace {
                continue;
            }
            if live_workspace_ids.contains(allocation.resource_id.as_str())
                || reserved_ids.contains(allocation.resource_id.as_str())
            {
                continue;
            }
            match self
                .store
                .delete_allocation(allocation.kind, &allocation.resource_id)
            {
                Ok(()) => {
                    eprintln!(
                        "voie-fabricd: released abandoned workspace allocation {} (lv {})",
                        allocation.resource_id, allocation.lv_name
                    );
                    report
                        .orphan_allocations_released
                        .push(allocation.resource_id);
                }
                Err(error) => eprintln!(
                    "voie-fabricd: abandoned workspace allocation {} stays: {error}",
                    allocation.resource_id
                ),
            }
        }

        // A claimed slot is protected by a live workspace row, a held
        // reservation, or a remaining allocation. Deleted workspace names
        // do not keep leftover LVs. Everything else carrying a daemon-minted
        // name is an unclaimed leftover of a crashed prepare.
        let mut protected: HashSet<String> = workspaces
            .iter()
            .filter(|row| row.state != "deleted")
            .filter_map(|row| row.lv_name.clone())
            .collect();
        for reservation in &reserved {
            protected.insert(lv_name_for(&reservation.workspace_id));
        }
        for name in self.store.claimed_lv_names()? {
            protected.insert(name);
        }
        match self.live.list_lv_names().await {
            Ok(names) => {
                crate::storage::refuse_legacy_product_pool(&names)?;
                crate::storage::refuse_allocated_recovery_reserve(&names)?;
                crate::storage::require_runtime_pool(&names)?;
                crate::storage::require_workspace_pool(
                    &names,
                    &self.live.storage().workspace_pool,
                )?;
                if self.live.storage().runtime_pool_bytes > 0
                    || self.live.storage().workspace_pool_data_bytes > 0
                {
                    let vg = self.live.observe_vg().await?;
                    crate::storage::require_runtime_pool_size(
                        vg.runtime_pool_bytes,
                        self.live.storage().runtime_pool_bytes,
                    )?;
                    crate::storage::require_workspace_pool_size(
                        vg.workspace_pool_bytes,
                        self.live.storage().workspace_pool_data_bytes,
                    )?;
                }
                self.live.refuse_allocating_storage_classes().await?;
                self.live.refuse_retired_workspace_pool_pv().await?;
                let present: HashSet<String> = names.iter().cloned().collect();
                for name in self.store.claimed_lv_names()? {
                    if present.contains(&name) {
                        self.live.activate_lv(&name).await?;
                    }
                }
                for name in names {
                    if !is_daemon_lv_name(&name) || protected.contains(&name) {
                        continue;
                    }
                    let slot = BlockSlot {
                        device: String::new(),
                        lv_name: Some(name.clone()),
                        mapper_name: None,
                    };
                    match self.live.release_block(&slot).await {
                        Ok(()) => report.orphan_lvs_removed.push(name),
                        Err(error) => {
                            eprintln!("voie-fabricd: unclaimed LV {name} stays: {error}");
                            report.orphan_lv_failures.push(name);
                        }
                    }
                }
                for workspace in &workspaces {
                    if workspace.state != "ready" {
                        continue;
                    }
                    let expected = workspace
                        .lv_name
                        .clone()
                        .unwrap_or_else(|| lv_name_for(&workspace.id));
                    if !present.contains(&expected) {
                        eprintln!(
                            "voie-fabricd: workspace {} is ready in sqlite but LV {expected} is gone; not minting leftover capacity",
                            workspace.id
                        );
                        report.ready_without_volume.push(workspace.id.clone());
                        // Stale mapper reservations must not occupy recycled
                        // /dev/dm-N numbers. Releasing them does not remint
                        // the leftover workspace.
                        match self
                            .store
                            .release_reservation(&workspace.id, "leftover-lv-gone")
                        {
                            Ok(()) => {}
                            Err(error) => eprintln!(
                                "voie-fabricd: leftover workspace {} reservation stays: {error}",
                                workspace.id
                            ),
                        }
                    }
                }
                for allocation in self.store.list_allocations()? {
                    let lv = allocation.lv_name.trim();
                    if lv.is_empty() || present.contains(lv) {
                        continue;
                    }
                    match self
                        .store
                        .delete_allocation(allocation.kind, &allocation.resource_id)
                    {
                        Ok(()) => {
                            eprintln!(
                                "voie-fabricd: released {} allocation {} for absent LV {lv}",
                                allocation.kind.as_str(),
                                allocation.resource_id
                            );
                            report.orphan_allocations_released.push(allocation.resource_id);
                        }
                        Err(error) => eprintln!(
                            "voie-fabricd: allocation {} for absent LV {lv} stays: {error}",
                            allocation.resource_id
                        ),
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
        self.restore_encrypted_block_runtime(&mut report).await;
        Ok(report)
    }

    /// After reboot, `/dev/dm-N` names a different device. Reopen each
    /// claimed crypt mapping onto its stable mapper first, then replace
    /// PersistentVolumes and re-apply guest pods. Pods are applied only
    /// after the previous object is gone; `restartPolicy: Never` will not
    /// restart a terminating name.
    async fn restore_encrypted_block_runtime(&self, report: &mut StartupReport) {
        let allocations = match self.store.list_allocations() {
            Ok(rows) => rows,
            Err(error) => {
                eprintln!("voie-fabricd: cannot list allocations for crypt reopen: {error}");
                return;
            }
        };
        let mut opened: Vec<(crate::storage::VolumeAllocation, String)> = Vec::new();
        for allocation in allocations {
            if allocation.state != "allocated" {
                continue;
            }
            match self.live.reopen_encrypted_lv(&allocation.lv_name).await {
                Ok(slot) => {
                    report
                        .encrypted_volumes_reopened
                        .push(allocation.lv_name.clone());
                    let device = slot.device.clone();
                    if allocation.kind == crate::VolumeKind::Workspace {
                        if let Err(error) = self.store.retarget_workspace_block(
                            &allocation.resource_id,
                            &device,
                            Some(&allocation.lv_name),
                        ) {
                            eprintln!(
                                "voie-fabricd: workspace {} device stays: {error}",
                                allocation.resource_id
                            );
                        }
                        if let Err(error) = self
                            .store
                            .retarget_reservation_device(&allocation.resource_id, &device)
                        {
                            eprintln!(
                                "voie-fabricd: reservation {} device stays: {error}",
                                allocation.resource_id
                            );
                        }
                    }
                    opened.push((allocation, device));
                }
                Err(error) => {
                    eprintln!(
                        "voie-fabricd: cannot reopen encrypted LV {}: {error}",
                        allocation.lv_name
                    );
                    report
                        .encrypted_reopen_failures
                        .push(allocation.lv_name.clone());
                }
            }
        }
        for (allocation, device) in &opened {
            if let Err(error) = self
                .retarget_claimed_volume_pv(allocation, device, report)
                .await
            {
                eprintln!(
                    "voie-fabricd: PV retarget for {} failed: {error}",
                    allocation.lv_name
                );
            }
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        for pod_name in &report.pods_rebound {
            let remain = deadline.saturating_duration_since(std::time::Instant::now());
            if remain.is_zero() {
                eprintln!("voie-fabricd: pod {pod_name} not Ready before listen deadline");
                continue;
            }
            if let Err(error) = self.live.wait_pod_ready(pod_name, remain).await {
                eprintln!("voie-fabricd: {error}");
            }
        }
    }

    async fn retarget_claimed_volume_pv(
        &self,
        allocation: &crate::storage::VolumeAllocation,
        device: &str,
        report: &mut StartupReport,
    ) -> Result<(), FabricError> {
        require_stable_block_path(device)?;
        let (pv_name, pvc_name, pod_name) = self.runtime_object_names(allocation)?;
        let existing_pod = self.live.get_namespaced("pod", &pod_name).await?;
        let pod_ready = matches!(
            self.live.get_pod(&pod_name).await,
            Ok(Some(pod)) if pod.ready
        );
        if existing_pod.is_some() && !pod_ready {
            if let Err(error) = self
                .live
                .delete_named_wait("pod", &pod_name, true, 30, false)
                .await
            {
                eprintln!(
                    "voie-fabricd: cannot release pod {pod_name} before PV retarget: {error}"
                );
            }
            if let Err(error) = self
                .live
                .wait_named_gone("pod", &pod_name, true, std::time::Duration::from_secs(15))
                .await
            {
                return Err(error);
            }
        }
        let pv_yaml = self
            .live
            .pv_yaml(&allocation.resource_id, &pv_name, device, allocation.allocated_bytes);
        let pvc_yaml = self.live.pvc_yaml(
            &allocation.resource_id,
            &pvc_name,
            &pv_name,
            allocation.allocated_bytes,
        );
        let replaced = self
            .live
            .ensure_local_pv_device(&pv_name, &pvc_name, device, &pv_yaml, &pvc_yaml)
            .await?;
        if replaced {
            report.stale_pvs_replaced.push(pv_name);
        }
        if pod_ready && !replaced {
            return Ok(());
        }
        if existing_pod.is_some() && pod_ready && replaced {
            let _ = self
                .live
                .delete_named_wait("pod", &pod_name, true, 30, false)
                .await;
            self.live
                .wait_named_gone("pod", &pod_name, true, std::time::Duration::from_secs(15))
                .await?;
        }
        if allocation.kind == crate::VolumeKind::Workspace {
            if let Some(workspace) = self.store.get_workspace(&allocation.resource_id)? {
                if workspace.state == "ready" {
                    let generation = self
                        .store
                        .latest_generation(&allocation.resource_id)?
                        .map(|row| row.generation)
                        .unwrap_or(1);
                    let yaml =
                        self.live
                            .pod_yaml(&allocation.resource_id, &pod_name, &pvc_name, generation);
                    self.live.apply_yaml(&yaml).await?;
                    report.pods_rebound.push(pod_name);
                    return Ok(());
                }
            }
        }
        if let Some(yaml) = self.product_runtime_pod_yaml(allocation).await? {
            self.live.apply_yaml(&yaml).await?;
            report.pods_rebound.push(pod_name);
            return Ok(());
        }
        if let Some(json) = existing_pod {
            self.live.apply_json(json).await?;
            report.pods_rebound.push(pod_name);
        }
        Ok(())
    }

    /// Last applied product Pod YAML, or a typed Database render when the
    /// claimed LV still exists and the cluster object does not. Deployment
    /// Pods require stored YAML: run argv is not recoverable from the LV
    /// name.
    async fn product_runtime_pod_yaml(
        &self,
        allocation: &crate::storage::VolumeAllocation,
    ) -> Result<Option<String>, FabricError> {
        let product_kind = match allocation.kind {
            crate::VolumeKind::Database | crate::VolumeKind::DatabaseRestore => "database",
            crate::VolumeKind::Deployment => "deployment",
            crate::VolumeKind::Workspace | crate::VolumeKind::WorkspaceRestore => return Ok(None),
        };
        if let Some(yaml) = self
            .store
            .product_desired_yaml(product_kind, &allocation.resource_id)?
        {
            return Ok(Some(yaml));
        }
        match allocation.kind {
            crate::VolumeKind::Database | crate::VolumeKind::DatabaseRestore => {
                let (slug, kind) = self.database_slug_kind(&allocation.resource_id).await;
                Ok(Some(postgres_runtime_pod_yaml(
                    self.live(),
                    &allocation.resource_id,
                    &allocation.lv_name,
                    allocation.operation_id.as_deref(),
                    &slug,
                    &kind,
                )))
            }
            _ => Ok(None),
        }
    }

    async fn database_slug_kind(&self, database_id: &str) -> (String, String) {
        let name = postgres_network_policy_name(database_id);
        match self.live.get_namespaced("networkpolicy", &name).await {
            Ok(Some(value)) => slug_kind_from_object(&value),
            _ => (String::new(), "dev".into()),
        }
    }

    fn runtime_object_names(
        &self,
        allocation: &crate::storage::VolumeAllocation,
    ) -> Result<(String, String, String), FabricError> {
        match allocation.kind {
            crate::VolumeKind::Workspace => {
                if let Some(workspace) = self.store.get_workspace(&allocation.resource_id)? {
                    let generation = self
                        .store
                        .latest_generation(&allocation.resource_id)?
                        .map(|row| row.generation)
                        .unwrap_or(1);
                    let (_, _, pod) = object_names(&allocation.resource_id, generation);
                    Ok((workspace.pv_name, workspace.pvc_name, pod))
                } else {
                    Ok(object_names(&allocation.resource_id, 1))
                }
            }
            crate::VolumeKind::WorkspaceRestore => {
                let generation = self
                    .store
                    .latest_generation(&allocation.resource_id)?
                    .map(|row| row.generation)
                    .unwrap_or(1);
                Ok(restore_object_names(&allocation.resource_id, generation))
            }
            crate::VolumeKind::Database | crate::VolumeKind::DatabaseRestore => {
                let pv = postgres_pvc_for_lv(&allocation.lv_name, &allocation.resource_id);
                let pod = postgres_pod_for_lv(&allocation.lv_name, &allocation.resource_id);
                Ok((pv.clone(), pv, pod))
            }
            crate::VolumeKind::Deployment => {
                let pv = deployment_volume_name(&allocation.resource_id);
                let pod = self
                    .store
                    .get_product_resource("deployment", &allocation.resource_id)?
                    .and_then(|(pod, _, _)| pod)
                    .unwrap_or_else(|| app_pod_name(&allocation.resource_id));
                Ok((pv.clone(), pv, pod))
            }
        }
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
            mapper_name: None,
        };
        self.live.release_block(&slot).await?;
        self.store
            .release_reservation(workspace_id, "startup-unrealized")?;
        Ok(true)
    }

    pub async fn create_workspace(
        &self,
        id: &str,
        allocated_bytes: Option<u64>,
        elevated: Option<bool>,
    ) -> Result<WorkspaceView, FabricError> {
        let _lifecycle = self.lifecycle_guard(id).await;
        if let Some(existing) = self.store.get_workspace(id)? {
            match existing.state.as_str() {
                "ready" => {
                    let expected = existing.lv_name.clone().unwrap_or_else(|| lv_name_for(id));
                    let names = self.live.list_lv_names().await?;
                    if !names.iter().any(|name| name == &expected) {
                        return Err(FabricError::Realize(format!(
                            "workspace {id} is recorded ready but LV {expected} is gone; refuse leftover capacity"
                        )));
                    }
                    return self.view_from_row(&existing);
                }
                "deleting" => {
                    return Err(FabricError::Realize(format!("workspace {id} is deleting")));
                }
                "deleted" => {
                    return Err(FabricError::Conflict(format!(
                        "workspace {id} is retired and cannot be reused"
                    )));
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

        let bytes = match allocated_bytes {
            Some(bytes) => bytes,
            None => self
                .live
                .storage()
                .workspace_size(elevated.unwrap_or(false)),
        };
        if !self
            .live
            .storage()
            .matches_tier(crate::VolumeKind::Workspace, bytes, false)
        {
            return Err(FabricError::Conflict(
                "workspace size is not a platform storage tier".into(),
            ));
        }
        let slot = self
            .allocate_volume(crate::VolumeKind::Workspace, id, bytes, None)
            .await?;
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

        require_stable_block_path(&slot.device)?;
        self.live
            .apply_yaml(&self.live.pv_yaml(id, &pv_name, &slot.device, bytes))
            .await?;
        let Some(pv) = self.live.get_pv(&pv_name).await? else {
            return Err(FabricError::Unknown(format!(
                "PV {pv_name} missing after apply"
            )));
        };
        self.live.verify_pv(&pv, id, &slot.device)?;
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
            .apply_yaml(&self.live.pvc_yaml(id, &pvc_name, &pv_name, bytes))
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

    /// GET view plus the running Pod image when kubectl can observe it.
    pub async fn observe_workspace(&self, id: &str) -> Result<WorkspaceView, FabricError> {
        let mut view = self.view(id)?;
        if !view.pod_name.is_empty() {
            if let Ok(Some(pod)) = self.live.get_pod(&view.pod_name).await {
                view.image = pod.image;
            }
        }
        Ok(view)
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
        if !workspace_allows_exec(&workspace.state) {
            return Err(FabricError::Realize(format!(
                "workspace {workspace_id} is {}",
                workspace.state
            )));
        }
        if let Err(error) = self.maybe_grow_workspace_for_pressure(workspace_id).await {
            if matches!(error, FabricError::Conflict(_)) {
                // Budget refused the automatic 16→32 step; continue on the
                // current virtual size rather than failing the exec.
            } else {
                return Err(error);
            }
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

    /// Packages one Workspace generation inside the guest with `voie-pack`.
    /// The host copies the staged artifact; it never packs Application source
    /// itself. Bytes stay on a host file; the HTTP handler streams them. Same
    /// operation hash is not packed again.
    pub async fn pack_workspace(
        &self,
        workspace_id: &str,
        operation_id: &str,
        request_hash: &str,
        relative_root: &str,
    ) -> Result<(PathBuf, String, u64), FabricError> {
        validate_pack_root(relative_root)?;
        let state = self.store.begin_product_operation(
            "workspace-pack",
            workspace_id,
            operation_id,
            request_hash,
        )?;
        let staged = self
            .release_root
            .join("pack")
            .join(workspace_id)
            .join(format!("{operation_id}.tar.zst"));
        if state != "dispatched" {
            if state == "unknown" {
                return Err(FabricError::Unknown(
                    "workspace pack outcome unknown; the intent will not be dispatched again"
                        .into(),
                ));
            }
            let (hash, length) =
                crate::product::hash_staged_file(&staged).map_err(|_| FabricError::NotFound)?;
            return Ok((staged, hash, length));
        }
        let _lifecycle = self.lifecycle_guard(workspace_id).await;
        let workspace = self
            .store
            .get_workspace(workspace_id)?
            .ok_or(FabricError::NotFound)?;
        if workspace.state != "ready" {
            let _ = self.store.complete_product_operation(
                "workspace-pack",
                workspace_id,
                operation_id,
                "unknown",
            )?;
            return Err(FabricError::Realize(format!(
                "workspace {workspace_id} is {}",
                workspace.state
            )));
        }
        let generation = self
            .store
            .latest_generation(workspace_id)?
            .ok_or(FabricError::Realize("workspace has no execution".into()))?;
        const PACK_TIMEOUT_MS: u64 = 300_000;
        let result = self
            .live
            .exec_guest(
                &generation.pod_name,
                "runner",
                &["/bin/voie-pack", "/workspace", relative_root],
                PACK_TIMEOUT_MS,
            )
            .await;
        let output = match result {
            Ok(output) if !output.ambiguous && output.exit_code == 0 => output,
            Ok(output) if !output.ambiguous => {
                let _ = self.store.complete_product_operation(
                    "workspace-pack",
                    workspace_id,
                    operation_id,
                    "failed",
                )?;
                return Err(FabricError::Realize(format!(
                    "voie-pack exited {}",
                    output.exit_code
                )));
            }
            _ => {
                let _ = self.store.complete_product_operation(
                    "workspace-pack",
                    workspace_id,
                    operation_id,
                    "unknown",
                )?;
                return Err(FabricError::Unknown("workspace pack did not settle".into()));
            }
        };
        let remote = if relative_root == "." {
            "/workspace/.voie/tmp/release.tar.zst".to_owned()
        } else {
            format!("/workspace/{relative_root}/.voie/tmp/release.tar.zst")
        };
        if let Some(parent) = staged.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                FabricError::Realize(format!("cannot stage packed release: {error}"))
            })?;
        }
        if let Err(error) = self
            .live
            .copy_from_pod(&generation.pod_name, "runner", &remote, &staged)
            .await
        {
            let _ = self.store.complete_product_operation(
                "workspace-pack",
                workspace_id,
                operation_id,
                "unknown",
            )?;
            return Err(error);
        }
        let (hash, length) = match crate::product::hash_staged_file(&staged) {
            Ok(value) if value.1 > 0 => value,
            _ => {
                let _ = self.store.complete_product_operation(
                    "workspace-pack",
                    workspace_id,
                    operation_id,
                    "unknown",
                )?;
                return Err(FabricError::Unknown(
                    "packed artifact vanished after copy".into(),
                ));
            }
        };
        if let Some(reported) = pack_hash_from_stdout(&output.stdout) {
            if reported != hash {
                let _ = self.store.complete_product_operation(
                    "workspace-pack",
                    workspace_id,
                    operation_id,
                    "unknown",
                )?;
                return Err(FabricError::Realize(
                    "packed artifact hash did not match voie-pack output".into(),
                ));
            }
        }
        self.store.complete_product_operation(
            "workspace-pack",
            workspace_id,
            operation_id,
            "terminal",
        )?;
        Ok((staged, hash, length))
    }

    /// Snapshots one Workspace including `.git`. Bytes stay on a host file;
    /// the HTTP handler streams them. Same operation hash is not packed again.
    pub async fn snapshot_workspace(
        &self,
        workspace_id: &str,
        operation_id: &str,
        request_hash: &str,
    ) -> Result<(std::path::PathBuf, String, u64), FabricError> {
        let state = self.store.begin_product_operation(
            "workspace-snapshot",
            workspace_id,
            operation_id,
            request_hash,
        )?;
        let staged = self
            .stage_root
            .join("snapshots")
            .join(workspace_id)
            .join(format!("{operation_id}.tar.zst"));
        if state != "dispatched" {
            if state == "unknown" {
                return Err(FabricError::Unknown(
                    "workspace snapshot outcome unknown; the intent will not be dispatched again"
                        .into(),
                ));
            }
            if state == "acked" {
                return Err(FabricError::Conflict(
                    "workspace snapshot already acked".into(),
                ));
            }
            let (hash, length) = crate::product::hash_staged_file(&staged)?;
            return Ok((staged, hash, length));
        }
        let _lifecycle = self.lifecycle_guard(workspace_id).await;
        let workspace = self
            .store
            .get_workspace(workspace_id)?
            .ok_or(FabricError::NotFound)?;
        if !workspace_allows_snapshot(&workspace.state) {
            self.abandon_staging_operation(
                "workspace-snapshot",
                workspace_id,
                operation_id,
                "unknown",
            )?;
            return Err(FabricError::Realize(format!(
                "workspace {workspace_id} is {}",
                workspace.state
            )));
        }
        let generation = match self.store.latest_generation(workspace_id)? {
            Some(generation) => generation,
            None => {
                self.abandon_staging_operation(
                    "workspace-snapshot",
                    workspace_id,
                    operation_id,
                    "unknown",
                )?;
                return Err(FabricError::Realize("workspace has no execution".into()));
            }
        };
        const SNAPSHOT_TIMEOUT_MS: u64 = crate::storage::PRODUCT_VOLUME_IO_TIMEOUT_MS;
        let result = self
            .live
            .exec_guest(
                &generation.pod_name,
                "runner",
                &["/bin/voie-pack", "workspace-snapshot", "/workspace"],
                SNAPSHOT_TIMEOUT_MS,
            )
            .await;
        let output = match result {
            Ok(output) if !output.ambiguous && output.exit_code == 0 => output,
            Ok(output) if !output.ambiguous => {
                self.abandon_staging_operation(
                    "workspace-snapshot",
                    workspace_id,
                    operation_id,
                    "failed",
                )?;
                return Err(FabricError::Realize(format!(
                    "voie-pack snapshot exited {}",
                    output.exit_code
                )));
            }
            _ => {
                self.abandon_staging_operation(
                    "workspace-snapshot",
                    workspace_id,
                    operation_id,
                    "unknown",
                )?;
                return Err(FabricError::Unknown(
                    "workspace snapshot did not settle".into(),
                ));
            }
        };
        if let Some(parent) = staged.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                self.abandon_staging_operation(
                    "workspace-snapshot",
                    workspace_id,
                    operation_id,
                    "unknown",
                )?;
                return Err(FabricError::Realize(format!(
                    "cannot stage workspace snapshot: {error}"
                )));
            }
        }
        if let Err(error) = self
            .live
            .copy_from_pod(
                &generation.pod_name,
                "runner",
                "/workspace/.voie/tmp/workspace-snapshot.tar.zst",
                &staged,
            )
            .await
        {
            self.abandon_staging_operation(
                "workspace-snapshot",
                workspace_id,
                operation_id,
                "unknown",
            )?;
            return Err(error);
        }
        let (hash, length) = match crate::product::hash_staged_file(&staged) {
            Ok(value) if value.1 > 0 => value,
            _ => {
                self.abandon_staging_operation(
                    "workspace-snapshot",
                    workspace_id,
                    operation_id,
                    "unknown",
                )?;
                return Err(FabricError::Unknown(
                    "workspace snapshot vanished after copy".into(),
                ));
            }
        };
        if let Some(reported) = pack_hash_from_stdout(&output.stdout) {
            if reported != hash {
                self.abandon_staging_operation(
                    "workspace-snapshot",
                    workspace_id,
                    operation_id,
                    "unknown",
                )?;
                return Err(FabricError::Realize(
                    "workspace snapshot hash did not match voie-pack output".into(),
                ));
            }
        }
        let _ = self
            .live
            .exec_guest(
                &generation.pod_name,
                "runner",
                &["/sbin/fstrim", "-v", "/workspace"],
                60_000,
            )
            .await;
        self.store.complete_product_operation(
            "workspace-snapshot",
            workspace_id,
            operation_id,
            "terminal",
        )?;
        Ok((staged, hash, length))
    }

    pub fn ack_workspace_snapshot(
        &self,
        workspace_id: &str,
        operation_id: &str,
    ) -> Result<(), FabricError> {
        let path = self
            .stage_root
            .join("snapshots")
            .join(workspace_id)
            .join(format!("{operation_id}.tar.zst"));
        self.ack_staging_file("workspace-snapshot", workspace_id, operation_id, &path)
    }

    pub fn ack_workspace_pack(&self, workspace_id: &str, operation_id: &str) {
        let path = self
            .release_root
            .join("pack")
            .join(workspace_id)
            .join(format!("{operation_id}.tar.zst"));
        let _ = std::fs::remove_file(path);
    }

    pub fn ack_database_backup(
        &self,
        database_id: &str,
        operation_id: &str,
    ) -> Result<(), FabricError> {
        let path = self
            .stage_root
            .join("backups")
            .join(database_id)
            .join(format!("{operation_id}.pgdump"));
        self.ack_staging_file("database-backup", database_id, operation_id, &path)
    }

    pub fn begin_restore_artifact(
        &self,
        kind: &str,
        resource_id: &str,
    ) -> Result<String, FabricError> {
        self.store
            .begin_product_operation(kind, resource_id, "artifact", "put")
    }

    pub fn finish_restore_artifact(
        &self,
        kind: &str,
        resource_id: &str,
    ) -> Result<(), FabricError> {
        self.store
            .complete_product_operation(kind, resource_id, "artifact", "terminal")
    }

    /// Settle a staging write that will not produce an artifact: the partial
    /// file is removed first, then the journal releases the slot. A leftover
    /// file keeps the slot occupied.
    pub fn abandon_staging_operation(
        &self,
        kind: &str,
        resource_id: &str,
        operation_id: &str,
        state: &str,
    ) -> Result<(), FabricError> {
        if let Some(path) = self.staging_file_path(kind, resource_id, operation_id) {
            unlink_staged_path(&path)?;
        }
        self.store
            .complete_product_operation(kind, resource_id, operation_id, state)?;
        self.trim_stage();
        Ok(())
    }

    pub fn ack_restore_artifact(
        &self,
        kind: &str,
        resource_id: &str,
        path: &Path,
    ) -> Result<(), FabricError> {
        self.ack_staging_file(kind, resource_id, "artifact", path)
    }

    fn ack_staging_file(
        &self,
        kind: &str,
        resource_id: &str,
        operation_id: &str,
        path: &Path,
    ) -> Result<(), FabricError> {
        unlink_staged_path(path)?;
        self.store
            .ack_staging_operation(kind, resource_id, operation_id)?;
        self.trim_stage();
        Ok(())
    }

    fn trim_stage(&self) {
        match Command::new("fstrim").arg(&self.stage_root).output() {
            Ok(out) if out.status.success() => {}
            Ok(out) => eprintln!(
                "voie-fabricd: fstrim staging did not reclaim physical capacity: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(error) => {
                eprintln!("voie-fabricd: fstrim staging did not reclaim physical capacity: {error}")
            }
        }
    }

    fn workspace_restore_stage_path(&self, workspace_id: &str) -> PathBuf {
        self.stage_root
            .join("snapshots")
            .join(workspace_id)
            .join("restore.tar.zst")
    }

    fn reconcile_staging(&self) -> Result<(), FabricError> {
        let incomplete = self.store.list_dispatched_staging()?;
        for (kind, resource, operation) in incomplete {
            let released = match self.staging_file_path(&kind, &resource, &operation) {
                Some(path) => unlink_staged_path(&path).is_ok(),
                None => true,
            };
            if released {
                self.store
                    .complete_product_operation(&kind, &resource, &operation, "unknown")?;
            }
        }
        for (kind, resource, operation) in self.store.list_terminal_staging()? {
            let missing = match self.staging_file_path(&kind, &resource, &operation) {
                Some(path) => !path.exists(),
                None => true,
            };
            if missing {
                let _ = self
                    .store
                    .ack_staging_operation(&kind, &resource, &operation);
            }
        }
        self.sweep_staging_dir("snapshots", "workspace-snapshot", "tar.zst")?;
        self.sweep_staging_dir("backups", "database-backup", "pgdump")?;
        Ok(())
    }

    fn staging_file_path(&self, kind: &str, resource: &str, operation: &str) -> Option<PathBuf> {
        Some(match kind {
            "workspace-snapshot" => self
                .stage_root
                .join("snapshots")
                .join(resource)
                .join(format!("{operation}.tar.zst")),
            "database-backup" => self
                .stage_root
                .join("backups")
                .join(resource)
                .join(format!("{operation}.pgdump")),
            "workspace-restore-artifact" => self.workspace_restore_stage_path(resource),
            "database-restore-artifact" => self
                .stage_root
                .join("backups")
                .join(resource)
                .join("restore.pgdump"),
            _ => return None,
        })
    }

    fn sweep_staging_dir(
        &self,
        subdir: &str,
        kind: &str,
        extension: &str,
    ) -> Result<(), FabricError> {
        let root = self.stage_root.join(subdir);
        let Ok(resources) = std::fs::read_dir(&root) else {
            return Ok(());
        };
        for resource in resources.flatten() {
            let resource_id = resource.file_name();
            let resource_id = resource_id.to_string_lossy();
            let Ok(files) = std::fs::read_dir(resource.path()) else {
                continue;
            };
            for file in files.flatten() {
                let name = file.file_name();
                let name = name.to_string_lossy();
                if name == format!("restore.{extension}") {
                    continue;
                }
                let Some(operation) = name.strip_suffix(&format!(".{extension}")) else {
                    let _ = std::fs::remove_file(file.path());
                    continue;
                };
                match self
                    .store
                    .product_operation_state(kind, &resource_id, operation)?
                    .as_deref()
                {
                    Some("terminal") => {}
                    _ => {
                        let _ = std::fs::remove_file(file.path());
                    }
                }
            }
        }
        Ok(())
    }

    fn workspace_restore_mount(&self, workspace_id: &str) -> PathBuf {
        self.release_root
            .join("snapshots")
            .join(workspace_id)
            .join("restore-mnt")
    }

    /// Restores a Workspace snapshot onto a candidate LV and switches only
    /// after the candidate Pod is Ready and a Workspace probe succeeds.
    /// The previous generation stays until that proof; a failed boot leaves
    /// the live volume untouched.
    pub async fn restore_workspace(
        &self,
        workspace_id: &str,
        operation_id: &str,
        request_hash: &str,
        artifact_hash: Option<&str>,
        allocated_bytes: Option<u64>,
        elevated: Option<bool>,
    ) -> Result<WorkspaceView, FabricError> {
        let _lifecycle = self.lifecycle_guard(workspace_id).await;
        let state = self.store.begin_product_operation(
            "workspace-restore",
            workspace_id,
            operation_id,
            request_hash,
        )?;
        if state != "dispatched" {
            if state == "unknown" {
                return Err(FabricError::Unknown(
                    "workspace restore outcome unknown; the intent will not be dispatched again"
                        .into(),
                ));
            }
            return self.view(workspace_id);
        }
        let path = self.workspace_restore_stage_path(workspace_id);
        if !path.exists() {
            let _ = self.store.complete_product_operation(
                "workspace-restore",
                workspace_id,
                operation_id,
                "failed",
            )?;
            return Err(FabricError::Realize(
                "restore artifact has not been staged".into(),
            ));
        }
        if let Some(expected) = artifact_hash {
            if let Err(error) = crate::product::verify_file_hash(&path, expected) {
                let _ = self.store.complete_product_operation(
                    "workspace-restore",
                    workspace_id,
                    operation_id,
                    "failed",
                )?;
                return Err(error);
            }
        }
        let existing = self.store.get_workspace(workspace_id)?;
        if existing.as_ref().is_some_and(|row| row.state == "deleting") {
            let _ = self.store.complete_product_operation(
                "workspace-restore",
                workspace_id,
                operation_id,
                "failed",
            )?;
            return Err(FabricError::Realize(format!(
                "workspace {workspace_id} is deleting"
            )));
        }
        let next_generation = self
            .store
            .latest_generation(workspace_id)?
            .map(|row| row.generation + 1)
            .unwrap_or(1);
        let restore_alloc =
            self.get_allocation(crate::VolumeKind::WorkspaceRestore, workspace_id)?;
        let live_uses_restore = existing
            .as_ref()
            .and_then(|row| row.lv_name.as_ref())
            .zip(restore_alloc.as_ref())
            .is_some_and(|(lv, restore)| lv == &restore.lv_name);
        if live_uses_restore {
            if let Err(error) = self
                .promote_restore(workspace_id, crate::VolumeKind::Workspace)
                .await
            {
                let _ = self.store.complete_product_operation(
                    "workspace-restore",
                    workspace_id,
                    operation_id,
                    "unknown",
                )?;
                return Err(error);
            }
            let _ = std::fs::remove_file(&path);
            self.store.complete_product_operation(
                "workspace-restore",
                workspace_id,
                operation_id,
                "terminal",
            )?;
            return self.view(workspace_id);
        }
        self.teardown_restore_candidate_objects(workspace_id, next_generation)
            .await;
        let bytes = match allocated_bytes {
            Some(bytes) => bytes,
            None => {
                if let Some(row) =
                    self.get_allocation(crate::VolumeKind::Workspace, workspace_id)?
                {
                    row.allocated_bytes
                } else {
                    self.live
                        .storage()
                        .workspace_size(elevated.unwrap_or(false))
                }
            }
        };
        if !self
            .live
            .storage()
            .matches_tier(crate::VolumeKind::Workspace, bytes, false)
        {
            let _ = self.store.complete_product_operation(
                "workspace-restore",
                workspace_id,
                operation_id,
                "failed",
            )?;
            return Err(FabricError::Conflict(
                "workspace size is not a platform storage tier".into(),
            ));
        }
        if let Err(error) = self.ensure_runtime_class().await {
            let _ = self.store.complete_product_operation(
                "workspace-restore",
                workspace_id,
                operation_id,
                "unknown",
            )?;
            return Err(error);
        }
        let slot = match self
            .allocate_volume(
                crate::VolumeKind::WorkspaceRestore,
                workspace_id,
                bytes,
                Some(operation_id),
            )
            .await
        {
            Ok(slot) => slot,
            Err(error) => {
                let terminal = matches!(error, FabricError::Conflict(_) | FabricError::Realize(_));
                let _ = self.store.complete_product_operation(
                    "workspace-restore",
                    workspace_id,
                    operation_id,
                    if terminal { "failed" } else { "unknown" },
                );
                return Err(error);
            }
        };
        let extracted = self
            .extract_snapshot_onto_candidate(workspace_id, &slot.device, &path)
            .await;
        if let Err(error) = extracted {
            self.teardown_restore_candidate_objects(workspace_id, next_generation)
                .await;
            let terminal = matches!(error, FabricError::Realize(_) | FabricError::Conflict(_));
            let _ = self.store.complete_product_operation(
                "workspace-restore",
                workspace_id,
                operation_id,
                if terminal { "failed" } else { "unknown" },
            );
            return Err(error);
        }
        let old = existing;
        let old_pod = self
            .store
            .latest_generation(workspace_id)?
            .map(|row| row.pod_name);
        if let Err(error) = self
            .boot_restore_candidate(workspace_id, next_generation, &slot, bytes)
            .await
        {
            self.teardown_restore_candidate_objects(workspace_id, next_generation)
                .await;
            let terminal = matches!(error, FabricError::Realize(_) | FabricError::Conflict(_));
            let _ = self.store.complete_product_operation(
                "workspace-restore",
                workspace_id,
                operation_id,
                if terminal { "failed" } else { "unknown" },
            );
            return Err(error);
        }
        if let Err(error) = self
            .switch_workspace_to_candidate(workspace_id, next_generation, &slot, bytes)
            .await
        {
            let pointed = self
                .store
                .get_workspace(workspace_id)
                .ok()
                .flatten()
                .and_then(|row| row.lv_name);
            if pointed.as_ref() != slot.lv_name.as_ref() {
                self.teardown_restore_candidate_objects(workspace_id, next_generation)
                    .await;
            }
            let _ = self.store.complete_product_operation(
                "workspace-restore",
                workspace_id,
                operation_id,
                "unknown",
            )?;
            return Err(error);
        }
        if let Some(old) = old.as_ref() {
            if let Err(error) = self
                .retire_old_workspace_generation(workspace_id, old, old_pod.as_deref())
                .await
            {
                let _ = self.store.complete_product_operation(
                    "workspace-restore",
                    workspace_id,
                    operation_id,
                    "unknown",
                )?;
                return Err(error);
            }
        }
        self.ack_restore_artifact("workspace-restore-artifact", workspace_id, &path)?;
        self.store.complete_product_operation(
            "workspace-restore",
            workspace_id,
            operation_id,
            "terminal",
        )?;
        self.view(workspace_id)
    }

    async fn extract_snapshot_onto_candidate(
        &self,
        workspace_id: &str,
        device: &str,
        path: &std::path::Path,
    ) -> Result<(), FabricError> {
        self.live.mkfs_ext4_if_needed(device).await?;
        let mount = self.workspace_restore_mount(workspace_id);
        let mount_s = mount.to_string_lossy().into_owned();
        let _ = self.live.unmount(&mount_s).await;
        self.live.mount_ext4(device, &mount_s).await?;
        let extracted = crate::product_realize::extract_archive_file(path, &mount);
        let unmounted = self.live.unmount(&mount_s).await;
        extracted?;
        unmounted
    }

    async fn teardown_workspace_restore_candidate(&self, workspace_id: &str) {
        let mount = self.workspace_restore_mount(workspace_id);
        let _ = self.live.unmount(&mount.to_string_lossy()).await;
        let _ = self
            .free_volume(crate::VolumeKind::WorkspaceRestore, workspace_id)
            .await;
    }

    async fn teardown_restore_candidate_objects(&self, workspace_id: &str, generation: i64) {
        let (pv_name, pvc_name, pod_name) = restore_object_names(workspace_id, generation);
        let _ = self.live.delete_named("pod", &pod_name, true, 30).await;
        let _ = self.live.delete_named("pvc", &pvc_name, true, 30).await;
        let _ = self.live.delete_named("pv", &pv_name, false, 30).await;
        self.teardown_workspace_restore_candidate(workspace_id)
            .await;
    }

    async fn boot_restore_candidate(
        &self,
        id: &str,
        generation: i64,
        slot: &BlockSlot,
        bytes: u64,
    ) -> Result<(), FabricError> {
        let (pv_name, pvc_name, pod_name) = restore_object_names(id, generation);
        self.live.ensure_namespace().await?;
        self.live.ensure_storage_class().await?;
        self.live.ensure_workspace_service_account().await?;
        self.ensure_network_policy().await?;
        self.refuse_foreign(id, &pv_name, &pvc_name, &pod_name)
            .await?;
        require_stable_block_path(&slot.device)?;
        self.live
            .apply_yaml(&self.live.pv_yaml(id, &pv_name, &slot.device, bytes))
            .await?;
        let Some(pv) = self.live.get_pv(&pv_name).await? else {
            return Err(FabricError::Unknown(format!(
                "PV {pv_name} missing after apply"
            )));
        };
        self.live.verify_pv(&pv, id, &slot.device)?;
        self.live
            .apply_yaml(&self.live.pvc_yaml(id, &pvc_name, &pv_name, bytes))
            .await?;
        self.live
            .apply_yaml(&self.live.pod_yaml(id, &pod_name, &pvc_name, generation))
            .await?;
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
        self.probe_workspace_mount(&pod_name).await
    }

    async fn probe_workspace_mount(&self, pod_name: &str) -> Result<(), FabricError> {
        let output = self
            .live
            .exec_guest(pod_name, "runner", &["test", "-d", "/workspace"], 15_000)
            .await?;
        if output.ambiguous {
            return Err(FabricError::Unknown(
                "workspace probe did not settle".into(),
            ));
        }
        if output.exit_code != 0 {
            return Err(FabricError::Realize("workspace probe failed".into()));
        }
        Ok(())
    }

    async fn switch_workspace_to_candidate(
        &self,
        id: &str,
        generation: i64,
        slot: &BlockSlot,
        _bytes: u64,
    ) -> Result<(), FabricError> {
        let (pv_name, pvc_name, pod_name) = restore_object_names(id, generation);
        self.store.release_reservation(id, "restore-switch")?;
        self.store
            .reserve_volume(id, &slot.device, self.live.node_name(), &pv_name)?;
        self.store.upsert_workspace(&WorkspaceRow {
            id: id.to_owned(),
            state: "creating".into(),
            device: slot.device.clone(),
            node: self.live.node_name().to_owned(),
            pv_name: pv_name.clone(),
            pvc_name: pvc_name.clone(),
            lv_name: slot.lv_name.clone(),
        })?;
        let pod =
            self.live.get_pod(&pod_name).await?.ok_or_else(|| {
                FabricError::Unknown(format!("pod {pod_name} missing after Ready"))
            })?;
        if self
            .store
            .latest_generation(id)?
            .is_some_and(|row| row.generation == generation)
        {
            self.store.update_generation_runtime(
                id,
                generation,
                &pod.uid,
                pod.sandbox_id.as_deref(),
                "running",
            )?;
        } else {
            self.store.insert_generation(&GenerationRow {
                workspace_id: id.to_owned(),
                generation,
                pod_name: pod_name.clone(),
                pod_uid: Some(pod.uid.clone()),
                sandbox_id: pod.sandbox_id.clone(),
                state: "running".into(),
            })?;
        }
        self.promote_restore(id, crate::VolumeKind::Workspace)
            .await?;
        self.store.set_workspace_state(id, "ready")?;
        Ok(())
    }

    async fn retire_old_workspace_generation(
        &self,
        workspace_id: &str,
        old: &WorkspaceRow,
        old_pod: Option<&str>,
    ) -> Result<(), FabricError> {
        if old.state != "deleted" {
            let pod_name = old_pod
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| object_names(workspace_id, 1).2);
            self.live
                .delete_named("pod", &pod_name, true, self.live.residue_wait().as_secs())
                .await?;
            self.live
                .delete_named("pvc", &old.pvc_name, true, 30)
                .await?;
            self.live
                .delete_named("pv", &old.pv_name, false, 30)
                .await?;
        }
        let _ = self.store.delete_cleanup(workspace_id);
        let slot = BlockSlot {
            device: old.device.clone(),
            lv_name: Some(
                old.lv_name
                    .clone()
                    .unwrap_or_else(|| lv_name_for(workspace_id)),
            ),
            mapper_name: None,
        };
        self.live.release_block(&slot).await
    }

    /// Runs one typed test or build argv inside the Workspace guest. This is
    /// not user Bash: the argv is declared in `voie.toml` and the deadline is
    /// server-selected. Same operation hash is not dispatched again.
    pub async fn guest_run(
        &self,
        workspace_id: &str,
        operation_id: &str,
        request_hash: &str,
        relative_root: &str,
        argv: &[String],
    ) -> Result<i32, FabricError> {
        validate_pack_root(relative_root)?;
        if argv.is_empty() {
            return Err(FabricError::Config("guest argv is required"));
        }
        for part in argv {
            if part.is_empty() || part.contains('\n') || part.contains('\0') {
                return Err(FabricError::Config("guest argv is invalid"));
            }
        }
        let state = self.store.begin_product_operation(
            "workspace-guest-run",
            workspace_id,
            operation_id,
            request_hash,
        )?;
        let staged = self
            .release_root
            .join("guest-run")
            .join(workspace_id)
            .join(format!("{operation_id}.code"));
        if state != "dispatched" {
            if state == "unknown" {
                return Err(FabricError::Unknown(
                    "workspace guest-run outcome unknown; the intent will not be dispatched again"
                        .into(),
                ));
            }
            let code = std::fs::read_to_string(&staged)
                .ok()
                .and_then(|text| text.trim().parse().ok())
                .unwrap_or(0);
            return Ok(code);
        }
        let _lifecycle = self.lifecycle_guard(workspace_id).await;
        let workspace = self
            .store
            .get_workspace(workspace_id)?
            .ok_or(FabricError::NotFound)?;
        if workspace.state != "ready" {
            let _ = self.store.complete_product_operation(
                "workspace-guest-run",
                workspace_id,
                operation_id,
                "unknown",
            )?;
            return Err(FabricError::Realize(format!(
                "workspace {workspace_id} is {}",
                workspace.state
            )));
        }
        let generation = self
            .store
            .latest_generation(workspace_id)?
            .ok_or(FabricError::Realize("workspace has no execution".into()))?;
        let workdir = if relative_root == "." {
            "/workspace".to_owned()
        } else {
            format!("/workspace/{relative_root}")
        };
        let mut guest_argv = vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "cd \"$1\" && shift && exec \"$@\"".to_owned(),
            "voie-guest-run".to_owned(),
            workdir,
        ];
        guest_argv.extend(argv.iter().cloned());
        let refs: Vec<&str> = guest_argv.iter().map(String::as_str).collect();
        const RUN_TIMEOUT_MS: u64 = 300_000;
        let result = self
            .live
            .exec_guest(&generation.pod_name, "runner", &refs, RUN_TIMEOUT_MS)
            .await;
        match result {
            Ok(output) if !output.ambiguous => {
                if let Some(parent) = staged.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&staged, output.exit_code.to_string());
                let op_state = if output.exit_code == 0 {
                    "terminal"
                } else {
                    "failed"
                };
                let _ = self.store.complete_product_operation(
                    "workspace-guest-run",
                    workspace_id,
                    operation_id,
                    op_state,
                );
                Ok(output.exit_code)
            }
            _ => {
                let _ = self.store.complete_product_operation(
                    "workspace-guest-run",
                    workspace_id,
                    operation_id,
                    "unknown",
                )?;
                Err(FabricError::Unknown(
                    "workspace guest-run did not settle".into(),
                ))
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
        let pod_name = object_names(workspace_id, next).2;
        self.live
            .apply_yaml(
                &self
                    .live
                    .pod_yaml(workspace_id, &pod_name, &workspace.pvc_name, next),
            )
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

        let residue = if generation.is_none() && sandbox_id.is_none() {
            // Creating never reached a Ready pod, so this workspace never
            // owned a jail or VMM. Host-wide Firecracker presence from other
            // live guests must not pin its reservation forever.
            Residue {
                pod_present: self.live.get_pod(&pod_name).await?.is_some(),
                jail_present: false,
                vmm_present: false,
                children_present: false,
            }
        } else {
            self.live
                .wait_residue_gone(&pod_name, sandbox_id.as_deref(), self.live.residue_wait())
                .await?
        };

        let pv = self.live.get_pv(&workspace.pv_name).await?;
        let pvc = self.live.get_namespaced("pvc", &workspace.pvc_name).await?;
        let sandbox_absent = self
            .live
            .wait_sandbox_absent(&pod_name, self.live.residue_wait())
            .await?;
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
                mapper_name: None,
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
            let _ = self
                .store
                .delete_allocation(crate::VolumeKind::Workspace, workspace_id);
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
            allocated_bytes: self
                .store
                .get_allocation(crate::VolumeKind::Workspace, &row.id)?
                .map(|row| row.allocated_bytes)
                .unwrap_or(0),
            image: String::new(),
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

fn validate_pack_root(relative_root: &str) -> Result<(), FabricError> {
    if relative_root.is_empty()
        || relative_root.starts_with('/')
        || relative_root.contains('\0')
        || relative_root.contains('\n')
    {
        return Err(FabricError::Config("pack root is invalid"));
    }
    for component in relative_root.split('/') {
        if component.is_empty() || component == ".." {
            return Err(FabricError::Config("pack root is invalid"));
        }
    }
    Ok(())
}

fn pack_hash_from_stdout(stdout: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    value
        .get("artifactHash")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn workspace_allows_exec(state: &str) -> bool {
    state == "ready"
}

fn workspace_allows_snapshot(state: &str) -> bool {
    matches!(state, "ready" | "fenced")
}

fn parse_df_use_percent(stdout: &str) -> Option<u64> {
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("Filesystem") {
            continue;
        }
        let capacity = line.split_whitespace().nth(4)?;
        let digits = capacity.trim_end_matches('%');
        if let Ok(percent) = digits.parse::<u64>() {
            return Some(percent);
        }
    }
    None
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

    #[test]
    fn df_use_percent_parses_posix_capacity() {
        let stdout = "Filesystem     1024-blocks     Used Available Capacity Mounted on\n\
/dev/workspace    16777216  14680064   2097152      88% /workspace\n";
        assert_eq!(parse_df_use_percent(stdout), Some(88));
        assert_eq!(parse_df_use_percent("Filesystem\n"), None);
    }

    #[test]
    fn fenced_workspace_rejects_exec_and_allows_snapshot() {
        assert!(workspace_allows_exec("ready"));
        assert!(!workspace_allows_exec("fenced"));
        assert!(!workspace_allows_exec("replacing"));
        assert!(!workspace_allows_exec("deleting"));
        assert!(workspace_allows_snapshot("ready"));
        assert!(workspace_allows_snapshot("fenced"));
        assert!(!workspace_allows_snapshot("deleting"));
    }

    #[test]
    fn staging_findmnt_accepts_lvm_mapper_encoding() {
        assert!(staging_source_from_vg(
            "/dev/voie-ws/stage",
            "voie-ws",
            "stage"
        ));
        assert!(staging_source_from_vg(
            "/dev/mapper/voie--ws-stage",
            "voie-ws",
            "stage"
        ));
        assert!(!staging_source_from_vg("/dev/sda1", "voie-ws", "stage"));
        assert!(!staging_source_from_vg(
            "/dev/mapper/other--vg-stage",
            "voie-ws",
            "stage"
        ));
        assert!(!staging_source_from_vg(
            "/dev/not-the-vg/voie-ws/stage",
            "voie-ws",
            "stage"
        ));
        assert!(!staging_source_from_vg(
            "/dev/mapper/voie--ws-stage-extra",
            "voie-ws",
            "stage"
        ));
        assert!(staging_source_from_vg(
            " /dev/mapper/voie--ws-stage\n",
            "voie-ws",
            "stage"
        ));
    }

    #[test]
    fn production_stage_mode_refuses_missing_volume_and_missing_mode() {
        assert!(require_stage_mode(None, None).is_err());
        assert!(require_stage_mode(Some("lvm"), None).is_err());
        assert!(require_stage_mode(Some("lvm"), Some("")).is_err());
        assert!(require_stage_mode(Some("guess"), Some("voie-ws/stage")).is_err());
        assert!(
            require_stage_mode(Some("dev-directory"), None)
                .unwrap()
                .is_none()
        );
        let spec = require_stage_mode(Some("lvm"), Some("voie-ws/stage")).unwrap();
        assert_eq!(spec, Some(("voie-ws".into(), "stage".into())));
    }

    #[test]
    fn unlink_staged_path_removes_a_file_and_refuses_a_directory() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let file = std::env::temp_dir().join(format!("voie-unlink-file-{stamp}"));
        std::fs::write(&file, b"staged").expect("write");
        unlink_staged_path(&file).expect("unlink file");
        assert!(!file.exists());
        unlink_staged_path(&file).expect("absent file is already released");
        let dir = std::env::temp_dir().join(format!("voie-unlink-dir-{stamp}"));
        std::fs::create_dir_all(&dir).expect("dir");
        assert!(
            unlink_staged_path(&dir).is_err(),
            "a directory left after unlink must not count as released"
        );
        assert!(dir.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
