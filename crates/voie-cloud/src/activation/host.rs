use base64::Engine;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use uuid::Uuid;

use super::{
    ActivationContext, ActivationError, ActivationMode, ActivationOutcome, ActivationRequest,
    BashIntent, BashOutcome, BashResult, BoundaryAttestation, ChildInputs, ModelRelay,
    ModelRequest, ModelResponse, SessionPersistence, WireMessage, WorkspaceExec,
};

const PARENT_FD: i32 = 3;
const MAX_FRAME_BYTES: usize = 1_048_576;
const MAX_HISTORY_FRAME_BYTES: usize = 64 * 1024;
const MAX_HISTORY_TOTAL_BYTES: usize = 4 * 1024 * 1024;
/// Profile 1 may replace the Workspace guest and run several model turns
/// (create, bash writes, tests). Pack, materialize, and Database provision
/// resume on HTTP/status reads and must not occupy this budget.
const CHILD_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Deserialize)]
struct ChildFrame {
    id: String,
    op: String,
    attest: Option<ChildAttestation>,
    system: Option<String>,
    tools: Option<Vec<ToolName>>,
    messages: Option<Vec<WireMessage>>,
    /// Serialized session events accumulated since the previous flush; the
    /// parent persists these bytes before acting on the requested effect.
    events: Option<String>,
    call_id: Option<String>,
    command: Option<String>,
    description: Option<String>,
    text: Option<String>,
    name: Option<String>,
    arguments: Option<serde_json::Value>,
}

/// Kernel-observed child boundary facts reported inside `hello`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ChildAttestation {
    pub fds: Vec<i32>,
    pub env_keys: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ToolName {
    name: String,
}

fn activation_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../activation")
}

fn find_node() -> PathBuf {
    if let Ok(path) = std::env::var("VOIE_NODE") {
        return PathBuf::from(path);
    }
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in path.split(':') {
        let candidate = Path::new(dir).join("node");
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from("node")
}

/// Resolves the prebuilt immutable child entry. This library never installs
/// or builds anything at activation runtime: the dist artifact is produced
/// ahead of time by Nix (`nix build .#activation-dist`) or by the developer
/// recipe (`just activation-dist`), or named explicitly through
/// `VOIE_ACTIVATION_ENTRY` (a Nix store path in deployment).
fn provisioned_entry() -> Result<PathBuf, ActivationError> {
    if let Ok(entry) = std::env::var("VOIE_ACTIVATION_ENTRY") {
        let path = PathBuf::from(entry);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(ActivationError::Child("voie_activation_entry_missing"))
        };
    }
    let entry = activation_root().join("dist/index.js");
    if entry.is_file() {
        return Ok(entry);
    }
    Err(ActivationError::Child("activation child not provisioned"))
}

fn mode_name(mode: ActivationMode) -> &'static str {
    match mode {
        ActivationMode::Create => "create",
        ActivationMode::Resume => "resume",
    }
}

/// Verifies the disposable-child prerequisites without launching one: the
/// provisioned entry resolves and a node runtime binary is present. Readiness
/// fails closed when either artifact is missing.
pub fn artifacts_ready() -> Result<(), &'static str> {
    provisioned_entry()
        .map(|_| ())
        .map_err(|error| match error {
            ActivationError::Protocol(message) | ActivationError::Child(message) => message,
            _ => "activation child entry unavailable",
        })?;
    // An existing file without the execute bit would only fail later at
    // spawn time with PermissionDenied; readiness must fail here instead.
    match std::fs::metadata(find_node()) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Err("node runtime is not a file"),
        Err(_) => return Err("node runtime not found"),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(find_node())
            .map_err(|_| "node runtime not found")?
            .permissions()
            .mode();
        if mode & 0o111 == 0 {
            return Err("node runtime is not executable");
        }
    }
    Ok(())
}

/// Parent that later accepts session, model, and Workspace implementations.
pub struct ActivationHost<'a, M, W, P, T> {
    pub context: ActivationContext,
    pub model: &'a M,
    pub workspace: &'a W,
    pub sessions: &'a P,
    pub product: &'a T,
}

/// Drive one disposable child with the supplied parent seams.
pub async fn run<M, W, P, T>(
    host: ActivationHost<'_, M, W, P, T>,
    request: ActivationRequest,
) -> Result<ActivationOutcome, ActivationError>
where
    M: ModelRelay,
    W: WorkspaceExec,
    P: SessionPersistence,
    T: super::ProductExec,
{
    let entry = provisioned_entry()?;
    let node = find_node();
    let home = tempfile_home()?;
    let argv = vec![node.display().to_string(), entry.display().to_string()];
    let env = vec![
        ("HOME".to_string(), home.display().to_string()),
        ("TMPDIR".to_string(), home.display().to_string()),
        ("LANG".to_string(), "C".to_string()),
        ("PATH".to_string(), child_path()),
    ];

    let (parent_std, child_std) = UnixStream::pair()?;
    parent_std.set_nonblocking(true)?;
    let parent_raw = parent_std.as_raw_fd();
    let child_raw = child_std.as_raw_fd();
    let parent = tokio::net::UnixStream::from_std(parent_std)?;

    let mut command = Command::new(&node);
    command
        .arg(&entry)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    for (key, value) in &env {
        command.env(key, value);
    }
    unsafe {
        command.pre_exec(move || {
            close_inherited_except(child_raw);
            if libc::dup2(child_raw, PARENT_FD) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let flags = libc::fcntl(PARENT_FD, libc::F_GETFD);
            if flags < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::fcntl(PARENT_FD, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if child_raw != PARENT_FD {
                libc::close(child_raw);
            }
            if parent_raw != PARENT_FD {
                libc::close(parent_raw);
            }
            Ok(())
        });
    }

    let mut child = command.spawn()?;
    drop(child_std);

    let bootstrap = json!({
        "mode": mode_name(request.mode),
        "session_id": host.context.session_id.to_string(),
        "prompt": request.prompt,
    });
    let child_inputs = ChildInputs {
        argv,
        env,
        bootstrap: bootstrap.to_string(),
    };

    let drive = drive_child(parent, &host, request, bootstrap, child_inputs);
    let outcome = match tokio::time::timeout(CHILD_TIMEOUT, drive).await {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill().await;
            return Err(ActivationError::Child("activation child timed out"));
        }
    };

    match outcome {
        Ok(mut outcome) => {
            // `drive_child` already received `finish` and appended those
            // bytes. A later non-zero process exit is teardown, not an
            // unknown Workspace effect, so the Run stays terminal.
            let status =
                match tokio::time::timeout(std::time::Duration::from_secs(8), child.wait()).await {
                    Ok(Ok(status)) => status,
                    Ok(Err(_)) => {
                        let _ = child.kill().await;
                        outcome.child_exit_code = 1;
                        return Ok(outcome);
                    }
                    Err(_) => {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        outcome.child_exit_code = 1;
                        return Ok(outcome);
                    }
                };
            outcome.child_exit_code = status.code().unwrap_or(1);
            Ok(outcome)
        }
        Err(error) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(error)
        }
    }
}

fn tempfile_home() -> Result<PathBuf, ActivationError> {
    static NEXT_HOME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let nonce = NEXT_HOME.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("voie-act-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

/// Marks every descriptor above stdio, except `keep`, as close-on-exec so no
/// unrelated control or data descriptor can leak into the child image.
///
/// Fast path is Linux `close_range(.., CLOSE_RANGE_CLOEXEC)`; the portable
/// fallback is a bounded explicit `fcntl` sweep up to the soft fd limit.
fn close_inherited_except(keep: i32) {
    const STDIO_MAX: i32 = 2;
    // Fast path: one syscall marks everything above stdio close-on-exec.
    if unsafe {
        libc::close_range(
            STDIO_MAX as libc::c_uint + 1,
            libc::c_uint::MAX,
            libc::CLOSE_RANGE_CLOEXEC as libc::c_int,
        )
    } == 0
    {
        return;
    }
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let bound = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0 {
        limit.rlim_cur.min(1 << 20)
    } else {
        1 << 16
    };
    for fd in (STDIO_MAX + 1)..=bound as i32 {
        if fd == keep {
            continue;
        }
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
}

/// Derives the append identity from the durable Run and child request
/// identity. The exact bytes remain a separate content hash, so a retry with
/// changed bytes is detected as an append conflict rather than a new event.
fn append_id(run_id: Uuid, frame_id: &str) -> Uuid {
    let digest = Sha256::digest([run_id.as_bytes().as_slice(), frame_id.as_bytes()].concat());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn child_path() -> String {
    std::env::var("VOIE_ACTIVATION_PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string())
}

/// Splits hash-verified canonical history into bounded parent frames. A
/// single event larger than the frame bound or a total beyond the activation
/// budget is refused instead of truncated.
fn history_frames(events: Vec<Vec<u8>>) -> Result<Vec<Value>, ActivationError> {
    let mut frames = Vec::new();
    let mut items: Vec<Value> = Vec::new();
    let mut frame_bytes = 0usize;
    let mut total_bytes = 0usize;
    for event in events {
        if event.len() > MAX_HISTORY_FRAME_BYTES {
            return Err(ActivationError::Protocol(
                "session history event exceeded the frame bound",
            ));
        }
        total_bytes += event.len();
        if total_bytes > MAX_HISTORY_TOTAL_BYTES {
            return Err(ActivationError::Protocol(
                "session history exceeded the activation bound",
            ));
        }
        if !items.is_empty() && frame_bytes + event.len() > MAX_HISTORY_FRAME_BYTES {
            frames.push(json!({ "items": std::mem::take(&mut items) }));
            frame_bytes = 0;
        }
        frame_bytes += event.len();
        items.push(json!({
            "bytes": base64::engine::general_purpose::STANDARD.encode(&event),
        }));
    }
    if !items.is_empty() {
        frames.push(json!({ "items": items }));
    }
    Ok(frames)
}

/// Writes parent-initiated history chunks before any model or bash effect.
async fn stream_history<P: super::SessionPersistence>(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    sessions: &P,
    session_id: Uuid,
) -> Result<usize, ActivationError> {
    let events = sessions.history(session_id).await?;
    let mut frames = history_frames(events)?;
    let total = frames.len();
    // An empty history still sends one terminal frame so the child can await
    // a deterministic end-of-history marker before resuming.
    if frames.is_empty() {
        frames.push(json!({ "items": [] }));
    }
    let sent_total = total;
    for (index, mut frame) in frames.into_iter().enumerate() {
        let object = frame.as_object_mut().expect("history frame is an object");
        object.insert("id".to_owned(), json!(format!("history-{index}")));
        object.insert("op".to_owned(), json!("history"));
        object.insert("index".to_owned(), json!(index));
        object.insert("total".to_owned(), json!(sent_total));
        if index + 1 == sent_total.max(1) {
            object.insert("done".to_owned(), json!(true));
        }
        let encoded = format!("{frame}\n");
        if encoded.len() > MAX_FRAME_BYTES {
            return Err(ActivationError::Protocol("history frame exceeded bound"));
        }
        writer.write_all(encoded.as_bytes()).await?;
        writer.flush().await?;
    }
    Ok(total)
}

async fn drive_child<M, W, P, T>(
    stream: tokio::net::UnixStream,
    host: &ActivationHost<'_, M, W, P, T>,
    request: ActivationRequest,
    bootstrap: Value,
    child_inputs: ChildInputs,
) -> Result<ActivationOutcome, ActivationError>
where
    M: ModelRelay,
    W: WorkspaceExec,
    P: SessionPersistence,
    T: super::ProductExec,
{
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::with_capacity(MAX_FRAME_BYTES + 2, reader).lines();
    let mut opened = false;
    let mut attestation: Option<BoundaryAttestation> = None;
    let mut final_text = String::new();
    let mut bash_intents = Vec::new();
    let mut finished = false;

    while let Some(line) = lines.next_line().await? {
        if line.len() > MAX_FRAME_BYTES {
            return Err(ActivationError::Protocol("child frame exceeded bound"));
        }
        if line.is_empty() {
            continue;
        }
        let mut frame: ChildFrame = serde_json::from_str(&line)
            .map_err(|_| ActivationError::Protocol("child sent invalid json"))?;
        let reply = match frame.op.as_str() {
            "hello" => {
                opened = true;
                let observed = frame.attest.take().ok_or(ActivationError::Protocol(
                    "child did not attest its descriptor and environment boundary",
                ))?;
                verify_attestation(&observed)?;
                attestation = Some(BoundaryAttestation {
                    fds: observed.fds,
                    env_keys: observed.env_keys,
                });
                match request.mode {
                    super::ActivationMode::Create => {
                        host.sessions
                            .bootstrap(host.context.session_id, request.mode)
                            .await?
                    }
                    super::ActivationMode::Resume => {
                        host.sessions.resume(host.context.session_id).await?
                    }
                }
                json!({ "id": frame.id, "ok": true, "bootstrap": bootstrap })
            }
            "model" => {
                // Checkpoint-before-effect: the actual event bytes leave the
                // child and land durably before the model call runs.
                let event_bytes = frame.events.as_deref().unwrap_or_default().as_bytes();
                let append_id = append_id(host.context.run_id, &frame.id);
                host.sessions
                    .append_events(host.context.session_id, append_id, event_bytes)
                    .await?;
                let tools = frame
                    .tools
                    .unwrap_or_default()
                    .into_iter()
                    .map(|tool| tool.name)
                    .collect();
                let messages = frame.messages.unwrap_or_default();
                let response = host
                    .model
                    .complete(ModelRequest {
                        system: frame.system,
                        tools,
                        messages,
                    })
                    .await?;
                if let ModelResponse::ToolCall { name, .. } = &response {
                    if name == "bash" && !host.context.bash_enabled {
                        return Err(ActivationError::Protocol(
                            "model returned an unauthorized tool",
                        ));
                    }
                }
                let model = model_json(&response)?;
                json!({ "id": frame.id, "ok": true, "model": model })
            }
            "bash" => {
                if !host.context.bash_enabled {
                    return Err(ActivationError::Protocol(
                        "bash is not enabled for this agent",
                    ));
                }
                let event_bytes = frame.events.as_deref().unwrap_or_default().as_bytes();
                let append_id = append_id(host.context.run_id, &frame.id);
                host.sessions
                    .append_events(host.context.session_id, append_id, event_bytes)
                    .await?;
                let call_id = frame.call_id.clone().filter(|id| !id.is_empty()).ok_or(
                    ActivationError::Protocol("bash intent lacks its model call id"),
                )?;
                let intent = BashIntent {
                    call_id,
                    command: frame.command.unwrap_or_default(),
                    description: frame.description.unwrap_or_default(),
                };
                bash_intents.push(intent.clone());
                let result = host.workspace.bash(intent.clone()).await?;
                json!({
                    "id": frame.id,
                    "ok": true,
                    "call_id": intent.call_id,
                    "bash": bash_json(&result),
                })
            }
            "product" => {
                let event_bytes = frame.events.as_deref().unwrap_or_default().as_bytes();
                let append_id = append_id(host.context.run_id, &frame.id);
                host.sessions
                    .append_events(host.context.session_id, append_id, event_bytes)
                    .await?;
                let call_id = frame.call_id.clone().filter(|id| !id.is_empty()).ok_or(
                    ActivationError::Protocol("product intent lacks its model call id"),
                )?;
                let name = frame.name.clone().filter(|name| !name.is_empty()).ok_or(
                    ActivationError::Protocol("product intent lacks a tool name"),
                )?;
                let arguments_json = frame
                    .arguments
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "{}".to_owned());
                let result = host
                    .product
                    .execute(super::ProductIntent {
                        call_id: call_id.clone(),
                        name,
                        arguments_json,
                    })
                    .await?;
                json!({
                    "id": frame.id,
                    "ok": true,
                    "call_id": call_id,
                    "product": {
                        "text": result.text,
                        "is_error": result.is_error,
                    },
                })
            }
            "finish" => {
                let event_bytes = frame.events.as_deref().unwrap_or_default().as_bytes();
                let append_id = append_id(host.context.run_id, &frame.id);
                host.sessions
                    .append_events(host.context.session_id, append_id, event_bytes)
                    .await?;
                final_text = frame.text.unwrap_or_default();
                finished = true;
                json!({ "id": frame.id, "ok": true })
            }
            _ => json!({ "id": frame.id, "ok": false, "error": "unknown op" }),
        };
        let encoded = format!("{reply}\n");
        if encoded.len() > MAX_FRAME_BYTES {
            return Err(ActivationError::Protocol("parent frame exceeded bound"));
        }
        writer.write_all(encoded.as_bytes()).await?;
        writer.flush().await?;
        if frame.op == "hello" {
            // Seed canonical history immediately after the hello reply so the
            // child can resume its agent loop over durable bytes before any
            // model or Workspace effect. Bounded chunks; never one giant frame.
            stream_history(&mut writer, host.sessions, host.context.session_id).await?;
        }
        if finished {
            break;
        }
    }

    if !opened {
        return Err(ActivationError::Child(
            "child did not open the inherited connection",
        ));
    }
    if !finished {
        return Err(ActivationError::Child(
            "child exited before a final response",
        ));
    }
    let _ = request;
    Ok(ActivationOutcome {
        final_text,
        bash_intents,
        child_exit_code: 0,
        child_opened_connection: opened,
        child_inputs,
        child_attestation: attestation.expect("attested hello verified above"),
    })
}

/// The parent independently re-checks what the child reports from the kernel:
/// the inherited endpoint set must be EXACTLY the bridge socket and the
/// environment EXACTLY the agreed minimum. Missing pieces fail exactly like
/// unexpected ones; a boundary is only proven by full equality.
pub fn verify_attestation(attestation: &ChildAttestation) -> Result<(), ActivationError> {
    const REQUIRED_FDS: [i32; 1] = [PARENT_FD];
    const REQUIRED_ENV_KEYS: [&str; 4] = ["HOME", "LANG", "PATH", "TMPDIR"];
    let mut fds = attestation.fds.clone();
    fds.sort_unstable();
    if fds != REQUIRED_FDS {
        return Err(ActivationError::Child(
            "child descriptors are not exactly the bridge socket on fd 3",
        ));
    }
    let mut env_keys = attestation.env_keys.clone();
    env_keys.sort();
    if env_keys != REQUIRED_ENV_KEYS {
        return Err(ActivationError::Child(
            "child environment is not exactly HOME, LANG, PATH, TMPDIR",
        ));
    }
    Ok(())
}

fn model_json(response: &ModelResponse) -> Result<Value, ActivationError> {
    match response {
        ModelResponse::Text(text) => Ok(json!({ "kind": "text", "text": text })),
        ModelResponse::ToolCall {
            call_id,
            name,
            arguments_json,
        } => Ok(json!({
            "kind": "tool_call",
            "call_id": call_id,
            "name": name,
            "arguments": arguments_from_json(arguments_json)?,
        })),
    }
}

/// Parses the relay's authored argument object once so the wire carries a
/// real JSON value; the child re-stringifies it for DSH verbatim.
fn arguments_from_json(arguments_json: &str) -> Result<Value, ActivationError> {
    serde_json::from_str(arguments_json)
        .map_err(|_| ActivationError::Protocol("model relay arguments are not a JSON object"))
}

fn bash_json(result: &BashResult) -> Value {
    let outcome = match &result.outcome {
        BashOutcome::Completed { exit_code } => json!({ "completed": { "exit_code": exit_code } }),
        BashOutcome::TimedOut => json!({ "timed_out": true }),
        BashOutcome::Aborted => json!({ "aborted": true }),
        BashOutcome::Unknown { reason } => json!({ "unknown": { "reason": reason } }),
    };
    json!({
        "outcome": outcome,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "timeout_ms": result.timeout_ms,
    })
}
