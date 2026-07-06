use crate::config::LmStudioConfig;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LmStudioError {
    #[error("lmstudio request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("lmstudio returned status {status}: {body}")]
    Status { status: StatusCode, body: String },
    #[error("lmstudio response did not contain parseable JSON intent")]
    InvalidResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LmStudioIntent {
    pub intent: String,
    #[serde(default)]
    pub arguments: Map<String, Value>,
    pub confidence: f32,
    #[serde(default)]
    pub speak: Option<String>,
}

#[derive(Clone)]
pub struct LmStudioClient {
    config: LmStudioConfig,
    http: reqwest::Client,
}

impl LmStudioClient {
    pub fn new(config: LmStudioConfig) -> Self {
        let timeout = Duration::from_millis(config.timeout_ms);
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client configuration is valid");
        Self { config, http }
    }

    pub async fn parse_intent(&self, utterance: &str) -> Result<LmStudioIntent, LmStudioError> {
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let model = self.config.model.as_deref().unwrap_or("local-model");
        let response = self
            .http
            .post(url)
            .json(&json!({
                "model": model,
                "temperature": 0.0,
                "response_format": { "type": "json_object" },
                "messages": [
                    {
                        "role": "system",
                        "content": "You parse offline voice assistant commands. Return only JSON with keys: intent string, arguments object, confidence number 0..1, speak nullable string. Prefer concise machine intents."
                    },
                    {
                        "role": "user",
                        "content": utterance
                    }
                ]
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(LmStudioError::Status { status, body });
        }

        let body: Value = response.json().await?;
        let content = body
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .ok_or(LmStudioError::InvalidResponse)?;
        parse_json_content(content)
    }
}

fn parse_json_content(content: &str) -> Result<LmStudioIntent, LmStudioError> {
    if let Ok(parsed) = serde_json::from_str::<LmStudioIntent>(content) {
        return Ok(parsed);
    }

    let start = content.find('{').ok_or(LmStudioError::InvalidResponse)?;
    let end = content.rfind('}').ok_or(LmStudioError::InvalidResponse)?;
    serde_json::from_str(&content[start..=end]).map_err(|_| LmStudioError::InvalidResponse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_strict_json_content() {
        let parsed = parse_json_content(
            r#"{"intent":"open_app","arguments":{"app":"browser"},"confidence":0.9,"speak":null}"#,
        )
        .unwrap();
        assert_eq!(parsed.intent, "open_app");
        assert_eq!(parsed.confidence, 0.9);
    }

    #[test]
    fn parses_json_inside_extra_text() {
        let parsed = parse_json_content(
            r#"Sure: {"intent":"search","arguments":{"query":"weather"},"confidence":0.75}"#,
        )
        .unwrap();
        assert_eq!(parsed.intent, "search");
    }
}
