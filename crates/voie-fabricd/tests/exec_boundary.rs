//! Focused contract tests for the fabricd -> runner exec boundary.
//!
//! The corrected `voie-runner` preserves the post-`--` vector verbatim and
//! never implies a shell, so fabricd must compose guest commands as an exact
//! argv vector. These tests pin that composition with a fake kubectl that
//! records its argv, and pin the outcome classification of runner-owned
//! exit statuses.

use std::path::{Path, PathBuf};

use voie_fabricd::Config;
use voie_fabricd::{ExecVerdict, Live, classify_exec};

fn test_config(tag: &str, kubectl_program: &str) -> Config {
    let sqlite = std::env::temp_dir().join(format!(
        "voie-fabricd-exec-{}-{tag}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&sqlite);
    Config {
        bind: "localhost:0".into(),
        sqlite,
        node_name: "node-under-test".into(),
        namespace: "voie-workspace".into(),
        storage_class: "voie-workspace-block".into(),
        runtime_class: "voie-firecracker".into(),
        runtime_handler: "kata-fc-rs-voie".into(),
        runner_image: "voie-runner:c1".into(),
        jailer_root: PathBuf::from("/run/kata-containers/shared/firecracker"),
        vg: "voie-ws".into(),
        lv_size: "1G".into(),
        residue_wait_secs: 120,
        runtime_class_wait_secs: 60,
        kubectl_program: kubectl_program.to_owned(),
        kubectl_prefix: vec![],
        kubeconfig: None,
        crictl_program: "k3s".into(),
        crictl_prefix: vec!["crictl".into()],
        tls_cert: PathBuf::from("/tmp/voie-fabricd-test-cert.pem"),
        tls_key: PathBuf::from("/tmp/voie-fabricd-test-key.pem"),
        tls_ca: PathBuf::from("/tmp/voie-fabricd-test-ca.pem"),
        approved_egress: None,
        client_sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
    }
}

/// Writes a fake kubectl that records its full argv (NUL-separated) into
/// a fixed capture file and always exits 0.
fn fake_kubectl(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("voie-fabricd-fake-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("fake bin dir");
    let program = dir.join("kubectl");
    let capture = dir.join("argv.bin");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > '{}'\nexit 0\n",
            capture.display()
        ),
    )
    .expect("write fake kubectl");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake kubectl");
    std::fs::write(&capture, b"").expect("truncate argv capture");
    program
}

fn captured_argv(program: &Path) -> Vec<String> {
    let capture = program.parent().unwrap().join("argv.bin");
    let bytes = std::fs::read(capture).expect("argv capture exists");
    bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

#[tokio::test]
async fn exec_composes_shell_as_explicit_argv_never_joined() {
    let tag = "argv";
    let program = fake_kubectl(tag);
    let live = Live::from_config(&test_config(tag, program.to_str().unwrap())).unwrap();

    // The command text is one shell script; it must travel as ONE argv
    // element behind -c, never split, joined, or re-wrapped.
    let command = "printf marker > /workspace/marker && cat /workspace/marker";
    live.exec_runner("pod-e1", command, 30_000).await.unwrap();

    let argv = captured_argv(&program);
    let separator = argv
        .iter()
        .position(|arg| arg == "--")
        .expect("`--` present");
    assert_eq!(
        &argv[separator + 1..],
        &[
            "/bin/voie-runner".to_owned(),
            "--timeout-ms".to_owned(),
            "30000".to_owned(),
            "--".to_owned(),
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            command.to_owned(),
        ],
        "post-`--` vector must be the exact composed argv: {argv:?}"
    );
    assert!(argv.contains(&"pod-e1".to_owned()));
    assert!(argv.windows(2).any(|pair| pair == ["exec", "-n"]));
}

#[test]
fn runner_owned_statuses_are_never_terminal() {
    // 124: the runner killed the group at its own deadline.
    assert_eq!(classify_exec(124, ""), ExecVerdict::Unknown);
    // 125: the runner failed to start or wait for the program.
    assert_eq!(classify_exec(125, ""), ExecVerdict::Unknown);
    // Transport failures between fabricd and the pod are unresolved attempts.
    for stderr in [
        "Unable to upgrade connection: pod not found",
        "error upgrading connection: EOF",
        "lost connection to pod",
    ] {
        assert_eq!(classify_exec(1, stderr), ExecVerdict::Unknown, "{stderr}");
    }
    // Everything else is the guest program's own status, including failure.
    assert_eq!(classify_exec(0, ""), ExecVerdict::Terminal(0));
    assert_eq!(classify_exec(3, ""), ExecVerdict::Terminal(3));
}
