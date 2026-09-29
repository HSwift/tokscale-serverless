//! On-demand diagnostics. No listener; read-only unless --sync is explicit.
use crate::{config, device, export, http, scan, sync};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn path_check(path: &Path) -> Value {
    let result = std::fs::metadata(path).and_then(|metadata| {
        if metadata.is_dir() {
            std::fs::read_dir(path).map(|_| "directory")
        } else {
            std::fs::File::open(path).map(|_| "file")
        }
    });
    match result {
        Ok(kind) => json!({"path":path,"readable":true,"kind":kind}),
        Err(e) => {
            json!({"path":path,"readable":false,"missing":e.kind() == std::io::ErrorKind::NotFound,"error":e.to_string()})
        }
    }
}

fn configured_source_checks(cfg: &config::Config) -> Vec<Value> {
    use tokscale_core::{clients::ClientId, scanner::extra_scan_paths_for};
    let enabled = |client: &str| {
        cfg.clients
            .as_ref()
            .is_none_or(|clients| clients.iter().any(|name| name == client))
    };
    let clients = cfg
        .scanner_settings
        .extra_scan_paths
        .keys()
        .filter_map(|name| ClientId::from_str(name))
        .filter(|client| enabled(client.as_str()))
        .collect();
    let mut paths: Vec<_> = extra_scan_paths_for(&cfg.scanner_settings, &clients)
        .into_iter()
        .map(|(client, path)| {
            (
                client.as_str(),
                path,
                matches!(client, ClientId::Codex | ClientId::Claude),
            )
        })
        .collect();
    if enabled("opencode") {
        paths.extend(
            cfg.scanner_settings
                .opencode_db_paths
                .iter()
                .cloned()
                .map(|path| ("opencode", path, false)),
        );
    }
    if enabled("qoder") {
        paths.extend(
            crate::settings::qoder_paths(&cfg.scanner_settings)
                .iter()
                .cloned()
                .map(|path| ("qoder", path, false)),
        );
    }
    paths
        .into_iter()
        .map(|(client, path, directory_only)| {
            let mut result = path_check(&path);
            result["client"] = client.into();
            result["configured"] = true.into();
            result["directoryOnly"] = directory_only.into();
            result
        })
        .collect()
}

fn check(report: &mut Value, stage: &str, ok: bool, started: Instant, details: Value) {
    report["checks"].as_array_mut().unwrap().push(json!({
        "stage":stage,"ok":ok,"elapsedMs":started.elapsed().as_millis(),"details":details
    }));
    if !ok {
        report["ok"] = false.into();
    }
}

pub async fn run(local_only: bool, do_sync: bool) -> bool {
    let mut report = json!({"schema":"tokscale-debug/1","version":env!("CARGO_PKG_VERSION"),
        "binary":std::env::current_exe().ok(),"os":std::env::consts::OS,"arch":std::env::consts::ARCH,
        "time":chrono::Utc::now().to_rfc3339(),"mode":if local_only {"local"} else if do_sync {"sync"} else {"readOnly"},
        "ok":true,"checks":[]});
    let start = Instant::now();
    let mut cfg = match config::Config::from_env() {
        Ok(cfg) => cfg,
        Err(error) => {
            check(
                &mut report,
                "configuration",
                false,
                start,
                json!({"error":error}),
            );
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
            return false;
        }
    };
    if local_only && cfg.pricing == config::PricingMode::Remote {
        cfg.pricing = config::PricingMode::Cached;
    }
    let home = cfg
        .tokscale_home
        .as_ref()
        .map(PathBuf::from)
        .or_else(tokscale_core::paths::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    let config_dir = tokscale_core::paths::get_config_dir();
    let mut roots = vec![path_check(&home)];
    for (client, variable, fallback) in [
        ("codex", "CODEX_HOME", ".codex"),
        ("claude", "CLAUDE_CONFIG_DIR", ".claude"),
    ] {
        if cfg
            .clients
            .as_ref()
            .is_none_or(|clients| clients.iter().any(|c| c == client))
        {
            let path = cfg
                .use_env_roots
                .then(|| std::env::var_os(variable))
                .flatten()
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(fallback));
            roots.push(path_check(&path));
        }
    }
    if cfg
        .clients
        .as_ref()
        .is_none_or(|clients| clients.iter().any(|c| c == "qoder"))
    {
        let env = |key: &str| {
            cfg.use_env_roots
                .then(|| std::env::var_os(key))
                .flatten()
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let mut candidates = crate::qoder::db_candidates(&home, env);
        candidates.extend(crate::qoder::projects_candidates(&home, env));
        candidates.sort();
        candidates.dedup();
        roots.extend(candidates.iter().map(|path| path_check(path)));
    }
    roots.extend(configured_source_checks(&cfg));
    check(
        &mut report,
        "configuration",
        true,
        start,
        json!({
            "configFile":config::connection_path(),"scanHome":home,"clients":cfg.clients,
            "settingsFile":crate::settings::path(),"scanner":cfg.scanner_settings,
            "useEnvRoots":cfg.use_env_roots,"pricing":format!("{:?}",cfg.pricing),
            "intervalSecs":cfg.refresh_interval_secs,"syncUrl":cfg.sync_url,
            "tokenConfigured":cfg.sync_token.is_some(),
            "coefficients":crate::coeffs::diagnostics(),
            "anchorPresent":config_dir.join("sync-state.json").exists()
        }),
    );
    let source_ok = roots[0]["readable"] == true
        && roots.iter().skip(1).all(|path| {
            if path["configured"] == true {
                path["readable"] == true
                    && (path["directoryOnly"] != true || path["kind"] == "directory")
            } else {
                path["readable"] == true || path["missing"] == true
            }
        });
    check(
        &mut report,
        "sourceAccess",
        source_ok,
        Instant::now(),
        json!({"paths":roots}),
    );

    let mut session = None;
    let mut prepared = None;
    let mut validation_body = None;
    let start = Instant::now();
    match sync::Session::acquire(true).await {
        Ok(Some(lock)) => {
            check(
                &mut report,
                "syncLock",
                true,
                start,
                json!({"acquired":true}),
            );
            let identity = device::resolve();
            report["configDiskFreeBytes"] = json!(fs2::available_space(&config_dir).ok());
            let anchor = lock.state_summary(&cfg, &identity);
            check(
                &mut report,
                "syncState",
                anchor["state"] != "unreadable",
                Instant::now(),
                anchor,
            );
            let start = Instant::now();
            match scan::collect(&cfg).await {
                Err(error) => check(
                    &mut report,
                    "collection",
                    false,
                    start,
                    json!({"error":error}),
                ),
                Ok(snapshot) => {
                    let mut tools = BTreeMap::<String, usize>::new();
                    for message in &snapshot.messages {
                        *tools.entry(message.client.clone()).or_default() += 1;
                    }
                    check(
                        &mut report,
                        "collection",
                        true,
                        start,
                        json!({
                            "messages":snapshot.messages.len(),"messagesByTool":tools,"creditRecords":snapshot.credits.len(),
                            "missingModelMessages":snapshot.messages.iter().filter(|m| m.model_id.trim().is_empty()).count(),
                            "missingTimestamps":snapshot.messages.iter().filter(|m| m.timestamp <= 0).count(),
                            "estimatedMessages":snapshot.messages.iter().filter(|m| m.cost_source == tokscale_core::sessions::CostSource::Estimated).count(),
                            "pricingLoaded":snapshot.pricing_loaded,"scanMs":snapshot.scan_duration_ms,
                            "warning":if snapshot.messages.is_empty() {Some("No usage collected; check the service user, paths, filters and file permissions.")} else {None}
                        }),
                    );
                    let start = Instant::now();
                    let payload = export::build_payload(snapshot, &identity);
                    let issues = export::validation_issues(&payload);
                    check(
                        &mut report,
                        "localPayload",
                        issues.is_empty(),
                        start,
                        json!({
                            "days":payload.contributions.len(),"dailyRows":payload.contributions.iter().map(|d| d.clients.len()).sum::<usize>(),
                            "timelineVersion":payload.meta.timeline_version,"sourceTimeZone":payload.meta.source_time_zone,
                            "utcMinutes":payload.contributions.iter().flat_map(|day| &day.clients).map(|row| row.timeline.as_ref().map_or(0, Vec::len)).sum::<usize>(),
                            "hourlyRows":payload.hourly.len(),"models":payload.summary.models.len(),
                            "dateRange":payload.meta.date_range,"bytes":serde_json::to_vec(&payload).map(|b| b.len()).ok(),
                            "validationErrors":issues
                        }),
                    );
                    let start = Instant::now();
                    if cfg.sync_url.is_some() {
                        match lock.prepare(&cfg, payload, false) {
                            Ok(plan) => {
                                validation_body = Some(match &plan {
                                    Some(plan) => serde_json::to_value(&plan.payload).unwrap(),
                                    None => {
                                        json!({"device":device::resolve(),"contributions":[],"hourly":[]})
                                    }
                                });
                                let body = validation_body.as_ref().unwrap();
                                check(
                                    &mut report,
                                    "syncPlan",
                                    true,
                                    start,
                                    json!({
                                        "mode":plan.as_ref().map(|p| if p.full {"full"} else {"incremental"}).unwrap_or("unchanged"),
                                        "pendingDays":body["contributions"].as_array().unwrap().len(),
                                        "pendingHourlyRows":body["hourly"].as_array().unwrap().len(),
                                        "requestBytes":body.to_string().len(),"anchorAdvanced":false
                                    }),
                                );
                                prepared = plan;
                            }
                            Err(error) => check(
                                &mut report,
                                "syncPlan",
                                false,
                                start,
                                json!({"error":error}),
                            ),
                        }
                    }
                }
            }
            session = Some(lock);
        }
        result => check(
            &mut report,
            "syncLock",
            false,
            start,
            json!({"error":result.err().unwrap_or_else(|| "another sync is running".into())}),
        ),
    }
    if !local_only {
        let start = Instant::now();
        match cfg.sync_url.as_deref().zip(cfg.sync_token.as_deref()) {
            None => check(
                &mut report,
                "remote",
                false,
                start,
                json!({"error":"No saved connection; run tokscale-client connect first."}),
            ),
            Some((url, token)) => {
                // Probe independently of local parsing so failures on either side
                // remain visible. Health performs one small, read-only D1 query.
                let mut authenticated = false;
                for (stage, path, body) in [
                    ("authentication", "/api/me", None),
                    ("serverHealth", "/api/diagnostics", None),
                    (
                        "remoteValidation",
                        "/api/ingest/validate",
                        validation_body.as_ref(),
                    ),
                ] {
                    if stage == "remoteValidation" && body.is_none() {
                        continue;
                    }
                    let start = Instant::now();
                    if stage != "authentication" && !authenticated {
                        check(
                            &mut report,
                            stage,
                            false,
                            start,
                            json!({"skipped":true,"reason":"Authentication/connectivity check failed."}),
                        );
                        continue;
                    }
                    match probe(url, token, path, body).await {
                        Ok(reply) => {
                            let expected = if stage == "authentication" {
                                reply.body["authMethod"] == "bearer"
                            } else if stage == "remoteValidation" {
                                reply.body["ok"] == true && reply.body["validationOnly"] == true
                            } else {
                                reply.body["ok"] == true
                            };
                            let ok = reply.success() && expected;
                            if stage == "authentication" {
                                authenticated = ok;
                            }
                            check(
                                &mut report,
                                stage,
                                ok,
                                start,
                                json!({"response":reply,
                                "hint":if matches!(reply.status,404|405) {Some("Deploy the updated API Worker to enable diagnostics; no ingest fallback was attempted.")} else {None}}),
                            );
                        }
                        Err(error) => {
                            check(&mut report, stage, false, start, json!({"error":error}))
                        }
                    }
                }
                if do_sync {
                    let start = Instant::now();
                    if report["ok"] != true {
                        check(
                            &mut report,
                            "upload",
                            false,
                            start,
                            json!({"skipped":true,"anchorAdvanced":false,"reason":"Resolve failed checks before uploading."}),
                        );
                    } else if let (Some(session), Some(plan)) = (session.as_ref(), prepared) {
                        match session.send(&cfg, plan).await {
                            Ok(reply) => check(
                                &mut report,
                                "upload",
                                true,
                                start,
                                json!({"response":reply,"anchorAdvanced":true}),
                            ),
                            Err(error) => check(
                                &mut report,
                                "upload",
                                false,
                                start,
                                json!({"error":error,"anchorAdvanced":false}),
                            ),
                        }
                    } else {
                        check(
                            &mut report,
                            "upload",
                            true,
                            start,
                            json!({"skipped":true,"reason":"Usage unchanged","anchorAdvanced":false}),
                        );
                    }
                }
            }
        }
    }
    let ok = report["ok"] == true;
    http::redact_value(&mut report, cfg.sync_token.as_deref());
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    ok
}

async fn probe(
    url: &str,
    token: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<http::Reply, String> {
    let mut url = reqwest::Url::parse(url).map_err(|_| "invalid API URL")?;
    url.set_path(path);
    let client = crate::connect::http_client()?;
    let request = match body {
        Some(body) => client.post(url).json(body),
        None => client.get(url),
    };
    http::send(request.bearer_auth(token), Some(token)).await
}
