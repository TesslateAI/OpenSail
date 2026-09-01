//! Platform Application egress proxy.
//!
//! Application Pods do not receive Workspace CIDR egress. They speak HTTP or
//! CONNECT to this process; the proxy's NetworkPolicy is what may reach
//! deployment-approved CIDRs. This is not a user ingress policy or mesh.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

const LISTEN: &str = "0.0.0.0:8080";
const MAX_HEADER_BYTES: usize = 8192;
const IO_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONCURRENT_CONNECTIONS: usize = 256;
static LIVE_CONNECTIONS: AtomicUsize = AtomicUsize::new(0);

fn main() {
    let listener = match TcpListener::bind(LISTEN) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("voie-egress: bind {LISTEN}: {error}");
            std::process::exit(2);
        }
    };
    for incoming in listener.incoming() {
        let Ok(stream) = incoming else {
            continue;
        };
        loop {
            let live = LIVE_CONNECTIONS.load(Ordering::Relaxed);
            if live >= MAX_CONCURRENT_CONNECTIONS {
                let _ = stream.shutdown(std::net::Shutdown::Both);
                break;
            }
            if LIVE_CONNECTIONS
                .compare_exchange(live, live + 1, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                thread::spawn(move || {
                    let _ = handle_client(stream);
                    LIVE_CONNECTIONS.fetch_sub(1, Ordering::SeqCst);
                });
                break;
            }
        }
    }
}

fn handle_client(mut client: TcpStream) -> io::Result<()> {
    client.set_read_timeout(Some(IO_TIMEOUT))?;
    client.set_write_timeout(Some(IO_TIMEOUT))?;
    let header = read_http_head(&mut client)?;
    let target = match parse_proxy_target(&header) {
        Some(target) => target,
        None => {
            let _ = client.write_all(
                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return Ok(());
        }
    };
    let addr = match resolve_target(&target) {
        Some(addr) => addr,
        None => {
            let _ = client.write_all(
                b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return Ok(());
        }
    };
    let mut upstream = TcpStream::connect_timeout(&addr, IO_TIMEOUT)?;
    upstream.set_read_timeout(Some(IO_TIMEOUT))?;
    upstream.set_write_timeout(Some(IO_TIMEOUT))?;
    if header.starts_with("CONNECT ") {
        client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
    } else {
        upstream.write_all(header.as_bytes())?;
    }
    tunnel(client, upstream)
}

fn read_http_head(stream: &mut TcpStream) -> io::Result<String> {
    let mut bytes = Vec::new();
    let mut one = [0u8; 1];
    while bytes.len() < MAX_HEADER_BYTES {
        let n = stream.read(&mut one)?;
        if n == 0 {
            break;
        }
        bytes.push(one[0]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    if bytes.len() >= MAX_HEADER_BYTES && !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "proxy request head exceeded the bound",
        ));
    }
    String::from_utf8(bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not utf-8"))
}

/// CONNECT host:port or absolute-form `http://host[:port]/...`.
pub fn parse_proxy_target(head: &str) -> Option<String> {
    let line = head.lines().next()?.trim();
    if line.contains('\0') {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    let _version = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if method.eq_ignore_ascii_case("CONNECT") {
        return validate_hostport(target);
    }
    absolute_http_hostport(target)
}

fn absolute_http_hostport(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("http://")?;
    let hostport = rest.split('/').next().unwrap_or(rest);
    if hostport.contains(':') {
        validate_hostport(hostport)
    } else {
        validate_hostport(&format!("{hostport}:80"))
    }
}

fn validate_hostport(value: &str) -> Option<String> {
    let (host, port) = value.rsplit_once(':')?;
    if host.is_empty() || host.starts_with('[') || host.contains('/') || host.contains('\\') {
        return None;
    }
    if host.chars().any(|ch| ch.is_ascii_control() || ch == ' ') {
        return None;
    }
    let port: u16 = port.parse().ok()?;
    if port == 0 {
        return None;
    }
    Some(format!("{host}:{port}"))
}

fn resolve_target(hostport: &str) -> Option<SocketAddr> {
    hostport.to_socket_addrs().ok()?.next()
}

fn tunnel(mut left: TcpStream, mut right: TcpStream) -> io::Result<()> {
    let mut left_to_right = left.try_clone()?;
    let mut right_in = right.try_clone()?;
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = left_to_right.read(&mut buf) {
            if n == 0 {
                break;
            }
            if right.write_all(&buf[..n]).is_err() {
                break;
            }
        }
        let _ = right.shutdown(std::net::Shutdown::Write);
    });
    let mut buf = [0u8; 8192];
    while let Ok(n) = right_in.read(&mut buf) {
        if n == 0 {
            break;
        }
        if left.write_all(&buf[..n]).is_err() {
            break;
        }
    }
    let _ = left.shutdown(std::net::Shutdown::Write);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_proxy_target;

    #[test]
    fn parses_connect_and_absolute_form() {
        assert_eq!(
            parse_proxy_target("CONNECT example.com:443 HTTP/1.1\r\n\r\n").as_deref(),
            Some("example.com:443")
        );
        assert_eq!(
            parse_proxy_target(
                "GET http://example.com/healthz HTTP/1.1\r\nHost: example.com\r\n\r\n"
            )
            .as_deref(),
            Some("example.com:80")
        );
        assert_eq!(
            parse_proxy_target("GET http://example.com:8081/x HTTP/1.1\r\n\r\n").as_deref(),
            Some("example.com:8081")
        );
    }

    #[test]
    fn refuses_control_and_origin_form() {
        assert!(parse_proxy_target("CONNECT example.com:0 HTTP/1.1\r\n\r\n").is_none());
        assert!(parse_proxy_target("GET /healthz HTTP/1.1\r\nHost: example.com\r\n\r\n").is_none());
        assert!(parse_proxy_target("CONNECT example.com:443 extra HTTP/1.1\r\n\r\n").is_none());
        assert!(parse_proxy_target("CONNECT [::1]:443 HTTP/1.1\r\n\r\n").is_none());
    }
}
