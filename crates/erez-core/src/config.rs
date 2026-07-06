use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Ru,
    En,
}

impl Default for Language {
    fn default() -> Self {
        Self::Ru
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelConfig {
    pub ru_vosk_path: Option<PathBuf>,
    pub en_vosk_path: Option<PathBuf>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            ru_vosk_path: None,
            en_vosk_path: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LmStudioConfig {
    pub enabled: bool,
    pub base_url: String,
    pub model: Option<String>,
    pub timeout_ms: u64,
    pub min_confidence: f32,
}

impl Default for LmStudioConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            base_url: "http://localhost:1234/v1".to_string(),
            model: None,
            timeout_ms: 2500,
            min_confidence: 0.55,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WhisperConfig {
    pub enabled: bool,
    pub cli_path: Option<PathBuf>,
    pub model_path: Option<PathBuf>,
    pub language: Option<String>,
    pub timeout_ms: u64,
    #[serde(default)]
    pub extra_args: Vec<String>,
}

impl Default for WhisperConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cli_path: None,
            model_path: None,
            language: Some("ru".to_string()),
            timeout_ms: 8_000,
            extra_args: vec!["-nt".to_string()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioConfig {
    pub sample_rate_hz: u32,
    pub command_timeout_ms: u64,
    pub end_silence_ms: u64,
    pub command_preroll_ms: u64,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate_hz: 16_000,
            command_timeout_ms: 5_000,
            end_silence_ms: 700,
            command_preroll_ms: 300,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SoundConfig {
    #[serde(default)]
    pub startup: Option<String>,
    #[serde(default)]
    pub shutdown: Option<String>,
    #[serde(default)]
    pub wake: Option<String>,
    #[serde(default)]
    pub listening: Option<String>,
    #[serde(default)]
    pub success: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub timeout: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ErezConfig {
    pub wake_phrase: String,
    #[serde(default)]
    pub wake_phrases: Vec<String>,
    pub wake_grammar: Vec<String>,
    pub primary_language: Language,
    pub english_fallback: bool,
    pub models: ModelConfig,
    pub lmstudio: LmStudioConfig,
    #[serde(default)]
    pub whisper: WhisperConfig,
    pub audio: AudioConfig,
    #[serde(default)]
    pub sounds: SoundConfig,
    pub plugin_dirs: Vec<PathBuf>,
}

impl Default for ErezConfig {
    fn default() -> Self {
        Self {
            wake_phrase: "комп".to_string(),
            wake_phrases: vec!["комп".to_string(), "компьютер".to_string()],
            wake_grammar: vec!["комп".to_string(), "компьютер".to_string()],
            primary_language: Language::Ru,
            english_fallback: true,
            models: ModelConfig::default(),
            lmstudio: LmStudioConfig::default(),
            whisper: WhisperConfig::default(),
            audio: AudioConfig::default(),
            sounds: SoundConfig::default(),
            plugin_dirs: vec![PathBuf::from("plugins")],
        }
    }
}

impl ErezConfig {
    pub fn from_toml_str(input: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(input)
    }

    pub fn to_toml_string(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    pub fn effective_wake_grammar(&self) -> Vec<String> {
        let mut grammar = self.wake_grammar.clone();
        let phrases = if self.wake_phrases.is_empty() {
            vec![self.wake_phrase.clone()]
        } else {
            self.wake_phrases.clone()
        };
        for phrase in phrases {
            let normalized = crate::normalize::normalize_phrase(&phrase);
            if !normalized.is_empty() && !grammar.iter().any(|item| item == &normalized) {
                grammar.push(normalized.clone());
            }
            if normalized == "эрез" && !grammar.iter().any(|item| item == "ерез") {
                grammar.push("ерез".to_string());
            }
        }
        grammar
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::matches_wake_phrase;

    #[test]
    fn effective_wake_grammar_includes_multiple_phrases_and_legacy_grammar() {
        let mut config = ErezConfig::default();
        config.wake_phrases = vec![
            "Эрез".into(),
            "Джарвис".into(),
            "компьютер".into(),
            "комп".into(),
        ];
        config.wake_grammar = vec!["эй рез".into()];
        let grammar = config.effective_wake_grammar();

        assert!(matches_wake_phrase("джарвис открой браузер", &grammar));
        assert!(matches_wake_phrase("компьютер открой браузер", &grammar));
        assert!(matches_wake_phrase("комп открой браузер", &grammar));
        assert!(matches_wake_phrase("эй рез открой браузер", &grammar));
        assert!(matches_wake_phrase("эрез открой браузер", &grammar));
        assert!(grammar.iter().any(|phrase| phrase == "ерез"));
    }
}
