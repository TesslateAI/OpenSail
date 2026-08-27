//! Credentialless disposable DSH activation parent.
//!
//! The child is a real Node process bound to one inherited local connection.
//! Parent-side context is the authority: the child cannot select another
//! Project, Agent, Session, Run, or Workspace. Model keys, database, Blob,
//! Fabric, Workspace bearer, OIDC, Azure, and Headscale material never enter
//! the child environment, argv, bootstrap frame, or inherited descriptors.

mod host;

use std::future::Future;

use uuid::Uuid;

pub use host::{ActivationHost, ChildAttestation, artifacts_ready, run, verify_attestation};

/// Server-side identifiers bound to one inherited activation connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivationContext {
    pub project_id: Uuid,
    pub agent_id: Uuid,
    pub session_id: Uuid,
    pub run_id: Uuid,
    pub workspace_id: Uuid,
    pub writer_generation: i64,
}

/// Create a fresh DSH session or resume the parent-owned session identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationMode {
    Create,
    Resume,
}

/// One user prompt delivered through the parent-owned bootstrap frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationRequest {
    pub mode: ActivationMode,
    pub prompt: String,
}

/// Compact model request snapshot received from the child.
///
/// Messages keep their structure: visible text plus typed tool calls and
/// tool results, so the relay never re-parses content to reconstruct a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequest {
    pub system: Option<String>,
    pub tools: Vec<String>,
    pub messages: Vec<WireMessage>,
}

/// One conversation turn as routed by the child.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct WireMessage {
    pub role: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<WireToolCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_results: Vec<WireToolResult>,
}

/// Assistant tool invocation, passed through verbatim.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct WireToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Tool outcome returned to the model; `is_error` preserves failure typing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct WireToolResult {
    pub call_id: String,
    pub text: String,
    pub is_error: bool,
}

/// Scripted or live model completion returned by the parent.
///
/// Tool calls are carried as typed data end to end: the relay names the tool
/// and hands over the argument object it authored. Nothing on either side of
/// the bridge parses assistant content text to discover a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelResponse {
    Text(String),
    ToolCall {
        call_id: String,
        name: String,
        arguments_json: String,
    },
}

/// Bash tool intent the child must not execute itself. `call_id` is the
/// stable model-issued identity upstream stores use for no-replay decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashIntent {
    pub call_id: String,
    pub command: String,
    pub description: String,
}

/// Fixed Workspace Bash execution boundary. The model never chooses the
/// working directory or timeout; the control plane composes them.
pub const BASH_WORKDIR: &str = "/workspace";
pub const BASH_TIMEOUT_MS: u64 = 30_000;

/// Result the parent returns in place of Workspace execution.
///
/// Outcome uncertainty is explicit: when the remote authority disappears
/// before reporting a terminal status, [`BashOutcome::Unknown`] preserves
/// that fact instead of masquerading as a failure exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashResult {
    pub outcome: BashOutcome,
    pub stdout: String,
    pub stderr: String,
    pub timeout_ms: u64,
}

/// Terminal classification of one remote Bash settlement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BashOutcome {
    /// The remote authority reported this exit status.
    Completed { exit_code: i32 },
    /// The remote authority reported the timeout boundary.
    TimedOut,
    /// The remote authority reported an abort.
    Aborted,
    /// No terminal status ever arrived; the outcome is unknown.
    Unknown { reason: String },
}

impl BashResult {
    pub fn stdout(text: impl Into<String>) -> Self {
        BashResult {
            outcome: BashOutcome::Completed { exit_code: 0 },
            stdout: text.into(),
            stderr: String::new(),
            timeout_ms: BASH_TIMEOUT_MS,
        }
    }
}

/// Durable receipt for one appended event-byte batch. The append_id is
/// caller-supplied so retried effects reuse the identical identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendReceipt {
    /// Stable caller-supplied identifier committed by the store.
    pub append_id: Uuid,
}

/// Observed child inputs owned by this component. Used to prove credentials
/// did not enter argv, environment, or the bootstrap frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildInputs {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub bootstrap: String,
}

/// Completed activation as observed by the parent.
///
/// `child_attestation` carries what the child itself observed from the kernel
/// at startup (descriptor numbers and environment keys); the host re-verifies
/// it, so boundary proofs rest on actual child-visible state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationOutcome {
    pub final_text: String,
    pub bash_intents: Vec<BashIntent>,
    pub child_exit_code: i32,
    pub child_opened_connection: bool,
    pub child_inputs: ChildInputs,
    pub child_attestation: BoundaryAttestation,
}

/// Kernel-observed child boundary facts reported in `hello`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundaryAttestation {
    pub fds: Vec<i32>,
    pub env_keys: Vec<String>,
}

/// Failure at the activation parent boundary. Display never includes secrets.
#[derive(Debug)]
pub enum ActivationError {
    Child(&'static str),
    Protocol(&'static str),
    Io(std::io::Error),
}

impl std::fmt::Display for ActivationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActivationError::Child(message) | ActivationError::Protocol(message) => {
                write!(f, "{message}")
            }
            ActivationError::Io(error) => write!(f, "activation io: {error}"),
        }
    }
}

impl std::error::Error for ActivationError {}

impl From<std::io::Error> for ActivationError {
    fn from(error: std::io::Error) -> Self {
        ActivationError::Io(error)
    }
}

/// Parent-owned model completion seam. Later wired to the real model proxy.
pub trait ModelRelay: Send + Sync {
    fn complete(
        &self,
        request: ModelRequest,
    ) -> impl Future<Output = Result<ModelResponse, ActivationError>> + Send;
}

/// Parent-owned Workspace execution seam. Later wired to Fabric exec.
pub trait WorkspaceExec: Send + Sync {
    fn bash(
        &self,
        intent: BashIntent,
    ) -> impl Future<Output = Result<BashResult, ActivationError>> + Send;
}

/// Parent-owned session durability seam. Later wired to Blob + PostgreSQL.
///
/// The bridge appends the child's actual serialized event bytes and receives
/// a stable append receipt BEFORE any model call or Workspace effect runs;
/// checkpoint-before-effect therefore guards real bytes, not counters.
pub trait SessionPersistence: Send + Sync {
    /// Create-mode history bootstrap. Implementations may validate that the
    /// Session is empty before the child receives its prompt.
    fn bootstrap(
        &self,
        _session_id: Uuid,
        _mode: ActivationMode,
    ) -> impl Future<Output = Result<(), ActivationError>> + Send {
        async { Ok(()) }
    }

    /// Resume-mode history verification/loading hook. Canonical history stays
    /// in the implementation's durable store; the child receives no
    /// credential or alternate transcript.
    fn resume(
        &self,
        _session_id: Uuid,
    ) -> impl Future<Output = Result<(), ActivationError>> + Send {
        async { Ok(()) }
    }

    /// Canonical history bytes for a resumed Session, in durable order and
    /// already hash-verified by the store. The parent streams these to the
    /// child in bounded chunks; the child never receives credentials.
    fn history(
        &self,
        _session_id: Uuid,
    ) -> impl Future<Output = Result<Vec<Vec<u8>>, ActivationError>> + Send {
        async { Ok(Vec::new()) }
    }

    /// Appends actual event bytes under the caller-supplied stable
    /// `append_id`; the store must never mint its own identity for a batch,
    /// so a retried effect reuses the identical id.
    fn append_events(
        &self,
        session_id: Uuid,
        append_id: Uuid,
        event_bytes: &[u8],
    ) -> impl Future<Output = Result<AppendReceipt, ActivationError>> + Send;
}

/// Scripted model replies consumed in order.
pub struct ScriptedModel {
    replies: std::sync::Mutex<std::collections::VecDeque<ModelResponse>>,
    routed: std::sync::Mutex<Vec<ModelRequest>>,
}

impl ScriptedModel {
    pub fn new(replies: impl Into<Vec<ModelResponse>>) -> Self {
        ScriptedModel {
            replies: std::sync::Mutex::new(replies.into().into()),
            routed: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Snapshot of every request the bridge routed to this relay.
    pub fn routed_requests(&self) -> Vec<ModelRequest> {
        self.routed
            .lock()
            .expect("scripted model routed lock")
            .clone()
    }
}

impl ModelRelay for ScriptedModel {
    fn complete(
        &self,
        request: ModelRequest,
    ) -> impl Future<Output = Result<ModelResponse, ActivationError>> + Send {
        self.routed
            .lock()
            .expect("scripted model routed lock")
            .push(request);
        let reply = self
            .replies
            .lock()
            .expect("scripted model lock")
            .pop_front();
        async move {
            reply.ok_or(ActivationError::Protocol(
                "scripted model replies exhausted",
            ))
        }
    }
}

/// Minimal (workspace_id, call_id) no-replay gate mirroring the CloudWorkspace
/// uniqueness rule so implementations can share one seam contract.
#[derive(Debug, Default)]
pub struct ReplayGuard {
    seen: std::sync::Mutex<std::collections::HashSet<(Uuid, String)>>,
}

impl ReplayGuard {
    pub fn admit(&self, workspace_id: Uuid, call_id: &str) -> Result<(), ActivationError> {
        let mut seen = self.seen.lock().expect("replay guard lock");
        if !seen.insert((workspace_id, call_id.to_owned())) {
            return Err(ActivationError::Protocol(
                "duplicate bash call for workspace",
            ));
        }
        Ok(())
    }
}

/// Workspace exec that never starts a host-local process.
pub struct SyntheticWorkspace {
    pub stdout: String,
}

impl WorkspaceExec for SyntheticWorkspace {
    fn bash(
        &self,
        _intent: BashIntent,
    ) -> impl Future<Output = Result<BashResult, ActivationError>> + Send {
        let result = BashResult::stdout(self.stdout.clone());
        async move { Ok(result) }
    }
}

/// Workspace exec that reports an explicitly unknown settlement: the remote
/// authority vanished before any terminal status arrived.
pub struct UnknownWorkspace {
    pub reason: String,
}

impl WorkspaceExec for UnknownWorkspace {
    fn bash(
        &self,
        _intent: BashIntent,
    ) -> impl Future<Output = Result<BashResult, ActivationError>> + Send {
        let result = BashResult {
            outcome: BashOutcome::Unknown {
                reason: self.reason.clone(),
            },
            stdout: String::new(),
            stderr: String::new(),
            timeout_ms: BASH_TIMEOUT_MS,
        };
        async move { Ok(result) }
    }
}
