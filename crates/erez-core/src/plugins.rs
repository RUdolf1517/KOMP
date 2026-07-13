use crate::normalize::{fuzzy_phrase_score, normalize_phrase};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, fs, path::Path};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("failed to read plugin manifest {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid plugin manifest {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid command regex `{pattern}`: {source}")]
    Regex {
        pattern: String,
        #[source]
        source: regex::Error,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub commands: Vec<PluginCommand>,
    #[serde(default)]
    pub scenarios: Vec<Scenario>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginCommand {
    pub id: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub patterns: Vec<String>,
    #[serde(default)]
    pub slots: HashMap<String, String>,
    pub action: Action,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Scenario {
        plugin_id: String,
        scenario_id: String,
    },
    OpenApp {
        app: String,
    },
    SetVolume {
        #[serde(default)]
        level: Option<i32>,
        #[serde(default)]
        delta: Option<i32>,
    },
    MediaControl {
        command: String,
        #[serde(default)]
        seconds: Option<i32>,
    },
    PlaySound {
        file: String,
    },
    SaySound {
        file: String,
    },
    SayText {
        text: String,
        #[serde(default)]
        voice: Option<String>,
        #[serde(default = "default_tts_speed")]
        speed: f32,
        #[serde(default = "default_true")]
        cache: bool,
    },
    Ask {
        #[serde(default)]
        sound: Option<String>,
        #[serde(default)]
        text: Option<String>,
        reply_slot: String,
    },
    WaitForReply {
        reply_slot: String,
    },
    Shell {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        enabled: bool,
    },
    Hotkey {
        keys: Vec<String>,
    },
    Url {
        url: String,
    },
    HttpRequest {
        method: String,
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
        #[serde(default)]
        body: Option<Value>,
        #[serde(default)]
        response_slot: Option<String>,
        #[serde(default)]
        json_path: Option<String>,
        #[serde(default = "default_http_timeout_ms")]
        timeout_ms: u64,
    },
    ConvertCurrency {
        amount: String,
        from: String,
        to: String,
        #[serde(default = "default_result_slot")]
        result_slot: String,
        #[serde(default = "default_currency_api_url")]
        api_url: String,
    },
    Calculate {
        expression: String,
        #[serde(default = "default_result_slot")]
        result_slot: String,
    },
    Weather {
        #[serde(default)]
        location: String,
        #[serde(default)]
        fallback_location: String,
        #[serde(default = "default_weather_result_slot")]
        result_slot: String,
    },
    EmitEvent {
        event: String,
        #[serde(default)]
        payload: Value,
    },
}

fn default_tts_speed() -> f32 {
    1.0
}

fn default_true() -> bool {
    true
}

fn default_http_timeout_ms() -> u64 {
    10_000
}

fn default_result_slot() -> String {
    "result".into()
}

fn default_weather_result_slot() -> String {
    "weather".into()
}

fn default_currency_api_url() -> String {
    "https://open.er-api.com/v6/latest/{{from_code}}".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginMatch {
    pub plugin_id: String,
    pub command_id: String,
    pub action: Action,
    pub confidence: f32,
    pub slots: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Scenario {
    pub id: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub patterns: Vec<String>,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub sounds: ScenarioSounds,
    #[serde(default)]
    pub steps: Vec<ScenarioStep>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ScenarioSounds {
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
pub struct ScenarioStep {
    pub id: String,
    #[serde(default)]
    pub when: Option<Condition>,
    pub action: Action,
    #[serde(default)]
    pub on_success: Option<String>,
    #[serde(default)]
    pub on_error: Option<String>,
    #[serde(default)]
    pub before_sound: Option<String>,
    #[serde(default)]
    pub after_sound: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Condition {
    #[serde(default)]
    pub slot: Option<String>,
    #[serde(default)]
    pub equals: Option<String>,
    #[serde(default)]
    pub contains: Option<String>,
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub previous_success: Option<bool>,
    #[serde(default)]
    pub file_exists: Option<String>,
    #[serde(default)]
    pub app_exists: Option<String>,
    #[serde(default)]
    pub reply_contains: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PluginRegistry {
    manifests: Vec<PluginManifest>,
}

impl PluginRegistry {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_manifests(manifests: Vec<PluginManifest>) -> Self {
        Self { manifests }
    }

    pub fn load_dir(path: impl AsRef<Path>) -> Result<Self, PluginError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::empty());
        }

        let mut manifests = Vec::new();
        load_manifests_recursive(path, &mut manifests)?;

        Ok(Self { manifests })
    }

    pub fn load_manifest(path: impl AsRef<Path>) -> Result<PluginManifest, PluginError> {
        let path = path.as_ref();
        let content = fs::read_to_string(path).map_err(|source| PluginError::Read {
            path: path.display().to_string(),
            source,
        })?;
        toml::from_str(&content).map_err(|source| PluginError::Parse {
            path: path.display().to_string(),
            source,
        })
    }

    pub fn manifests(&self) -> &[PluginManifest] {
        &self.manifests
    }

    pub fn find_scenario(
        &self,
        plugin_id: &str,
        scenario_id: &str,
    ) -> Option<(&PluginManifest, &Scenario)> {
        self.manifests.iter().find_map(|manifest| {
            if manifest.id != plugin_id || !manifest.enabled {
                return None;
            }
            manifest
                .scenarios
                .iter()
                .find(|scenario| scenario.id == scenario_id)
                .map(|scenario| (manifest, scenario))
        })
    }

    pub fn find_match(&self, utterance: &str) -> Result<Option<PluginMatch>, PluginError> {
        let normalized = normalize_phrase(utterance);
        let mut best: Option<PluginMatch> = None;

        for manifest in self.manifests.iter().filter(|manifest| manifest.enabled) {
            for scenario in &manifest.scenarios {
                if let Some(plugin_match) = match_scenario(manifest, scenario, &normalized)? {
                    let replace = best
                        .as_ref()
                        .map(|current| should_replace_match(current, &plugin_match))
                        .unwrap_or(true);
                    if replace {
                        best = Some(plugin_match);
                    }
                }
            }

            for command in &manifest.commands {
                if let Some(plugin_match) = match_command(manifest, command, &normalized)? {
                    let replace = best
                        .as_ref()
                        .map(|current| should_replace_match(current, &plugin_match))
                        .unwrap_or(true);
                    if replace {
                        best = Some(plugin_match);
                    }
                }
            }
        }

        Ok(best)
    }
}

fn should_replace_match(current: &PluginMatch, next: &PluginMatch) -> bool {
    if next.confidence > current.confidence {
        return true;
    }
    let same_confidence = (next.confidence - current.confidence).abs() <= f32::EPSILON;
    same_confidence
        && next.plugin_id.starts_with("user_")
        && !current.plugin_id.starts_with("user_")
}

fn load_manifests_recursive(
    path: &Path,
    manifests: &mut Vec<PluginManifest>,
) -> Result<(), PluginError> {
    for entry in fs::read_dir(path).map_err(|source| PluginError::Read {
        path: path.display().to_string(),
        source,
    })? {
        let entry = entry.map_err(|source| PluginError::Read {
            path: path.display().to_string(),
            source,
        })?;
        let entry_path = entry.path();
        if entry_path.is_dir() {
            load_manifests_recursive(&entry_path, manifests)?;
        } else if entry_path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
            manifests.push(PluginRegistry::load_manifest(&entry_path)?);
        }
    }
    Ok(())
}

fn match_command(
    manifest: &PluginManifest,
    command: &PluginCommand,
    normalized: &str,
) -> Result<Option<PluginMatch>, PluginError> {
    for alias in &command.aliases {
        if let Some(score) = fuzzy_phrase_score(normalized, alias) {
            return Ok(Some(PluginMatch {
                plugin_id: manifest.id.clone(),
                command_id: command.id.clone(),
                action: command.action.clone(),
                confidence: score,
                slots: HashMap::new(),
            }));
        }
    }

    for pattern in &command.patterns {
        let regex = Regex::new(pattern).map_err(|source| PluginError::Regex {
            pattern: pattern.clone(),
            source,
        })?;
        if let Some(captures) = regex.captures(normalized) {
            let mut slots = HashMap::new();
            for name in regex.capture_names().flatten() {
                if let Some(value) = captures.name(name) {
                    slots.insert(name.to_string(), value.as_str().trim().to_string());
                }
            }
            return Ok(Some(PluginMatch {
                plugin_id: manifest.id.clone(),
                command_id: command.id.clone(),
                action: command.action.clone(),
                confidence: 0.92,
                slots,
            }));
        }
    }

    Ok(None)
}

fn match_scenario(
    manifest: &PluginManifest,
    scenario: &Scenario,
    normalized: &str,
) -> Result<Option<PluginMatch>, PluginError> {
    let priority_boost = (scenario.priority as f32 / 100.0).clamp(0.0, 0.1);
    for alias in &scenario.aliases {
        if let Some(score) = fuzzy_phrase_score(normalized, alias) {
            return Ok(Some(PluginMatch {
                plugin_id: manifest.id.clone(),
                command_id: scenario.id.clone(),
                action: Action::Scenario {
                    plugin_id: manifest.id.clone(),
                    scenario_id: scenario.id.clone(),
                },
                confidence: score + priority_boost,
                slots: HashMap::new(),
            }));
        }
    }

    for pattern in &scenario.patterns {
        let regex = Regex::new(pattern).map_err(|source| PluginError::Regex {
            pattern: pattern.clone(),
            source,
        })?;
        if let Some(captures) = regex.captures(normalized) {
            let mut slots = HashMap::new();
            for name in regex.capture_names().flatten() {
                if let Some(value) = captures.name(name) {
                    slots.insert(name.to_string(), value.as_str().trim().to_string());
                }
            }
            return Ok(Some(PluginMatch {
                plugin_id: manifest.id.clone(),
                command_id: scenario.id.clone(),
                action: Action::Scenario {
                    plugin_id: manifest.id.clone(),
                    scenario_id: scenario.id.clone(),
                },
                confidence: 0.94 + priority_boost,
                slots,
            }));
        }
    }

    Ok(None)
}

fn default_enabled() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_registry() -> PluginRegistry {
        PluginRegistry::from_manifests(vec![PluginManifest {
            id: "system".into(),
            name: "System".into(),
            enabled: true,
            commands: vec![
                PluginCommand {
                    id: "open_browser".into(),
                    aliases: vec!["открой браузер".into(), "open browser".into()],
                    patterns: vec![],
                    slots: HashMap::new(),
                    action: Action::OpenApp {
                        app: "browser".into(),
                    },
                },
                PluginCommand {
                    id: "search".into(),
                    aliases: vec![],
                    patterns: vec![r"^найди (?P<query>.+)$".into()],
                    slots: HashMap::new(),
                    action: Action::EmitEvent {
                        event: "search".into(),
                        payload: Value::Null,
                    },
                },
            ],
            scenarios: vec![Scenario {
                id: "browser_quieter".into(),
                aliases: vec!["включи браузер громкость ниже".into()],
                patterns: vec![],
                priority: 10,
                sounds: ScenarioSounds::default(),
                steps: vec![ScenarioStep {
                    id: "open".into(),
                    when: None,
                    action: Action::OpenApp {
                        app: "browser".into(),
                    },
                    on_success: None,
                    on_error: None,
                    before_sound: None,
                    after_sound: None,
                }],
            }],
        }])
    }

    #[test]
    fn matches_russian_alias() {
        let found = test_registry()
            .find_match("Эрез, открой браузер")
            .unwrap()
            .unwrap();
        assert_eq!(found.command_id, "open_browser");
        assert!(found.confidence > 0.8);
    }

    #[test]
    fn matches_english_alias() {
        let found = test_registry().find_match("open browser").unwrap().unwrap();
        assert_eq!(found.command_id, "open_browser");
    }

    #[test]
    fn fuzzy_matches_alias_with_small_recognition_error() {
        let found = test_registry()
            .find_match("эрез аткрой браузер")
            .unwrap()
            .unwrap();
        assert_eq!(found.command_id, "open_browser");
        assert!(found.confidence >= 0.78);
    }

    #[test]
    fn extracts_named_slots() {
        let found = test_registry()
            .find_match("найди погоду москва")
            .unwrap()
            .unwrap();
        assert_eq!(found.slots.get("query"), Some(&"погоду москва".to_string()));
    }

    #[test]
    fn bundled_user_voice_tools_extract_arguments() {
        let currency: PluginManifest = toml::from_str(include_str!(
            "../../../plugins.user/scenarios/currency_converter/scenario.toml"
        ))
        .unwrap();
        let calculator: PluginManifest = toml::from_str(include_str!(
            "../../../plugins.user/scenarios/calculator/scenario.toml"
        ))
        .unwrap();
        let registry = PluginRegistry::from_manifests(vec![currency, calculator]);

        let conversion = registry
            .find_match("сколько 100 евро в рублях")
            .unwrap()
            .unwrap();
        assert_eq!(conversion.command_id, "currency_converter");
        assert_eq!(
            conversion.slots.get("amount").map(String::as_str),
            Some("100")
        );
        assert_eq!(
            conversion.slots.get("from").map(String::as_str),
            Some("евро")
        );
        assert_eq!(
            conversion.slots.get("to").map(String::as_str),
            Some("рублях")
        );

        let calculation = registry
            .find_match("посчитай двенадцать умножить на пять")
            .unwrap()
            .unwrap();
        assert_eq!(calculation.command_id, "calculator");
        assert_eq!(
            calculation.slots.get("expression").map(String::as_str),
            Some("двенадцать умножить на пять")
        );

        let short_calculation = registry.find_match("два плюс два").unwrap().unwrap();
        assert_eq!(short_calculation.command_id, "calculator");
    }

    #[test]
    fn scenario_priority_wins_over_short_command_alias() {
        let found = test_registry()
            .find_match("эрез включи браузер громкость ниже")
            .unwrap()
            .unwrap();
        assert_eq!(found.command_id, "browser_quieter");
        assert!(matches!(found.action, Action::Scenario { .. }));
    }

    #[test]
    fn user_manifest_overrides_system_on_equal_confidence() {
        let system = PluginManifest {
            id: "system_minimize_window".into(),
            name: "System minimize".into(),
            enabled: true,
            commands: vec![],
            scenarios: vec![Scenario {
                id: "minimize_active_window".into(),
                aliases: vec!["сверни окно".into()],
                patterns: vec![],
                priority: 15,
                sounds: ScenarioSounds::default(),
                steps: vec![],
            }],
        };
        let user = PluginManifest {
            id: "user_minimize_active_window_copy".into(),
            name: "User minimize".into(),
            enabled: true,
            commands: vec![],
            scenarios: vec![Scenario {
                id: "minimize_active_window_copy".into(),
                aliases: vec!["сверни окно".into()],
                patterns: vec![],
                priority: 15,
                sounds: ScenarioSounds {
                    success: Some("ok.mp3".into()),
                    ..ScenarioSounds::default()
                },
                steps: vec![],
            }],
        };
        let found = PluginRegistry::from_manifests(vec![system, user])
            .find_match("сверни окно")
            .unwrap()
            .unwrap();
        assert_eq!(found.plugin_id, "user_minimize_active_window_copy");
        assert_eq!(found.command_id, "minimize_active_window_copy");
    }

    #[test]
    fn parses_toml_scenarios() {
        let manifest: PluginManifest = toml::from_str(
            r#"
id = "demo"
name = "Demo"

[[scenarios]]
id = "browser_quieter"
aliases = ["включи браузер громкость ниже"]
priority = 10

[[scenarios.steps]]
id = "sound"
action = { type = "play_sound", file = "sounds/system/listening.mp3" }

[[scenarios.steps]]
id = "volume"
when = { previous_success = true }
action = { type = "set_volume", delta = -15 }
"#,
        )
        .unwrap();
        assert_eq!(manifest.scenarios[0].steps.len(), 2);
        assert!(matches!(
            manifest.scenarios[0].steps[0].action,
            Action::PlaySound { .. }
        ));
    }
}
