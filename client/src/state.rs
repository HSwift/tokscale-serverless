use crate::config::Config;
use crate::device::DeviceInfo;
use crate::qoder::QoderCredit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;
use tokscale_core::sessions::UnifiedMessage;

pub struct Snapshot {
    pub messages: Vec<UnifiedMessage>,
    /// Qoder plan credits from transcript JSONL. Kept out of
    /// `UnifiedMessage.cost` (credits are not USD) and reported side-band.
    pub credits: Vec<QoderCredit>,
    pub scanned_at: Instant,
    pub scanned_at_rfc3339: String,
    pub scan_duration_ms: u128,
    /// Whether a pricing dataset was loaded for this scan. Drives the
    /// `costIsComplete: false` marker in exports when false.
    pub pricing_loaded: bool,
}

pub struct AppState {
    pub cfg: Config,
    pub device: DeviceInfo,
    snapshot: RwLock<Option<Arc<Snapshot>>>,
    pub scan_lock: tokio::sync::Mutex<()>,
    scanning: AtomicBool,
}

impl AppState {
    pub fn new(cfg: Config, device: DeviceInfo) -> Self {
        Self {
            cfg,
            device,
            snapshot: RwLock::new(None),
            scan_lock: tokio::sync::Mutex::new(()),
            scanning: AtomicBool::new(false),
        }
    }

    pub fn current_snapshot(&self) -> Option<Arc<Snapshot>> {
        self.snapshot.read().ok()?.clone()
    }

    pub fn store_snapshot(&self, snap: Snapshot) -> Arc<Snapshot> {
        let snap = Arc::new(snap);
        if let Ok(mut guard) = self.snapshot.write() {
            *guard = Some(Arc::clone(&snap));
        }
        snap
    }

    pub fn set_scanning(&self, value: bool) {
        self.scanning.store(value, Ordering::Relaxed);
    }

    pub fn is_scanning(&self) -> bool {
        self.scanning.load(Ordering::Relaxed)
    }
}
