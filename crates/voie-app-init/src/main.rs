//! Process supervisor for one Application argv. Kubernetes restarts the Pod;
//! this binary never restarts the child itself and never offers a shell.

use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicI32, Ordering};

const APP_ROOT: &str = "/app";
const BIND_ANY: &str = "/lib/libvoie-bind-any.so";

static CHILD_PGID: AtomicI32 = AtomicI32::new(0);

fn main() -> ExitCode {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("--") {
        argv.remove(0);
    }
    if argv.is_empty() {
        eprintln!("voie-app-init: missing command");
        return ExitCode::from(2);
    }
    if !Path::new(APP_ROOT).is_dir() {
        eprintln!("voie-app-init: {APP_ROOT} is not a directory");
        return ExitCode::from(2);
    }
    install_signals();
    let mut command = Command::new(&argv[0]);
    let path = std::env::var("PATH").unwrap_or_default();
    command
        .args(&argv[1..])
        .current_dir(APP_ROOT)
        .env("PATH", format!("/bin:/usr/bin:{path}"));
    apply_listen_env(&mut command);
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = writeln!(io::stderr(), "voie-app-init: failed to exec: {error}");
            return ExitCode::from(125);
        }
    };
    CHILD_PGID.store(child.id() as i32, Ordering::SeqCst);
    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            let _ = writeln!(io::stderr(), "voie-app-init: wait failed: {error}");
            return ExitCode::from(125);
        }
    };
    reap_descendants();
    if let Some(code) = status.code() {
        return ExitCode::from(code as u8);
    }
    ExitCode::from(1)
}

fn compose_ld_preload(existing: Option<&str>) -> String {
    match existing {
        Some(existing) if !existing.is_empty() => format!("{BIND_ANY}:{existing}"),
        _ => BIND_ANY.to_string(),
    }
}

fn apply_listen_env(command: &mut Command) {
    if Path::new(BIND_ANY).is_file() {
        command.env(
            "LD_PRELOAD",
            compose_ld_preload(std::env::var("LD_PRELOAD").ok().as_deref()),
        );
    }
    if std::env::var_os("HOST").is_none() {
        command.env("HOST", "0.0.0.0");
    }
}

fn install_signals() {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = forward_signal as *const () as usize;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGHUP, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGQUIT, &action, std::ptr::null_mut());
    }
}

fn kill_target(pgid: i32) -> libc::pid_t {
    if pgid > 0 { -pgid } else { 0 }
}

extern "C" fn forward_signal(signum: libc::c_int) {
    let target = kill_target(CHILD_PGID.load(Ordering::SeqCst));
    if target < 0 {
        unsafe {
            libc::kill(target, signum);
        }
    }
}

fn reap_descendants() {
    unsafe {
        let mut status = 0;
        while libc::waitpid(-1, &mut status, libc::WNOHANG) > 0 {}
    }
}

#[cfg(test)]
mod tests {
    use super::kill_target;

    #[test]
    fn forwards_to_the_child_process_group() {
        assert_eq!(kill_target(42), -42);
        assert_eq!(kill_target(0), 0);
        assert_eq!(kill_target(-1), 0);
    }

    #[test]
    fn ld_preload_keeps_existing_entries() {
        assert_eq!(super::compose_ld_preload(None), super::BIND_ANY);
        assert_eq!(
            super::compose_ld_preload(Some("libother.so")),
            format!("{}:libother.so", super::BIND_ANY)
        );
    }
}
