//! Focused contract tests for the RuntimeClass admission precondition.
//!
//! fabricd can finish its own mTLS bootstrap before the cluster has
//! converged the estate RuntimeClass through the k3s auto-deploy loop —
//! exactly the window where a pod apply was rejected by admission with a
//! bare HTTP 500. Workspace realization therefore positively observes the
//! configured RuntimeClass (`kubectl get runtimeclass`) before any pod
//! apply: present with the configured CRI handler admits, absence past a
//! bounded wait fails Unknown, and presence with a different handler fails
//! Foreign immediately because deployment state this daemon does not own
//! never converges into what admission needs on its own. Nothing here may
//! create, replace, or fall back: the pod manifest keeps naming exactly the
//! configured class.
//!
//! The name/handler split is deliberate deployment state: the host profile
//! declares RuntimeClass `voie-firecracker` selecting handler
//! `kata-fc-rs-voie`, so a gate that compared the handler against the class
//! name could never pass and is pinned against here.

use std::path::{Path, PathBuf};
use std::time::Duration;

use voie_fabricd::{Config, Fabric, FabricError, Live, Store};

const CLASS: &str = "voie-firecracker";
const HANDLER: &str = "kata-fc-rs-voie";
const OTHER_HANDLER: &str = "runc";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "voie-fabricd-runtimeclass-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn write_executable(dir: &Path, name: &str, body: &str) -> PathBuf {
    let program = dir.join(name);
    // Publish atomically: a concurrent exec must never catch the file
    // mid-write (ETXTBSY) or half-written.
    let staging = dir.join(format!(".{name}.staging"));
    std::fs::write(&staging, body).expect("stage fake program");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::fs::rename(&staging, &program).expect("publish fake program");
    program
}

fn test_config(kubectl_program: &str, sqlite: PathBuf) -> Config {
    Config {
        bind: "localhost:0".into(),
        sqlite,
        node_name: "node-under-test".into(),
        namespace: "voie-workspace".into(),
        storage_class: "voie-workspace-block".into(),
        runtime_class: CLASS.into(),
        runtime_handler: HANDLER.into(),
        runner_image: "voie-runner:c1".into(),
        jailer_root: PathBuf::from("/run/kata-containers/shared/firecracker"),
        vg: "voie-ws".into(),
        lv_size: "1G".into(),
        residue_wait_secs: 1,
        runtime_class_wait_secs: 60,
        kubectl_program: kubectl_program.to_owned(),
        kubectl_prefix: vec![],
        kubeconfig: None,
        crictl_program: "k3s".into(),
        crictl_prefix: vec!["crictl".into()],
        tls_cert: PathBuf::from("/dev/null"),
        tls_key: PathBuf::from("/dev/null"),
        tls_ca: PathBuf::from("/dev/null"),
        approved_egress: None,
        client_sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
    }
}

/// A realistic API-server RuntimeClass object: server-managed metadata
/// around the top-level `handler` field that pod admission resolves.
fn runtime_class_json(handler: &str) -> String {
    serde_json::json!({
        "apiVersion": "node.k8s.io/v1",
        "kind": "RuntimeClass",
        "metadata": {
            "name": CLASS,
            "uid": "5a1c9e40-7b2d-4e08-9c3a-1f6d2b8e4a70",
            "resourceVersion": "18446",
            "creationTimestamp": "2026-08-26T09:30:00Z",
            "labels": { "io.voie/managed": "true" }
        },
        "handler": handler,
    })
    .to_string()
}

/// Fake kubectl answering every `get` with one fixed JSON document and
/// logging each invocation as one `$*` line; applies consume stdin and
/// succeed. Used for outcomes decided on the very first observation.
fn fake_kubectl(dir: &Path, response_json: &str) -> PathBuf {
    let response = dir.join("runtimeclass.json");
    std::fs::write(&response, response_json).expect("write runtimeclass response");
    let log = dir.join("kubectl-calls.log");
    write_executable(
        dir,
        "kubectl",
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
if [ "$1" = "get" ]; then
  cat '{}'
  exit 0
fi
cat > /dev/null
exit 0
"#,
            log.display(),
            response.display()
        ),
    )
}

/// Fake kubectl whose first `get runtimeclass` reports NotFound exactly as
/// the API server does and every later `get` serves `response_json`,
/// modelling the class materializing mid-wait through k3s auto-deploy.
fn fake_kubectl_appearing_late(dir: &Path, response_json: &str) -> PathBuf {
    let response = dir.join("runtimeclass.json");
    std::fs::write(&response, response_json).expect("write runtimeclass response");
    let log = dir.join("kubectl-calls.log");
    let seen = dir.join("first-get-seen");
    write_executable(
        dir,
        "kubectl",
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{log}'
if [ "$1" = "get" ] && [ ! -f '{seen}' ]; then
  touch '{seen}'
  printf 'Error from server (NotFound): runtimeclasses.node.k8s.io "{class}" not found\n' >&2
  exit 1
fi
cat '{response}'
exit 0
"#,
            log = log.display(),
            seen = seen.display(),
            response = response.display(),
            class = CLASS,
        ),
    )
}

/// Fake kubectl reporting NotFound forever: a manifest the auto-deploy loop
/// never delivers.
fn fake_kubectl_notfound(dir: &Path) -> PathBuf {
    let log = dir.join("kubectl-calls.log");
    write_executable(
        dir,
        "kubectl",
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
printf 'Error from server (NotFound): runtimeclasses.node.k8s.io "{}" not found\n' >&2
exit 1
"#,
            log.display(),
            CLASS
        ),
    )
}

/// Fake kubectl whose first `get` fails with a connection-refused stderr
/// exactly as an API server mid-restart does; every later `get` serves
/// `response_json`.
fn fake_kubectl_refusing_then_ready(dir: &Path, response_json: &str) -> PathBuf {
    let response = dir.join("runtimeclass.json");
    std::fs::write(&response, response_json).expect("write runtimeclass response");
    let log = dir.join("kubectl-calls.log");
    let refused = dir.join("first-get-refused");
    write_executable(
        dir,
        "kubectl",
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{log}'
if [ "$1" = "get" ] && [ ! -f '{refused}' ]; then
  touch '{refused}'
  printf 'The connection to the server [IP_ADDRESS]:6443 was refused - did you specify the right host or port?\n' >&2
  exit 1
fi
cat '{response}'
exit 0
"#,
            log = log.display(),
            refused = refused.display(),
            response = response.display(),
        ),
    )
}

/// Fake kubectl failing every invocation with connection-refused stderr:
/// an API surface that never answers.
fn fake_kubectl_always_refusing(dir: &Path) -> PathBuf {
    let log = dir.join("kubectl-calls.log");
    write_executable(
        dir,
        "kubectl",
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
printf 'The connection to the server [IP_ADDRESS]:6443 was refused - did you specify the right host or port?\n' >&2
exit 1
"#,
            log.display()
        ),
    )
}

fn kubectl_call_log(program: &Path) -> Vec<String> {
    let log = program.parent().unwrap().join("kubectl-calls.log");
    String::from_utf8_lossy(&std::fs::read(log).unwrap_or_default())
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The `--request-timeout` seconds value of one logged kubectl invocation.
fn request_timeout_secs(line: &str) -> Option<u64> {
    line.split_whitespace()
        .position(|word| word == "--request-timeout")
        .and_then(|index| {
            line.split_whitespace()
                .nth(index + 1)
                .and_then(|value| value.strip_suffix('s'))
                .and_then(|value| value.parse().ok())
        })
}

/// Installs recording stand-ins for every host tool block preparation
/// would invoke (`lvs`, `lvcreate`, `readlink`, `findmnt`, `blkid`,
/// `mkfs.ext4`) until the guard drops; each logs its argv to one capture
/// file and exits successfully.
struct RecordingHostTools {
    _lock: std::sync::MutexGuard<'static, ()>,
    previous_path: std::ffi::OsString,
    bin: PathBuf,
}

impl RecordingHostTools {
    fn install(tag: &str) -> Self {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let lock = LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let bin = temp_dir(&format!("bin-{tag}"));
        let capture = bin.join("host-tools.log");
        for tool in [
            "lvs",
            "lvcreate",
            "readlink",
            "findmnt",
            "blkid",
            "mkfs.ext4",
        ] {
            write_executable(
                &bin,
                tool,
                &format!(
                    "#!/bin/sh\nprintf '%s\\0' \"$0\" \"$@\" >> '{capture}'\nexit 0\n",
                    capture = capture.display(),
                ),
            );
        }
        let previous_path = std::env::var_os("PATH").unwrap_or_default();
        let mut path = bin.as_os_str().to_owned();
        path.push(":");
        path.push(&previous_path);
        // Safe under LOCK: no other test thread reads or writes PATH now.
        unsafe { std::env::set_var("PATH", &path) };
        Self {
            _lock: lock,
            previous_path,
            bin,
        }
    }

    /// True when not a single host tool was invoked.
    fn nothing_ran(&self) -> bool {
        std::fs::read(self.bin.join("host-tools.log"))
            .map(|bytes| bytes.is_empty())
            .unwrap_or(true)
    }
}

impl Drop for RecordingHostTools {
    fn drop(&mut self) {
        // Safe under the same lock the guard held.
        unsafe { std::env::set_var("PATH", &self.previous_path) };
    }
}

#[tokio::test]
async fn configured_handler_admits_without_touching_the_object() {
    let tag = "ready";
    let dir = temp_dir(tag);
    let program = fake_kubectl(&dir, &runtime_class_json(HANDLER));
    let config = test_config(program.to_str().unwrap(), dir.join("state.sqlite"));
    let fabric = Fabric::open(config.clone(), Live::from_config(&config).unwrap()).unwrap();

    fabric
        .ensure_runtime_class()
        .await
        .expect("the configured RuntimeClass with its configured handler admits pods");

    // Positive observation only: the gate reads the object and never
    // applies anything — the RuntimeClass is estate deployment state.
    let log = kubectl_call_log(&program);
    assert_eq!(log.len(), 1, "exactly one observation: {log:?}");
    assert!(
        log[0].starts_with(&format!("get runtimeclass {CLASS} -o json")),
        "{:?}",
        log[0]
    );
    // The read itself is bounded within the production readiness budget.
    let secs = request_timeout_secs(&log[0]).expect("--request-timeout present");
    assert!(
        (1..=60).contains(&secs),
        "within the 60s budget: {:?}",
        log[0]
    );
}

#[tokio::test]
async fn different_handler_refused_immediately_as_foreign() {
    let tag = "wrong-handler";
    let dir = temp_dir(tag);
    let program = fake_kubectl(&dir, &runtime_class_json(OTHER_HANDLER));
    let config = test_config(program.to_str().unwrap(), dir.join("state.sqlite"));
    let fabric = Fabric::open(config.clone(), Live::from_config(&config).unwrap()).unwrap();

    let error = fabric
        .ensure_runtime_class()
        .await
        .expect_err("a same-named RuntimeClass backed by another runtime must never admit");

    match error {
        FabricError::Foreign(message) => {
            assert!(message.contains(CLASS), "{message}");
            assert!(message.contains(OTHER_HANDLER), "{message}");
            assert!(message.contains(HANDLER), "{message}");
        }
        other => panic!("expected FabricError::Foreign, got: {other:?}"),
    }
    // Refusal is immediate: waiting cannot convert foreign deployment state
    // into the configured handler, so exactly one observation is made even
    // though the production bound spans many poll periods.
    let log = kubectl_call_log(&program);
    assert_eq!(log.len(), 1, "one bounded read only: {log:?}");
    assert!(log[0].starts_with(&format!("get runtimeclass {CLASS} -o json")));
    let secs = request_timeout_secs(&log[0]).expect("--request-timeout present");
    assert!(
        (1..=60).contains(&secs),
        "within the 60s budget: {:?}",
        log[0]
    );
}

#[tokio::test]
async fn absent_class_fails_unknown_at_the_bound() {
    let tag = "missing";
    let dir = temp_dir(tag);
    let program = fake_kubectl_notfound(&dir);
    let config = test_config(program.to_str().unwrap(), dir.join("state.sqlite"));
    // Zero-width bound: one positive observation happens, then the gate
    // reports truthfully instead of pretending success or retrying forever.
    let live = Live::from_config(&config).unwrap();
    let error = live
        .wait_runtime_class_ready(Duration::ZERO)
        .await
        .expect_err("an absent RuntimeClass can never satisfy pod admission");

    match error {
        FabricError::Unknown(message) => {
            assert!(message.contains(CLASS), "{message}");
            assert!(message.contains(HANDLER), "{message}");
            assert!(
                message.to_ascii_lowercase().contains("did not appear"),
                "the failure must say the class never materialized: {message}"
            );
        }
        other => panic!("expected FabricError::Unknown, got: {other:?}"),
    }
    // The single observation was still individually bounded: a hung read
    // resolves through kubectl's own --request-timeout instead of hanging
    // realization past the stated budget.
    let log = kubectl_call_log(&program);
    assert_eq!(request_timeout_secs(&log[0]), Some(1), "{:?}", log);
}

#[tokio::test]
async fn class_materializing_midwait_converges_within_the_bound() {
    let tag = "late";
    let dir = temp_dir(tag);
    let program = fake_kubectl_appearing_late(&dir, &runtime_class_json(HANDLER));
    let config = test_config(program.to_str().unwrap(), dir.join("state.sqlite"));
    let live = Live::from_config(&config).unwrap();

    // This is the exact production race: the daemon becomes reachable while
    // the k3s auto-deploy loop has not yet delivered the manifest. The gate
    // waits across poll periods and admits once the class verifiably exists.
    live.wait_runtime_class_ready(Duration::from_secs(30))
        .await
        .expect("a RuntimeClass delivered during the bounded wait admits pods");

    let log = kubectl_call_log(&program);
    assert_eq!(log.len(), 2, "poll until observed convergence: {log:?}");
    for line in &log {
        assert!(line.starts_with(&format!("get runtimeclass {CLASS} -o json")));
        let secs = request_timeout_secs(line).expect("every read carries --request-timeout");
        assert!(
            (1..=30).contains(&secs),
            "bound within the 30s wait: {line}"
        );
    }
}

#[tokio::test]
async fn transient_read_failure_is_retried_until_the_class_verifiably_admits() {
    let tag = "transient";
    let dir = temp_dir(tag);
    let program = fake_kubectl_refusing_then_ready(&dir, &runtime_class_json(HANDLER));
    let config = test_config(program.to_str().unwrap(), dir.join("state.sqlite"));
    let live = Live::from_config(&config).unwrap();

    // A failed read is not an answer about the object: an API server
    // restarting inside the convergence window must not fail realization,
    // so the gate retries within its bound and admits on the first positive
    // observation.
    live.wait_runtime_class_ready(Duration::from_secs(30))
        .await
        .expect("a transient API-server refusal must be retried within the bound");

    let log = kubectl_call_log(&program);
    assert_eq!(log.len(), 2, "failed read then successful read: {log:?}");
}

#[tokio::test]
async fn persistent_read_failure_fails_unknown_with_bounded_reason() {
    let tag = "unreachable";
    let dir = temp_dir(tag);
    let program = fake_kubectl_always_refusing(&dir);
    let mut config = test_config(program.to_str().unwrap(), dir.join("state.sqlite"));
    config.runtime_class_wait_secs = 0;
    let live = Live::from_config(&config).unwrap();

    let error = live
        .wait_runtime_class_ready(live.runtime_class_wait())
        .await
        .expect_err("an API surface that never answers can never verify admission");

    match error {
        FabricError::Unknown(message) => {
            assert!(message.contains("did not appear with handler"), "{message}");
            // The last failed read's reason is preserved, bounded, so a
            // genuinely broken API surface is diagnosed rather than masked
            // as mere lateness.
            assert!(message.contains("last read failure"), "{message}");
            assert!(message.contains("refused"), "{message}");
        }
        other => panic!("expected FabricError::Unknown, got: {other:?}"),
    }
}

#[tokio::test]
async fn unready_class_stops_creation_before_any_realization_side_effect() {
    let tag = "no-side-effects";
    let dir = temp_dir(tag);
    let program = fake_kubectl_notfound(&dir);
    let sqlite = dir.join("state.sqlite");
    let mut config = test_config(program.to_str().unwrap(), sqlite.clone());
    // Zero-width bound keeps the failure path deterministic and fast while
    // exercising exactly the same gate the production bound guards.
    config.runtime_class_wait_secs = 0;
    let fabric = Fabric::open(config.clone(), Live::from_config(&config).unwrap()).unwrap();
    let _tools = RecordingHostTools::install(tag);

    let error = fabric
        .create_workspace("ws-noside")
        .await
        .expect_err("an absent RuntimeClass must stop creation outright");
    assert!(matches!(error, FabricError::Unknown(_)), "{error:?}");

    // Not one logical volume carved, device probed, or filesystem made:
    // admission's precondition is checked before the first irreversible
    // byte moves anywhere.
    assert!(
        _tools.nothing_ran(),
        "block preparation tools must never run behind a failed gate"
    );

    // The cluster saw reads only: no namespace, storage class, account,
    // policy, PV, PVC, or pod apply ever left the daemon.
    let log = kubectl_call_log(&program);
    assert!(
        !log.is_empty()
            && log
                .iter()
                .all(|line| { line.starts_with(&format!("get runtimeclass {CLASS} -o json")) }),
        "only bounded RuntimeClass reads may occur: {log:?}"
    );

    // The store holds no workspace row and no volume reservation: a retry
    // after deployment converges starts from a pristine slate.
    let store = Store::open(&sqlite).unwrap();
    assert!(store.get_workspace("ws-noside").unwrap().is_none());
}
