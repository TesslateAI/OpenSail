//! Thin process launcher for the voie-cloud state kernel.
//!
//! Configuration comes only from the environment: `VOIE_DATABASE_URL`
//! (required) and `VOIE_BIND` (optional listen address). The database URL is
//! never printed.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use voie_cloud::auth::{Auth, AuthConfig};
use voie_cloud::integration::Services;
use voie_cloud::{Config, Kernel, serve_with_services_graceful};

#[tokio::main]
async fn main() -> ExitCode {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("voie-cloud: {error}");
            return ExitCode::from(2);
        }
    };

    let kernel = match Kernel::connect(&config).await {
        Ok(kernel) => kernel,
        Err(error) => {
            eprintln!("voie-cloud: cannot reach PostgreSQL: {error}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(error) = kernel.migrate().await {
        eprintln!("voie-cloud: migration failed: {error}");
        return ExitCode::FAILURE;
    }

    let auth_config = match AuthConfig::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("voie-cloud: auth configuration failed: {error}");
            return ExitCode::from(2);
        }
    };
    let kernel = Arc::new(kernel);
    let auth = match Auth::connect(auth_config, kernel.pool().clone()).await {
        Ok(auth) => Arc::new(auth),
        Err(error) => {
            eprintln!("voie-cloud: auth configuration failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    // Native admin bootstrap: idempotent, runs at every startup when
    // native auth is enabled and VOIE_NATIVE_ADMIN_USERNAME is set.
    if let Err(error) = auth.bootstrap_native_admin(&kernel).await {
        eprintln!("voie-cloud: native admin bootstrap failed: {error}");
        return ExitCode::FAILURE;
    }
    let services = match Services::from_env(kernel.pool().clone()) {
        Ok(services) => services,
        Err(error) => {
            eprintln!("voie-cloud: backend configuration failed: {error}");
            return ExitCode::from(2);
        }
    };
    if let Err(error) = services.recover(&kernel).await {
        eprintln!("voie-cloud: run recovery failed: {error}");
        return ExitCode::FAILURE;
    }
    let bind = std::env::var("VOIE_BIND").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("voie-cloud: cannot bind {bind}: {error}");
            return ExitCode::FAILURE;
        }
    };

    println!("voie-cloud listening on {bind}");
    let running = serve_with_services_graceful(listener, kernel, auth, services);

    // SIGTERM and Ctrl-C both resolve through the same bounded drain:
    // stop accepting, finish in-flight requests, then exit.
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("SIGTERM handler installs");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = terminate.recv() => {},
    }
    println!("voie-cloud draining connections");
    match running.drain(Duration::from_secs(15)).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("voie-cloud: drain failed: {error}");
            ExitCode::FAILURE
        }
    }
}
