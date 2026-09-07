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
const EDGE_ALLOW: &str = "/run/voie-fabricd/gateway-host-edge.allow";
pub const GATEWAY_HOST_EDGE_PORT: u16 = 8082;
pub const GATEWAY_HOST_EDGE_MAX_SPLICES: usize = 64;

const GATEWAY_HOST_EDGE_PY: &str = r#"import os, socket, subprocess, sys, threading

PID = int(sys.argv[1])
PORT = int(sys.argv[2])
CONTROL_IP = sys.argv[3]
MAX_SPLICES = int(sys.argv[4])
SLOTS = threading.BoundedSemaphore(MAX_SPLICES)
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
    try:
        fd = conn.fileno()
        os.set_inheritable(fd, True)
        subprocess.call(
            ["nsenter", "-t", str(PID), "-n", "python3", "-c", INNER, str(fd)],
            close_fds=False,
        )
    except Exception:
        pass
    finally:
        try:
            SLOTS.release()
        except Exception:
            pass
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
        conn, addr = srv.accept()
        ip = addr[0] if addr else ""
        if ip != CONTROL_IP:
            try:
                conn.close()
            except Exception:
                pass
            continue
        if not SLOTS.acquire(blocking=False):
            try:
                conn.close()
            except Exception:
                pass
            continue
        threading.Thread(target=handle, args=(conn,), daemon=True).start()

if __name__ == "__main__":
    main()
"#;

impl Live {
    pub async fn ensure_gateway_host_edge(&self) -> Result<(), FabricError> {
        let control_ip = gateway_control_ip()?;
        let gateway_pid = self.gateway_container_pid().await?;
        if host_edge_serves_pid(gateway_pid, &control_ip) {
            return Ok(());
        }
        stop_host_edge();
        start_host_edge(gateway_pid, &control_ip)
    }

    /// HTTP GET from the gateway netns to a cluster IPv4 URL. In-guest wget
    /// on 127.0.0.1 is not dataplane proof: a localhost bind looks Ready and
    /// then the edge returns 502. Proven probes the Pod IP because the
    /// Environment ClusterIP exists only after traffic cutover.
    pub async fn probe_http_via_gateway(&self, url: &str) -> bool {
        if !gateway_probe_url_ok(url) {
            return false;
        }
        let Ok(pid) = self.gateway_container_pid().await else {
            return false;
        };
        let url = url.to_owned();
        tokio::task::spawn_blocking(move || {
            std::process::Command::new("nsenter")
                .args([
                    "-t",
                    &pid.to_string(),
                    "-n",
                    "curl",
                    "-fsS",
                    "-m",
                    "3",
                    "-o",
                    "/dev/null",
                    &url,
                ])
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        })
        .await
        .unwrap_or(false)
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

fn gateway_probe_url_ok(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    !rest.is_empty()
        && !rest.contains('\n')
        && !rest.contains('\r')
        && !rest.contains(' ')
        && !rest.contains('\'')
        && !rest.contains('"')
        && !rest.contains(';')
}

fn gateway_control_ip() -> Result<String, FabricError> {
    let raw = std::env::var("VOIE_GATEWAY_CONTROL_IP").map_err(|_| {
        FabricError::Config(
            "VOIE_GATEWAY_CONTROL_IP is required when the production gateway host edge is enabled",
        )
    })?;
    let ip: std::net::Ipv4Addr = raw.trim().parse().map_err(|_| {
        FabricError::Config("VOIE_GATEWAY_CONTROL_IP must be a Tailscale IPv4 address")
    })?;
    Ok(ip.to_string())
}

fn host_edge_serves_pid(gateway_pid: u32, control_ip: &str) -> bool {
    let allow = fs::read_to_string(EDGE_ALLOW)
        .ok()
        .map(|value| value.trim().to_owned());
    if allow.as_deref() != Some(control_ip) {
        return false;
    }
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

fn start_host_edge(gateway_pid: u32, control_ip: &str) -> Result<(), FabricError> {
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
        .arg(control_ip)
        .arg(GATEWAY_HOST_EDGE_MAX_SPLICES.to_string())
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
    fs::write(EDGE_ALLOW, format!("{control_ip}\n")).map_err(|error| {
        FabricError::Realize(format!("cannot record gateway host-edge allow: {error}"))
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
        assert!(GATEWAY_HOST_EDGE_PY.contains("CONTROL_IP"));
        assert!(GATEWAY_HOST_EDGE_PY.contains("BoundedSemaphore"));
        assert!(GATEWAY_HOST_EDGE_PY.contains("acquire(blocking=False)"));
        assert!(!GATEWAY_HOST_EDGE_PY.contains("hostNetwork"));
        assert!(!GATEWAY_HOST_EDGE_PY.contains("LoadBalancer"));
        assert_eq!(GATEWAY_HOST_EDGE_MAX_SPLICES, 64);
    }

    #[test]
    fn crictl_inspect_pid_is_the_container_pid() {
        let json = r#"{"status":{},"info":{"pid":768492,"runtimeType":"io.containerd.runc.v2"}}"#;
        assert_eq!(parse_crictl_pid(json), Some(768492));
        assert_eq!(parse_crictl_pid("{}"), None);
        assert_eq!(parse_crictl_pid("not-json"), None);
    }

    #[test]
    fn gateway_probe_url_refuses_shell_metacharacters() {
        assert!(gateway_probe_url_ok("http://10.43.12.73:8080/healthz"));
        assert!(!gateway_probe_url_ok("https://10.43.12.73:8080/healthz"));
        assert!(!gateway_probe_url_ok("http://10.43.12.73:8080/healthz;id"));
        assert!(!gateway_probe_url_ok("http://10.43.12.73:8080/healthz\n"));
    }
}
