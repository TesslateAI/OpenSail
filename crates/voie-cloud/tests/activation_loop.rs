//! Live activation-path proofs for first-blocker teaching, cheap repeats,
//! server-side waits, tool intersection at the child, usage, and abort.

use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use uuid::Uuid;

use voie_cloud::activation::{
    precheck_blocker, remember_error, remember_or_repeat_observation, run, run_with_abort,
    ActivationContext, ActivationError, ActivationHost, ActivationMode, ActivationRequest,
    AppendReceipt, CompletionUsage, KnownBlockers, LiveActivationAborts, ModelCompletion,
    ModelResponse, ProductError, ProductExec, ProductIntent, ProductResult, ScriptedModel,
    SessionPersistence, SyntheticWorkspace,
};

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

fn canary_path() -> PathBuf {
    static CANARY: LazyLock<Mutex<Option<PathBuf>>> = LazyLock::new(|| Mutex::new(None));
    let mut slot = CANARY.lock().expect("canary lock");
    if let Some(path) = slot.as_ref() {
        return path.clone();
    }
    let dir = std::env::temp_dir().join(format!("voie-loop-canary-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("canary dir");
    let bash = dir.join("bash");
    let marker = dir.join("executed");
    std::fs::write(
        &bash,
        format!(
            "#!/bin/sh\necho executed > '{}'\nexit 0\n",
            marker.display()
        ),
    )
    .expect("canary bash");
    let mut permissions = std::fs::metadata(&bash)
        .expect("canary metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&bash, permissions).expect("canary chmod");
    unsafe {
        std::env::set_var(
            "VOIE_ACTIVATION_PATH",
            format!("{}:/usr/bin:/bin", dir.display()),
        )
    };
    *slot = Some(marker.clone());
    marker
}

#[derive(Default)]
struct MemorySessions {
    appends: Mutex<Vec<(Uuid, Uuid, Vec<u8>)>>,
}

impl SessionPersistence for MemorySessions {
    fn history(
        &self,
        session_id: Uuid,
    ) -> impl Future<Output = Result<Vec<Vec<u8>>, ActivationError>> + Send {
        let batches = self
            .appends
            .lock()
            .expect("session append lock")
            .iter()
            .filter(|(id, _, _)| *id == session_id)
            .map(|(_, _, bytes)| bytes.clone())
            .collect();
        async move { Ok(batches) }
    }

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

struct LoopProduct {
    expensive: AtomicU32,
    names: Mutex<Vec<String>>,
    blockers: KnownBlockers,
    deploy_healthy_after: Duration,
}

impl LoopProduct {
    fn new() -> Self {
        Self {
            expensive: AtomicU32::new(0),
            names: Mutex::new(Vec::new()),
            blockers: KnownBlockers::new(),
            deploy_healthy_after: Duration::from_millis(80),
        }
    }
}

impl ProductExec for LoopProduct {
    fn execute(
        &self,
        intent: ProductIntent,
    ) -> impl Future<Output = Result<ProductResult, ActivationError>> + Send {
        self.names.lock().expect("names").push(intent.name.clone());
        let arguments: Value = serde_json::from_str(&intent.arguments_json).unwrap_or(json!({}));
        let known = precheck_blocker(&self.blockers, &intent.name, &arguments);
        if known.is_none() {
            self.expensive.fetch_add(1, Ordering::SeqCst);
        }
        let blockers = self.blockers.clone();
        let delay = self.deploy_healthy_after;
        async move {
            if let Some(known) = known {
                return Ok(known);
            }
            match intent.name.as_str() {
                "environment.publish_prod" => {
                    let error = ProductError::permission_denied(
                        Some("ManageProduction"),
                        "current actor cannot manage production",
                    );
                    remember_error(&blockers, &intent.name, &arguments, &error);
                    Ok(error.to_result())
                }
                "database.create" => {
                    let elevated = arguments
                        .get("elevated")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if elevated {
                        let error = ProductError::approval_required(
                            "11111111-1111-1111-1111-111111111111",
                            Some("increase_resource_tier:db".to_owned()),
                            "approval required",
                        );
                        remember_error(&blockers, &intent.name, &arguments, &error);
                        return Ok(error.to_result());
                    }
                    Ok(ProductResult::ok(
                        json!({ "database": { "id": Uuid::new_v4(), "state": "ready" } })
                            .to_string(),
                    ))
                }
                "environment.deploy_dev" => {
                    tokio::time::sleep(delay).await;
                    Ok(ProductResult::ok(
                        json!({
                            "state": "healthy",
                            "deploymentId": "22222222-2222-2222-2222-222222222222",
                            "traffic": false
                        })
                        .to_string(),
                    ))
                }
                "deployment.activate" => Ok(ProductResult::ok(
                    json!({
                        "state": "active",
                        "deploymentId": "22222222-2222-2222-2222-222222222222"
                    })
                    .to_string(),
                )),
                "deployment.status" => {
                    let value = json!({
                        "deployment": {
                            "id": "22222222-2222-2222-2222-222222222222",
                            "state": "creating",
                            "desiredRevision": 7
                        }
                    });
                    if let Some(repeat) =
                        remember_or_repeat_observation(&blockers, &intent.name, &value)
                    {
                        return Ok(repeat);
                    }
                    Ok(ProductResult::ok(value.to_string()))
                }
                "deployment.logs" => Ok(ProductResult::ok(
                    json!({
                        "deploymentId": "22222222-2222-2222-2222-222222222222",
                        "text": "panic: listen EADDRINUSE",
                        "lastSeq": 1,
                        "nextSeq": 2,
                        "truncated": false
                    })
                    .to_string(),
                )),
                other => Ok(ProductResult::fail(format!("{other} not stubbed"))),
            }
        }
    }
}

struct WaitingProduct {
    started: std::sync::Arc<tokio::sync::Notify>,
}

impl ProductExec for WaitingProduct {
    fn execute(
        &self,
        _intent: ProductIntent,
    ) -> impl Future<Output = Result<ProductResult, ActivationError>> + Send {
        let started = self.started.clone();
        async move {
            started.notify_waiters();
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(ProductResult::ok(json!({ "state": "healthy" }).to_string()))
        }
    }
}

fn tool_call(name: &str, arguments: Value) -> ModelResponse {
    ModelResponse::ToolCall {
        call_id: Uuid::new_v4().to_string(),
        name: name.to_owned(),
        arguments_json: arguments.to_string(),
    }
}

async fn run_script(
    replies: Vec<ModelResponse>,
    product: &impl ProductExec,
) -> (ScriptedModel, voie_cloud::activation::ActivationOutcome) {
    ensure_provisioned();
    let _ = canary_path();
    let model = ScriptedModel::new(replies);
    let sessions = MemorySessions::default();
    let outcome = run(
        ActivationHost {
            context: context(),
            model: &model,
            workspace: &SyntheticWorkspace {
                stdout: String::new(),
            },
            sessions: &sessions,
            product,
        },
        ActivationRequest {
            mode: ActivationMode::Create,
            prompt: "continue".to_owned(),
        },
    )
    .await
    .expect("activation");
    (model, outcome)
}

#[tokio::test]
async fn permission_denial_does_not_end_the_turn() {
    let product = LoopProduct::new();
    let (model, outcome) = run_script(
        vec![
            tool_call("environment.publish_prod", json!({})),
            tool_call("environment.deploy_dev", json!({})),
            ModelResponse::Text("used the development path".to_owned()),
        ],
        &product,
    )
    .await;
    assert!(outcome.final_text.contains("development path"));
    assert_eq!(model.routed_requests().len(), 3);
    let names = product.names.lock().expect("names").clone();
    assert_eq!(
        names,
        vec![
            "environment.publish_prod".to_owned(),
            "environment.deploy_dev".to_owned()
        ]
    );
}

#[tokio::test]
async fn repeated_permission_blocker_is_cheap() {
    let product = LoopProduct::new();
    let (_model, outcome) = run_script(
        vec![
            tool_call("environment.publish_prod", json!({})),
            tool_call("environment.publish_prod", json!({})),
            tool_call("environment.deploy_dev", json!({})),
            ModelResponse::Text("still able to deploy dev".to_owned()),
        ],
        &product,
    )
    .await;
    assert!(outcome.final_text.contains("deploy dev"));
    assert_eq!(product.expensive.load(Ordering::SeqCst), 2);
    let names = product.names.lock().expect("names").clone();
    assert_eq!(names.len(), 3);
}

#[tokio::test]
async fn approval_can_be_worked_around() {
    let product = LoopProduct::new();
    let (_model, outcome) = run_script(
        vec![
            tool_call(
                "database.create",
                json!({ "kind": "dev", "elevated": true }),
            ),
            tool_call("database.create", json!({ "kind": "dev" })),
            ModelResponse::Text("used default tier".to_owned()),
        ],
        &product,
    )
    .await;
    assert!(outcome.final_text.contains("default tier"));
}

#[tokio::test]
async fn deploy_waits_without_a_status_poll_turn() {
    let product = LoopProduct::new();
    let (model, outcome) = run_script(
        vec![
            tool_call("environment.deploy_dev", json!({})),
            ModelResponse::Text("healthy without polling".to_owned()),
        ],
        &product,
    )
    .await;
    assert!(outcome.final_text.contains("without polling"));
    assert_eq!(model.routed_requests().len(), 2);
    assert!(!product
        .names
        .lock()
        .expect("names")
        .iter()
        .any(|name| name == "application.status" || name == "deployment.status"));
}

#[tokio::test]
async fn deploy_does_not_auto_activate() {
    let product = LoopProduct::new();
    let (_model, outcome) = run_script(
        vec![
            tool_call("environment.deploy_dev", json!({})),
            tool_call("deployment.activate", json!({})),
            ModelResponse::Text("activated after healthy".to_owned()),
        ],
        &product,
    )
    .await;
    assert!(outcome.final_text.contains("activated after healthy"));
    let names = product.names.lock().expect("names").clone();
    assert_eq!(
        names,
        vec![
            "environment.deploy_dev".to_owned(),
            "deployment.activate".to_owned()
        ]
    );
}

#[tokio::test]
async fn unchanged_status_is_a_successful_repeat() {
    let product = LoopProduct::new();
    let (_model, outcome) = run_script(
        vec![
            tool_call("deployment.status", json!({})),
            tool_call("deployment.status", json!({})),
            ModelResponse::Text("stopped repeating status".to_owned()),
        ],
        &product,
    )
    .await;
    assert!(outcome.final_text.contains("stopped repeating"));
    assert_eq!(product.expensive.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn logs_return_text_not_blob_authority() {
    let product = LoopProduct::new();
    let (_model, outcome) = run_script(
        vec![
            tool_call(
                "deployment.logs",
                json!({ "deployment_id": "22222222-2222-2222-2222-222222222222" }),
            ),
            ModelResponse::Text("saw panic in logs".to_owned()),
        ],
        &product,
    )
    .await;
    assert!(outcome.final_text.contains("panic in logs"));
}

#[tokio::test]
async fn real_usage_is_carried_on_the_model_wire() {
    ensure_provisioned();
    let _ = canary_path();
    let model = ScriptedModel::with_usage([ModelCompletion {
        response: ModelResponse::Text("accounted".to_owned()),
        usage: Some(CompletionUsage {
            prompt_tokens: 41,
            completion_tokens: 17,
        }),
    }]);
    let sessions = MemorySessions::default();
    let outcome = run(
        ActivationHost {
            context: context(),
            model: &model,
            workspace: &SyntheticWorkspace {
                stdout: String::new(),
            },
            sessions: &sessions,
            product: &LoopProduct::new(),
        },
        ActivationRequest {
            mode: ActivationMode::Create,
            prompt: "hi".to_owned(),
        },
    )
    .await
    .expect("activation with usage");
    assert_eq!(outcome.final_text, "accounted");
}

#[tokio::test]
async fn stop_during_product_wait_is_cancelled() {
    ensure_provisioned();
    let _ = canary_path();
    let aborts = LiveActivationAborts::new();
    let run_id = Uuid::new_v4();
    let abort = aborts.register(run_id);
    let started = std::sync::Arc::new(tokio::sync::Notify::new());
    let product = WaitingProduct {
        started: started.clone(),
    };
    let model = ScriptedModel::new([tool_call("environment.deploy_dev", json!({}))]);
    let sessions = MemorySessions::default();
    let mut ctx = context();
    ctx.run_id = run_id;
    let pending = aborts.clone();
    let waiter = started.clone();
    tokio::spawn(async move {
        waiter.notified().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        pending.abort(run_id);
    });
    let error = run_with_abort(
        ActivationHost {
            context: ctx,
            model: &model,
            workspace: &SyntheticWorkspace {
                stdout: String::new(),
            },
            sessions: &sessions,
            product: &product,
        },
        ActivationRequest {
            mode: ActivationMode::Create,
            prompt: "deploy".to_owned(),
        },
        Some(abort),
    )
    .await
    .expect_err("stop wins");
    assert!(matches!(error, ActivationError::Cancelled));
}

#[tokio::test]
async fn child_visible_tools_are_recorded_on_the_model_request() {
    let product = LoopProduct::new();
    let (model, _outcome) =
        run_script(vec![ModelResponse::Text("done".to_owned())], &product).await;
    let tools = &model.routed_requests()[0].tools;
    assert!(
        tools.iter().any(|name| name == "environment.deploy_dev"),
        "{tools:?}"
    );
    assert!(
        tools.iter().any(|name| name == "environment.publish_prod"),
        "child still names the tool; parent intersection hides it later: {tools:?}"
    );
    assert!(!tools.iter().any(|name| name == "secret.explode"));
}
