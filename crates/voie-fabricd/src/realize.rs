//! Local block slot and Kubernetes realization for one Fabric host.
//!
//! Commands run only against LVM, the declared block device, and `kubectl`.
//! User shell text is never executed on this host.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::sleep;

use crate::FabricError;
use crate::fabric::bounded_text;

/// Filesystem label stamped onto every VOIE-formatted workspace device.
const MKFS_LABEL_PREFIX: &str = "voie-ws";

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockSlot {
    pub device: String,
    pub lv_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PodInfo {
    pub name: String,
    pub uid: String,
    pub sandbox_id: Option<String>,
    pub runtime_class: String,
    pub phase: String,
    pub ready: bool,
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
    jailer_root: PathBuf,
    vg: String,
    lv_size: String,
    residue_wait_secs: u64,
    runtime_class_wait_secs: u64,
    approved_egress: Option<ApprovedEgress>,
    kubectl_program: String,
    kubectl_prefix: Vec<String>,
    kubeconfig: Option<PathBuf>,
    crictl_program: String,
    crictl_prefix: Vec<String>,
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
            jailer_root: config.jailer_root.clone(),
            vg: config.vg.clone(),
            lv_size: config.lv_size.clone(),
            residue_wait_secs: config.residue_wait_secs,
            runtime_class_wait_secs: config.runtime_class_wait_secs,
            approved_egress: config.approved_egress.clone(),
            kubectl_program: config.kubectl_program.clone(),
            kubectl_prefix: config.kubectl_prefix.clone(),
            kubeconfig: config.kubeconfig.clone(),
            crictl_program: config.crictl_program.clone(),
            crictl_prefix: config.crictl_prefix.clone(),
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

    pub fn jailer_root(&self) -> &Path {
        &self.jailer_root
    }

    pub fn vg_name(&self) -> &str {
        &self.vg
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

    async fn crictl(&self, args: &[&str]) -> Result<CmdOut, FabricError> {
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

    /// Carves this workspace's logical volume out of the declared pool.
    ///
    /// Workspace bytes may only live in the declared local linear-LV pool;
    /// there is deliberately no file- or loop-backed escape hatch because
    /// such a device hides whether durability is real.
    pub async fn prepare_block(&self, workspace_id: &str) -> Result<BlockSlot, FabricError> {
        let lv_name = lv_name_for(workspace_id);
        let mapper = format!("/dev/{}/{}", self.vg, lv_name);
        let exists = self
            .host("lvs", &[&format!("{}/{}", self.vg, lv_name)])
            .await?;
        if exists.status != 0 {
            let created = self
                .host(
                    "lvcreate",
                    &["-y", "-L", &self.lv_size, "-n", &lv_name, &self.vg],
                )
                .await?;
            if created.status != 0 {
                return Err(FabricError::Realize(format!(
                    "lvcreate failed: {}",
                    created.stderr.trim()
                )));
            }
        }
        let device = self.canonical_device(&mapper).await?;
        if !Path::new(&device).exists() {
            return Err(FabricError::Realize(format!(
                "reserved logical volume `{device}` is absent"
            )));
        }
        Ok(BlockSlot {
            device,
            lv_name: Some(lv_name),
        })
    }

    pub async fn mkfs_ext4_if_needed(&self, device: &str) -> Result<(), FabricError> {
        let probed = self
            .host("blkid", &["-o", "value", "-s", "TYPE", device])
            .await?;
        let fs = probed.stdout.trim().to_owned();
        if fs == "ext4" {
            let labeled = self
                .host("blkid", &["-o", "value", "-s", "LABEL", device])
                .await?;
            let label = labeled.stdout.trim();
            // An ext4 filesystem that carries some other identity belongs to
            // someone else; VOIE never reformats foreign bytes.
            if !label.is_empty() && !label.starts_with("voie-ws") {
                return Err(FabricError::Foreign(format!(
                    "block device `{device}` carries foreign ext4 label `{label}`"
                )));
            }
            return Ok(());
        }
        if !fs.is_empty() {
            return Err(FabricError::Foreign(format!(
                "block device `{device}` has foreign filesystem `{fs}`"
            )));
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
        .await
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
        if pv.path != device {
            return Err(FabricError::Realize(format!(
                "PV {} path {} does not match reserved device {device}",
                pv.name, pv.path
            )));
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
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(pod) = self.get_pod(name).await? {
                if pod.phase == "Running" && pod.ready && pod.uid != "" {
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
                return Err(FabricError::Unknown(format!(
                    "pod {name} did not become Ready"
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
        }))
    }

    async fn lookup_sandbox(&self, pod_name: &str) -> Result<Option<String>, FabricError> {
        let out = self
            .crictl(&["pods", "--name", pod_name, "-q", "-n", &self.namespace])
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
        let timeout = format!("{timeout_secs}s");
        let mut args = vec![
            "delete",
            kind,
            name,
            "--ignore-not-found",
            "--wait=true",
            "--timeout",
            timeout.as_str(),
        ];
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
            .crictl(&["pods", "--name", pod_name, "-q", "-n", &self.namespace])
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

    /// The desired guest isolation policy: default-deny ingress AND egress
    /// for every pod in the namespace, except DNS towards kube-system and,
    /// when deployment approved them, exactly the configured destination
    /// CIDRs over one TCP port. Guests can therefore not reach the
    /// Kubernetes API or any cloud metadata service, and nothing unsolicited
    /// can reach the guest.
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
  podSelector: {{}}
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
            "podSelector": {},
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

    pub fn pv_yaml(&self, workspace_id: &str, pv_name: &str, device: &str) -> String {
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
            size = self.lv_size,
        )
    }

    pub fn pvc_yaml(&self, workspace_id: &str, pvc_name: &str, pv_name: &str) -> String {
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
            size = self.lv_size,
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
          mount -t ext4 /dev/workspace /workspace
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
            image = self.runner_image,
            sa = WORKSPACE_SERVICE_ACCOUNT_NAME,
        )
    }
}

struct CmdOut {
    status: i32,
    stdout: String,
    stderr: String,
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

pub fn lv_name_for(workspace_id: &str) -> String {
    let compact: String = workspace_id.chars().filter(|ch| *ch != '-').collect();
    format!("ws{compact}")
}

/// True only for names this daemon mints: `ws` plus the 32 hex characters
/// of a compacted UUID. Any other name in the pool is not ours to touch.
pub fn is_daemon_lv_name(name: &str) -> bool {
    name.len() == 34
        && name.as_bytes()[..2] == *b"ws"
        && name.as_bytes()[2..]
            .iter()
            .all(|byte| byte.is_ascii_hexdigit())
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
            lv_size: "1G".into(),
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
    }
}
