use super::*;

const LABEL: &str = "io.tokscale.collector";
const TEMPLATE: &str = include_str!("../../launchd/io.tokscale.collector.plist");

fn plist_path(paths: &Paths) -> PathBuf {
    paths
        .home
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

fn log_path(paths: &Paths) -> PathBuf {
    paths.home.join("Library/Logs/tokscale-client.log")
}

fn domain() -> Result<String, String> {
    let result = output("id", &["-u"])?;
    let uid = String::from_utf8_lossy(&result.stdout).trim().to_string();
    if !result.status.success() || uid.is_empty() || !uid.bytes().all(|b| b.is_ascii_digit()) {
        return Err("cannot determine the current user ID".into());
    }
    Ok(format!("gui/{uid}"))
}

fn loaded(target: &str) -> Result<bool, String> {
    Ok(output("launchctl", &["print", target])?.status.success())
}

pub(super) fn preflight(_paths: &Paths) -> Result<(), String> {
    if loaded(&domain()?)? {
        Ok(())
    } else {
        Err("no macOS login session found; install the LaunchAgent from a logged-in user's terminal".into())
    }
}

fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
        .replace('\r', "&#13;")
}

fn render(paths: &Paths, environment: &BTreeMap<String, String>) -> Result<String, String> {
    let env = environment
        .iter()
        .map(|(key, value)| format!("<key>{}</key><string>{}</string>\n", xml(key), xml(value)))
        .collect::<String>();
    Ok(TEMPLATE
        .replace("{{EXECUTABLE}}", &xml(path_text(&paths.binary)?))
        .replace("{{ENVIRONMENT}}", &env)
        .replace("{{LOG_PATH}}", &xml(path_text(&log_path(paths))?)))
}

pub(super) fn install(paths: &Paths, environment: &BTreeMap<String, String>) -> Result<(), String> {
    let plist = plist_path(paths);
    let log = log_path(paths);
    fs::create_dir_all(log.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(&log)
        .map_err(|e| e.to_string())?;
    write_file(&plist, &render(paths, environment)?)?;
    let domain = domain()?;
    let target = format!("{domain}/{LABEL}");
    if loaded(&target)? {
        run("launchctl", &["bootout", &target])?;
    }
    run("launchctl", &["enable", &target])?;
    run("launchctl", &["bootstrap", &domain, path_text(&plist)?])?;
    println!(
        "Enabled automatic startup at login. Service file: {}",
        plist.display()
    );
    Ok(())
}

pub(super) fn manage(paths: &Paths, action: &str) -> Result<(), String> {
    let plist = plist_path(paths);
    let domain = domain()?;
    let target = format!("{domain}/{LABEL}");
    match action {
        "uninstall" => {
            if loaded(&target)? {
                run("launchctl", &["bootout", &target])?;
            }
            if plist.exists() {
                fs::remove_file(&plist).map_err(|e| e.to_string())?;
            }
            println!("Service removed. Executable, configuration, and logs retained.");
            Ok(())
        }
        "status" => run("launchctl", &["print", &target]),
        "logs" => run("tail", &["-n", "100", "-f", path_text(&log_path(paths))?]),
        "stop" => {
            if loaded(&target)? {
                run("launchctl", &["bootout", &target])?;
            }
            Ok(())
        }
        _ => {
            if loaded(&target)? {
                if action == "restart" {
                    run("launchctl", &["kickstart", "-k", &target])
                } else {
                    run("launchctl", &["kickstart", &target])
                }
            } else {
                if !plist.is_file() {
                    return Err(
                        "service is not installed; run `tokscale-client service install` first"
                            .into(),
                    );
                }
                run("launchctl", &["enable", &target])?;
                run("launchctl", &["bootstrap", &domain, path_text(&plist)?])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_escapes_paths_and_preserves_argument_boundaries() {
        let paths = Paths {
            home: "/Users/测试 & space".into(),
            binary: "/Users/测试 & space/\"bin\"/tokscale-client".into(),
            config: "/config".into(),
        };
        let env = BTreeMap::from([("TOKSCALE_HOME".into(), "/logs/<test>&more".into())]);
        let plist = render(&paths, &env).unwrap();
        assert!(plist
            .contains("<string>/Users/测试 &amp; space/&quot;bin&quot;/tokscale-client</string>"));
        assert!(plist.contains("<string>/logs/&lt;test&gt;&amp;more</string>"));
        assert!(plist.contains("<string>run</string>"));
        assert!(!plist.contains("{{"));
        assert!(!plist.contains("SYNC_TOKEN"));
    }
}
