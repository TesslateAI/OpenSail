//! Profile-1 Fabric storage: thin Workspaces, linear Databases/Deployments.
//! Runtime snapshots stay in the `runtime` thin pool. Workspaces use a
//! dedicated `workspace` thin pool. Databases and Deployments stay linear.

use crate::FabricError;

pub const MIB: u64 = 1024 * 1024;
pub const GIB: u64 = 1024 * MIB;

/// Fabric-1 runtime snapshots. Containerd never uses the Workspace pool.
pub const RUNTIME_POOL_BYTES: u64 = 64 * GIB;
pub const RUNTIME_POOL_METADATA_BYTES: u64 = GIB;
/// Dedicated Workspace thin-pool data. Internal budgets: 128 + 64 restore + 64 staging + 8.
pub const WORKSPACE_POOL_DATA_BYTES: u64 = 264 * GIB;
pub const WORKSPACE_POOL_METADATA_BYTES: u64 = 2 * GIB;
pub const WORKSPACE_NORMAL_BUDGET_BYTES: u64 = 128 * GIB;
pub const WORKSPACE_RESTORE_HEADROOM_BYTES: u64 = 64 * GIB;
pub const WORKSPACE_POOL_SLACK_BYTES: u64 = 8 * GIB;

pub const WORKSPACE_BYTES: u64 = 16 * GIB;
pub const WORKSPACE_LARGE_BYTES: u64 = 32 * GIB;
pub const WORKSPACE_ELEVATED_BYTES: u64 = 64 * GIB;
pub const WORKSPACE_GROW_PRESSURE_PERCENT: u64 = 85;

/// Physical remainder on a 475 GiB Fabric-1 VG after runtime, workspace
/// pools+metadata, and the 48 GiB recovery reserve: 96 GiB.
pub const LINEAR_NORMAL_BUDGET_BYTES: u64 = 96 * GIB;
pub const RECOVERY_RESERVE_BYTES: u64 = 48 * GIB;
pub const EMERGENCY_FLOOR_BYTES: u64 = 16 * GIB;
pub const DATABASE_RESTORE_BUDGET_BYTES: u64 = 32 * GIB;
/// Host staging for backup/snapshot/restore artifacts lives on this thin
/// volume in the workspace pool, never on the OS disk.
pub const STAGING_VOLUME_BYTES: u64 = 64 * GIB;

pub const DATABASE_DEV_BYTES: u64 = 8 * GIB;
pub const DATABASE_DEV_ELEVATED_BYTES: u64 = 16 * GIB;
pub const DATABASE_PROD_BYTES: u64 = 16 * GIB;
pub const DATABASE_PROD_ELEVATED_BYTES: u64 = 32 * GIB;
pub const DEPLOYMENT_BYTES: u64 = GIB;

/// Guest dump, host copy, and pg_restore of product volumes up to 32 GiB.
/// 180s cannot finish an 8–32 GiB stream; Fabric HTTP already waits 3600s.
pub const PRODUCT_VOLUME_IO_TIMEOUT_MS: u64 = 3_600_000;

const KINDS: &[&str] = &[
    "workspace",
    "database",
    "deployment",
    "workspace_restore",
    "database_restore",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeKind {
    Workspace,
    Database,
    Deployment,
    WorkspaceRestore,
    DatabaseRestore,
}

impl VolumeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            VolumeKind::Workspace => "workspace",
            VolumeKind::Database => "database",
            VolumeKind::Deployment => "deployment",
            VolumeKind::WorkspaceRestore => "workspace_restore",
            VolumeKind::DatabaseRestore => "database_restore",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "workspace" => Some(VolumeKind::Workspace),
            "database" => Some(VolumeKind::Database),
            "deployment" => Some(VolumeKind::Deployment),
            "workspace_restore" => Some(VolumeKind::WorkspaceRestore),
            "database_restore" => Some(VolumeKind::DatabaseRestore),
            // Leftover sqlite rows from the linear-restore layout were
            // Database restore candidates.
            "restore" => Some(VolumeKind::DatabaseRestore),
            _ => None,
        }
    }

    pub fn is_thin(self) -> bool {
        matches!(self, VolumeKind::Workspace | VolumeKind::WorkspaceRestore)
    }

    pub fn is_linear_normal(self) -> bool {
        matches!(self, VolumeKind::Database | VolumeKind::Deployment)
    }

    pub fn restore_source(self) -> Option<VolumeKind> {
        match self {
            VolumeKind::Workspace => Some(VolumeKind::WorkspaceRestore),
            VolumeKind::Database => Some(VolumeKind::DatabaseRestore),
            _ => None,
        }
    }
}

/// Estate-selected platform tiers. Production Ansible pins the Fabric-1
/// sizes; the development VM may shrink them to fit its disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoragePolicy {
    pub runtime_pool_bytes: u64,
    pub workspace_pool: String,
    pub workspace_pool_data_bytes: u64,
    pub workspace_pool_metadata_bytes: u64,
    pub workspace_normal_budget_bytes: u64,
    pub workspace_restore_headroom_bytes: u64,
    pub staging_volume_bytes: u64,
    pub workspace_bytes: u64,
    pub workspace_large_bytes: u64,
    pub workspace_elevated_bytes: u64,
    pub linear_normal_budget_bytes: u64,
    pub recovery_reserve_bytes: u64,
    pub emergency_floor_bytes: u64,
    pub database_restore_budget_bytes: u64,
    pub database_dev_bytes: u64,
    pub database_dev_elevated_bytes: u64,
    pub database_prod_bytes: u64,
    pub database_prod_elevated_bytes: u64,
    pub deployment_bytes: u64,
}

impl StoragePolicy {
    pub fn production() -> Self {
        StoragePolicy {
            runtime_pool_bytes: RUNTIME_POOL_BYTES,
            workspace_pool: "workspace".into(),
            workspace_pool_data_bytes: WORKSPACE_POOL_DATA_BYTES,
            workspace_pool_metadata_bytes: WORKSPACE_POOL_METADATA_BYTES,
            workspace_normal_budget_bytes: WORKSPACE_NORMAL_BUDGET_BYTES,
            workspace_restore_headroom_bytes: WORKSPACE_RESTORE_HEADROOM_BYTES,
            staging_volume_bytes: STAGING_VOLUME_BYTES,
            workspace_bytes: WORKSPACE_BYTES,
            workspace_large_bytes: WORKSPACE_LARGE_BYTES,
            workspace_elevated_bytes: WORKSPACE_ELEVATED_BYTES,
            linear_normal_budget_bytes: LINEAR_NORMAL_BUDGET_BYTES,
            recovery_reserve_bytes: RECOVERY_RESERVE_BYTES,
            emergency_floor_bytes: EMERGENCY_FLOOR_BYTES,
            database_restore_budget_bytes: DATABASE_RESTORE_BUDGET_BYTES,
            database_dev_bytes: DATABASE_DEV_BYTES,
            database_dev_elevated_bytes: DATABASE_DEV_ELEVATED_BYTES,
            database_prod_bytes: DATABASE_PROD_BYTES,
            database_prod_elevated_bytes: DATABASE_PROD_ELEVATED_BYTES,
            deployment_bytes: DEPLOYMENT_BYTES,
        }
    }

    /// Compact sizes so unit tests keep small `lvcreate` arguments.
    pub fn test() -> Self {
        StoragePolicy {
            runtime_pool_bytes: 0,
            workspace_pool: "workspace".into(),
            workspace_pool_data_bytes: 0,
            workspace_pool_metadata_bytes: 0,
            workspace_normal_budget_bytes: 4 * GIB,
            workspace_restore_headroom_bytes: 2 * GIB,
            staging_volume_bytes: 0,
            workspace_bytes: GIB,
            workspace_large_bytes: 2 * GIB,
            workspace_elevated_bytes: 4 * GIB,
            linear_normal_budget_bytes: 8 * GIB,
            recovery_reserve_bytes: GIB,
            emergency_floor_bytes: 512 * MIB,
            database_restore_budget_bytes: 2 * GIB,
            database_dev_bytes: GIB,
            database_dev_elevated_bytes: 2 * GIB,
            database_prod_bytes: GIB,
            database_prod_elevated_bytes: 2 * GIB,
            deployment_bytes: GIB,
        }
    }

    pub fn from_env() -> Result<Self, FabricError> {
        let mut policy = Self::production();
        if let Some(value) = env_bytes("VOIE_STORAGE_RUNTIME_POOL")? {
            policy.runtime_pool_bytes = value;
        }
        if let Some(value) = env_string("VOIE_STORAGE_WORKSPACE_POOL") {
            if value == "workspaces" || value == "ws-root" {
                return Err(FabricError::Config(
                    "VOIE_STORAGE_WORKSPACE_POOL must not be the retired workspaces pool",
                ));
            }
            policy.workspace_pool = value;
        }
        if let Some(value) = env_bytes("VOIE_STORAGE_WORKSPACE_POOL_DATA")? {
            policy.workspace_pool_data_bytes = value;
        }
        if let Some(value) = env_bytes("VOIE_STORAGE_WORKSPACE_NORMAL_BUDGET")? {
            policy.workspace_normal_budget_bytes = value;
        }
        if let Some(value) = env_bytes("VOIE_STORAGE_WORKSPACE_RESTORE_HEADROOM")? {
            policy.workspace_restore_headroom_bytes = value;
        }
        if let Some(value) = env_bytes("VOIE_STORAGE_STAGING")? {
            policy.staging_volume_bytes = value;
        }
        if let Some(value) = env_bytes("VOIE_STORAGE_WORKSPACE_DEFAULT")? {
            policy.workspace_bytes = value;
        }
        if let Some(value) = env_bytes("VOIE_STORAGE_WORKSPACE_LARGE")? {
            policy.workspace_large_bytes = value;
        }
        if let Some(value) = env_bytes("VOIE_STORAGE_WORKSPACE_ELEVATED")? {
            policy.workspace_elevated_bytes = value;
        }
        if let Some(value) = env_bytes("VOIE_STORAGE_LINEAR_NORMAL_BUDGET")? {
            policy.linear_normal_budget_bytes = value;
        }
        if let Some(value) = env_bytes("VOIE_STORAGE_LINEAR_RECOVERY_RESERVE")? {
            policy.recovery_reserve_bytes = value;
        }
        if let Some(value) = env_bytes("VOIE_STORAGE_EMERGENCY_FLOOR")? {
            policy.emergency_floor_bytes = value;
        }
        if let Some(value) = env_bytes("VOIE_STORAGE_DATABASE_DEV")? {
            policy.database_dev_bytes = value;
        }
        if let Some(value) = env_bytes("VOIE_STORAGE_DATABASE_DEV_ELEVATED")? {
            policy.database_dev_elevated_bytes = value;
        }
        if let Some(value) = env_bytes("VOIE_STORAGE_DATABASE_PROD")? {
            policy.database_prod_bytes = value;
        }
        if let Some(value) = env_bytes("VOIE_STORAGE_DATABASE_PROD_ELEVATED")? {
            policy.database_prod_elevated_bytes = value;
        }
        if let Some(value) = env_bytes("VOIE_STORAGE_DEPLOYMENT")? {
            policy.deployment_bytes = value;
        }
        policy.database_restore_budget_bytes = policy
            .database_prod_elevated_bytes
            .max(policy.database_dev_elevated_bytes);
        if policy.workspace_large_bytes < policy.workspace_bytes
            || policy.workspace_elevated_bytes < policy.workspace_large_bytes
            || policy.database_dev_elevated_bytes < policy.database_dev_bytes
            || policy.database_prod_elevated_bytes < policy.database_prod_bytes
        {
            return Err(FabricError::Config(
                "elevated storage tiers must not be smaller than the default tiers",
            ));
        }
        let workspace_logical = policy
            .workspace_normal_budget_bytes
            .saturating_add(policy.workspace_restore_headroom_bytes)
            .saturating_add(policy.staging_volume_bytes);
        if policy.workspace_pool_data_bytes > 0
            && workspace_logical > policy.workspace_pool_data_bytes
        {
            return Err(FabricError::Config(
                "workspace logical budgets exceed the workspace thin-pool data size",
            ));
        }
        Ok(policy)
    }

    pub fn workspace_size(&self, elevated: bool) -> u64 {
        if elevated {
            self.workspace_elevated_bytes
        } else {
            self.workspace_bytes
        }
    }

    pub fn workspace_size_for_tier(&self, tier: &str) -> u64 {
        match tier {
            "elevated" => self.workspace_elevated_bytes,
            "large" => self.workspace_large_bytes,
            _ => self.workspace_bytes,
        }
    }

    pub fn workspace_tier_name(&self, bytes: u64) -> Option<&'static str> {
        if bytes == self.workspace_bytes {
            Some("default")
        } else if bytes == self.workspace_large_bytes {
            Some("large")
        } else if bytes == self.workspace_elevated_bytes {
            Some("elevated")
        } else {
            None
        }
    }

    pub fn database_size(&self, prod: bool, elevated: bool) -> u64 {
        match (prod, elevated) {
            (false, false) => self.database_dev_bytes,
            (false, true) => self.database_dev_elevated_bytes,
            (true, false) => self.database_prod_bytes,
            (true, true) => self.database_prod_elevated_bytes,
        }
    }

    pub fn workspace_pool_slack_bytes(&self) -> u64 {
        self.workspace_pool_data_bytes.saturating_sub(
            self.workspace_normal_budget_bytes
                .saturating_add(self.workspace_restore_headroom_bytes)
                .saturating_add(self.staging_volume_bytes),
        )
    }

    pub fn matches_tier(&self, kind: VolumeKind, bytes: u64, prod: bool) -> bool {
        match kind {
            VolumeKind::Workspace | VolumeKind::WorkspaceRestore => {
                bytes == self.workspace_bytes
                    || bytes == self.workspace_large_bytes
                    || bytes == self.workspace_elevated_bytes
            }
            VolumeKind::Database | VolumeKind::DatabaseRestore => {
                bytes == self.database_size(prod, false) || bytes == self.database_size(prod, true)
            }
            VolumeKind::Deployment => bytes == self.deployment_bytes,
        }
    }

    pub fn next_extension(&self, kind: VolumeKind, current: u64, prod: bool) -> Option<u64> {
        match kind {
            VolumeKind::Workspace if current == self.workspace_bytes => {
                Some(self.workspace_large_bytes)
            }
            VolumeKind::Workspace if current == self.workspace_large_bytes => {
                Some(self.workspace_elevated_bytes)
            }
            VolumeKind::Database if current == self.database_size(prod, false) => {
                Some(self.database_size(prod, true))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeAllocation {
    pub kind: VolumeKind,
    pub resource_id: String,
    pub lv_name: String,
    pub allocated_bytes: u64,
    pub state: String,
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapacityReport {
    pub device_bytes: u64,
    pub health: &'static str,
    pub runtime: RuntimeCapacity,
    pub workspaces: WorkspaceCapacity,
    pub linear: LinearCapacity,
    pub recovery: RecoveryCapacity,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeCapacity {
    pub pool_bytes: u64,
    pub used_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceCapacity {
    pub pool_bytes: u64,
    pub pool_used_bytes: u64,
    pub logical_budget_bytes: u64,
    pub logical_allocated_bytes: u64,
    pub restore_headroom_bytes: u64,
    pub restore_allocated_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinearCapacity {
    pub budget_bytes: u64,
    pub allocated_bytes: u64,
    pub allocatable_now_bytes: u64,
    pub databases_bytes: u64,
    pub deployments_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryCapacity {
    pub reserve_bytes: u64,
    pub emergency_floor_bytes: u64,
    pub physical_free_bytes: u64,
}

pub fn admit_budget(
    allocated: u64,
    requested: u64,
    budget: u64,
    label: &str,
) -> Result<(), FabricError> {
    let Some(total) = allocated.checked_add(requested) else {
        return Err(FabricError::Conflict(format!(
            "storage allocation overflows the {label} budget"
        )));
    };
    if total > budget {
        return Err(FabricError::Conflict(format!(
            "requested {requested} bytes exceeds the {budget} byte {label} budget"
        )));
    }
    Ok(())
}

pub fn admit_normal(allocated: u64, requested: u64, budget: u64) -> Result<(), FabricError> {
    admit_budget(allocated, requested, budget, "linear")
}

pub fn admit_workspace(allocated: u64, requested: u64, budget: u64) -> Result<(), FabricError> {
    admit_budget(allocated, requested, budget, "workspace")
}

/// Permanent-budget check when a restore candidate becomes a live
/// Workspace or Database. Restore headroom and recovery reserve are
/// transition capacity, not hidden permanent capacity.
pub fn admit_permanent_promotion(
    normal_allocated: u64,
    existing_live: u64,
    candidate: u64,
    budget: u64,
    label: &str,
) -> Result<(), FabricError> {
    admit_budget(
        normal_allocated.saturating_sub(existing_live),
        candidate,
        budget,
        label,
    )
}

pub fn admit_workspace_restore(
    allocated: u64,
    requested: u64,
    headroom: u64,
) -> Result<(), FabricError> {
    admit_budget(allocated, requested, headroom, "workspace restore")
}

pub fn admit_linear(
    allocated: u64,
    requested: u64,
    budget: u64,
    physical_free: u64,
    recovery_reserve: u64,
) -> Result<(), FabricError> {
    admit_budget(allocated, requested, budget, "linear")?;
    let Some(needed) = requested.checked_add(recovery_reserve) else {
        return Err(FabricError::Conflict(
            "linear allocation overflows physical free space".into(),
        ));
    };
    if physical_free < needed {
        return Err(FabricError::Conflict(format!(
            "linear allocation needs {requested} bytes plus a {recovery_reserve} byte recovery reserve"
        )));
    }
    Ok(())
}

pub fn linear_allocatable_now(
    budget: u64,
    allocated: u64,
    physical_free: u64,
    recovery_reserve: u64,
) -> u64 {
    let policy = budget.saturating_sub(allocated);
    let physical = physical_free.saturating_sub(recovery_reserve);
    policy.min(physical)
}

pub fn admit_database_restore(
    allocated: u64,
    requested: u64,
    budget: u64,
    physical_free: u64,
    emergency_floor: u64,
) -> Result<(), FabricError> {
    admit_budget(allocated, requested, budget, "database restore")?;
    let Some(needed) = requested.checked_add(emergency_floor) else {
        return Err(FabricError::Conflict(
            "database restore overflows physical free space".into(),
        ));
    };
    if physical_free < needed {
        return Err(FabricError::Conflict(format!(
            "database restore needs {requested} bytes plus a {emergency_floor} byte emergency floor"
        )));
    }
    Ok(())
}

pub fn capacity_health(
    physical_free: u64,
    emergency_floor: u64,
    workspace_pool: u64,
    workspace_pool_used: u64,
    workspace_pool_slack: u64,
    workspace_metadata_percent: Option<f64>,
    workspace_logical_allocated: u64,
    workspace_logical_budget: u64,
    linear_allocated: u64,
    linear_budget: u64,
    runtime_used: u64,
    runtime_pool: u64,
) -> &'static str {
    if physical_free < emergency_floor {
        return "critical";
    }
    if workspace_metadata_percent.is_some_and(|percent| percent >= 90.0) {
        return "critical";
    }
    if workspace_pool > 0 {
        let headroom_floor = workspace_pool.saturating_sub(workspace_pool_slack);
        if workspace_pool_used > headroom_floor {
            return "critical";
        }
    }
    let workspace_warn = workspace_logical_budget > 0
        && workspace_logical_allocated * 100 / workspace_logical_budget >= 80;
    let linear_warn = linear_budget > 0 && linear_allocated * 100 / linear_budget >= 80;
    let runtime_warn = runtime_pool > 0 && runtime_used * 100 / runtime_pool >= 75;
    let pool_warn = workspace_pool > 0 && workspace_pool_used * 100 / workspace_pool >= 75;
    if workspace_warn || linear_warn || runtime_warn || pool_warn {
        "warning"
    } else {
        "healthy"
    }
}

pub fn lv_size_arg(bytes: u64) -> String {
    if bytes % GIB == 0 {
        format!("{}G", bytes / GIB)
    } else if bytes % MIB == 0 {
        format!("{}M", bytes / MIB)
    } else {
        format!("{bytes}B")
    }
}

pub fn k8s_quantity(bytes: u64) -> String {
    if bytes % GIB == 0 {
        format!("{}Gi", bytes / GIB)
    } else if bytes % MIB == 0 {
        format!("{}Mi", bytes / MIB)
    } else {
        format!("{bytes}")
    }
}

/// Retired Ansible layout: a durable product thin pool named `workspaces`
/// and a 200 GiB `ws-root`. The daemon must not treat those volumes as
/// crashed prepares and must not allocate beside them.
pub fn refuse_legacy_product_pool(lv_names: &[String]) -> Result<(), FabricError> {
    let retired: Vec<&str> = lv_names
        .iter()
        .map(String::as_str)
        .filter(|name| *name == "workspaces" || *name == "ws-root")
        .collect();
    if retired.is_empty() {
        return Ok(());
    }
    Err(FabricError::Realize(format!(
        "volume group still has retired product volumes {}; there is no durable product thin pool named workspaces",
        retired.join(", ")
    )))
}

/// The recovery reserve is unused VG extents, not an LV.
pub fn refuse_allocated_recovery_reserve(lv_names: &[String]) -> Result<(), FabricError> {
    let named: Vec<&str> = lv_names
        .iter()
        .map(String::as_str)
        .filter(|name| *name == "reserve" || *name == "recovery")
        .collect();
    if named.is_empty() {
        return Ok(());
    }
    Err(FabricError::Realize(format!(
        "volume group has allocated recovery volumes {}; the reserve stays physically unallocated",
        named.join(", ")
    )))
}

/// Containerd/Firecracker snapshots live only in the `runtime` thin pool.
pub fn require_runtime_pool(lv_names: &[String]) -> Result<(), FabricError> {
    if lv_names.iter().any(|name| name == "runtime") {
        return Ok(());
    }
    Err(FabricError::Realize(
        "volume group has no runtime thin pool; containerd snapshots are not a product volume"
            .into(),
    ))
}

pub fn require_workspace_pool(lv_names: &[String], pool: &str) -> Result<(), FabricError> {
    if pool.is_empty() {
        return Ok(());
    }
    if lv_names.iter().any(|name| name == pool) {
        return Ok(());
    }
    Err(FabricError::Realize(format!(
        "volume group has no workspace thin pool `{pool}`"
    )))
}

/// Fabric-1 runtime snapshots occupy 64 GiB. LVM extent rounding may
/// land a few hundred MiB short; a compact estate sets the expected size
/// explicitly. Zero expected means presence was already checked.
pub fn require_runtime_pool_size(observed: u64, expected: u64) -> Result<(), FabricError> {
    require_pool_size("runtime", observed, expected)
}

pub fn require_workspace_pool_size(observed: u64, expected: u64) -> Result<(), FabricError> {
    require_pool_size("workspace", observed, expected)
}

fn require_pool_size(label: &str, observed: u64, expected: u64) -> Result<(), FabricError> {
    if expected == 0 {
        return Ok(());
    }
    let slack = (expected / 64).max(1);
    if observed >= expected.saturating_sub(slack) {
        return Ok(());
    }
    Err(FabricError::Realize(format!(
        "{label} thin pool is {observed} bytes; Fabric-1 requires {expected} bytes"
    )))
}

pub fn parse_size(raw: &str) -> Result<u64, FabricError> {
    let trimmed = raw.trim();
    let (digits, factor) = if let Some(rest) = trimmed.strip_suffix("Gi") {
        (rest, GIB)
    } else if let Some(rest) = trimmed.strip_suffix("Mi") {
        (rest, MIB)
    } else if let Some(rest) = trimmed.strip_suffix('G') {
        (rest, GIB)
    } else if let Some(rest) = trimmed.strip_suffix('M') {
        (rest, MIB)
    } else if let Some(rest) = trimmed.strip_suffix('B') {
        (rest, 1)
    } else {
        (trimmed, 1)
    };
    let count: u64 = digits
        .trim()
        .parse()
        .map_err(|_| FabricError::Config("storage size is not a number"))?;
    count
        .checked_mul(factor)
        .ok_or(FabricError::Config("storage size overflows"))
}

#[allow(dead_code)]
pub fn known_kind(value: &str) -> bool {
    KINDS.contains(&value) || value == "restore"
}

fn env_bytes(name: &str) -> Result<Option<u64>, FabricError> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(Some(parse_size(&value)?)),
        _ => Ok(None),
    }
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_default_virtual_size_is_sixteen_gib() {
        assert_eq!(WORKSPACE_BYTES, 16 * GIB);
        assert_eq!(StoragePolicy::production().workspace_bytes, 16 * GIB);
        assert_eq!(WORKSPACE_POOL_SLACK_BYTES, 8 * GIB);
    }

    #[test]
    fn workspace_logical_budget_cannot_exceed_one_hundred_twenty_eight_gib() {
        admit_workspace(0, 16 * GIB, WORKSPACE_NORMAL_BUDGET_BYTES).unwrap();
        admit_workspace(
            WORKSPACE_NORMAL_BUDGET_BYTES,
            GIB,
            WORKSPACE_NORMAL_BUDGET_BYTES,
        )
        .unwrap_err();
        admit_workspace(
            WORKSPACE_NORMAL_BUDGET_BYTES - 16 * GIB,
            16 * GIB,
            WORKSPACE_NORMAL_BUDGET_BYTES,
        )
        .unwrap();
        admit_workspace(120 * GIB, 16 * GIB, WORKSPACE_NORMAL_BUDGET_BYTES).unwrap_err();
    }

    #[test]
    fn workspace_restore_cannot_exceed_sixty_four_gib() {
        admit_workspace_restore(0, 64 * GIB, WORKSPACE_RESTORE_HEADROOM_BYTES).unwrap();
        admit_workspace_restore(0, 16 * GIB, WORKSPACE_RESTORE_HEADROOM_BYTES).unwrap();
        admit_workspace_restore(64 * GIB, 16 * GIB, WORKSPACE_RESTORE_HEADROOM_BYTES).unwrap_err();
        admit_workspace_restore(48 * GIB, 32 * GIB, WORKSPACE_RESTORE_HEADROOM_BYTES).unwrap_err();
    }

    #[test]
    fn normal_workspace_cannot_consume_restore_headroom() {
        let policy = StoragePolicy::production();
        admit_workspace(128 * GIB, 16 * GIB, policy.workspace_normal_budget_bytes).unwrap_err();
        admit_workspace_restore(0, 16 * GIB, policy.workspace_restore_headroom_bytes).unwrap();
        assert_eq!(
            policy.workspace_normal_budget_bytes
                + policy.workspace_restore_headroom_bytes
                + policy.staging_volume_bytes
                + policy.workspace_pool_slack_bytes(),
            policy.workspace_pool_data_bytes
        );
    }

    #[test]
    fn restore_promotion_reenters_the_permanent_budget() {
        admit_permanent_promotion(
            WORKSPACE_NORMAL_BUDGET_BYTES,
            0,
            64 * GIB,
            WORKSPACE_NORMAL_BUDGET_BYTES,
            "workspace",
        )
        .unwrap_err();
        admit_permanent_promotion(
            WORKSPACE_NORMAL_BUDGET_BYTES,
            64 * GIB,
            64 * GIB,
            WORKSPACE_NORMAL_BUDGET_BYTES,
            "workspace",
        )
        .unwrap();
        admit_permanent_promotion(
            LINEAR_NORMAL_BUDGET_BYTES,
            0,
            16 * GIB,
            LINEAR_NORMAL_BUDGET_BYTES,
            "linear",
        )
        .unwrap_err();
        admit_permanent_promotion(
            LINEAR_NORMAL_BUDGET_BYTES,
            16 * GIB,
            16 * GIB,
            LINEAR_NORMAL_BUDGET_BYTES,
            "linear",
        )
        .unwrap();
    }

    #[test]
    fn linear_allocation_cannot_exceed_ninety_six_gib() {
        admit_linear(
            0,
            32 * GIB,
            LINEAR_NORMAL_BUDGET_BYTES,
            200 * GIB,
            RECOVERY_RESERVE_BYTES,
        )
        .unwrap();
        admit_linear(
            LINEAR_NORMAL_BUDGET_BYTES,
            GIB,
            LINEAR_NORMAL_BUDGET_BYTES,
            200 * GIB,
            RECOVERY_RESERVE_BYTES,
        )
        .unwrap_err();
    }

    #[test]
    fn allocatable_now_is_the_min_of_policy_remainder_and_physical_minus_reserve() {
        assert_eq!(
            linear_allocatable_now(96 * GIB, 0, 144 * GIB, 48 * GIB),
            96 * GIB
        );
        assert_eq!(
            linear_allocatable_now(96 * GIB, 9 * GIB, 70 * GIB + 94 * MIB, 48 * GIB),
            22 * GIB + 94 * MIB
        );
        assert_eq!(
            linear_allocatable_now(96 * GIB, 90 * GIB, 200 * GIB, 48 * GIB),
            6 * GIB
        );
        assert_eq!(
            linear_allocatable_now(96 * GIB, 96 * GIB, 200 * GIB, 48 * GIB),
            0
        );
        assert_eq!(linear_allocatable_now(96 * GIB, 0, 40 * GIB, 48 * GIB), 0);
    }

    #[test]
    fn fabric1_vg_equation_fits_four_hundred_seventy_five_gib() {
        assert_eq!(
            RUNTIME_POOL_BYTES
                + RUNTIME_POOL_METADATA_BYTES
                + WORKSPACE_POOL_DATA_BYTES
                + WORKSPACE_POOL_METADATA_BYTES
                + LINEAR_NORMAL_BUDGET_BYTES
                + RECOVERY_RESERVE_BYTES,
            475 * GIB
        );
    }

    #[test]
    fn normal_linear_allocation_preserves_forty_eight_gib_physical_reserve() {
        admit_linear(
            0,
            16 * GIB,
            LINEAR_NORMAL_BUDGET_BYTES,
            64 * GIB,
            RECOVERY_RESERVE_BYTES,
        )
        .unwrap();
        admit_linear(
            0,
            16 * GIB,
            LINEAR_NORMAL_BUDGET_BYTES,
            63 * GIB,
            RECOVERY_RESERVE_BYTES,
        )
        .unwrap_err();
        admit_linear(
            0,
            GIB,
            LINEAR_NORMAL_BUDGET_BYTES,
            48 * GIB,
            RECOVERY_RESERVE_BYTES,
        )
        .unwrap_err();
    }

    #[test]
    fn database_restore_leaves_sixteen_gib_physical_floor() {
        admit_database_restore(
            0,
            32 * GIB,
            DATABASE_RESTORE_BUDGET_BYTES,
            48 * GIB,
            EMERGENCY_FLOOR_BYTES,
        )
        .unwrap();
        admit_database_restore(
            0,
            32 * GIB,
            DATABASE_RESTORE_BUDGET_BYTES,
            47 * GIB,
            EMERGENCY_FLOOR_BYTES,
        )
        .unwrap_err();
        admit_database_restore(
            32 * GIB,
            16 * GIB,
            DATABASE_RESTORE_BUDGET_BYTES,
            64 * GIB,
            EMERGENCY_FLOOR_BYTES,
        )
        .unwrap_err();
    }

    #[test]
    fn only_documented_workspace_extensions_exist() {
        let policy = StoragePolicy::production();
        assert_eq!(
            policy.next_extension(VolumeKind::Workspace, 16 * GIB, false),
            Some(32 * GIB)
        );
        assert_eq!(
            policy.next_extension(VolumeKind::Workspace, 32 * GIB, false),
            Some(64 * GIB)
        );
        assert!(policy
            .next_extension(VolumeKind::Workspace, 64 * GIB, false)
            .is_none());
        assert_eq!(
            policy.next_extension(VolumeKind::Database, 8 * GIB, false),
            Some(16 * GIB)
        );
        assert_eq!(
            policy.next_extension(VolumeKind::Database, 16 * GIB, true),
            Some(32 * GIB)
        );
        assert!(policy
            .next_extension(VolumeKind::Deployment, GIB, false)
            .is_none());
    }

    #[test]
    fn leftover_restore_rows_parse_as_database_restore() {
        assert_eq!(
            VolumeKind::parse("restore"),
            Some(VolumeKind::DatabaseRestore)
        );
        assert_eq!(
            VolumeKind::parse("workspace_restore"),
            Some(VolumeKind::WorkspaceRestore)
        );
        assert_eq!(VolumeKind::WorkspaceRestore.as_str(), "workspace_restore");
        assert!(!VolumeKind::Workspace.is_linear_normal());
        assert!(VolumeKind::Workspace.is_thin());
        assert!(VolumeKind::Database.is_linear_normal());
    }

    #[test]
    fn capacity_json_reports_the_split_layout() {
        let report = CapacityReport {
            device_bytes: 476 * GIB,
            health: "healthy",
            runtime: RuntimeCapacity {
                pool_bytes: 64 * GIB,
                used_bytes: 22 * GIB,
            },
            workspaces: WorkspaceCapacity {
                pool_bytes: 264 * GIB,
                pool_used_bytes: 61 * GIB,
                logical_budget_bytes: 128 * GIB,
                logical_allocated_bytes: 96 * GIB,
                restore_headroom_bytes: 64 * GIB,
                restore_allocated_bytes: 0,
            },
            linear: LinearCapacity {
                budget_bytes: 96 * GIB,
                allocated_bytes: 73 * GIB,
                allocatable_now_bytes: 23 * GIB,
                databases_bytes: 71 * GIB,
                deployments_bytes: 2 * GIB,
            },
            recovery: RecoveryCapacity {
                reserve_bytes: 48 * GIB,
                emergency_floor_bytes: 16 * GIB,
                physical_free_bytes: 137 * GIB,
            },
        };
        let value = serde_json::to_value(&report).expect("capacity json");
        assert_eq!(value["workspaces"]["logicalBudgetBytes"], 128 * GIB);
        assert_eq!(value["workspaces"]["poolBytes"], 264 * GIB);
        assert_eq!(value["linear"]["budgetBytes"], 96 * GIB);
        assert_eq!(value["linear"]["allocatableNowBytes"], 23 * GIB);
        assert_eq!(value["recovery"]["reserveBytes"], 48 * GIB);
        assert!(value.get("normalBudgetBytes").is_none());
        assert!(value.get("allocations").is_none());
    }

    #[test]
    fn health_separates_workspace_pool_linear_and_runtime() {
        assert_eq!(
            capacity_health(
                48 * GIB,
                16 * GIB,
                200 * GIB,
                0,
                8 * GIB,
                Some(1.0),
                0,
                128 * GIB,
                0,
                LINEAR_NORMAL_BUDGET_BYTES,
                0,
                64 * GIB,
            ),
            "healthy"
        );
        assert_eq!(
            capacity_health(
                48 * GIB,
                16 * GIB,
                200 * GIB,
                0,
                8 * GIB,
                Some(1.0),
                103 * GIB,
                128 * GIB,
                0,
                LINEAR_NORMAL_BUDGET_BYTES,
                0,
                64 * GIB,
            ),
            "warning"
        );
        assert_eq!(
            capacity_health(
                48 * GIB,
                16 * GIB,
                200 * GIB,
                0,
                8 * GIB,
                Some(1.0),
                0,
                128 * GIB,
                128 * GIB,
                LINEAR_NORMAL_BUDGET_BYTES,
                0,
                64 * GIB,
            ),
            "warning"
        );
        assert_eq!(
            capacity_health(
                48 * GIB,
                16 * GIB,
                200 * GIB,
                0,
                8 * GIB,
                Some(1.0),
                0,
                128 * GIB,
                0,
                LINEAR_NORMAL_BUDGET_BYTES,
                48 * GIB,
                64 * GIB,
            ),
            "warning"
        );
        assert_eq!(
            capacity_health(
                48 * GIB,
                16 * GIB,
                200 * GIB,
                150 * GIB,
                8 * GIB,
                Some(1.0),
                0,
                128 * GIB,
                0,
                LINEAR_NORMAL_BUDGET_BYTES,
                0,
                64 * GIB,
            ),
            "warning"
        );
        assert_eq!(
            capacity_health(
                15 * GIB,
                16 * GIB,
                200 * GIB,
                0,
                8 * GIB,
                Some(1.0),
                0,
                128 * GIB,
                0,
                LINEAR_NORMAL_BUDGET_BYTES,
                0,
                64 * GIB,
            ),
            "critical"
        );
        assert_eq!(
            capacity_health(
                48 * GIB,
                16 * GIB,
                200 * GIB,
                0,
                8 * GIB,
                Some(90.0),
                0,
                128 * GIB,
                0,
                LINEAR_NORMAL_BUDGET_BYTES,
                0,
                64 * GIB,
            ),
            "critical"
        );
        assert_eq!(
            capacity_health(
                48 * GIB,
                16 * GIB,
                200 * GIB,
                193 * GIB,
                8 * GIB,
                Some(1.0),
                0,
                128 * GIB,
                0,
                LINEAR_NORMAL_BUDGET_BYTES,
                0,
                64 * GIB,
            ),
            "critical"
        );
    }

    #[test]
    fn size_args_are_exact() {
        assert_eq!(lv_size_arg(16 * GIB), "16G");
        assert_eq!(k8s_quantity(16 * GIB), "16Gi");
        assert_eq!(parse_size("16G").unwrap(), 16 * GIB);
        assert_eq!(parse_size("512M").unwrap(), 512 * MIB);
        assert_eq!(parse_size("64Gi").unwrap(), 64 * GIB);
    }

    #[test]
    fn retired_product_thin_pool_is_refused() {
        refuse_legacy_product_pool(&["runtime".into(), "workspace".into()]).unwrap();
        let err = refuse_legacy_product_pool(&[
            "workspaces".into(),
            "ws-root".into(),
            "ws0123456789abcdef0123456789abcdef".into(),
        ])
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("workspaces"), "{message}");
        assert!(message.contains("ws-root"), "{message}");
    }

    #[test]
    fn recovery_reserve_must_not_be_an_lv() {
        refuse_allocated_recovery_reserve(&["runtime".into(), "workspace".into()]).unwrap();
        let err =
            refuse_allocated_recovery_reserve(&["runtime".into(), "reserve".into()]).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("reserve"), "{message}");
        assert!(message.contains("unallocated"), "{message}");
    }

    #[test]
    fn runtime_and_workspace_thin_pools_are_required() {
        require_runtime_pool(&["runtime".into(), "workspace".into()]).unwrap();
        require_workspace_pool(&["runtime".into(), "workspace".into()], "workspace").unwrap();
        let err = require_runtime_pool(&["workspace".into()]).unwrap_err();
        assert!(err.to_string().contains("runtime"), "{err}");
        let err = require_workspace_pool(&["runtime".into()], "workspace").unwrap_err();
        assert!(err.to_string().contains("workspace"), "{err}");
    }

    #[test]
    fn runtime_thin_pool_must_be_sixty_four_gib() {
        require_runtime_pool_size(64 * GIB, RUNTIME_POOL_BYTES).unwrap();
        require_runtime_pool_size(63 * GIB, RUNTIME_POOL_BYTES).unwrap();
        require_runtime_pool_size(2 * GIB, 2 * GIB).unwrap();
        require_runtime_pool_size(0, 0).unwrap();
        let err = require_runtime_pool_size(2 * GIB, RUNTIME_POOL_BYTES).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("runtime"), "{message}");
    }
}
