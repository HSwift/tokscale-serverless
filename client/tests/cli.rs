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
                        Ok(Json(json!({"ok": true})))
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
                        Json(json!({"ok":true}))
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
    assert!(String::from_utf8_lossy(&skipped.stdout).contains("usage unchanged"));
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
