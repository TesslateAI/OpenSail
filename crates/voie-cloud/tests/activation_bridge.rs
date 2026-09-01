//! Focused ACT1 acceptance for the repaired activation bridge.
//!
//! Proves on every run:
//! - a real Node child mounts over one inherited fd-3 connection;
//! - no runtime pnpm install/build happens anywhere in the activation path;
//! - the sweep leaves exactly stdio + fd 3 visible to the child kernel-side,
//!   attested by the child itself (actual observed state, not metadata);
//! - scripted non-tool reply, typed tool-call data, explicit unknown bash
//!   outcome, durable append-before-effect with real event bytes;
//! - clean child exit; canary secrets absent from argv/env/bootstrap.

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{LazyLock, Mutex};

use serde_json::json;
use uuid::Uuid;

use voie_cloud::activation::{
    ActivationContext, ActivationError, ActivationHost, ActivationMode, ActivationOutcome,
    ActivationRequest, AppendReceipt, BashIntent, BashResult, ChildInputs, ModelRelay,
    ModelRequest, ModelResponse, NoopProduct, ScriptedModel, SessionPersistence,
    SyntheticWorkspace, UnknownWorkspace, WorkspaceExec, run,
};
use voie_cloud::activation::{ChildAttestation, verify_attestation};

const SECRET_KEYS: &[&str] = &[
    "DEEPSEEK_API_KEY",
    "OPENAI_API_KEY",
    "VOIE_DATABASE_URL",
    "DATABASE_URL",
    "PGPASSWORD",
    "AZURE_CLIENT_SECRET",
    "AZURE_CLIENT_ID",
    "AZURE_TENANT_ID",
    "AZURE_STORAGE_KEY",
    "HEADSCALE_KEY",
    "WORKSPACE_BEARER",
    "OIDC_CLIENT_SECRET",
    "FABRIC_CERT",
    "FABRIC_KEY",
];

/// Serializes every test that mutates process environment variables.
static ENV_SERIAL: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Poisoning-tolerant acquisition: a panicked test must not wedge siblings.
fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    match ENV_SERIAL.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

static PARENT_SECRETS: LazyLock<()> = LazyLock::new(|| {
    for key in SECRET_KEYS {
        // These values exist only in the parent test process.
        unsafe { std::env::set_var(key, "parent-secret-must-not-enter-child") };
    }
});

fn install_parent_secrets() {
    LazyLock::force(&PARENT_SECRETS);
}

fn context() -> ActivationContext {
    ActivationContext {
        project_id: Uuid::new_v4(),
        agent_id: Uuid::new_v4(),
        session_id: Uuid::new_v4(),
        run_id: Uuid::new_v4(),
        workspace_id: Uuid::new_v4(),
        writer_generation: 1,
        bash_enabled: true,
    }
}

fn canary_path() -> PathBuf {
    static CANARY: LazyLock<Mutex<Option<PathBuf>>> = LazyLock::new(|| Mutex::new(None));
    let mut slot = CANARY.lock().expect("canary lock");
    if let Some(path) = slot.as_ref() {
        return path.clone();
    }
    let dir = std::env::temp_dir().join(format!("voie-act-canary-{}", Uuid::new_v4()));
    fs::create_dir_all(&dir).expect("canary dir");
    let bash = dir.join("bash");
    let marker = dir.join("executed");
    fs::write(
        &bash,
        format!(
            "#!/bin/sh\necho executed > '{}'\nexit 0\n",
            marker.display()
        ),
    )
    .expect("canary bash");
    let mut permissions = fs::metadata(&bash).expect("canary metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&bash, permissions).expect("canary chmod");
    unsafe {
        std::env::set_var(
            "VOIE_ACTIVATION_PATH",
            format!("{}:/usr/bin:/bin", dir.display()),
        )
    };
    *slot = Some(marker.clone());
    marker
}

/// Dev-time provisioning of the immutable dist entry. The activation library
/// never installs or builds; this helper plays the role of `just
/// activation-dist` / `nix build .#activation-dist` inside the focused test.
fn ensure_provisioned() {
    if std::env::var_os("VOIE_ACTIVATION_ENTRY").is_some() {
        return;
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../activation");
    let entry = root.join("dist/index.js");
    if !entry.is_file() {
        for args in [["install", "--frozen-lockfile"], ["run", "build"]] {
            let status = Command::new("pnpm")
                .args(args)
                .current_dir(&root)
                .status()
                .expect("dev shell pnpm");
            assert!(status.success(), "dev provisioning step {args:?} failed");
        }
    }
}

fn assert_child_has_no_secrets(inputs: &ChildInputs) {
    let blob = format!(
        "{}\n{}\n{}",
        inputs.argv.join(" "),
        inputs
            .env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("\n"),
        inputs.bootstrap
    );
    for key in SECRET_KEYS {
        assert!(
            !blob.contains(key),
            "protected credential {key} entered child inputs"
        );
    }
    assert!(
        !blob.contains("parent-secret-must-not-enter-child"),
        "protected credential value entered child inputs"
    );
}

fn assert_boundary_attested(outcome: &ActivationOutcome) {
    assert_eq!(
        outcome.child_attestation.fds,
        vec![3],
        "exactly one inherited endpoint: the bridge"
    );
    let mut env_keys = outcome.child_attestation.env_keys.clone();
    env_keys.sort();
    assert_eq!(
        env_keys,
        vec!["HOME", "LANG", "PATH", "TMPDIR"],
        "minimal environment"
    );
}

/// Orders durability appends against effects across one activation.
#[derive(Default)]
struct OrderLog {
    entries: Mutex<Vec<&'static str>>,
}

impl OrderLog {
    fn push(&self, label: &'static str) {
        self.entries.lock().expect("order log").push(label);
    }

    fn snapshot(&self) -> Vec<&'static str> {
        self.entries.lock().expect("order log").clone()
    }
}

struct RecordingSessions<'a> {
    inner: &'a MemorySessions,
    order: &'a OrderLog,
    received: Mutex<Vec<(Uuid, Uuid, Vec<u8>)>>,
}

/// Test-local in-memory session log recording every append with its bytes.
#[derive(Default)]
struct MemorySessions {
    appends: std::sync::Mutex<Vec<(Uuid, Uuid, Vec<u8>)>>,
}

impl SessionPersistence for MemorySessions {
    fn append_events(
        &self,
        session_id: Uuid,
        append_id: Uuid,
        event_bytes: &[u8],
    ) -> impl Future<Output = Result<AppendReceipt, ActivationError>> + Send {
        self.appends.lock().expect("session append lock").push((
            session_id,
            append_id,
            event_bytes.to_vec(),
        ));
        async move { Ok(AppendReceipt { append_id }) }
    }
}

impl SessionPersistence for RecordingSessions<'_> {
    fn append_events(
        &self,
        session_id: Uuid,
        append_id: Uuid,
        event_bytes: &[u8],
    ) -> impl Future<Output = Result<AppendReceipt, ActivationError>> + Send {
        self.order.push("append");
        self.received.lock().expect("received lock").push((
            session_id,
            append_id,
            event_bytes.to_vec(),
        ));
        self.inner.append_events(session_id, append_id, event_bytes)
    }
}

struct RecordingModel<'a> {
    inner: &'a ScriptedModel,
    order: &'a OrderLog,
}

impl ModelRelay for RecordingModel<'_> {
    fn complete(
        &self,
        request: ModelRequest,
    ) -> impl Future<Output = Result<ModelResponse, ActivationError>> + Send {
        self.order.push("effect:model");
        self.inner.complete(request)
    }
}

struct RecordingWorkspace<'a> {
    inner: &'a SyntheticWorkspace,
    order: &'a OrderLog,
}

impl WorkspaceExec for RecordingWorkspace<'_> {
    fn bash(
        &self,
        intent: BashIntent,
    ) -> impl Future<Output = Result<BashResult, ActivationError>> + Send {
        self.order.push("effect:bash");
        self.inner.bash(intent)
    }
}

struct ForbiddenWorkspace;

impl WorkspaceExec for ForbiddenWorkspace {
    fn bash(
        &self,
        _intent: BashIntent,
    ) -> impl Future<Output = Result<BashResult, ActivationError>> + Send {
        async {
            Err(ActivationError::Protocol(
                "workspace bash handler must not run",
            ))
        }
    }
}

#[tokio::test]
async fn activation_bridge_scripted_text_and_tool_paths() {
    let _env_guard = lock_env();
    install_parent_secrets();
    let canary = canary_path();
    if canary.exists() {
        fs::remove_file(&canary).expect("reset canary");
    }
    ensure_provisioned();

    // --- Text path ---------------------------------------------------------
    let text_context = context();
    let sessions = MemorySessions::default();
    let order = OrderLog::default();
    let text_model =
        ScriptedModel::new([ModelResponse::Text("scripted non-tool reply".to_owned())]);
    let outcome = run(
        ActivationHost {
            context: text_context,
            model: &RecordingModel {
                inner: &text_model,
                order: &order,
            },
            workspace: &SyntheticWorkspace {
                stdout: "workspace-ok\n".to_owned(),
            },
            sessions: &RecordingSessions {
                inner: &sessions,
                order: &order,
                received: Mutex::new(Vec::new()),
            },
            product: &NoopProduct,
        },
        ActivationRequest {
            mode: ActivationMode::Create,
            prompt: "reply without tools".to_owned(),
        },
    )
    .await
    .expect("text-only activation");

    assert!(
        outcome.child_opened_connection,
        "real Node child opened FD 3"
    );
    assert_eq!(outcome.final_text, "scripted non-tool reply");
    assert!(
        outcome.bash_intents.is_empty(),
        "text path must not emit bash"
    );
    assert_eq!(outcome.child_exit_code, 0, "child exits cleanly");
    assert_boundary_attested(&outcome);
    assert_child_has_no_secrets(&outcome.child_inputs);

    // --- Typed tool-call path ----------------------------------------------
    let tool_context = context();
    let tool_sessions = MemorySessions::default();
    let tool_order = OrderLog::default();
    let arguments =
        json!({ "command": "echo voie-act1", "description": "Print activation marker" });
    let tool_model = ScriptedModel::new([
        ModelResponse::ToolCall {
            call_id: "call_bash_1".to_owned(),
            name: "bash".to_owned(),
            arguments_json: arguments.to_string(),
        },
        ModelResponse::Text("final after bash".to_owned()),
    ]);
    let tool_outcome = run(
        ActivationHost {
            context: tool_context,
            model: &RecordingModel {
                inner: &tool_model,
                order: &tool_order,
            },
            workspace: &RecordingWorkspace {
                inner: &SyntheticWorkspace {
                    stdout: "workspace-ok\n".to_owned(),
                },
                order: &tool_order,
            },
            sessions: &RecordingSessions {
                inner: &tool_sessions,
                order: &tool_order,
                received: Mutex::new(Vec::new()),
            },
            product: &NoopProduct,
        },
        ActivationRequest {
            mode: ActivationMode::Create,
            prompt: "run one bash command".to_owned(),
        },
    )
    .await
    .expect("bash-intent activation");

    assert!(tool_outcome.child_opened_connection);
    assert_eq!(
        tool_outcome.bash_intents.len(),
        1,
        "exactly one bash intent"
    );
    assert_eq!(tool_outcome.bash_intents[0].command, "echo voie-act1");
    assert_eq!(
        tool_outcome.bash_intents[0].call_id, "call_bash_1",
        "model-issued call id must reach the workspace seam"
    );
    assert_eq!(tool_outcome.final_text, "final after bash");
    assert_eq!(tool_outcome.child_exit_code, 0, "child exits cleanly");
    assert!(!canary.exists(), "no host-local bash executed");
    assert_boundary_attested(&tool_outcome);
    assert_child_has_no_secrets(&tool_outcome.child_inputs);

    // Durability precedes every effect; finish flush lands last.
    assert_eq!(
        tool_order.snapshot(),
        vec![
            "append",
            "effect:model",
            "append",
            "effect:bash",
            "append",
            "effect:model",
            "append"
        ],
        "append-before-effect discipline"
    );

    // Appended bytes are real serialized events bound to the owned session.
    let appends = tool_sessions.appends.lock().expect("appends lock").clone();
    assert_eq!(
        appends.len(),
        4,
        "model, bash, second model, and finish flushes"
    );
    assert!(
        appends
            .iter()
            .all(|(session, _, _)| *session == tool_context.session_id)
    );
    let ids: HashSet<Uuid> = appends.iter().map(|(_, append_id, _)| *append_id).collect();
    assert_eq!(
        ids.len(),
        4,
        "each logical event keeps its own stable append id"
    );
    assert!(
        String::from_utf8_lossy(&appends[0].2).contains("run one bash command"),
        "first flush carries the prompt event bytes"
    );

    // The typed tool call reached DSH with its authored arguments verbatim.
    let routed = tool_model.routed_requests();
    let settlement_round = &routed[1].messages;
    assert!(
        settlement_round.iter().any(|message| {
            message.text.contains("workspace-ok")
                || message
                    .tool_results
                    .iter()
                    .any(|result| result.text.contains("workspace-ok"))
        }),
        "second round carries the synthetic bash settlement"
    );
    let assistant_call = settlement_round
        .iter()
        .flat_map(|message| message.tool_calls.iter())
        .find(|call| call.name == "bash")
        .expect("typed tool call round-trips");
    assert_eq!(assistant_call.id, "call_bash_1");
    assert!(assistant_call.arguments.contains("echo voie-act1"));
}

#[tokio::test]
async fn activation_bridge_preserves_unknown_outcome() {
    let _env_guard = lock_env();
    install_parent_secrets();
    ensure_provisioned();
    let unknown_context = context();
    let reason = "fabric link lost before settlement";
    let unknown_model = ScriptedModel::new([
        ModelResponse::ToolCall {
            call_id: "call_bash_unknown".to_owned(),
            name: "bash".to_owned(),
            arguments_json:
                json!({ "command": "echo unseen", "description": "Print unseen marker" })
                    .to_string(),
        },
        ModelResponse::Text("final after unknown".to_owned()),
    ]);
    let sessions = MemorySessions::default();
    let outcome = run(
        ActivationHost {
            context: unknown_context,
            model: &unknown_model,
            workspace: &UnknownWorkspace {
                reason: reason.to_owned(),
            },
            sessions: &sessions,
            product: &NoopProduct,
        },
        ActivationRequest {
            mode: ActivationMode::Create,
            prompt: "lose the fabric link".to_owned(),
        },
    )
    .await
    .expect("unknown-outcome activation");

    assert_eq!(outcome.final_text, "final after unknown");
    assert_eq!(outcome.child_exit_code, 0);
    assert_boundary_attested(&outcome);
    assert_child_has_no_secrets(&outcome.child_inputs);

    // The explicit unknown reached DSH as the rendered tool result: the next
    // routed model request carries the authored reason verbatim instead of a
    // fabricated exit code.
    let routed = unknown_model.routed_requests();
    assert!(
        routed.into_iter().skip(1).any(|request| {
            request.messages.iter().any(|message| {
                message.text.contains(reason)
                    || message
                        .tool_results
                        .iter()
                        .any(|result| result.text.contains(reason))
            })
        }),
        "unknown outcome preserved explicitly through the bridge"
    );

    let appends = sessions.appends.lock().expect("appends lock").clone();
    assert!(!appends.is_empty());
    let session_ids: HashSet<Uuid> = appends.iter().map(|(session, _, _)| *session).collect();
    assert_eq!(session_ids, HashSet::from([unknown_context.session_id]));
    let append_ids: HashSet<Uuid> = appends.iter().map(|(_, append_id, _)| *append_id).collect();
    assert_eq!(
        append_ids.len(),
        appends.len(),
        "append ids are unique per logical event"
    );
}

#[tokio::test]
async fn bash_disabled_refuses_bash_before_the_workspace_handler() {
    let _env_guard = lock_env();
    install_parent_secrets();
    ensure_provisioned();
    let mut disabled = context();
    disabled.bash_enabled = false;
    let model = ScriptedModel::new([
        ModelResponse::ToolCall {
            call_id: "call_bash_disabled".to_owned(),
            name: "bash".to_owned(),
            arguments_json: json!({ "command": "echo forbidden" }).to_string(),
        },
        ModelResponse::Text("must not run".to_owned()),
    ]);
    let error = run(
        ActivationHost {
            context: disabled,
            model: &model,
            workspace: &ForbiddenWorkspace,
            sessions: &MemorySessions::default(),
            product: &NoopProduct,
        },
        ActivationRequest {
            mode: ActivationMode::Create,
            prompt: "run bash".to_owned(),
        },
    )
    .await
    .err()
    .expect("disabled bash must fail closed");
    assert!(
        matches!(
            error,
            ActivationError::Protocol("model returned an unauthorized tool")
                | ActivationError::Protocol("bash is not enabled for this agent")
        ),
        "{error}"
    );
}

#[tokio::test]
async fn activation_consumes_prebuilt_artifact_and_never_provisions() {
    let _env_guard = lock_env();
    install_parent_secrets();

    // A missing entry is a hard error; nothing spawns pnpm to fix it.
    unsafe { std::env::set_var("VOIE_ACTIVATION_ENTRY", "/nonexistent/voie/dist/index.js") };
    let missing = run(
        ActivationHost {
            context: context(),
            model: &ScriptedModel::new([ModelResponse::Text("unused".to_owned())]),
            workspace: &SyntheticWorkspace {
                stdout: String::new(),
            },
            sessions: &MemorySessions::default(),
            product: &NoopProduct,
        },
        ActivationRequest {
            mode: ActivationMode::Create,
            prompt: "unused".to_owned(),
        },
    )
    .await
    .err()
    .expect("missing entry must fail");
    assert!(matches!(
        missing,
        ActivationError::Child("voie_activation_entry_missing")
    ));

    // The Nix store artifact drives the same real-child path green.
    let artifact = std::process::Command::new("nix")
        .args([
            "build",
            "--no-link",
            "--print-out-paths",
            ".#voie-activation-dist",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("nix build for artifact path");
    assert!(
        artifact.status.success(),
        "nix build voie-activation-dist failed"
    );
    let out = String::from_utf8_lossy(&artifact.stdout).trim().to_owned();
    unsafe {
        std::env::set_var(
            "VOIE_ACTIVATION_ENTRY",
            format!("{out}/lib/voie-activation/dist/index.js"),
        )
    };

    let model = ScriptedModel::new([ModelResponse::Text("prebuilt artifact reply".to_owned())]);
    let outcome = run(
        ActivationHost {
            context: context(),
            model: &model,
            workspace: &SyntheticWorkspace {
                stdout: String::new(),
            },
            sessions: &MemorySessions::default(),
            product: &NoopProduct,
        },
        ActivationRequest {
            mode: ActivationMode::Create,
            prompt: "reply from the store artifact".to_owned(),
        },
    )
    .await
    .expect("artifact-backed activation");
    assert_eq!(outcome.final_text, "prebuilt artifact reply");
    assert_eq!(outcome.child_exit_code, 0);
    assert_boundary_attested(&outcome);
    unsafe { std::env::remove_var("VOIE_ACTIVATION_ENTRY") };
}

#[tokio::test]
async fn replay_guard_rejects_repeated_workspace_call() {
    let workspace = Uuid::new_v4();
    let guard = voie_cloud::activation::ReplayGuard::default();
    guard
        .admit(workspace, "call_bash_1")
        .expect("first admission");
    assert!(
        guard.admit(workspace, "call_bash_1").is_err(),
        "same (workspace, call) pair must be rejected as a replay"
    );
    let other_workspace = Uuid::new_v4();
    guard
        .admit(other_workspace, "call_bash_1")
        .expect("the same call id under another workspace is distinct");
}

fn exact_attestation() -> ChildAttestation {
    ChildAttestation {
        fds: vec![3],
        env_keys: ["HOME", "LANG", "PATH", "TMPDIR"]
            .iter()
            .map(|key| key.to_string())
            .collect(),
    }
}

#[test]
fn boundary_verification_requires_exact_descriptor_and_environment_sets() {
    verify_attestation(&exact_attestation()).expect("the proven boundary passes");

    // Missing bridge endpoint in every shape must fail like an intrusion.
    for fds in [Vec::new(), vec![0, 1, 2], vec![0, 1, 2, 4]] {
        let candidate = ChildAttestation {
            fds: fds.clone(),
            env_keys: exact_attestation().env_keys,
        };
        assert!(
            verify_attestation(&candidate).is_err(),
            "fds {fds:?} lack exactly fd 3"
        );
    }

    // One extra inherited socket is a violation even with fd 3 present.
    let extra_fd = ChildAttestation {
        fds: vec![3, 7],
        env_keys: exact_attestation().env_keys,
    };
    assert!(
        verify_attestation(&extra_fd).is_err(),
        "extra socket must fail"
    );

    // A missing environment key is a violation.
    let missing_key = ChildAttestation {
        fds: vec![3],
        env_keys: ["HOME", "LANG", "TMPDIR"]
            .iter()
            .map(|key| key.to_string())
            .collect(),
    };
    assert!(
        verify_attestation(&missing_key).is_err(),
        "missing PATH must fail"
    );

    // An extra environment key is a violation.
    let mut extra_env = exact_attestation().env_keys;
    extra_env.push("AZURE_CLIENT_SECRET".to_owned());
    let extra_key = ChildAttestation {
        fds: vec![3],
        env_keys: extra_env,
    };
    assert!(
        verify_attestation(&extra_key).is_err(),
        "extra environment key must fail"
    );

    // Comparison is canonical: unsorted-but-exact reports pass.
    let unsorted = ChildAttestation {
        fds: vec![3],
        env_keys: ["PATH", "HOME", "TMPDIR", "LANG"]
            .iter()
            .map(|key| key.to_string())
            .collect(),
    };
    verify_attestation(&unsorted).expect("order-independent exact match");
}
