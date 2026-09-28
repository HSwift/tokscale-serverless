//! Export payload shaped after tokscale's own submit protocol
//! (`crates/tokscale-cli/src/main.rs` Ts* structs): per-day rows with
//! per-(client, model) contributions, plus hourly token totals. All fields
//! use camelCase on the wire for the Cloudflare Worker ingest endpoint.

use crate::device::DeviceInfo;
use crate::scan::Snapshot;
use chrono::{Local, Timelike};
use serde::Serialize;
use std::collections::BTreeMap;
use tokscale_core::sessions::UnifiedMessage;
use tokscale_core::{
    aggregate_by_date, calculate_summary, DailyContribution, DataSummary, TokenBreakdown,
};

/// Build the complete cloud upload without retaining raw records after the scan.
pub fn build_payload(mut snapshot: Snapshot, device: &DeviceInfo) -> TsExport {
    // Some source records carry usage with an empty model name. Preserve that
    // usage under the same identity in daily, hourly and summary aggregation,
    // including records restored from tokscale-core's parser cache.
    let mut missing_models = 0;
    for message in &mut snapshot.messages {
        if message.model_id.trim().is_empty() {
            message.model_id = "unknown".into();
            missing_models += 1;
        }
    }
    if missing_models > 0 {
        tracing::warn!(
            messages = missing_models,
            "usage has empty model IDs; exporting as unknown"
        );
    }
    let hourly = aggregate_hourly(&snapshot.messages);
    let daily = aggregate_by_date(snapshot.messages);
    let summary = calculate_summary(&daily);
    let mut credits = BTreeMap::<String, f64>::new();
    for row in snapshot.credits {
        *credits.entry(row.date).or_default() += row.credits;
    }
    let contributions: Vec<_> = daily
        .iter()
        .map(|day| {
            to_ts_daily(
                day,
                snapshot.pricing_loaded,
                credits.get(&day.date).copied().filter(|value| *value > 0.0),
            )
        })
        .collect();
    let start = contributions
        .iter()
        .map(|day| day.date.as_str())
        .min()
        .unwrap_or("")
        .to_string();
    let end = contributions
        .iter()
        .map(|day| day.date.as_str())
        .max()
        .unwrap_or("")
        .to_string();
    TsExport {
        meta: TsExportMeta {
            generated_at: chrono::Utc::now().to_rfc3339(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            date_range: TsDateRange { start, end },
        },
        device: device.clone(),
        summary,
        contributions,
        hourly,
    }
}

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
    pub hourly: Vec<TsHourlyContribution>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TsHourlyContribution {
    pub date: String,
    pub hour: u32,
    pub client: String,
    pub model_id: String,
    pub tokens: i64,
}

/// Match the API's wire checks without dropping or repairing suspect records.
/// At most 20 examples are included; indices refer to this payload's arrays.
pub fn validation_issues(payload: &TsExport) -> Vec<serde_json::Value> {
    let valid_date = |date: &str| {
        date.len() == 10
            && chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok()
            && date.as_bytes()[4] == b'-'
            && date.as_bytes()[7] == b'-'
            && date
                .bytes()
                .enumerate()
                .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
    };
    let mut issues = Vec::new();
    if payload.device.id.trim().is_empty() {
        issues.push(serde_json::json!({"path":"device.id","reason":"must be a nonempty string"}));
    }
    for (i, day) in payload.contributions.iter().enumerate() {
        if issues.len() >= 20 {
            break;
        }
        if !valid_date(&day.date) {
            issues.push(serde_json::json!({"path":format!("contributions[{i}].date"),"reason":"invalid calendar date","date":day.date}));
        }
    }
    for (i, row) in payload.hourly.iter().enumerate() {
        if issues.len() >= 20 {
            break;
        }
        let field = if !valid_date(&row.date) {
            Some("date")
        } else if row.hour > 23 {
            Some("hour")
        } else if row.client.is_empty() {
            Some("client")
        } else if row.model_id.is_empty() {
            Some("modelId")
        } else if !(0..=9_007_199_254_740_991).contains(&row.tokens) {
            Some("tokens")
        } else {
            None
        };
        if let Some(field) = field {
            issues.push(serde_json::json!({"path":format!("hourly[{i}].{field}"),"reason":"invalid hourly field","record":row}));
        }
    }
    issues
}

/// Use the same local calendar as tokscale-core's daily aggregation. Missing
/// timestamps stay out of hourly data rather than inventing a midnight spike.
pub fn aggregate_hourly(messages: &[UnifiedMessage]) -> Vec<TsHourlyContribution> {
    let mut buckets = BTreeMap::<(String, u32, String, String), i64>::new();
    for message in messages {
        if message.timestamp <= 0 {
            continue;
        }
        let Some(time) = chrono::DateTime::from_timestamp_millis(message.timestamp) else {
            continue;
        };
        let local = time.with_timezone(&Local);
        if local.format("%Y-%m-%d").to_string() != message.date {
            continue;
        }
        let key = (
            message.date.clone(),
            local.hour(),
            message.client.clone(),
            message.model_id.clone(),
        );
        let total = buckets.entry(key).or_default();
        *total = total.saturating_add(message.tokens.total());
    }
    buckets
        .into_iter()
        .map(
            |((date, hour, client, model_id), tokens)| TsHourlyContribution {
                date,
                hour,
                client,
                model_id,
                tokens,
            },
        )
        .collect()
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
    use chrono::TimeZone;
    use tokscale_core::{ClientContribution, DailyTotals};

    #[test]
    fn hourly_uses_message_local_time_and_all_token_categories() {
        let message = |day, hour, minute, client, model| {
            UnifiedMessage::new(
                client,
                model,
                "provider",
                "session",
                Local
                    .with_ymd_and_hms(2026, 9, day, hour, minute, 0)
                    .single()
                    .unwrap()
                    .timestamp_millis(),
                TokenBreakdown {
                    input: 10,
                    output: 20,
                    cache_read: 30,
                    cache_write: 40,
                    reasoning: 50,
                },
                0.0,
            )
        };
        let valid = vec![
            message(27, 23, 59, "codex", "model-a"),
            message(28, 0, 0, "codex", "model-a"),
            message(28, 0, 59, "codex", "model-a"),
            message(28, 0, 30, "codex", "model-b"),
            message(28, 0, 30, "claude", "model-b"),
        ];
        let mut messages = valid.clone();
        let mut missing = valid[0].clone();
        missing.timestamp = 0;
        messages.push(missing);
        let mut mismatched = valid[0].clone();
        mismatched.date = "2026-09-28".into();
        messages.push(mismatched);
        let rows = aggregate_hourly(&messages);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows.iter().map(|row| row.tokens).sum::<i64>(), 750);
        assert_eq!(rows[0].date, "2026-09-27");
        assert_eq!(rows[0].hour, 23);
        let midnight = rows
            .iter()
            .find(|row| row.date == "2026-09-28" && row.model_id == "model-a")
            .unwrap();
        assert_eq!((midnight.hour, midnight.tokens), (0, 300));
        let wire = serde_json::to_value(midnight).unwrap();
        assert_eq!(wire["modelId"], "model-a");
    }

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
