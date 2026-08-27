//! One-shot credentialless command runner for VOIE Firecracker guests.
//!
//! Runs a single program with its arguments passed through verbatim — no
//! implicit shell — in the foreground with stdin closed and its own process
//! group. Both output pipes are drained concurrently and in full so the child
//! never blocks on a full pipe; only a bounded prefix of each stream is
//! retained, with truncation recorded on the side. One absolute deadline
//! covers the whole run; on expiry the entire child process group is killed
//! and reaped. The result surfaces only through ordinary streams and the
//! process exit status: no framing protocol, socket, credential, PTY,
//! background mode, or shell interpretation exists here.

use std::fs;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread;
use std::time::{Duration, Instant};

/// Default whole-run deadline, matching the BETTERDAM exec bounds.
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// Maximum whole-run deadline in milliseconds.
pub const MAX_TIMEOUT_MS: u64 = 120_000;
/// Default retained prefix per stream, matching the BETTERDAM exec bounds.
pub const DEFAULT_OUTPUT_MAX_BYTES: usize = 64 * 1024;
/// Maximum retained prefix per stream in bytes.
pub const MAX_OUTPUT_MAX_BYTES: usize = 1024 * 1024;

/// Malformed invocation or out-of-range option value.
pub const EXIT_MISUSE: u8 = 2;
/// The absolute deadline expired; the child process group was killed.
pub const EXIT_TIMED_OUT: u8 = 124;
/// The runner could not start or wait for the child at all.
pub const EXIT_RUN_FAILED: u8 = 125;

/// How long captured-output readers may still run after the group kill before
/// the runner gives up on any further bytes. SIGKILL closes the pipes promptly;
/// the grace only guards against a process that escaped the group.
const POST_KILL_GRACE: Duration = Duration::from_secs(1);
/// Foreground polling interval while waiting for the child under the deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Everything needed to run one command; values must already be validated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Invocation {
    /// Working directory; the CLI policy restricts this to `/workspace`.
    pub workdir: String,
    pub timeout_ms: u64,
    pub stdout_max_bytes: usize,
    pub stderr_max_bytes: usize,
    /// Executable to run; looked up on `PATH` when it contains no slash.
    pub program: String,
    /// Argument vector handed to the program unchanged.
    pub args: Vec<String>,
}

/// What the caller asked the CLI to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Help,
    Version,
    Run(Invocation),
}

/// Bounded capture of one output stream.
#[derive(Debug, Eq, PartialEq)]
pub struct Captured {
    pub bytes: Vec<u8>,
    /// True when at least one byte was read and dropped beyond the cap.
    pub truncated: bool,
}

/// Result of one completed run.
#[derive(Debug)]
pub struct Outcome {
    pub status: ExitStatus,
    pub stdout: Captured,
    pub stderr: Captured,
    /// True only when the deadline expired and the group was killed.
    pub timed_out: bool,
}

impl Captured {
    fn empty() -> Self {
        Self {
            bytes: Vec::new(),
            truncated: false,
        }
    }
}

impl Default for Invocation {
    fn default() -> Self {
        Self {
            workdir: "/workspace".to_owned(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            stdout_max_bytes: DEFAULT_OUTPUT_MAX_BYTES,
            stderr_max_bytes: DEFAULT_OUTPUT_MAX_BYTES,
            program: String::new(),
            args: Vec::new(),
        }
    }
}

/// Parse CLI arguments into an [`Action`] or a human-readable misuse message.
///
/// Options may appear in any order before `--`, at most once each, either as
/// `--opt value` or `--opt=value`. The first argument after `--` names the
/// program to run; the rest are its arguments, never shell-interpreted.
/// Empty input means help, preserving the historical behavior of the skeleton
/// binary.
pub fn parse_args(args: &[String]) -> Result<Action, String> {
    if args.is_empty() {
        return Ok(Action::Help);
    }

    let mut workdir: Option<String> = None;
    let mut timeout_ms: Option<u64> = None;
    let mut stdout_max: Option<usize> = None;
    let mut stderr_max: Option<usize> = None;

    let mut index = 0;
    let mut saw_separator = false;
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "--help" => return Ok(Action::Help),
            "--version" => return Ok(Action::Version),
            "--" => {
                index += 1;
                saw_separator = true;
                break;
            }
            _ => {}
        }
        let (name, attached) = split_option(arg)?;
        index += 1;
        let value = match attached {
            Some(value) => value.to_owned(),
            None => {
                let Some(value) = args.get(index) else {
                    return Err(format!("option `{name}` requires a value"));
                };
                index += 1;
                value.clone()
            }
        };
        match name {
            "--workdir" => set_once(&mut workdir, name, value)?,
            "--timeout-ms" => {
                let parsed = parse_number(name, &value)?;
                set_once(&mut timeout_ms, name, parsed)?
            }
            "--stdout-max-bytes" => {
                let parsed = parse_number(name, &value)?;
                set_once(&mut stdout_max, name, parsed)?
            }
            "--stderr-max-bytes" => {
                let parsed = parse_number(name, &value)?;
                set_once(&mut stderr_max, name, parsed)?
            }
            _ => {
                return Err(format!(
                    "unknown option `{name}`; expected --workdir, --timeout-ms, \
                     --stdout-max-bytes, --stderr-max-bytes, or `--`"
                ));
            }
        }
    }

    if !saw_separator {
        return Err("missing `--` separator before the program".to_owned());
    }
    // The first token after `--` names the executable; everything else is its
    // argument vector, preserved byte-for-byte.
    let (program, rest) = match args[index..].split_first() {
        Some((program, rest)) => (program.clone(), rest.to_vec()),
        None => return Err("`--` must be followed by a program to run".to_owned()),
    };

    let timeout_ms = match timeout_ms {
        Some(value) if value == 0 || value > MAX_TIMEOUT_MS => {
            return Err(format!(
                "--timeout-ms must be between 1 and {MAX_TIMEOUT_MS}"
            ));
        }
        Some(value) => value,
        None => DEFAULT_TIMEOUT_MS,
    };
    let stdout_max_bytes = bounded_output(stdout_max, "--stdout-max-bytes")?;
    let stderr_max_bytes = bounded_output(stderr_max, "--stderr-max-bytes")?;
    let workdir = match workdir {
        Some(path) => {
            validate_workdir(&path)?;
            path
        }
        None => "/workspace".to_owned(),
    };

    Ok(Action::Run(Invocation {
        workdir,
        timeout_ms,
        stdout_max_bytes,
        stderr_max_bytes,
        program,
        args: rest,
    }))
}

fn split_option(arg: &str) -> Result<(&str, Option<&str>), String> {
    if !arg.starts_with("--") || arg.len() <= 2 {
        return Err(format!(
            "unexpected argument `{arg}`; expected an option or `--`"
        ));
    }
    match arg.split_once('=') {
        Some((_, value)) => {
            let name = &arg[..arg.len() - value.len() - 1];
            Ok((name, Some(value)))
        }
        None => Ok((arg, None)),
    }
}

fn set_once<T>(slot: &mut Option<T>, name: &str, value: T) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("option `{name}` given more than once"));
    }
    *slot = Some(value);
    Ok(())
}

fn parse_number<T>(name: &str, raw: &str) -> Result<T, String>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    raw.parse::<T>()
        .map_err(|error| format!("{name} value `{raw}` is not a valid number: {error}"))
}

fn bounded_output(slot: Option<usize>, name: &str) -> Result<usize, String> {
    match slot {
        Some(value) if value == 0 || value > MAX_OUTPUT_MAX_BYTES => Err(format!(
            "{name} must be between 1 and {MAX_OUTPUT_MAX_BYTES}"
        )),
        Some(value) => Ok(value),
        None => Ok(DEFAULT_OUTPUT_MAX_BYTES),
    }
}

/// Enforce the lexical workdir policy: exactly `/workspace` or beneath it,
/// with no `..` component. Purely textual; existence is the kernel's business.
pub fn validate_workdir(path: &str) -> Result<(), String> {
    let Some(rest) = path.strip_prefix("/workspace") else {
        return Err(format!(
            "--workdir `{path}` must be `/workspace` or a path beneath it"
        ));
    };
    if rest.is_empty() {
        return Ok(());
    }
    if !rest.starts_with('/') {
        return Err(format!(
            "--workdir `{path}` must be `/workspace` or a path beneath it"
        ));
    }
    for component in rest.split('/').filter(|part| !part.is_empty()) {
        if component == ".." {
            return Err(format!(
                "--workdir `{path}` must not contain a `..` component"
            ));
        }
    }
    Ok(())
}

/// Run one validated invocation to completion.
///
/// Only spawn/wait failures of the runner itself surface as [`io::Error`]
/// (mapped to exit 125 by the CLI); every child outcome is reported through
/// [`Outcome`].
pub fn run(invocation: &Invocation) -> io::Result<Outcome> {
    run_with_workspace(Path::new("/workspace"), invocation)
}

/// Test-support entry point that keeps production execution fixed to
/// `/workspace` while allowing host tests to use an isolated temporary root.
#[doc(hidden)]
pub fn run_with_workspace(workspace: &Path, invocation: &Invocation) -> io::Result<Outcome> {
    let workdir = canonical_workdir(workspace, Path::new(&invocation.workdir))?;
    let deadline = Instant::now() + Duration::from_millis(invocation.timeout_ms);

    let mut child = Command::new(&invocation.program)
        .args(&invocation.args)
        .current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // New process group keyed on the child pid, so a group-wide kill
        // reaches everything the command spawned.
        .process_group(0)
        .spawn()?;
    let pgid = child.id() as i32;

    let stdout_pipe = child.stdout.take().expect("stdout was piped");
    let stderr_pipe = child.stderr.take().expect("stderr was piped");
    let stdout_cap = invocation.stdout_max_bytes;
    let stderr_cap = invocation.stderr_max_bytes;
    let (stdout_tx, stdout_rx) = channel();
    let (stderr_tx, stderr_rx) = channel();
    thread::spawn(move || {
        let _ = stdout_tx.send(drain(stdout_pipe, stdout_cap));
    });
    thread::spawn(move || {
        let _ = stderr_tx.send(drain(stderr_pipe, stderr_cap));
    });

    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            kill_process_group(pgid);
            timed_out = true;
            // Reap the leader; SIGKILL makes this prompt.
            break child.wait()?;
        }
        thread::sleep(POLL_INTERVAL);
    };

    let stdout = wait_capture(stdout_rx, deadline);
    let stderr = wait_capture(stderr_rx, deadline);

    Ok(Outcome {
        status,
        stdout,
        stderr,
        timed_out,
    })
}

fn canonical_workdir(workspace: &Path, requested: &Path) -> io::Result<PathBuf> {
    let workspace = fs::canonicalize(workspace)?;
    let requested = fs::canonicalize(requested)?;
    if !requested.starts_with(&workspace) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "working directory resolves outside /workspace",
        ));
    }
    Ok(requested)
}

/// Read a stream to EOF, keeping at most `cap` leading bytes.
///
/// Reading never stops early: dropping retained bytes instead of closing the
/// pipe keeps the writer free of SIGPIPE surprises and lets the real exit
/// status survive truncation.
fn drain(mut source: impl Read, cap: usize) -> Captured {
    let mut chunk = [0u8; 8192];
    let mut captured = Vec::new();
    let mut truncated = false;
    loop {
        match source.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                let room = cap.saturating_sub(captured.len());
                let kept = read.min(room);
                captured.extend_from_slice(&chunk[..kept]);
                truncated |= kept < read;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            // A failing pipe has no more truth to give; keep what arrived.
            Err(_) => break,
        }
    }
    Captured {
        bytes: captured,
        truncated,
    }
}

fn wait_capture(receiver: Receiver<Captured>, deadline: Instant) -> Captured {
    let now = Instant::now();
    let budget = if deadline > now {
        deadline - now
    } else {
        POST_KILL_GRACE
    };
    match receiver.recv_timeout(budget) {
        Ok(captured) => captured,
        Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => Captured::empty(),
    }
}

fn kill_process_group(pgid: i32) {
    // A negative pid targets the whole group. ESRCH simply means every member
    // was already gone, which is the desired state.
    let _ = unsafe { libc::kill(-pgid, libc::SIGKILL) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn empty_arguments_mean_help() {
        assert_eq!(parse_args(&[]), Ok(Action::Help));
    }

    #[test]
    fn help_and_version_win_anywhere_before_separator() {
        assert_eq!(parse_args(&args(&["--version"])), Ok(Action::Version));
        assert_eq!(
            parse_args(&args(&["--timeout-ms", "5", "--help"])),
            Ok(Action::Help)
        );
    }

    #[test]
    fn defaults_follow_betterdam_bounds() {
        let parsed = parse_args(&args(&["--", "true"])).unwrap();
        let Action::Run(invocation) = parsed else {
            panic!("expected run")
        };
        assert_eq!(invocation.workdir, "/workspace");
        assert_eq!(invocation.timeout_ms, DEFAULT_TIMEOUT_MS);
        assert_eq!(invocation.stdout_max_bytes, DEFAULT_OUTPUT_MAX_BYTES);
        assert_eq!(invocation.stderr_max_bytes, DEFAULT_OUTPUT_MAX_BYTES);
        assert_eq!(invocation.program, "true");
        assert_eq!(invocation.args, Vec::<String>::new());
    }

    #[test]
    fn accepts_both_value_forms_and_preserves_argv() {
        let parsed = parse_args(&args(&[
            "--timeout-ms=7",
            "--workdir",
            "/workspace/x",
            "--",
            "echo",
            "a b",
            "b",
        ]))
        .unwrap();
        let Action::Run(invocation) = parsed else {
            panic!("expected run")
        };
        assert_eq!(invocation.timeout_ms, 7);
        assert_eq!(invocation.workdir, "/workspace/x");
        // Arguments are never joined or reinterpreted: the embedded space
        // survives as part of one argv entry.
        assert_eq!(invocation.program, "echo");
        assert_eq!(invocation.args, vec!["a b", "b"]);
    }

    #[test]
    fn accepts_inclusive_bounds() {
        for timeout in ["1", "120000"] {
            let parsed = parse_args(&args(&["--timeout-ms", timeout, "--", ":"])).unwrap();
            let Action::Run(_) = parsed else {
                panic!("expected run")
            };
        }
        for cap in ["1", "1048576"] {
            let parsed = parse_args(&args(&[
                "--stdout-max-bytes",
                cap,
                "--stderr-max-bytes",
                cap,
                "--",
                ":",
            ]))
            .unwrap();
            let Action::Run(invocation) = parsed else {
                panic!("expected run")
            };
            assert_eq!(invocation.stdout_max_bytes, cap.parse::<usize>().unwrap());
        }
    }

    #[test]
    fn rejects_out_of_range_and_garbage_values() {
        for case in [
            vec!["--timeout-ms", "0", "--", ":"],
            vec!["--timeout-ms", "120001", "--", ":"],
            vec!["--timeout-ms", "soon", "--", ":"],
            vec!["--timeout-ms", "-1", "--", ":"],
            vec!["--stdout-max-bytes", "0", "--", ":"],
            vec!["--stderr-max-bytes", "1048577", "--", ":"],
            vec!["--stdout-max-bytes", "lots", "--", ":"],
        ] {
            let error = parse_args(&args(&case)).unwrap_err();
            assert!(!error.is_empty(), "case {case:?} should explain itself");
        }
    }

    #[test]
    fn rejects_duplicate_options() {
        let error =
            parse_args(&args(&["--timeout-ms", "5", "--timeout-ms=6", "--", ":"])).unwrap_err();
        assert!(error.contains("more than once"), "{error}");
    }

    #[test]
    fn rejects_missing_separator_and_missing_program() {
        assert!(parse_args(&args(&["echo", "hi"])).is_err());
        assert!(parse_args(&args(&["--"])).is_err());
    }

    #[test]
    fn rejects_unknown_and_stray_arguments() {
        assert!(parse_args(&args(&["--wat", "--", ":"])).is_err());
        assert!(parse_args(&args(&["stray", "--", ":"])).is_err());
        let missing = parse_args(&args(&["--workdir"])).unwrap_err();
        assert!(missing.contains("requires a value"), "{missing}");
    }

    #[test]
    fn workdir_policy_is_lexical_under_workspace() {
        for ok in [
            "/workspace",
            "/workspace/",
            "/workspace/sub",
            "/workspace//deep/x",
        ] {
            assert_eq!(validate_workdir(ok), Ok(()), "{ok}");
        }
        for bad in [
            "",
            "/",
            "workspace",
            "/workspacefoo",
            "/workspace/../etc",
            "../escape",
            "sub/dir",
            "/tmp/place",
            "/workspace/sub/../../etc",
        ] {
            let error = validate_workdir(bad).unwrap_err();
            assert!(!error.is_empty(), "{bad} should explain itself");
        }
    }

    #[test]
    fn drain_keeps_exact_prefix_and_marks_truncation() {
        // Exactly ten bytes built arithmetically; no embedded literals.
        let data: Vec<u8> = (b'a'..=b'j').collect();

        let full = drain(Cursor::new(&data[..]), 10);
        assert_eq!(full.bytes, data);
        assert!(!full.truncated);

        let short = drain(Cursor::new(&data[..]), 3);
        assert_eq!(short.bytes, data[..3].to_vec());
        assert!(short.truncated);

        let none = drain(Cursor::new(&data[..]), 0);
        assert!(none.bytes.is_empty());
        assert!(none.truncated);
    }

    #[cfg(unix)]
    #[test]
    fn canonical_workdir_rejects_workspace_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "voie-runner-canonical-workdir-{}",
            std::process::id()
        ));
        let workspace = root.join("workspace");
        let outside = root.join("outside");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, workspace.join("link")).unwrap();

        assert!(canonical_workdir(&workspace, &workspace.join("link")).is_err());

        std::fs::remove_dir_all(root).unwrap();
    }
}
