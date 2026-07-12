use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use erez_core::{
    audio::{frame_duration_ms, read_wav_mono_i16, rms_i16},
    normalize::matches_wake_phrase,
    ActionExecutor, DefaultIntentResolver, ErezConfig, IntentRequest, IntentResolver, Language,
    PluginRegistry,
};
use std::{fs, path::PathBuf, time::Instant};

#[derive(Debug, Parser)]
#[command(name = "komp")]
#[command(about = "CLI tools for the KOMP offline voice assistant backend")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    InitConfig {
        #[arg(default_value = "komp.toml")]
        path: PathBuf,
    },
    WakeTest {
        text: String,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    PluginsValidate {
        path: PathBuf,
    },
    ScenariosValidate {
        path: PathBuf,
    },
    ScenarioRun {
        id: String,
        #[arg(long)]
        plugins: PathBuf,
        #[arg(long)]
        plugin_id: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long = "reply")]
        replies: Vec<String>,
    },
    SoundTest {
        path: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
    Resolve {
        text: String,
        #[arg(long)]
        plugins: Option<PathBuf>,
        #[arg(long)]
        no_lmstudio: bool,
    },
    WavInfo {
        path: PathBuf,
        #[arg(long, default_value_t = 16_000)]
        sample_rate: u32,
    },
    TranscribeWav {
        path: PathBuf,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, default_value = "ru")]
        language: LanguageArg,
    },
    WhisperWav {
        path: PathBuf,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        cli_path: Option<PathBuf>,
        #[arg(long)]
        model_path: Option<PathBuf>,
        #[arg(long, default_value = "ru")]
        language: LanguageArg,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum LanguageArg {
    Ru,
    En,
}

impl From<LanguageArg> for Language {
    fn from(value: LanguageArg) -> Self {
        match value {
            LanguageArg::Ru => Language::Ru,
            LanguageArg::En => Language::En,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        Command::InitConfig { path } => {
            let config = ErezConfig::default();
            fs::write(&path, config.to_toml_string()?)
                .with_context(|| format!("failed to write {}", path.display()))?;
            println!("wrote {}", path.display());
        }
        Command::WakeTest { text, config } => {
            let config = load_config(config)?;
            let matched = matches_wake_phrase(&text, &config.effective_wake_grammar());
            println!("{}", if matched { "wake_matched" } else { "no_match" });
        }
        Command::PluginsValidate { path } => {
            let registry = PluginRegistry::load_dir(&path)
                .with_context(|| format!("failed to load plugins from {}", path.display()))?;
            println!("valid: {} manifest(s)", registry.manifests().len());
        }
        Command::ScenariosValidate { path } => {
            let registry = PluginRegistry::load_dir(&path)
                .with_context(|| format!("failed to load plugins from {}", path.display()))?;
            let count = registry
                .manifests()
                .iter()
                .map(|manifest| manifest.scenarios.len())
                .sum::<usize>();
            println!(
                "valid: {} manifest(s), {} scenario(s)",
                registry.manifests().len(),
                count
            );
        }
        Command::ScenarioRun {
            id,
            plugins,
            plugin_id,
            dry_run,
            replies,
        } => {
            let registry = PluginRegistry::load_dir(&plugins)
                .with_context(|| format!("failed to load plugins from {}", plugins.display()))?;
            let plugin_id = plugin_id
                .or_else(|| {
                    registry
                        .manifests()
                        .iter()
                        .find(|manifest| {
                            manifest.scenarios.iter().any(|scenario| scenario.id == id)
                        })
                        .map(|manifest| manifest.id.clone())
                })
                .context("scenario not found in loaded plugins")?;
            let runner = erez_core::ScenarioRunner::new(registry, ActionExecutor::default())
                .dry_run(dry_run);
            let mut replies = erez_core::StaticReplyProvider::new(replies);
            let run = runner.run(&plugin_id, &id, Default::default(), &mut replies)?;
            println!("{}", serde_json::to_string_pretty(&run)?);
        }
        Command::SoundTest { path, dry_run } => {
            let action = erez_core::Action::PlaySound {
                file: path.display().to_string(),
            };
            if dry_run {
                erez_core::scenario::validate_action(&action)?;
                println!("valid sound: {}", path.display());
            } else {
                let outcome = ActionExecutor::default().execute(&action)?;
                println!("{}", serde_json::to_string_pretty(&outcome)?);
            }
        }
        Command::Resolve {
            text,
            plugins,
            no_lmstudio,
        } => {
            let registry = match plugins {
                Some(path) => PluginRegistry::load_dir(&path)
                    .with_context(|| format!("failed to load plugins from {}", path.display()))?,
                None => PluginRegistry::empty(),
            };
            let mut config = ErezConfig::default();
            if no_lmstudio {
                config.lmstudio.enabled = false;
            }
            let resolver = DefaultIntentResolver::new(registry, config.lmstudio);
            let result = resolver
                .resolve(IntentRequest {
                    utterance: text,
                    locale_hint: None,
                })
                .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            if let Some(resolved) = result.resolved {
                match ActionExecutor::default().execute(&resolved.action) {
                    Ok(outcome) => println!("{}", serde_json::to_string_pretty(&outcome)?),
                    Err(err) => println!("action_not_executed: {err}"),
                }
            }
        }
        Command::WavInfo { path, sample_rate } => {
            let frame = read_wav_mono_i16(&path, sample_rate)
                .with_context(|| format!("failed to read {}", path.display()))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "sample_rate_hz": frame.sample_rate_hz,
                    "samples": frame.samples_i16.len(),
                    "duration_ms": frame_duration_ms(&frame),
                    "rms": rms_i16(&frame.samples_i16)
                }))?
            );
        }
        Command::TranscribeWav {
            path,
            config,
            language,
        } => {
            transcribe_wav(path, config, language).await?;
        }
        Command::WhisperWav {
            path,
            config,
            cli_path,
            model_path,
            language,
        } => {
            whisper_wav(path, config, cli_path, model_path, language).await?;
        }
    }

    Ok(())
}

#[cfg(feature = "vosk-stt")]
async fn transcribe_wav(
    path: PathBuf,
    config: Option<PathBuf>,
    language: LanguageArg,
) -> Result<()> {
    use erez_core::stt::{vosk_backend::VoskSpeechRecognizer, SpeechRecognizer};

    let config = load_config(config)?;
    let frame = read_wav_mono_i16(&path, config.audio.sample_rate_hz)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let mut recognizer = VoskSpeechRecognizer::from_config(&config)?;
    let transcript = recognizer.transcribe(&frame.samples_i16, language.into())?;
    println!("{}", serde_json::to_string_pretty(&transcript)?);
    Ok(())
}

#[cfg(not(feature = "vosk-stt"))]
async fn transcribe_wav(
    _path: PathBuf,
    _config: Option<PathBuf>,
    _language: LanguageArg,
) -> Result<()> {
    anyhow::bail!("transcribe-wav requires building erez-cli with --features vosk-stt")
}

async fn whisper_wav(
    path: PathBuf,
    config: Option<PathBuf>,
    cli_path: Option<PathBuf>,
    model_path: Option<PathBuf>,
    language: LanguageArg,
) -> Result<()> {
    use erez_core::stt::{whisper_backend::WhisperCppRecognizer, SpeechRecognizer};

    let mut config = load_config(config)?;
    config.whisper.enabled = true;
    if cli_path.is_some() {
        config.whisper.cli_path = cli_path;
    }
    if model_path.is_some() {
        config.whisper.model_path = model_path;
    }
    config.whisper.language = Some(
        match language {
            LanguageArg::Ru => "ru",
            LanguageArg::En => "en",
        }
        .to_string(),
    );

    let frame = read_wav_mono_i16(&path, config.audio.sample_rate_hz)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let mut recognizer =
        WhisperCppRecognizer::from_config(&config)?.context("whisper.cpp is not configured")?;
    let started_at = Instant::now();
    let transcript = recognizer.transcribe(&frame.samples_i16, language.into())?;
    let latency_ms = started_at.elapsed().as_millis() as u64;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "transcript": transcript,
            "latency_ms": latency_ms
        }))?
    );
    Ok(())
}

fn load_config(path: Option<PathBuf>) -> Result<ErezConfig> {
    let Some(path) = path else {
        return Ok(ErezConfig::default());
    };
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(ErezConfig::from_toml_str(&content)?)
}
