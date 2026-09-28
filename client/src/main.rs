mod auth;
mod coeffs;
mod config;
mod connect;
mod device;
mod error;
mod export;
mod handlers;
mod qoder;
mod scan;
mod service;
mod state;
mod sync;

use axum::routing::{get, post};
use axum::Router;
use std::io::IsTerminal;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("");
    if matches!(command, "--help" | "-h" | "help") {
        println!("tokscale-client [connect [WORKER_URL] | run | local | service <COMMAND>]\n\n  connect  Verify and save Worker URL/token (token input is hidden)\n  run      Collect and sync using saved credentials, without prompts\n  local    Collect locally without cloud sync\n  service  Install and manage a Linux/macOS background service\n           Commands: install, uninstall, start, stop, restart, status, logs\n\nWith no command, first-time interactive startup offers connection setup.\nConfiguration: {}\nEnvironment variables override saved configuration.", config::connection_path().display());
        return;
    }
    if command == "service" {
        if let Err(error) = service::execute(&args[1..]) {
            eprintln!("service error: {error}");
            std::process::exit(2);
        }
        return;
    }
    if command == "connect" && args.len() <= 2 {
        if let Err(error) = connect::connect(args.get(1).cloned()).await {
            eprintln!("connection error: {error}");
            std::process::exit(2);
        }
        return;
    }
    if !matches!(command, "" | "run" | "local") || args.len() > 1 {
        eprintln!(
            "usage: tokscale-client [connect [WORKER_URL] | run | local | service <COMMAND>]"
        );
        std::process::exit(2);
    }

    let configuration = if command == "local" {
        config::Config::local_from_env()
    } else {
        config::Config::from_env()
    };
    let mut cfg = match configuration {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("configuration error: {e}");
            std::process::exit(2);
        }
    };
    if command.is_empty()
        && cfg.sync_url.is_none()
        && std::env::var_os("SYNC_URL").is_none()
        && std::io::stdin().is_terminal()
    {
        if let Err(error) = connect::connect(None).await {
            eprintln!("connection error: {error}");
            std::process::exit(2);
        }
        cfg = config::Config::from_env().unwrap_or_else(|error| {
            eprintln!("configuration error: {error}");
            std::process::exit(2);
        });
    }
    if command == "run" && cfg.sync_url.is_none() {
        eprintln!("no Worker connection configured; run `tokscale-client connect` first");
        std::process::exit(2);
    }
    let bind_addr = cfg.bind_addr.clone();
    let auth_enabled = cfg.api_token.is_some();
    let state = Arc::new(state::AppState::new(cfg, device::resolve()));

    {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            match scan::refresh(&state).await {
                Ok(scan::RefreshOutcome::Completed(snap)) => tracing::info!(
                    messages = snap.messages.len(),
                    duration_ms = snap.scan_duration_ms,
                    "initial scan complete"
                ),
                Ok(scan::RefreshOutcome::AlreadyScanning) => {}
                Err(e) => tracing::error!("initial scan failed: {e}"),
            }
        });
    }

    if state.cfg.refresh_interval_secs > 0 {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            let interval = std::time::Duration::from_secs(state.cfg.refresh_interval_secs);
            loop {
                tokio::time::sleep(interval).await;
                match scan::refresh(&state).await {
                    Ok(scan::RefreshOutcome::Completed(snap)) => {
                        tracing::info!(messages = snap.messages.len(), "periodic rescan complete")
                    }
                    Ok(scan::RefreshOutcome::AlreadyScanning) => {}
                    Err(e) => tracing::error!("periodic rescan failed: {e}"),
                }
            }
        });
    }

    let api = Router::new()
        .route("/api/summary", get(handlers::summary))
        .route("/api/daily", get(handlers::daily))
        .route("/api/models", get(handlers::models))
        .route("/api/clients", get(handlers::clients))
        .route("/api/sessions", get(handlers::sessions))
        .route("/api/export", get(handlers::export))
        .route("/api/refresh", post(handlers::refresh))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ));

    let app = Router::new()
        .route("/health", get(handlers::health))
        .merge(api)
        .with_state(state);

    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("failed to bind {bind_addr}: {e}");
            std::process::exit(1);
        }
    };
    tracing::info!("tokscale-client listening on http://{bind_addr}");
    if auth_enabled {
        tracing::info!("bearer auth enabled for /api/*");
    }
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        eprintln!("serve error: {e}");
        std::process::exit(1);
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {},
                    _ = terminate.recv() => {},
                }
            }
            Err(error) => {
                tracing::warn!("could not install SIGTERM handler: {error}");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
