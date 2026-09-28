use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[cfg(target_os = "linux")]
#[path = "systemd.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "launchd.rs"]
mod platform;

pub(super) struct Paths {
    home: PathBuf,
    binary: PathBuf,
    config: PathBuf,
}

pub(super) fn execute(action: &str) -> Result<(), String> {
    let home =
        tokscale_core::paths::home_dir().ok_or("cannot determine the user's home directory")?;
    let home = absolute(&home)?;
    let paths = Paths {
        binary: home.join(".local/bin/tokscale-client"),
        config: absolute(&tokscale_core::paths::get_config_dir())?,
        home,
    };
    if action == "install" {
        if crate::config::load_connection(&paths.config.join("device.json"))?.is_none() {
            return Err(
                "no saved Worker connection; run `tokscale-client connect` first".to_string(),
            );
        }
        let environment = service_environment(&paths)?;
        platform::preflight(&paths)?;
        install_binary(
            &std::env::current_exe().map_err(|e| e.to_string())?,
            &paths.binary,
        )?;
        platform::install(&paths, &environment)?;
        println!(
            "Service installed and started. Configuration: {}",
            paths.config.display()
        );
        println!(
            "Use `tokscale-client service status` or `tokscale-client service logs` to inspect it."
        );
        Ok(())
    } else {
        platform::manage(&paths, action)
    }
}

fn service_environment(paths: &Paths) -> Result<BTreeMap<String, String>, String> {
    let mut env = BTreeMap::from([
        (
            "TOKSCALE_CONFIG_DIR".into(),
            path_text(&paths.config)?.into(),
        ),
        ("HOME".into(), path_text(&paths.home)?.into()),
    ]);
    // Capture collector options, not the invoking shell's entire environment.
    // SYNC_URL and SYNC_TOKEN continue to come from device.json after rotation.
    for key in [
        "BIND_ADDR",
        "REFRESH_INTERVAL_SECS",
        "TOKSCALE_CLIENTS",
        "TOKSCALE_PRICING",
        "TOKSCALE_USE_ENV_ROOTS",
        "TOKSCALE_DEVICE_ID",
        "TOKSCALE_DEVICE_NAME",
        "TOKSCALE_API_TOKEN",
        "RUST_LOG",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
    ] {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.into(), value);
        }
    }
    for key in [
        "TOKSCALE_HOME",
        "TOKSCALE_QODER_COEFFS",
        "XDG_CONFIG_HOME",
        "QODER_DB_PATH",
        "QODER_CN_DB_PATH",
        "QODER_HOME",
        "QODER_CN_HOME",
        "QODER_PROJECTS_DIR",
        "QODER_CN_PROJECTS_DIR",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
    ] {
        if let Some(value) = std::env::var_os(key).filter(|value| !value.is_empty()) {
            env.insert(key.into(), path_text(&absolute(Path::new(&value))?)?.into());
        }
    }
    Ok(env)
}

fn absolute(path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path).map_err(|e| format!("cannot resolve {}: {e}", path.display()))
}

fn path_text(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| "service paths must be valid UTF-8".to_string())
}

fn install_binary(source: &Path, destination: &Path) -> Result<(), String> {
    if let (Ok(source), Ok(destination)) = (fs::canonicalize(source), fs::canonicalize(destination))
    {
        if source == destination {
            return Ok(());
        }
    }
    let parent = destination.parent().ok_or("binary path has no parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temporary = parent.join(format!(".tokscale-client-{}", uuid::Uuid::new_v4()));
    let result = (|| -> std::io::Result<()> {
        let mut target = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&temporary)?;
        std::io::copy(&mut fs::File::open(source)?, &mut target)?;
        target.set_permissions(fs::Permissions::from_mode(0o755))?;
        target.sync_all()?;
        drop(target);
        fs::rename(&temporary, destination)
    })();
    let _ = fs::remove_file(&temporary);
    result.map_err(|e| format!("cannot install {}: {e}", destination.display()))
}

fn write_file(path: &Path, contents: &str) -> Result<(), String> {
    let parent = path.parent().ok_or("service path has no parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temporary = parent.join(format!(".tokscale-service-{}", uuid::Uuid::new_v4()));
    let result = (|| -> std::io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    let _ = fs::remove_file(&temporary);
    result.map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} {} failed ({status})", args.join(" ")))
    }
}

fn output(program: &str, args: &[&str]) -> Result<Output, String> {
    Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run {program}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_binary_atomically_and_can_install_itself() {
        let dir = crate::config::tests::TestDirectory::new();
        let source = dir.0.join("下载 binary");
        let destination = dir.0.join("bin/tokscale-client");
        fs::write(&source, b"version one").unwrap();
        install_binary(&source, &destination).unwrap();
        let mut running = fs::File::open(&destination).unwrap();
        fs::write(&source, b"version two").unwrap();
        install_binary(&source, &destination).unwrap();
        let mut old = String::new();
        std::io::Read::read_to_string(&mut running, &mut old).unwrap();
        assert_eq!(old, "version one");
        assert_eq!(fs::read(&destination).unwrap(), b"version two");
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
            0o755
        );
        install_binary(&destination, &destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"version two");
    }
}
