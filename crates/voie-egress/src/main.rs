//! Platform Application egress proxy.
//!
//! Application Pods do not receive Workspace CIDR egress. They speak HTTP or
//! CONNECT to this process; the proxy's NetworkPolicy is what may reach
//! deployment-approved CIDRs. This is not a user ingress policy or mesh.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

const LISTEN: &str = "0.0.0.0:8080";
const MAX_HEADER_BYTES: usize = 8192;
const IO_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONCURRENT_CONNECTIONS: usize = 256;
const MAX_PER_SOURCE: usize = 32;

struct ConnectionSlots {
    global: AtomicUsize,
    per_source: Mutex<HashMap<IpAddr, usize>>,
}

impl ConnectionSlots {
    fn new() -> Self {
        Self {
            global: AtomicUsize::new(0),
            per_source: Mutex::new(HashMap::new()),
        }
    }

    fn acquire(&self, source: IpAddr) -> bool {
        loop {
            let live = self.global.load(Ordering::Relaxed);
            if live >= MAX_CONCURRENT_CONNECTIONS {
                return false;
            }
            if self
                .global
                .compare_exchange(live, live + 1, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }
        let mut map = match self.per_source.lock() {
            Ok(map) => map,
            Err(_) => {
                self.global.fetch_sub(1, Ordering::SeqCst);
                return false;
            }
        };
        let count = map.entry(source).or_insert(0);
        if *count >= MAX_PER_SOURCE {
            drop(map);
            self.global.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        *count += 1;
        true
    }

    fn release(&self, source: IpAddr) {
        if let Ok(mut map) = self.per_source.lock() {
            if let Some(count) = map.get_mut(&source) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    map.remove(&source);
                }
            }
        }
        self.global.fetch_sub(1, Ordering::SeqCst);
    }
}

fn slots() -> &'static ConnectionSlots {
    static SLOTS: OnceLock<ConnectionSlots> = OnceLock::new();
    SLOTS.get_or_init(ConnectionSlots::new)
}

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
        let Ok(peer) = stream.peer_addr() else {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            continue;
        };
        let source = peer.ip();
        if !slots().acquire(source) {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            continue;
        }
        thread::spawn(move || {
            let _ = handle_client(stream);
            slots().release(source);
        });
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
    match resolve_target(&target) {
        Resolve::Ok(addr) => {
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
        Resolve::Refused => {
            let _ = client.write_all(
                b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            Ok(())
        }
        Resolve::Failed => {
            let _ = client.write_all(
                b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            Ok(())
        }
    }
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

#[derive(Debug, PartialEq, Eq)]
enum Resolve {
    Ok(SocketAddr),
    Refused,
    Failed,
}

fn resolve_target(hostport: &str) -> Resolve {
    let Ok(addrs) = hostport.to_socket_addrs() else {
        return Resolve::Failed;
    };
    pick_resolved(addrs)
}

/// Prefer IPv4: egress NetworkPolicy is IPv4, and node resolvers often
/// return AAAA first.
fn pick_resolved(addrs: impl IntoIterator<Item = SocketAddr>) -> Resolve {
    let mut saw_special = false;
    let mut first_v6 = None;
    for addr in addrs {
        if is_refused_ip(addr.ip()) {
            saw_special = true;
            continue;
        }
        if addr.is_ipv4() {
            return Resolve::Ok(addr);
        }
        if first_v6.is_none() {
            first_v6 = Some(addr);
        }
    }
    if let Some(addr) = first_v6 {
        Resolve::Ok(addr)
    } else if saw_special {
        Resolve::Refused
    } else {
        Resolve::Failed
    }
}

fn is_refused_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unicast_link_local()
                || v6.is_unspecified()
                || v6.is_multicast()
        }
    }
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
    use super::{
        ConnectionSlots, MAX_PER_SOURCE, Resolve, is_refused_ip, parse_proxy_target, pick_resolved,
        resolve_target,
    };
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

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

    #[test]
    fn special_addresses_are_refused() {
        assert!(is_refused_ip("127.0.0.1".parse().unwrap()));
        assert!(is_refused_ip("::1".parse().unwrap()));
        assert!(is_refused_ip("169.254.1.1".parse().unwrap()));
        assert!(is_refused_ip("0.0.0.0".parse().unwrap()));
        assert!(is_refused_ip("224.0.0.1".parse().unwrap()));
        assert!(is_refused_ip("ff02::1".parse().unwrap()));
        assert!(!is_refused_ip("1.1.1.1".parse().unwrap()));
        assert!(!is_refused_ip("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn loopback_and_link_local_targets_resolve_as_refused() {
        assert_eq!(resolve_target("127.0.0.1:80"), Resolve::Refused);
        assert_eq!(resolve_target("169.254.1.1:80"), Resolve::Refused);
        assert_eq!(resolve_target("0.0.0.0:80"), Resolve::Refused);
    }

    #[test]
    fn pick_resolved_prefers_ipv4_over_aaaa() {
        let addrs = [
            SocketAddr::from((Ipv6Addr::new(0x2606, 0, 0, 0, 0, 0, 0, 1), 80)),
            SocketAddr::from((Ipv4Addr::new(1, 1, 1, 1), 80)),
        ];
        assert_eq!(
            pick_resolved(addrs),
            Resolve::Ok(SocketAddr::from((Ipv4Addr::new(1, 1, 1, 1), 80)))
        );
    }

    #[test]
    fn per_source_ceiling_leaves_capacity_for_another_source() {
        let slots = ConnectionSlots::new();
        let a: IpAddr = "10.0.0.1".parse().unwrap();
        let b: IpAddr = "10.0.0.2".parse().unwrap();
        for _ in 0..MAX_PER_SOURCE {
            assert!(slots.acquire(a));
        }
        assert!(!slots.acquire(a), "one source cannot exhaust the proxy");
        assert!(
            slots.acquire(b),
            "a second source still connects below its own limit"
        );
        slots.release(a);
        assert!(slots.acquire(a));
    }
}
