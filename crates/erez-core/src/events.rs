use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    WakeDetected,
    SpeechRecognized,
    IntentResolved,
    ActionExecuted,
    CommandUnrecognized,
    Error,
    Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantEvent {
    pub id: Uuid,
    pub ts_ms: u128,
    pub kind: EventKind,
    pub message: String,
    pub data: Value,
}

impl AssistantEvent {
    pub fn new(kind: EventKind, message: impl Into<String>, data: Value) -> Self {
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        Self {
            id: Uuid::new_v4(),
            ts_ms,
            kind,
            message: message.into(),
            data,
        }
    }
}
