use serde::{Deserialize, Serialize};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

pub const DEFAULT_REFRESH_INTERVAL_SECS: u64 = 60;

/// Persisted alongside device identity, but never included in device exports.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedConnection {
    pub sync_url: String,
    pub sync_token: String,
    pub refresh_interval_secs: u64,
}

pub fn connection_path() -> PathBuf {
    tokscale_core::paths::get_config_dir().join("device.json")
}

pub fn load_connection(path: &Path) -> Result<Option<SavedConnection>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    // Serde's detailed errors can contain invalid field values, including a token.
    let document: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "invalid configuration in {} at line {}, column {}",
            path.display(),
            error.line(),
            error.column()
        )
    })?;
    let fields = document
        .as_object()
        .ok_or("device configuration must be a JSON object")?;
    if !["syncUrl", "syncToken", "refreshIntervalSecs"]
        .iter()
        .any(|key| fields.contains_key(*key))
    {
        return Ok(None);
    }
    let mut saved: SavedConnection = serde_json::from_value(document)
        .map_err(|_| "invalid saved connection in device.json; syncUrl, syncToken and refreshIntervalSecs are required".to_string())?;
    saved.sync_url = normalize_sync_url(&saved.sync_url)?;
    saved.sync_token =
        nonempty(saved.sync_token).ok_or_else(|| "saved syncToken cannot be empty".to_string())?;
    Ok(Some(saved))
}

/// Replace credentials atomically, with owner-only access from file creation.
pub fn save_connection(path: &Path, saved: &SavedConnection) -> Result<(), String> {
    let mut document = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes)
            .map_err(|_| "cannot update invalid device.json".to_string())?,
        Err(error) if error.kind() == ErrorKind::NotFound => crate::device::new_identity_document(),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let fields = document
        .as_object_mut()
        .ok_or("device configuration must be a JSON object")?;
    if fields
        .get("id")
        .and_then(|id| id.as_str())
        .is_none_or(|id| id.trim().is_empty())
    {
        return Err("device.json has no valid device id".to_string());
    }
    fields.insert("syncUrl".to_string(), serde_json::json!(saved.sync_url));
    fields.insert("syncToken".to_string(), serde_json::json!(saved.sync_token));
    fields.insert(
        "refreshIntervalSecs".to_string(),
        serde_json::json!(saved.refresh_interval_secs),
    );
    let parent = path.parent().ok_or("configuration path has no parent")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create configuration directory: {error}"))?;
    let temporary = parent.join(format!(".device-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        serde_json::to_writer_pretty(&mut file, &document)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|error| format!("cannot save {}: {error}", path.display()))
}

pub fn normalize_sync_url(raw: &str) -> Result<String, String> {
    let mut url = reqwest::Url::parse(raw.trim())
        .map_err(|_| "Worker URL must be an absolute http:// or https:// URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path().trim_end_matches('/'), "" | "/api/ingest")
    {
        return Err(
            "use the Worker base URL or /api/ingest URL, without credentials, query, or fragment"
                .to_string(),
        );
    }
    url.set_path("/api/ingest");
    Ok(url.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PricingMode {
    Off,
    Cached,
    Remote,
}

#[derive(Clone)]
pub struct Config {
    pub tokscale_home: Option<String>,
    pub clients: Option<Vec<String>>,
    pub pricing: PricingMode,
    pub refresh_interval_secs: u64,
    pub use_env_roots: bool,
    /// Cloud sync target (the Worker's /api/ingest URL). Required to run.
    pub sync_url: Option<String>,
    /// Shared ingest token sent as `Authorization: Bearer` on sync pushes.
    pub sync_token: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let saved = load_connection(&connection_path())?;
        Self::resolve(saved.as_ref(), |name| std::env::var(name).ok())
    }

    fn resolve(
        saved: Option<&SavedConnection>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, String> {
        let env_opt = |name: &str| env(name).and_then(nonempty);
        let pricing = match env_opt("TOKSCALE_PRICING").as_deref().unwrap_or("cached") {
            "off" => PricingMode::Off,
            "cached" => PricingMode::Cached,
            "remote" => PricingMode::Remote,
            other => {
                return Err(format!(
                    "invalid TOKSCALE_PRICING '{other}' (expected off|cached|remote)"
                ));
            }
        };
        let clients = env_opt("TOKSCALE_CLIENTS")
            .map(|raw| {
                raw.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty());

        // An explicit empty SYNC_URL disables saved sync configuration. Never
        // send a saved token to an endpoint overridden through the environment.
        let url_override = env("SYNC_URL");
        let sync_url = match &url_override {
            Some(value) => nonempty(value.clone()),
            None => saved.map(|value| value.sync_url.clone()),
        }
        .map(|value| normalize_sync_url(&value))
        .transpose()?;
        let sync_token = match env("SYNC_TOKEN") {
            Some(value) => nonempty(value),
            None if url_override.is_none() => saved.map(|value| value.sync_token.clone()),
            None => None,
        };
        if sync_url.is_some() && sync_token.is_none() {
            return Err("SYNC_TOKEN is required when SYNC_URL is set; run `tokscale-client connect` to save a connection".to_string());
        }
        let refresh_interval_secs =
            match env_opt("REFRESH_INTERVAL_SECS") {
                Some(value) => value.parse::<u64>().map_err(|_| {
                    "REFRESH_INTERVAL_SECS must be a non-negative integer".to_string()
                })?,
                None => saved.map(|value| value.refresh_interval_secs).unwrap_or(
                    if sync_url.is_some() {
                        DEFAULT_REFRESH_INTERVAL_SECS
                    } else {
                        0
                    },
                ),
            };
        Ok(Self {
            tokscale_home: env_opt("TOKSCALE_HOME"),
            clients,
            pricing,
            refresh_interval_secs,
            use_env_roots: env_opt("TOKSCALE_USE_ENV_ROOTS")
                .map(|v| v != "false" && v != "0")
                .unwrap_or(true),
            sync_url,
            sync_token,
        })
    }
}

fn nonempty(value: String) -> Option<String> {
    Some(value.trim().to_string()).filter(|v| !v.is_empty())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub struct TestDirectory(pub PathBuf);

    impl TestDirectory {
        pub fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("tokscale-config-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn saved() -> SavedConnection {
        SavedConnection {
            sync_url: "https://worker.example/api/ingest".to_string(),
            sync_token: "saved-token".to_string(),
            refresh_interval_secs: 90,
        }
    }

    fn resolve(saved: Option<&SavedConnection>, env: &[(&str, &str)]) -> Result<Config, String> {
        Config::resolve(saved, |name| {
            env.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        })
    }

    #[test]
    fn saved_credentials_and_interval_work_without_environment() {
        let saved = saved();
        let cfg = resolve(Some(&saved), &[]).unwrap();
        assert_eq!(cfg.sync_url.as_deref(), Some(saved.sync_url.as_str()));
        assert_eq!(cfg.sync_token.as_deref(), Some("saved-token"));
        assert_eq!(cfg.refresh_interval_secs, 90);
    }

    #[test]
    fn environment_overrides_saved_connection_and_interval() {
        let cfg = resolve(
            Some(&saved()),
            &[
                ("SYNC_URL", "http://localhost:18787"),
                ("SYNC_TOKEN", "replacement-token"),
                ("REFRESH_INTERVAL_SECS", "120"),
            ],
        )
        .unwrap();
        assert_eq!(
            cfg.sync_url.as_deref(),
            Some("http://localhost:18787/api/ingest")
        );
        assert_eq!(cfg.sync_token.as_deref(), Some("replacement-token"));
        assert_eq!(cfg.refresh_interval_secs, 120);
    }

    #[test]
    fn overriding_endpoint_cannot_reuse_saved_token() {
        assert!(resolve(Some(&saved()), &[("SYNC_URL", "https://another.example")]).is_err());
        assert!(resolve(Some(&saved()), &[("SYNC_TOKEN", " ")]).is_err());
        let cfg = resolve(Some(&saved()), &[("SYNC_URL", " ")]).unwrap();
        assert!(cfg.sync_url.is_none());
        assert!(cfg.sync_token.is_none());
    }

    #[test]
    fn interval_defaults_and_explicit_disable() {
        assert_eq!(resolve(None, &[]).unwrap().refresh_interval_secs, 0);
        assert_eq!(
            resolve(
                None,
                &[
                    ("SYNC_URL", "https://worker.example"),
                    ("SYNC_TOKEN", "token")
                ]
            )
            .unwrap()
            .refresh_interval_secs,
            60
        );
        assert_eq!(
            resolve(Some(&saved()), &[("REFRESH_INTERVAL_SECS", "0")])
                .unwrap()
                .refresh_interval_secs,
            0
        );
        for invalid in ["-1", "abc", "1.5"] {
            assert!(resolve(Some(&saved()), &[("REFRESH_INTERVAL_SECS", invalid)]).is_err());
        }
    }

    #[test]
    fn worker_url_validation_does_not_echo_credentials() {
        for url in [
            "https://worker.example",
            "https://worker.example/",
            "https://worker.example/api/ingest/",
        ] {
            assert_eq!(
                normalize_sync_url(url).unwrap(),
                "https://worker.example/api/ingest"
            );
        }
        for url in [
            "/api/ingest",
            "ftp://worker.example",
            "https://user:secret@worker.example",
            "https://worker.example/?token=secret",
            "https://worker.example/#secret",
            "https://worker.example/api/me",
        ] {
            let error = normalize_sync_url(url).unwrap_err();
            assert!(!error.contains("secret"));
        }
    }

    #[test]
    fn credential_file_roundtrip_and_atomic_replacement() {
        let dir = TestDirectory::new();
        let path = dir.0.join("device.json");
        assert!(load_connection(&path).unwrap().is_none());
        let identity = serde_json::json!({"id":"dev_existing", "name":"my machine", "createdAt":"2026-01-01T00:00:00Z", "customField":true});
        std::fs::write(&path, serde_json::to_vec(&identity).unwrap()).unwrap();
        assert!(load_connection(&path).unwrap().is_none());
        save_connection(&path, &saved()).unwrap();
        assert_eq!(
            load_connection(&path).unwrap().unwrap().sync_token,
            "saved-token"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let mut replacement = saved();
        replacement.sync_token = "new-token".to_string();
        save_connection(&path, &replacement).unwrap();
        assert_eq!(
            load_connection(&path).unwrap().unwrap().sync_token,
            "new-token"
        );
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for key in ["id", "name", "createdAt", "customField"] {
            assert_eq!(document[key], identity[key]);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn invalid_configuration_fails_without_echoing_values() {
        let dir = TestDirectory::new();
        let path = dir.0.join("device.json");
        std::fs::write(&path, r#"{"syncUrl":"https://worker.example","syncToken":"token","refreshIntervalSecs":"secret"}"#).unwrap();
        let error = load_connection(&path).err().unwrap();
        assert!(!error.contains("secret"));
        assert!(error.contains("invalid saved connection"));
    }
}
