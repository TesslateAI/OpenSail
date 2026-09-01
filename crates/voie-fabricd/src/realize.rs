//! Local block slot and Kubernetes realization for one Fabric host.
//!
//! Commands run only against LVM, the declared block device, and `kubectl`.
//! User shell text is never executed on this host.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::sleep;

use crate::FabricError;
use crate::fabric::bounded_text;

/// Filesystem label stamped onto every VOIE-formatted workspace device.
const MKFS_LABEL_PREFIX: &str = "voie-ws";
const RETIRED_WORKSPACE_POOL_PV: &str = "voie-ws-pool";

enum BlkidFs {
    Ext4,
    None,
}

/// Formatting is allowed only after a positive "no signature" observation.
/// Empty stdout with any other exit is unknown and must not wipe the device.
fn classify_blkid(status: i32, stdout: &str) -> Result<BlkidFs, FabricError> {
    let fs = stdout.trim();
    match (status, fs) {
        (0, "ext4") => Ok(BlkidFs::Ext4),
        (0, other) if !other.is_empty() => Err(FabricError::Foreign(format!(
            "block device has foreign filesystem `{other}`"
        ))),
        (2, "") => Ok(BlkidFs::None),
        _ => Err(FabricError::Unknown(format!(
            "blkid could not positively determine filesystem (status {status})"
        ))),
    }
}

/// `cryptsetup close` is success only when the mapper is positively gone.
/// Unknown failures must retain the key and LV.
fn classify_cryptsetup_close(status: i32, stderr: &str) -> Result<(), FabricError> {
    if status == 0 {
        return Ok(());
    }
    let text = stderr.to_ascii_lowercase();
    if text.contains("is not active") || text.contains("no such device") {
        return Ok(());
    }
    Err(FabricError::Unknown(format!(
        "cryptsetup close failed (status {status}): {}",
        stderr.trim()
    )))
}

/// The one guest-egress NetworkPolicy this daemon owns for its namespace.
/// A single concrete object by design; there is no policy framework here.
pub const NETWORK_POLICY_NAME: &str = "voie-guest-egress";

/// The one ServiceAccount every workspace pod is admitted under. Dedicated
/// and unprivileged by construction: no Role or binding anywhere grants it
/// anything, and its own manifest disables token automount, so the guest
/// never holds a Kubernetes credential. Naming it explicitly also removes
/// any dependency on a `default` ServiceAccount existing in the workspace
/// namespace — the exact absence that once failed pod admission.
pub const WORKSPACE_SERVICE_ACCOUNT_NAME: &str = "voie-guest";

/// The deployment-approved guest egress destinations, if any: destination
/// CIDRs reachable over one fixed TCP port. This is configuration for the
/// one concrete NetworkPolicy, never a policy language; without it guests
/// can only resolve names through cluster DNS and reach nothing else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovedEgress {
    pub cidrs: Vec<String>,
    pub tcp_port: u16,
}

impl ApprovedEgress {
    /// Parses `VOIE_WORKSPACE_EGRESS_CIDRS` (comma-separated) plus
    /// `VOIE_WORKSPACE_EGRESS_PORT`. Both must be set together or not at
    /// all; every CIDR must be a strict `address/prefix` pair so a typo
    /// fails startup instead of silently widening guest reach.
    pub fn parse(cidrs: Option<String>, port: Option<String>) -> Result<Option<Self>, FabricError> {
        let Some(raw_cidrs) = cidrs else {
            if port.is_some() {
                return Err(FabricError::Config(
                    "VOIE_WORKSPACE_EGRESS_PORT is set without VOIE_WORKSPACE_EGRESS_CIDRS",
                ));
            }
            return Ok(None);
        };
        let Some(raw_port) = port else {
            return Err(FabricError::Config(
                "VOIE_WORKSPACE_EGRESS_CIDRS is set without VOIE_WORKSPACE_EGRESS_PORT",
            ));
        };
        let mut parsed = Vec::new();
        for candidate in raw_cidrs.split(',') {
            let candidate = candidate.trim();
            validate_cidr(candidate)?;
            parsed.push(candidate.to_owned());
        }
        if parsed.is_empty() {
            return Err(FabricError::Config("VOIE_WORKSPACE_EGRESS_CIDRS is empty"));
        }
        let tcp_port = raw_port
            .trim()
            .parse::<u16>()
            .map_err(|_| FabricError::Config("VOIE_WORKSPACE_EGRESS_PORT is not a TCP port"))?;
        if tcp_port == 0 {
            return Err(FabricError::Config("VOIE_WORKSPACE_EGRESS_PORT is zero"));
        }
        Ok(Some(ApprovedEgress {
            cidrs: parsed,
            tcp_port,
        }))
    }
}

/// Validates one strict `address/prefix` CIDR block. Bare addresses are
/// refused because an unbounded host route is exactly what this setting
/// must never grant by accident.
fn validate_cidr(value: &str) -> Result<(), FabricError> {
    let Some((address, prefix)) = value.split_once('/') else {
        return Err(FabricError::Config(
            "approved egress entry is not address/prefix CIDR",
        ));
    };
    let max = match address.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(_)) => 32,
        Ok(std::net::IpAddr::V6(_)) => 128,
        Err(_) => {
            return Err(FabricError::Config(
                "approved egress entry has an unusable address",
            ));
        }
    };
    match prefix.parse::<u8>() {
        Ok(bits) if (bits as u32) <= max => Ok(()),
        _ => Err(FabricError::Config(
            "approved egress entry has an unusable prefix length",
        )),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct BlockSlot {
    pub device: String,
    pub lv_name: Option<String>,
    pub mapper_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PodInfo {
    pub name: String,
    pub uid: String,
    pub sandbox_id: Option<String>,
    pub runtime_class: String,
    pub phase: String,
    pub ready: bool,
    pub image: String,
    pub host_network: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PvInfo {
    pub name: String,
    pub path: String,
    pub node: String,
    pub volume_mode: String,
    pub access_modes: Vec<String>,
    pub reclaim: String,
    pub storage_class: String,
    pub workspace_label: Option<String>,
    pub managed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub ambiguous: bool,
}

/// How a completed `kubectl exec` attempt maps onto the exec journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecVerdict {
    /// The guest program's own exit status was observed.
    Terminal(i32),
    /// The attempt cannot be attributed to the program: the runner killed
    /// its own group at the deadline (124), failed to start or wait for the
    /// program (125), or kubectl reported a transport failure. The outcome
    /// is unknown and must never be replayed under the same call id.
    Unknown,
}

/// Classify one finished `kubectl exec` invocation.
///
/// Exit statuses 124 and 125 belong to the runner itself, never to the
/// program, so they can never be recorded as the program's terminal status.
/// Transport-marker matches are checked against stderr, which also carries
/// the guest's own stream; a false positive only errs toward unknown, which
/// is the safe direction for no-replay.
pub fn classify_exec(exit_code: i32, stderr: &str) -> ExecVerdict {
    const TRANSPORT_MARKERS: [&str; 5] = [
        "unable to upgrade connection",
        "error upgrading connection",
        "lost connection to pod",
        "does not exist",
        "connection refused",
    ];
    // kubectl capitalizes its errors ("Unable to upgrade connection"), while
    // the guest stream is arbitrary; match case-insensitively.
    let stderr_lower = stderr.to_ascii_lowercase();
    if TRANSPORT_MARKERS
        .iter()
        .any(|marker| stderr_lower.contains(marker))
    {
        return ExecVerdict::Unknown;
    }
    match exit_code {
        // The runner constants are u8 exit statuses; kubectl surfaces them as
        // the process exit code, which is i32.
        code if code == i32::from(voie_runner::EXIT_TIMED_OUT)
            || code == i32::from(voie_runner::EXIT_RUN_FAILED) =>
        {
            ExecVerdict::Unknown
        }
        code => ExecVerdict::Terminal(code),
    }
}

/// Hex SHA-256 over the canonical JSON of a spec; the durable comparison
/// digest between desired and observed NetworkPolicy specs.
pub fn spec_sha(spec: &Value) -> String {
    let canonical =
        serde_json::to_string(spec).expect("spec JSON serialization cannot fail for Value");
    hex(&Sha256::digest(canonical.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Residue {
    pub pod_present: bool,
    pub jail_present: bool,
    pub vmm_present: bool,
    pub children_present: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VgObservation {
    pub device_bytes: u64,
    pub physical_free_bytes: u64,
    pub runtime_pool_bytes: u64,
    pub runtime_pool_used_bytes: u64,
    pub workspace_pool_bytes: u64,
    pub workspace_pool_used_bytes: u64,
    pub workspace_pool_metadata_percent: Option<i32>,
}

impl Residue {
    pub fn runtime_clean(&self) -> bool {
        !self.pod_present && !self.jail_present && !self.vmm_present && !self.children_present
    }
}

pub struct Live {
    node_name: String,
    namespace: String,
    storage_class: String,
    runtime_class: String,
    runtime_handler: String,
    runner_image: String,
    /// Profile 1 development guest (`voie-workspace:v1` when configured).
    /// Falls back to `runner_image` so Profile 0 C1/C2 keep `voie-runner:c1`.
    workspace_image: String,
    jailer_root: PathBuf,
    vg: String,
    storage: crate::StoragePolicy,
    residue_wait_secs: u64,
    runtime_class_wait_secs: u64,
    approved_egress: Option<ApprovedEgress>,
    kubectl_program: String,
    kubectl_prefix: Vec<String>,
    kubeconfig: Option<PathBuf>,
    crictl_program: String,
    crictl_prefix: Vec<String>,
    volume_key_dir: PathBuf,
}

impl Live {
    pub fn from_config(config: &crate::Config) -> Result<Self, FabricError> {
        Ok(Live {
            node_name: config.node_name.clone(),
            namespace: config.namespace.clone(),
            storage_class: config.storage_class.clone(),
            runtime_class: config.runtime_class.clone(),
            runtime_handler: config.runtime_handler.clone(),
            runner_image: config.runner_image.clone(),
            workspace_image: std::env::var("VOIE_WORKSPACE_IMAGE")
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| config.runner_image.clone()),
            jailer_root: config.jailer_root.clone(),
            vg: config.vg.clone(),
            storage: config.storage.clone(),
            residue_wait_secs: config.residue_wait_secs,
            runtime_class_wait_secs: config.runtime_class_wait_secs,
            approved_egress: config.approved_egress.clone(),
            kubectl_program: config.kubectl_program.clone(),
            kubectl_prefix: config.kubectl_prefix.clone(),
            kubeconfig: config.kubeconfig.clone(),
            crictl_program: config.crictl_program.clone(),
            crictl_prefix: config.crictl_prefix.clone(),
            volume_key_dir: config
                .sqlite
                .parent()
                .unwrap_or(Path::new("/var/lib/voie/fabric"))
                .join("volume-keys"),
        })
    }

    pub fn node_name(&self) -> &str {
        &self.node_name
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn storage_class(&self) -> &str {
        &self.storage_class
    }

    pub fn runtime_class(&self) -> &str {
        &self.runtime_class
    }

    pub fn runtime_handler(&self) -> &str {
        &self.runtime_handler
    }

    pub fn runner_image(&self) -> &str {
        &self.runner_image
    }

    pub fn workspace_image(&self) -> &str {
        &self.workspace_image
    }

    /// Deployment-approved CIDRs used by the Workspace guest policy and by
    /// the Application egress-proxy Pod. Application Pods themselves never
    /// receive these CIDRs.
    pub fn approved_egress(&self) -> Option<&ApprovedEgress> {
        self.approved_egress.as_ref()
    }

    pub fn jailer_root(&self) -> &Path {
        &self.jailer_root
    }

    pub fn vg_name(&self) -> &str {
        &self.vg
    }

    pub fn storage(&self) -> &crate::StoragePolicy {
        &self.storage
    }

    pub fn residue_wait(&self) -> Duration {
        Duration::from_secs(self.residue_wait_secs)
    }

    pub fn runtime_class_wait(&self) -> Duration {
        Duration::from_secs(self.runtime_class_wait_secs)
    }

    async fn kubectl(&self, args: &[&str]) -> Result<CmdOut, FabricError> {
        let mut command = Command::new(&self.kubectl_program);
        command.args(&self.kubectl_prefix);
        if let Some(kubeconfig) = &self.kubeconfig {
            command.arg("--kubeconfig").arg(kubeconfig);
        }
        command.args(args);
        run(command).await
    }

    pub(crate) async fn crictl(&self, args: &[&str]) -> Result<CmdOut, FabricError> {
        let mut command = Command::new(&self.crictl_program);
        command.args(&self.crictl_prefix);
        command.args(args);
        run(command).await
    }

    async fn host(&self, program: &str, args: &[&str]) -> Result<CmdOut, FabricError> {
        let mut command = Command::new(program);
        command.args(args);
        run(command).await
    }

    pub async fn canonical_device(&self, path: &str) -> Result<String, FabricError> {
        let out = self.host("readlink", &["-f", path]).await?;
        if out.status != 0 {
            return Err(FabricError::Realize(format!(
                "block device `{path}` is not resolvable"
            )));
        }
        let resolved = out.stdout.trim().to_owned();
        if resolved.is_empty() {
            return Err(FabricError::Realize(format!(
                "block device `{path}` resolved empty"
            )));
        }
        Ok(resolved)
    }

    /// Carves this workspace's logical volume from the Workspace thin pool.
    /// The virtual size is the platform tier; physical blocks are consumed
    /// only as the guest writes. There is no file- or loop-backed escape.
    pub async fn prepare_block(
        &self,
        workspace_id: &str,
        bytes: u64,
    ) -> Result<BlockSlot, FabricError> {
        let lv_name = lv_name_for(workspace_id);
        self.prepare_encrypted_lv(&lv_name, &crate::lv_size_arg(bytes), true)
            .await
    }

    /// Carves one dedicated Database LV. Firecracker attaches it as
    /// `/dev/pgdata`; a host directory is not a guest drive.
    pub async fn prepare_postgres_block(
        &self,
        database_id: &str,
        bytes: u64,
    ) -> Result<BlockSlot, FabricError> {
        self.prepare_named_block(lv_name_for_postgres(database_id), bytes)
            .await
    }

    /// Carves one Deployment's private copy of a Release. Two Environments
    /// cannot share a RWO Deployment drive; each candidate gets its own LV.
    pub async fn prepare_deployment_block(
        &self,
        deployment_id: &str,
    ) -> Result<BlockSlot, FabricError> {
        self.prepare_named_block(
            lv_name_for_deployment(deployment_id),
            self.storage.deployment_bytes,
        )
        .await
    }

    pub async fn prepare_restore_block(
        &self,
        operation_id: &str,
        bytes: u64,
    ) -> Result<BlockSlot, FabricError> {
        self.prepare_named_block(lv_name_for_restore(operation_id), bytes)
            .await
    }

    pub async fn prepare_named_block(
        &self,
        lv_name: String,
        bytes: u64,
    ) -> Result<BlockSlot, FabricError> {
        self.prepare_encrypted_lv(&lv_name, &crate::lv_size_arg(bytes), false)
            .await
    }

    pub async fn prepare_thin_named_block(
        &self,
        lv_name: String,
        bytes: u64,
    ) -> Result<BlockSlot, FabricError> {
        self.prepare_encrypted_lv(&lv_name, &crate::lv_size_arg(bytes), true)
            .await
    }

    async fn prepare_encrypted_lv(
        &self,
        lv_name: &str,
        size: &str,
        thin: bool,
    ) -> Result<BlockSlot, FabricError> {
        let mapper = format!("/dev/{}/{}", self.vg, lv_name);
        let exists = self
            .host("lvs", &[&format!("{}/{}", self.vg, lv_name)])
            .await?;
        if exists.status != 0 {
            let created = if thin {
                let pool = format!("{}/{}", self.vg, self.storage.workspace_pool);
                self.host(
                    "lvcreate",
                    &[
                        "-y",
                        "--virtualsize",
                        size,
                        "--thinpool",
                        &pool,
                        "--name",
                        lv_name,
                    ],
                )
                .await?
            } else {
                self.host("lvcreate", &["-y", "-L", size, "-n", lv_name, &self.vg])
                    .await?
            };
            if created.status != 0 {
                return Err(FabricError::Realize(format!(
                    "lvcreate failed: {}",
                    created.stderr.trim()
                )));
            }
            self.create_volume_key(lv_name)?;
        } else {
            self.require_volume_key(lv_name)?;
        }
        // Empty auto_activation_volume_list leaves existing product LVs
        // inactive across reboot. Boot activates only runtime/workspace/stage;
        // this daemon owns product LV activation.
        self.activate_lv(lv_name).await?;
        let device = self.canonical_device(&mapper).await?;
        if !Path::new(&device).exists() {
            return Err(FabricError::Realize(format!(
                "reserved logical volume `{device}` is absent"
            )));
        }
        self.wrap_encrypted(&device, lv_name).await
    }

    /// Manual activation. Initrd and udev leave `voie-ws` product LVs
    /// inactive so leftover pools cannot hang stage-1. `--activate y`
    /// rather than `ay`: `ay` honors the empty auto-activation list.
    /// Do not pass `--noudevsync`: thin-pool tmeta nodes come from udev.
    pub async fn activate_lv(&self, lv_name: &str) -> Result<(), FabricError> {
        let spec = format!("{}/{}", self.vg, lv_name);
        let out = self.host("lvchange", &["--activate", "y", &spec]).await?;
        if out.status != 0 {
            return Err(FabricError::Realize(format!(
                "lvchange --activate y {spec} failed: {}",
                out.stderr.trim()
            )));
        }
        Ok(())
    }

    /// Extends a thin Workspace LV's virtual size, then resizes the
    /// dm-crypt plain mapping so the guest sees the new device length.
    pub async fn extend_thin_lv(&self, lv_name: &str, bytes: u64) -> Result<(), FabricError> {
        let spec = format!("{}/{}", self.vg, lv_name);
        let size = crate::lv_size_arg(bytes);
        let extended = self.host("lvextend", &["-L", &size, &spec]).await?;
        if extended.status != 0
            && !extended.stderr.contains("matches existing size")
            && !extended.stderr.contains("already")
        {
            return Err(FabricError::Realize(format!(
                "lvextend {spec} failed: {}",
                extended.stderr.trim()
            )));
        }
        let mapper = Self::mapper_name_for(lv_name);
        let resized = self.host("cryptsetup", &["resize", &mapper]).await?;
        if resized.status != 0 {
            return Err(FabricError::Realize(format!(
                "cryptsetup resize {mapper} failed: {}",
                resized.stderr.trim()
            )));
        }
        Ok(())
    }

    fn mapper_name_for(lv_name: &str) -> String {
        format!("voie-crypt-{lv_name}")
    }

    pub fn encrypted_mapper_path(&self, lv_name: &str) -> String {
        encrypted_mapper_device(lv_name)
    }

    pub fn has_volume_key(&self, lv_name: &str) -> bool {
        self.volume_key_path(lv_name).exists()
    }

    /// Re-activate a claimed LV and reopen its crypt mapping after reboot.
    /// The key record is required; a missing key is not minted.
    pub async fn reopen_encrypted_lv(&self, lv_name: &str) -> Result<BlockSlot, FabricError> {
        self.require_volume_key(lv_name)?;
        self.activate_lv(lv_name).await?;
        let lv_path = format!("/dev/{}/{}", self.vg, lv_name);
        let backend = if Path::new(&lv_path).exists() {
            lv_path
        } else {
            self.canonical_device(&format!("/dev/{}/{}", self.vg, lv_name))
                .await
                .unwrap_or(lv_path)
        };
        self.wrap_encrypted(&backend, lv_name).await
    }

    pub async fn apply_json(&self, mut value: Value) -> Result<(), FabricError> {
        strip_runtime_metadata(&mut value);
        self.apply_yaml(&serde_json::to_string(&value).map_err(|error| {
            FabricError::Realize(format!("cannot serialize cluster object: {error}"))
        })?)
        .await
    }

    /// Recreate a Bound local PV whose path is a recycled `/dev/dm-N` (or
    /// any other path that is not the stable encrypted mapper). Kubernetes
    /// treats `spec.local.path` as immutable, so the PV and its PVC are
    /// deleted and re-applied. Callers must delete attaching pods first.
    pub async fn replace_local_pv_device(
        &self,
        pv_name: &str,
        pvc_name: &str,
        expected_device: &str,
    ) -> Result<bool, FabricError> {
        require_stable_block_path(expected_device)?;
        let Some(mut pv_json) = self.get_unnamespaced("pv", pv_name).await? else {
            return Ok(false);
        };
        let current = pv_json
            .pointer("/spec/local/path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if !ephemeral_devmapper_path(&current) && current == expected_device {
            return Ok(false);
        }
        if !ephemeral_devmapper_path(&current) {
            let same_inode = std::fs::canonicalize(&current)
                .ok()
                .zip(std::fs::canonicalize(expected_device).ok())
                .is_some_and(|(left, right)| left == right);
            if same_inode {
                return Ok(false);
            }
        }
        let pvc_json = self.get_namespaced("pvc", pvc_name).await?;
        if let Some(spec) = pv_json.get_mut("spec").and_then(Value::as_object_mut) {
            if let Some(local) = spec.get_mut("local").and_then(Value::as_object_mut) {
                local.insert("path".into(), Value::String(expected_device.to_owned()));
            }
            spec.remove("claimRef");
        }
        strip_runtime_metadata(&mut pv_json);
        self.delete_named("pvc", pvc_name, true, 30).await?;
        self.delete_named("pv", pv_name, false, 30).await?;
        self.apply_yaml(&serde_json::to_string(&pv_json).map_err(|error| {
            FabricError::Realize(format!("cannot serialize retargeted PV: {error}"))
        })?)
        .await?;
        if let Some(mut pvc_json) = pvc_json {
            strip_runtime_metadata(&mut pvc_json);
            self.apply_yaml(&serde_json::to_string(&pvc_json).map_err(|error| {
                FabricError::Realize(format!("cannot serialize retargeted PVC: {error}"))
            })?)
            .await?;
        }
        Ok(true)
    }

    /// Create the local PV/PVC when they are absent after reboot, or replace
    /// a recycled `/dev/dm-N` path. A missing PV is not "already correct".
    pub async fn ensure_local_pv_device(
        &self,
        pv_name: &str,
        pvc_name: &str,
        expected_device: &str,
        pv_yaml: &str,
        pvc_yaml: &str,
    ) -> Result<bool, FabricError> {
        require_stable_block_path(expected_device)?;
        if self.get_unnamespaced("pv", pv_name).await?.is_none() {
            self.apply_yaml(pv_yaml).await?;
            self.apply_yaml(pvc_yaml).await?;
            return Ok(true);
        }
        let replaced = self
            .replace_local_pv_device(pv_name, pvc_name, expected_device)
            .await?;
        if replaced {
            return Ok(true);
        }
        if self.get_namespaced("pvc", pvc_name).await?.is_none() {
            self.apply_yaml(pvc_yaml).await?;
            return Ok(true);
        }
        Ok(false)
    }

    pub async fn get_unnamespaced(
        &self,
        kind: &str,
        name: &str,
    ) -> Result<Option<Value>, FabricError> {
        let out = self.kubectl(&["get", kind, name, "-o", "json"]).await?;
        if is_not_found(&out) {
            return Ok(None);
        }
        if out.status != 0 {
            return Err(FabricError::Realize(out.stderr));
        }
        serde_json::from_str(&out.stdout)
            .map(Some)
            .map_err(|_| FabricError::Realize(format!("{kind} JSON was unusable")))
    }

    fn volume_key_path(&self, lv_name: &str) -> PathBuf {
        self.volume_key_dir.join(lv_name)
    }

    fn create_volume_key(&self, lv_name: &str) -> Result<PathBuf, FabricError> {
        std::fs::create_dir_all(&self.volume_key_dir).map_err(|error| {
            FabricError::Realize(format!(
                "cannot create volume key directory {}: {error}",
                self.volume_key_dir.display()
            ))
        })?;
        let path = self.volume_key_path(lv_name);
        if path.exists() {
            return Ok(path);
        }
        let mut key = [0u8; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut key))
            .map_err(|error| {
                FabricError::Realize(format!("cannot read volume key material: {error}"))
            })?;
        std::fs::write(&path, key).map_err(|error| {
            FabricError::Realize(format!(
                "cannot write volume key {}: {error}",
                path.display()
            ))
        })?;
        let mut perms = std::fs::metadata(&path)
            .map_err(|error| {
                FabricError::Realize(format!(
                    "cannot stat volume key {}: {error}",
                    path.display()
                ))
            })?
            .permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&path, perms).map_err(|error| {
            FabricError::Realize(format!(
                "cannot restrict volume key {}: {error}",
                path.display()
            ))
        })?;
        Ok(path)
    }

    fn require_volume_key(&self, lv_name: &str) -> Result<PathBuf, FabricError> {
        let path = self.volume_key_path(lv_name);
        if !path.exists() {
            return Err(FabricError::Foreign(format!(
                "logical volume `{lv_name}` has no matching key record"
            )));
        }
        Ok(path)
    }

    async fn wrap_encrypted(&self, lv_path: &str, lv_name: &str) -> Result<BlockSlot, FabricError> {
        let key_path = self.require_volume_key(lv_name)?;
        let mapper = Self::mapper_name_for(lv_name);
        let key_file = key_path.to_str().ok_or_else(|| {
            FabricError::Realize(format!(
                "volume key path {} is not utf-8",
                key_path.display()
            ))
        })?;
        let opened = self
            .host(
                "cryptsetup",
                &[
                    "open",
                    "--type",
                    "plain",
                    "--cipher",
                    "aes-xts-plain64",
                    "--key-size",
                    "256",
                    "--key-file",
                    key_file,
                    lv_path,
                    &mapper,
                ],
            )
            .await?;
        if opened.status != 0
            && !opened.stderr.contains("Device already exists")
            && !opened.stderr.contains("already exists")
        {
            return Err(FabricError::Realize(format!(
                "cryptsetup open failed: {}",
                opened.stderr.trim()
            )));
        }
        let mapper_dev = format!("/dev/mapper/{mapper}");
        if !Path::new(&mapper_dev).exists() {
            return Err(FabricError::Realize(format!(
                "encrypted mapper `{mapper}` is absent"
            )));
        }
        // Reservations and PVs must use this stable mapper path. Canonical
        // `/dev/dm-N` is recycled across reboot and will collide with a
        // live workspace's stale reservation.
        Ok(BlockSlot {
            device: mapper_dev,
            lv_name: Some(lv_name.to_owned()),
            mapper_name: Some(mapper),
        })
    }

    async fn close_encrypted(&self, lv_name: &str) -> Result<(), FabricError> {
        let mapper = Self::mapper_name_for(lv_name);
        let mapper_dev = format!("/dev/mapper/{mapper}");
        let closed = self.host("cryptsetup", &["close", &mapper]).await?;
        classify_cryptsetup_close(closed.status, &closed.stderr)?;
        if Path::new(&mapper_dev).exists() {
            return Err(FabricError::Unknown(format!(
                "encrypted mapper `{mapper}` is still present"
            )));
        }
        let key_path = self.volume_key_path(lv_name);
        if key_path.exists() {
            let _ = std::fs::write(&key_path, [0u8; 32]);
            std::fs::remove_file(&key_path).map_err(|error| {
                FabricError::Realize(format!(
                    "cannot destroy volume key {}: {error}",
                    key_path.display()
                ))
            })?;
        }
        Ok(())
    }

    pub async fn mount_ext4(&self, device: &str, target: &str) -> Result<(), FabricError> {
        std::fs::create_dir_all(target)
            .map_err(|error| FabricError::Realize(format!("cannot create mount point: {error}")))?;
        let mounted = self.host("mount", &["-t", "ext4", device, target]).await?;
        if mounted.status != 0 {
            return Err(FabricError::Realize(format!(
                "mount ext4 failed: {}",
                mounted.stderr.trim()
            )));
        }
        Ok(())
    }

    pub async fn unmount(&self, target: &str) -> Result<(), FabricError> {
        let out = self.host("umount", &[target]).await?;
        if out.status != 0 && !out.stderr.contains("not mounted") {
            return Err(FabricError::Realize(format!(
                "umount failed: {}",
                out.stderr.trim()
            )));
        }
        Ok(())
    }

    pub async fn mkfs_ext4_if_needed(&self, device: &str) -> Result<(), FabricError> {
        let probed = self
            .host("blkid", &["-o", "value", "-s", "TYPE", device])
            .await?;
        match classify_blkid(probed.status, &probed.stdout) {
            Ok(BlkidFs::Ext4) => {
                let labeled = self
                    .host("blkid", &["-o", "value", "-s", "LABEL", device])
                    .await?;
                if labeled.status != 0 {
                    return Err(FabricError::Unknown(format!(
                        "blkid could not positively determine label on {device}"
                    )));
                }
                let label = labeled.stdout.trim();
                // An ext4 filesystem that carries some other identity belongs to
                // someone else; VOIE never reformats foreign bytes.
                if !label.is_empty() && !label.starts_with("voie-") {
                    return Err(FabricError::Foreign(format!(
                        "block device `{device}` carries foreign ext4 label `{label}`"
                    )));
                }
                return Ok(());
            }
            Ok(BlkidFs::None) => {}
            Err(error) => return Err(error),
        }
        let mkfs = self
            .host("mkfs.ext4", &["-F", "-q", "-L", MKFS_LABEL_PREFIX, device])
            .await?;
        if mkfs.status != 0 {
            return Err(FabricError::Realize(format!(
                "mkfs.ext4 failed: {}",
                mkfs.stderr.trim()
            )));
        }
        Ok(())
    }

    pub async fn device_mounted(&self, device: &str) -> Result<bool, FabricError> {
        let out = self.host("findmnt", &["-n", "-S", device]).await?;
        match (out.status, out.stdout.trim().is_empty()) {
            // `findmnt` exits zero and prints a row when the source is
            // mounted. An empty successful response is not an absence
            // verdict: it means the observer did not return its contract.
            (0, false) => Ok(true),
            // Exit status one is findmnt's positive "no match" result.
            (1, true) => Ok(false),
            (_, _) => Err(FabricError::Unknown(format!(
                "findmnt could not positively determine whether {device} is mounted"
            ))),
        }
    }

    pub async fn ensure_namespace(&self) -> Result<(), FabricError> {
        self.apply_yaml(&format!(
            "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: {}\n  labels:\n    io.voie/managed: \"true\"\n",
            self.namespace
        ))
        .await
    }

    pub async fn ensure_storage_class(&self) -> Result<(), FabricError> {
        if let Some(existing) = self.get_storage_class().await? {
            if existing != "kubernetes.io/no-provisioner" {
                return Err(FabricError::Foreign(format!(
                    "StorageClass {} has foreign provisioner {existing}",
                    self.storage_class
                )));
            }
            self.refuse_allocating_storage_classes().await?;
            self.refuse_retired_workspace_pool_pv().await?;
            return Ok(());
        }
        self.apply_yaml(&format!(
            "apiVersion: storage.k8s.io/v1
kind: StorageClass
metadata:
  name: {}
  labels:
    io.voie/managed: \"true\"
provisioner: kubernetes.io/no-provisioner
reclaimPolicy: Retain
volumeBindingMode: Immediate
allowVolumeExpansion: false
",
            self.storage_class
        ))
        .await?;
        self.refuse_allocating_storage_classes().await?;
        self.refuse_retired_workspace_pool_pv().await
    }

    /// Kubernetes must not allocate product bytes. Any StorageClass with a
    /// real provisioner (k3s local-path) is a competing allocator, even when
    /// it is not the cluster default.
    pub async fn refuse_allocating_storage_classes(&self) -> Result<(), FabricError> {
        let out = self.kubectl(&["get", "storageclass", "-o", "json"]).await?;
        if is_not_found(&out) {
            return Ok(());
        }
        if out.status != 0 {
            return Err(FabricError::Realize(out.stderr));
        }
        let value: Value = serde_json::from_str(&out.stdout)
            .map_err(|_| FabricError::Realize("StorageClass list JSON was unusable".into()))?;
        let competing = allocating_storage_classes(&value);
        if competing.is_empty() {
            return Ok(());
        }
        let names = competing
            .iter()
            .map(|(name, provisioner)| format!("{name} ({provisioner})"))
            .collect::<Vec<_>>()
            .join(", ");
        Err(FabricError::Foreign(format!(
            "StorageClass {names} allocates bytes; product volumes are linear LVs with no-provisioner"
        )))
    }

    /// Retired Ansible 200Gi filesystem PV. Product volumes are exact
    /// linear LVs; this PV must not remain as a competing capacity claim.
    pub async fn refuse_retired_workspace_pool_pv(&self) -> Result<(), FabricError> {
        let out = self
            .kubectl(&["get", "pv", RETIRED_WORKSPACE_POOL_PV, "-o", "json"])
            .await?;
        if is_not_found(&out) {
            return Ok(());
        }
        if out.status != 0 {
            return Err(FabricError::Realize(out.stderr));
        }
        let value: Value = serde_json::from_str(&out.stdout)
            .map_err(|_| FabricError::Realize("PV JSON was unusable".into()))?;
        let name = value
            .get("metadata")
            .and_then(|metadata| metadata.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if name != RETIRED_WORKSPACE_POOL_PV {
            return Ok(());
        }
        Err(FabricError::Foreign(
            "persistent volume voie-ws-pool is the retired 200Gi workspace pool; product volumes are exact linear LVs".into(),
        ))
    }

    async fn get_storage_class(&self) -> Result<Option<String>, FabricError> {
        let out = self
            .kubectl(&["get", "storageclass", &self.storage_class, "-o", "json"])
            .await?;
        if is_not_found(&out) {
            return Ok(None);
        }
        if out.status != 0 {
            return Err(FabricError::Realize(out.stderr));
        }
        let value: Value = serde_json::from_str(&out.stdout)
            .map_err(|_| FabricError::Realize("StorageClass JSON was unusable".into()))?;
        Ok(value
            .get("provisioner")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned))
    }

    /// Observes the configured RuntimeClass once. Cluster-scoped by
    /// definition, so no namespace flag. The read carries a kubectl
    /// `--request-timeout` so one hung API-server round trip can never
    /// outlive the caller's readiness budget; a timed-out or otherwise
    /// failed read comes back as [`RuntimeClassObservation::Unreadable`]
    /// rather than as an error, leaving the wait loop in charge of the
    /// bound and of the truthful final verdict.
    async fn observe_runtime_class(&self, request_timeout: Duration) -> RuntimeClassObservation {
        let timeout_arg = format!("{}s", request_timeout.as_secs().max(1));
        let out = match self
            .kubectl(&[
                "get",
                "runtimeclass",
                &self.runtime_class,
                "-o",
                "json",
                "--request-timeout",
                &timeout_arg,
            ])
            .await
        {
            Ok(out) => out,
            Err(error) => {
                return RuntimeClassObservation::Unreadable(bounded_text(&error.to_string()));
            }
        };
        if is_not_found(&out) {
            return RuntimeClassObservation::Absent;
        }
        if out.status != 0 {
            return RuntimeClassObservation::Unreadable(bounded_text(out.stderr.trim()));
        }
        match serde_json::from_str::<Value>(&out.stdout) {
            Ok(value) => RuntimeClassObservation::Present {
                handler: value
                    .pointer("/handler")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            },
            Err(_) => RuntimeClassObservation::Unreadable("RuntimeClass JSON was unusable".into()),
        }
    }

    /// Positive admission precondition for every workspace pod: the estate
    /// RuntimeClass must exist and select the configured CRI handler before
    /// any pod naming it may be applied.
    ///
    /// The RuntimeClass is deployment state applied by the host profile's
    /// k3s auto-deploy loop, never by this daemon: fabricd can finish its
    /// own mTLS bootstrap long before the cluster has converged the
    /// manifest, and a pod applied inside that window is rejected by
    /// admission outright. This gate therefore positively observes
    /// convergence instead of racing it, bounded because a manifest that
    /// never arrives is a deployment fault no retry here can repair.
    /// Absence past the bound fails Unknown; presence with a different
    /// handler fails Foreign immediately, since waiting cannot turn
    /// deployment state this daemon does not own into what admission
    /// needs. There is deliberately no fallback: the pod manifest keeps
    /// naming exactly this class.
    ///
    /// Every observation is bounded twice: the loop by the overall
    /// readiness bound, and each kubectl read individually by whatever
    /// remains of that bound (never less than one second), so even a hung
    /// API-server connection resolves to the truthful Unknown outcome
    /// instead of stretching realization past its stated limit. A failed
    /// read is not an answer: connection refusals, request timeouts, and
    /// unusable responses count as absence for the remainder of the bound
    /// — the same convergence window this gate exists for — while the last
    /// failure's bounded reason is preserved in the final Unknown so a
    /// genuinely broken API surface is diagnosed, not masked as mere
    /// lateness.
    pub async fn wait_runtime_class_ready(&self, timeout: Duration) -> Result<(), FabricError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut last_read_failure: Option<String> = None;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let observed = self
                .observe_runtime_class(remaining.max(Duration::from_secs(1)))
                .await;
            if classify_runtime_class(&self.runtime_class, &self.runtime_handler, &observed)? {
                return Ok(());
            }
            if let RuntimeClassObservation::Unreadable(reason) = &observed {
                last_read_failure = Some(reason.clone());
            }
            if tokio::time::Instant::now() >= deadline {
                let reason = last_read_failure
                    .map(|reason| format!("; last read failure: {reason}"))
                    .unwrap_or_default();
                return Err(FabricError::Unknown(format!(
                    "RuntimeClass {} did not appear with handler {} within the readiness bound{reason}",
                    self.runtime_class, self.runtime_handler
                )));
            }
            sleep(Duration::from_secs(2)).await;
        }
    }

    /// Renders the workspace ServiceAccount. The account is estate-owned
    /// and permissionless: nothing binds a role to it, and token automount
    /// is disabled on the account itself so even a hypothetical pod that
    /// forgot its own suppression would never see a credential.
    pub fn service_account_yaml(&self) -> String {
        format!(
            "apiVersion: v1
kind: ServiceAccount
metadata:
  name: {name}
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
automountServiceAccountToken: false
",
            name = WORKSPACE_SERVICE_ACCOUNT_NAME,
            ns = self.namespace,
        )
    }

    /// Applies the workspace ServiceAccount and then positively reads it
    /// back from the API server. Pod admission names this account, so a pod
    /// create may only proceed once the account verifiably exists — there
    /// is no fallback to a `default` ServiceAccount and none is required.
    pub async fn ensure_workspace_service_account(&self) -> Result<(), FabricError> {
        self.apply_yaml(&self.service_account_yaml()).await?;
        if self
            .get_namespaced("serviceaccount", WORKSPACE_SERVICE_ACCOUNT_NAME)
            .await?
            .is_none()
        {
            return Err(FabricError::Unknown(format!(
                "workspace service account {} missing from namespace {} after apply",
                WORKSPACE_SERVICE_ACCOUNT_NAME, self.namespace
            )));
        }
        Ok(())
    }

    pub async fn apply_yaml(&self, yaml: &str) -> Result<(), FabricError> {
        let mut command = Command::new(&self.kubectl_program);
        command.args(&self.kubectl_prefix);
        if let Some(kubeconfig) = &self.kubeconfig {
            command.arg("--kubeconfig").arg(kubeconfig);
        }
        command.args(["apply", "-f", "-"]);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| FabricError::Realize(format!("spawn kubectl: {error}")))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(yaml.as_bytes())
                .await
                .map_err(|error| FabricError::Realize(format!("write kubectl apply: {error}")))?;
        }
        let output = child
            .wait_with_output()
            .await
            .map_err(|error| FabricError::Realize(format!("kubectl apply: {error}")))?;
        if !output.status.success() {
            return Err(FabricError::Realize(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        Ok(())
    }

    pub async fn get_pv(&self, name: &str) -> Result<Option<PvInfo>, FabricError> {
        let out = self.kubectl(&["get", "pv", name, "-o", "json"]).await?;
        if is_not_found(&out) {
            return Ok(None);
        }
        if out.status != 0 {
            return Err(FabricError::Realize(out.stderr));
        }
        let value: Value = serde_json::from_str(&out.stdout)
            .map_err(|_| FabricError::Realize("PV JSON was unusable".into()))?;
        Ok(Some(parse_pv(name, &value)))
    }

    pub async fn get_namespaced(
        &self,
        kind: &str,
        name: &str,
    ) -> Result<Option<Value>, FabricError> {
        let out = self
            .kubectl(&["get", kind, name, "-n", &self.namespace, "-o", "json"])
            .await?;
        if is_not_found(&out) {
            return Ok(None);
        }
        if out.status != 0 {
            return Err(FabricError::Realize(out.stderr));
        }
        serde_json::from_str(&out.stdout)
            .map(Some)
            .map_err(|_| FabricError::Realize(format!("{kind} JSON was unusable")))
    }

    /// Stable Database Service endpoints after a generation selector update.
    /// Empty means a zero-endpoint cutover window; more than one name is
    /// split-brain.
    pub fn endpoint_pod_names(value: &Value) -> Vec<String> {
        let mut names = Vec::new();
        if let Some(subsets) = value.get("subsets").and_then(|item| item.as_array()) {
            for subset in subsets {
                if let Some(addresses) = subset.get("addresses").and_then(|item| item.as_array()) {
                    for address in addresses {
                        if let Some(name) = address
                            .pointer("/targetRef/name")
                            .and_then(|item| item.as_str())
                        {
                            names.push(name.to_owned());
                        }
                    }
                }
            }
        }
        names.sort();
        names.dedup();
        names
    }

    /// Cutover is proven only when the stable Service endpoints are exactly
    /// the candidate Pod. Empty, missing, old-only, or split sets are not
    /// success.
    pub fn endpoints_are_exactly(names: &[String], candidate: &str) -> bool {
        names.len() == 1 && names[0] == candidate
    }

    pub async fn wait_endpoints_exactly(
        &self,
        service: &str,
        candidate: &str,
        timeout: Duration,
    ) -> Result<(), FabricError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let endpoints = self.get_namespaced("endpoints", service).await?;
            if let Some(value) = endpoints {
                let names = Self::endpoint_pod_names(&value);
                if Self::endpoints_are_exactly(&names, candidate) {
                    return Ok(());
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(FabricError::Unknown(format!(
                    "database service {service} did not settle on {candidate}"
                )));
            }
            sleep(Duration::from_secs(2)).await;
        }
    }

    pub async fn object_is_foreign(
        &self,
        kind: &str,
        name: &str,
        namespaced: bool,
        workspace_id: &str,
    ) -> Result<bool, FabricError> {
        let value = if namespaced {
            self.get_namespaced(kind, name).await?
        } else {
            let out = self.kubectl(&["get", kind, name, "-o", "json"]).await?;
            if is_not_found(&out) {
                None
            } else if out.status != 0 {
                return Err(FabricError::Realize(out.stderr));
            } else {
                Some(
                    serde_json::from_str(&out.stdout)
                        .map_err(|_| FabricError::Realize(format!("{kind} JSON was unusable")))?,
                )
            }
        };
        let Some(value) = value else {
            return Ok(false);
        };
        Ok(!owned_by(&value, workspace_id))
    }

    pub fn verify_pv(
        &self,
        pv: &PvInfo,
        workspace_id: &str,
        device: &str,
    ) -> Result<(), FabricError> {
        if pv.volume_mode != "Block" {
            return Err(FabricError::Realize(format!(
                "PV {} volumeMode is {}, want Block",
                pv.name, pv.volume_mode
            )));
        }
        if !pv.access_modes.iter().any(|mode| mode == "ReadWriteOnce") {
            return Err(FabricError::Realize(format!(
                "PV {} is not ReadWriteOnce",
                pv.name
            )));
        }
        if pv.reclaim != "Retain" {
            return Err(FabricError::Realize(format!(
                "PV {} reclaim policy is {}, want Retain",
                pv.name, pv.reclaim
            )));
        }
        if pv.storage_class != self.storage_class {
            return Err(FabricError::Realize(format!(
                "PV {} storage class is {}",
                pv.name, pv.storage_class
            )));
        }
        if pv.node != self.node_name {
            return Err(FabricError::Realize(format!(
                "PV {} node affinity is {}, want {}",
                pv.name, pv.node, self.node_name
            )));
        }
        require_stable_block_path(&pv.path)?;
        require_stable_block_path(device)?;
        if pv.path != device {
            let same_inode = std::fs::canonicalize(&pv.path)
                .ok()
                .zip(std::fs::canonicalize(device).ok())
                .is_some_and(|(left, right)| left == right);
            if !same_inode {
                return Err(FabricError::Realize(format!(
                    "PV {} path {} does not match reserved device {device}",
                    pv.name, pv.path
                )));
            }
        }
        if pv.workspace_label.as_deref() != Some(workspace_id) || !pv.managed {
            return Err(FabricError::Foreign(format!(
                "PV {} is not owned by workspace {workspace_id}",
                pv.name
            )));
        }
        Ok(())
    }

    /// Waits until Kubernetes reports the pod's `Ready` condition as `True`.
    ///
    /// The generated Pod carries a mount-validating readinessProbe, so Ready
    /// is exactly the statement that the workspace block device exists and an
    /// ext4 filesystem is live at the fixed `/workspace` path. Phase Running
    /// alone never proves readiness.
    pub async fn wait_pod_ready(
        &self,
        name: &str,
        timeout: Duration,
    ) -> Result<PodInfo, FabricError> {
        self.wait_pod_phase(name, timeout, true).await
    }

    /// Waits until the pod phase is `Running`. Application Ready is not
    /// required: `/healthz` may depend on a migration that has not run.
    pub async fn wait_pod_running(
        &self,
        name: &str,
        timeout: Duration,
    ) -> Result<PodInfo, FabricError> {
        self.wait_pod_phase(name, timeout, false).await
    }

    async fn wait_pod_phase(
        &self,
        name: &str,
        timeout: Duration,
        require_ready: bool,
    ) -> Result<PodInfo, FabricError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(pod) = self.get_pod(name).await? {
                let running = pod.phase == "Running" && pod.uid != "";
                if running && (!require_ready || pod.ready) {
                    return Ok(pod);
                }
                if pod.phase == "Failed" || pod.phase == "Succeeded" {
                    return Err(FabricError::Realize(format!(
                        "pod {name} reached {} before Running",
                        pod.phase
                    )));
                }
            }
            if tokio::time::Instant::now() >= deadline {
                let wanted = if require_ready { "Ready" } else { "Running" };
                return Err(FabricError::Unknown(format!(
                    "pod {name} did not become {wanted}"
                )));
            }
            sleep(Duration::from_secs(2)).await;
        }
    }

    pub async fn get_pod(&self, name: &str) -> Result<Option<PodInfo>, FabricError> {
        let Some(value) = self.get_namespaced("pod", name).await? else {
            return Ok(None);
        };
        let uid = value
            .pointer("/metadata/uid")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let phase = value
            .pointer("/status/phase")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let runtime_class = value
            .pointer("/spec/runtimeClassName")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let ready = ready_condition_true(&value);
        let image = value
            .pointer("/spec/containers/0/image")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let host_network = value
            .pointer("/spec/hostNetwork")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let container_id = value
            .pointer("/status/containerStatuses/0/containerID")
            .and_then(Value::as_str)
            .map(|id| id.rsplit("://").next().unwrap_or(id).to_owned());
        let sandbox_id = match self.lookup_sandbox(name).await {
            Ok(Some(id)) => Some(id),
            _ => container_id,
        };
        Ok(Some(PodInfo {
            name: name.to_owned(),
            uid,
            sandbox_id,
            runtime_class,
            phase,
            ready,
            image,
            host_network,
        }))
    }

    /// ClusterIP for a Fabric Service. CoreDNS is not required on the
    /// Application data plane: Caddy reverse_proxy uses this address.
    pub async fn service_cluster_ip(&self, name: &str) -> Result<String, FabricError> {
        let Some(value) = self.get_namespaced("svc", name).await? else {
            return Err(FabricError::Unknown(format!("service {name} is missing")));
        };
        let ip = value
            .pointer("/spec/clusterIP")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if ip.is_empty() || ip == "None" || ip.contains(':') || ip.split('.').count() != 4 {
            return Err(FabricError::Unknown(format!(
                "service {name} has no IPv4 ClusterIP"
            )));
        }
        Ok(ip)
    }

    async fn lookup_sandbox(&self, pod_name: &str) -> Result<Option<String>, FabricError> {
        let out = self
            .crictl(&[
                "pods",
                "--name",
                pod_name,
                "-q",
                "--namespace",
                &self.namespace,
            ])
            .await;
        let Ok(out) = out else {
            return Ok(None);
        };
        if out.status != 0 {
            return Ok(None);
        }
        let id = out
            .stdout
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(ToOwned::to_owned);
        Ok(id)
    }

    pub async fn delete_named(
        &self,
        kind: &str,
        name: &str,
        namespaced: bool,
        timeout_secs: u64,
    ) -> Result<(), FabricError> {
        self.delete_named_wait(kind, name, namespaced, timeout_secs, true)
            .await
    }

    /// Startup PV retarget must not block listen on a hung pod. `--wait=false`
    /// plus grace-period 0 releases the object; PVC/PV recreate follows.
    pub async fn delete_named_wait(
        &self,
        kind: &str,
        name: &str,
        namespaced: bool,
        timeout_secs: u64,
        wait: bool,
    ) -> Result<(), FabricError> {
        let timeout = format!("{timeout_secs}s");
        let mut args = vec!["delete", kind, name, "--ignore-not-found"];
        if wait {
            args.extend_from_slice(&["--wait=true", "--timeout", timeout.as_str()]);
        } else {
            args.extend_from_slice(&["--wait=false", "--grace-period=0"]);
        }
        if namespaced {
            args.extend_from_slice(&["-n", self.namespace.as_str()]);
        }
        let out = self.kubectl(&args).await?;
        if out.status != 0 && !is_not_found(&out) {
            return Err(FabricError::Unknown(format!(
                "delete {kind}/{name}: {}",
                out.stderr.trim()
            )));
        }
        Ok(())
    }

    /// Startup must not apply a replacement while the previous object still
    /// occupies the name. `delete --wait=false` only sends the request.
    pub async fn wait_named_gone(
        &self,
        kind: &str,
        name: &str,
        namespaced: bool,
        timeout: Duration,
    ) -> Result<(), FabricError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let present = if namespaced {
                self.get_namespaced(kind, name).await?.is_some()
            } else {
                self.get_unnamespaced(kind, name).await?.is_some()
            };
            if !present {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(FabricError::Unknown(format!(
                    "{kind}/{name} still present after delete"
                )));
            }
            sleep(Duration::from_millis(200)).await;
        }
    }

    pub async fn exec_runner(
        &self,
        pod: &str,
        command: &str,
        timeout_ms: u64,
    ) -> Result<ExecOutput, FabricError> {
        // The corrected runner preserves the post-`--` vector verbatim and
        // never implies a shell, so the shell must be requested explicitly:
        // PROGRAM=/bin/sh, ARGS=["-c", command]. Nothing is ever joined.
        let timeout_arg = timeout_ms.to_string();
        let request_timeout = format!("{}s", (timeout_ms / 1000).saturating_add(15));
        let mut process = Command::new(&self.kubectl_program);
        process.args(&self.kubectl_prefix);
        if let Some(kubeconfig) = &self.kubeconfig {
            process.arg("--kubeconfig").arg(kubeconfig);
        }
        process.args([
            "exec",
            "-n",
            &self.namespace,
            pod,
            "-c",
            "runner",
            "--request-timeout",
            &request_timeout,
            "--",
            "/bin/voie-runner",
            "--timeout-ms",
            &timeout_arg,
            "--",
            "/bin/sh",
            "-c",
            command,
        ]);
        process.stdout(Stdio::piped());
        process.stderr(Stdio::piped());
        let output = process
            .output()
            .await
            .map_err(|_| FabricError::Unknown("kubectl exec failed to start".into()))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        match output.status.code() {
            Some(code) => Ok(ExecOutput {
                exit_code: code,
                stdout,
                stderr,
                ambiguous: false,
            }),
            None => Ok(ExecOutput {
                exit_code: -1,
                stdout,
                stderr,
                ambiguous: true,
            }),
        }
    }

    /// Runs one typed guest helper (not user Bash) with a server-selected
    /// deadline. Packaging uses this path so the 30s runner bound does not
    /// apply to `voie-pack`.
    pub async fn exec_guest(
        &self,
        pod: &str,
        container: &str,
        argv: &[&str],
        timeout_ms: u64,
    ) -> Result<ExecOutput, FabricError> {
        if argv.is_empty() {
            return Err(FabricError::Config("guest argv is required"));
        }
        if !valid_k8s_name(container) {
            return Err(FabricError::Config("guest container name is invalid"));
        }
        if !valid_k8s_name(pod) {
            return Err(FabricError::Config("guest pod name is invalid"));
        }
        let request_timeout = format!("{}s", (timeout_ms / 1000).saturating_add(15));
        let mut process = Command::new(&self.kubectl_program);
        process.args(&self.kubectl_prefix);
        if let Some(kubeconfig) = &self.kubeconfig {
            process.arg("--kubeconfig").arg(kubeconfig);
        }
        process.args([
            "exec",
            "-n",
            &self.namespace,
            pod,
            "-c",
            container,
            "--request-timeout",
            &request_timeout,
            "--",
        ]);
        process.args(argv);
        process.stdout(Stdio::piped());
        process.stderr(Stdio::piped());
        let output = process
            .output()
            .await
            .map_err(|_| FabricError::Unknown("kubectl exec failed to start".into()))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        match output.status.code() {
            Some(code) => Ok(ExecOutput {
                exit_code: code,
                stdout,
                stderr,
                ambiguous: false,
            }),
            None => Ok(ExecOutput {
                exit_code: -1,
                stdout,
                stderr,
                ambiguous: true,
            }),
        }
    }

    /// Streams guest stdout onto a host file. Database dumps and workspace
    /// snapshots must not be buffered as `Vec<u8>`.
    pub async fn exec_guest_stdout_file(
        &self,
        pod: &str,
        container: &str,
        argv: &[&str],
        local: &Path,
        timeout_ms: u64,
    ) -> Result<ExecOutput, FabricError> {
        if argv.is_empty() {
            return Err(FabricError::Config("guest argv is required"));
        }
        if !valid_k8s_name(container) || !valid_k8s_name(pod) {
            return Err(FabricError::Config("guest identity is invalid"));
        }
        if let Some(parent) = local.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                FabricError::Realize(format!("cannot stage guest stdout: {error}"))
            })?;
        }
        let request_timeout = format!("{}s", (timeout_ms / 1000).saturating_add(15));
        let mut process = Command::new(&self.kubectl_program);
        process.kill_on_drop(true);
        process.args(&self.kubectl_prefix);
        if let Some(kubeconfig) = &self.kubeconfig {
            process.arg("--kubeconfig").arg(kubeconfig);
        }
        process.args([
            "exec",
            "-n",
            &self.namespace,
            pod,
            "-c",
            container,
            "--request-timeout",
            &request_timeout,
            "--",
        ]);
        process.args(argv);
        process.stdout(Stdio::piped());
        process.stderr(Stdio::piped());
        let mut child = process
            .spawn()
            .map_err(|_| FabricError::Unknown("kubectl exec failed to start".into()))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| FabricError::Unknown("kubectl exec stdout missing".into()))?;
        let mut file = tokio::fs::File::create(local).await.map_err(|error| {
            FabricError::Realize(format!("cannot create guest stdout file: {error}"))
        })?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        match tokio::time::timeout_at(deadline, tokio::io::copy(&mut stdout, &mut file)).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                let _ = child.kill().await;
                return Err(FabricError::Realize(format!(
                    "cannot write guest stdout: {error}"
                )));
            }
            Err(_) => {
                let _ = child.kill().await;
                return Err(FabricError::Unknown(
                    "guest stdout copy did not settle".into(),
                ));
            }
        }
        let output = match tokio::time::timeout_at(deadline, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(_)) => {
                return Err(FabricError::Unknown("kubectl exec failed to finish".into()));
            }
            Err(_) => {
                return Err(FabricError::Unknown(
                    "guest stdout copy did not settle".into(),
                ));
            }
        };
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        match output.status.code() {
            Some(code) => Ok(ExecOutput {
                exit_code: code,
                stdout: String::new(),
                stderr,
                ambiguous: false,
            }),
            None => Ok(ExecOutput {
                exit_code: -1,
                stdout: String::new(),
                stderr,
                ambiguous: true,
            }),
        }
    }

    /// Streams a host file into guest stdin. Restore never loads the dump
    /// as `Vec<u8>`.
    pub async fn exec_guest_stdin_file(
        &self,
        pod: &str,
        container: &str,
        argv: &[&str],
        local: &Path,
        timeout_ms: u64,
    ) -> Result<ExecOutput, FabricError> {
        if argv.is_empty() {
            return Err(FabricError::Config("guest argv is required"));
        }
        if !valid_k8s_name(container) || !valid_k8s_name(pod) {
            return Err(FabricError::Config("guest identity is invalid"));
        }
        let request_timeout = format!("{}s", (timeout_ms / 1000).saturating_add(15));
        let mut process = Command::new(&self.kubectl_program);
        process.kill_on_drop(true);
        process.args(&self.kubectl_prefix);
        if let Some(kubeconfig) = &self.kubeconfig {
            process.arg("--kubeconfig").arg(kubeconfig);
        }
        process.args([
            "exec",
            "-i",
            "-n",
            &self.namespace,
            pod,
            "-c",
            container,
            "--request-timeout",
            &request_timeout,
            "--",
        ]);
        process.args(argv);
        process.stdin(Stdio::piped());
        process.stdout(Stdio::piped());
        process.stderr(Stdio::piped());
        let mut child = process
            .spawn()
            .map_err(|_| FabricError::Unknown("kubectl exec failed to start".into()))?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        {
            let Some(mut stdin) = child.stdin.take() else {
                let _ = child.kill().await;
                return Err(FabricError::Unknown("kubectl exec stdin missing".into()));
            };
            let mut file = tokio::fs::File::open(local).await.map_err(|error| {
                FabricError::Realize(format!("cannot read restore artifact: {error}"))
            })?;
            match tokio::time::timeout_at(deadline, tokio::io::copy(&mut file, &mut stdin)).await {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    let _ = child.kill().await;
                    return Err(FabricError::Realize(format!(
                        "cannot stream restore artifact: {error}"
                    )));
                }
                Err(_) => {
                    let _ = child.kill().await;
                    return Err(FabricError::Unknown(
                        "guest stdin copy did not settle".into(),
                    ));
                }
            }
        }
        let output = match tokio::time::timeout_at(deadline, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(_)) => {
                return Err(FabricError::Unknown("kubectl exec failed to finish".into()));
            }
            Err(_) => {
                return Err(FabricError::Unknown(
                    "guest stdin copy did not settle".into(),
                ));
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        match output.status.code() {
            Some(code) => Ok(ExecOutput {
                exit_code: code,
                stdout,
                stderr,
                ambiguous: false,
            }),
            None => Ok(ExecOutput {
                exit_code: -1,
                stdout,
                stderr,
                ambiguous: true,
            }),
        }
    }

    pub async fn label_namespaced(
        &self,
        kind: &str,
        name: &str,
        label: &str,
    ) -> Result<(), FabricError> {
        let out = self
            .kubectl(&[
                "label",
                kind,
                name,
                label,
                "--overwrite",
                "-n",
                self.namespace.as_str(),
            ])
            .await?;
        if out.status != 0 {
            return Err(FabricError::Unknown(format!(
                "label {kind}/{name}: {}",
                out.stderr.trim()
            )));
        }
        Ok(())
    }

    /// Copies one guest file onto the Fabric host. The host never runs the
    /// Application pack; it only collects the guest-produced artifact.
    /// Firecracker/Kata guests do not support `kubectl cp`; bytes stream
    /// through `kubectl exec` the same way bash and `voie-pack` already run.
    pub async fn copy_from_pod(
        &self,
        pod: &str,
        container: &str,
        remote: &str,
        local: &Path,
    ) -> Result<(), FabricError> {
        copy_pod_file(self, pod, container, remote, local, true).await
    }

    /// Stages one host file into a guest path. Used for Database restore
    /// dumps; Blob credentials never enter the guest.
    pub async fn copy_to_pod(
        &self,
        pod: &str,
        container: &str,
        local: &Path,
        remote: &str,
    ) -> Result<(), FabricError> {
        copy_pod_file(self, pod, container, remote, local, false).await
    }

    /// Loads a new route map into the running gateway without deleting the
    /// Pod. Cutover probes the wildcard edge; recreating Caddy on every
    /// route change drops every Application. The Caddyfile is piped on
    /// stdin because the gateway image has no `tar` for `kubectl cp`.
    pub async fn reload_gateway_caddyfile(&self, caddyfile: &str) -> Result<(), FabricError> {
        let pod = "voie-gateway";
        let mut process = Command::new(&self.kubectl_program);
        process.args(&self.kubectl_prefix);
        if let Some(kubeconfig) = &self.kubeconfig {
            process.arg("--kubeconfig").arg(kubeconfig);
        }
        process.args([
            "exec",
            "-i",
            "-n",
            &self.namespace,
            pod,
            "-c",
            "gateway",
            "--request-timeout",
            "30s",
            "--",
            "/bin/caddy",
            "reload",
            "--config",
            "-",
            "--adapter",
            "caddyfile",
            "--address",
            "unix//tmp/caddy-admin.sock",
        ]);
        process.stdin(Stdio::piped());
        process.stdout(Stdio::piped());
        process.stderr(Stdio::piped());
        let mut child = process
            .spawn()
            .map_err(|_| FabricError::Unknown("gateway reload failed to start".into()))?;
        {
            let Some(mut stdin) = child.stdin.take() else {
                let _ = child.kill().await;
                return Err(FabricError::Unknown("gateway reload stdin missing".into()));
            };
            if let Err(error) = stdin.write_all(caddyfile.as_bytes()).await {
                let _ = child.kill().await;
                return Err(FabricError::Realize(format!(
                    "cannot send gateway Caddyfile: {error}"
                )));
            }
        }
        let output =
            match tokio::time::timeout(Duration::from_secs(20), child.wait_with_output()).await {
                Ok(Ok(output)) => output,
                Ok(Err(_)) => {
                    return Err(FabricError::Unknown(
                        "gateway reload failed to finish".into(),
                    ));
                }
                Err(_) => {
                    return Err(FabricError::Unknown("gateway reload did not settle".into()));
                }
            };
        match output.status.code() {
            None => Err(FabricError::Unknown("gateway reload did not settle".into())),
            Some(0) => Ok(()),
            Some(code) => Err(FabricError::Realize(format!(
                "gateway reload exited {code}: {}",
                String::from_utf8_lossy(&output.stderr)
            ))),
        }
    }

    /// Creates or replaces one Opaque Secret from bytes. The value is never
    /// written into a product API body or a daemon-owned Pod template.
    pub async fn apply_opaque_secret(
        &self,
        name: &str,
        key: &str,
        value: &[u8],
        extra_labels: &[(&str, &str)],
    ) -> Result<(), FabricError> {
        self.apply_yaml(&opaque_secret_yaml(
            &self.namespace,
            name,
            extra_labels,
            &[(key, value)],
        )?)
        .await
    }

    /// Creates or replaces one Opaque Secret with several keys. Values are
    /// never written into a Pod template.
    pub async fn apply_opaque_secret_pairs(
        &self,
        name: &str,
        pairs: &[(String, Vec<u8>)],
        extra_labels: &[(&str, &str)],
    ) -> Result<(), FabricError> {
        let refs: Vec<(&str, &[u8])> = pairs
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_slice()))
            .collect();
        self.apply_yaml(&opaque_secret_yaml(
            &self.namespace,
            name,
            extra_labels,
            &refs,
        )?)
        .await
    }

    /// Reads one Opaque Secret key. Used to copy a Database password into an
    /// Application env secret without putting the value in a Pod template.
    pub async fn read_opaque_secret(&self, name: &str, key: &str) -> Result<Vec<u8>, FabricError> {
        if !valid_k8s_name(name) || !valid_secret_key(key) {
            return Err(FabricError::Config("secret identity is invalid"));
        }
        let jsonpath = format!("{{.data.{key}}}");
        let mut process = Command::new(&self.kubectl_program);
        process.args(&self.kubectl_prefix);
        if let Some(kubeconfig) = &self.kubeconfig {
            process.arg("--kubeconfig").arg(kubeconfig);
        }
        process.args([
            "get",
            "secret",
            name,
            "-n",
            &self.namespace,
            "-o",
            &format!("jsonpath={jsonpath}"),
        ]);
        process.stdout(Stdio::piped());
        process.stderr(Stdio::piped());
        let output = process
            .output()
            .await
            .map_err(|_| FabricError::Unknown("kubectl get secret failed to start".into()))?;
        if !output.status.success() {
            return Err(FabricError::Unknown("opaque secret is unreadable".into()));
        }
        let encoded = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if encoded.is_empty() {
            return Err(FabricError::Unknown("opaque secret is empty".into()));
        }
        BASE64
            .decode(encoded.as_bytes())
            .map_err(|_| FabricError::Unknown("opaque secret encoding is unusable".into()))
    }

    /// Bounded Application container logs. The bytes are not journaled.
    pub async fn pod_logs(
        &self,
        pod: &str,
        container: &str,
        tail: u32,
    ) -> Result<Vec<u8>, FabricError> {
        if !valid_k8s_name(pod) || !valid_k8s_name(container) {
            return Err(FabricError::Config("log identity is invalid"));
        }
        let tail = tail.clamp(1, 10_000).to_string();
        let mut process = Command::new(&self.kubectl_program);
        process.args(&self.kubectl_prefix);
        if let Some(kubeconfig) = &self.kubeconfig {
            process.arg("--kubeconfig").arg(kubeconfig);
        }
        process.args([
            "logs",
            "-n",
            &self.namespace,
            pod,
            "-c",
            container,
            "--tail",
            &tail,
        ]);
        process.stdout(Stdio::piped());
        process.stderr(Stdio::piped());
        let output = process
            .output()
            .await
            .map_err(|_| FabricError::Unknown("kubectl logs failed to start".into()))?;
        if output.stdout.len() > 256 * 1024 {
            Ok(output.stdout[..256 * 1024].to_vec())
        } else {
            Ok(output.stdout)
        }
    }

    /// Observes every place a guest execution can outlive its workspace:
    /// the pod object, the jailer tree (including the identity side-index),
    /// the live VMM process, and its children.
    pub async fn observe_residue(
        &self,
        pod_name: &str,
        sandbox_id: Option<&str>,
    ) -> Result<Residue, FabricError> {
        let pod_present = self.get_pod(pod_name).await?.is_some();
        let (jail_present, vmm_present, children_present) =
            if let Some(id) = sandbox_id.filter(|id| !id.is_empty()) {
                let jail = self.jailer_root.join(id);
                let mut jail_present = path_exists(&jail)?;
                let identities = self.jailer_root.join(".jailer-identities");
                match std::fs::read_dir(&identities) {
                    Ok(entries) => {
                        for entry in entries {
                            let entry = entry.map_err(|error| {
                                FabricError::Unknown(format!(
                                    "cannot enumerate jailer identity index: {error}"
                                ))
                            })?;
                            let bytes = std::fs::read(entry.path()).map_err(|error| {
                                FabricError::Unknown(format!(
                                    "cannot read jailer identity index: {error}"
                                ))
                            })?;
                            if bytes
                                .windows(id.len())
                                .any(|window| window == id.as_bytes())
                            {
                                jail_present = true;
                                break;
                            }
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(FabricError::Unknown(format!(
                            "cannot inspect jailer identity index: {error}"
                        )));
                    }
                }
                let procs = firecracker_for_sandbox(id)?;
                (jail_present, !procs.is_empty(), any_children(&procs)?)
            } else {
                let pids = firecracker_pids()?;
                // Without a sandbox identity this pod's jail and VMM cannot be
                // attributed, so per-pod absence is unprovable. The only truthful
                // positive absence left is host-wide: no jail tree entry at all
                // and no firecracker or jailer process at all. Anything present
                // keeps the residue unknown, which holds the reservation.
                (
                    jailer_has_jails(&self.jailer_root)?,
                    !pids.is_empty(),
                    any_children(&pids)?,
                )
            };
        Ok(Residue {
            pod_present,
            jail_present,
            vmm_present,
            children_present,
        })
    }

    /// Positively observes that no CRI sandbox remains for this pod. A
    /// successful empty `crictl pods -q` response is absence; an unreadable
    /// or failed response is unknown and therefore blocks cleanup.
    pub(crate) async fn sandbox_absent(&self, pod_name: &str) -> Result<bool, FabricError> {
        let out = self
            .crictl(&[
                "pods",
                "--name",
                pod_name,
                "-q",
                "--namespace",
                &self.namespace,
            ])
            .await
            .map_err(|error| {
                FabricError::Unknown(format!(
                    "cannot observe sandbox for pod {pod_name}: {error}"
                ))
            })?;
        if out.status != 0 {
            return Err(FabricError::Unknown(format!(
                "cannot observe sandbox for pod {pod_name}: {}",
                out.stderr.trim()
            )));
        }
        Ok(out.stdout.lines().all(|line| line.trim().is_empty()))
    }

    /// Recovers the sandbox identity for a pod from the local CRI when the
    /// store lost it. Returns `None` when no live sandbox carries the pod's
    /// name; callers must then treat runtime cleanliness as unproven.
    pub async fn discover_sandbox_id(&self, pod_name: &str) -> Option<String> {
        match self.lookup_sandbox(pod_name).await {
            Ok(Some(id)) if !id.is_empty() => Some(id),
            _ => None,
        }
    }

    /// Lists logical volume names in the declared pool. Used by startup
    /// reconciliation to find prepared-but-unclaimed slots; an unusable
    /// listing is an error so the caller removes nothing it cannot see.
    pub async fn list_lv_names(&self) -> Result<Vec<String>, FabricError> {
        let out = self
            .host(
                "lvs",
                &["--noheadings", "--readonly", "-o", "lv_name", &self.vg],
            )
            .await?;
        if out.status != 0 {
            return Err(FabricError::Realize(format!(
                "lvs {}: {}",
                self.vg,
                out.stderr.trim()
            )));
        }
        Ok(out
            .stdout
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToOwned::to_owned)
            .collect())
    }

    pub async fn observe_vg(&self) -> Result<VgObservation, FabricError> {
        let vg = self
            .host(
                "vgs",
                &[
                    "--noheadings",
                    "--nosuffix",
                    "--units",
                    "b",
                    "-o",
                    "vg_size,vg_free",
                    &self.vg,
                ],
            )
            .await?;
        if vg.status != 0 {
            return Err(FabricError::Realize(format!(
                "vgs {}: {}",
                self.vg,
                vg.stderr.trim()
            )));
        }
        let mut parts = vg.stdout.split_whitespace();
        let device_bytes = parse_lvm_bytes(parts.next().unwrap_or(""))?;
        let physical_free_bytes = parse_lvm_bytes(parts.next().unwrap_or(""))?;
        let lvs = self
            .host(
                "lvs",
                &[
                    "--noheadings",
                    "--nosuffix",
                    "--units",
                    "b",
                    "-o",
                    "lv_name,lv_size,data_percent,metadata_percent",
                    &self.vg,
                ],
            )
            .await?;
        if lvs.status != 0 {
            return Err(FabricError::Realize(format!(
                "lvs {}: {}",
                self.vg,
                lvs.stderr.trim()
            )));
        }
        let mut runtime_pool_bytes = 0;
        let mut runtime_pool_used_bytes = 0;
        let mut workspace_pool_bytes = 0;
        let mut workspace_pool_used_bytes = 0;
        let mut workspace_pool_metadata_percent = None;
        let workspace_pool = self.storage.workspace_pool.as_str();
        for line in lvs.stdout.lines() {
            let mut cols = line.split_whitespace();
            let Some(name) = cols.next() else { continue };
            let size = parse_lvm_bytes(cols.next().unwrap_or("0"))?;
            let percent = cols.next().unwrap_or("");
            let metadata = cols.next().unwrap_or("");
            if name == "runtime" {
                runtime_pool_bytes = size;
                if let Ok(used) = percent.parse::<f64>() {
                    runtime_pool_used_bytes = ((size as f64) * used / 100.0) as u64;
                }
            }
            if name == workspace_pool {
                workspace_pool_bytes = size;
                if let Ok(used) = percent.parse::<f64>() {
                    workspace_pool_used_bytes = ((size as f64) * used / 100.0) as u64;
                }
                if let Ok(meta) = metadata.parse::<f64>() {
                    workspace_pool_metadata_percent = Some(meta.round() as i32);
                }
            }
        }
        Ok(VgObservation {
            device_bytes,
            physical_free_bytes,
            runtime_pool_bytes,
            runtime_pool_used_bytes,
            workspace_pool_bytes,
            workspace_pool_used_bytes,
            workspace_pool_metadata_percent,
        })
    }

    /// Waits until the residue is positively clean or the deadline passes;
    /// the final observation is always returned so callers decide on facts,
    /// never on timeouts alone.
    pub async fn wait_residue_gone(
        &self,
        pod_name: &str,
        sandbox_id: Option<&str>,
        timeout: Duration,
    ) -> Result<Residue, FabricError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let residue = self.observe_residue(pod_name, sandbox_id).await?;
            if residue.runtime_clean() {
                return Ok(residue);
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(residue);
            }
            sleep(Duration::from_secs(2)).await;
        }
    }

    /// Waits until no CRI sandbox remains for this pod, or the deadline
    /// passes. Kata Firecracker sandboxes linger as NotReady after the
    /// Kubernetes pod is gone; a single observation would hold the
    /// reservation forever even though the sandbox self-reaps. The
    /// final observation is returned so callers decide on facts.
    pub async fn wait_sandbox_absent(
        &self,
        pod_name: &str,
        timeout: Duration,
    ) -> Result<bool, FabricError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.sandbox_absent(pod_name).await? {
                return Ok(true);
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(false);
            }
            sleep(Duration::from_secs(2)).await;
        }
    }

    /// The desired guest isolation policy: default-deny ingress AND egress
    /// for every Workspace pod (`io.voie/kind=workspace`), except DNS towards
    /// kube-system and, when deployment approved them, exactly the configured
    /// destination CIDRs over one TCP port. Application and gateway pods use
    /// their own policies.
    pub fn network_policy_yaml(&self) -> String {
        let approved = self
            .approved_egress
            .as_ref()
            .map(|approved| {
                let mut blocks = String::new();
                for cidr in &approved.cidrs {
                    blocks.push_str(&format!(
                        "        - ipBlock:\n            cidr: {cidr}\n"
                    ));
                }
                format!(
                    "    - to:\n{blocks}      ports:\n        - protocol: TCP\n          port: {}\n",
                    approved.tcp_port
                )
            })
            .unwrap_or_default();
        format!(
            "apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: {name}
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
spec:
  podSelector:
    matchLabels:
      io.voie/kind: \"workspace\"
  policyTypes:
    - Ingress
    - Egress
  ingress: []
  egress:
    - to:
        - namespaceSelector:
            matchLabels:
              kubernetes.io/metadata.name: kube-system
      ports:
        - protocol: UDP
          port: 53
        - protocol: TCP
          port: 53
{approved}",
            name = NETWORK_POLICY_NAME,
            ns = self.namespace,
        )
    }

    /// The spec half of [`Live::network_policy_yaml`] as JSON, so the live
    /// object read back from `kubectl get -o json` can be compared by
    /// content rather than by YAML text.
    pub fn desired_network_policy_spec(&self) -> Value {
        let mut egress = vec![serde_json::json!({
            "to": [
                {
                    "namespaceSelector": {
                        "matchLabels": {
                            "kubernetes.io/metadata.name": "kube-system"
                        }
                    }
                }
            ],
            "ports": [
                { "protocol": "UDP", "port": 53 },
                { "protocol": "TCP", "port": 53 }
            ]
        })];
        if let Some(approved) = &self.approved_egress {
            egress.push(serde_json::json!({
                "to": approved
                    .cidrs
                    .iter()
                    .map(|cidr| serde_json::json!({ "ipBlock": { "cidr": cidr } }))
                    .collect::<Vec<_>>(),
                "ports": [{ "protocol": "TCP", "port": approved.tcp_port }],
            }));
        }
        serde_json::json!({
            "podSelector": {
                "matchLabels": {
                    "io.voie/kind": "workspace"
                }
            },
            "policyTypes": ["Ingress", "Egress"],
            // An empty ingress list is the default-deny statement: nothing
            // may open a connection towards a guest pod. Return traffic for
            // allowed egress flows stays stateful and is unaffected.
            "ingress": [],
            "egress": egress,
        })
    }

    /// Reads the live NetworkPolicy object, if it exists.
    pub async fn observe_network_policy(&self) -> Result<Option<Value>, FabricError> {
        self.get_namespaced("networkpolicy", NETWORK_POLICY_NAME)
            .await
    }

    pub async fn release_block(&self, slot: &BlockSlot) -> Result<(), FabricError> {
        let Some(lv_name) = &slot.lv_name else {
            return Ok(());
        };
        self.close_encrypted(lv_name).await?;
        let spec = format!("{}/{}", self.vg, lv_name);
        let out = self.host("lvremove", &["-y", &spec]).await?;
        if out.status != 0 && !out.stderr.contains("Failed to find") {
            return Err(FabricError::Unknown(format!(
                "lvremove {spec}: {}",
                out.stderr.trim()
            )));
        }
        Ok(())
    }

    pub fn pv_yaml(&self, workspace_id: &str, pv_name: &str, device: &str, bytes: u64) -> String {
        format!(
            "apiVersion: v1
kind: PersistentVolume
metadata:
  name: {pv_name}
  labels:
    io.voie/managed: \"true\"
    io.voie/workspace: \"{workspace_id}\"
spec:
  capacity:
    storage: {size}
  volumeMode: Block
  accessModes:
    - ReadWriteOnce
  persistentVolumeReclaimPolicy: Retain
  storageClassName: {sc}
  local:
    path: {device}
  nodeAffinity:
    required:
      nodeSelectorTerms:
        - matchExpressions:
            - key: kubernetes.io/hostname
              operator: In
              values:
                - {node}
",
            sc = self.storage_class,
            node = self.node_name,
            size = crate::k8s_quantity(bytes),
        )
    }

    pub fn pvc_yaml(
        &self,
        workspace_id: &str,
        pvc_name: &str,
        pv_name: &str,
        bytes: u64,
    ) -> String {
        format!(
            "apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: {pvc_name}
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
    io.voie/workspace: \"{workspace_id}\"
spec:
  accessModes:
    - ReadWriteOnce
  volumeMode: Block
  storageClassName: {sc}
  volumeName: {pv_name}
  resources:
    requests:
      storage: {size}
",
            ns = self.namespace,
            sc = self.storage_class,
            size = crate::k8s_quantity(bytes),
        )
    }

    /// Renders the guest pod manifest. This manifest is daemon-owned, so the
    /// identity and credential posture of the guest are decided here: the
    /// pod is admitted under the dedicated [`WORKSPACE_SERVICE_ACCOUNT_NAME`]
    /// account — which nothing grants any RBAC role — while
    /// `automountServiceAccountToken: false` suppresses the only credential
    /// Kubernetes would inject on its own and `enableServiceLinks: false`
    /// keeps service environment links out of the container. Combined with
    /// the guest-egress
    /// NetworkPolicy, the guest holds no Kubernetes or cloud credentials and
    /// cannot reach the surfaces that would accept them. The mount-validating
    /// readinessProbe keeps the pod's `Ready` condition true only while the
    /// workspace device exists and is ext4-mounted at `/workspace`.
    pub fn pod_yaml(
        &self,
        workspace_id: &str,
        pod_name: &str,
        pvc_name: &str,
        generation: i64,
    ) -> String {
        format!(
            "apiVersion: v1
kind: Pod
metadata:
  name: {pod_name}
  namespace: {ns}
  labels:
    io.voie/managed: \"true\"
    io.voie/kind: \"workspace\"
    io.voie/workspace: \"{workspace_id}\"
    io.voie/generation: \"{generation}\"
spec:
  restartPolicy: Never
  terminationGracePeriodSeconds: 5
  runtimeClassName: {runtime}
  nodeName: {node}
  serviceAccountName: {sa}
  automountServiceAccountToken: false
  enableServiceLinks: false
  containers:
    - name: runner
      image: {image}
      imagePullPolicy: Never
      securityContext:
        privileged: true
      readinessProbe:
        exec:
          command:
            - /bin/sh
            - -c
            - grep -qs \" /workspace \" /proc/mounts
        initialDelaySeconds: 1
        periodSeconds: 2
        timeoutSeconds: 5
        failureThreshold: 30
      command:
        - /bin/sh
        - -c
        - |
          mkdir -p /workspace
          if [ ! -b /dev/workspace ]; then
            echo 'voie-fabricd: workspace block device is missing' >&2
            exit 1
          fi
          if ! mount -t ext4 -o discard /dev/workspace /workspace; then
            echo 'voie-fabricd: workspace volume did not mount as ext4' >&2
            exit 1
          fi
          exec sleep 86400
      volumeDevices:
        - name: workspace
          devicePath: /dev/workspace
  volumes:
    - name: workspace
      persistentVolumeClaim:
        claimName: {pvc_name}
",
            ns = self.namespace,
            runtime = self.runtime_class,
            node = self.node_name,
            image = self.workspace_image,
            sa = WORKSPACE_SERVICE_ACCOUNT_NAME,
        )
    }
}

async fn copy_pod_file(
    live: &Live,
    pod: &str,
    container: &str,
    remote: &str,
    local: &Path,
    from_guest: bool,
) -> Result<(), FabricError> {
    if !valid_guest_copy_path(remote) {
        return Err(FabricError::Config("guest copy path is invalid"));
    }
    if !valid_k8s_name(pod) || !valid_k8s_name(container) {
        return Err(FabricError::Config("guest copy identity is invalid"));
    }
    if from_guest {
        copy_from_guest_exec(live, pod, container, remote, local).await
    } else {
        copy_to_guest_exec(live, pod, container, remote, local).await
    }
}

fn valid_guest_copy_path(remote: &str) -> bool {
    remote.starts_with('/')
        && remote.len() <= 512
        && !remote.contains("..")
        && !remote.contains("//")
        && remote
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
}

async fn copy_from_guest_exec(
    live: &Live,
    pod: &str,
    container: &str,
    remote: &str,
    local: &Path,
) -> Result<(), FabricError> {
    live.exec_guest_stdout_file(
        pod,
        container,
        &["/bin/cat", "--", remote],
        local,
        crate::storage::PRODUCT_VOLUME_IO_TIMEOUT_MS,
    )
    .await
    .and_then(guest_copy_settled)
}

/// Streams a host file into the guest. Database dumps are 8–32 GiB and
/// must not be loaded as `Vec<u8>`.
async fn copy_to_guest_exec(
    live: &Live,
    pod: &str,
    container: &str,
    remote: &str,
    local: &Path,
) -> Result<(), FabricError> {
    live.exec_guest_stdin_file(
        pod,
        container,
        &[
            "/bin/busybox",
            "sh",
            "-c",
            "cat > \"$1\"",
            "voie-copy",
            remote,
        ],
        local,
        crate::storage::PRODUCT_VOLUME_IO_TIMEOUT_MS,
    )
    .await
    .and_then(guest_copy_settled)
}

fn guest_copy_settled(output: ExecOutput) -> Result<(), FabricError> {
    if output.ambiguous {
        Err(FabricError::Unknown("guest copy did not settle".into()))
    } else if output.exit_code != 0 {
        Err(FabricError::Unknown(format!(
            "guest copy: {}",
            output.stderr
        )))
    } else {
        Ok(())
    }
}

fn valid_k8s_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn opaque_secret_yaml(
    namespace: &str,
    name: &str,
    extra_labels: &[(&str, &str)],
    pairs: &[(&str, &[u8])],
) -> Result<String, FabricError> {
    if !valid_k8s_name(name) {
        return Err(FabricError::Config("secret identity is invalid"));
    }
    if pairs.is_empty() {
        return Err(FabricError::Config("secret value is empty"));
    }
    let mut labels = String::from("    io.voie/managed: \"true\"\n");
    for (key, value) in extra_labels {
        if !valid_secret_label(key) || !valid_secret_label(value) {
            return Err(FabricError::Config("secret identity is invalid"));
        }
        labels.push_str("    ");
        labels.push_str(key);
        labels.push_str(": \"");
        labels.push_str(value);
        labels.push_str("\"\n");
    }
    let mut data = String::new();
    for (key, value) in pairs {
        if !valid_secret_key(key) {
            return Err(FabricError::Config("secret identity is invalid"));
        }
        if value.is_empty() {
            return Err(FabricError::Config("secret value is empty"));
        }
        data.push_str("  ");
        data.push_str(key);
        data.push_str(": ");
        data.push_str(&BASE64.encode(value));
        data.push('\n');
    }
    Ok(format!(
        "apiVersion: v1
kind: Secret
metadata:
  name: {name}
  namespace: {namespace}
  labels:
{labels}type: Opaque
data:
{data}"
    ))
}

fn valid_secret_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && !value.contains('\n')
        && !value.contains('"')
        && !value.contains('\\')
}

fn valid_secret_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 63
        && key.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.'
        })
}

pub(crate) struct CmdOut {
    pub(crate) status: i32,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

async fn run(mut command: Command) -> Result<CmdOut, FabricError> {
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let output = command
        .output()
        .await
        .map_err(|error| FabricError::Realize(format!("spawn: {error}")))?;
    Ok(CmdOut {
        status: output.status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn is_not_found(out: &CmdOut) -> bool {
    out.status != 0
        && (out.stderr.contains("NotFound")
            || out.stderr.contains("not found")
            || out.stderr.contains("(NotFound)"))
}

/// StorageClasses whose provisioner actually allocates. Product PVs bind an
/// already-carved linear LV through `kubernetes.io/no-provisioner`.
fn allocating_storage_classes(value: &Value) -> Vec<(String, String)> {
    let Some(items) = value.get("items").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut competing = Vec::new();
    for item in items {
        let name = item
            .get("metadata")
            .and_then(|metadata| metadata.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let provisioner = item
            .get("provisioner")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if provisioner != "kubernetes.io/no-provisioner" && !name.is_empty() {
            competing.push((name, provisioner));
        }
    }
    competing
}

/// Maps one RuntimeClass observation onto the pod-admission precondition.
/// Ok(true): present and selecting exactly the configured handler.
/// Ok(false): absent so far; the caller keeps waiting within its bound.
/// Foreign: present with a different handler — deployment state this
/// daemon does not own, which no amount of waiting converts into what
/// admission needs.
enum RuntimeClassObservation {
    /// The object exists; `handler` is what it selects (a stored
    /// RuntimeClass always declares one, so `None` means foreign junk).
    Present { handler: Option<String> },
    /// Positively absent per the API server: k3s auto-deploy has not
    /// delivered the manifest yet.
    Absent,
    /// The read itself failed (connection refused, request timeout,
    /// unusable response). That is never evidence about the object;
    /// within the readiness bound it is retryable absence, and the
    /// bounded reason travels into the final Unknown if nothing positive
    /// is ever observed.
    Unreadable(String),
}

fn classify_runtime_class(
    class: &str,
    want_handler: &str,
    observed: &RuntimeClassObservation,
) -> Result<bool, FabricError> {
    match observed {
        RuntimeClassObservation::Present {
            handler: Some(handler),
        } if handler == want_handler => Ok(true),
        RuntimeClassObservation::Present {
            handler: Some(handler),
        } => Err(FabricError::Foreign(format!(
            "RuntimeClass {class} selects handler {handler}, want {want_handler}"
        ))),
        RuntimeClassObservation::Present { handler: None } => Err(FabricError::Foreign(format!(
            "RuntimeClass {class} does not declare a handler, want {want_handler}"
        ))),
        // Absence and unreadable reads are both "not yet": the bound, not
        // this classification, decides when waiting stops.
        RuntimeClassObservation::Absent | RuntimeClassObservation::Unreadable(_) => Ok(false),
    }
}

fn owned_by(value: &Value, workspace_id: &str) -> bool {
    managed(value)
        && value
            .pointer("/metadata/labels/io.voie~1workspace")
            .and_then(Value::as_str)
            == Some(workspace_id)
}

/// True when the object carries this Fabric's managed label, independent of
/// any workspace ownership. Used for shared objects like the namespace-wide
/// NetworkPolicy that belong to the estate rather than to one workspace.
pub fn managed(value: &Value) -> bool {
    value
        .pointer("/metadata/labels")
        .and_then(|labels| labels.get("io.voie/managed"))
        .and_then(Value::as_str)
        == Some("true")
}

fn parse_pv(name: &str, value: &Value) -> PvInfo {
    let path = value
        .pointer("/spec/local/path")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let node = value
        .pointer("/spec/nodeAffinity/required/nodeSelectorTerms/0/matchExpressions/0/values/0")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let volume_mode = value
        .pointer("/spec/volumeMode")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let access_modes = value
        .pointer("/spec/accessModes")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let reclaim = value
        .pointer("/spec/persistentVolumeReclaimPolicy")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let storage_class = value
        .pointer("/spec/storageClassName")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let workspace_label = value
        .pointer("/metadata/labels/io.voie~1workspace")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let managed = value
        .pointer("/metadata/labels/io.voie~1managed")
        .and_then(Value::as_str)
        == Some("true");
    PvInfo {
        name: name.to_owned(),
        path,
        node,
        volume_mode,
        access_modes,
        reclaim,
        storage_class,
        workspace_label,
        managed,
    }
}

/// Recycled device-mapper nodes (`/dev/dm-N`) are not a persistent identity.
/// After reboot they name a different LV (or the thin-pool tdata).
pub fn ephemeral_devmapper_path(path: &str) -> bool {
    let path = path.trim();
    path == "/dev/dm" || path.starts_with("/dev/dm-")
}

pub fn require_stable_block_path(path: &str) -> Result<(), FabricError> {
    if ephemeral_devmapper_path(path) {
        return Err(FabricError::Realize(format!(
            "block path `{path}` is a recycled device-mapper node; persist the encrypted mapper"
        )));
    }
    Ok(())
}

pub fn encrypted_mapper_device(lv_name: &str) -> String {
    format!("/dev/mapper/voie-crypt-{lv_name}")
}

fn strip_runtime_metadata(value: &mut Value) {
    if let Some(meta) = value.get_mut("metadata").and_then(Value::as_object_mut) {
        for key in [
            "resourceVersion",
            "uid",
            "creationTimestamp",
            "generation",
            "managedFields",
        ] {
            meta.remove(key);
        }
    }
    if let Some(object) = value.as_object_mut() {
        object.remove("status");
    }
    if let Some(spec) = value.get_mut("spec").and_then(Value::as_object_mut) {
        spec.remove("claimRef");
    }
}

pub fn lv_name_for(workspace_id: &str) -> String {
    let compact: String = workspace_id.chars().filter(|ch| *ch != '-').collect();
    format!("ws{compact}")
}

pub fn lv_name_for_release(release_id: &str) -> String {
    let compact: String = release_id.chars().filter(|ch| *ch != '-').collect();
    format!("rel{compact}")
}

pub fn lv_name_for_postgres(database_id: &str) -> String {
    let compact: String = database_id.chars().filter(|ch| *ch != '-').collect();
    format!("pg{compact}")
}

pub fn lv_name_for_deployment(deployment_id: &str) -> String {
    let compact: String = deployment_id.chars().filter(|ch| *ch != '-').collect();
    format!("dep{compact}")
}

pub fn lv_name_for_restore(resource_id: &str) -> String {
    let compact: String = resource_id.chars().filter(|ch| *ch != '-').collect();
    format!("rst{compact}")
}

/// True for names this daemon mints: a product prefix plus the 32 hex
/// characters of a compacted UUID. Any other name in the pool is not ours.
pub fn is_daemon_lv_name(name: &str) -> bool {
    daemon_lv_prefix_len(name).is_some_and(|prefix| {
        name.len() == prefix + 32
            && name.as_bytes()[prefix..]
                .iter()
                .all(|byte| byte.is_ascii_hexdigit())
    })
}

fn daemon_lv_prefix_len(name: &str) -> Option<usize> {
    if name.starts_with("ws") || name.starts_with("pg") {
        Some(2)
    } else if name.starts_with("dep") || name.starts_with("rel") || name.starts_with("rst") {
        Some(3)
    } else {
        None
    }
}

fn parse_lvm_bytes(raw: &str) -> Result<u64, FabricError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(FabricError::Realize("LVM size was empty".into()));
    }
    let digits: String = trimmed
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    digits
        .parse()
        .map_err(|_| FabricError::Realize(format!("LVM size `{trimmed}` is not an integer")))
}

fn path_exists(path: &Path) -> Result<bool, FabricError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(FabricError::Unknown(format!(
            "cannot inspect {}: {error}",
            path.display()
        ))),
    }
}

/// True when at least one real jail directory exists under the jailer root.
/// The `.jailer-identities` side index is bookkeeping, not a jail; an index
/// alone never proves a guest runtime survived. An unreadable root is
/// indeterminate, never positive absence.
fn jailer_has_jails(root: &Path) -> Result<bool, FabricError> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(FabricError::Unknown(format!(
                "cannot inspect jailer root {}: {error}",
                root.display()
            )));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| {
            FabricError::Unknown(format!(
                "cannot enumerate jailer root {}: {error}",
                root.display()
            ))
        })?;
        if entry.file_name().to_string_lossy() != ".jailer-identities" {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Every live firecracker or jailer process on this host, regardless of
/// sandbox identity. This is the host-wide positive-presence check that
/// cleanup falls back to when no sandbox identity is known.
fn firecracker_pids() -> Result<Vec<u32>, FabricError> {
    let proc = std::fs::read_dir("/proc")
        .map_err(|error| FabricError::Unknown(format!("cannot inspect /proc: {error}")))?;
    let mut pids = Vec::new();
    for entry in proc {
        let entry = entry
            .map_err(|error| FabricError::Unknown(format!("cannot enumerate /proc: {error}")))?;
        let pid: u32 = match entry.file_name().to_string_lossy().parse() {
            Ok(pid) => pid,
            Err(_) => continue,
        };
        let cmdline = match std::fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(cmdline) => cmdline,
            // Processes may disappear between enumeration and inspection;
            // that race is a positive non-presence for that process.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(FabricError::Unknown(format!(
                    "cannot inspect /proc/{pid}/cmdline: {error}"
                )));
            }
        };
        if cmdline
            .windows(b"firecracker".len())
            .any(|w| w == b"firecracker")
            || cmdline.windows(b"jailer".len()).any(|w| w == b"jailer")
        {
            pids.push(pid);
        }
    }
    Ok(pids)
}

/// Kubernetes' own readiness verdict: the pod object reports a condition of
/// type `Ready` with status `True`. The generated Pod's mount-validating
/// readinessProbe makes this exactly "the workspace device exists and an
/// ext4 filesystem is live at `/workspace`".
pub fn ready_condition_true(pod: &Value) -> bool {
    pod.pointer("/status/conditions")
        .and_then(Value::as_array)
        .map(|conditions| {
            conditions.iter().any(|condition| {
                condition.get("type").and_then(Value::as_str) == Some("Ready")
                    && condition.get("status").and_then(Value::as_str) == Some("True")
            })
        })
        .unwrap_or(false)
}

pub fn object_names(workspace_id: &str, generation: i64) -> (String, String, String) {
    let pv = format!("voie-ws-{workspace_id}");
    let pvc = pv.clone();
    let pod = format!("voie-ws-{workspace_id}-e{generation}");
    (pv, pvc, pod)
}

/// Candidate restore PV/PVC/Pod names. Live create keeps generation-free
/// PV/PVC so replace can remount the same volume; restore must boot a
/// second generation beside the old one, so those objects cannot share
/// the live names.
pub fn restore_object_names(workspace_id: &str, generation: i64) -> (String, String, String) {
    let stem = format!("voie-ws-{workspace_id}-e{generation}");
    (stem.clone(), stem.clone(), stem)
}

fn firecracker_for_sandbox(sandbox_id: &str) -> Result<Vec<u32>, FabricError> {
    let proc = std::fs::read_dir("/proc")
        .map_err(|error| FabricError::Unknown(format!("cannot inspect /proc: {error}")))?;
    let mut pids = Vec::new();
    for entry in proc {
        let entry = entry
            .map_err(|error| FabricError::Unknown(format!("cannot enumerate /proc: {error}")))?;
        let pid: u32 = match entry.file_name().to_string_lossy().parse() {
            Ok(pid) => pid,
            Err(_) => continue,
        };
        let cmdline = match std::fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(cmdline) => cmdline,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(FabricError::Unknown(format!(
                    "cannot inspect /proc/{pid}/cmdline: {error}"
                )));
            }
        };
        if cmdline
            .windows(sandbox_id.len())
            .any(|w| w == sandbox_id.as_bytes())
            && (cmdline
                .windows(b"firecracker".len())
                .any(|w| w == b"firecracker")
                || cmdline.windows(b"jailer".len()).any(|w| w == b"jailer"))
        {
            pids.push(pid);
        }
    }
    Ok(pids)
}

fn has_children(pid: u32) -> Result<bool, FabricError> {
    let proc = std::fs::read_dir("/proc")
        .map_err(|error| FabricError::Unknown(format!("cannot inspect /proc: {error}")))?;
    for entry in proc {
        let entry = entry
            .map_err(|error| FabricError::Unknown(format!("cannot enumerate /proc: {error}")))?;
        let child: u32 = match entry.file_name().to_string_lossy().parse() {
            Ok(child) => child,
            Err(_) => continue,
        };
        if child == pid {
            continue;
        }
        let stat = match std::fs::read_to_string(format!("/proc/{child}/stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(FabricError::Unknown(format!(
                    "cannot inspect /proc/{child}/stat: {error}"
                )));
            }
        };
        if let Some(ppid) = parse_ppid(&stat) {
            if ppid == pid {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn any_children(pids: &[u32]) -> Result<bool, FabricError> {
    for &pid in pids {
        if has_children(pid)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn parse_ppid(stat: &str) -> Option<u32> {
    let close = stat.rfind(')')?;
    let rest = stat.get(close + 2..)?;
    rest.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use std::path::PathBuf;

    #[test]
    fn readiness_requires_explicit_true_ready_condition() {
        let ready_json: Value = serde_json::from_str(
            r#"{"status":{"phase":"Running","conditions":[
                {"type":"Initialized","status":"True"},
                {"type":"Ready","status":"True"}]}}"#,
        )
        .unwrap();
        assert!(ready_condition_true(&ready_json));

        let not_ready_json: Value = serde_json::from_str(
            r#"{"status":{"phase":"Running","conditions":[
                {"type":"Ready","status":"False","reason":"ContainersNotReady"}]}}"#,
        )
        .unwrap();
        assert!(!ready_condition_true(&not_ready_json));

        // Phase Running alone is never readiness.
        let running_no_conditions: Value =
            serde_json::from_str(r#"{"status":{"phase":"Running"}}"#).unwrap();
        assert!(!ready_condition_true(&running_no_conditions));
    }

    #[test]
    fn database_endpoints_prove_exactly_the_candidate() {
        let candidate = "pg-rst-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let old = "pg-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let exact: Value = serde_json::from_str(&format!(
            r#"{{"subsets":[{{"addresses":[{{"targetRef":{{"name":"{candidate}"}}}}]}}]}}"#
        ))
        .unwrap();
        assert!(Live::endpoints_are_exactly(
            &Live::endpoint_pod_names(&exact),
            candidate
        ));

        let empty: Value = serde_json::from_str(r#"{"subsets":[]}"#).unwrap();
        assert!(!Live::endpoints_are_exactly(
            &Live::endpoint_pod_names(&empty),
            candidate
        ));

        let old_only: Value = serde_json::from_str(&format!(
            r#"{{"subsets":[{{"addresses":[{{"targetRef":{{"name":"{old}"}}}}]}}]}}"#
        ))
        .unwrap();
        assert!(!Live::endpoints_are_exactly(
            &Live::endpoint_pod_names(&old_only),
            candidate
        ));

        let split: Value = serde_json::from_str(&format!(
            r#"{{"subsets":[{{"addresses":[
                {{"targetRef":{{"name":"{old}"}}}},
                {{"targetRef":{{"name":"{candidate}"}}}}
            ]}}]}}"#
        ))
        .unwrap();
        assert!(!Live::endpoints_are_exactly(
            &Live::endpoint_pod_names(&split),
            candidate
        ));
    }

    #[test]
    fn guest_copy_paths_are_absolute_and_refuse_shell_metacharacters() {
        assert!(valid_guest_copy_path(
            "/workspace/.voie/tmp/release.tar.zst"
        ));
        assert!(valid_guest_copy_path("/tmp/voie-backup.dump"));
        assert!(!valid_guest_copy_path("workspace/release.tar.zst"));
        assert!(!valid_guest_copy_path("/tmp/../etc/passwd"));
        assert!(!valid_guest_copy_path("/tmp/voie-backup.dump;id"));
        assert!(!valid_guest_copy_path("/tmp/foo bar"));
        assert!(!valid_guest_copy_path("/tmp//dump"));
    }

    #[test]
    fn default_local_path_is_a_competing_allocator() {
        let json: Value = serde_json::from_str(
            r#"{
              "items": [
                {
                  "metadata": {
                    "name": "local-path",
                    "annotations": {
                      "storageclass.kubernetes.io/is-default-class": "true"
                    }
                  },
                  "provisioner": "rancher.io/local-path"
                },
                {
                  "metadata": {"name": "voie-workspace"},
                  "provisioner": "kubernetes.io/no-provisioner"
                }
              ]
            }"#,
        )
        .unwrap();
        assert_eq!(
            allocating_storage_classes(&json),
            vec![("local-path".into(), "rancher.io/local-path".into())]
        );
    }

    #[test]
    fn leftover_local_path_is_a_competing_allocator_even_when_not_default() {
        let json: Value = serde_json::from_str(
            r#"{
              "items": [
                {
                  "metadata": {"name": "local-path"},
                  "provisioner": "rancher.io/local-path"
                },
                {
                  "metadata": {"name": "voie-workspace"},
                  "provisioner": "kubernetes.io/no-provisioner"
                }
              ]
            }"#,
        )
        .unwrap();
        assert_eq!(
            allocating_storage_classes(&json),
            vec![("local-path".into(), "rancher.io/local-path".into())]
        );
    }

    #[test]
    fn no_provisioner_default_is_not_a_competing_allocator() {
        let json: Value = serde_json::from_str(
            r#"{
              "items": [
                {
                  "metadata": {
                    "name": "voie-workspace",
                    "annotations": {
                      "storageclass.kubernetes.io/is-default-class": "true"
                    }
                  },
                  "provisioner": "kubernetes.io/no-provisioner"
                }
              ]
            }"#,
        )
        .unwrap();
        assert!(allocating_storage_classes(&json).is_empty());
    }

    /// Minimal offline configuration; mirrors the lib test helper so these
    /// manifest renders never depend on host environment.
    fn render_config(tag: &str) -> Config {
        Config {
            bind: "[IP_ADDRESS]:0".into(),
            sqlite: std::env::temp_dir().join(format!("voie-fabricd-realize-{tag}.sqlite")),
            node_name: "node-under-test".into(),
            namespace: "voie-workspace".into(),
            storage_class: "voie-workspace-block".into(),
            runtime_class: "voie-firecracker".into(),
            runtime_handler: "kata-fc-rs-voie".into(),
            runner_image: "voie-runner:c1".into(),
            jailer_root: std::env::temp_dir().join(format!("voie-fabricd-jails-{tag}")),
            vg: "voie-ws".into(),
            storage: crate::StoragePolicy::test(),
            residue_wait_secs: 120,
            runtime_class_wait_secs: 60,
            kubectl_program: "kubectl".into(),
            kubectl_prefix: vec![],
            kubeconfig: None,
            crictl_program: "crictl".into(),
            crictl_prefix: vec![],
            tls_cert: PathBuf::from("/dev/null"),
            tls_key: PathBuf::from("/dev/null"),
            tls_ca: PathBuf::from("/dev/null"),
            approved_egress: None,
            client_sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .into(),
        }
    }

    #[test]
    fn service_account_renders_managed_permissionless_and_tokenless() {
        let live = Live::from_config(&render_config("sa-render")).unwrap();
        let yaml = live.service_account_yaml();
        // The account name is part of the admission contract with the pod
        // manifest below; it is asserted here to pin it against silent drift.
        assert_eq!(WORKSPACE_SERVICE_ACCOUNT_NAME, "voie-guest");
        assert!(yaml.contains("kind: ServiceAccount\n"), "{yaml}");
        assert!(
            yaml.contains(&format!("  name: {WORKSPACE_SERVICE_ACCOUNT_NAME}\n")),
            "{yaml}"
        );
        assert!(yaml.contains("  namespace: voie-workspace\n"), "{yaml}");
        assert!(yaml.contains("    io.voie/managed: \"true\"\n"), "{yaml}");
        // The account itself must never hand out a token, and it carries no
        // secrets, image-pull secrets, or RBAC references of any kind.
        assert!(
            yaml.contains("automountServiceAccountToken: false\n"),
            "{yaml}"
        );
        for absent in ["secrets:", "imagePullSecrets:", "role", "Role"] {
            assert!(!yaml.contains(absent), "unexpected {absent} in: {yaml}");
        }
    }

    #[test]
    fn pod_admits_under_dedicated_account_with_token_and_links_suppressed() {
        let live = Live::from_config(&render_config("pod-identity")).unwrap();
        let yaml = live.pod_yaml(
            "ws-under-test",
            "voie-ws-ws-under-test-e2",
            "voie-ws-ws-under-test",
            2,
        );
        let sa_line = format!("  serviceAccountName: {WORKSPACE_SERVICE_ACCOUNT_NAME}\n");
        assert!(yaml.contains(&sa_line), "{yaml}");
        // Exactly once, at pod-spec level: never duplicated onto containers.
        assert_eq!(yaml.matches("serviceAccountName").count(), 1, "{yaml}");
        assert_eq!(
            yaml.matches("automountServiceAccountToken: false").count(),
            1,
            "{yaml}"
        );
        assert_eq!(
            yaml.matches("enableServiceLinks: false").count(),
            1,
            "{yaml}"
        );
        // Identity is declared inside the pod spec and before admission
        // reaches container definitions.
        let spec = yaml.find("\nspec:\n").expect("pod manifest has a spec");
        let sa = yaml.find(&sa_line).expect("checked above");
        let containers = yaml.find("  containers:\n").expect("pod has containers");
        assert!(spec < sa && sa < containers, "{yaml}");
        // No implicit fallback identity anywhere in the manifest.
        assert!(!yaml.contains("default"), "{yaml}");
        assert!(yaml.contains("io.voie/kind: \"workspace\""), "{yaml}");
    }

    #[test]
    fn opaque_secret_carries_identity_labels_and_no_plaintext() {
        let yaml = opaque_secret_yaml(
            "voie-workspace",
            "voie-pgcred-abc",
            &[
                ("io.voie/kind", "postgres"),
                ("io.voie/database", "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"),
                ("io.voie/slug", "invoice-demo"),
            ],
            &[("postgres-password", b"secret-bytes")],
        )
        .expect("renders");
        assert!(yaml.contains("io.voie/managed: \"true\""), "{yaml}");
        assert!(yaml.contains("io.voie/kind: \"postgres\""), "{yaml}");
        assert!(yaml.contains("io.voie/database:"), "{yaml}");
        assert!(yaml.contains("io.voie/slug: \"invoice-demo\""), "{yaml}");
        assert!(!yaml.contains("secret-bytes"), "{yaml}");
        assert!(!yaml.contains("postgres://"), "{yaml}");
    }

    #[test]
    fn blkid_formats_only_after_positive_no_signature() {
        assert!(matches!(classify_blkid(2, ""), Ok(BlkidFs::None)));
        assert!(matches!(classify_blkid(0, "ext4\n"), Ok(BlkidFs::Ext4)));
        assert!(classify_blkid(0, "xfs").is_err());
        assert!(classify_blkid(0, "").is_err());
        assert!(classify_blkid(1, "").is_err());
        assert!(classify_blkid(4, "").is_err());
        assert!(classify_blkid(2, "ext4").is_err());
    }

    #[test]
    fn cryptsetup_close_retains_key_on_unknown_failure() {
        assert!(classify_cryptsetup_close(0, "").is_ok());
        assert!(classify_cryptsetup_close(1, "Device voie-crypt-ws is not active").is_ok());
        assert!(classify_cryptsetup_close(1, "No such device").is_ok());
        assert!(classify_cryptsetup_close(1, "device-mapper: remove ioctl failed").is_err());
        assert!(classify_cryptsetup_close(1, "").is_err());
    }

    #[test]
    fn recycled_devmapper_nodes_are_not_stable_block_paths() {
        assert!(ephemeral_devmapper_path("/dev/dm-4"));
        assert!(ephemeral_devmapper_path("/dev/dm-0"));
        assert!(ephemeral_devmapper_path(" /dev/dm-20 "));
        assert!(!ephemeral_devmapper_path("/dev/mapper/voie-crypt-wsabc"));
        assert!(!ephemeral_devmapper_path("/dev/voie-ws/wsabc"));
        assert!(require_stable_block_path("/dev/dm-4").is_err());
        assert!(require_stable_block_path("/dev/mapper/voie-crypt-wsabc").is_ok());
        assert_eq!(
            encrypted_mapper_device("rstadd02a4281b44853b7502c6ede1341ab"),
            "/dev/mapper/voie-crypt-rstadd02a4281b44853b7502c6ede1341ab"
        );
    }

    #[test]
    fn verify_pv_rejects_recycled_dm_n_path() {
        let live = Live::from_config(&render_config("pv-ephemeral")).unwrap();
        let pv = PvInfo {
            name: "voie-pgdata-rst-add02a4281b44853b7502c6ede1341ab".into(),
            path: "/dev/dm-4".into(),
            node: "node-under-test".into(),
            volume_mode: "Block".into(),
            access_modes: vec!["ReadWriteOnce".into()],
            reclaim: "Retain".into(),
            storage_class: "voie-workspace-block".into(),
            workspace_label: Some("ws-under-test".into()),
            managed: true,
        };
        let err = live
            .verify_pv(
                &pv,
                "ws-under-test",
                "/dev/mapper/voie-crypt-rstadd02a4281b44853b7502c6ede1341ab",
            )
            .unwrap_err();
        assert!(
            err.to_string().contains("recycled device-mapper"),
            "{err}"
        );
    }

    #[test]
    fn restore_candidate_names_do_not_collide_with_live_pv() {
        let id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let live = object_names(id, 1);
        let candidate = restore_object_names(id, 2);
        assert_ne!(live.0, candidate.0);
        assert_ne!(live.1, candidate.1);
        assert_ne!(live.2, candidate.2);
        assert_eq!(candidate.0, candidate.1);
        assert_eq!(candidate.0, candidate.2);
    }
}
