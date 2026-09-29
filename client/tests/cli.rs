//! Native CLI tests: these run on each OS/architecture in the release matrix.
use axum::http::{HeaderMap, StatusCode};
use axum::{
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("tokscale 测试 space {}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(path.join("data")).unwrap();
        Self(path)
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tokscale-client"));
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("TOKSCALE_")
                || matches!(
                    name.as_ref(),
                    "SYNC_URL"
                        | "SYNC_TOKEN"
                        | "REFRESH_INTERVAL_SECS"
                        | "HOSTNAME"
                        | "COMPUTERNAME"
                )
            {
                cmd.env_remove(key);
            }
        }
        cmd.current_dir(&self.0)
            .env("TOKSCALE_CONFIG_DIR", self.0.join("设置 config"))
            .env("TOKSCALE_HOME", self.0.join("data"))
            .env("TOKSCALE_USE_ENV_ROOTS", "false")
            .env("TOKSCALE_CLIENTS", "qoder")
            .env("TOKSCALE_PRICING", "off")
            .env("QODER_PROJECTS_DIR", self.0.join("excluded projects"))
            .env("NO_PROXY", "127.0.0.1,localhost")
            .stdin(Stdio::null());
        cmd
    }

    fn device_path(&self) -> PathBuf {
        self.0.join("设置 config").join("device.json")
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct RunningClient(Child);

impl Drop for RunningClient {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn help_resolves_unicode_configuration_directory_and_run_never_prompts() {
    let workspace = Workspace::new();
    let result = workspace.command().arg("--help").output().unwrap();
    assert!(result.status.success());
    assert!(String::from_utf8_lossy(&result.stdout)
        .contains(&workspace.device_path().display().to_string()));
    assert!(!workspace.device_path().exists());
    for args in [vec![], vec!["run"], vec!["sync"], vec!["sync", "--full"]] {
        let result = workspace.command().args(args).output().unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&result.stderr).contains("connect"));
    }
    let result = workspace.command().arg("local").output().unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("usage:"));
    assert!(!workspace.device_path().exists());
}

#[test]
fn default_configuration_directory_matches_platform_convention() {
    let workspace = Workspace::new();
    let result = workspace
        .command()
        .arg("--help")
        .env_remove("TOKSCALE_CONFIG_DIR")
        .env_remove("XDG_CONFIG_HOME")
        .output()
        .unwrap();
    assert!(result.status.success());
    let output = String::from_utf8_lossy(&result.stdout);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let expected = tokscale_core::paths::home_dir()
        .unwrap()
        .join(".config/tokscale/device.json");
    #[cfg(windows)]
    let expected = PathBuf::from(std::env::var_os("APPDATA").unwrap())
        .join("tokscale")
        .join("device.json");
    assert!(
        output.contains(&expected.display().to_string()),
        "unexpected configuration location: {output}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_honors_xdg_config_home() {
    let workspace = Workspace::new();
    let xdg = workspace.0.join("xdg settings");
    let result = workspace
        .command()
        .arg("--help")
        .env_remove("TOKSCALE_CONFIG_DIR")
        .env("XDG_CONFIG_HOME", &xdg)
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(String::from_utf8_lossy(&result.stdout)
        .contains(&xdg.join("tokscale/device.json").display().to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_rotation_restart_and_periodic_collection_across_platforms() {
    let received: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
    let uploads = received.clone();
    let app = Router::new()
        .route(
            "/api/me",
            get(|headers: HeaderMap| async move {
                match headers.get("authorization").and_then(|v| v.to_str().ok()) {
                    Some("Bearer test-token" | "Bearer rotated-token") => {
                        Ok(Json(json!({"actor":{"method":"bearer"}})))
                    }
                    _ => Err(StatusCode::UNAUTHORIZED),
                }
            }),
        )
        .route(
            "/api/ingest",
            post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let uploads = uploads.clone();
                async move {
                    let token = headers
                        .get("authorization")
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_string();
                    let attempt = {
                        let mut uploads = uploads.lock().unwrap();
                        uploads.push((token, body));
                        uploads.len()
                    };
                    // Exercise retrying a failed upload and shutdown during an
                    // in-flight request, without interrupting the last upload.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    if attempt == 1 {
                        Err(StatusCode::SERVICE_UNAVAILABLE)
                    } else {
                        Ok(Json(json!({"ok": true,"timelineVersion":1})))
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let workspace = Workspace::new();
    let project = workspace.0.join("data/.qoder/projects/会话 session");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("session.jsonl"),
        json!({
            "type":"assistant", "timestamp":"2026-09-21T17:18:22Z", "sessionId":"test-session",
            "message":{"id":"message-1", "role":"assistant", "model":"example-qoder-model", "usage":{
                "input_tokens":100, "output_tokens":10, "cache_read_input_tokens":20, "credits":0.1
            }}
        })
        .to_string(),
    )
    .unwrap();
    // Even a valid extra source must be ignored when use_env_roots is false.
    let excluded = workspace.0.join("excluded projects");
    std::fs::create_dir_all(&excluded).unwrap();
    let mut extra: Value =
        serde_json::from_slice(&std::fs::read(project.join("session.jsonl")).unwrap()).unwrap();
    extra["message"]["id"] = json!("outside-message");
    std::fs::write(excluded.join("outside.jsonl"), extra.to_string()).unwrap();
    let mut identity = Value::Null;
    for token in ["test-token", "rotated-token"] {
        let result = workspace
            .command()
            .args(["connect", &url])
            .env("SYNC_TOKEN", token)
            .env("REFRESH_INTERVAL_SECS", "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let saved: Value =
            serde_json::from_slice(&std::fs::read(workspace.device_path()).unwrap()).unwrap();
        assert_eq!(saved["syncToken"], token);
        assert_eq!(saved["refreshIntervalSecs"], 1);
        if identity.is_null() {
            identity = saved["id"].clone();
        }
        assert_eq!(saved["id"], identity);
    }
    let original = std::fs::read(workspace.device_path()).unwrap();
    let rejected = workspace
        .command()
        .args(["connect", &url])
        .env("SYNC_TOKEN", "wrong-token")
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(2));
    assert_eq!(std::fs::read(workspace.device_path()).unwrap(), original);

    // A stale service setting must no longer cause a port conflict.
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    for periodic in [true, true, false] {
        let before = received.lock().unwrap().len();
        // Each round starts without an anchor; restart persistence is exercised
        // separately by the one-shot sync test below.
        let _ = std::fs::remove_file(workspace.device_path().with_file_name("sync-state.json"));
        let mut command = workspace.command();
        command
            .arg("run")
            .env("BIND_ADDR", occupied.local_addr().unwrap().to_string());
        if !periodic {
            command.env("REFRESH_INTERVAL_SECS", "0");
        }
        let mut client = RunningClient(
            command
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        tokio::time::timeout(Duration::from_secs(30), async {
            let expected = before + if before == 0 { 2 } else { 1 };
            while received.lock().unwrap().len() < expected {
                assert!(
                    client.0.try_wait().unwrap().is_none(),
                    "collector exited early"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("collector did not upload or retry the initial scan");
        #[cfg(target_os = "linux")]
        assert_no_listening_sockets(client.0.id());
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert_eq!(
            received.lock().unwrap().len(),
            before + if before == 0 { 2 } else { 1 }
        );
        assert!(client.0.try_wait().unwrap().is_none());
        #[cfg(unix)]
        {
            assert!(Command::new("kill")
                .args(["-TERM", &client.0.id().to_string()])
                .status()
                .unwrap()
                .success());
            let status = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if let Some(status) = client.0.try_wait().unwrap() {
                        break status;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("collector did not shut down gracefully");
            assert!(
                status.success(),
                "collector was terminated instead of stopping cleanly"
            );
        }
        drop(client);
    }
    for (token, payload) in received.lock().unwrap().iter() {
        assert_eq!(token, "Bearer rotated-token");
        assert_eq!(payload["device"]["id"], identity);
        assert_eq!(payload["device"]["os"], std::env::consts::OS);
        assert_eq!(payload["device"]["arch"], std::env::consts::ARCH);
        assert_ne!(payload["device"]["hostname"], "unknown");
        assert_eq!(payload["contributions"].as_array().unwrap().len(), 1);
        assert_eq!(payload["contributions"][0]["totals"]["messages"], 1);
        assert_eq!(payload["contributions"][0]["totals"]["tokens"], 110);
        assert_eq!(payload["contributions"][0]["totals"]["credits"], 0.1);
        assert_eq!(
            payload["contributions"][0]["totals"]["costIsComplete"],
            false
        );
        assert_eq!(payload["hourly"].as_array().unwrap().len(), 1);
        assert_eq!(payload["hourly"][0]["tokens"], 110);
        assert_eq!(payload["hourly"][0]["client"], "qoder");
        assert_eq!(payload["hourly"][0]["modelId"], "example-qoder-model");
        assert_eq!(
            payload["meta"]["dateRange"]["start"],
            payload["contributions"][0]["date"]
        );
        assert_eq!(
            payload["meta"]["dateRange"]["end"],
            payload["contributions"][0]["date"]
        );
        assert!(!payload.to_string().contains("rotated-token"));
        assert!(payload["device"].get("syncToken").is_none());
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_shot_sync_reuses_anchor_and_full_resends_history() {
    let received: Arc<Mutex<Vec<Value>>> = Arc::default();
    let uploads = received.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api/ingest", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/api/ingest",
                post(move |Json(body): Json<Value>| {
                    let uploads = uploads.clone();
                    async move {
                        uploads.lock().unwrap().push(body);
                        Json(json!({"ok":true,"timelineVersion":1}))
                    }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let workspace = Workspace::new();
    std::fs::create_dir_all(workspace.device_path().parent().unwrap()).unwrap();
    std::fs::write(
        workspace.device_path(),
        json!({
            "id":"dev_sync", "createdAt":"2026-09-01T00:00:00Z",
            "syncUrl":url, "syncToken":"test-token", "refreshIntervalSecs":60
        })
        .to_string(),
    )
    .unwrap();
    let project = workspace.0.join("data/.qoder/projects/test");
    std::fs::create_dir_all(&project).unwrap();
    let write_usage = |input: i64| {
        let rows = [
            ("2026-09-21T12:00:00Z", "old", input),
            ("2026-09-22T12:00:00Z", "new", 100),
        ];
        std::fs::write(project.join("session.jsonl"), rows.into_iter().map(|(timestamp, id, input)| json!({
            "type":"assistant", "timestamp":timestamp, "sessionId":"test",
            "message":{"id":id, "model":"test-model", "usage":{"input_tokens":input,"output_tokens":10}}
        }).to_string()).collect::<Vec<_>>().join("\n")).unwrap();
    };
    let sync = |args: &[&str]| {
        let output = workspace.command().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    write_usage(100);
    sync(&["sync"]);
    assert_eq!(received.lock().unwrap().len(), 1);
    assert!(workspace
        .device_path()
        .with_file_name("sync-state.json")
        .is_file());
    let skipped = sync(&["sync"]);
    assert!(String::from_utf8_lossy(&skipped.stderr).contains("usage unchanged"));
    assert_eq!(received.lock().unwrap().len(), 1);
    write_usage(90); // A correction to an older day must not be missed.
    sync(&["sync"]);
    sync(&["sync", "--full"]);
    let uploads = received.lock().unwrap();
    assert_eq!(uploads.len(), 3);
    assert_eq!(uploads[0]["contributions"].as_array().unwrap().len(), 2);
    assert_eq!(uploads[1]["contributions"].as_array().unwrap().len(), 1);
    assert_eq!(
        uploads[1]["contributions"][0]["clients"][0]["tokens"]["input"],
        90
    );
    assert_eq!(uploads[1]["hourly"].as_array().unwrap().len(), 1);
    assert_eq!(uploads[1]["hourly"][0]["tokens"], 100);
    assert_eq!(uploads[2]["contributions"].as_array().unwrap().len(), 2);
    assert_eq!(uploads[2]["hourly"].as_array().unwrap().len(), 2);
    server.abort();
}

#[cfg(target_os = "linux")]
fn assert_no_listening_sockets(pid: u32) {
    let sockets: std::collections::HashSet<_> = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
        .filter_map(|path| {
            path.to_str()?
                .strip_prefix("socket:[")?
                .strip_suffix(']')
                .map(str::to_string)
        })
        .collect();
    for protocol in ["tcp", "tcp6"] {
        let table = std::fs::read_to_string(format!("/proc/{pid}/net/{protocol}")).unwrap();
        for line in table.lines().skip(1) {
            let fields: Vec<_> = line.split_whitespace().collect();
            assert!(
                !(fields[3] == "0A" && sockets.contains(fields[9])),
                "collector unexpectedly owns a listening {protocol} socket"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_empty_models_keep_usage_and_sync_consistently_after_restart() {
    let received: Arc<Mutex<Vec<Value>>> = Arc::default();
    let uploads = received.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api/ingest", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/api/ingest",
                post(move |Json(body): Json<Value>| {
                    let uploads = uploads.clone();
                    async move {
                        if body["hourly"].as_array().unwrap().iter().any(|row| {
                            row["modelId"]
                                .as_str()
                                .is_none_or(|id| id.trim().is_empty())
                        }) {
                            return Err(StatusCode::BAD_REQUEST);
                        }
                        uploads.lock().unwrap().push(body);
                        Ok(Json(json!({"ok":true,"timelineVersion":1})))
                    }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let workspace = Workspace::new();
    std::fs::create_dir_all(workspace.device_path().parent().unwrap()).unwrap();
    std::fs::write(
        workspace.device_path(),
        json!({
            "id":"dev_empty_models", "createdAt":"2026-01-01T00:00:00Z",
            "syncUrl":url, "syncToken":"test-token", "refreshIntervalSecs":3600
        })
        .to_string(),
    )
    .unwrap();
    let project = workspace.0.join("data/.claude/projects/test");
    std::fs::create_dir_all(&project).unwrap();
    let rows = ["", " \t ", "unknown", "example-model"].into_iter().enumerate().map(|(i, model)| json!({
        "type":"assistant", "timestamp":"2026-01-02T12:00:00Z",
        "requestId":format!("request-{i}"),
        "message":{"id":format!("message-{i}"),"model":model,"usage":{
            "input_tokens":100,"output_tokens":10,"cache_read_input_tokens":20,"cache_creation_input_tokens":30
        }}
    }).to_string()).collect::<Vec<_>>().join("\n");
    std::fs::write(project.join("session.jsonl"), rows).unwrap();
    let run = |args: &[&str]| {
        let output = workspace
            .command()
            .env("TOKSCALE_CLIENTS", "claude")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    let report: Value = serde_json::from_slice(&run(&["debug", "--local"]).stdout).unwrap();
    let collection = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["stage"] == "collection")
        .unwrap();
    assert_eq!(collection["details"]["missingModelMessages"], 2);
    let anchor = workspace.device_path().with_file_name("sync-state.json");
    assert!(!anchor.exists());
    run(&["sync"]);
    let saved = std::fs::read(&anchor).unwrap();
    // A new process reuses the parser cache and acknowledged bucket identities.
    run(&["sync"]);
    assert_eq!(received.lock().unwrap().len(), 1);
    assert_eq!(std::fs::read(&anchor).unwrap(), saved);
    run(&["sync", "--full"]);
    let uploads = received.lock().unwrap();
    assert_eq!(uploads.len(), 2);
    for body in uploads.iter() {
        let day = &body["contributions"][0];
        assert_eq!(day["totals"]["tokens"], 640);
        assert_eq!(day["totals"]["messages"], 4);
        assert_eq!(day["clients"].as_array().unwrap().len(), 2);
        let unknown = day["clients"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["modelId"] == "unknown")
            .unwrap();
        assert_eq!(
            unknown["tokens"],
            json!({"input":300,"output":30,"cacheRead":60,"cacheWrite":90,"reasoning":0})
        );
        assert_eq!(body["hourly"].as_array().unwrap().len(), 2);
        for row in body["hourly"].as_array().unwrap() {
            assert_eq!(
                row["tokens"],
                if row["modelId"] == "unknown" {
                    480
                } else {
                    160
                }
            );
        }
        assert_eq!(body["summary"]["total_tokens"], 640);
        assert_eq!(body["summary"]["models"].as_array().unwrap().len(), 2);
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diagnostics_trace_local_remote_and_sync_failures_without_leaking_tokens() {
    use axum::response::IntoResponse;
    use std::sync::atomic::{AtomicU8, Ordering};
    let mode = Arc::new(AtomicU8::new(0));
    let routes: Arc<Mutex<Vec<String>>> = Arc::default();
    let state = mode.clone();
    let requests = routes.clone();
    let app = Router::new().fallback(move |request: axum::extract::Request| {
        let mode = state.load(Ordering::SeqCst);
        let path = request.uri().path().to_string();
        requests.lock().unwrap().push(path.clone());
        async move {
            assert_eq!(
                request.headers()["user-agent"],
                concat!("tokscale-client/", env!("CARGO_PKG_VERSION"))
            );
            assert_eq!(request.headers()["authorization"], "Bearer test-token");
            if mode == 1 && path == "/api/me" {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error":{"message":"invalid test-token"}})),
                )
                    .into_response();
            }
            if mode == 2 && path == "/api/me" {
                return (StatusCode::FOUND, [("location", "/login")]).into_response();
            }
            if mode == 3 && path == "/api/diagnostics" {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"ok":false,"database":{"ok":false,"error":"quota exceeded"}})),
                )
                    .into_response();
            }
            if mode == 4 && path == "/api/ingest" {
                return (StatusCode::BAD_REQUEST, [("cf-ray","test-ray")], Json(json!({
                    "error":{"code":"bad_request","message":"hourly[0].tokens: invalid test-token"}
                }))).into_response();
            }
            if mode == 5 && path == "/api/ingest/validate" {
                return (StatusCode::METHOD_NOT_ALLOWED, "old API").into_response();
            }
            if mode == 6 && path == "/api/me" {
                return (StatusCode::OK, "<html>test-token login page</html>").into_response();
            }
            if mode == 7 && path == "/api/me" {
                return (StatusCode::BAD_GATEWAY, "test-token".repeat(3000)).into_response();
            }
            Json(match path.as_str() {
                "/api/me" => json!({"actor":{"method":"bearer"}}),
                "/api/diagnostics" => {
                    json!({"ok":true,"apiVersion":"0.3.0","database":{"ok":true}})
                }
                "/api/ingest/validate" => json!({"ok":true,"validationOnly":true}),
                "/api/ingest" => {
                    json!({"ok":true,"timelineVersion":1,"rowsRead":2,"rowsWritten":1})
                }
                _ => panic!("unexpected diagnostic route"),
            })
            .into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let workspace = Workspace::new();
    std::fs::create_dir_all(workspace.device_path().parent().unwrap()).unwrap();
    std::fs::write(
        workspace.device_path(),
        json!({
            "id":"dev_debug","createdAt":"2026-01-01T00:00:00Z", "syncUrl":url,
            "syncToken":"test-token","refreshIntervalSecs":3600
        })
        .to_string(),
    )
    .unwrap();
    let source = workspace.0.join("data/.qoder/projects/test");
    std::fs::create_dir_all(&source).unwrap();
    let usage = |model: &str| {
        json!({"type":"assistant", "timestamp":"2026-09-22T08:00:00Z", "message":{
            "id":"example", "model":model, "usage":{"input_tokens":100,"output_tokens":10,"credits":1}
        }})
    };
    std::fs::write(
        source.join("test.jsonl"),
        usage("example-model").to_string(),
    )
    .unwrap();
    let anchor = workspace.device_path().with_file_name("sync-state.json");
    let run = |args: &[&str], expected: bool| -> Value {
        let output = workspace.command().args(args).output().unwrap();
        assert_eq!(
            output.status.success(),
            expected,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!combined.contains("test-token"));
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let check = |report: &Value, stage: &str| {
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["stage"] == stage)
            .unwrap()
            .clone()
    };
    let local = run(&["debug", "--local"], true);
    assert_eq!(local["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        check(&local, "collection")["details"]["messagesByTool"]["qoder"],
        1
    );
    assert!(routes.lock().unwrap().is_empty());
    assert!(!anchor.exists());
    let dry = run(&["debug"], true);
    assert_eq!(check(&dry, "syncPlan")["details"]["mode"], "full");
    assert!(!anchor.exists());
    assert_eq!(
        *routes.lock().unwrap(),
        ["/api/me", "/api/diagnostics", "/api/ingest/validate"]
    );
    let sent = run(&["debug", "--sync"], true);
    assert_eq!(
        check(&sent, "upload")["details"]["response"]["body"]["rowsWritten"],
        1
    );
    let saved = std::fs::read(&anchor).unwrap();
    for value in [1, 2, 3, 5, 6, 7] {
        mode.store(value, Ordering::SeqCst);
        let report = run(&["debug", "--sync"], false);
        assert_eq!(check(&report, "upload")["details"]["anchorAdvanced"], false);
        assert_eq!(std::fs::read(&anchor).unwrap(), saved);
    }
    mode.store(4, Ordering::SeqCst);
    let failed = workspace
        .command()
        .args(["sync", "--full"])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(stderr.contains("hourly[0].tokens"));
    assert!(stderr.contains("test-ray"));
    assert!(!stderr.contains("test-token"));
    assert_eq!(std::fs::read(&anchor).unwrap(), saved);
    std::fs::write(source.join("test.jsonl"), usage("").to_string()).unwrap();
    let unnamed = run(&["debug", "--local"], true);
    assert_eq!(
        check(&unnamed, "collection")["details"]["missingModelMessages"],
        1
    );
    assert_eq!(check(&unnamed, "localPayload")["ok"], true);
    let mut oversized = usage("example-model");
    oversized["message"]["usage"]["input_tokens"] = json!(9_007_199_254_740_992_i64);
    std::fs::write(source.join("test.jsonl"), oversized.to_string()).unwrap();
    let invalid = run(&["debug", "--local"], false);
    let issues = &check(&invalid, "localPayload")["details"]["validationErrors"];
    for path in ["hourly[0].tokens", "contributions[0].clients[0].timeline"] {
        assert!(issues
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["path"] == path));
    }
    server.abort();
    let _ = server.await;
    let disconnected = run(&["debug"], false);
    assert!(check(&disconnected, "authentication")["details"]["error"].is_string());
    assert_eq!(std::fs::read(&anchor).unwrap(), saved);
    std::fs::write(&anchor, b"truncated {").unwrap();
    let corrupt = run(&["debug", "--local"], false);
    assert_eq!(check(&corrupt, "syncState")["details"]["state"], "invalid");
    assert_eq!(std::fs::read(&anchor).unwrap(), b"truncated {");
    std::fs::write(
        workspace.device_path(),
        r#"{"syncToken":"test-token", "refreshIntervalSecs":"bad"}"#,
    )
    .unwrap();
    let invalid_config = run(&["debug"], false);
    assert_eq!(check(&invalid_config, "configuration")["ok"], false);
}

#[test]
fn upstream_settings_collect_multiple_roots_and_report_missing_or_wrong_paths() {
    let workspace = Workspace::new();
    let mut sources = json!({"codex":[],"claude":[],"qoder":[]});
    for (tool, default) in [
        ("codex", ".codex/sessions"),
        ("claude", ".claude/projects"),
        ("qoder", ".qoder/projects"),
    ] {
        for i in 0..3 {
            let root = if i == 0 {
                workspace.0.join("data").join(default)
            } else {
                workspace.0.join(format!("private sources/{tool}/{i}"))
            };
            // Include the default root, duplicates and overlapping roots to check deduplication.
            sources[tool].as_array_mut().unwrap().extend([
                json!(root),
                json!(root.join("nested")),
                json!(root),
            ]);
            std::fs::create_dir_all(root.join("nested")).unwrap();
            let usage = json!({"input_tokens":100,"output_tokens":10});
            let rows = if tool == "codex" {
                vec![
                    json!({"type":"session_meta","payload":{"id":format!("session-{i}")}}),
                    json!({"type":"turn_context","payload":{"model":"example-model"}}),
                    json!({"type":"event_msg","timestamp":"2026-01-02T12:00:00Z",
                        "payload":{"type":"token_count","info":{"total_token_usage":usage,"last_token_usage":usage}}}),
                ]
            } else {
                vec![
                    json!({"type":"assistant","timestamp":"2026-01-02T12:00:00Z",
                    "sessionId":format!("session-{i}"),"requestId":format!("request-{i}"),
                    "message":{"id":format!("message-{i}"),"model":"example-model","usage":usage}}),
                ]
            };
            std::fs::write(
                root.join(format!("nested/rollout-session-{i}.jsonl")),
                rows.iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .unwrap();
        }
    }
    for i in 0..2 {
        let path = workspace.0.join(format!("private sources/qoder-{i}.db"));
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE chat_message(id TEXT, session_id TEXT, request_id TEXT, role TEXT, token_info TEXT, model_info TEXT, gmt_create INTEGER); CREATE TABLE chat_record(request_id TEXT, extra TEXT);").unwrap();
        conn.execute(
            "INSERT INTO chat_message VALUES (?1,'session',?1,'assistant',?2,?3,1767355200000)",
            rusqlite::params![
                format!("database-request-{i}"),
                r#"{"prompt_tokens":100,"completion_tokens":10}"#,
                r#"{"model_key":"example-model"}"#
            ],
        )
        .unwrap();
        sources["qoder"]
            .as_array_mut()
            .unwrap()
            .extend([json!(path), json!(path)]);
    }
    let settings_path = workspace.device_path().with_file_name("settings.json");
    let mut document =
        json!({"scanner":{"extraScanPaths":sources}, "defaultClients":["codex","claude","qoder"]});
    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    std::fs::write(
        workspace.device_path(),
        json!({"id":"dev_sources","createdAt":"2026-01-01T00:00:00Z"}).to_string(),
    )
    .unwrap();
    let write = |value: &Value| std::fs::write(&settings_path, value.to_string()).unwrap();
    write(&document);
    let run = |clients: &str| {
        let mut command = workspace.command();
        if clients == "defaults" {
            command.env_remove("TOKSCALE_CLIENTS");
        } else {
            command.env("TOKSCALE_CLIENTS", clients);
        }
        let output = command
            .current_dir(workspace.0.join("data"))
            .args(["debug", "--local"])
            .output()
            .unwrap();
        let report: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
        (output.status.success(), report)
    };
    let stage = |report: &Value, name: &str| {
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["stage"] == name)
            .unwrap()
            .clone()
    };
    // Also run a second process to exercise parser caches and persisted configuration.
    for _ in 0..2 {
        let (ok, report) = run("defaults");
        assert!(ok, "{report}");
        assert_eq!(
            stage(&report, "collection")["details"]["messagesByTool"],
            json!({"codex":3,"claude":3,"qoder":5})
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(&settings_path).unwrap()).unwrap(),
            document
        );
    }
    let (ok, report) = run("codex");
    assert!(ok);
    assert_eq!(
        stage(&report, "collection")["details"]["messagesByTool"],
        json!({"codex":3})
    );

    document["scanner"]["extraScanPaths"]["codex"] = json!([workspace.0.join("missing sessions")]);
    write(&document);
    let (ok, report) = run("codex");
    assert!(!ok);
    let access = stage(&report, "sourceAccess");
    assert_eq!(access["ok"], false);
    assert!(access["details"]["paths"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path["configured"] == true && path["missing"] == true));
    // A file accidentally entered as a directory must also fail the source check.
    document["scanner"]["extraScanPaths"]["codex"] = json!([workspace.device_path()]);
    write(&document);
    let (ok, report) = run("codex");
    assert!(!ok);
    assert_eq!(stage(&report, "sourceAccess")["ok"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_timezone_aligns_daily_hourly_and_qoder_credits() {
    let received: Arc<Mutex<Vec<Value>>> = Arc::default();
    let uploads = received.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api/ingest", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/api/ingest",
                post(move |Json(body): Json<Value>| {
                    let uploads = uploads.clone();
                    async move {
                        uploads.lock().unwrap().push(body);
                        Json(json!({"ok":true,"timelineVersion":1}))
                    }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let workspace = Workspace::new();
    std::fs::create_dir_all(workspace.device_path().parent().unwrap()).unwrap();
    std::fs::write(
        workspace.device_path(),
        json!({
            "id":"dev_timezone","createdAt":"2026-01-01T00:00:00Z",
            "syncUrl":url,"syncToken":"test-token","refreshIntervalSecs":3600
        })
        .to_string(),
    )
    .unwrap();
    let settings = json!({"defaultClients":["claude","qoder"],
        "scanner":{"bucketTimezone":"Pacific/Kiritimati"},"colorPalette":"blue"});
    let settings_path = workspace.device_path().with_file_name("settings.json");
    std::fs::write(&settings_path, settings.to_string()).unwrap();
    for tool in ["claude", "qoder"] {
        let root = workspace.0.join(format!("data/.{tool}/projects/test"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("session.jsonl"),
            json!({
                "type":"assistant","timestamp":"2026-01-02T12:00:00Z","sessionId":"test-session",
                "requestId":"request","message":{"id":"message","model":"example-model",
                    "usage":{"input_tokens":100,"output_tokens":10,"credits":2.5}}
            })
            .to_string(),
        )
        .unwrap();
    }
    // Restart under another machine timezone: the pinned buckets must remain identical.
    for local_zone in ["UTC", "America/New_York"] {
        let output = workspace
            .command()
            .env_remove("TOKSCALE_CLIENTS")
            .env("TZ", local_zone)
            .args(["sync", "--full"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let uploads = received.lock().unwrap();
    assert_eq!(uploads.len(), 2);
    for body in uploads.iter() {
        assert_eq!(body["contributions"].as_array().unwrap().len(), 1);
        let daily = &body["contributions"][0];
        assert_eq!(daily["date"], "2026-01-03");
        assert_eq!(body["meta"]["timelineVersion"], 1);
        assert_eq!(body["meta"]["sourceTimeZone"], "Pacific/Kiritimati");
        let minute = chrono::DateTime::parse_from_rfc3339("2026-01-02T12:00:00Z")
            .unwrap()
            .timestamp()
            / 60;
        for row in daily["clients"].as_array().unwrap() {
            assert_eq!(row["timeline"], json!([[minute, 110]]));
        }
        assert_eq!(daily["totals"]["tokens"], 220);
        assert_eq!(daily["totals"]["credits"], 2.5);
        assert_eq!(body["hourly"].as_array().unwrap().len(), 2);
        for row in body["hourly"].as_array().unwrap() {
            assert_eq!(row["date"], "2026-01-03");
            assert_eq!(row["hour"], 2);
            assert_eq!(row["tokens"], 110);
        }
        assert!(body.get("scanner").is_none());
    }
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&settings_path).unwrap()).unwrap(),
        settings
    );
    server.abort();
}
