use crate::device::DeviceInfo;
use crate::error::AppError;
use crate::export::{
    aggregate_hourly, to_ts_daily, TsDateRange, TsExport, TsExportMeta, TsTokenBreakdown,
};
use crate::qoder::{QoderCredit, QODER_CLIENT};
use crate::scan::{self, RefreshOutcome};
use crate::state::{AppState, Snapshot};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use tokscale_core::sessions::UnifiedMessage;
use tokscale_core::{aggregate_by_date, aggregate_by_session, calculate_summary, TokenBreakdown};

#[derive(Debug, Default, Deserialize)]
pub struct FilterQuery {
    pub clients: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
}

pub async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let snapshot = state.current_snapshot();
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "pricing": state.cfg.pricing.as_str(),
        "scanning": state.is_scanning(),
        "device": { "id": state.device.id, "hostname": state.device.hostname },
        "snapshot": snapshot.map(|s| json!({
            "scannedAt": s.scanned_at_rfc3339,
            "ageSecs": s.scanned_at.elapsed().as_secs(),
            "messages": s.messages.len(),
            "pricingLoaded": s.pricing_loaded,
        })),
    }))
}

pub async fn summary(
    State(state): State<Arc<AppState>>,
    Query(query): Query<FilterQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let snap = require_snapshot(&state)?;
    let daily = aggregate_by_date(filter_messages(&snap.messages, &query)?);
    Ok(Json(json!({
        "summary": calculate_summary(&daily),
        "credits": credits_sum(&snap.credits, &query),
        "costComplete": snap.pricing_loaded,
    })))
}

pub async fn daily(
    State(state): State<Arc<AppState>>,
    Query(query): Query<FilterQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let snap = require_snapshot(&state)?;
    let contributions = aggregate_by_date(filter_messages(&snap.messages, &query)?);
    Ok(Json(json!({
        "contributions": contributions,
        "creditsByDate": credits_by_date(&snap.credits, &query),
        "costComplete": snap.pricing_loaded,
    })))
}

pub async fn sessions(
    State(state): State<Arc<AppState>>,
    Query(query): Query<FilterQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let snap = require_snapshot(&state)?;
    let sessions = aggregate_by_session(filter_messages(&snap.messages, &query)?);
    Ok(Json(json!({ "sessions": sessions })))
}

pub async fn models(
    State(state): State<Arc<AppState>>,
    Query(query): Query<FilterQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let snap = require_snapshot(&state)?;
    let messages = filter_messages(&snap.messages, &query)?;
    Ok(Json(json!({
        "models": model_rollup(&messages),
        "costComplete": snap.pricing_loaded,
    })))
}

pub async fn clients(
    State(state): State<Arc<AppState>>,
    Query(query): Query<FilterQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let snap = require_snapshot(&state)?;
    let messages = filter_messages(&snap.messages, &query)?;
    let mut rows = client_rollup(&messages);
    let credits = credits_sum(&snap.credits, &query);
    if credits > 0.0 {
        if let Some(row) = rows.iter_mut().find(|r| r["client"] == QODER_CLIENT) {
            row["credits"] = json!(credits);
        }
    }
    Ok(Json(json!({
        "clients": rows,
        "costComplete": snap.pricing_loaded,
    })))
}

pub async fn export(
    State(state): State<Arc<AppState>>,
    Query(query): Query<FilterQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let snap = require_snapshot(&state)?;
    Ok(Json(export_payload(&snap, &state.device, &query)?))
}

/// Build the TsExport payload. Shared by the HTTP handler and the cloud sync
/// uploader (which pushes with a default, unfiltered query).
pub fn export_payload(
    snap: &Snapshot,
    device: &DeviceInfo,
    query: &FilterQuery,
) -> Result<serde_json::Value, AppError> {
    let messages = filter_messages(&snap.messages, query)?;
    let hourly = aggregate_hourly(&messages);
    let daily = aggregate_by_date(messages);
    let summary = calculate_summary(&daily);
    let credits = credits_by_date(&snap.credits, query);
    let contributions: Vec<_> = daily
        .iter()
        .map(|day| {
            to_ts_daily(
                day,
                snap.pricing_loaded,
                credits.get(day.date.as_str()).copied().filter(|c| *c > 0.0),
            )
        })
        .collect();
    let (start, end) = date_range(&contributions);
    Ok(json!(TsExport {
        meta: TsExportMeta {
            generated_at: chrono::Utc::now().to_rfc3339(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            date_range: TsDateRange { start, end },
        },
        device: device.clone(),
        summary,
        contributions,
        hourly,
    }))
}

pub async fn refresh(State(state): State<Arc<AppState>>) -> Result<Response, AppError> {
    match scan::refresh(&state).await.map_err(AppError::Core)? {
        RefreshOutcome::AlreadyScanning => {
            Ok((StatusCode::ACCEPTED, Json(json!({ "scanning": true }))).into_response())
        }
        RefreshOutcome::Completed(snap) => Ok(Json(json!({
            "scanning": false,
            "messages": snap.messages.len(),
            "scannedAt": snap.scanned_at_rfc3339,
            "durationMs": snap.scan_duration_ms,
            "pricingLoaded": snap.pricing_loaded,
        }))
        .into_response()),
    }
}

fn require_snapshot(state: &AppState) -> Result<Arc<Snapshot>, AppError> {
    state.current_snapshot().ok_or(AppError::ScanPending)
}

fn date_range(contributions: &[crate::export::TsDailyContribution]) -> (String, String) {
    let start = contributions
        .iter()
        .map(|c| c.date.as_str())
        .min()
        .unwrap_or("")
        .to_string();
    let end = contributions
        .iter()
        .map(|c| c.date.as_str())
        .max()
        .unwrap_or("")
        .to_string();
    (start, end)
}

fn validate_date(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    if parts.len() != 3 || parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2 {
        return false;
    }
    let (year, month, day) = match (
        parts[0].parse::<i64>(),
        parts[1].parse::<u32>(),
        parts[2].parse::<u32>(),
    ) {
        (Ok(y), Ok(m), Ok(d)) => (y, m, d),
        _ => return false,
    };
    if !(1..=12).contains(&month) {
        return false;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => unreachable!(),
    };
    (1..=days_in_month).contains(&day)
}

fn filter_messages(
    messages: &[UnifiedMessage],
    query: &FilterQuery,
) -> Result<Vec<UnifiedMessage>, AppError> {
    if let Some(since) = &query.since {
        if !validate_date(since) {
            return Err(AppError::BadRequest(format!(
                "invalid since date '{since}' (expected YYYY-MM-DD)"
            )));
        }
    }
    if let Some(until) = &query.until {
        if !validate_date(until) {
            return Err(AppError::BadRequest(format!(
                "invalid until date '{until}' (expected YYYY-MM-DD)"
            )));
        }
    }
    let client_filter: Option<HashSet<&str>> = query.clients.as_ref().map(|raw| {
        raw.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect()
    });
    Ok(messages
        .iter()
        .filter(|m| {
            client_filter
                .as_ref()
                .is_none_or(|set| set.contains(m.client.as_str()))
                && query
                    .since
                    .as_ref()
                    .is_none_or(|since| m.date.as_str() >= since.as_str())
                && query
                    .until
                    .as_ref()
                    .is_none_or(|until| m.date.as_str() <= until.as_str())
        })
        .cloned()
        .collect())
}

/// Credits follow the same filter semantics as messages: a `clients` filter
/// without "qoder" zeroes them; since/until compare date strings. Callers run
/// `filter_messages` first, so dates are already validated here.
fn credit_rows<'a>(credits: &'a [QoderCredit], query: &FilterQuery) -> Vec<&'a QoderCredit> {
    let qoder_included = query
        .clients
        .as_ref()
        .is_none_or(|raw| raw.split(',').map(str::trim).any(|c| c == QODER_CLIENT));
    if !qoder_included {
        return Vec::new();
    }
    credits
        .iter()
        .filter(|c| {
            query
                .since
                .as_ref()
                .is_none_or(|since| c.date.as_str() >= since.as_str())
                && query
                    .until
                    .as_ref()
                    .is_none_or(|until| c.date.as_str() <= until.as_str())
        })
        .collect()
}

fn credits_sum(credits: &[QoderCredit], query: &FilterQuery) -> f64 {
    // `Sum for f64` folds from -0.0; normalize so empty selections render 0.0.
    credit_rows(credits, query)
        .iter()
        .map(|c| c.credits)
        .sum::<f64>()
        + 0.0
}

fn credits_by_date(credits: &[QoderCredit], query: &FilterQuery) -> BTreeMap<String, f64> {
    let mut by_date: BTreeMap<String, f64> = BTreeMap::new();
    for c in credit_rows(credits, query) {
        *by_date.entry(c.date.clone()).or_insert(0.0) += c.credits;
    }
    by_date
}

fn model_rollup(messages: &[UnifiedMessage]) -> Vec<serde_json::Value> {
    #[derive(Default)]
    struct Acc {
        provider: String,
        tokens: TokenBreakdown,
        cost: f64,
        messages: i64,
    }
    let mut by_model: HashMap<&str, Acc> = HashMap::new();
    for m in messages {
        let acc = by_model.entry(m.model_id.as_str()).or_default();
        if acc.provider.is_empty() {
            acc.provider = m.provider_id.clone();
        }
        acc.tokens += &m.tokens;
        acc.cost += m.cost;
        acc.messages += i64::from(m.message_count.max(0));
    }
    let mut rows: Vec<_> = by_model.into_iter().collect();
    rows.sort_by_key(|(_, acc)| std::cmp::Reverse(acc.tokens.total()));
    rows.into_iter()
        .map(|(model, acc)| {
            json!({
                "modelId": model,
                "providerId": acc.provider,
                "tokens": TsTokenBreakdown::from(&acc.tokens),
                "totalTokens": acc.tokens.total(),
                "cost": acc.cost,
                "messages": acc.messages,
            })
        })
        .collect()
}

fn client_rollup(messages: &[UnifiedMessage]) -> Vec<serde_json::Value> {
    #[derive(Default)]
    struct Acc {
        tokens: TokenBreakdown,
        cost: f64,
        messages: i64,
        models: BTreeSet<String>,
    }
    let mut by_client: HashMap<&str, Acc> = HashMap::new();
    for m in messages {
        let acc = by_client.entry(m.client.as_str()).or_default();
        acc.tokens += &m.tokens;
        acc.cost += m.cost;
        acc.messages += i64::from(m.message_count.max(0));
        acc.models.insert(m.model_id.clone());
    }
    let mut rows: Vec<_> = by_client.into_iter().collect();
    rows.sort_by_key(|(_, acc)| std::cmp::Reverse(acc.tokens.total()));
    rows.into_iter()
        .map(|(client, acc)| {
            json!({
                "client": client,
                "tokens": TsTokenBreakdown::from(&acc.tokens),
                "totalTokens": acc.tokens.total(),
                "cost": acc.cost,
                "messages": acc.messages,
                "models": acc.models,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokscale_core::sessions::CostSource;

    fn msg(client: &str, model: &str, date: &str, input: i64, output: i64) -> UnifiedMessage {
        UnifiedMessage {
            client: client.to_string(),
            model_id: model.to_string(),
            provider_id: "test-provider".to_string(),
            session_id: "s1".to_string(),
            workspace_key: None,
            workspace_label: None,
            timestamp: 0,
            date: date.to_string(),
            tokens: TokenBreakdown {
                input,
                output,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            cost: 0.0,
            cost_source: CostSource::Unknown,
            duration_ms: None,
            message_count: 1,
            agent: None,
            dedup_key: None,
            session_title: None,
            is_turn_start: false,
            model_attribution_conflicted: false,
        }
    }

    fn query(clients: Option<&str>, since: Option<&str>, until: Option<&str>) -> FilterQuery {
        FilterQuery {
            clients: clients.map(str::to_string),
            since: since.map(str::to_string),
            until: until.map(str::to_string),
        }
    }

    #[test]
    fn validate_date_accepts_real_dates() {
        for good in ["2026-09-01", "2024-02-29", "2000-02-29", "1999-12-31"] {
            assert!(validate_date(good), "{good}");
        }
    }

    #[test]
    fn validate_date_rejects_bad_dates() {
        for bad in [
            "2026-13-99",
            "2026-02-30",
            "2023-02-29",
            "2026-9-1",
            "2026-09-1x",
            "",
            "abcd",
            "2026/09/01",
            "20260901",
        ] {
            assert!(!validate_date(bad), "{bad}");
        }
    }

    #[test]
    fn filter_applies_clients_and_date_range() {
        let messages = vec![
            msg("claude", "m1", "2026-09-01", 100, 10),
            msg("codex", "m2", "2026-09-02", 200, 20),
            msg("claude", "m1", "2026-09-03", 400, 40),
        ];
        let only_claude = filter_messages(&messages, &query(Some("claude"), None, None)).unwrap();
        assert_eq!(only_claude.len(), 2);
        let ranged = filter_messages(
            &messages,
            &query(None, Some("2026-09-02"), Some("2026-09-02")),
        )
        .unwrap();
        assert_eq!(ranged.len(), 1);
        assert_eq!(ranged[0].client, "codex");
        let both = filter_messages(
            &messages,
            &query(Some("claude,codex"), Some("2026-09-03"), None),
        )
        .unwrap();
        assert_eq!(both.len(), 1);
    }

    #[test]
    fn filter_rejects_invalid_dates() {
        let messages = vec![msg("claude", "m1", "2026-09-01", 100, 10)];
        assert!(matches!(
            filter_messages(&messages, &query(None, Some("2026-13-99"), None)),
            Err(AppError::BadRequest(_))
        ));
    }

    #[test]
    fn model_rollup_sums_and_sorts() {
        let messages = vec![
            msg("claude", "m1", "2026-09-01", 100, 10),
            msg("claude", "m1", "2026-09-02", 100, 10),
            msg("codex", "m2", "2026-09-02", 1000, 0),
        ];
        let rows = model_rollup(&messages);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["modelId"], "m2");
        assert_eq!(rows[0]["totalTokens"], 1000);
        assert_eq!(rows[1]["totalTokens"], 220);
        assert_eq!(rows[1]["messages"], 2);
    }

    #[test]
    fn credits_follow_client_and_date_filters() {
        let credits = vec![
            QoderCredit {
                date: "2026-09-01".to_string(),
                credits: 1.5,
                model_id: "k".to_string(),
                session_id: "s".to_string(),
            },
            QoderCredit {
                date: "2026-09-02".to_string(),
                credits: 2.5,
                model_id: "k".to_string(),
                session_id: "s".to_string(),
            },
        ];
        let all = credits_sum(&credits, &query(None, None, None));
        assert!((all - 4.0).abs() < 1e-9);
        let ranged = credits_sum(&credits, &query(None, Some("2026-09-02"), None));
        assert!((ranged - 2.5).abs() < 1e-9);
        let with_qoder = credits_sum(&credits, &query(Some("claude,qoder"), None, None));
        assert!((with_qoder - 4.0).abs() < 1e-9);
        let without_qoder = credits_sum(&credits, &query(Some("claude"), None, None));
        assert_eq!(without_qoder, 0.0);
        let by_date = credits_by_date(&credits, &query(None, None, None));
        assert_eq!(by_date.len(), 2);
        assert!((by_date["2026-09-01"] - 1.5).abs() < 1e-9);
    }
}
