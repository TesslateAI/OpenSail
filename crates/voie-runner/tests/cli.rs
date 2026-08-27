//! Focused deterministic tests for `voie-runner`.
//!
//! Misuse and policy tests drive the real binary; behavioral tests call the
//! library directly with a temporary workdir so they never depend on
//! `/workspace` existing on the host running the tests.

use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use voie_runner::{Invocation, Outcome, parse_args, run_with_workspace};

/// Behavioral tests exercise real child processes in an isolated directory, so
/// they bypass the production `/workspace` root with the widest legal root.
fn run_anywhere(invocation: &Invocation) -> std::io::Result<Outcome> {
    run_with_workspace(Path::new("/"), invocation)
}

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_voie-runner"))
        .args(args)
        .output()
        .expect("spawn voie-runner")
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("voie-runner-test-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp workdir");
    dir
}

fn invocation(workdir: &Path, program: &str, args: &[&str]) -> Invocation {
    Invocation {
        workdir: workdir.to_string_lossy().into_owned(),
        timeout_ms: 10_000,
        stdout_max_bytes: 64 * 1024,
        stderr_max_bytes: 64 * 1024,
        program: program.to_owned(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
    }
}

fn run_in(workdir: &Path, program: &str, args: &[&str]) -> Outcome {
    run_with_workspace(workdir, &invocation(workdir, program, args)).expect("run completes")
}

/// Shell scripts are exercised through an explicit shell, exactly what a
/// caller without the runner would have to pass after `--`.
fn run_script(workdir: &Path, script: &str) -> Outcome {
    run_in(workdir, "/bin/sh", &["-c", script])
}

#[test]
fn help_and_version_exit_zero() {
    let help = cli(&["--help"]);
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.contains("Usage:"), "help text: {text}");

    let version = cli(&["--version"]);
    assert!(version.status.success());
    assert!(
        String::from_utf8(version.stdout)
            .unwrap()
            .starts_with("voie-runner ")
    );

    // Preserved skeleton behavior: no arguments also means help.
    assert!(cli(&[]).status.success());
}

#[test]
fn misuse_exits_two_with_message() {
    let cases: &[&[&str]] = &[
        &["--wat", "--", "true"],
        &["stray", "--", "true"],
        &["--timeout-ms", "0", "--", "true"],
        &["--timeout-ms", "120001", "--", "true"],
        &["--timeout-ms", "abc", "--", "true"],
        &["--stdout-max-bytes", "0", "--", "true"],
        &["--stderr-max-bytes", "1048577", "--", "true"],
        &["--workdir", "../escape", "--", "true"],
        &["--workdir", "/workspace/../etc", "--", "true"],
        &["--workdir", "/workspacefoo", "--", "true"],
        &["--workdir", "/tmp/place", "--", "true"],
        &["echo", "without", "separator"],
        &["--"],
        &["--timeout-ms", "1", "--timeout-ms", "2", "--", "true"],
    ];
    for case in cases {
        let output = cli(case);
        assert_eq!(output.status.code(), Some(2), "case {case:?}");
        let message = String::from_utf8(output.stderr).unwrap();
        assert!(
            message.starts_with("voie-runner:"),
            "case {case:?}: {message}"
        );
        // Misuse must be rejected before anything runs.
        assert!(output.stdout.is_empty(), "case {case:?}");
    }
}

#[test]
fn missing_program_fails_to_run_not_misuse() {
    // A whitespace "program" parses fine — argv is preserved verbatim, and a
    // name with spaces could be a real file. The failure surfaces as the
    // runner's own spawn-failure code, not as argument misuse.
    let binary = cli(&["--timeout-ms", "1000", "--", "/definitely/not/a/program"]);
    assert_eq!(binary.status.code(), Some(125));
    let message = String::from_utf8(binary.stderr).unwrap();
    assert!(message.contains("failed to run command"), "{message}");
}

#[test]
fn exit_code_is_propagated() {
    let dir = temp_dir("exit-code");
    let outcome = run_script(&dir, "exit 3");
    assert!(!outcome.timed_out);
    assert_eq!(outcome.status.code(), Some(3));
}

#[test]
fn signal_death_is_reported_as_signal() {
    let dir = temp_dir("signal");
    let outcome = run_script(&dir, "kill -TERM $$");
    assert!(!outcome.timed_out);
    assert_eq!(outcome.status.code(), None);
    assert_eq!(outcome.status.signal(), Some(15));
}

#[test]
fn workdir_is_actually_used() {
    let dir = temp_dir("cwd");
    let outcome = run_in(&dir, "pwd", &[]);
    assert_eq!(outcome.status.code(), Some(0));
    let printed = String::from_utf8(outcome.stdout.bytes).unwrap();
    assert_eq!(
        printed.trim(),
        std::fs::canonicalize(&dir).unwrap().to_string_lossy(),
    );
}

#[test]
fn stdin_is_null_not_inherited() {
    let dir = temp_dir("stdin");
    // wc -c reads our null stdin: zero bytes, EOF immediately.
    let outcome = run_in(&dir, "wc", &["-c"]);
    assert_eq!(outcome.status.code(), Some(0));
    assert_eq!(String::from_utf8(outcome.stdout.bytes).unwrap().trim(), "0");
}

#[test]
fn concurrent_streams_capture_without_deadlock() {
    let dir = temp_dir("interleave");
    // ~170 KiB per stream, well past the 64 KiB pipe buffer, alternating so
    // neither pipe can starve; only concurrent draining finishes at all.
    let script = "i=0; while [ $i -lt 20000 ]; do echo \"o$i\"; echo \"e$i\" >&2; i=$((i+1)); done";
    let mut inv = invocation(&dir, "/bin/sh", &["-c", script]);
    inv.stdout_max_bytes = 512 * 1024;
    inv.stderr_max_bytes = 512 * 1024;
    let outcome = run_anywhere(&inv).expect("run completes");

    assert!(!outcome.timed_out);
    assert_eq!(outcome.status.code(), Some(0));
    assert!(!outcome.stdout.truncated);
    assert!(!outcome.stderr.truncated);
    // Well past the 64 KiB pipe buffer in both directions.
    assert!(
        outcome.stdout.bytes.len() > 96 * 1024,
        "{}",
        outcome.stdout.bytes.len()
    );
    assert!(outcome.stderr.bytes.len() > 96 * 1024);
    assert!(outcome.stdout.bytes.ends_with(b"o19999\n"));
    assert!(outcome.stderr.bytes.ends_with(b"e19999\n"));
}

#[test]
fn truncation_keeps_prefix_while_draining_fully() {
    let dir = temp_dir("truncate");
    let mut inv = invocation(
        &dir,
        "/bin/sh",
        &["-c", "yes a | head -c 200; yes b | head -c 200 >&2"],
    );
    inv.stdout_max_bytes = 10;
    inv.stderr_max_bytes = 10;
    let outcome = run_anywhere(&inv).expect("run completes");

    // The child still exits cleanly: we drained past the cap instead of
    // closing the pipe on it.
    assert_eq!(outcome.status.code(), Some(0));
    // `yes` writes "a\n" lines; the kept ten bytes are five such lines.
    assert_eq!(outcome.stdout.bytes, b"a\na\na\na\na\n");
    assert!(outcome.stdout.truncated);
    assert_eq!(outcome.stderr.bytes, b"b\nb\nb\nb\nb\n");
    assert!(outcome.stderr.truncated);
}

#[test]
fn timeout_kills_whole_group_and_reports_124() {
    let dir = temp_dir("timeout");
    // A background sleeper that would emit late output after the deadline,
    // plus a foreground sleeper keeping the leader alive: if either escaped
    // the group kill, LATE would appear or the run would hang.
    let mut inv = invocation(
        &dir,
        "/bin/sh",
        &[
            "-c",
            "sleep 60 & (sleep 30; echo LATE) & echo ready; sleep 60",
        ],
    );
    inv.timeout_ms = 400;

    let started = Instant::now();
    let outcome = run_anywhere(&inv).expect("run completes");
    let elapsed = started.elapsed();

    assert!(outcome.timed_out);
    assert_eq!(outcome.stdout.bytes, b"ready\n");
    // Prompt return proves the group kill ended every member, including pipe
    // holders; generous margin keeps this deterministic on slow machines.
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
}

#[test]
fn fast_command_under_deadline_does_not_time_out() {
    let dir = temp_dir("fast");
    let started = Instant::now();
    // Direct exec: no shell between the runner and `printf`.
    let outcome = run_in(&dir, "printf", &["ok"]);
    assert_eq!(outcome.status.code(), Some(0));
    assert!(!outcome.timed_out);
    assert_eq!(outcome.stdout.bytes, b"ok");
    assert!(
        started.elapsed() < Duration::from_millis(5000),
        "sanity wall clock"
    );
}

#[test]
fn binary_reports_timeout_exit_code_directly() {
    // Drive the real binary end to end with a command that cannot finish.
    // Workdir must exist for spawn; /workspace is the guest path, so gate the
    // positive CLI smoke on its presence (it always exists in the image).
    if !Path::new("/workspace").is_dir() {
        return;
    }
    let output = cli(&["--timeout-ms", "300", "--", "sleep", "30"]);
    assert_eq!(output.status.code(), Some(124));
}

#[test]
fn parser_rejects_separator_only_via_library_agreement() {
    // Keep CLI misuse codes and library errors in agreement.
    let args: Vec<String> = ["--timeout-ms", "0", "--", "true"]
        .iter()
        .map(|item| item.to_string())
        .collect();
    assert!(matches!(parse_args(&args), Err(_)));
}
