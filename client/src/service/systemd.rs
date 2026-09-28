use super::*;

const UNIT: &str = "tokscale-client.service";
const TEMPLATE: &str = include_str!("../../systemd/tokscale-client.service");

fn unit_path(paths: &Paths) -> Result<PathBuf, String> {
    let directory = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| paths.home.join(".config"));
    Ok(absolute(&directory)?.join("systemd/user").join(UNIT))
}

pub(super) fn preflight(_paths: &Paths) -> Result<(), String> {
    let result = output("systemctl", &["--user", "show-environment"])?;
    if result.status.success() {
        Ok(())
    } else {
        Err("cannot reach the systemd user manager; run this command as the logged-in user on a systemd host".into())
    }
}

fn quote(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
            .replace('%', "%%")
    )
}

fn render(paths: &Paths, environment: &BTreeMap<String, String>) -> Result<String, String> {
    let executable = quote(path_text(&paths.binary)?);
    let env = environment
        .iter()
        .map(|(key, value)| format!("Environment={}\n", quote(&format!("{key}={value}"))))
        .collect::<String>();
    Ok(TEMPLATE.replace(
        "ExecStart=%h/.local/bin/tokscale-client run",
        &format!("{env}ExecStart=:{executable} run"),
    ))
}

pub(super) fn install(paths: &Paths, environment: &BTreeMap<String, String>) -> Result<(), String> {
    let path = unit_path(paths)?;
    write_file(&path, &render(paths, environment)?)?;
    run("systemctl", &["--user", "daemon-reload"])?;
    run("systemctl", &["--user", "enable", UNIT])?;
    run("systemctl", &["--user", "restart", UNIT])?;
    match output("loginctl", &["--no-ask-password", "enable-linger"]) {
        Ok(result) if result.status.success() => println!("Enabled startup at boot, including while logged out."),
        _ => eprintln!("Service is running, but lingering could not be enabled. For startup without login, run: sudo loginctl enable-linger \"$USER\""),
    }
    println!("Service file: {}", path.display());
    Ok(())
}

pub(super) fn manage(paths: &Paths, action: &str) -> Result<(), String> {
    match action {
        "uninstall" => {
            let path = unit_path(paths)?;
            if !path.exists() {
                println!("Service is not installed.");
                return Ok(());
            }
            run("systemctl", &["--user", "disable", "--now", UNIT])?;
            fs::remove_file(&path).map_err(|e| e.to_string())?;
            run("systemctl", &["--user", "daemon-reload"])?;
            println!("Service removed. Executable and configuration retained.");
            Ok(())
        }
        "status" => run("systemctl", &["--user", "--no-pager", "status", UNIT]),
        "logs" => run(
            "journalctl",
            &["--user", "--unit", UNIT, "--lines", "100", "--follow"],
        ),
        _ => run("systemctl", &["--user", action, UNIT]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_paths_and_environment_without_shell_or_specifier_expansion() {
        let paths = Paths {
            home: "/home/测试 space".into(),
            binary: "/home/测试 space/%user/$bin\"/tokscale-client".into(),
            config: "/config".into(),
        };
        let environment = BTreeMap::from([(
            "TOKSCALE_HOME".into(),
            "/logs/%h/$HOME/quoted\"\nnext".into(),
        )]);
        let unit = render(&paths, &environment).unwrap();
        assert!(
            unit.contains("ExecStart=:\"/home/测试 space/%%user/$bin\\\"/tokscale-client\" run")
        );
        assert!(unit.contains("Environment=\"TOKSCALE_HOME=/logs/%%h/$HOME/quoted\\\"\\nnext\""));
        assert!(unit.contains("RestartPreventExitStatus=2"));
        assert!(!unit.contains("SYNC_TOKEN"));
    }
}
