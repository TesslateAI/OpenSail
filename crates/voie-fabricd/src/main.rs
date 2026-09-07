//! Process launcher for `voie-fabricd`.
//!
//! Empty arguments start the daemon (the systemd unit invokes the binary with
//! no flags). Configuration is environment-only; `--help` and `--version`
//! remain available. The daemon serves only product HTTPS with mutual TLS:
//! without the certificate trio it refuses to start rather than fall back to
//! a plaintext transport.

use std::process::ExitCode;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use tokio::sync::Notify;
use voie_fabricd::{Config, Fabric, Live, serve_tls};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn print_help() {
    println!("voie-fabricd {VERSION}");
    println!("Portable VOIE Firecracker Fabric service.");
    println!();
    println!("Usage: voie-fabricd [--help | --version]");
    println!();
    println!("With no arguments the daemon listens for the private C2/C3 API over product mTLS.");
    println!("Bind address: VOIE_FABRICD_BIND (default 0.0.0.0:7840).");
    println!("SQLite path:  VOIE_FABRICD_SQLITE (default /var/lib/voie-fabricd/state.sqlite).");
    println!("Staging root: VOIE_FABRICD_STAGE_ROOT (default <sqlite-dir>/stage).");
    println!("Product mTLS: VOIE_FABRIC_CERT, VOIE_FABRIC_KEY, and VOIE_FABRIC_CA are required.");
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.len() == 1 && args[0] == "--help" {
        print_help();
        return ExitCode::SUCCESS;
    }
    if args.len() == 1 && args[0] == "--version" {
        println!("voie-fabricd {VERSION}");
        return ExitCode::SUCCESS;
    }
    if !args.is_empty() {
        eprintln!("voie-fabricd: unsupported arguments; use --help");
        return ExitCode::from(2);
    }

    let config = match Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("voie-fabricd: {error}");
            return ExitCode::from(2);
        }
    };

    let acceptor = match config.tls_acceptor() {
        Ok(acceptor) => acceptor,
        Err(error) => {
            eprintln!("voie-fabricd: {error}");
            return ExitCode::from(2);
        }
    };
    let bind = config.bind.clone();
    let client_sha256 = config.client_sha256.clone();
    let live = match Live::from_config(&config) {
        Ok(live) => live,
        Err(error) => {
            eprintln!("voie-fabricd: {error}");
            return ExitCode::from(2);
        }
    };
    let fabric = match Fabric::open(config, live) {
        Ok(fabric) => Arc::new(fabric),
        Err(error) => {
            eprintln!("voie-fabricd: cannot open sqlite: {error}");
            return ExitCode::FAILURE;
        }
    };

    // Before serving anything, classify crash leftovers. Reservations whose
    // realization is not positively absent stay held; nothing is guessed.
    let report = match fabric.reconcile_startup().await {
        Ok(report) => report,
        Err(error) => {
            eprintln!("voie-fabricd: startup reconciliation failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "voie-fabricd reconciliation: released={:?} held={:?} removed_lvs={:?} failed_lvs={:?} transient={:?} ready_without_volume={:?} released_allocations={:?} reopened={:?} reopen_failures={:?} pvs_replaced={:?} pods_rebound={:?}",
        report.orphan_reservations_released,
        report.orphan_reservations_held,
        report.orphan_lvs_removed,
        report.orphan_lv_failures,
        report.transient_workspaces,
        report.ready_without_volume,
        report.orphan_allocations_released,
        report.encrypted_volumes_reopened,
        report.encrypted_reopen_failures,
        report.stale_pvs_replaced,
        report.pods_rebound,
    );

    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("voie-fabricd: cannot bind {bind}: {error}");
            return ExitCode::FAILURE;
        }
    };

    println!("voie-fabricd listening on {bind} (product mTLS required)");

    let shutdown = Arc::new(Notify::new());
    let inflight = Arc::new(AtomicUsize::new(0));
    let residue = fabric.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            // Startup may run before kubelet has the gateway container.
            // Retry the host :8082 splice so a reboot does not leave the
            // control Tailscale edge dark until the next route PUT.
            if let Err(error) = residue.live().ensure_gateway_host_edge().await {
                eprintln!("voie-fabricd: gateway host edge: {error}");
            }
            // Spec heal and route/traffic heal share this tick but not a
            // wait: a leftover route ClusterIP miss must not delay WaitPod
            // Workspace observation. Held-volume release stays inside
            // reconcile_accepted_specs.
            let specs = voie_fabricd::reconcile_accepted_specs(&residue);
            let edge = voie_fabricd::reconcile_runtime_edge(&residue);
            let (specs, edge) = tokio::join!(specs, edge);
            if let Err(error) = specs {
                eprintln!("voie-fabricd: accepted-spec reconcile: {error}");
            }
            if let Err(error) = edge {
                eprintln!("voie-fabricd: route/traffic reconcile: {error}");
            }
        }
    });
    let server = tokio::spawn(serve_tls(
        listener,
        fabric,
        acceptor,
        Arc::from(client_sha256),
        shutdown.clone(),
        inflight.clone(),
    ));
    wait_for_shutdown_signal().await;
    println!("voie-fabricd shutting down: draining in-flight operations");
    // A stored permit survives the race where the serve loop has not yet
    // reached its select arm.
    shutdown.notify_one();
    let deadline = Instant::now() + Duration::from_secs(15);
    while inflight.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let remaining = inflight.load(Ordering::SeqCst);
    if remaining > 0 {
        // Interrupted operations stop at SQLite-consistent boundaries; the
        // next start reconciles their leftovers truthfully.
        eprintln!("voie-fabricd: drain window elapsed with {remaining} operation(s) interrupted");
    }
    server.abort();
    ExitCode::SUCCESS
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(terminate) => terminate,
            Err(error) => {
                eprintln!("voie-fabricd: cannot install SIGTERM handler: {error}");
                tokio::signal::ctrl_c().await.ok();
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.ok();
    }
}
