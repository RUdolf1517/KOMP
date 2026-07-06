use crate::{
    config::LmStudioConfig,
    lmstudio::{LmStudioClient, LmStudioError},
    plugins::{Action, PluginRegistry},
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IntentRequest {
    pub utterance: String,
    #[serde(default)]
    pub locale_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolvedAction {
    pub source: String,
    pub plugin_id: Option<String>,
    pub command_id: Option<String>,
    pub action: Action,
    pub confidence: f32,
    #[serde(default)]
    pub slots: HashMap<String, String>,
    #[serde(default)]
    pub speak: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IntentResult {
    pub utterance: String,
    pub resolved: Option<ResolvedAction>,
    #[serde(default)]
    pub fallback_error: Option<String>,
}

#[derive(Debug, Error)]
pub enum IntentError {
    #[error(transparent)]
    Plugin(#[from] crate::plugins::PluginError),
    #[error(transparent)]
    LmStudio(#[from] LmStudioError),
}

#[async_trait]
pub trait IntentResolver {
    async fn resolve(&self, request: IntentRequest) -> Result<IntentResult, IntentError>;
}

#[derive(Clone)]
pub struct DefaultIntentResolver {
    registry: PluginRegistry,
    lmstudio: Option<LmStudioClient>,
    lmstudio_config: LmStudioConfig,
}

impl DefaultIntentResolver {
    pub fn new(registry: PluginRegistry, lmstudio_config: LmStudioConfig) -> Self {
        let lmstudio = lmstudio_config
            .enabled
            .then(|| LmStudioClient::new(lmstudio_config.clone()));
        Self {
            registry,
            lmstudio,
            lmstudio_config,
        }
    }
}

#[async_trait]
impl IntentResolver for DefaultIntentResolver {
    async fn resolve(&self, request: IntentRequest) -> Result<IntentResult, IntentError> {
        if let Some(plugin_match) = self.registry.find_match(&request.utterance)? {
            let source = if matches!(plugin_match.action, crate::plugins::Action::Scenario { .. }) {
                "scenario"
            } else {
                "plugin"
            };
            return Ok(IntentResult {
                utterance: request.utterance,
                resolved: Some(ResolvedAction {
                    source: source.into(),
                    plugin_id: Some(plugin_match.plugin_id),
                    command_id: Some(plugin_match.command_id),
                    action: plugin_match.action,
                    confidence: plugin_match.confidence,
                    slots: plugin_match.slots,
                    speak: None,
                }),
                fallback_error: None,
            });
        }

        let Some(client) = &self.lmstudio else {
            return Ok(IntentResult {
                utterance: request.utterance,
                resolved: None,
                fallback_error: Some("lmstudio disabled".into()),
            });
        };

        match client.parse_intent(&request.utterance).await {
            Ok(parsed) if parsed.confidence >= self.lmstudio_config.min_confidence => {
                let action = Action::EmitEvent {
                    event: parsed.intent.clone(),
                    payload: Value::Object(parsed.arguments.clone().into_iter().collect()),
                };
                Ok(IntentResult {
                    utterance: request.utterance,
                    resolved: Some(ResolvedAction {
                        source: "lmstudio".into(),
                        plugin_id: None,
                        command_id: Some(parsed.intent),
                        action,
                        confidence: parsed.confidence,
                        slots: HashMap::new(),
                        speak: parsed.speak,
                    }),
                    fallback_error: None,
                })
            }
            Ok(parsed) => Ok(IntentResult {
                utterance: request.utterance,
                resolved: None,
                fallback_error: Some(format!("lmstudio low confidence: {}", parsed.confidence)),
            }),
            Err(err) => Ok(IntentResult {
                utterance: request.utterance,
                resolved: None,
                fallback_error: Some(err.to_string()),
            }),
        }
    }
}
