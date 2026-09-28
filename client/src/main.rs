mod coeffs;
mod config;
mod connect;
mod device;
mod export;
mod qoder;
mod scan;
mod service;
mod sync;

use std::io::IsTerminal;

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
        println!("tokscale-client [connect [WORKER_URL] | run | service <COMMAND>]\n\n  connect  Verify and save Worker URL/token (token input is hidden)\n  run      Collect and sync using saved credentials, without prompts\n  service  Install and manage a Linux/macOS background service\n           Commands: install, uninstall, start, stop, restart, status, logs\n\nWith no command, first-time interactive startup offers connection setup.\nConfiguration: {}\nEnvironment variables override saved configuration.", config::connection_path().display());
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
    if !matches!(command, "" | "run") || args.len() > 1 {
        eprintln!("usage: tokscale-client [connect [WORKER_URL] | run | service <COMMAND>]");
        std::process::exit(2);
    }

    let mut cfg = match config::Config::from_env() {
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
    if cfg.sync_url.is_none() {
        eprintln!("no Worker connection configured; run `tokscale-client connect` first");
        std::process::exit(2);
    }
    let device = device::resolve();
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    tracing::info!(
        interval_secs = cfg.refresh_interval_secs,
        "collector started"
    );

    loop {
        let cycle = collect_and_sync(&cfg, &device);
        tokio::pin!(cycle);
        tokio::select! {
            _ = &mut shutdown => {
                tracing::info!("shutdown requested; finishing current collection and upload");
                cycle.await;
                break;
            }
            _ = &mut cycle => {}
        }
        let next_scan = async {
            if cfg.refresh_interval_secs == 0 {
                std::future::pending::<()>().await;
            } else {
                tokio::time::sleep(std::time::Duration::from_secs(cfg.refresh_interval_secs)).await;
            }
        };
        tokio::select! {
            _ = &mut shutdown => break,
            _ = next_scan => {}
        }
    }
    tracing::info!("collector stopped");
}

async fn collect_and_sync(cfg: &config::Config, device: &device::DeviceInfo) {
    match scan::collect(cfg).await {
        Ok(snapshot) => {
            tracing::info!(
                messages = snapshot.messages.len(),
                duration_ms = snapshot.scan_duration_ms,
                "scan complete"
            );
            if let Err(error) = sync::push(cfg, device, snapshot).await {
                tracing::warn!("cloud sync push failed: {error}");
            }
        }
        Err(error) => tracing::error!("scan failed: {error}"),
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
