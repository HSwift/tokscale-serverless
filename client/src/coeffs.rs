//! Per-model Qoder credit coefficients, fitted empirically by the 2026-09-22
//! calibration sweep (16 models, `qodercli -p --output-format json`):
//! `credits * 4336 = (uncached_input + output + d * cached_input) * pf`.
//!
//! Coefficients are a *fallback*: they estimate token counts only for records
//! that carry credits but no real usage (e.g. headless transcripts written
//! before `QODER_EXPOSE_TOKEN_USAGE=1`). Records with real metrics are never
//! touched.
//!
//! Defaults are embedded from `qoder-coeffs.json`; a JSON file at
//! `$TOKSCALE_QODER_COEFFS`, else `$TOKSCALE_CONFIG_DIR/qoder-coeffs.json`,
//! is merged over them when present. `auto` reflects the routing observed
//! during calibration (to gmodel/GLM-5.3); it drifts with Qoder's router.
//! `mode-24b28…` (DogFooding) is unmetered: pf 0 means "no estimate".

use std::collections::HashMap;
use std::path::PathBuf;

/// credits*4336 = billable_tokens*pf — one credit buys 4336/pf billable tokens.
const CREDIT_SCALE: f64 = 4336.0;

#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct ModelCoeff {
    pub pf: f64,
    #[serde(default)]
    #[allow(dead_code)]
    pub d: f64,
}

#[derive(Debug, Clone)]
pub struct CoeffTable {
    map: HashMap<String, ModelCoeff>,
}

const DEFAULT_JSON: &str = include_str!("../qoder-coeffs.json");

impl CoeffTable {
    pub fn load() -> Self {
        let mut map: HashMap<String, ModelCoeff> =
            serde_json::from_str(DEFAULT_JSON).unwrap_or_else(|e| {
                tracing::warn!("embedded qoder-coeffs.json invalid: {e}");
                HashMap::new()
            });
        if let Some(path) = override_path() {
            match std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|s| {
                    serde_json::from_str::<HashMap<String, ModelCoeff>>(&s)
                        .map_err(|e| e.to_string())
                }) {
                Ok(overrides) => {
                    tracing::info!(path = %path.display(), entries = overrides.len(), "merged qoder coeff overrides");
                    map.extend(overrides);
                }
                Err(e) => tracing::warn!("ignoring qoder coeff override {}: {e}", path.display()),
            }
        }
        Self { map }
    }

    /// Estimated billable tokens for a record with `credits` but no real
    /// usage. `None` when the model is unknown or unmetered (pf <= 0).
    /// The estimate cannot know the cache split, so callers put it all in
    /// uncached input.
    pub fn estimate_tokens(&self, model: &str, credits: f64) -> Option<i64> {
        if credits <= 0.0 {
            return None;
        }
        let coeff = self.map.get(model)?;
        if coeff.pf <= 0.0 {
            return None;
        }
        Some((credits * CREDIT_SCALE / coeff.pf).round() as i64)
    }
}

fn override_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TOKSCALE_QODER_COEFFS") {
        return Some(PathBuf::from(p));
    }
    let p = PathBuf::from(std::env::var_os("TOKSCALE_CONFIG_DIR")?)
        .join("qoder-coeffs.json");
    p.is_file().then_some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_from_embedded_table() {
        let table = CoeffTable::load();
        // qmodel_38max: 1 credit ≈ 4336/0.186 ≈ 23312 billable tokens.
        let est = table.estimate_tokens("qmodel_38max", 1.0).unwrap();
        assert!((est as f64 - 4336.0 / 0.186).abs() < 1.0);
    }

    #[test]
    fn unknown_or_free_models_yield_no_estimate() {
        let table = CoeffTable::load();
        assert!(table.estimate_tokens("no-such-model", 1.0).is_none());
        assert!(table
            .estimate_tokens("mode-24b28efaa85443a5bf7eac4de15190f5", 1.0)
            .is_none());
        assert!(table.estimate_tokens("qmodel_38max", 0.0).is_none());
    }
}
