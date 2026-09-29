mod coeffs;
mod config;
mod connect;
mod device;
mod diagnostics;
mod export;
mod http;
mod qoder;
mod scan;
mod service;
mod settings;
mod sync;

use std::io::IsTerminal;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("");
    if matches!(command, "--version" | "-V") {
        println!("tokscale-client {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if command == "debug" {
        let mode = args.get(1).map(String::as_str);
        if args.len() > 2 || !matches!(mode, None | Some("--local" | "--sync")) {
            eprintln!("usage: tokscale-client debug [--local | --sync]");
            std::process::exit(2);
        }
        let ok = diagnostics::run(mode == Some("--local"), mode == Some("--sync")).await;
        std::process::exit(if ok { 0 } else { 1 });
    }
    if matches!(command, "--help" | "-h" | "help") {
        println!("tokscale-client [connect [WORKER_URL] | run | sync [--full] | debug [--local | --sync] | service <COMMAND>]\n\n  connect  Verify and save Worker URL/token (token input is hidden)\n  run      Collect periodically; upload only changed usage\n  sync     Collect and sync once, then exit; --full resends all local history\n  debug    JSON diagnostics: collection, sync plan, network, auth and D1\n           --local skips remote probes; --sync also uploads changes\n  service  Install and manage a Linux/macOS background service\n           Commands: install, uninstall, start, stop, restart, status, logs\n\nWith no command, first-time interactive startup offers connection setup.\nConfiguration: {}\nEnvironment variables override saved configuration.", config::connection_path().display());
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
    let once = command == "sync";
    let full = once && args.get(1).is_some_and(|arg| arg == "--full");
    if !(once && (args.len() == 1 || (full && args.len() == 2))
        || matches!(command, "" | "run") && args.len() <= 1)
    {
        eprintln!("usage: tokscale-client [connect [WORKER_URL] | run | sync [--full] | debug [--local | --sync] | service <COMMAND>]");
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
    if once {
        if let Err(error) = collect_and_sync(&cfg, &device, full, true).await {
            eprintln!("sync failed: {error}");
            std::process::exit(1);
        }
        return;
    }
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    tracing::info!(
        interval_secs = cfg.refresh_interval_secs,
        "collector started"
    );

    loop {
        let cycle = async {
            if let Err(error) = collect_and_sync(&cfg, &device, false, false).await {
                tracing::warn!("collection or sync failed: {error}");
            }
        };
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

async fn collect_and_sync(
    cfg: &config::Config,
    device: &device::DeviceInfo,
    full: bool,
    wait_for_lock: bool,
) -> Result<(), String> {
    // Lock before scanning: a manual sync must not race a background scan and
    // overwrite newer data with a snapshot collected before the lock was held.
    let Some(session) = sync::Session::acquire(wait_for_lock).await? else {
        tracing::debug!("another sync is running; skipping this scan");
        return Ok(());
    };
    let snapshot = scan::collect(cfg).await?;
    tracing::info!(
        messages = snapshot.messages.len(),
        duration_ms = snapshot.scan_duration_ms,
        "scan complete"
    );
    session.push(cfg, device, snapshot, full).await
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
