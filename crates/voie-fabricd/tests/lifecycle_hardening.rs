//! Focused contract tests for fabricd lifecycle hardening.
//!
//! Covered invariants, each externally meaningful:
//! - startup reconciliation releases an orphaned reservation only on
//!   positive absence of every realization surface and keeps it held
//!   otherwise; unclaimed daemon-minted LVs are removed while claimed ones
//!   survive; a retired product thin pool (`workspaces`/`ws-root`) fails
//!   closed before any LV is removed; a volume group without the `runtime`
//!   thin pool fails closed; leftover ready rows whose LV is gone are
//!   listed and never reminted;
//! - cleanup without a sandbox identity stays unknown/held unless absence
//!   is positively observable host-wide;
//! - the desired NetworkPolicy denies ingress by default and constrains
//!   egress to DNS plus deployment-approved CIDR/TCP-port entries;
//! - the mTLS surface accepts exactly the pinned control client identity
//!   and closes any other CA-signed client;
//! - SIGTERM-equivalent shutdown stops admission and drains in-flight work.
//!
//! Host tools that `Live` invokes directly (`findmnt`, `lvs`, `lvremove`)
//! are resolved via PATH, so their fakes are staged on a private PATH under
//! a process-wide lock; kubectl/crictl are configurable fields and need no
//! environment games.

use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use sha2::Digest;
use tokio::sync::Notify;
use voie_fabricd::{
    client_identity_matches, serve_tls, ApprovedEgress, Config, Fabric, GenerationRow, Live, Store,
    VolumeKind, WorkspaceRow,
};

// ---------------------------------------------------------------------------
// Fake host tools (PATH-scoped) and fake kubectl/crictl (config-scoped)
// ---------------------------------------------------------------------------

/// All tests touching PATH-faked host tools serialize on this lock; parallel
/// mutation of the process environment is otherwise undefined.
static HOST_TOOLS_LOCK: Mutex<()> = Mutex::new(());

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "voie-fabricd-lifecycle-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn write_executable(dir: &Path, name: &str, body: &str) -> PathBuf {
    let program = dir.join(name);
    std::fs::write(&program, body).expect("write fake program");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    program
}

struct HostTools {
    _lock: std::sync::MutexGuard<'static, ()>,
    lvremove_capture: PathBuf,
    previous_path: std::ffi::OsString,
}

impl Drop for HostTools {
    fn drop(&mut self) {
        // Safe under HOST_TOOLS_LOCK: no other test thread reads PATH now.
        unsafe { std::env::set_var("PATH", &self.previous_path) };
    }
}

/// Installs fake `findmnt`, `lvs`, `lvremove`, `lvchange`, and `cryptsetup`
/// until the guard drops. `lvs` prints `runtime` plus `lv_lines`, `findmnt`
/// always reports "not mounted", `cryptsetup close` and `lvchange` are
/// no-ops, and `lvremove` records its argv into the returned capture file
/// before exiting 0.
fn with_fake_host_tools(tag: &str, lv_lines: &[&str]) -> HostTools {
    let mut names = Vec::with_capacity(lv_lines.len() + 1);
    names.push("runtime");
    names.push("workspace");
    names.extend_from_slice(lv_lines);
    fake_host_tools(tag, &names)
}

fn with_exact_lv_names(tag: &str, lv_lines: &[&str]) -> HostTools {
    fake_host_tools(tag, lv_lines)
}

fn fake_host_tools(tag: &str, lv_lines: &[&str]) -> HostTools {
    let lock = HOST_TOOLS_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let bin = temp_dir(&format!("bin-{tag}"));
    write_executable(&bin, "findmnt", "#!/bin/sh\nexit 1\n");
    write_executable(&bin, "cryptsetup", "#!/bin/sh\nexit 0\n");
    write_executable(&bin, "lvchange", "#!/bin/sh\nexit 0\n");
    // Integration tests link the library without `cfg(test)`, so production
    // stage-mode fail-closed applies. Directory staging is the explicit
    // development mode; these tests never mount a real LV.
    unsafe { std::env::set_var("VOIE_FABRICD_STAGE_MODE", "dev-directory") };
    let mut lvs_script = String::from("#!/bin/sh\n");
    for line in lv_lines {
        lvs_script.push_str(&format!("echo '{line}'\n"));
    }
    write_executable(&bin, "lvs", &lvs_script);
    let lvremove_capture = bin.join("lvremove.bin");
    write_executable(
        &bin,
        "lvremove",
        &format!(
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > '{}'\nexit 0\n",
            lvremove_capture.display()
        ),
    );
    let previous_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path = bin.as_os_str().to_owned();
    path.push(":");
    path.push(&previous_path);
    unsafe { std::env::set_var("PATH", &path) };
    HostTools {
        _lock: lock,
        lvremove_capture,
        previous_path,
    }
}

fn captured_args(capture: &Path) -> Vec<String> {
    let bytes = std::fs::read(capture).unwrap_or_default();
    bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

/// A fake kubectl whose `get` answers NotFound for every object; deletes are
/// successful no-ops. This models "positively absent everywhere".
fn kubectl_notfound(dir: &Path) -> String {
    write_executable(
        dir,
        "kubectl-notfound",
        r#"#!/bin/sh
if [ "$1" = "get" ]; then
  echo "Error from server (NotFound)" >&2
  exit 1
fi
exit 0
"#,
    )
    .to_string_lossy()
    .into_owned()
}

/// A fake kubectl where `get pv NAME` returns the given JSON; everything
/// else is NotFound. Models a realization surface that still exists.
fn kubectl_pv_present(dir: &Path, json: &str) -> String {
    write_executable(
        dir,
        "kubectl-pv-present",
        &format!(
            r#"#!/bin/sh
if [ "$1" = "get" ] && [ "$2" = "pv" ]; then
  cat <<'JSON'
{json}
JSON
  exit 0
fi
if [ "$1" = "get" ]; then
  echo "Error from server (NotFound)" >&2
  exit 1
fi
exit 0
"#
        ),
    )
    .to_string_lossy()
    .into_owned()
}

/// A fake crictl that knows no sandbox for any pod.
fn crictl_empty(dir: &Path) -> String {
    write_executable(dir, "crictl-empty", "#!/bin/sh\nexit 0\n")
        .to_string_lossy()
        .into_owned()
}

/// A fake crictl whose first `pods` listing still names a leftover
/// NotReady sandbox, then reports empty. Models the Kata Firecracker
/// sandbox that lingers after kubectl has already deleted the pod.
fn crictl_sandbox_then_empty(dir: &Path) -> String {
    let nfile = dir.join("crictl-n");
    std::fs::write(&nfile, "0").expect("crictl counter");
    write_executable(
        dir,
        "crictl-sandbox-then-empty",
        &format!(
            r#"#!/bin/sh
nfile='{nfile}'
n=$(cat "$nfile")
n=$((n+1))
printf '%s' "$n" > "$nfile"
if [ "$n" -le 2 ]; then
  echo leftover-sandbox
fi
exit 0
"#,
            nfile = nfile.display()
        ),
    )
    .to_string_lossy()
    .into_owned()
}

fn config_with(tag: &str, kubectl: &str, crictl: &str, jailer_root: PathBuf) -> Config {
    Config {
        bind: "127.0.0.1:0".into(),
        sqlite: temp_dir(&format!("sqlite-{tag}")).join("state.sqlite"),
        node_name: "node-under-test".into(),
        namespace: "voie-workspace".into(),
        storage_class: "voie-workspace-block".into(),
        runtime_class: "voie-firecracker".into(),
        runtime_handler: "kata-fc-rs-voie".into(),
        runner_image: "voie-runner:c1".into(),
        jailer_root,
        vg: "voie-ws".into(),
        storage: voie_fabricd::StoragePolicy::test(),
        residue_wait_secs: 1,
        runtime_class_wait_secs: 1,
        kubectl_program: kubectl.to_owned(),
        kubectl_prefix: vec![],
        kubeconfig: None,
        crictl_program: crictl.to_owned(),
        crictl_prefix: vec![],
        tls_cert: PathBuf::from("/dev/null"),
        tls_key: PathBuf::from("/dev/null"),
        tls_ca: PathBuf::from("/dev/null"),
        approved_egress: None,
        client_sha256: PINNED_IDENTITY_SHA.into(),
    }
}

const WORKSPACE: &str = "11111111-2222-3333-4444-555555555555";

/// Placeholder identity pin for configs whose transport is never served.
const PINNED_IDENTITY_SHA: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn compact_lv(workspace_id: &str) -> String {
    format!("ws{}", workspace_id.replace('-', ""))
}

// ---------------------------------------------------------------------------
// Startup reconciliation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn orphaned_reservation_is_released_only_on_positive_absence() {
    let tools = with_fake_host_tools("release", &[]);
    let dir = temp_dir("reconcile-release");
    let config = config_with(
        "release",
        &kubectl_notfound(&dir),
        &crictl_empty(&dir),
        dir.join("jails"),
    );
    {
        let store = Store::open(&config.sqlite).unwrap();
        store
            .reserve_volume(
                WORKSPACE,
                "/dev/voie-ws/orphan",
                "node-under-test",
                "voie-ws-orphan",
            )
            .unwrap();
    }

    let fabric = Fabric::open(config.clone(), Live::from_config(&config).unwrap()).unwrap();
    let report = fabric.reconcile_startup().await.unwrap();

    assert_eq!(report.orphan_reservations_released, vec![WORKSPACE]);
    assert!(
        report.orphan_reservations_held.is_empty(),
        "{:?}",
        report.orphan_reservations_held
    );

    // The prepared LV was removed first, then the reservation released
    // with an explicit reason.
    let args = captured_args(&tools.lvremove_capture);
    assert!(
        args.contains(&format!("voie-ws/{}", compact_lv(WORKSPACE))),
        "lvremove argv must target the orphan's slot: {args:?}"
    );
    let store = Store::open(&config.sqlite).unwrap();
    let row = store.get_reservation(WORKSPACE).unwrap().unwrap();
    assert_eq!(row.state, "released");
}

#[tokio::test]
async fn orphaned_reservation_stays_held_when_a_surface_is_present() {
    let tools = with_fake_host_tools("held", &[]);
    let dir = temp_dir("reconcile-held");
    let config = config_with(
        "held",
        &kubectl_notfound(&dir),
        &crictl_empty(&dir),
        dir.join("jails"),
    );
    {
        let store = Store::open(&config.sqlite).unwrap();
        store
            .reserve_volume(
                WORKSPACE,
                "/dev/voie-ws/orphan",
                "node-under-test",
                "voie-ws-orphan",
            )
            .unwrap();
    }

    // The PV still exists: unknown fate of bytes means held, and no
    // destructive action may be attempted.
    let mut held_config = config_with(
        "held-pv",
        &kubectl_pv_present(&dir, r#"{"metadata":{"name":"voie-ws-x"}}"#),
        &crictl_empty(&dir),
        dir.join("jails"),
    );
    held_config.sqlite = config.sqlite.clone();
    let fabric = Fabric::open(
        held_config.clone(),
        Live::from_config(&held_config).unwrap(),
    )
    .unwrap();
    let report = fabric.reconcile_startup().await.unwrap();

    assert_eq!(report.orphan_reservations_held, vec![WORKSPACE]);
    assert!(report.orphan_reservations_released.is_empty());
    let args = captured_args(&tools.lvremove_capture);
    assert!(
        args.is_empty(),
        "held reservations must not remove LVs: {args:?}"
    );
    let store = Store::open(&held_config.sqlite).unwrap();
    let row = store.get_reservation(WORKSPACE).unwrap().unwrap();
    assert_eq!(row.state, "reserved");
}

#[tokio::test]
async fn unclaimed_daemon_lvs_are_removed_and_claimed_ones_survive() {
    let claimed = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let orphan = "ws0123456789abcdef0123456789abcdef"; // daemon-minted shape
    let foreign = "not-a-voie-lv";
    let tools = with_fake_host_tools("lvscan", &[&compact_lv(claimed), orphan, foreign]);
    let dir = temp_dir("lvscan");
    let config = config_with(
        "lvscan",
        &kubectl_notfound(&dir),
        &crictl_empty(&dir),
        dir.join("jails"),
    );
    {
        let store = Store::open(&config.sqlite).unwrap();
        store
            .upsert_workspace(&WorkspaceRow {
                id: claimed.to_owned(),
                state: "ready".into(),
                device: "/dev/voie-ws/wsaaaaaaaabbbbccccddddeeeeeeeeeeee".into(),
                node: "node-under-test".into(),
                pv_name: "voie-ws-live".into(),
                pvc_name: "voie-ws-live".into(),
                lv_name: Some(compact_lv(claimed)),
            })
            .unwrap();
        store
            .reserve_volume(
                claimed,
                "/dev/voie-ws/live",
                "node-under-test",
                "voie-ws-live",
            )
            .unwrap();
    }

    let fabric = Fabric::open(config.clone(), Live::from_config(&config).unwrap()).unwrap();
    let report = fabric.reconcile_startup().await.unwrap();

    assert_eq!(report.orphan_lvs_removed, vec![orphan.to_string()]);
    assert!(report.orphan_lv_failures.is_empty());
    let args = captured_args(&tools.lvremove_capture);
    assert!(
        args.iter().any(|arg| arg.ends_with(orphan)),
        "only the unclaimed LV is removed: {args:?}"
    );
    assert!(
        !args.iter().any(|arg| arg.ends_with(&compact_lv(claimed))),
        "a claimed LV must never be removed: {args:?}"
    );
    assert!(
        !args.iter().any(|arg| arg.ends_with(foreign)),
        "names outside the daemon scheme must never be touched: {args:?}"
    );
    assert!(
        report.ready_without_volume.is_empty(),
        "a claimed ready LV must not be reported missing: {report:?}"
    );
}

#[tokio::test]
async fn abandoned_workspace_allocation_releases_budget_and_lv() {
    let claimed = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let abandoned = "adc209a4-9d51-4f09-85c2-6fe17dd342f5";
    let claimed_lv = compact_lv(claimed);
    let abandoned_lv = compact_lv(abandoned);
    let tools = with_fake_host_tools("abandoned-alloc", &[&claimed_lv, &abandoned_lv]);
    let dir = temp_dir("abandoned-alloc");
    let config = config_with(
        "abandoned-alloc",
        &kubectl_notfound(&dir),
        &crictl_empty(&dir),
        dir.join("jails"),
    );
    {
        let store = Store::open(&config.sqlite).unwrap();
        store
            .upsert_workspace(&WorkspaceRow {
                id: claimed.to_owned(),
                state: "ready".into(),
                device: format!("/dev/voie-ws/{claimed_lv}"),
                node: "node-under-test".into(),
                pv_name: "voie-ws-live".into(),
                pvc_name: "voie-ws-live".into(),
                lv_name: Some(claimed_lv.clone()),
            })
            .unwrap();
        store
            .reserve_allocation(
                VolumeKind::Workspace,
                claimed,
                &claimed_lv,
                16 * 1024 * 1024 * 1024,
                None,
            )
            .unwrap();
        store
            .reserve_allocation(
                VolumeKind::Workspace,
                abandoned,
                &abandoned_lv,
                16 * 1024 * 1024 * 1024,
                None,
            )
            .unwrap();
    }

    let fabric = Fabric::open(config.clone(), Live::from_config(&config).unwrap()).unwrap();
    let report = fabric.reconcile_startup().await.unwrap();

    assert_eq!(
        report.orphan_allocations_released,
        vec![abandoned.to_string()],
        "only the workspace-less allocation is released: {report:?}"
    );
    assert_eq!(
        report.orphan_lvs_removed,
        vec![abandoned_lv.clone()],
        "the abandoned LV must be removed after the claim drops: {report:?}"
    );
    let args = captured_args(&tools.lvremove_capture);
    assert!(
        args.iter().any(|arg| arg.ends_with(&abandoned_lv)),
        "abandoned LV is removed: {args:?}"
    );
    assert!(
        !args.iter().any(|arg| arg.ends_with(&claimed_lv)),
        "a live workspace LV must never be removed: {args:?}"
    );
    let store = Store::open(&config.sqlite).unwrap();
    assert!(
        store
            .get_allocation(VolumeKind::Workspace, claimed)
            .unwrap()
            .is_some(),
        "live workspace allocation must remain"
    );
    assert!(
        store
            .get_allocation(VolumeKind::Workspace, abandoned)
            .unwrap()
            .is_none(),
        "abandoned allocation must be gone"
    );
}

#[tokio::test]
async fn leftover_ready_workspace_without_lv_is_not_reminted() {
    let leftover = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let tools = with_fake_host_tools("leftover-ready", &[]);
    let dir = temp_dir("leftover-ready");
    let config = config_with(
        "leftover-ready",
        &kubectl_notfound(&dir),
        &crictl_empty(&dir),
        dir.join("jails"),
    );
    {
        let store = Store::open(&config.sqlite).unwrap();
        store
            .upsert_workspace(&WorkspaceRow {
                id: leftover.to_owned(),
                state: "ready".into(),
                device: format!("/dev/voie-ws/{}", compact_lv(leftover)),
                node: "node-under-test".into(),
                pv_name: "voie-ws-leftover".into(),
                pvc_name: "voie-ws-leftover".into(),
                lv_name: Some(compact_lv(leftover)),
            })
            .unwrap();
        store
            .reserve_volume(leftover, "/dev/dm-4", "node-under-test", "voie-ws-leftover")
            .unwrap();
    }

    let fabric = Fabric::open(config.clone(), Live::from_config(&config).unwrap()).unwrap();
    let report = fabric.reconcile_startup().await.unwrap();
    assert_eq!(report.ready_without_volume, vec![leftover.to_string()]);
    assert!(
        captured_args(&tools.lvremove_capture)
            .iter()
            .all(|arg| !arg.contains(&compact_lv(leftover))),
        "missing leftover LVs must not be created or removed: {:?}",
        captured_args(&tools.lvremove_capture)
    );

    let error = fabric
        .create_workspace(leftover, None, None)
        .await
        .expect_err("leftover ready without LV must not return ready or mint capacity");
    let message = error.to_string();
    assert!(message.contains("refuse leftover capacity"), "{message}");
    let store = Store::open(&config.sqlite).unwrap();
    assert!(
        store.normal_allocated_bytes().unwrap() == 0,
        "leftover create must not insert a volume_allocations row"
    );
    let reservation = store.get_reservation(leftover).unwrap().unwrap();
    assert_eq!(
        reservation.state, "released",
        "leftover ready-without-volume must not keep a mapper reservation"
    );
}

#[tokio::test]
async fn retired_product_thin_pool_stops_startup_before_orphan_removal() {
    let orphan = "ws0123456789abcdef0123456789abcdef";
    let tools = with_fake_host_tools("legacy-pool", &["workspaces", "ws-root", orphan]);
    let dir = temp_dir("legacy-pool");
    let config = config_with(
        "legacy-pool",
        &kubectl_notfound(&dir),
        &crictl_empty(&dir),
        dir.join("jails"),
    );
    let fabric = Fabric::open(config.clone(), Live::from_config(&config).unwrap()).unwrap();
    let error = fabric.reconcile_startup().await.unwrap_err();
    let message = error.to_string();
    assert!(message.contains("workspaces"), "{message}");
    assert!(message.contains("ws-root"), "{message}");
    let args = captured_args(&tools.lvremove_capture);
    assert!(
        args.is_empty(),
        "retired layout must not trigger lvremove: {args:?}"
    );
}

#[tokio::test]
async fn missing_runtime_pool_stops_startup_before_orphan_removal() {
    let orphan = "ws0123456789abcdef0123456789abcdef";
    let tools = with_exact_lv_names("no-runtime", &[orphan]);
    let dir = temp_dir("no-runtime");
    let config = config_with(
        "no-runtime",
        &kubectl_notfound(&dir),
        &crictl_empty(&dir),
        dir.join("jails"),
    );
    let fabric = Fabric::open(config.clone(), Live::from_config(&config).unwrap()).unwrap();
    let error = fabric.reconcile_startup().await.unwrap_err();
    let message = error.to_string();
    assert!(message.contains("runtime"), "{message}");
    let args = captured_args(&tools.lvremove_capture);
    assert!(
        args.is_empty(),
        "missing runtime pool must not trigger lvremove: {args:?}"
    );
}

#[tokio::test]
async fn missing_workspace_pool_stops_startup_before_orphan_removal() {
    let orphan = "ws0123456789abcdef0123456789abcdef";
    let tools = with_exact_lv_names("no-workspace-pool", &["runtime", orphan]);
    let dir = temp_dir("no-workspace-pool");
    let config = config_with(
        "no-workspace-pool",
        &kubectl_notfound(&dir),
        &crictl_empty(&dir),
        dir.join("jails"),
    );
    let fabric = Fabric::open(config.clone(), Live::from_config(&config).unwrap()).unwrap();
    let error = fabric.reconcile_startup().await.unwrap_err();
    let message = error.to_string();
    assert!(message.contains("workspace"), "{message}");
    let args = captured_args(&tools.lvremove_capture);
    assert!(
        args.is_empty(),
        "missing workspace pool must not trigger lvremove: {args:?}"
    );
}

// ---------------------------------------------------------------------------
// Cleanup without a sandbox identity
// ---------------------------------------------------------------------------

fn seed_workspace_without_sandbox(config: &Config, state: &str) {
    let store = Store::open(&config.sqlite).unwrap();
    store
        .upsert_workspace(&WorkspaceRow {
            id: WORKSPACE.to_owned(),
            state: state.into(),
            device: "/dev/voie-ws/guest".into(),
            node: "node-under-test".into(),
            pv_name: format!("voie-ws-{WORKSPACE}"),
            pvc_name: format!("voie-ws-{WORKSPACE}"),
            lv_name: Some(compact_lv(WORKSPACE)),
        })
        .unwrap();
    store
        .insert_generation(&GenerationRow {
            workspace_id: WORKSPACE.to_owned(),
            generation: 1,
            pod_name: format!("voie-ws-{WORKSPACE}-e1"),
            pod_uid: None,
            sandbox_id: None, // the identity the store lost
            state: "running".into(),
        })
        .unwrap();
    store
        .reserve_volume(
            WORKSPACE,
            "/dev/voie-ws/guest",
            "node-under-test",
            &format!("voie-ws-{WORKSPACE}"),
        )
        .unwrap();
}

fn seed_ready_workspace_without_sandbox(config: &Config) {
    seed_workspace_without_sandbox(config, "ready");
}

fn seed_deleting_workspace_without_sandbox(config: &Config) {
    seed_workspace_without_sandbox(config, "deleting");
}

#[tokio::test]
async fn startup_retries_deleting_workspace_after_restart_on_positive_absence() {
    let tools = with_fake_host_tools("restart-delete-absent", &[]);
    let dir = temp_dir("restart-delete-absent");
    let jails = dir.join("jails");
    std::fs::create_dir_all(&jails).unwrap();
    let config = config_with(
        "restart-delete-absent",
        &kubectl_notfound(&dir),
        &crictl_empty(&dir),
        jails,
    );
    seed_deleting_workspace_without_sandbox(&config);

    // A fresh Fabric instance stands in for the daemon after a restart.
    let fabric = Fabric::open(config.clone(), Live::from_config(&config).unwrap()).unwrap();
    let report = fabric.reconcile_startup().await.unwrap();

    assert!(
        !report.transient_workspaces.contains(&WORKSPACE.to_owned()),
        "positive cleanup must not remain transient: {report:?}"
    );
    let store = Store::open(&config.sqlite).unwrap();
    assert_eq!(
        store.get_workspace(WORKSPACE).unwrap().unwrap().state,
        "deleted"
    );
    assert_eq!(
        store.get_reservation(WORKSPACE).unwrap().unwrap().state,
        "released"
    );
    let cleanup = store.get_cleanup(WORKSPACE).unwrap().unwrap();
    assert!(
        cleanup.pod_absent
            && cleanup.reservation_released
            && cleanup.jail_absent
            && cleanup.vmm_absent
            && cleanup.children_absent
    );

    // The supported DELETE retry is idempotent after startup has completed;
    // it returns the durable result without another LV removal.
    let retry = fabric.delete_workspace(WORKSPACE).await.unwrap();
    assert_eq!(retry.state, "deleted");
    assert_eq!(
        captured_args(&tools.lvremove_capture)
            .iter()
            .filter(|arg| arg.ends_with(&compact_lv(WORKSPACE)))
            .count(),
        1
    );
}

#[tokio::test]
async fn startup_keeps_restarted_deleting_workspace_held_on_unknown_residue() {
    let tools = with_fake_host_tools("restart-delete-unknown", &[]);
    let dir = temp_dir("restart-delete-unknown");
    let jails = dir.join("jails");
    std::fs::create_dir_all(jails.join("ghost-sandbox")).unwrap();
    let config = config_with(
        "restart-delete-unknown",
        &kubectl_notfound(&dir),
        &crictl_empty(&dir),
        jails,
    );
    seed_deleting_workspace_without_sandbox(&config);

    let fabric = Fabric::open(config.clone(), Live::from_config(&config).unwrap()).unwrap();
    let report = fabric.reconcile_startup().await.unwrap();

    assert_eq!(report.transient_workspaces, vec![WORKSPACE.to_owned()]);
    let store = Store::open(&config.sqlite).unwrap();
    assert_eq!(
        store.get_workspace(WORKSPACE).unwrap().unwrap().state,
        "deleting"
    );
    assert_eq!(
        store.get_reservation(WORKSPACE).unwrap().unwrap().state,
        "reserved"
    );
    assert!(
        !store
            .get_cleanup(WORKSPACE)
            .unwrap()
            .unwrap()
            .reservation_released
    );
    assert!(
        captured_args(&tools.lvremove_capture).is_empty(),
        "unknown residue must not release its LV"
    );
}

#[tokio::test]
async fn cleanup_without_sandbox_identity_completes_on_positive_host_wide_absence() {
    let _tools = with_fake_host_tools("clean-del", &[]);
    let dir = temp_dir("cleanup-absent");
    let jails = dir.join("jails"); // empty: no jail anywhere
    std::fs::create_dir_all(&jails).unwrap();
    let config = config_with(
        "cleanup-absent",
        &kubectl_notfound(&dir),
        &crictl_empty(&dir),
        jails,
    );
    seed_ready_workspace_without_sandbox(&config);
    let fabric = Fabric::open(config.clone(), Live::from_config(&config).unwrap()).unwrap();

    let view = fabric.delete_workspace(WORKSPACE).await.unwrap();

    assert_eq!(view.state, "deleted", "{view:?}");
    assert!(
        view.cleaned.pod
            && view.cleaned.reservation
            && view.cleaned.jail
            && view.cleaned.vmm
            && view.cleaned.children
    );
    let store = Store::open(&config.sqlite).unwrap();
    let row = store.get_reservation(WORKSPACE).unwrap().unwrap();
    assert_eq!(row.state, "released");
}

#[tokio::test]
async fn cleanup_waits_for_lingering_cri_sandbox_before_releasing_reservation() {
    let _tools = with_fake_host_tools("cri-wait-del", &[]);
    let dir = temp_dir("cleanup-cri-wait");
    let jails = dir.join("jails");
    std::fs::create_dir_all(&jails).unwrap();
    let config = config_with(
        "cleanup-cri-wait",
        &kubectl_notfound(&dir),
        &crictl_sandbox_then_empty(&dir),
        jails,
    );
    seed_ready_workspace_without_sandbox(&config);
    let fabric = Fabric::open(config.clone(), Live::from_config(&config).unwrap()).unwrap();

    let view = fabric.delete_workspace(WORKSPACE).await.unwrap();

    assert_eq!(view.state, "deleted", "{view:?}");
    assert!(
        view.cleaned.reservation,
        "lingering CRI sandbox must be waited out, not held forever: {view:?}"
    );
    let store = Store::open(&config.sqlite).unwrap();
    let row = store.get_reservation(WORKSPACE).unwrap().unwrap();
    assert_eq!(row.state, "released");
}

#[tokio::test]
async fn cleanup_without_sandbox_identity_stays_unknown_when_a_jail_exists() {
    let _tools = with_fake_host_tools("dirty-del", &[]);
    let dir = temp_dir("cleanup-present");
    let jails = dir.join("jails");
    std::fs::create_dir_all(jails.join("ghost-sandbox")).unwrap();
    let config = config_with(
        "cleanup-present",
        &kubectl_notfound(&dir),
        &crictl_empty(&dir),
        jails,
    );
    seed_ready_workspace_without_sandbox(&config);
    let fabric = Fabric::open(config.clone(), Live::from_config(&config).unwrap()).unwrap();

    let view = fabric.delete_workspace(WORKSPACE).await.unwrap();

    // Unknown runtime residue: the reservation stays held forever until
    // positive absence, and nothing reports success.
    assert_eq!(view.state, "deleting", "{view:?}");
    assert!(!view.cleaned.reservation);
    assert!(!view.cleaned.jail);
    // vmm/children flags stay environment-honest per surface; the
    // contract under test is that cleanup as a whole never reports a
    // held reservation released while any jail tree exists.
    let store = Store::open(&config.sqlite).unwrap();
    let row = store.get_reservation(WORKSPACE).unwrap().unwrap();
    assert_eq!(row.state, "reserved");
    let workspace = store.get_workspace(WORKSPACE).unwrap().unwrap();
    assert_eq!(workspace.state, "deleting");
}
// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

#[test]
fn approved_egress_requires_cidrs_and_port_together() {
    assert!(ApprovedEgress::parse(None, None).unwrap().is_none());

    let err = ApprovedEgress::parse(Some("127.0.0.1/32".into()), None).unwrap_err();
    assert!(
        err.to_string().contains("VOIE_WORKSPACE_EGRESS_PORT"),
        "{err}"
    );

    let err = ApprovedEgress::parse(None, Some("443".into())).unwrap_err();
    assert!(
        err.to_string().contains("VOIE_WORKSPACE_EGRESS_CIDRS"),
        "{err}"
    );

    let err = ApprovedEgress::parse(Some("10.1.2.3".into()), Some("443".into())).unwrap_err();
    assert!(err.to_string().contains("address/prefix"), "{err}");

    let err = ApprovedEgress::parse(Some("10.0.0.0/33".into()), Some("443".into())).unwrap_err();
    assert!(err.to_string().contains("prefix"), "{err}");

    let approved =
        ApprovedEgress::parse(Some(" 10.0.0.0/24, fd00::/8 ".into()), Some("8443".into()))
            .unwrap()
            .expect("configured");
    assert_eq!(approved.cidrs, vec!["10.0.0.0/24", "fd00::/8"]);
    assert_eq!(approved.tcp_port, 8443);
}

// ---------------------------------------------------------------------------
// mTLS client identity pinning (real TLS over loopback)
// ---------------------------------------------------------------------------

/// Ephemeral mTLS material generated at test time in a temp directory; no
/// key bytes ever live in the repository. Same throwaway convention as the
/// control-side REST flow tests.
struct MtlsMaterial {
    ca_pem: PathBuf,
    server_crt: PathBuf,
    server_key: PathBuf,
    good_crt: PathBuf,
    good_key: PathBuf,
    other_crt: PathBuf,
    other_key: PathBuf,
}

fn generate_mtls_material(dir: &Path) -> MtlsMaterial {
    fn openssl(args: &[&str]) {
        let done = std::process::Command::new("openssl")
            .args(args)
            .output()
            .expect("openssl runs");
        assert!(
            done.status.success(),
            "openssl failed: {}",
            String::from_utf8_lossy(&done.stderr)
        );
    }
    let ca_pem = dir.join("ca.pem");
    let ca_key = dir.join("ca.key");
    openssl(&[
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-keyout",
        ca_key.to_str().unwrap(),
        "-out",
        ca_pem.to_str().unwrap(),
        "-days",
        "2",
        "-nodes",
        "-subj",
        "/CN=voie-test-ca",
    ]);

    // Server identity must carry the loopback SAN the tests dial plus
    // serverAuth, exactly like the deployed fabric server certificate.
    let server_ext = dir.join("server.ext");
    std::fs::write(
        &server_ext,
        "subjectAltName=IP:127.0.0.1,DNS:localhost\nextendedKeyUsage=serverAuth\n",
    )
    .unwrap();
    let server_crt = dir.join("server.crt");
    let server_key = dir.join("server.key");
    issue_certificate(
        &openssl,
        &ca_pem,
        &ca_key,
        &server_ext,
        "/CN=baremetal-1",
        &server_key,
        &server_crt,
    );

    // Two distinct client identities from the SAME CA: only one may be pinned.
    let client_ext = dir.join("client.ext");
    std::fs::write(&client_ext, "extendedKeyUsage=clientAuth\n").unwrap();
    let good_crt = dir.join("client-good.crt");
    let good_key = dir.join("client-good.key");
    issue_certificate(
        &openssl,
        &ca_pem,
        &ca_key,
        &client_ext,
        "/CN=baremetal-1",
        &good_key,
        &good_crt,
    );
    let other_crt = dir.join("client-other.crt");
    let other_key = dir.join("client-other.key");
    issue_certificate(
        &openssl,
        &ca_pem,
        &ca_key,
        &client_ext,
        "/CN=stranger",
        &other_key,
        &other_crt,
    );

    MtlsMaterial {
        ca_pem,
        server_crt,
        server_key,
        good_crt,
        good_key,
        other_crt,
        other_key,
    }
}

#[allow(clippy::too_many_arguments)]
fn issue_certificate(
    openssl: &dyn Fn(&[&str]),
    ca_pem: &Path,
    ca_key: &Path,
    ext_file: &Path,
    subject: &str,
    key_out: &Path,
    cert_out: &Path,
) {
    let csr = key_out.with_extension("csr");
    openssl(&[
        "req",
        "-newkey",
        "rsa:2048",
        "-keyout",
        key_out.to_str().unwrap(),
        "-out",
        csr.to_str().unwrap(),
        "-nodes",
        "-subj",
        subject,
    ]);
    openssl(&[
        "x509",
        "-req",
        "-in",
        csr.to_str().unwrap(),
        "-CA",
        ca_pem.to_str().unwrap(),
        "-CAkey",
        ca_key.to_str().unwrap(),
        "-out",
        cert_out.to_str().unwrap(),
        "-days",
        "2",
        "-extfile",
        ext_file.to_str().unwrap(),
    ]);
}

/// SHA-256 over DER of a generated certificate, exercising the same
/// normalization production applies to the configured pin.
fn der_sha256_hex(cert_pem: &Path) -> String {
    let pem = std::fs::read_to_string(cert_pem).unwrap();
    let certs: Vec<_> = rustls_pemfile::certs(&mut pem.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap();
    hex(&sha2::Sha256::digest(certs[0].as_ref()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Returns a serving Config whose transport is exactly the production
/// rustls acceptor path, loaded from the generated material.
fn mtls_config(tag: &str, material: &MtlsMaterial) -> Config {
    let mut config = config_with(tag, "true", "true", temp_dir(tag).join("jails"));
    config.tls_cert = material.server_crt.clone();
    config.tls_key = material.server_key.clone();
    config.tls_ca = material.ca_pem.clone();
    config
}

struct RunningServer {
    handle: tokio::task::JoinHandle<std::io::Result<()>>,
    port: u16,
    shutdown: Arc<Notify>,
    inflight: Arc<AtomicUsize>,
}

fn spawn_mtls_server(config: Config, pinned_client_sha: String) -> RunningServer {
    let acceptor = config.tls_acceptor().expect("production acceptor builds");
    let live = Live::from_config(&config).unwrap();
    let fabric = Arc::new(Fabric::open(config.clone(), live).unwrap());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
    let port = listener.local_addr().unwrap().port();
    let shutdown = Arc::new(Notify::new());
    let inflight = Arc::new(AtomicUsize::new(0));
    let handle = tokio::spawn(serve_tls(
        listener,
        fabric,
        acceptor,
        Arc::from(pinned_client_sha.as_str()),
        shutdown.clone(),
        inflight.clone(),
    ));
    RunningServer {
        handle,
        port,
        shutdown,
        inflight,
    }
}

fn client_config(ca_pem: &Path, cert_pem: &Path, key_pem: &Path) -> rustls::ClientConfig {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut roots = rustls::RootCertStore::empty();
    let ca = std::fs::read_to_string(ca_pem).unwrap();
    for cert in rustls_pemfile::certs(&mut ca.as_bytes()) {
        roots.add(cert.unwrap()).unwrap();
    }
    let cert_text = std::fs::read_to_string(cert_pem).unwrap();
    let key_text = std::fs::read_to_string(key_pem).unwrap();
    let certs: Vec<_> = rustls_pemfile::certs(&mut cert_text.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap();
    let key = rustls_pemfile::private_key(&mut key_text.as_bytes())
        .unwrap()
        .unwrap();
    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(certs, key)
        .unwrap();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    config
}

/// Performs one HTTPS request; returns the raw response text, or the empty
/// string when the server closed without answering.
async fn https_get(port: u16, client: rustls::ClientConfig) -> std::io::Result<String> {
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    let server_name = rustls::pki_types::ServerName::IpAddress(
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST).into(),
    );
    let mut tls = connector.connect(server_name, stream).await?;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tls.write_all(b"GET /v1/health HTTP/1.1\r\nHost: voie\r\nConnection: close\r\n\r\n")
        .await?;
    tls.flush().await?;
    let mut response = Vec::new();
    match tls.read_to_end(&mut response).await {
        Ok(_) => {}
        Err(error) => return Err(error),
    }
    Ok(String::from_utf8_lossy(&response).into_owned())
}

#[tokio::test(flavor = "multi_thread")]
async fn pinned_control_identity_is_accepted_and_other_ca_clients_are_refused() {
    let material = generate_mtls_material(&temp_dir("mtls-pin"));
    let pinned_sha = der_sha256_hex(&material.good_crt);
    let server = spawn_mtls_server(mtls_config("pin", &material), pinned_sha);

    // Exactly the pinned control certificate passes and gets the API answer.
    let good = https_get(
        server.port,
        client_config(&material.ca_pem, &material.good_crt, &material.good_key),
    )
    .await
    .unwrap_or_default();
    assert!(
        good.starts_with("HTTP/1.1 200"),
        "pinned control client must be served: {good:?}"
    );

    // A different certificate from the same CA is closed after the
    // handshake: no HTTP answer exists for it.
    let other = https_get(
        server.port,
        client_config(&material.ca_pem, &material.other_crt, &material.other_key),
    )
    .await
    .unwrap_or_default();
    assert!(
        !other.contains("200"),
        "foreign CA-signed client must not be served: {other:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_stops_admission_and_drains_in_flight_connections() {
    let material = generate_mtls_material(&temp_dir("mtls-drain"));
    let pinned_sha = der_sha256_hex(&material.good_crt);
    let server = spawn_mtls_server(mtls_config("drain", &material), pinned_sha);

    // One request completes normally before shutdown.
    let good = https_get(
        server.port,
        client_config(&material.ca_pem, &material.good_crt, &material.good_key),
    )
    .await
    .unwrap_or_default();
    assert!(good.contains("200"));

    server.shutdown.notify_one();
    let handle = server.handle;
    match tokio::time::timeout(std::time::Duration::from_secs(5), handle).await {
        Ok(Ok(Ok(()))) => {}
        other => panic!("serve loop did not drain and exit cleanly: {other:?}"),
    }
    assert_eq!(
        server.inflight.load(Ordering::SeqCst),
        0,
        "all connections drained"
    );

    // Admission stopped: the listener no longer accepts new connections.
    let refused = tokio::net::TcpStream::connect(("127.0.0.1", server.port)).await;
    assert!(refused.is_err(), "listener must be closed after shutdown");
}

// ---------------------------------------------------------------------------
// Identity pin plumbing
// ---------------------------------------------------------------------------

#[test]
fn identity_pin_matches_only_the_exact_certificate() {
    let material = generate_mtls_material(&temp_dir("identity-pin"));
    let expected = der_sha256_hex(&material.good_crt);
    let good_text = std::fs::read_to_string(&material.good_crt).unwrap();
    let certs: Vec<_> = rustls_pemfile::certs(&mut good_text.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap();

    assert!(client_identity_matches(Some(&certs), &expected));
    // Normalization: colons and uppercase hex in configuration are accepted.
    let decorated: String = expected
        .as_bytes()
        .chunks(2)
        .map(|pair| std::str::from_utf8(pair).unwrap().to_ascii_uppercase())
        .collect::<Vec<_>>()
        .join(":");
    assert!(client_identity_matches(Some(&certs), &decorated));

    // Any other certificate, no peer certificate, or a malformed pin fails.
    let other_text = std::fs::read_to_string(&material.other_crt).unwrap();
    let other: Vec<_> = rustls_pemfile::certs(&mut other_text.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(!client_identity_matches(Some(&other), &expected));
    assert!(!client_identity_matches(None, &expected));
    assert!(!client_identity_matches(Some(&certs), "not-hex"));
}

#[tokio::test]
async fn existing_lv_without_key_is_refused() {
    let _tools = with_fake_host_tools("legacy-lv", &[]);
    let dir = temp_dir("legacy-lv");
    let config = config_with(
        "legacy-lv",
        &kubectl_notfound(&dir),
        &crictl_empty(&dir),
        dir.join("jails"),
    );
    let live = Live::from_config(&config).unwrap();
    let error = live
        .prepare_block(
            WORKSPACE,
            voie_fabricd::StoragePolicy::test().workspace_bytes,
        )
        .await
        .expect_err("an existing LV with no key record is legacy");
    match error {
        voie_fabricd::FabricError::Foreign(message) => {
            assert!(message.contains("no matching key record"), "{message}");
        }
        other => panic!("expected Foreign, got {other:?}"),
    }
    let keys = config
        .sqlite
        .parent()
        .expect("sqlite parent")
        .join("volume-keys");
    let generated = std::fs::read_dir(&keys)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(
        generated, 0,
        "a missing key must not mint a fresh random key over existing bytes"
    );
}

#[tokio::test]
async fn cryptsetup_close_failure_retains_the_key_and_skips_lvremove() {
    let lock = HOST_TOOLS_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let bin = temp_dir("bin-close-fail");
    write_executable(&bin, "findmnt", "#!/bin/sh\nexit 1\n");
    write_executable(&bin, "lvchange", "#!/bin/sh\nexit 0\n");
    write_executable(
        &bin,
        "cryptsetup",
        "#!/bin/sh\nif [ \"$1\" = close ]; then echo 'device-mapper: remove ioctl failed' >&2; exit 1; fi\nexit 0\n",
    );
    write_executable(&bin, "lvs", "#!/bin/sh\nexit 0\n");
    let lvremove_capture = bin.join("lvremove.bin");
    write_executable(
        &bin,
        "lvremove",
        &format!(
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > '{}'\nexit 0\n",
            lvremove_capture.display()
        ),
    );
    let previous_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path = bin.as_os_str().to_owned();
    path.push(":");
    path.push(&previous_path);
    unsafe { std::env::set_var("PATH", &path) };
    let _tools = HostTools {
        _lock: lock,
        lvremove_capture: lvremove_capture.clone(),
        previous_path,
    };

    let dir = temp_dir("close-fail");
    let config = config_with(
        "close-fail",
        &kubectl_notfound(&dir),
        &crictl_empty(&dir),
        dir.join("jails"),
    );
    let key_dir = config
        .sqlite
        .parent()
        .expect("sqlite parent")
        .join("volume-keys");
    std::fs::create_dir_all(&key_dir).expect("key dir");
    let lv_name = compact_lv(WORKSPACE);
    let key_path = key_dir.join(&lv_name);
    std::fs::write(&key_path, [7u8; 32]).expect("key material");

    let live = Live::from_config(&config).unwrap();
    let error = live
        .release_block(&voie_fabricd::BlockSlot {
            device: String::new(),
            lv_name: Some(lv_name),
            mapper_name: None,
        })
        .await
        .expect_err("unknown close must fail closed");
    match error {
        voie_fabricd::FabricError::Unknown(message) => {
            assert!(message.contains("cryptsetup close failed"), "{message}");
        }
        other => panic!("expected Unknown, got {other:?}"),
    }
    assert!(
        key_path.exists(),
        "the volume key must be retained when mapper close is unknown"
    );
    assert!(
        !lvremove_capture.exists() || captured_args(&lvremove_capture).is_empty(),
        "lvremove must not run after a failed mapper close"
    );
}

fn kubectl_pv_retarget(dir: &Path, pv_path: &str, apply_log: &Path, delete_log: &Path) -> String {
    write_executable(
        dir,
        "kubectl-pv-retarget",
        &format!(
            r#"#!/bin/sh
if [ "$1" = "get" ] && [ "$2" = "pv" ]; then
  cat <<'JSON'
{{"apiVersion":"v1","kind":"PersistentVolume","metadata":{{"name":"voie-pgdata-rst-x","uid":"abc","resourceVersion":"7"}},"spec":{{"local":{{"path":"{pv_path}"}},"claimRef":{{"name":"voie-pgdata-rst-x"}}}},"status":{{"phase":"Bound"}}}}
JSON
  exit 0
fi
if [ "$1" = "get" ]; then
  echo "Error from server (NotFound)" >&2
  exit 1
fi
if [ "$1" = "delete" ]; then
  printf '%s\n' "$*" >> '{delete}'
  exit 0
fi
if [ "$1" = "apply" ]; then
  cat >> '{apply}'
  exit 0
fi
exit 0
"#,
            pv_path = pv_path,
            apply = apply_log.display(),
            delete = delete_log.display()
        ),
    )
    .to_string_lossy()
    .into_owned()
}

#[tokio::test]
async fn recycled_dm_n_pv_is_replaced_with_encrypted_mapper() {
    let _tools = with_fake_host_tools("pv-retarget", &[]);
    let dir = temp_dir("pv-retarget");
    let apply_log = dir.join("apply.log");
    let delete_log = dir.join("delete.log");
    let mapper = "/dev/mapper/voie-crypt-rstadd02a4281b44853b7502c6ede1341ab";
    let config = config_with(
        "pv-retarget",
        &kubectl_pv_retarget(&dir, "/dev/dm-4", &apply_log, &delete_log),
        &crictl_empty(&dir),
        dir.join("jails"),
    );
    let live = Live::from_config(&config).unwrap();
    let replaced = live
        .replace_local_pv_device("voie-pgdata-rst-x", "voie-pgdata-rst-x", mapper)
        .await
        .unwrap();
    assert!(replaced, "recycled /dev/dm-4 must be replaced");
    let applied = std::fs::read_to_string(&apply_log).unwrap();
    assert!(
        applied.contains(mapper),
        "retargeted PV must persist the encrypted mapper: {applied}"
    );
    assert!(
        !applied.contains("/dev/dm-4"),
        "recycled dm-N must not remain on the PV: {applied}"
    );
    let deleted = std::fs::read_to_string(&delete_log).unwrap();
    assert!(deleted.contains("pvc"), "PVC must be deleted before recreate: {deleted}");
    assert!(deleted.contains(" pv "), "PV must be deleted before recreate: {deleted}");
}

#[tokio::test]
async fn stable_mapper_pv_is_not_replaced() {
    let _tools = with_fake_host_tools("pv-stable", &[]);
    let dir = temp_dir("pv-stable");
    let apply_log = dir.join("apply.log");
    let delete_log = dir.join("delete.log");
    let mapper = "/dev/mapper/voie-crypt-rstadd02a4281b44853b7502c6ede1341ab";
    let config = config_with(
        "pv-stable",
        &kubectl_pv_retarget(&dir, mapper, &apply_log, &delete_log),
        &crictl_empty(&dir),
        dir.join("jails"),
    );
    let live = Live::from_config(&config).unwrap();
    let replaced = live
        .replace_local_pv_device("voie-pgdata-rst-x", "voie-pgdata-rst-x", mapper)
        .await
        .unwrap();
    assert!(!replaced, "identical mapper path must be left in place");
    assert!(
        !apply_log.exists() || std::fs::read_to_string(&apply_log).unwrap().is_empty(),
        "stable PV must not be re-applied"
    );
    assert!(
        !delete_log.exists() || std::fs::read_to_string(&delete_log).unwrap().is_empty(),
        "stable PV must not be deleted"
    );
}

fn kubectl_pv_absent(dir: &Path, apply_log: &Path) -> String {
    write_executable(
        dir,
        "kubectl-pv-absent",
        &format!(
            r#"#!/bin/sh
if [ "$1" = "get" ]; then
  echo "Error from server (NotFound)" >&2
  exit 1
fi
if [ "$1" = "apply" ]; then
  cat >> '{apply}'
  echo '---' >> '{apply}'
  exit 0
fi
exit 0
"#,
            apply = apply_log.display()
        ),
    )
    .to_string_lossy()
    .into_owned()
}

#[tokio::test]
async fn absent_pv_is_created_on_stable_mapper() {
    let _tools = with_fake_host_tools("pv-absent", &[]);
    let dir = temp_dir("pv-absent");
    let apply_log = dir.join("apply.log");
    let mapper = "/dev/mapper/voie-crypt-rsta2b0705aaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let config = config_with(
        "pv-absent",
        &kubectl_pv_absent(&dir, &apply_log),
        &crictl_empty(&dir),
        dir.join("jails"),
    );
    let live = Live::from_config(&config).unwrap();
    let created = live
        .ensure_local_pv_device(
            "voie-ws-697b",
            "voie-ws-697b",
            mapper,
            "kind: PersistentVolume\nmetadata:\n  name: voie-ws-697b\nspec:\n  local:\n    path: PLACEHOLDER\n",
            "kind: PersistentVolumeClaim\nmetadata:\n  name: voie-ws-697b\n",
        )
        .await
        .unwrap();
    assert!(created, "missing PV must be created, not skipped");
    let applied = std::fs::read_to_string(&apply_log).unwrap();
    assert!(
        applied.contains("voie-ws-697b"),
        "absent PV YAML must be applied: {applied}"
    );
}

#[tokio::test]
async fn wait_named_gone_returns_when_get_is_not_found() {
    let _tools = with_fake_host_tools("pod-gone", &[]);
    let dir = temp_dir("pod-gone");
    let config = config_with(
        "pod-gone",
        &kubectl_pv_absent(&dir, &dir.join("apply.log")),
        &crictl_empty(&dir),
        dir.join("jails"),
    );
    let live = Live::from_config(&config).unwrap();
    live.wait_named_gone("pod", "voie-ws-x", true, std::time::Duration::from_secs(1))
        .await
        .expect("NotFound means gone");
}

