//! Qoder usage from SQLite and recursive transcript JSONL, including subagents.
//! Reported tokens win. Missing usage can be estimated from context ratios and
//! user-measured prices; credits alone never become tokens. Credits stay separate
//! from USD cost. Request IDs deduplicate matching SQLite/transcript records.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use tokscale_core::sessions::{CostSource, UnifiedMessage};
use tokscale_core::TokenBreakdown;

use crate::coeffs::CoeffTable;

mod estimate;

pub const QODER_CLIENT: &str = "qoder";

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QoderCredit {
    pub date: String,
    pub credits: f64,
    pub model_id: String,
    pub session_id: String,
}

#[derive(Debug, Default)]
pub struct QoderScan {
    pub messages: Vec<UnifiedMessage>,
    pub credits: Vec<QoderCredit>,
    message_indexes: HashMap<String, usize>,
    credit_indexes: HashMap<String, usize>,
}

pub fn scan(home: &Path, use_env_roots: bool, coeffs: &CoeffTable) -> QoderScan {
    let mut out = QoderScan::default();
    let mut seen: HashSet<String> = HashSet::new();
    let env = |name: &str| {
        if use_env_roots {
            std::env::var_os(name)
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
        } else {
            None
        }
    };
    for db in db_candidates(home, env) {
        if db.is_file() {
            parse_db(&db, &mut seen, &mut out);
        }
    }
    for dir in projects_candidates(home, env) {
        if dir.is_dir() {
            parse_projects(&dir, &mut seen, &mut out, coeffs);
        }
    }
    out
}

fn db_candidates(home: &Path, env: impl Fn(&str) -> Option<PathBuf>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = env("QODER_DB_PATH") {
        v.push(p);
    }
    if let Some(p) = env("QODER_CN_DB_PATH") {
        v.push(p);
    }
    // TokenTracker semantics: QODER_HOME points at the app-support root.
    for (home_key, app) in [("QODER_HOME", "Qoder"), ("QODER_CN_HOME", "QoderCN")] {
        if let Some(root) = env(home_key) {
            v.push(shared_client_cache_db(&root));
        }
        v.push(
            home.join("Library")
                .join("Application Support")
                .join(app)
                .join("SharedClientCache")
                .join("cache")
                .join("db")
                .join("local.db"),
        );
        let appdata = env("APPDATA").unwrap_or_else(|| home.join("AppData").join("Roaming"));
        v.push(shared_client_cache_db(&appdata.join(app)));
        if let Some(xdg) = env("XDG_CONFIG_HOME").filter(|path| path.is_absolute()) {
            v.push(shared_client_cache_db(&xdg.join(app)));
            v.push(
                xdg.join(app)
                    .join("qodercli")
                    .join("cache")
                    .join("db")
                    .join("local.db"),
            );
        }
        v.push(shared_client_cache_db(&home.join(".config").join(app)));
    }
    // Headless qodercli variant observed on Linux (agent-runner machines).
    v.push(
        home.join(".config")
            .join("Qoder")
            .join("qodercli")
            .join("cache")
            .join("db")
            .join("local.db"),
    );
    v
}

fn shared_client_cache_db(app_root: &Path) -> PathBuf {
    app_root
        .join("SharedClientCache")
        .join("cache")
        .join("db")
        .join("local.db")
}

fn projects_candidates(home: &Path, env: impl Fn(&str) -> Option<PathBuf>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = env("QODER_PROJECTS_DIR") {
        v.push(p);
    }
    if let Some(p) = env("QODER_CN_PROJECTS_DIR") {
        v.push(p);
    }
    v.push(home.join(".qoder").join("projects"));
    v.push(home.join(".qoder-cn").join("projects"));
    v.push(
        home.join(".agent-runner")
            .join("qoder-home")
            .join("projects"),
    );
    v
}

fn local_date(timestamp_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(timestamp_ms)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_default()
}

fn blank_message(client: &str) -> UnifiedMessage {
    UnifiedMessage {
        client: client.to_string(),
        model_id: String::new(),
        provider_id: QODER_CLIENT.to_string(),
        session_id: String::new(),
        workspace_key: None,
        workspace_label: None,
        timestamp: 0,
        date: String::new(),
        tokens: TokenBreakdown::default(),
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

// ---------- SQLite local.db (real tokens) ----------

const QODER_SQL_WITH_RECORD: &str = "
SELECT cm.id, cm.session_id, cm.token_info, cm.model_info, cm.gmt_create, cr.extra, cm.request_id
FROM chat_message cm
LEFT JOIN chat_record cr ON cr.request_id = cm.request_id
WHERE cm.role = 'assistant'
  AND cm.token_info IS NOT NULL
  AND trim(cm.token_info) NOT IN ('', '{}')
ORDER BY cm.gmt_create, cm.rowid
";

const QODER_SQL_MESSAGE_ONLY: &str = "
SELECT cm.id, cm.session_id, cm.token_info, cm.model_info, cm.gmt_create, NULL, cm.request_id
FROM chat_message cm
WHERE cm.role = 'assistant'
  AND cm.token_info IS NOT NULL
  AND trim(cm.token_info) NOT IN ('', '{}')
ORDER BY cm.gmt_create, cm.rowid
";

fn parse_db(path: &Path, seen: &mut HashSet<String>, out: &mut QoderScan) {
    let conn = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("qoder: cannot open {}: {e}", path.display());
            return;
        }
    };
    let has_chat_record: bool = conn
        .prepare("SELECT 1 FROM sqlite_master WHERE type='table' AND name='chat_record'")
        .and_then(|mut s| s.exists([]))
        .unwrap_or(false);
    let sql = if has_chat_record {
        QODER_SQL_WITH_RECORD
    } else {
        QODER_SQL_MESSAGE_ONLY
    };
    // Message-only databases may not have a request_id column. Keep reading
    // their real tokens, using the message ID for deduplication in that case.
    let prepared = conn.prepare(sql).or_else(|error| {
        if has_chat_record {
            Err(error)
        } else {
            conn.prepare(&QODER_SQL_MESSAGE_ONLY.replace("cm.request_id", "NULL"))
        }
    });
    let mut stmt = match prepared {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("qoder: query failed on {}: {e}", path.display());
            return;
        }
    };
    let rows = match stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<String>>(6)?,
        ))
    }) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("qoder: reading {} failed: {e}", path.display());
            return;
        }
    };
    for row in rows.flatten() {
        let (id, session_id, token_info, model_info, gmt_create_ms, record_extra, request_id) = row;
        let Some(tokens) = normalize_token_info(&token_info) else {
            continue;
        };
        if tokens.total() == 0 {
            continue;
        }
        let dedup = request_id
            .filter(|id| !id.is_empty())
            .map(|id| format!("request:{id}"))
            .unwrap_or_else(|| {
                format!(
                    "db:{}",
                    if id.is_empty() {
                        format!("{gmt_create_ms}")
                    } else {
                        id.clone()
                    }
                )
            });
        if !seen.insert(dedup.clone()) {
            continue;
        }
        let mut msg = blank_message(QODER_CLIENT);
        msg.model_id = model_from_db(model_info.as_deref(), record_extra.as_deref());
        msg.session_id = session_id.unwrap_or_default();
        // UnifiedMessage timestamps are milliseconds, as are Qoder's DB values.
        msg.timestamp = gmt_create_ms;
        msg.date = local_date(gmt_create_ms);
        msg.tokens = tokens;
        msg.cost_source = CostSource::ProviderReported;
        msg.dedup_key = Some(dedup.clone());
        out.message_indexes.insert(dedup, out.messages.len());
        out.messages.push(msg);
    }
}

/// Qoder's `prompt_tokens` already includes `cached_tokens`; keep cached input
/// in its own bucket and report only the remainder as ordinary input.
fn normalize_token_info(token_info: &str) -> Option<TokenBreakdown> {
    let v: serde_json::Value = serde_json::from_str(token_info).ok()?;
    let prompt = v.get("prompt_tokens")?.as_f64()?;
    let cached = v
        .get("cached_tokens")
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0);
    let completion = v.get("completion_tokens")?.as_f64()?;
    if prompt < 0.0 || cached < 0.0 || completion < 0.0 {
        return None;
    }
    let prompt = prompt as i64;
    let cached = (cached as i64).min(prompt);
    Some(TokenBreakdown {
        input: (prompt - cached).max(0),
        output: completion as i64,
        cache_read: cached,
        cache_write: 0,
        reasoning: 0,
    })
}

fn model_from_db(model_info: Option<&str>, record_extra: Option<&str>) -> String {
    let from_model_info = model_info
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| {
            v.get("model_key")
                .or_else(|| v.get("modelKey"))
                .and_then(|x| x.as_str().map(str::to_string))
        });
    let from_extra = record_extra
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| {
            v.get("modelConfig")
                .or_else(|| v.get("model_config"))
                .and_then(|c| c.get("key"))
                .and_then(|x| x.as_str().map(str::to_string))
        });
    from_model_info
        .or(from_extra)
        .unwrap_or_else(|| "unknown".to_string())
}

// ---------- Transcript JSONL (reported usage or ratio/price estimates) ----------

fn parse_projects(
    dir: &Path,
    seen: &mut HashSet<String>,
    out: &mut QoderScan,
    coeffs: &CoeffTable,
) {
    let mut files = Vec::new();
    collect_jsonl(dir, &mut files);
    files.sort();
    for file in files {
        parse_transcript(&file, seen, out, coeffs);
    }
}

fn collect_jsonl(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            files.push(path);
        }
    }
}

#[derive(Debug)]
struct TranscriptRow {
    message: UnifiedMessage,
    usage: estimate::Usage,
    agent: String,
    key: String,
}

fn parse_transcript(
    path: &Path,
    seen: &mut HashSet<String>,
    out: &mut QoderScan,
    coeffs: &CoeffTable,
) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    let mut rows: Vec<TranscriptRow> = Vec::new();
    let mut indexes: HashMap<String, usize> = HashMap::new();
    let mut reset = false;
    for (idx, line) in content.lines().enumerate() {
        let Ok(rec) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if rec.get("subtype").and_then(|v| v.as_str()) == Some("compact_boundary")
            || rec.get("isCompactSummary").and_then(|v| v.as_bool()) == Some(true)
        {
            reset = true;
        }
        if rec.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let message = rec.get("message").cloned().unwrap_or_default();
        let usage = message.get("usage").cloned().unwrap_or_default();
        let input = usage_i64(&usage, "input_tokens");
        let cache_read = usage_i64(&usage, "cache_read_input_tokens").min(input);
        let tokens = TokenBreakdown {
            input: input - cache_read,
            output: usage_i64(&usage, "output_tokens"),
            cache_read,
            cache_write: usage_i64(&usage, "cache_creation_input_tokens"),
            reasoning: 0,
        };
        let credits = usage_number(&usage, "credits");
        let original_credits = usage_number(&usage, "original_credits");
        if tokens.total() == 0
            && credits.unwrap_or(0.0) <= 0.0
            && original_credits.unwrap_or(0.0) <= 0.0
        {
            continue;
        }
        let model = message
            .get("model")
            .and_then(|v| v.as_str())
            .or_else(|| usage.get("model").and_then(|v| v.as_str()))
            .unwrap_or("unknown");
        let key = usage
            .get("request_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|id| format!("request:{id}"))
            .unwrap_or_else(|| {
                let id = message
                    .get("id")
                    .and_then(|v| v.as_str())
                    .or_else(|| rec.get("uuid").and_then(|v| v.as_str()))
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{}:{idx}", path.display()));
                format!("jsonl:{id}")
            });
        let timestamp = rec
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.timestamp_millis())
            .unwrap_or(0);
        let mut msg = blank_message(QODER_CLIENT);
        msg.model_id = coeffs.canonical_model(model).to_string();
        msg.session_id = rec
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        msg.workspace_key = rec.get("cwd").and_then(|v| v.as_str()).map(str::to_string);
        msg.timestamp = timestamp;
        msg.date = local_date(timestamp);
        msg.cost_source = if tokens.total() > 0 {
            CostSource::ProviderReported
        } else {
            CostSource::Unknown
        };
        msg.tokens = tokens.clone();
        msg.dedup_key = Some(key.clone());
        let mut row = TranscriptRow {
            message: msg,
            usage: estimate::Usage {
                tokens,
                credits,
                original_credits,
                ratio: usage_number(&usage, "context_usage_ratio"),
                reset,
            },
            agent: rec
                .get("agentId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            key: key.clone(),
        };
        reset = false;
        // Streaming fragments can contain the same ID several times. Keep real
        // usage even if another fragment has credits but zeroed token fields.
        if let Some(&index) = indexes.get(&key) {
            let old = &rows[index];
            row.usage.reset |= old.usage.reset;
            row.usage.credits = row.usage.credits.or(old.usage.credits);
            row.usage.original_credits = row.usage.original_credits.or(old.usage.original_credits);
            row.usage.ratio = row.usage.ratio.or(old.usage.ratio);
            if row.message.timestamp == 0 {
                row.message.timestamp = old.message.timestamp;
                row.message.date.clone_from(&old.message.date);
            }
            if old.usage.tokens.total() > 0 && row.usage.tokens.total() == 0 {
                row.usage.tokens = old.usage.tokens.clone();
                row.message.tokens = old.message.tokens.clone();
                row.message.cost_source = CostSource::ProviderReported;
            }
            rows[index] = row;
        } else {
            indexes.insert(key, rows.len());
            rows.push(row);
        }
    }

    // File order reflects request order even when timestamps are absent. Never
    // carry a cache chain across sessions, model switches, or subagents.
    let mut start = 0;
    while start < rows.len() {
        let mut end = start + 1;
        while end < rows.len()
            && rows[end].message.session_id == rows[start].message.session_id
            && rows[end].message.model_id == rows[start].message.model_id
            && rows[end].agent == rows[start].agent
        {
            end += 1;
        }
        if let Some(model) = coeffs.model(&rows[start].message.model_id) {
            let usage: Vec<_> = rows[start..end].iter().map(|r| r.usage.clone()).collect();
            for (row, estimated) in rows[start..end]
                .iter_mut()
                .zip(estimate::estimate(&usage, model))
            {
                if let Some(tokens) = estimated {
                    row.message.tokens = tokens;
                    row.message.cost_source = CostSource::Estimated;
                }
            }
        }
        start = end;
    }
    for row in rows {
        emit_transcript(row, seen, out);
    }
}

fn emit_transcript(row: TranscriptRow, seen: &mut HashSet<String>, out: &mut QoderScan) {
    if let Some(credits) = row.usage.credits.filter(|c| *c > 0.0) {
        let credit = QoderCredit {
            date: row.message.date.clone(),
            credits,
            model_id: row.message.model_id.clone(),
            session_id: row.message.session_id.clone(),
        };
        if let Some(&index) = out.credit_indexes.get(&row.key) {
            out.credits[index] = credit;
        } else {
            out.credit_indexes
                .insert(row.key.clone(), out.credits.len());
            out.credits.push(credit);
        }
    }
    if let Some(&index) = out.message_indexes.get(&row.key) {
        let old = &out.messages[index];
        if row.message.cost_source == CostSource::ProviderReported
            || (old.cost_source != CostSource::ProviderReported && row.message.tokens.total() > 0)
        {
            out.messages[index] = row.message;
        }
    } else if seen.insert(row.key.clone()) {
        out.message_indexes.insert(row.key, out.messages.len());
        out.messages.push(row.message);
    }
}

fn usage_number(usage: &serde_json::Value, key: &str) -> Option<f64> {
    usage
        .get(key)
        .and_then(|v| v.as_f64())
        .filter(|v| v.is_finite() && *v >= 0.0)
}

fn usage_i64(usage: &serde_json::Value, key: &str) -> i64 {
    usage.get(key).and_then(|v| v.as_i64()).unwrap_or(0).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    fn test_coeffs() -> CoeffTable {
        serde_json::from_str(
            r#"{
            "schema":"qoder-token-estimates/2",
            "models":{"test-model":{
                "prices":{"freshInput":1,"output":5,"cacheRead":0.1}
            }}
        }"#,
        )
        .unwrap()
    }

    fn assert_hourly_usage(out: &QoderScan, timestamp_ms: i64, tokens: i64) {
        let expected = chrono::DateTime::from_timestamp_millis(timestamp_ms)
            .unwrap()
            .with_timezone(&chrono::Local);
        let hourly = crate::export::aggregate_hourly(&out.messages);
        assert_eq!(hourly.len(), 1, "Qoder records must reach hourly export");
        assert_eq!(hourly[0].date, expected.format("%Y-%m-%d").to_string());
        assert_eq!(hourly[0].hour, expected.hour());
        assert_eq!(hourly[0].client, QODER_CLIENT);
        assert_eq!(hourly[0].tokens, tokens);
    }

    #[test]
    fn discovers_desktop_database_layouts_for_all_platforms() {
        let home = std::env::temp_dir().join("用户 home");
        let candidates = db_candidates(&home, |_| None);
        for root in [
            home.join("Library").join("Application Support"),
            home.join("AppData").join("Roaming"),
            home.join(".config"),
        ] {
            for app in ["Qoder", "QoderCN"] {
                assert!(candidates.contains(&shared_client_cache_db(&root.join(app))));
            }
        }
        assert!(candidates.contains(&home.join(".config/Qoder/qodercli/cache/db/local.db")));
    }

    #[test]
    fn respects_relocated_config_and_explicit_qoder_paths() {
        let root = std::env::temp_dir().join("用户 paths");
        let env = |name: &str| match name {
            "APPDATA" => Some(root.join("Roaming data")),
            "XDG_CONFIG_HOME" => Some(root.join("xdg config")),
            "QODER_HOME" => Some(root.join("custom Qoder")),
            "QODER_DB_PATH" => Some(root.join("custom.db")),
            "QODER_PROJECTS_DIR" => Some(root.join("custom projects")),
            _ => None,
        };
        let candidates = db_candidates(&root, env);
        for app_root in [
            root.join("Roaming data/Qoder"),
            root.join("xdg config/Qoder"),
            root.join("custom Qoder"),
        ] {
            assert!(candidates.contains(&shared_client_cache_db(&app_root)));
        }
        assert_eq!(candidates[0], root.join("custom.db"));
        assert_eq!(
            projects_candidates(&root, env)[0],
            root.join("custom projects")
        );
    }

    #[test]
    fn token_info_separates_cached_without_double_counting() {
        let t = normalize_token_info(
            r#"{"prompt_tokens":12000,"cached_tokens":9000,"completion_tokens":500}"#,
        )
        .unwrap();
        assert_eq!(t.input, 3000);
        assert_eq!(t.cache_read, 9000);
        assert_eq!(t.output, 500);
        assert_eq!(t.total(), 12000 + 500);
    }

    #[test]
    fn token_info_rejects_garbage() {
        assert!(normalize_token_info("not json").is_none());
        assert!(normalize_token_info(r#"{"prompt_tokens":1}"#).is_none());
        assert!(normalize_token_info(r#"{"prompt_tokens":-1,"completion_tokens":2}"#).is_none());
    }

    #[test]
    fn transcript_extracts_credits_and_dedupes_by_message_id() {
        let dir = std::env::temp_dir().join(format!("qoder-lane-test-{}", std::process::id()));
        let proj = dir.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let record = |msg_id: &str, credits: f64, ts: &str| {
            serde_json::json!({
                "type": "assistant",
                "timestamp": ts,
                "uuid": msg_id,
                "sessionId": "sess-1",
                "cwd": "/tmp/proj",
                "message": {
                    "id": msg_id,
                    "role": "assistant",
                    "model": "test-model",
                    "usage": {
                        "credits": credits,
                        "input_tokens": 0,
                        "output_tokens": 0
                    }
                }
            })
        };
        let lines = [
            record("m1", 1.5, "2026-08-30T11:00:00Z"),
            record("m2", 2.0, "2026-08-30T11:00:00.500Z"),
            serde_json::json!({"type": "user", "timestamp": "2026-08-30T10:59:00Z"}),
            record("m3-zero", 0.0, "2026-08-30T11:20:00Z"),
        ];
        let body = lines
            .iter()
            .map(|l| serde_json::to_string(l).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        // Same content in two roots simulates the hardlinked agent-runner layout.
        std::fs::write(proj.join("a.jsonl"), &body).unwrap();
        let proj2 = dir.join("proj2");
        std::fs::create_dir_all(&proj2).unwrap();
        std::fs::write(proj2.join("a-copy.jsonl"), &body).unwrap();

        let mut out = QoderScan::default();
        let mut seen = HashSet::new();
        // With no user configuration, credits survive but tokens are not invented.
        parse_projects(&dir, &mut seen, &mut out, &CoeffTable::default());
        assert_eq!(out.messages.len(), 2);
        assert_eq!(out.credits.len(), 2);
        assert!(out.messages.iter().all(|m| m.tokens.total() == 0));
        assert_eq!(out.credits.iter().map(|c| c.credits).sum::<f64>(), 3.5);
        assert_hourly_usage(&out, out.messages[0].timestamp, 0);

        let mut out = QoderScan::default();
        let mut seen = HashSet::new();
        let coeffs: CoeffTable = test_coeffs();
        parse_projects(&dir, &mut seen, &mut out, &coeffs);
        assert_eq!(
            out.messages.len(),
            2,
            "zero-credit and duplicate rows skipped"
        );
        assert_eq!(out.credits.len(), 2);
        let total: f64 = out.credits.iter().map(|c| c.credits).sum();
        assert!((total - 3.5).abs() < 1e-9);
        assert_eq!(out.messages[0].client, "qoder");
        assert_eq!(out.messages[0].model_id, "test-model");
        assert_eq!(out.messages[0].cost, 0.0, "credits must not leak into cost");
        assert!(!out.messages[0].date.is_empty());
        // Credits alone no longer manufacture token totals, even with prices.
        assert_eq!(out.messages[0].cost_source, CostSource::Unknown);
        assert!(out.messages.iter().all(|m| m.tokens.total() == 0));
        let timestamp_ms = chrono::DateTime::parse_from_rfc3339("2026-08-30T11:00:00Z")
            .unwrap()
            .timestamp_millis();
        assert_eq!(out.messages[0].timestamp, timestamp_ms);
        assert_hourly_usage(
            &out,
            timestamp_ms,
            out.messages.iter().map(|m| m.tokens.total()).sum(),
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn transcript_splits_cached_input_out_of_input_tokens() {
        let dir = std::env::temp_dir().join(format!("qoder-lane-tok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Synthetic usage: input includes cached reads; subtract them once.
        let record = serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-09-22T02:18:22.123+09:00",
            "sessionId": "sess-tok",
            "message": {
                "id": "m-tok",
                "role": "assistant",
                "model": "test-model",
                "usage": {
                    "input_tokens": 10000,
                    "cache_read_input_tokens": 8000,
                    "cache_creation_input_tokens": 0,
                    "output_tokens": 100,
                    "credits": 0.5
                }
            }
        });
        std::fs::write(dir.join("s.jsonl"), serde_json::to_string(&record).unwrap()).unwrap();
        let mut out = QoderScan::default();
        let mut seen = HashSet::new();
        let coeffs: CoeffTable = test_coeffs();
        parse_projects(&dir, &mut seen, &mut out, &coeffs);
        assert_eq!(out.messages.len(), 1);
        let m = &out.messages[0];
        let t = &m.tokens;
        assert_eq!(t.input, 2000, "real usage must win over credit estimation");
        assert_eq!(t.cache_read, 8000);
        assert_eq!(t.output, 100);
        assert_eq!(t.total(), 10000 + 100);
        assert_eq!(m.cost_source, CostSource::ProviderReported);
        assert_eq!(out.credits.len(), 1);
        let timestamp_ms = chrono::DateTime::parse_from_rfc3339("2026-09-21T17:18:22.123Z")
            .unwrap()
            .timestamp_millis();
        assert_eq!(m.timestamp, timestamp_ms);
        assert_hourly_usage(&out, timestamp_ms, 10000 + 100);
        let mut without_coeffs = QoderScan::default();
        parse_projects(
            &dir,
            &mut HashSet::new(),
            &mut without_coeffs,
            &CoeffTable::default(),
        );
        assert_eq!(without_coeffs.messages[0].tokens.total(), 10000 + 100);
        assert_hourly_usage(&without_coeffs, timestamp_ms, 10000 + 100);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn db_rows_become_token_messages() {
        let dir = std::env::temp_dir().join(format!("qoder-db-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("local.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE chat_message(
                    id TEXT, session_id TEXT, request_id TEXT, role TEXT,
                    token_info TEXT, model_info TEXT, gmt_create INTEGER
                );
                CREATE TABLE chat_record(request_id TEXT, extra TEXT);
                INSERT INTO chat_message VALUES
                    ('a1','s1','r1','assistant','{\"prompt_tokens\":1000,\"cached_tokens\":800,\"completion_tokens\":50}','{\"model_key\":\"test-model\"}',1784681696263),
                    ('a2','s1','r1','user',NULL,NULL,1784681697000),
                    ('a3','s1','r2','assistant','{}',NULL,1784681698000);",
            )
            .unwrap();
        }
        let mut out = QoderScan::default();
        let mut seen = HashSet::new();
        parse_db(&db_path, &mut seen, &mut out);
        assert_eq!(out.messages.len(), 1);
        let m = &out.messages[0];
        assert_eq!(m.client, "qoder");
        assert_eq!(m.model_id, "test-model");
        assert_eq!(m.tokens.input, 200);
        assert_eq!(m.tokens.cache_read, 800);
        assert_eq!(m.tokens.output, 50);
        assert_eq!(m.timestamp, 1784681696263);
        assert!(!m.date.is_empty());
        assert!(out.credits.is_empty());
        assert_hourly_usage(&out, 1784681696263, 1050);

        // A matching API request in JSONL contributes credits without counting
        // the SQLite tokens for a second time.
        let transcript = serde_json::json!({
            "type":"assistant", "sessionId":"s1", "timestamp":"2026-07-22T00:00:00Z",
            "message":{"id":"other-message-id", "model":"test-model", "usage":{
                "request_id":"r1", "input_tokens":1000,"output_tokens":50,
                "cache_read_input_tokens":800,"credits":1.25
            }}
        });
        std::fs::write(dir.join("request.jsonl"), transcript.to_string()).unwrap();
        parse_transcript(
            &dir.join("request.jsonl"),
            &mut seen,
            &mut out,
            &CoeffTable::default(),
        );
        assert_eq!(out.messages.len(), 1);
        assert_eq!(out.messages[0].tokens.total(), 1050);
        assert_eq!(out.credits.len(), 1);
        assert_eq!(out.credits[0].credits, 1.25);
        Connection::open(&db_path)
            .unwrap()
            .execute_batch(
                "DROP TABLE chat_record; ALTER TABLE chat_message DROP COLUMN request_id;",
            )
            .unwrap();
        let mut message_only = QoderScan::default();
        parse_db(&db_path, &mut HashSet::new(), &mut message_only);
        assert_eq!(message_only.messages.len(), 1);
        assert_eq!(message_only.messages[0].tokens.total(), 1050);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn ratio_record(id: &str, input: i64, credits: f64) -> serde_json::Value {
        serde_json::json!({
            "type":"assistant", "sessionId":"ratio-session", "timestamp":"2026-08-30T11:00:00Z",
            "message":{"id":id, "model":"test-model", "usage":{
                "input_tokens":0,"output_tokens":0,"credits":credits,
                "context_usage_ratio":input as f64 / 400000.0
            }}
        })
    }

    #[test]
    fn recursively_estimates_subagents_and_exports_hourly_usage() {
        let dir = crate::config::tests::TestDirectory::new();
        let subagents = dir.0.join("projects/main-session/subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        let records = [
            ratio_record("a", 7904, 2624.0),
            ratio_record("b", 8001, 1046.0),
        ];
        std::fs::write(
            subagents.join("agent.jsonl"),
            records
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let mut out = QoderScan::default();
        parse_projects(&dir.0, &mut HashSet::new(), &mut out, &test_coeffs());
        assert_eq!(out.messages.len(), 2);
        for (message, input) in out.messages.iter().zip([7904, 8001]) {
            assert_eq!(message.tokens.input + message.tokens.cache_read, input);
            assert_eq!(message.cost_source, CostSource::Estimated);
            assert_eq!(message.cost, 0.0);
        }
        assert_eq!(out.credits.iter().map(|c| c.credits).sum::<f64>(), 3670.0);
        assert_hourly_usage(
            &out,
            out.messages[0].timestamp,
            out.messages.iter().map(|m| m.tokens.total()).sum(),
        );
    }

    #[test]
    fn later_reported_usage_replaces_estimates_without_duplicating_credits() {
        let dir = crate::config::tests::TestDirectory::new();
        let a = ratio_record("same", 7904, 2624.0);
        let b = ratio_record("second", 8001, 1046.0);
        let mut reported = a.clone();
        reported["message"]["usage"]["input_tokens"] = 7904.into();
        reported["message"]["usage"]["cache_read_input_tokens"] = 6000.into();
        reported["message"]["usage"]["output_tokens"] = 24.into();
        std::fs::write(dir.0.join("a.jsonl"), format!("{a}\n{b}\n{reported}")).unwrap();
        std::fs::write(dir.0.join("b.jsonl"), format!("{a}\n{b}")).unwrap();
        let mut out = QoderScan::default();
        parse_projects(&dir.0, &mut HashSet::new(), &mut out, &test_coeffs());
        assert_eq!(out.messages.len(), 2);
        assert_eq!(out.credits.len(), 2);
        assert_eq!(out.messages[0].tokens.total(), 7928);
        assert_eq!(out.messages[0].cost_source, CostSource::ProviderReported);
    }

    #[test]
    fn does_not_carry_cache_between_agents() {
        let dir = crate::config::tests::TestDirectory::new();
        let a = ratio_record("a", 7904, 2624.0);
        let mut b = ratio_record("b", 8001, 1046.0);
        b["agentId"] = "different-agent".into();
        std::fs::write(dir.0.join("agents.jsonl"), format!("{a}\n{b}")).unwrap();
        let mut out = QoderScan::default();
        parse_projects(&dir.0, &mut HashSet::new(), &mut out, &test_coeffs());
        assert_eq!(
            out.messages[0].tokens.total(),
            0,
            "first window stays ambiguous"
        );
        assert_ne!(
            out.messages[1].tokens.cache_read, 7904,
            "second agent cannot inherit the first agent's prefix"
        );
    }

    #[test]
    fn auto_keeps_reported_usage_and_credits_but_never_estimates() {
        let dir = crate::config::tests::TestDirectory::new();
        let coeffs: CoeffTable = serde_json::from_str(
            r#"{"schema":"qoder-token-estimates/2",
                "meta":{"aliases":{"Auto":"test-model"}},
                "models":{"test-model":{"prices":{"freshInput":1,"output":5,"cacheRead":0.1}}}}
            "#,
        )
        .unwrap();
        let mut rows = [
            ratio_record("a", 7904, 2624.0),
            ratio_record("b", 8001, 1046.0),
            ratio_record("c", 8010, 1060.0),
        ];
        for row in &mut rows {
            row["message"]["model"] = "Auto".into();
        }
        rows[2]["message"]["usage"]["input_tokens"] = 8010.into();
        rows[2]["message"]["usage"]["cache_read_input_tokens"] = 7900.into();
        rows[2]["message"]["usage"]["output_tokens"] = 32.into();
        std::fs::write(
            dir.0.join("auto.jsonl"),
            rows.iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let mut out = QoderScan::default();
        parse_projects(&dir.0, &mut HashSet::new(), &mut out, &coeffs);
        assert_eq!(out.messages.len(), 3);
        assert_eq!(out.messages[0].tokens.total(), 0);
        assert_eq!(out.messages[1].tokens.total(), 0);
        assert_eq!(out.messages[2].tokens.total(), 8042);
        assert_eq!(out.messages[2].cost_source, CostSource::ProviderReported);
        assert!(out.messages.iter().all(|m| m.model_id == "Auto"));
        assert_eq!(out.credits.len(), 3);
    }
}
