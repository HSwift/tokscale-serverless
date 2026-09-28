//! Exercise service installation without touching the host's service manager.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::{Command, Output, Stdio};

    struct Workspace(PathBuf);

    impl Workspace {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "tokscale service 测试 & space {}",
                uuid::Uuid::new_v4()
            ));
            let mocks = root.join("mocks");
            fs::create_dir_all(&mocks).unwrap();
            for name in [
                "systemctl",
                "loginctl",
                "journalctl",
                "launchctl",
                "id",
                "tail",
            ] {
                let path = mocks.join(name);
                fs::write(
                    &path,
                    r#"#!/bin/sh
name=${0##*/}
printf '%s' "$name" >> "$MOCK_LOG"
printf ' <%s>' "$@" >> "$MOCK_LOG"
printf '\n' >> "$MOCK_LOG"
if [ "$name $*" = "$MOCK_FAIL" ]; then exit 1; fi
case "$name" in
  id) printf '501\n' ;;
  launchctl)
    case "$1" in
      print)
        if [ "$2" != gui/501 ]; then test -f "$MOCK_STATE"; exit $?; fi ;;
      bootstrap) : > "$MOCK_STATE" ;;
      bootout) /bin/rm -f "$MOCK_STATE" ;;
    esac ;;
esac
exit 0
"#,
                )
                .unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
            Self(root)
        }

        fn command(&self, action: &str) -> Command {
            let mut command = Command::new(env!("CARGO_BIN_EXE_tokscale-client"));
            command
                .env_clear()
                .current_dir(&self.0)
                .env("HOME", &self.0)
                .env("PATH", self.0.join("mocks"))
                .env("TOKSCALE_CONFIG_DIR", "设置 config")
                .env("XDG_CONFIG_HOME", self.0.join("xdg config"))
                .env("TOKSCALE_HOME", "usage data")
                .env("TOKSCALE_QODER_COEFFS", "设置 config/qoder-coeffs.json")
                .env("REFRESH_INTERVAL_SECS", "120")
                .env("MOCK_LOG", self.0.join("commands.log"))
                .env("MOCK_STATE", self.0.join("loaded"))
                .args(["service", action])
                .stdin(Stdio::null());
            command
        }

        fn saved_connection(&self) -> PathBuf {
            self.0.join("设置 config/device.json")
        }

        fn connect(&self) {
            fs::create_dir_all(self.saved_connection().parent().unwrap()).unwrap();
            fs::write(self.saved_connection(), r#"{"id":"test-device","syncUrl":"https://worker.example/api/ingest","syncToken":"saved-test-token","refreshIntervalSecs":60}"#).unwrap();
        }

        fn binary(&self) -> PathBuf {
            self.0.join(".local/bin/tokscale-client")
        }

        fn service_file(&self) -> PathBuf {
            #[cfg(target_os = "linux")]
            let path = "xdg config/systemd/user/tokscale-client.service";
            #[cfg(target_os = "macos")]
            let path = "Library/LaunchAgents/io.tokscale.collector.plist";
            self.0.join(path)
        }

        fn log(&self) -> String {
            fs::read_to_string(self.0.join("commands.log")).unwrap_or_default()
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn success(output: Output) -> String {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn install_update_manage_and_uninstall_preserve_saved_configuration() {
        let workspace = Workspace::new();
        workspace.connect();
        let original = fs::read(workspace.saved_connection()).unwrap();
        success(
            workspace
                .command("install")
                .env("SYNC_TOKEN", "temporary-token-should-not-persist")
                .env("SYNC_URL", "https://temporary.example")
                .output()
                .unwrap(),
        );
        let service = fs::read_to_string(workspace.service_file()).unwrap();
        assert!(service.contains("TOKSCALE_CONFIG_DIR"));
        assert!(service.contains("设置 config"));
        assert!(service.contains("usage data"));
        assert!(service.contains("TOKSCALE_QODER_COEFFS"));
        assert!(service.contains("120"));
        for value in [
            "SYNC_TOKEN",
            "SYNC_URL",
            "saved-test-token",
            "temporary-token",
            "temporary.example",
        ] {
            assert!(!service.contains(value));
        }
        assert_eq!(
            fs::metadata(workspace.binary())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(workspace.service_file())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        #[cfg(target_os = "linux")]
        {
            assert!(service.contains(&format!(
                "ExecStart=:\"{}\" run",
                workspace.binary().display()
            )));
            assert!(workspace
                .log()
                .contains("systemctl <--user> <enable> <tokscale-client.service>"));
            assert!(workspace
                .log()
                .contains("loginctl <--no-ask-password> <enable-linger>"));
        }
        #[cfg(target_os = "macos")]
        {
            success(
                Command::new("/usr/bin/plutil")
                    .arg("-lint")
                    .arg(workspace.service_file())
                    .output()
                    .unwrap(),
            );
            assert!(workspace.log().contains(&format!(
                "launchctl <bootstrap> <gui/501> <{}>",
                workspace.service_file().display()
            )));
        }

        // Reinstalling an updated download replaces the executable and restarts.
        fs::write(workspace.binary(), b"old version").unwrap();
        success(workspace.command("install").output().unwrap());
        assert_eq!(
            fs::metadata(workspace.binary()).unwrap().len(),
            fs::metadata(env!("CARGO_BIN_EXE_tokscale-client"))
                .unwrap()
                .len()
        );
        // Also allow installing from the already installed executable.
        let self_install = workspace.command("install");
        let mut installed = Command::new(workspace.binary());
        installed
            .env_clear()
            .envs(
                self_install
                    .get_envs()
                    .filter_map(|(key, value)| value.map(|value| (key, value))),
            )
            .current_dir(&workspace.0)
            .args(["service", "install"])
            .stdin(Stdio::null());
        success(installed.output().unwrap());

        for action in ["status", "restart", "stop", "start", "logs"] {
            success(workspace.command(action).output().unwrap());
        }
        #[cfg(target_os = "linux")]
        assert!(workspace.log().contains(
            "journalctl <--user> <--unit> <tokscale-client.service> <--lines> <100> <--follow>"
        ));
        #[cfg(target_os = "macos")]
        {
            assert!(workspace
                .log()
                .contains("launchctl <kickstart> <-k> <gui/501/io.tokscale.collector>"));
            assert!(workspace
                .log()
                .contains("launchctl <bootout> <gui/501/io.tokscale.collector>"));
        }
        success(workspace.command("uninstall").output().unwrap());
        success(workspace.command("uninstall").output().unwrap());
        assert!(!workspace.service_file().exists());
        assert!(workspace.binary().is_file());
        assert_eq!(fs::read(workspace.saved_connection()).unwrap(), original);
    }

    #[test]
    fn missing_connection_and_unavailable_manager_do_not_install_files() {
        let workspace = Workspace::new();
        let output = workspace.command("install").output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("connect"));
        assert!(workspace.log().is_empty());
        workspace.connect();
        #[cfg(target_os = "linux")]
        let failure = "systemctl --user show-environment";
        #[cfg(target_os = "macos")]
        let failure = "launchctl print gui/501";
        let output = workspace
            .command("install")
            .env("MOCK_FAIL", failure)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(!workspace.binary().exists());
        assert!(!workspace.service_file().exists());
    }

    #[test]
    fn native_start_failure_is_reported() {
        let workspace = Workspace::new();
        workspace.connect();
        #[cfg(target_os = "linux")]
        let failure = "systemctl --user restart tokscale-client.service".to_string();
        #[cfg(target_os = "macos")]
        let failure = format!(
            "launchctl bootstrap gui/501 {}",
            workspace.service_file().display()
        );
        let output = workspace
            .command("install")
            .env("MOCK_FAIL", failure)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("failed"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("Service installed and started"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linger_permission_failure_keeps_the_service_and_explains_manual_setup() {
        let workspace = Workspace::new();
        workspace.connect();
        let output = workspace
            .command("install")
            .env("MOCK_FAIL", "loginctl --no-ask-password enable-linger")
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&output.stderr).contains("sudo loginctl enable-linger"));
        success(output);
        assert!(workspace.service_file().is_file());
    }
}

#[test]
fn service_help_and_unknown_command_work_without_loading_configuration() {
    for action in ["--help", "invalid-action"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_tokscale-client"))
            .args(["service", action])
            .env("TOKSCALE_PRICING", "invalid")
            .output()
            .unwrap();
        assert_eq!(output.status.success(), action == "--help");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("TOKSCALE_PRICING"));
    }
}

#[cfg(target_os = "windows")]
#[test]
fn windows_reports_unsupported_service_management() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_tokscale-client"))
        .args(["service", "install"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Task Scheduler"));
}
