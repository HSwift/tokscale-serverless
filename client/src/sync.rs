//! Acknowledged bucket fingerprints keep unchanged history out of D1.
//! Changed buckets contain absolute totals, making retries idempotent.

use crate::config::Config;
use crate::device::DeviceInfo;
use crate::export::TsExport;
use crate::scan::Snapshot;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const STATE_VERSION: u32 = 1;
const HEARTBEAT_SECS: i64 = 60 * 60;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Checkpoint {
    version: u32,
    scope: String,
    acknowledged_at: i64,
    fingerprints: BTreeMap<String, String>,
}

pub struct Session {
    _lock: File,
    state_path: PathBuf,
}

impl Session {
    pub async fn acquire(wait: bool) -> Result<Option<Self>, String> {
        Self::acquire_in(&tokscale_core::paths::get_config_dir(), wait).await
    }

    async fn acquire_in(directory: &Path, wait: bool) -> Result<Option<Self>, String> {
        std::fs::create_dir_all(directory)
            .map_err(|e| format!("cannot create sync directory: {e}"))?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(directory.join("sync.lock"))
            .map_err(|e| format!("cannot open sync lock: {e}"))?;
        let started = Instant::now();
        loop {
            match FileExt::try_lock_exclusive(&lock) {
                Ok(()) => {
                    return Ok(Some(Self {
                        _lock: lock,
                        state_path: directory.join("sync-state.json"),
                    }))
                }
                Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                    if !wait {
                        return Ok(None);
                    }
                    if started.elapsed() >= Duration::from_secs(60) {
                        return Err("another sync is still running; retry when it finishes".into());
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(e) => return Err(format!("cannot lock sync state: {e}")),
            }
        }
    }

    pub async fn push(
        &self,
        cfg: &Config,
        device: &DeviceInfo,
        snapshot: Snapshot,
        full: bool,
    ) -> Result<(), String> {
        let payload = crate::export::build_payload(snapshot, device);
        self.push_payload(cfg, payload, full).await
    }

    async fn push_payload(
        &self,
        cfg: &Config,
        payload: TsExport,
        force_full: bool,
    ) -> Result<(), String> {
        let url = cfg.sync_url.as_deref().ok_or("sync disabled")?;
        // Credentials are never persisted here. Changing the endpoint or device
        // starts with a complete upload, not another connection's anchor.
        let scope = fingerprint(&(url, &payload.device.id))?;
        let previous = read_checkpoint(&self.state_path, &scope)?;
        let full = force_full || previous.is_none();
        let plan = plan(
            payload,
            previous,
            scope,
            full,
            chrono::Utc::now().timestamp(),
        )?;
        let Some((payload, checkpoint)) = plan else {
            tracing::info!("sync skipped; usage unchanged");
            return Ok(());
        };
        let days = payload.contributions.len();
        let rows: usize = payload
            .contributions
            .iter()
            .map(|day| day.clients.len())
            .sum();
        let hours = payload.hourly.len();
        let client = crate::connect::http_client()?;
        let mut request = client.post(url).json(&payload);
        if let Some(token) = &cfg.sync_token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .map_err(|e| format!("send failed: {e}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("worker replied {status}; sync anchor unchanged"));
        }
        // An HTML login page or an incomplete response is not an ingest ACK.
        let ack: serde_json::Value = response
            .json()
            .await
            .map_err(|_| "worker returned an invalid acknowledgement; sync anchor unchanged")?;
        if ack.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            return Err("worker did not acknowledge the upload; sync anchor unchanged".into());
        }
        save_checkpoint(&self.state_path, &checkpoint)?;
        tracing::info!(
            full,
            days,
            rows,
            hours,
            "cloud sync push complete; sync anchor saved"
        );
        Ok(())
    }
}

fn fingerprint(value: &impl Serialize) -> Result<String, String> {
    let bytes = serde_json::to_vec(value).map_err(|e| format!("cannot fingerprint usage: {e}"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn key(kind: &str, parts: &impl Serialize) -> Result<String, String> {
    serde_json::to_string(parts)
        .map(|parts| format!("{kind}:{parts}"))
        .map_err(|e| format!("cannot encode sync key: {e}"))
}

fn changed(
    state: &mut Checkpoint,
    key: String,
    value: &impl Serialize,
    full: bool,
) -> Result<bool, String> {
    let digest = fingerprint(value)?;
    let changed = full || state.fingerprints.get(&key) != Some(&digest);
    state.fingerprints.insert(key, digest);
    Ok(changed)
}

fn plan(
    mut payload: TsExport,
    previous: Option<Checkpoint>,
    scope: String,
    full: bool,
    now: i64,
) -> Result<Option<(TsExport, Checkpoint)>, String> {
    let mut state = previous.unwrap_or_else(|| Checkpoint {
        version: STATE_VERSION,
        scope,
        acknowledged_at: 0,
        fingerprints: BTreeMap::new(),
    });
    let device_changed = changed(&mut state, "device".into(), &payload.device, full)?;
    let mut days = Vec::new();
    for mut day in payload.contributions {
        let mut rows = Vec::new();
        for row in day.clients {
            let id = key("daily", &(&day.date, &row.client, &row.model_id))?;
            // Ignore presentation metadata: only fields stored by the API
            // affect fingerprints, not intensity or export generation time.
            if changed(&mut state, id, &(&row, day.totals.cost_is_complete), full)? {
                rows.push(row);
            }
        }
        day.clients = rows;
        let credits_changed = if let Some(credits) = day.totals.credits {
            changed(&mut state, key("credits", &day.date)?, &credits, full)?
        } else {
            false
        };
        if !credits_changed {
            day.totals.credits = None;
        }
        if !day.clients.is_empty() || credits_changed {
            days.push(day);
        }
    }
    payload.contributions = days;
    let mut hours = Vec::new();
    for row in payload.hourly {
        let id = key("hourly", &(&row.date, row.hour, &row.client, &row.model_id))?;
        if changed(&mut state, id, &row.tokens, full)? {
            hours.push(row);
        }
    }
    payload.hourly = hours;
    let heartbeat =
        now < state.acknowledged_at || now.saturating_sub(state.acknowledged_at) >= HEARTBEAT_SECS;
    if !full
        && !device_changed
        && payload.contributions.is_empty()
        && payload.hourly.is_empty()
        && !heartbeat
    {
        return Ok(None);
    }
    // Rotated/deleted local logs do not instruct deletion of cloud history.
    // Keep their anchors so reappearing logs remain unchanged.
    state.acknowledged_at = now;
    Ok(Some((payload, state)))
}

fn read_checkpoint(path: &Path, scope: &str) -> Result<Option<Checkpoint>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read sync anchor: {e}")),
    };
    match serde_json::from_slice::<Checkpoint>(&bytes) {
        Ok(state) if state.version == STATE_VERSION && state.scope == scope => Ok(Some(state)),
        _ => {
            tracing::warn!(
                "sync anchor is invalid or belongs to a different connection; full upload required"
            );
            Ok(None)
        }
    }
}

fn save_checkpoint(path: &Path, checkpoint: &Checkpoint) -> Result<(), String> {
    let temporary = path.with_file_name(format!(".sync-state-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        serde_json::to_writer(&mut file, checkpoint)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|e| format!("upload acknowledged but sync anchor could not be saved; next sync will safely retry: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::TestDirectory;
    use chrono::TimeZone;
    use tokscale_core::{sessions::UnifiedMessage, TokenBreakdown};

    fn payload() -> TsExport {
        let messages = [(1, 8, "a"), (1, 9, "b"), (2, 10, "a")]
            .into_iter()
            .map(|(day, hour, model)| {
                UnifiedMessage::new(
                    "qoder",
                    model,
                    "qoder",
                    "session",
                    chrono::Local
                        .with_ymd_and_hms(2026, 9, day, hour, 0, 0)
                        .unwrap()
                        .timestamp_millis(),
                    TokenBreakdown {
                        input: 100,
                        output: 10,
                        ..TokenBreakdown::default()
                    },
                    0.0,
                )
            })
            .collect();
        crate::export::build_payload(
            Snapshot {
                messages,
                credits: vec![crate::qoder::QoderCredit {
                    date: "2026-09-01".into(),
                    credits: 1.0,
                    model_id: "a".into(),
                    session_id: "session".into(),
                }],
                pricing_loaded: false,
                scan_duration_ms: 0,
            },
            &DeviceInfo {
                id: "test-device".into(),
                name: None,
                hostname: "host".into(),
                os: "test".into(),
                arch: "test".into(),
            },
        )
    }

    fn anchor() -> Checkpoint {
        plan(payload(), None, "scope".into(), true, 1000)
            .unwrap()
            .unwrap()
            .1
    }

    #[test]
    fn presentation_and_order_changes_do_not_resend_history() {
        let mut next = payload();
        next.meta.generated_at = "different generation time".into();
        next.contributions.reverse();
        next.hourly.reverse();
        for day in &mut next.contributions {
            day.intensity = 99;
            day.clients.reverse();
        }
        assert!(plan(next, Some(anchor()), "scope".into(), false, 1001)
            .unwrap()
            .is_none());
    }

    #[test]
    fn historical_corrections_send_only_changed_absolute_buckets() {
        let mut next = payload();
        next.contributions[0].clients[0].tokens.input = 90;
        next.contributions[0].totals.credits = Some(0.5);
        next.hourly[0].tokens = 100;
        let (delta, checkpoint) = plan(next, Some(anchor()), "scope".into(), false, 1001)
            .unwrap()
            .unwrap();
        assert_eq!(delta.contributions.len(), 1);
        assert_eq!(delta.contributions[0].date, "2026-09-01");
        assert_eq!(delta.contributions[0].clients.len(), 1);
        assert_eq!(delta.contributions[0].clients[0].tokens.input, 90);
        assert_eq!(delta.contributions[0].totals.credits, Some(0.5));
        assert_eq!(delta.hourly.len(), 1);
        assert_eq!(delta.hourly[0].tokens, 100);
        assert_eq!(checkpoint.acknowledged_at, 1001);
    }

    #[test]
    fn full_sync_and_heartbeat_have_distinct_payloads() {
        let (full, _) = plan(payload(), Some(anchor()), "scope".into(), true, 1001)
            .unwrap()
            .unwrap();
        assert_eq!(full.contributions.len(), 2);
        assert_eq!(full.hourly.len(), 3);
        let (heartbeat, _) = plan(
            payload(),
            Some(anchor()),
            "scope".into(),
            false,
            1000 + HEARTBEAT_SECS,
        )
        .unwrap()
        .unwrap();
        assert!(heartbeat.contributions.is_empty());
        assert!(heartbeat.hourly.is_empty());
        let mut renamed = payload();
        renamed.device.name = Some("renamed".into());
        assert!(plan(renamed, Some(anchor()), "scope".into(), false, 1001)
            .unwrap()
            .is_some());
    }

    #[test]
    fn removed_local_logs_do_not_erase_cloud_history() {
        let mut next = payload();
        next.contributions.clear();
        next.hourly.clear();
        assert!(plan(next, Some(anchor()), "scope".into(), false, 1001)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn checkpoints_survive_restart_and_locks_exclude_another_process() {
        let dir = TestDirectory::new();
        let first = Session::acquire_in(&dir.0, false).await.unwrap().unwrap();
        assert!(Session::acquire_in(&dir.0, false).await.unwrap().is_none());
        save_checkpoint(&first.state_path, &anchor()).unwrap();
        assert!(
            read_checkpoint(&first.state_path, "other-endpoint-or-device")
                .unwrap()
                .is_none()
        );
        drop(first);
        let second = Session::acquire_in(&dir.0, true).await.unwrap().unwrap();
        let saved = read_checkpoint(&second.state_path, "scope")
            .unwrap()
            .unwrap();
        assert!(plan(payload(), Some(saved), "scope".into(), false, 1001)
            .unwrap()
            .is_none());
        // Atomic replacement must also work when the destination exists.
        save_checkpoint(&second.state_path, &anchor()).unwrap();
        std::fs::write(&second.state_path, b"truncated {").unwrap();
        assert!(read_checkpoint(&second.state_path, "scope")
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn failed_or_unacknowledged_uploads_never_advance_the_anchor() {
        use axum::{routing::post, Json, Router};
        use std::sync::{
            atomic::{AtomicU8, Ordering},
            Arc,
        };
        let mode = Arc::new(AtomicU8::new(0));
        let server_mode = mode.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api/ingest", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/api/ingest",
                    post(move || {
                        let mode = server_mode.clone();
                        async move {
                            match mode.load(Ordering::SeqCst) {
                                0 => (
                                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                                    Json(serde_json::json!({"ok":false})),
                                ),
                                value => (
                                    axum::http::StatusCode::OK,
                                    Json(serde_json::json!({"ok":value == 2})),
                                ),
                            }
                        }
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let dir = TestDirectory::new();
        let session = Session::acquire_in(&dir.0, true).await.unwrap().unwrap();
        let cfg = Config {
            tokscale_home: None,
            clients: None,
            pricing: crate::config::PricingMode::Off,
            refresh_interval_secs: 0,
            use_env_roots: false,
            sync_url: Some(url),
            sync_token: Some("test-token".into()),
        };
        for value in [0, 1] {
            mode.store(value, Ordering::SeqCst);
            assert!(session.push_payload(&cfg, payload(), false).await.is_err());
            assert!(!session.state_path.exists());
        }
        mode.store(2, Ordering::SeqCst);
        session.push_payload(&cfg, payload(), false).await.unwrap();
        let saved = std::fs::read(&session.state_path).unwrap();
        assert!(!String::from_utf8_lossy(&saved).contains("test-token"));
        mode.store(0, Ordering::SeqCst);
        // Unchanged input succeeds without contacting the failing server.
        session.push_payload(&cfg, payload(), false).await.unwrap();
        assert_eq!(std::fs::read(&session.state_path).unwrap(), saved);
        assert!(session.push_payload(&cfg, payload(), true).await.is_err());
        assert_eq!(std::fs::read(&session.state_path).unwrap(), saved);
        server.abort();
    }
}
