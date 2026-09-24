use crate::config::{self, SavedConnection, DEFAULT_REFRESH_INTERVAL_SECS};
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::time::Duration;

pub fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .use_rustls_tls()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("tokscale-client/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("cannot build HTTP client: {error}"))
}

pub async fn connect(worker_url: Option<String>) -> Result<(), String> {
    let path = config::connection_path();
    let previous = config::load_connection(&path)?;
    let url = match worker_url.or_else(|| env_nonempty("SYNC_URL")) {
        Some(url) => url,
        None => {
            require_terminal()?;
            let default = previous.as_ref().map(|value| value.sync_url.as_str());
            match default {
                Some(url) => print!("Worker URL [{url}]: "),
                None => print!("Worker URL: "),
            }
            io::stdout().flush().map_err(|error| error.to_string())?;
            let mut line = String::new();
            if io::stdin()
                .read_line(&mut line)
                .map_err(|error| error.to_string())?
                == 0
            {
                return Err("connection setup cancelled".to_string());
            }
            if line.trim().is_empty() {
                default.unwrap_or("").to_string()
            } else {
                line.trim().to_string()
            }
        }
    };
    let sync_url = config::normalize_sync_url(&url)?;
    let token = match env_nonempty("SYNC_TOKEN") {
        Some(token) => token,
        None => {
            require_terminal()?;
            rpassword::prompt_password("Worker token (hidden): ")
                .map_err(|error| format!("cannot read token: {error}"))?
        }
    };
    let sync_token = token.trim().to_string();
    if sync_token.is_empty() {
        return Err("Worker token cannot be empty".to_string());
    }
    let refresh_interval_secs = match env_nonempty("REFRESH_INTERVAL_SECS") {
        Some(value) => value
            .parse::<u64>()
            .map_err(|_| "REFRESH_INTERVAL_SECS must be a non-negative integer".to_string())?,
        None => previous
            .as_ref()
            .map(|value| value.refresh_interval_secs)
            .unwrap_or(DEFAULT_REFRESH_INTERVAL_SECS),
    };
    let connection = SavedConnection {
        sync_url,
        sync_token,
        refresh_interval_secs,
    };
    println!("Verifying Worker connection...");
    verify_and_save(&path, &connection).await?;
    println!("Connection verified and saved to {}", path.display());
    println!("Run `tokscale-client run` to collect and sync (interval: {refresh_interval_secs}s).");
    Ok(())
}

pub(crate) async fn verify_and_save(
    path: &Path,
    connection: &SavedConnection,
) -> Result<(), String> {
    let mut connection = connection.clone();
    connection.sync_url = config::normalize_sync_url(&connection.sync_url)?;
    connection.sync_token = connection.sync_token.trim().to_string();
    if connection.sync_token.is_empty() {
        return Err("Worker token cannot be empty".to_string());
    }
    let mut url =
        reqwest::Url::parse(&connection.sync_url).map_err(|_| "invalid Worker URL".to_string())?;
    url.set_path("/api/me");
    let response = http_client()?
        .get(url)
        .bearer_auth(&connection.sync_token)
        .send()
        .await
        .map_err(|error| format!("Worker connection failed: {}", error.without_url()))?;
    if !response.status().is_success() {
        return Err(format!(
            "Worker authentication failed (HTTP {}); saved configuration was not changed",
            response.status().as_u16()
        ));
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|_| "Worker returned an invalid authentication response".to_string())?;
    if body
        .pointer("/actor/method")
        .and_then(|value| value.as_str())
        != Some("bearer")
    {
        return Err(
            "Worker did not confirm bearer authentication; saved configuration was not changed"
                .to_string(),
        );
    }
    config::save_connection(path, &connection)
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn require_terminal() -> Result<(), String> {
    if io::stdin().is_terminal() {
        Ok(())
    } else {
        Err("run `tokscale-client connect <WORKER_URL>` in a terminal first, or supply SYNC_URL and SYNC_TOKEN for non-interactive setup".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::{routing::get, Json, Router};

    #[tokio::test]
    async fn verification_persists_only_authenticated_connections() {
        let app = Router::new()
            .route(
                "/api/me",
                get(|headers: HeaderMap| async move {
                    match headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                    {
                        Some("Bearer accepted-token") => {
                            Json(serde_json::json!({"actor":{"method":"bearer"}})).into_response()
                        }
                        Some("Bearer cookie-token") => {
                            Json(serde_json::json!({"actor":{"method":"access"}})).into_response()
                        }
                        Some("Bearer redirect-token") => (
                            StatusCode::TEMPORARY_REDIRECT,
                            [("location", "/redirect-target")],
                        )
                            .into_response(),
                        Some("Bearer server-error-token") => {
                            (StatusCode::INTERNAL_SERVER_ERROR, "server-error-token")
                                .into_response()
                        }
                        Some("Bearer invalid-json-token") => "invalid-json-token".into_response(),
                        _ => (StatusCode::UNAUTHORIZED, "wrong-token").into_response(),
                    }
                }),
            )
            .route(
                "/redirect-target",
                get(|| async { Json(serde_json::json!({"actor":{"method":"bearer"}})) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let dir = config::tests::TestDirectory::new();
        let path = dir.0.join("device.json");
        let mut connection = SavedConnection {
            sync_url: url,
            sync_token: "wrong-token".to_string(),
            refresh_interval_secs: 60,
        };
        assert!(verify_and_save(&path, &connection).await.is_err());
        assert!(!path.exists());
        connection.sync_token = "accepted-token".to_string();
        verify_and_save(&path, &connection).await.unwrap();
        let saved = config::load_connection(&path).unwrap().unwrap();
        assert_eq!(saved.sync_token, "accepted-token");
        assert!(saved.sync_url.ends_with("/api/ingest"));
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(document["id"].as_str().unwrap().starts_with("dev_"));
        assert!(document["createdAt"].is_string());
        let original = std::fs::read(&path).unwrap();
        for token in [
            "wrong-token",
            "cookie-token",
            "redirect-token",
            "server-error-token",
            "invalid-json-token",
            " ",
        ] {
            connection.sync_token = token.to_string();
            let error = verify_and_save(&path, &connection).await.unwrap_err();
            if !token.trim().is_empty() {
                assert!(!error.contains(token));
            }
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
        server.abort();
    }
}
