pub mod audio;
pub mod config;
pub mod events;
pub mod executor;
pub mod intent;
pub mod lmstudio;
pub mod normalize;
pub mod pipeline;
pub mod plugins;
pub mod scenario;
pub mod stt;

pub use config::{ErezConfig, Language, LmStudioConfig, ModelConfig};
pub use events::{AssistantEvent, EventKind};
pub use executor::{apply_slots_to_action, ActionExecutor, ActionOutcome};
pub use intent::{
    DefaultIntentResolver, IntentRequest, IntentResolver, IntentResult, ResolvedAction,
};
pub use pipeline::{
    capture_and_transcribe_command, transcribe_command_preferred, transcribe_with_fallback,
    RecognizedCommand,
};
pub use plugins::{Action, PluginCommand, PluginManifest, PluginRegistry};
pub use scenario::{NoopReplyProvider, ScenarioRun, ScenarioRunner, StaticReplyProvider};
