//! Host :8082 splice into the in-cluster gateway netns.
//!
//! Cilium kube-proxy-replacement does not publish hostPort for this
//! runc gateway, and hostNetwork cannot reach Firecracker Application
//! ClusterIPs. Control Caddy still reverse-proxies to
//! `http://baremetal-1:8082`, so fabricd owns a host listener that
//! `nsenter`s the gateway container and splices to 127.0.0.1:8082.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::FabricError;
use crate::realize::Live;

const EDGE_DIR: &str = "/run/voie-fabricd";
const EDGE_SCRIPT: &str = "/run/voie-fabricd/gateway-host-edge.py";
const EDGE_PIDFILE: &str = "/run/voie-fabricd/gateway-host-edge.pid";
const EDGE_TARGET: &str = "/run/voie-fabricd/gateway-host-edge.target";
const LEGACY_PIDFILE: &str = "/run/voie-gw-edge.pid";
pub const GATEWAY_HOST_EDGE_PORT: u16 = 8082;

const GATEWAY_HOST_EDGE_PY: &str = r#"import os, socket, subprocess, sys, threading

PID = int(sys.argv[1])
PORT = int(sys.argv[2])
INNER = r'''
import socket, select, sys
client = socket.socket(fileno=int(sys.argv[1]))
backend = socket.create_connection(("127.0.0.1", 8082))
sockets = [client, backend]
try:
    while True:
        r, _, x = select.select(sockets, [], sockets, 60)
        if x:
            break
        if not r:
            continue
        if client in r:
            data = client.recv(65536)
            if not data:
                break
            backend.sendall(data)
        if backend in r:
            data = backend.recv(65536)
            if not data:
                break
            client.sendall(data)
finally:
    try:
        client.close()
    except Exception:
        pass
    try:
        backend.close()
    except Exception:
        pass
'''

def handle(conn):
    fd = conn.fileno()
    os.set_inheritable(fd, True)
    try:
        subprocess.call(
            ["nsenter", "-t", str(PID), "-n", "python3", "-c", INNER, str(fd)],
            close_fds=False,
        )
    finally:
        try:
            conn.close()
        except Exception:
            pass

def main():
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("0.0.0.0", PORT))
    srv.listen(128)
    while True:
        conn, _ = srv.accept()
        threading.Thread(target=handle, args=(conn,), daemon=True).start()

if __name__ == "__main__":
    main()
"#;

impl Live {
    /// Keeps host tcp/8082 spliced into the running gateway container
    /// netns. Adopts a still-correct live helper; replaces a stale one.
    pub async fn ensure_gateway_host_edge(&self) -> Result<(), FabricError> {
        let gateway_pid = self.gateway_container_pid().await?;
        if host_edge_serves_pid(gateway_pid) {
            return Ok(());
        }
        stop_host_edge();
        start_host_edge(gateway_pid)
    }

    async fn gateway_container_pid(&self) -> Result<u32, FabricError> {
        let pods = self
            .crictl(&[
                "pods",
                "--name",
                "voie-gateway",
                "-q",
                "--namespace",
                self.namespace(),
            ])
            .await?;
        if pods.status != 0 {
            return Err(FabricError::Conflict(
                "cannot observe voie-gateway sandbox for host edge".into(),
            ));
        }
        let sandbox = pods
            .stdout
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .ok_or_else(|| {
                FabricError::Conflict("voie-gateway sandbox is absent for host edge".into())
            })?;
        let containers = self.crictl(&["ps", "--pod", sandbox, "-q"]).await?;
        if containers.status != 0 {
            return Err(FabricError::Conflict(
                "cannot observe voie-gateway container for host edge".into(),
            ));
        }
        let cid = containers
            .stdout
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .ok_or_else(|| {
                FabricError::Conflict("voie-gateway container is absent for host edge".into())
            })?;
        let inspect = self.crictl(&["inspect", cid]).await?;
        if inspect.status != 0 {
            return Err(FabricError::Conflict(format!(
                "cannot inspect voie-gateway container: {}",
                inspect.stderr.trim()
            )));
        }
        parse_crictl_pid(&inspect.stdout)
            .ok_or_else(|| FabricError::Conflict("voie-gateway container pid is unreadable".into()))
    }
}

pub(crate) fn parse_crictl_pid(inspect_json: &str) -> Option<u32> {
    let value: serde_json::Value = serde_json::from_str(inspect_json).ok()?;
    value
        .get("info")
        .and_then(|info| info.get("pid"))
        .and_then(|pid| pid.as_u64())
        .and_then(|pid| u32::try_from(pid).ok())
}

fn host_edge_serves_pid(gateway_pid: u32) -> bool {
    if process_alive(read_pidfile(EDGE_PIDFILE)) && read_pidfile(EDGE_TARGET) == Some(gateway_pid) {
        return true;
    }
    if let Some(legacy) = read_pidfile(LEGACY_PIDFILE) {
        if process_alive(Some(legacy)) && cmdline_contains_pid(legacy, gateway_pid) {
            return true;
        }
    }
    false
}

fn start_host_edge(gateway_pid: u32) -> Result<(), FabricError> {
    fs::create_dir_all(EDGE_DIR).map_err(|error| {
        FabricError::Realize(format!("cannot create gateway host-edge dir: {error}"))
    })?;
    fs::write(EDGE_SCRIPT, GATEWAY_HOST_EDGE_PY).map_err(|error| {
        FabricError::Realize(format!("cannot write gateway host-edge helper: {error}"))
    })?;
    let child = Command::new("python3")
        .arg(EDGE_SCRIPT)
        .arg(gateway_pid.to_string())
        .arg(GATEWAY_HOST_EDGE_PORT.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            FabricError::Realize(format!("cannot start gateway host-edge: {error}"))
        })?;
    fs::write(EDGE_PIDFILE, format!("{}\n", child.id())).map_err(|error| {
        FabricError::Realize(format!("cannot record gateway host-edge pid: {error}"))
    })?;
    fs::write(EDGE_TARGET, format!("{gateway_pid}\n")).map_err(|error| {
        FabricError::Realize(format!("cannot record gateway host-edge target: {error}"))
    })?;
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    if !process_alive(read_pidfile(EDGE_PIDFILE)) {
        return Err(FabricError::Conflict(
            "gateway host-edge exited before serving".into(),
        ));
    }
    Ok(())
}

fn stop_host_edge() {
    for path in [EDGE_PIDFILE, LEGACY_PIDFILE] {
        if let Some(pid) = read_pidfile(path) {
            let _ = Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .status();
        }
    }
}

fn read_pidfile(path: &str) -> Option<u32> {
    fs::read_to_string(path)
        .ok()?
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
}

fn process_alive(pid: Option<u32>) -> bool {
    let Some(pid) = pid else {
        return false;
    };
    Path::new(&format!("/proc/{pid}")).exists()
}

fn cmdline_contains_pid(pid: u32, gateway_pid: u32) -> bool {
    fs::read(format!("/proc/{pid}/cmdline"))
        .ok()
        .is_some_and(|bytes| {
            let text = String::from_utf8_lossy(&bytes);
            text.split('\0').any(|part| part == gateway_pid.to_string())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_edge_script_splices_into_gateway_netns() {
        assert!(GATEWAY_HOST_EDGE_PY.contains("nsenter"));
        assert!(GATEWAY_HOST_EDGE_PY.contains("python3"));
        assert!(GATEWAY_HOST_EDGE_PY.contains("127.0.0.1"));
        assert!(GATEWAY_HOST_EDGE_PY.contains("8082"));
        assert!(GATEWAY_HOST_EDGE_PY.contains("set_inheritable"));
        assert!(!GATEWAY_HOST_EDGE_PY.contains("hostNetwork"));
        assert!(!GATEWAY_HOST_EDGE_PY.contains("LoadBalancer"));
    }

    #[test]
    fn crictl_inspect_pid_is_the_container_pid() {
        let json = r#"{"status":{},"info":{"pid":768492,"runtimeType":"io.containerd.runc.v2"}}"#;
        assert_eq!(parse_crictl_pid(json), Some(768492));
        assert_eq!(parse_crictl_pid("{}"), None);
        assert_eq!(parse_crictl_pid("not-json"), None);
    }
}
