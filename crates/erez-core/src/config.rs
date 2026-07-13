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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct XttsConfig {
    pub base_url: String,
    pub model: String,
    pub language: String,
    pub device: String,
    pub autostart: bool,
    pub preload: bool,
    pub timeout_ms: u64,
    pub license_accepted: bool,
}

impl Default for XttsConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:50010".into(),
            model: "tts_models/multilingual/multi-dataset/xtts_v2".into(),
            language: "ru".into(),
            device: "auto".into(),
            autostart: true,
            preload: true,
            timeout_ms: 120_000,
            license_accepted: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TtsConfig {
    pub enabled: bool,
    pub provider: String,
    pub base_url: String,
    pub model_path: PathBuf,
    pub voice_id: String,
    pub autostart: bool,
    pub preload: bool,
    pub timeout_ms: u64,
    pub cache_enabled: bool,
    pub device: String,
    #[serde(default = "default_tts_playback_mode")]
    pub playback_mode: String,
    #[serde(default)]
    pub xtts: XttsConfig,
}

fn default_tts_playback_mode() -> String {
    "buffered".into()
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "cosyvoice".into(),
            base_url: "http://127.0.0.1:50000".into(),
            model_path: PathBuf::from("vendor/cosyvoice/models/Fun-CosyVoice3-0.5B"),
            voice_id: "komp".into(),
            autostart: true,
            preload: true,
            timeout_ms: 180_000,
            cache_enabled: true,
            device: "auto".into(),
            playback_mode: default_tts_playback_mode(),
            xtts: XttsConfig::default(),
        }
    }
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
pub struct WakeConfig {
    pub min_confidence: f32,
    pub require_final: bool,
}

impl Default for WakeConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.84,
            require_final: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WeatherConfig {
    pub base_location: String,
}

impl Default for WeatherConfig {
    fn default() -> Self {
        Self {
            base_location: "Москва".into(),
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
            command_timeout_ms: 10_000,
            end_silence_ms: 1_200,
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
    #[serde(default)]
    pub wake: WakeConfig,
    #[serde(default)]
    pub weather: WeatherConfig,
    #[serde(default)]
    pub tts: TtsConfig,
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
            wake: WakeConfig::default(),
            weather: WeatherConfig::default(),
            tts: TtsConfig::default(),
            audio: AudioConfig::default(),
            sounds: SoundConfig::default(),
            plugin_dirs: vec![PathBuf::from("plugins.example")],
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

    #[test]
    fn old_configs_receive_disabled_cosyvoice_defaults() {
        let config: ErezConfig = toml::from_str(
            r#"
wake_phrase = "комп"
wake_phrases = []
wake_grammar = ["комп"]
primary_language = "ru"
english_fallback = true
plugin_dirs = []

[models]
ru_vosk_path = "ru"
en_vosk_path = "en"

[lmstudio]
enabled = false
base_url = "http://localhost:1234/v1"
model = "local"
timeout_ms = 2500
min_confidence = 0.55

[audio]
sample_rate_hz = 16000
command_timeout_ms = 10000
end_silence_ms = 1200
command_preroll_ms = 300
"#,
        )
        .unwrap();
        assert!(!config.tts.enabled);
        assert_eq!(config.tts.provider, "cosyvoice");
    }

    #[test]
    fn old_tts_configs_default_to_buffered_playback() {
        let config: TtsConfig = toml::from_str(
            r#"
enabled = true
provider = "cosyvoice"
base_url = "http://127.0.0.1:50000"
model_path = "model"
voice_id = "komp"
autostart = true
preload = true
timeout_ms = 180000
cache_enabled = true
device = "auto"
"#,
        )
        .unwrap();

        assert_eq!(config.playback_mode, "buffered");
        assert_eq!(config.xtts, XttsConfig::default());
    }

    #[test]
    fn xtts_config_round_trips_with_manual_provider_selection() {
        let mut config = TtsConfig::default();
        config.enabled = true;
        config.provider = "xtts".into();
        config.voice_id = "cave".into();
        config.playback_mode = "streaming".into();
        config.xtts.license_accepted = true;

        let encoded = toml::to_string(&config).unwrap();
        let decoded: TtsConfig = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded.provider, "xtts");
        assert_eq!(decoded.voice_id, "cave");
        assert!(decoded.xtts.license_accepted);
    }
}
