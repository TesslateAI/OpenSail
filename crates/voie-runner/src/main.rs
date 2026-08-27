//! Thin CLI wrapper around the `voie_runner` library.
//!
//! Usage errors print a single explanatory line on stderr and exit 2. Child
//! output passes through byte-for-byte; the runner itself adds nothing to
//! stdout or stderr once the command has run.

use std::io::{self, Write};
use std::os::unix::process::ExitStatusExt;
use std::process::ExitCode;

use voie_runner::{Action, EXIT_MISUSE, EXIT_RUN_FAILED, EXIT_TIMED_OUT, parse_args, run};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn print_help() {
    println!("voie-runner {VERSION}");
    println!("Credentialless VOIE Firecracker guest runner.");
    println!();
    println!("Usage: voie-runner [options] -- PROGRAM [ARGS]...");
    println!();
    println!("Options:");
    println!("      --help                 Show this help and exit.");
    println!("      --version              Show the version and exit.");
    println!(
        "      --workdir <path>       Work under /workspace or beneath it (default /workspace)."
    );
    println!("      --timeout-ms <n>       Whole-run deadline in ms, 1..=120000 (default 30000).");
    println!(
        "      --stdout-max-bytes <n> Kept stdout prefix in bytes, 1..=1048576 (default 65536)."
    );
    println!(
        "      --stderr-max-bytes <n> Kept stderr prefix in bytes, 1..=1048576 (default 65536)."
    );
    println!();
    println!("PROGRAM runs directly with ARGS passed through verbatim — no");
    println!("implicit shell; pass `/bin/sh -c <script>` yourself if you need one.");
    println!("stdin is closed and the child gets its own process group. Captured");
    println!("stdout and stderr pass through unchanged;");
    println!("nothing else is printed for a completed run. Exit statuses:");
    println!("the child's exit code, 124 on timeout, 2 for invalid arguments,");
    println!("125 when the runner fails to start or wait for the child.");
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let invocation = match parse_args(&args) {
        Ok(Action::Help) => {
            print_help();
            return ExitCode::SUCCESS;
        }
        Ok(Action::Version) => {
            println!("voie-runner {VERSION}");
            return ExitCode::SUCCESS;
        }
        Ok(Action::Run(invocation)) => invocation,
        Err(message) => {
            eprintln!("voie-runner: {message}");
            return ExitCode::from(EXIT_MISUSE);
        }
    };

    let outcome = match run(&invocation) {
        Ok(outcome) => outcome,
        Err(error) => {
            eprintln!("voie-runner: failed to run command: {error}");
            return ExitCode::from(EXIT_RUN_FAILED);
        }
    };

    passthrough(&mut io::stdout().lock(), &outcome.stdout.bytes);
    passthrough(&mut io::stderr().lock(), &outcome.stderr.bytes);

    if outcome.timed_out {
        // The group was killed at the deadline; say so plainly instead of
        // echoing the kill signal as if it had come from outside.
        return ExitCode::from(EXIT_TIMED_OUT);
    }
    if let Some(code) = outcome.status.code() {
        return ExitCode::from(code as u8);
    }

    // The child died from a signal; die the same way so this process's wait
    // status is truthful to its own caller.
    if let Some(signal) = outcome.status.signal() {
        die_by_signal(signal);
    }

    // Unreachable on Unix: every exit status is code or signal.
    ExitCode::from(127)
}

fn passthrough(stream: &mut impl Write, bytes: &[u8]) {
    let _ = stream.write_all(bytes);
    let _ = stream.flush();
}

/// Restore the default disposition and raise `signal` against ourselves.
fn die_by_signal(signal: i32) -> ! {
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
    // Reachable only if the signal stayed blocked or ignored despite SIG_DFL;
    // fall back to the shell convention for death by that signal.
    std::process::exit(128 + signal);
}
