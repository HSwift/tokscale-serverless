//! User-measured Qoder token/credit ratios, used only when real usage is absent.
//! No coefficients are bundled. Load the user's JSON file from
//! `TOKSCALE_QODER_COEFFS` or the configuration directory's `qoder-coeffs.json`.
//! Missing configuration or an unconfigured model disables token estimation;
//! credits and provider-reported tokens are still collected.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCoeff {
    pub tokens_per_credit: f64,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(transparent)]
pub struct CoeffTable {
    map: HashMap<String, ModelCoeff>,
}

impl CoeffTable {
    pub fn load() -> Self {
        config_path()
            .map(|path| Self::from_path(&path))
            .unwrap_or_default()
    }

    fn from_path(path: &Path) -> Self {
        match std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str::<Self>(&s).map_err(|e| e.to_string()))
        {
            Ok(table) => {
                tracing::info!(path = %path.display(), entries = table.map.len(), "loaded user Qoder coefficients");
                table
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "Qoder estimation disabled; provide model entries with tokensPerCredit");
                Self::default()
            }
        }
    }

    /// Estimate tokens only with a user-supplied, positive finite ratio.
    /// The estimate cannot know the cache split, so callers put it all in
    /// uncached input.
    pub fn estimate_tokens(&self, model: &str, credits: f64) -> Option<i64> {
        if !credits.is_finite() || credits <= 0.0 {
            return None;
        }
        let coeff = self.map.get(model)?;
        if !coeff.tokens_per_credit.is_finite() || coeff.tokens_per_credit <= 0.0 {
            return None;
        }
        let tokens = (credits * coeff.tokens_per_credit).round();
        if !tokens.is_finite() || tokens >= i64::MAX as f64 {
            return None;
        }
        Some(tokens as i64)
    }
}

fn config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TOKSCALE_QODER_COEFFS").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let p = tokscale_core::paths::get_config_dir().join("qoder-coeffs.json");
    p.is_file().then_some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_only_from_user_configuration() {
        let dir = crate::config::tests::TestDirectory::new();
        let path = dir.0.join("qoder-coeffs.json");
        std::fs::write(&path, r#"{"test-model":{"tokensPerCredit":1000}}"#).unwrap();
        let table = CoeffTable::from_path(&path);
        assert_eq!(table.estimate_tokens("test-model", 1.5), Some(1500));
        assert_eq!(table.estimate_tokens("test-model", 0.0015), Some(2));
        assert_eq!(table.estimate_tokens("unconfigured-model", 1.0), None);
        assert_eq!(
            CoeffTable::default().estimate_tokens("test-model", 1.0),
            None
        );
    }

    #[test]
    fn missing_invalid_or_old_format_files_do_not_enable_estimation() {
        let dir = crate::config::tests::TestDirectory::new();
        let path = dir.0.join("qoder-coeffs.json");
        assert!(CoeffTable::from_path(&path).map.is_empty());
        for contents in ["not json", r#"{"test-model":{"pf":1,"d":0}}"#] {
            std::fs::write(&path, contents).unwrap();
            assert!(CoeffTable::from_path(&path).map.is_empty());
        }
    }

    #[test]
    fn invalid_values_cannot_produce_estimates() {
        for ratio in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::MAX] {
            let table = CoeffTable {
                map: HashMap::from([(
                    "test-model".to_string(),
                    ModelCoeff {
                        tokens_per_credit: ratio,
                    },
                )]),
            };
            assert_eq!(table.estimate_tokens("test-model", 2.0), None);
        }
        let table: CoeffTable =
            serde_json::from_str(r#"{"test-model":{"tokensPerCredit":1000}}"#).unwrap();
        for credits in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::MAX] {
            assert_eq!(table.estimate_tokens("test-model", credits), None);
        }
    }
}
