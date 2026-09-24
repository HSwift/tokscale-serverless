use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const DEVICE_FILE_NAME: &str = "device.json";

/// Host/device identity attached to every export. The id/name resolution
/// deliberately mirrors tokscale-cli's `device.rs` — same file, same JSON
/// shape, same env overrides — so a machine that already runs the CLI keeps
/// one stable device id across both, and the future sync backend can key on
/// it interchangeably.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub hostname: String,
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredDevice {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    created_at: String,
}

pub(crate) fn new_identity_document() -> serde_json::Value {
    serde_json::to_value(StoredDevice {
        id: format!("dev_{}", uuid::Uuid::new_v4().simple()),
        name: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    })
    .expect("device identity is serializable")
}

pub fn resolve() -> DeviceInfo {
    let (id, name) = resolve_id_name();
    DeviceInfo {
        id,
        name,
        hostname: hostname(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
    }
}

fn resolve_id_name() -> (String, Option<String>) {
    if let Some(id) = env_opt("TOKSCALE_DEVICE_ID") {
        return (id, env_opt("TOKSCALE_DEVICE_NAME"));
    }
    let path = device_file_path();
    let name_override = env_opt("TOKSCALE_DEVICE_NAME");
    if let Ok(content) = std::fs::read_to_string(&path) {
        if let Ok(stored) = serde_json::from_str::<StoredDevice>(&content) {
            if !stored.id.trim().is_empty() {
                return (stored.id, name_override.or(stored.name));
            }
        }
    }
    let stored = StoredDevice {
        id: format!("dev_{}", uuid::Uuid::new_v4().simple()),
        name: name_override,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(&stored) {
        let _ = std::fs::write(&path, json);
    }
    (stored.id, stored.name)
}

fn device_file_path() -> PathBuf {
    tokscale_core::paths::get_config_dir().join(DEVICE_FILE_NAME)
}

fn hostname() -> String {
    env_opt("HOSTNAME")
        .or_else(|| env_opt("COMPUTERNAME"))
        .or_else(|| {
            hostname::get()
                .ok()
                .map(|name| name.to_string_lossy().trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn env_opt(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}
