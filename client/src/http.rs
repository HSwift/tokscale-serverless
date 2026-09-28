//! Small, bounded HTTP diagnostics shared by normal sync and the debug command.
use serde::Serialize;
use serde_json::{json, Value};
use std::time::Instant;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    pub status: u16,
    pub elapsed_ms: u128,
    pub cf_ray: Option<String>,
    pub content_type: Option<String>,
    pub body: Value,
}

impl Reply {
    pub fn success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn describe(&self) -> String {
        let mut text = format!("HTTP {}", self.status);
        if let Some(message) = self.body.pointer("/error/message").and_then(Value::as_str) {
            text.push_str(&format!(": {message}"));
        } else if let Some(note) = self.body.get("note").and_then(Value::as_str) {
            text.push_str(&format!(": {note}"));
        }
        if let Some(ray) = &self.cf_ray {
            text.push_str(&format!(" (cf-ray: {ray})"));
        }
        text
    }
}

pub fn redact(text: &str, token: Option<&str>) -> String {
    match token.filter(|value| !value.is_empty()) {
        Some(token) => text.replace(token, "[redacted]"),
        None => text.to_string(),
    }
}

pub fn redact_value(value: &mut Value, token: Option<&str>) {
    match value {
        Value::String(text) => *text = redact(text, token),
        Value::Array(values) => values.iter_mut().for_each(|v| redact_value(v, token)),
        Value::Object(values) => values.values_mut().for_each(|v| redact_value(v, token)),
        _ => {}
    }
}

fn safe_text(text: &str, token: Option<&str>) -> String {
    redact(text, token)
        .chars()
        .filter(|c| !c.is_control())
        .take(1024)
        .collect()
}

pub async fn send(request: reqwest::RequestBuilder, token: Option<&str>) -> Result<Reply, String> {
    let start = Instant::now();
    let mut response = request.send().await.map_err(|error| {
        let error = error.without_url();
        let mut messages = vec![error.to_string()];
        let mut cause = std::error::Error::source(&error);
        while let Some(error) = cause {
            messages.push(error.to_string());
            cause = error.source();
        }
        safe_text(&messages.join(": "), token)
    })?;
    let header = |name| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|v| safe_text(v, token))
    };
    let mut reply = Reply {
        status: response.status().as_u16(),
        elapsed_ms: 0,
        cf_ray: header("cf-ray"),
        content_type: header("content-type"),
        body: Value::Null,
    };
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| {
        format!(
            "response body failed: {}",
            safe_text(&e.without_url().to_string(), token)
        )
    })? {
        if bytes.len() + chunk.len() > 16 * 1024 {
            reply.body = json!({"note":"response body exceeds 16 KiB"});
            reply.elapsed_ms = start.elapsed().as_millis();
            return Ok(reply);
        }
        bytes.extend_from_slice(&chunk);
    }
    reply.body = match serde_json::from_slice::<Value>(&bytes) {
        Ok(body) => {
            // Do not dump arbitrary server content, echoed headers or actor IDs.
            let mut selected = serde_json::Map::new();
            for key in [
                "ok",
                "validationOnly",
                "days",
                "rows",
                "hourly",
                "rowsRead",
                "rowsWritten",
                "apiVersion",
                "serverTime",
                "database",
            ] {
                if let Some(value) = body.get(key) {
                    selected.insert(key.into(), value.clone());
                }
            }
            if let Some(method) = body.pointer("/actor/method") {
                selected.insert("authMethod".into(), method.clone());
            }
            if let Some(error) = body.get("error") {
                let fields: serde_json::Map<String, Value> = ["code", "path", "message"]
                    .into_iter()
                    .filter_map(|key| {
                        error
                            .get(key)
                            .and_then(Value::as_str)
                            .map(|value| (key.into(), json!(safe_text(value, token))))
                    })
                    .collect();
                selected.insert("error".into(), fields.into());
            }
            // Also redact string fields in structured database diagnostics.
            let mut body = Value::Object(selected);
            redact_value(&mut body, token);
            body
        }
        Err(_) => json!({"note":"non-JSON response (possible proxy, login page, or server error)"}),
    };
    reply.elapsed_ms = start.elapsed().as_millis();
    Ok(reply)
}
