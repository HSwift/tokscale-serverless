use crate::config::{Config, PricingMode};
use crate::qoder::{self, QoderCredit};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokscale_core::pricing::PricingService;
use tokscale_core::scanner::ScannerSettings;
use tokscale_core::sessions::UnifiedMessage;
use tokscale_core::{parse_local_unified_messages_with_pricing, LocalParseOptions};

pub struct Snapshot {
    pub messages: Vec<UnifiedMessage>,
    /// Qoder plan credits, reported separately from USD costs.
    pub credits: Vec<QoderCredit>,
    pub pricing_loaded: bool,
    pub scan_duration_ms: u128,
}

/// Run parsing and disk access off the async executor. Each snapshot is consumed
/// by the uploader; there is no persistent in-memory query cache.
pub async fn collect(cfg: &Config) -> Result<Snapshot, String> {
    let cfg = cfg.clone();
    let started = Instant::now();
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| format!("failed to build scan runtime: {e}"))?;
        rt.block_on(async move {
            let pricing: Option<Arc<PricingService>> = match cfg.pricing {
                PricingMode::Off => None,
                PricingMode::Cached => PricingService::load_cached_any_age().map(Arc::new),
                PricingMode::Remote => match PricingService::get_or_init().await {
                    Ok(service) => Some(service),
                    Err(e) => {
                        tracing::warn!("remote pricing unavailable, continuing unpriced: {e}");
                        None
                    }
                },
            };
            let pricing_loaded = pricing.as_ref().is_some_and(|p| p.has_pricing_data());
            let options = LocalParseOptions {
                home_dir: cfg.tokscale_home.clone(),
                use_env_roots: cfg.use_env_roots,
                clients: cfg.clients.clone(),
                since: None,
                until: None,
                year: None,
                scanner_settings: ScannerSettings::default(),
            };
            let mut messages =
                parse_local_unified_messages_with_pricing(options, pricing.as_deref()).await?;
            // qoder is not a tokscale-core client; the lane runs alongside and
            // honors the same client filter when one is configured.
            let lane_enabled = cfg
                .clients
                .as_ref()
                .is_none_or(|list| list.iter().any(|c| c == qoder::QODER_CLIENT));
            let mut credits = Vec::new();
            if lane_enabled {
                let scan = qoder::scan(
                    &lane_home(&cfg),
                    cfg.use_env_roots,
                    &crate::coeffs::CoeffTable::load(),
                );
                tracing::info!(
                    messages = scan.messages.len(),
                    credits_rows = scan.credits.len(),
                    "qoder lane scan complete"
                );
                messages.extend(scan.messages);
                credits = scan.credits;
            }
            Ok(Snapshot {
                messages,
                credits,
                pricing_loaded,
                scan_duration_ms: started.elapsed().as_millis(),
            })
        })
    })
    .await
    .map_err(|e| format!("scan task failed to join: {e}"))?
}

fn lane_home(cfg: &Config) -> PathBuf {
    if let Some(home) = &cfg.tokscale_home {
        return PathBuf::from(home);
    }
    tokscale_core::paths::home_dir().unwrap_or_else(|| PathBuf::from("."))
}
