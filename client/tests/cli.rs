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
            .env("BIND_ADDR", "127.0.0.1:0")
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
    let result = workspace.command().arg("run").output().unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("connect"));
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
                    uploads.lock().unwrap().push((token, body));
                    Json(json!({"ok": true}))
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
            "message":{"id":"message-1", "role":"assistant", "model":"qmodel", "usage":{
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

    // Each process starts with no SYNC_* variables or interval override.
    for _ in 0..2 {
        let before = received.lock().unwrap().len();
        let mut client = RunningClient(
            workspace
                .command()
                .arg("run")
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        tokio::time::timeout(Duration::from_secs(30), async {
            while received.lock().unwrap().len() < before + 2 {
                assert!(
                    client.0.try_wait().unwrap().is_none(),
                    "collector exited early"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("collector did not upload initial and periodic scans");
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
        assert!(!payload.to_string().contains("rotated-token"));
        assert!(payload["device"].get("syncToken").is_none());
    }
    server.abort();
}
