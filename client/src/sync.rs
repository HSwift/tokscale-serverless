//! Cloud sync uploader: after every successful scan, push the full TsExport
//! payload to the Worker's /api/ingest. Ingest upserts per
//! (device, date, client, model), so resending full history is safe and needs
//! no cursor bookkeeping. Failed uploads are retried after the next scan.

use crate::config::Config;
use crate::device::DeviceInfo;
use crate::scan::Snapshot;

pub async fn push(cfg: &Config, device: &DeviceInfo, snapshot: Snapshot) -> Result<(), String> {
    let url = cfg
        .sync_url
        .as_deref()
        .ok_or_else(|| "sync disabled".to_string())?;
    let payload = crate::export::build_payload(snapshot, device);

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
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("worker replied {status}: {body}"));
    }
    tracing::info!(url = %url, response = %body, "cloud sync push complete");
    Ok(())
}
