//! Qoder usage lane. tokscale-core has no Qoder parser, and Qoder itself
//! records usage in two disjoint places depending on install flavour:
//!
//! - Desktop/CLI installs: SQLite `local.db` (`chat_message.token_info` JSON
//!   with `prompt_tokens`/`cached_tokens`/`completion_tokens`). This is the
//!   only source of *real token counts*; path layout follows TokenTracker's
//!   cross-platform resolution (intl `Qoder` + CN `QoderCN` editions).
//! - Headless/agent-runner installs: Claude-shaped transcript JSONL under
//!   `*/projects/**`. When real usage is absent (recorded before
//!   `QODER_EXPOSE_TOKEN_USAGE=1`), billable tokens are estimated from
//!   `usage.credits` via the fitted [`CoeffTable`] and flagged
//!   `CostSource::Estimated`; real metrics always take priority. Credits are
//!   not USD, so they are reported through [`QoderCredit`] side-band data,
//!   never through `UnifiedMessage.cost`.
//!
//! The same file can appear under several roots (on this machine
//! `~/.agent-runner/qoder-home/projects` and `~/.qoder/projects` are
//! hardlinks to one inode), so every emitted row is deduped by a stable
//! message key across all roots.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use tokscale_core::sessions::{CostSource, UnifiedMessage};
use tokscale_core::TokenBreakdown;

use crate::coeffs::CoeffTable;

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

fn local_date(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
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
SELECT cm.id, cm.session_id, cm.token_info, cm.model_info, cm.gmt_create, cr.extra
FROM chat_message cm
LEFT JOIN chat_record cr ON cr.request_id = cm.request_id
WHERE cm.role = 'assistant'
  AND cm.token_info IS NOT NULL
  AND trim(cm.token_info) NOT IN ('', '{}')
ORDER BY cm.gmt_create, cm.rowid
";

const QODER_SQL_MESSAGE_ONLY: &str = "
SELECT cm.id, cm.session_id, cm.token_info, cm.model_info, cm.gmt_create, NULL
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
    let mut stmt = match conn.prepare(sql) {
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
        ))
    }) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("qoder: reading {} failed: {e}", path.display());
            return;
        }
    };
    for row in rows.flatten() {
        let (id, session_id, token_info, model_info, gmt_create_ms, record_extra) = row;
        let Some(tokens) = normalize_token_info(&token_info) else {
            continue;
        };
        if tokens.total() == 0 {
            continue;
        }
        let dedup = format!(
            "db:{}",
            if id.is_empty() {
                format!("{gmt_create_ms}")
            } else {
                id.clone()
            }
        );
        if !seen.insert(dedup.clone()) {
            continue;
        }
        let secs = gmt_create_ms / 1000;
        let mut msg = blank_message(QODER_CLIENT);
        msg.model_id = model_from_db(model_info.as_deref(), record_extra.as_deref());
        msg.session_id = session_id.unwrap_or_default();
        msg.timestamp = secs;
        msg.date = local_date(secs);
        msg.tokens = tokens;
        msg.dedup_key = Some(dedup);
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

// ---------- Transcript JSONL (credits only; token fields are zeroed) ----------

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

fn parse_transcript(
    path: &Path,
    seen: &mut HashSet<String>,
    out: &mut QoderScan,
    coeffs: &CoeffTable,
) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    for (idx, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(rec) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if rec.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let message = rec.get("message").cloned().unwrap_or_default();
        let usage = message.get("usage").cloned().unwrap_or_default();
        let credits = usage.get("credits").and_then(|c| c.as_f64()).unwrap_or(0.0);
        // Transcripts use the same OpenAI-style semantics as the SQLite
        // token_info: input_tokens includes cache_read_input_tokens. Split
        // cached input out so TokenBreakdown::total() does not double-count.
        let cache_read = usage_i64(&usage, "cache_read_input_tokens");
        let mut tokens = TokenBreakdown {
            input: (usage_i64(&usage, "input_tokens") - cache_read).max(0),
            output: usage_i64(&usage, "output_tokens"),
            cache_read,
            cache_write: usage_i64(&usage, "cache_creation_input_tokens"),
            reasoning: 0,
        };
        if credits <= 0.0 && tokens.total() == 0 {
            continue;
        }
        let session_id = rec
            .get("sessionId")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let key = message
            .get("id")
            .and_then(|s| s.as_str())
            .or_else(|| rec.get("uuid").and_then(|s| s.as_str()))
            .map(str::to_string)
            .unwrap_or_else(|| format!("{session_id}:{idx}"));
        let dedup = format!("jsonl:{key}");
        if !seen.insert(dedup.clone()) {
            continue;
        }
        let secs = rec
            .get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.timestamp())
            .unwrap_or(0);
        let model = message
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown")
            .to_string();
        // Fallback for records that predate real usage recording: derive
        // billable tokens from credits via the fitted coefficient table.
        // The cache split is unknowable, so the estimate lands entirely in
        // uncached input and is flagged CostSource::Estimated.
        let mut cost_source = CostSource::ProviderReported;
        if tokens.total() == 0 && credits > 0.0 {
            if let Some(est) = coeffs.estimate_tokens(&model, credits) {
                tokens.input = est;
                cost_source = CostSource::Estimated;
            }
        }
        let date = local_date(secs);
        let mut msg = blank_message(QODER_CLIENT);
        msg.model_id = model.clone();
        msg.session_id = session_id.clone();
        msg.workspace_key = rec.get("cwd").and_then(|c| c.as_str()).map(str::to_string);
        msg.timestamp = secs;
        msg.date = date.clone();
        msg.tokens = tokens;
        // Credits stay out of `cost`: they are Qoder plan credits, not USD.
        msg.cost_source = cost_source;
        msg.dedup_key = Some(dedup);
        out.messages.push(msg);
        if credits > 0.0 {
            out.credits.push(QoderCredit {
                date,
                credits,
                model_id: model,
                session_id,
            });
        }
    }
}

fn usage_i64(usage: &serde_json::Value, key: &str) -> i64 {
    usage.get(key).and_then(|v| v.as_i64()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            r#"{"prompt_tokens":58299,"cached_tokens":57853,"completion_tokens":2812}"#,
        )
        .unwrap();
        assert_eq!(t.input, 446);
        assert_eq!(t.cache_read, 57853);
        assert_eq!(t.output, 2812);
        assert_eq!(t.total(), 58299 + 2812);
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
                    "model": "kmodel_latest",
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
            record("m2", 2.0, "2026-08-30T11:15:00Z"),
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
        let coeffs = CoeffTable::load();
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
        assert_eq!(out.messages[0].model_id, "kmodel_latest");
        assert_eq!(out.messages[0].cost, 0.0, "credits must not leak into cost");
        assert!(!out.messages[0].date.is_empty());
        // No real usage recorded: tokens estimated from credits and flagged.
        assert_eq!(out.messages[0].cost_source, CostSource::Estimated);
        assert_eq!(
            out.messages[0].tokens.input,
            (1.5_f64 * 4336.0 / 0.933).round() as i64
        );
        assert!(out.messages[0].tokens.total() > 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn transcript_splits_cached_input_out_of_input_tokens() {
        let dir = std::env::temp_dir().join(format!("qoder-lane-tok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Shape observed after Qoder started recording real usage (2026-09):
        // input 25106 includes cache_read 24782 → uncached input is 324.
        let record = serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-09-21T17:18:22Z",
            "sessionId": "sess-tok",
            "message": {
                "id": "m-tok",
                "role": "assistant",
                "model": "qmodel_38max",
                "usage": {
                    "input_tokens": 25106,
                    "cache_read_input_tokens": 24782,
                    "cache_creation_input_tokens": 0,
                    "output_tokens": 104,
                    "credits": 0.10949828571428571
                }
            }
        });
        std::fs::write(dir.join("s.jsonl"), serde_json::to_string(&record).unwrap()).unwrap();
        let mut out = QoderScan::default();
        let mut seen = HashSet::new();
        let coeffs = CoeffTable::load();
        parse_projects(&dir, &mut seen, &mut out, &coeffs);
        assert_eq!(out.messages.len(), 1);
        let m = &out.messages[0];
        let t = &m.tokens;
        assert_eq!(t.input, 324, "real usage must win over credit estimation");
        assert_eq!(t.cache_read, 24782);
        assert_eq!(t.output, 104);
        assert_eq!(t.total(), 25106 + 104);
        assert_eq!(m.cost_source, CostSource::ProviderReported);
        assert_eq!(out.credits.len(), 1);
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
                    ('a1','s1','r1','assistant','{\"prompt_tokens\":1000,\"cached_tokens\":800,\"completion_tokens\":50}','{\"model_key\":\"quest-ultimate\"}',1784681696263),
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
        assert_eq!(m.model_id, "quest-ultimate");
        assert_eq!(m.tokens.input, 200);
        assert_eq!(m.tokens.cache_read, 800);
        assert_eq!(m.tokens.output, 50);
        assert_eq!(m.timestamp, 1784681696);
        assert!(!m.date.is_empty());
        assert!(out.credits.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
