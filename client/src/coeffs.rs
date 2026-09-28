//! User-measured Qoder prices and context-window observations.
//! Only the versioned ratio/price format is accepted. No measured data is bundled.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
pub enum Schema {
    #[default]
    #[serde(rename = "qoder-token-estimates/2")]
    V2,
}

#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
pub enum CreditsField {
    #[default]
    #[serde(rename = "credits")]
    Billed,
    #[serde(rename = "original_credits")]
    Original,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prices {
    pub fresh_input: f64,
    pub output: f64,
    pub cache_read: f64,
    #[serde(default)]
    pub credits_field: CreditsField,
}

impl Prices {
    fn valid(&self) -> bool {
        self.fresh_input.is_finite()
            && self.fresh_input > 0.0
            && self.output.is_finite()
            && self.output > 0.0
            && self.cache_read.is_finite()
            && self.cache_read >= 0.0
    }
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Windows {
    #[serde(default)]
    pub observed: Vec<u64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ModelCoeff {
    pub prices: Prices,
    #[serde(default)]
    pub windows: Windows,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Metadata {
    #[serde(default)]
    pub aliases: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct CoeffTable {
    // Required when deserializing: an old flat map must never enable estimation.
    pub schema: Schema,
    models: HashMap<String, ModelCoeff>,
    #[serde(default)]
    meta: Metadata,
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
                tracing::info!(path = %path.display(), entries = table.models.len(), schema = ?table.schema, "loaded user Qoder calibration");
                table
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "Qoder estimation disabled; expected qoder-token-estimates/2 with per-model prices");
                Self::default()
            }
        }
    }

    pub fn model(&self, model: &str) -> Option<&ModelCoeff> {
        if model.eq_ignore_ascii_case("auto")
            || self.canonical_model(model).eq_ignore_ascii_case("auto")
        {
            return None;
        }
        self.models
            .get(self.canonical_model(model))
            .filter(|m| m.prices.valid())
    }

    pub fn canonical_model<'a>(&'a self, model: &'a str) -> &'a str {
        // Routing cannot be turned into one priced model through an alias.
        if model.eq_ignore_ascii_case("auto") {
            return model;
        }
        self.meta
            .aliases
            .get(model)
            .filter(|id| self.models.contains_key(*id))
            .map(String::as_str)
            .unwrap_or(model)
    }
}

fn config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TOKSCALE_QODER_COEFFS").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let p = tokscale_core::paths::get_config_dir().join("qoder-coeffs.json");
    p.is_file().then_some(p)
}

pub(crate) fn diagnostics() -> serde_json::Value {
    let Some(path) = config_path() else {
        return serde_json::json!({"configured":false,"note":"No calibration file; reported usage and credits still work."});
    };
    let result = std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|s| {
            serde_json::from_str::<CoeffTable>(&s)
                .map_err(|_| "expected qoder-token-estimates/2".to_string())
        });
    match result {
        Ok(table) => serde_json::json!({"configured":true,"path":path,"valid":true,
            "models":table.models.len(),"usableModels":table.models.keys().filter(|m| table.model(m).is_some()).count()}),
        Err(error) => {
            serde_json::json!({"configured":true,"path":path,"valid":false,"error":error})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_versioned_prices_and_exact_aliases() {
        let table: CoeffTable = serde_json::from_str(
            r#"{
              "schema":"qoder-token-estimates/2",
              "meta":{"aliases":{"Test Model":"test"}},
              "models":{"test":{
                "prices":{"freshInput":0.001,"output":0.003,"cacheRead":0.0001,"creditsField":"original_credits"},
                "windows":{"observed":[180000]},
                "calibration":{"samples":10}
              }}
            }"#,
        )
        .unwrap();
        let model = table.model("Test Model").unwrap();
        assert_eq!(model.windows.observed, [180000]);
        assert!(matches!(model.prices.credits_field, CreditsField::Original));
        assert!(table.model("unknown").is_none());
    }

    #[test]
    fn rejects_legacy_and_unrecognized_schemas() {
        for json in [
            r#"{"test":{"tokensPerCredit":1000}}"#,
            r#"{"test":{"pf":1,"d":0}}"#,
            r#"{"schema":"qoder-token-estimates/1","models":{}}"#,
            r#"{"schema":"qoder-token-estimates/2","models":{"test":{"tokensPerCredit":1000}}}"#,
        ] {
            assert!(serde_json::from_str::<CoeffTable>(json).is_err());
        }
        let dir = crate::config::tests::TestDirectory::new();
        let path = dir.0.join("qoder-coeffs.json");
        std::fs::write(&path, r#"{"test":{"tokensPerCredit":1000}}"#).unwrap();
        assert!(CoeffTable::from_path(&path).model("test").is_none());
    }

    #[test]
    fn auto_routing_never_uses_a_price_estimate() {
        let table: CoeffTable = serde_json::from_str(
            r#"{"schema":"qoder-token-estimates/2","models":{"auto":{
              "prices":{"freshInput":1,"output":2,"cacheRead":0.1}
            }}}"#,
        )
        .unwrap();
        assert!(table.model("auto").is_none());
        assert!(table.model("Auto").is_none());
    }

    #[test]
    fn invalid_prices_cannot_enable_estimates() {
        let mut table: CoeffTable = serde_json::from_str(
            r#"{"schema":"qoder-token-estimates/2","models":{"test":{
              "prices":{"freshInput":1,"output":2,"cacheRead":0.1}
            }}}"#,
        )
        .unwrap();
        for value in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            table.models.get_mut("test").unwrap().prices.output = value;
            assert!(table.model("test").is_none());
        }
        table.models.get_mut("test").unwrap().prices.output = 2.0;
        table.models.get_mut("test").unwrap().prices.cache_read = -0.1;
        assert!(table.model("test").is_none());
        table.models.get_mut("test").unwrap().prices.cache_read = 0.0;
        assert!(table.model("test").is_some());
    }
}
