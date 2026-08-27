//! Focused contract tests for NetworkPolicy convergence.
//!
//! The local API-server capture showed exactly one stored-shape difference:
//! a desired `spec.ingress: []` round-trips back with the key pruned
//! entirely. Confirmation therefore applies that single published
//! canonicalization — omitted ingress equals empty ingress while the
//! policyTypes declare Ingress isolation — and compares every other
//! meaningful field exactly. These tests pin the contract end to end
//! against a fake kubectl answering with realistic API-server-shaped
//! objects: the captured shape converges, while nonempty or unexpected
//! ingress, changed egress, and narrowed policyTypes still fail closed as
//! Unknown carrying bounded, field-level drift evidence and a durable
//! observed digest.

use std::path::{Path, PathBuf};

use sha2::Digest;
use voie_fabricd::{ApprovedEgress, Config, Fabric, FabricError, Live, Store};

const POLICY_NAME: &str = "voie-guest-egress";
const NAMESPACE: &str = "voie-workspace";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "voie-fabricd-policy-drift-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn test_config(kubectl_program: &str, sqlite: PathBuf) -> Config {
    Config {
        bind: "localhost:0".into(),
        sqlite,
        node_name: "node-under-test".into(),
        namespace: NAMESPACE.into(),
        storage_class: "voie-workspace-block".into(),
        runtime_class: "voie-firecracker".into(),
        runtime_handler: "kata-fc-rs-voie".into(),
        runner_image: "voie-runner:c1".into(),
        jailer_root: PathBuf::from("/run/kata-containers/shared/firecracker"),
        vg: "voie-ws".into(),
        lv_size: "1G".into(),
        residue_wait_secs: 1,
        runtime_class_wait_secs: 1,
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

/// Writes a fake kubectl that answers every `get networkpolicy` with the
/// given JSON document — as an API server would, unchanged by any number of
/// applies — records every invocation as one `$*` line, consumes stdin on
/// `apply`, and exits 0 everywhere.
fn fake_kubectl(dir: &Path, response_json: &str) -> PathBuf {
    let response = dir.join("networkpolicy.json");
    std::fs::write(&response, response_json).expect("write networkpolicy response");
    let log = dir.join("kubectl-calls.log");
    let program = dir.join("kubectl");
    std::fs::write(
        &program,
        format!(
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
    .expect("write fake kubectl");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    program
}

fn kubectl_call_log(program: &Path) -> Vec<String> {
    let log = program.parent().unwrap().join("kubectl-calls.log");
    String::from_utf8_lossy(&std::fs::read(log).unwrap_or_default())
        .lines()
        .map(str::to_owned)
        .collect()
}

/// A realistic `kubectl get networkpolicy -o json` answer: server-managed
/// metadata noise around the spec. The oversized annotation mimics the
/// unbounded payloads the evidence must never carry into an error message.
fn api_server_object(spec: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {
            "name": POLICY_NAME,
            "namespace": NAMESPACE,
            "uid": "0f6d4b5c-9a2e-4f7a-b3c1-5d8e7f6a9b0c",
            "resourceVersion": "184467",
            "generation": 1,
            "creationTimestamp": "2026-08-26T09:30:00Z",
            "labels": { "io.voie/managed": "true" },
            "annotations": {
                "kubectl.kubernetes.io/last-applied-configuration":
                    format!("{{\"spec\":{{\"padding\":\"{}\"}}}}", "x".repeat(4096))
            }
        },
        "spec": spec,
        "status": {}
    })
}

/// The desired spec with one field rewritten by `mutate`.
fn mutated_spec(
    live: &Live,
    mutate: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> serde_json::Value {
    let mut spec = live.desired_network_policy_spec();
    let map = spec.as_object_mut().expect("desired spec is an object");
    mutate(map);
    spec
}

/// The stored shape from the local API-server capture: identical to the
/// desired spec except the empty `ingress: []` list was pruned by the
/// storage round-trip. The one canonical equivalence confirmation accepts.
fn canonically_pruned_spec(live: &Live) -> serde_json::Value {
    mutated_spec(live, |spec| {
        spec.remove("ingress");
    })
}

fn spec_digest(spec: &serde_json::Value) -> String {
    let canonical = serde_json::to_string(spec).expect("spec serializes");
    sha2::Sha256::digest(canonical.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn unknown_message(error: FabricError) -> String {
    match error {
        FabricError::Unknown(message) => message,
        other => panic!("expected FabricError::Unknown, got: {other:?}"),
    }
}

/// Drift evidence must stay diagnostic-sized regardless of the observed
/// payload, and server-managed metadata never leaks into it.
fn assert_bounded_without_metadata_leakage(message: &str) {
    assert!(!message.contains("resourceVersion"), "{message}");
    assert!(!message.contains("last-applied"), "{message}");
    assert!(!message.contains("creationTimestamp"), "{message}");
    assert!(!message.contains("xxxxx"), "{message}");
    assert!(
        message.len() < 2048,
        "{} bytes is not bounded",
        message.len()
    );
}

#[tokio::test]
async fn captured_pruned_ingress_converges() {
    let tag = "captured";
    let dir = temp_dir(tag);
    // Run the regression against the full production policy shape: default
    // deny plus DNS and one approved egress block, with only the empty
    // ingress list pruned by the storage round-trip.
    let mut config_template = test_config("unused", dir.join("provisional.sqlite"));
    config_template.approved_egress = Some(ApprovedEgress {
        cidrs: vec!["127.0.0.0/24".into()],
        tcp_port: 443,
    });
    let live = Live::from_config(&config_template).unwrap();
    let desired_spec = live.desired_network_policy_spec();
    let captured_spec = canonically_pruned_spec(&live);

    let program = fake_kubectl(
        &dir,
        &serde_json::to_string(&api_server_object(&captured_spec)).expect("object serializes"),
    );
    let sqlite = dir.join("state.sqlite");
    let mut config = test_config(program.to_str().unwrap(), sqlite.clone());
    config.approved_egress = config_template.approved_egress;
    let fabric = Fabric::open(config.clone(), Live::from_config(&config).unwrap()).unwrap();

    fabric
        .ensure_network_policy()
        .await
        .expect("the captured stored shape must converge: omitted ingress is empty ingress");

    // The observation is durable and recorded as positively present.
    let store = Store::open(&sqlite).unwrap();
    let row = store.get_policy(POLICY_NAME).unwrap().expect("policy row");
    assert_eq!(row.observed_state, "present");
    assert_eq!(row.desired_spec_sha, spec_digest(&desired_spec));
    assert_eq!(
        row.observed_spec_sha.as_deref(),
        Some(spec_digest(&desired_spec).as_str())
    );

    // Convergence still runs the full conversation: observe, converge,
    // re-observe.
    assert_eq!(
        kubectl_call_log(&program),
        vec![
            format!("get networkpolicy {POLICY_NAME} -n {NAMESPACE} -o json"),
            "apply -f -".to_owned(),
            format!("get networkpolicy {POLICY_NAME} -n {NAMESPACE} -o json"),
        ],
        "one apply must still be attempted before confirmation"
    );
}

#[tokio::test]
async fn nonempty_ingress_fails_closed() {
    let tag = "ingress-allow";
    let dir = temp_dir(tag);
    let live = Live::from_config(&test_config("kubectl", dir.join("provisional.sqlite"))).unwrap();
    let desired_spec = live.desired_network_policy_spec();
    // An unexpected ingress allow-list: exactly what default-deny forbids.
    let drifted_spec = mutated_spec(&live, |spec| {
        spec.insert(
            "ingress".into(),
            serde_json::json!([{
                "from": [{
                    "namespaceSelector": {
                        "matchLabels": {
                            "kubernetes.io/metadata.name": "kube-system"
                        }
                    }
                }],
                "ports": [{"protocol": "TCP", "port": 8080}]
            }]),
        );
    });

    let program = fake_kubectl(
        &dir,
        &serde_json::to_string(&api_server_object(&drifted_spec)).expect("object serializes"),
    );
    let sqlite = dir.join("state.sqlite");
    let config = test_config(program.to_str().unwrap(), sqlite.clone());
    let fabric = Fabric::open(config.clone(), Live::from_config(&config).unwrap()).unwrap();

    let error = fabric
        .ensure_network_policy()
        .await
        .expect_err("a nonempty ingress list is real drift and must fail closed");
    let message = unknown_message(error);

    assert!(
        message.contains("guest-egress NetworkPolicy did not converge to the desired spec"),
        "{message}"
    );
    assert!(message.contains("[ingress] changed"), "{message}");
    assert!(message.contains("8080"), "{message}");
    // Only the drifted field is named; the canonicalized-away difference and
    // every unchanged field stay silent.
    assert_eq!(message.matches(';').count(), 1, "{message}");
    assert!(!message.contains("[egress]"), "{message}");
    assert!(!message.contains("[podSelector]"), "{message}");
    assert!(!message.contains("[policyTypes]"), "{message}");
    assert_bounded_without_metadata_leakage(&message);
    assert!(
        message.contains(&format!("desired_spec_sha={}", spec_digest(&desired_spec))),
        "{message}"
    );
    assert!(
        message.contains(&format!("observed_spec_sha={}", spec_digest(&drifted_spec))),
        "{message}"
    );

    let store = Store::open(&sqlite).unwrap();
    let row = store.get_policy(POLICY_NAME).unwrap().expect("policy row");
    assert_eq!(row.observed_state, "drifted");
    assert_eq!(
        row.observed_spec_sha.as_deref(),
        Some(spec_digest(&drifted_spec).as_str())
    );
}

#[tokio::test]
async fn changed_egress_still_fails_closed_with_field_evidence() {
    let tag = "egress-port";
    let dir = temp_dir(tag);
    let live = Live::from_config(&test_config("kubectl", dir.join("provisional.sqlite"))).unwrap();
    let desired_spec = live.desired_network_policy_spec();
    // The DNS rule quietly re-pointed at DoT-style port 5353.
    let drifted_spec = mutated_spec(&live, |spec| {
        spec.insert(
            "egress".into(),
            serde_json::json!([{
                "to": [{
                    "namespaceSelector": {
                        "matchLabels": {"kubernetes.io/metadata.name": "kube-system"}
                    }
                }],
                "ports": [
                    {"protocol": "UDP", "port": 5353},
                    {"protocol": "TCP", "port": 5353}
                ]
            }]),
        );
    });

    let program = fake_kubectl(
        &dir,
        &serde_json::to_string(&api_server_object(&drifted_spec)).expect("object serializes"),
    );
    let sqlite = dir.join("state.sqlite");
    let config = test_config(program.to_str().unwrap(), sqlite.clone());
    let fabric = Fabric::open(config.clone(), Live::from_config(&config).unwrap()).unwrap();

    let error = fabric
        .ensure_network_policy()
        .await
        .expect_err("changed egress rules are real drift and must fail closed");
    let message = unknown_message(error);

    assert!(
        message.contains("guest-egress NetworkPolicy did not converge to the desired spec"),
        "{message}"
    );
    assert!(message.contains("[egress] changed"), "{message}");
    assert!(message.contains("5353"), "{message}");
    assert!(!message.contains("[ingress]"), "{message}");
    assert!(!message.contains("[podSelector]"), "{message}");
    assert!(!message.contains("[policyTypes]"), "{message}");
    // Unbounded server metadata never leaks; evidence stays diagnostic-sized.
    assert_bounded_without_metadata_leakage(&message);
    assert!(
        message.contains(&format!("desired_spec_sha={}", spec_digest(&desired_spec))),
        "{message}"
    );
    assert!(
        message.contains(&format!("observed_spec_sha={}", spec_digest(&drifted_spec))),
        "{message}"
    );

    let store = Store::open(&sqlite).unwrap();
    let row = store.get_policy(POLICY_NAME).unwrap().expect("policy row");
    assert_eq!(row.observed_state, "drifted");
    assert_eq!(row.desired_spec_sha, spec_digest(&desired_spec));
    assert_eq!(
        row.observed_spec_sha.as_deref(),
        Some(spec_digest(&drifted_spec).as_str())
    );

    // Exactly the expected cluster conversation: observe, converge, re-observe.
    assert_eq!(
        kubectl_call_log(&program),
        vec![
            format!("get networkpolicy {POLICY_NAME} -n {NAMESPACE} -o json"),
            "apply -f -".to_owned(),
            format!("get networkpolicy {POLICY_NAME} -n {NAMESPACE} -o json"),
        ],
        "one apply must be attempted before confirmation"
    );
}

#[tokio::test]
async fn pruned_ingress_without_ingress_isolation_still_fails_closed() {
    let tag = "no-ingress-type";
    let dir = temp_dir(tag);
    let live = Live::from_config(&test_config("kubectl", dir.join("provisional.sqlite"))).unwrap();
    // The adversarial shape: ingress key pruned AND Ingress dropped from
    // policyTypes. Without the isolation declaration the omission no longer
    // means what the desired default-deny means, so the equivalence gate
    // must refuse to fire and both fields must surface as drift.
    let drifted_spec = mutated_spec(&live, |spec| {
        spec.remove("ingress");
        spec.insert("policyTypes".into(), serde_json::json!(["Egress"]));
    });

    let program = fake_kubectl(
        &dir,
        &serde_json::to_string(&api_server_object(&drifted_spec)).expect("object serializes"),
    );
    let sqlite = dir.join("state.sqlite");
    let config = test_config(program.to_str().unwrap(), sqlite.clone());
    let fabric = Fabric::open(config.clone(), Live::from_config(&config).unwrap()).unwrap();

    let error = fabric
        .ensure_network_policy()
        .await
        .expect_err("dropping Ingress isolation must defeat the ingress equivalence");
    let message = unknown_message(error);

    assert!(
        message.contains("[ingress] missing-from-observed desired=[]"),
        "{message}"
    );
    assert!(message.contains("[policyTypes] changed"), "{message}");
    assert!(!message.contains("[egress]"), "{message}");
    assert!(!message.contains("[podSelector]"), "{message}");
    assert_bounded_without_metadata_leakage(&message);

    let store = Store::open(&sqlite).unwrap();
    let row = store.get_policy(POLICY_NAME).unwrap().expect("policy row");
    assert_eq!(row.observed_state, "drifted");
    assert_eq!(
        row.observed_spec_sha.as_deref(),
        Some(spec_digest(&drifted_spec).as_str())
    );
}

#[tokio::test]
async fn converged_canonical_object_confirms_without_false_drift() {
    let tag = "converged";
    let dir = temp_dir(tag);
    let live = Live::from_config(&test_config("kubectl", dir.join("provisional.sqlite"))).unwrap();
    let desired_spec = live.desired_network_policy_spec();

    // The API server echoes back exactly the desired spec inside a fully
    // decorated object; only spec participates in the comparison.
    let program = fake_kubectl(
        &dir,
        &serde_json::to_string(&api_server_object(&desired_spec)).expect("object serializes"),
    );
    let sqlite = dir.join("state.sqlite");
    let config = test_config(program.to_str().unwrap(), sqlite.clone());
    let fabric = Fabric::open(config.clone(), Live::from_config(&config).unwrap()).unwrap();

    fabric
        .ensure_network_policy()
        .await
        .expect("metadata noise must not cause false drift");

    let store = Store::open(&sqlite).unwrap();
    let row = store.get_policy(POLICY_NAME).unwrap().expect("policy row");
    assert_eq!(row.observed_state, "present");
    assert_eq!(row.desired_spec_sha, spec_digest(&desired_spec));
    assert_eq!(
        row.observed_spec_sha.as_deref(),
        Some(spec_digest(&desired_spec).as_str())
    );
}
