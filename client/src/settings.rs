//! The collector reads the upstream settings.json without rewriting it.
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tokscale_core::{clients::ClientId, scanner::ScannerSettings};

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub scanner: ScannerSettings,
    #[serde(deserialize_with = "client_filter")]
    pub default_clients: Vec<String>,
}

pub fn path() -> PathBuf {
    tokscale_core::paths::get_config_dir().join("settings.json")
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
        };
        // Avoid printing arbitrary configuration values in an error.
        serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "invalid settings in {} at line {}, column {}",
                path.display(),
                error.line(),
                error.column()
            )
        })
    }
}

fn client_filter<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<String>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|value| {
            let name = value.as_str()?.trim().to_ascii_lowercase();
            if name == "qoder" {
                Some(name)
            } else {
                ClientId::from_str(&name).map(|client| client.as_str().to_string())
            }
        })
        .collect())
}

pub fn qoder_paths(scanner: &ScannerSettings) -> &[PathBuf] {
    scanner
        .extra_scan_paths
        .get("qoder")
        .map(Vec::as_slice)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_upstream_scanner_and_ignores_presentation_options() {
        let settings: Settings = serde_json::from_value(serde_json::json!({
            "colorPalette":"blue", "minutelyTabEnabled":true,
            "defaultClients":["codex", "gemini", "qoder", "invalid", null],
            "scanner":{
                "extraScanPaths":{"codex":["one","two"],"gemini":["three"],"qoder":["four"]},
                "opencodeDbPaths":["opencode.db"],"bucketTimezone":"UTC"
            }
        }))
        .unwrap();
        assert_eq!(settings.default_clients, ["codex", "gemini", "qoder"]);
        assert_eq!(settings.scanner.extra_scan_paths["codex"].len(), 2);
        assert_eq!(
            settings.scanner.opencode_db_paths,
            [PathBuf::from("opencode.db")]
        );
        assert_eq!(settings.scanner.bucket_timezone.as_deref(), Some("UTC"));
        assert_eq!(qoder_paths(&settings.scanner), [PathBuf::from("four")]);
    }

    #[test]
    fn missing_file_keeps_defaults_and_invalid_file_is_not_silently_ignored() {
        let dir = crate::config::tests::TestDirectory::new();
        let path = dir.0.join("settings.json");
        assert!(Settings::load(&path)
            .unwrap()
            .scanner
            .extra_scan_paths
            .is_empty());
        std::fs::write(&path, r#"{"scanner":{"extraScanPaths":"private value"}}"#).unwrap();
        let error = Settings::load(&path).err().unwrap();
        assert!(error.contains("settings.json"));
        assert!(!error.contains("private value"));
    }
}
