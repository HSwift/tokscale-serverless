mod auth;
mod coeffs;
mod config;
mod device;
mod error;
mod export;
mod handlers;
mod qoder;
mod scan;
mod state;
mod sync;

use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cfg = match config::Config::from_env() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("configuration error: {e}");
            std::process::exit(2);
        }
    };
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
                    Ok(scan::RefreshOutcome::Completed(snap)) => tracing::info!(
                        messages = snap.messages.len(),
                        "periodic rescan complete"
                    ),
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
    let _ = tokio::signal::ctrl_c().await;
}
