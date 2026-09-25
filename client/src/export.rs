//! Export payload shaped after tokscale's own submit protocol
//! (`crates/tokscale-cli/src/main.rs` Ts* structs): per-day rows with
//! per-(client, model) contributions, camelCase on the wire. The future
//! Cloudflare Worker ingest consumes this shape verbatim.

use crate::device::DeviceInfo;
use serde::Serialize;
use tokscale_core::{DailyContribution, DataSummary, TokenBreakdown};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TsTokenBreakdown {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub reasoning: i64,
}

impl From<&TokenBreakdown> for TsTokenBreakdown {
    fn from(t: &TokenBreakdown) -> Self {
        Self {
            input: t.input,
            output: t.output,
            cache_read: t.cache_read,
            cache_write: t.cache_write,
            reasoning: t.reasoning,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TsSourceContribution {
    pub client: String,
    pub model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    pub tokens: TsTokenBreakdown,
    pub cost: f64,
    pub messages: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TsDailyTotals {
    pub tokens: i64,
    pub cost: f64,
    pub messages: i32,
    /// Absent = complete; `Some(false)` marks the day's cost as a floor
    /// (e.g. the client scanned without a pricing dataset).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_is_complete: Option<bool>,
    /// Qoder plan credits consumed that day. Not USD — kept separate from
    /// `cost` so priced clients never mix units. Absent when zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credits: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TsDailyContribution {
    pub date: String,
    pub totals: TsDailyTotals,
    pub intensity: u8,
    pub token_breakdown: TsTokenBreakdown,
    pub clients: Vec<TsSourceContribution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_time_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TsDateRange {
    pub start: String,
    pub end: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TsExportMeta {
    pub generated_at: String,
    pub version: String,
    pub date_range: TsDateRange,
}

#[derive(Debug, Serialize)]
pub struct TsExport {
    pub meta: TsExportMeta,
    pub device: DeviceInfo,
    pub summary: DataSummary,
    pub contributions: Vec<TsDailyContribution>,
}

pub fn to_ts_daily(
    day: &DailyContribution,
    cost_complete: bool,
    credits: Option<f64>,
) -> TsDailyContribution {
    TsDailyContribution {
        date: day.date.clone(),
        totals: TsDailyTotals {
            tokens: day.totals.tokens,
            cost: day.totals.cost,
            messages: day.totals.messages,
            cost_is_complete: if cost_complete { None } else { Some(false) },
            credits,
        },
        intensity: day.intensity,
        token_breakdown: TsTokenBreakdown::from(&day.token_breakdown),
        clients: day
            .clients
            .iter()
            .map(|c| TsSourceContribution {
                client: c.client.clone(),
                model_id: c.model_id.clone(),
                provider_id: (!c.provider_id.is_empty()).then(|| c.provider_id.clone()),
                tokens: TsTokenBreakdown::from(&c.tokens),
                cost: c.cost,
                messages: c.messages,
            })
            .collect(),
        active_time_ms: day.active_time_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokscale_core::{ClientContribution, DailyTotals};

    fn fixture_day() -> DailyContribution {
        DailyContribution {
            date: "2026-09-01".to_string(),
            totals: DailyTotals {
                tokens: 1300,
                cost: 0.0,
                messages: 2,
            },
            intensity: 1,
            token_breakdown: TokenBreakdown {
                input: 1000,
                output: 300,
                ..Default::default()
            },
            clients: vec![ClientContribution {
                client: "claude".to_string(),
                model_id: "claude-sonnet-4-5".to_string(),
                provider_id: String::new(),
                tokens: TokenBreakdown {
                    input: 1000,
                    output: 300,
                    ..Default::default()
                },
                cost: 0.0,
                messages: 2,
            }],
            active_time_ms: None,
        }
    }

    #[test]
    fn marks_cost_incomplete_only_when_unpriced() {
        let day = fixture_day();
        assert_eq!(
            to_ts_daily(&day, false, None).totals.cost_is_complete,
            Some(false)
        );
        assert_eq!(to_ts_daily(&day, true, None).totals.cost_is_complete, None);
    }

    #[test]
    fn wire_shape_is_camel_case_and_skips_absent_fields() {
        let value = serde_json::to_value(to_ts_daily(&fixture_day(), false, None)).unwrap();
        assert_eq!(value["date"], "2026-09-01");
        assert_eq!(value["tokenBreakdown"]["cacheRead"], 0);
        assert_eq!(value["totals"]["costIsComplete"], false);
        assert!(value["clients"][0].get("providerId").is_none());
        assert!(value.get("activeTimeMs").is_none());
        assert!(value["totals"].get("credits").is_none());
        let with_credits =
            serde_json::to_value(to_ts_daily(&fixture_day(), true, Some(3.5))).unwrap();
        assert_eq!(with_credits["totals"]["credits"], 3.5);
        assert!(with_credits["totals"].get("costIsComplete").is_none());
    }
}
