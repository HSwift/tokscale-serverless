//! Cloud sync uploader: after every successful scan, push the full TsExport
//! payload to the Worker's /api/ingest. Ingest upserts per
//! (device, date, client, model), so resending full history is safe and needs
//! no cursor bookkeeping. Sync failures never fail the scan that triggered
//! them — local stats keep working offline.

use crate::handlers::{export_payload, FilterQuery};
use crate::state::AppState;
use std::sync::Arc;
use std::time::Duration;

pub fn spawn_push(state: Arc<AppState>) {
    tokio::spawn(async move {
        if let Err(e) = push(&state).await {
            tracing::warn!("cloud sync push failed: {e}");
        }
    });
}

async fn push(state: &Arc<AppState>) -> Result<(), String> {
    let url = state
        .cfg
        .sync_url
        .clone()
        .ok_or_else(|| "sync disabled".to_string())?;
    let snap = state
        .current_snapshot()
        .ok_or_else(|| "no snapshot yet".to_string())?;
    let payload = export_payload(&snap, &state.device, &FilterQuery::default())
        .map_err(|e| format!("failed to build export payload: {e:?}"))?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("failed to build http client: {e}"))?;
    let mut request = client.post(&url).json(&payload);
    if let Some(token) = &state.cfg.sync_token {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("send failed: {e}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("worker replied {status}: {body}"));
    }
    tracing::info!(url = %url, response = %body, "cloud sync push complete");
    Ok(())
}
