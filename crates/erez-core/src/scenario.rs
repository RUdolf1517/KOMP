use crate::{
    executor::{apply_slots_to_action, ActionError, ActionExecutor, ActionOutcome},
    normalize::normalize_phrase,
    plugins::{Action, Condition, PluginRegistry, Scenario, ScenarioStep},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ScenarioError {
    #[error("scenario not found: {plugin_id}/{scenario_id}")]
    NotFound {
        plugin_id: String,
        scenario_id: String,
    },
    #[error("scenario step not found: {0}")]
    StepNotFound(String),
    #[error("scenario loop detected at step: {0}")]
    LoopDetected(String),
    #[error(transparent)]
    Action(#[from] ActionError),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScenarioRun {
    pub plugin_id: String,
    pub scenario_id: String,
    pub dry_run: bool,
    pub steps: Vec<ScenarioStepResult>,
    pub slots: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScenarioStepResult {
    pub id: String,
    pub skipped: bool,
    pub success: bool,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct ScenarioContext {
    pub slots: HashMap<String, String>,
    pub last_reply: Option<String>,
    pub previous_success: Option<bool>,
}

pub trait ReplyProvider {
    fn next_reply(&mut self, prompt_slot: &str) -> Option<String>;
}

#[derive(Debug, Clone, Default)]
pub struct NoopReplyProvider;

impl ReplyProvider for NoopReplyProvider {
    fn next_reply(&mut self, _prompt_slot: &str) -> Option<String> {
        None
    }
}

#[derive(Debug, Clone)]
pub struct StaticReplyProvider {
    replies: Vec<String>,
}

impl StaticReplyProvider {
    pub fn new(replies: Vec<String>) -> Self {
        Self { replies }
    }

    pub fn is_empty(&self) -> bool {
        self.replies.is_empty()
    }
}

impl ReplyProvider for StaticReplyProvider {
    fn next_reply(&mut self, _prompt_slot: &str) -> Option<String> {
        if self.replies.is_empty() {
            None
        } else {
            Some(self.replies.remove(0))
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScenarioRunner {
    registry: PluginRegistry,
    executor: ActionExecutor,
    dry_run: bool,
}

impl ScenarioRunner {
    pub fn new(registry: PluginRegistry, executor: ActionExecutor) -> Self {
        Self {
            registry,
            executor,
            dry_run: false,
        }
    }

    pub fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    pub fn run(
        &self,
        plugin_id: &str,
        scenario_id: &str,
        initial_slots: HashMap<String, String>,
        reply_provider: &mut (impl ReplyProvider + ?Sized),
    ) -> Result<ScenarioRun, ScenarioError> {
        let (_, scenario) = self
            .registry
            .find_scenario(plugin_id, scenario_id)
            .ok_or_else(|| ScenarioError::NotFound {
                plugin_id: plugin_id.to_string(),
                scenario_id: scenario_id.to_string(),
            })?;
        let mut context = ScenarioContext {
            slots: initial_slots,
            ..ScenarioContext::default()
        };
        let mut run = ScenarioRun {
            plugin_id: plugin_id.to_string(),
            scenario_id: scenario_id.to_string(),
            dry_run: self.dry_run,
            steps: Vec::new(),
            slots: HashMap::new(),
        };
        let mut current = scenario.steps.first().map(|step| step.id.clone());
        let mut visited = HashSet::new();

        while let Some(step_id) = current {
            if !visited.insert(step_id.clone()) {
                return Err(ScenarioError::LoopDetected(step_id));
            }
            let step = find_step(scenario, &step_id)?;
            if !condition_matches(step.when.as_ref(), &context) {
                run.steps.push(ScenarioStepResult {
                    id: step.id.clone(),
                    skipped: true,
                    success: true,
                    message: "condition skipped".into(),
                });
                current = next_sequential_step(scenario, &step.id);
                continue;
            }

            let outcome = self.execute_step(step, &mut context, reply_provider);
            let success = outcome
                .as_ref()
                .map(|outcome| outcome.executed)
                .unwrap_or(false);
            let message = outcome
                .as_ref()
                .map(|outcome| outcome.message.clone())
                .unwrap_or_else(|err| err.to_string());
            context.previous_success = Some(success);
            run.steps.push(ScenarioStepResult {
                id: step.id.clone(),
                skipped: false,
                success,
                message,
            });

            current = if success {
                step.on_success
                    .clone()
                    .or_else(|| next_sequential_step(scenario, &step.id))
            } else {
                step.on_error
                    .clone()
                    .or_else(|| next_sequential_step(scenario, &step.id))
            };
        }

        let scenario_success = run
            .steps
            .iter()
            .filter(|step| !step.skipped)
            .all(|step| step.success);
        self.execute_final_scenario_sound(scenario, scenario_success, &mut run);

        run.slots = context.slots;
        Ok(run)
    }

    fn execute_final_scenario_sound(
        &self,
        scenario: &Scenario,
        scenario_success: bool,
        run: &mut ScenarioRun,
    ) {
        let (id, sound) = if scenario_success {
            ("scenario_success_sound", scenario.sounds.success.as_ref())
        } else {
            ("scenario_error_sound", scenario.sounds.error.as_ref())
        };
        let Some(sound) = sound else {
            return;
        };

        let outcome = self.execute_or_dry_run(&Action::PlaySound {
            file: sound.clone(),
        });
        let (success, message) = outcome
            .map(|outcome| (outcome.executed, outcome.message))
            .unwrap_or_else(|err| (false, err.to_string()));
        run.steps.push(ScenarioStepResult {
            id: id.into(),
            skipped: false,
            success,
            message,
        });
    }

    fn execute_step(
        &self,
        step: &ScenarioStep,
        context: &mut ScenarioContext,
        reply_provider: &mut (impl ReplyProvider + ?Sized),
    ) -> Result<ActionOutcome, ActionError> {
        if let Some(sound) = &step.before_sound {
            self.execute_or_dry_run(&Action::PlaySound {
                file: sound.clone(),
            })?;
        }

        let outcome = match &step.action {
            Action::Ask { sound, reply_slot } => {
                if let Some(sound) = sound {
                    self.execute_or_dry_run(&Action::PlaySound {
                        file: sound.clone(),
                    })?;
                }
                let reply = reply_provider.next_reply(reply_slot).unwrap_or_default();
                context.last_reply = Some(reply.clone());
                context.slots.insert(reply_slot.clone(), reply);
                Ok(ActionOutcome {
                    executed: true,
                    message: format!("stored reply in slot `{reply_slot}`"),
                })
            }
            Action::WaitForReply { reply_slot } => {
                let reply = reply_provider.next_reply(reply_slot).unwrap_or_default();
                context.last_reply = Some(reply.clone());
                context.slots.insert(reply_slot.clone(), reply);
                Ok(ActionOutcome {
                    executed: true,
                    message: format!("stored reply in slot `{reply_slot}`"),
                })
            }
            action => self.execute_or_dry_run_with_slots(action, &context.slots),
        }?;

        if outcome.executed {
            if let Some(sound) = &step.after_sound {
                self.execute_or_dry_run(&Action::PlaySound {
                    file: sound.clone(),
                })?;
            }
        }

        Ok(outcome)
    }

    fn execute_or_dry_run(&self, action: &Action) -> Result<ActionOutcome, ActionError> {
        self.execute_or_dry_run_with_slots(action, &HashMap::new())
    }

    fn execute_or_dry_run_with_slots(
        &self,
        action: &Action,
        slots: &HashMap<String, String>,
    ) -> Result<ActionOutcome, ActionError> {
        let action = apply_slots_to_action(action, slots);
        if self.dry_run {
            validate_action(&action)?;
            return Ok(ActionOutcome {
                executed: true,
                message: format!("dry-run: {action:?}"),
            });
        }
        self.executor.execute(&action)
    }
}

pub fn validate_action(action: &Action) -> Result<(), ActionError> {
    match action {
        Action::PlaySound { file } | Action::SaySound { file } => validate_sound_path(file),
        Action::SetVolume { level, delta } if level.is_none() && delta.is_none() => Err(
            ActionError::Invalid("set_volume requires `level` or `delta`".into()),
        ),
        _ => Ok(()),
    }
}

pub fn validate_sound_path(file: &str) -> Result<(), ActionError> {
    let path = Path::new(file);
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return Err(ActionError::Invalid("sound file needs an extension".into()));
    };
    match ext.to_ascii_lowercase().as_str() {
        "mp3" | "wav" | "ogg" => Ok(()),
        other => Err(ActionError::Invalid(format!(
            "unsupported sound extension: {other}"
        ))),
    }
}

fn find_step<'a>(scenario: &'a Scenario, step_id: &str) -> Result<&'a ScenarioStep, ScenarioError> {
    scenario
        .steps
        .iter()
        .find(|step| step.id == step_id)
        .ok_or_else(|| ScenarioError::StepNotFound(step_id.to_string()))
}

fn next_sequential_step(scenario: &Scenario, step_id: &str) -> Option<String> {
    let index = scenario.steps.iter().position(|step| step.id == step_id)?;
    scenario.steps.get(index + 1).map(|step| step.id.clone())
}

fn condition_matches(condition: Option<&Condition>, context: &ScenarioContext) -> bool {
    let Some(condition) = condition else {
        return true;
    };

    if let Some(expected) = condition.previous_success {
        if context.previous_success != Some(expected) {
            return false;
        }
    }

    if let Some(os) = &condition.os {
        if normalize_phrase(os) != normalize_phrase(std::env::consts::OS) {
            return false;
        }
    }

    if let Some(path) = &condition.file_exists {
        if !Path::new(path).exists() {
            return false;
        }
    }

    if let Some(app) = &condition.app_exists {
        if !app_exists(app) {
            return false;
        }
    }

    if let Some(reply_contains) = &condition.reply_contains {
        let reply = context.last_reply.as_deref().unwrap_or_default();
        if !normalize_phrase(reply).contains(&normalize_phrase(reply_contains)) {
            return false;
        }
    }

    if let Some(slot) = &condition.slot {
        let value = context
            .slots
            .get(slot)
            .map(String::as_str)
            .unwrap_or_default();
        if let Some(expected) = &condition.equals {
            if normalize_phrase(value) != normalize_phrase(expected) {
                return false;
            }
        }
        if let Some(contains) = &condition.contains {
            if !normalize_phrase(value).contains(&normalize_phrase(contains)) {
                return false;
            }
        }
    }

    true
}

fn app_exists(app: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        Path::new("/Applications")
            .join(format!("{app}.app"))
            .exists()
            || Path::new(app).exists()
    }
    #[cfg(target_os = "windows")]
    {
        Path::new(app).exists()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::process::Command::new("which")
            .arg(app)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{PluginManifest, ScenarioSounds};

    fn registry() -> PluginRegistry {
        PluginRegistry::from_manifests(vec![PluginManifest {
            id: "demo".into(),
            name: "Demo".into(),
            enabled: true,
            commands: vec![],
            scenarios: vec![Scenario {
                id: "branch".into(),
                aliases: vec!["branch".into()],
                patterns: vec![],
                priority: 0,
                sounds: ScenarioSounds::default(),
                steps: vec![
                    ScenarioStep {
                        id: "ask".into(),
                        when: None,
                        action: Action::Ask {
                            sound: None,
                            reply_slot: "browser".into(),
                        },
                        on_success: None,
                        on_error: None,
                        before_sound: None,
                        after_sound: None,
                    },
                    ScenarioStep {
                        id: "chrome".into(),
                        when: Some(Condition {
                            slot: Some("browser".into()),
                            contains: Some("chrome".into()),
                            equals: None,
                            os: None,
                            previous_success: None,
                            file_exists: None,
                            app_exists: None,
                            reply_contains: None,
                        }),
                        action: Action::EmitEvent {
                            event: "chrome".into(),
                            payload: serde_json::Value::Null,
                        },
                        on_success: None,
                        on_error: None,
                        before_sound: None,
                        after_sound: None,
                    },
                    ScenarioStep {
                        id: "safari".into(),
                        when: Some(Condition {
                            slot: Some("browser".into()),
                            contains: Some("safari".into()),
                            equals: None,
                            os: None,
                            previous_success: None,
                            file_exists: None,
                            app_exists: None,
                            reply_contains: None,
                        }),
                        action: Action::EmitEvent {
                            event: "safari".into(),
                            payload: serde_json::Value::Null,
                        },
                        on_success: None,
                        on_error: None,
                        before_sound: None,
                        after_sound: None,
                    },
                ],
            }],
        }])
    }

    #[test]
    fn ask_stores_reply_and_branches() {
        let runner = ScenarioRunner::new(registry(), ActionExecutor).dry_run(true);
        let mut replies = StaticReplyProvider::new(vec!["chrome пожалуйста".into()]);
        let run = runner
            .run("demo", "branch", HashMap::new(), &mut replies)
            .unwrap();
        assert_eq!(run.slots.get("browser").unwrap(), "chrome пожалуйста");
        assert!(!run.steps[1].skipped);
        assert!(run.steps[2].skipped);
    }

    #[test]
    fn scenario_success_sound_runs_after_successful_steps() {
        let mut manifest = registry().manifests().first().unwrap().clone();
        manifest.scenarios[0].sounds.success = Some("sounds/system/success.mp3".into());
        let registry = PluginRegistry::from_manifests(vec![manifest]);
        let runner = ScenarioRunner::new(registry, ActionExecutor).dry_run(true);
        let mut replies = StaticReplyProvider::new(vec!["chrome".into()]);
        let run = runner
            .run("demo", "branch", HashMap::new(), &mut replies)
            .unwrap();
        let final_step = run.steps.last().unwrap();
        assert_eq!(final_step.id, "scenario_success_sound");
        assert!(final_step.success);
    }

    #[test]
    fn validates_sound_extensions() {
        assert!(validate_sound_path("ok.mp3").is_ok());
        assert!(validate_sound_path("ok.wav").is_ok());
        assert!(validate_sound_path("ok.ogg").is_ok());
        assert!(validate_sound_path("ok.txt").is_err());
    }
}
