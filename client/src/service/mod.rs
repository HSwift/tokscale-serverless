const USAGE: &str = "tokscale-client service <install|uninstall|start|stop|restart|status|logs>";

pub fn execute(args: &[String]) -> Result<(), String> {
    if args.is_empty() || (args.len() == 1 && matches!(args[0].as_str(), "--help" | "-h" | "help"))
    {
        println!("{USAGE}\n\nInstall starts a user service using the saved connection.\nLinux: systemd user service, with a best-effort attempt to enable lingering.\nmacOS: launchd LaunchAgent, started automatically at login.\nUninstall keeps the executable, device identity, credentials, and logs.");
        return Ok(());
    }
    if args.len() != 1
        || !matches!(
            args[0].as_str(),
            "install" | "uninstall" | "start" | "stop" | "restart" | "status" | "logs"
        )
    {
        return Err(format!("usage: {USAGE}"));
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        native::execute(&args[0])
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err("service management is supported on Linux (systemd) and macOS (launchd); on Windows, use Task Scheduler to start `tokscale-client run`".to_string())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod native;
