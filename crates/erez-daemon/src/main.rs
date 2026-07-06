use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    http::StatusCode,
    response::{sse::Event, IntoResponse, Sse},
    routing::{get, post},
    Json, Router,
};
use erez_core::Action;
use erez_core::{
    apply_slots_to_action, ActionExecutor, AssistantEvent, DefaultIntentResolver, ErezConfig,
    EventKind, IntentRequest, IntentResolver, IntentResult, NoopReplyProvider, PluginRegistry,
    ScenarioRunner, StaticReplyProvider,
};
use futures_util::{SinkExt, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    convert::Infallible,
    fs,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
#[cfg(all(feature = "live-audio", feature = "vosk-stt"))]
use tokio::sync::mpsc;
use tokio::sync::{broadcast, RwLock};
use tower_http::cors::CorsLayer;
use tracing::{error, info};

const AUDIO_PLAYBACK_INITIAL_GUARD_MS: u64 = 3_000;
const AUDIO_PLAYBACK_SETTLE_GUARD_MS: u64 = 700;
const SCENARIO_AUDIO_GUARD_MS: u64 = 8_000;

#[derive(Clone)]
struct AppState {
    config: Arc<RwLock<ErezConfig>>,
    registry: Arc<RwLock<PluginRegistry>>,
    events: broadcast::Sender<AssistantEvent>,
    executor: ActionExecutor,
    listener: Arc<RwLock<Option<LiveListenerControl>>>,
    audio_playback_until_ms: Arc<AtomicU64>,
}

#[derive(Clone)]
struct LiveListenerControl {
    stop: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
}

#[derive(Debug, Deserialize)]
struct ListenOnceRequest {
    transcript: Option<String>,
    #[serde(default)]
    replies: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ModelsResponse {
    ru_vosk_path: Option<String>,
    en_vosk_path: Option<String>,
    wake_phrases: Vec<String>,
    wake_grammar: Vec<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "komp_daemon=info,erez_daemon=info,erez_core=info".into()),
        )
        .init();

    let config = load_initial_config();
    let startup_sound = config.sounds.startup.clone();
    let registry = load_registry(&config);
    let (events, _) = broadcast::channel(256);
    let state = AppState {
        config: Arc::new(RwLock::new(config)),
        registry: Arc::new(RwLock::new(registry)),
        events,
        executor: ActionExecutor,
        listener: Arc::new(RwLock::new(None)),
        audio_playback_until_ms: Arc::new(AtomicU64::new(0)),
    };

    emit(
        &state,
        EventKind::Status,
        "KOMP daemon started",
        json!({ "mode": "api_only" }),
    );
    play_configured_sound_blocking(
        "startup",
        startup_sound.as_deref(),
        Some(state.audio_playback_until_ms.as_ref()),
    );
    maybe_autostart_listener(state.clone());

    let app = build_router(state.clone());

    let addr: SocketAddr = std::env::var("KOMP_BIND")
        .or_else(|_| std::env::var("EREZ_BIND"))
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 3737)));

    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("KOMP daemon listening on http://{addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(state))
        .await?;
    Ok(())
}

async fn shutdown_signal(state: AppState) {
    if tokio::signal::ctrl_c().await.is_ok() {
        emit(
            &state,
            EventKind::Status,
            "KOMP daemon shutting down",
            json!({}),
        );
        let config = state.config.read().await.clone();
        play_configured_sound_blocking(
            "shutdown",
            config.sounds.shutdown.as_deref(),
            Some(state.audio_playback_until_ms.as_ref()),
        );
    }
}

fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/events", get(events_sse))
        .route("/events/ws", get(events_ws))
        .route("/commands/reload", post(commands_reload))
        .route("/listen/once", post(listen_once))
        .route("/listen/live", post(listen_live))
        .route("/listen/start", post(listen_start))
        .route("/listen/stop", post(listen_stop))
        .route("/config", post(update_config))
        .route("/models", get(models))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

fn load_initial_config() -> ErezConfig {
    let path = std::env::var_os("KOMP_CONFIG")
        .or_else(|| std::env::var_os("EREZ_CONFIG"))
        .map(PathBuf::from)
        .or_else(|| {
            let path = PathBuf::from("komp.toml");
            path.exists().then_some(path)
        })
        .or_else(|| {
            let path = PathBuf::from("erez.toml");
            path.exists().then_some(path)
        });

    let Some(path) = path else {
        return ErezConfig::default();
    };

    match fs::read_to_string(&path)
        .ok()
        .and_then(|content| ErezConfig::from_toml_str(&content).ok())
    {
        Some(config) => {
            info!("loaded config from {}", path.display());
            config
        }
        None => {
            error!(
                "failed to load config from {}, using defaults",
                path.display()
            );
            ErezConfig::default()
        }
    }
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn models(State(state): State<AppState>) -> Json<ModelsResponse> {
    let config = state.config.read().await;
    Json(ModelsResponse {
        ru_vosk_path: config
            .models
            .ru_vosk_path
            .as_ref()
            .map(|path| path.display().to_string()),
        en_vosk_path: config
            .models
            .en_vosk_path
            .as_ref()
            .map(|path| path.display().to_string()),
        wake_phrases: config.wake_phrases.clone(),
        wake_grammar: config.effective_wake_grammar(),
    })
}

async fn commands_reload(State(state): State<AppState>) -> impl IntoResponse {
    let config = state.config.read().await.clone();
    let registry = load_registry(&config);
    let count = registry.manifests().len();
    *state.registry.write().await = registry;
    emit(
        &state,
        EventKind::Status,
        "commands reloaded",
        json!({ "plugin_count": count }),
    );
    Json(json!({ "ok": true, "plugin_count": count }))
}

async fn update_config(
    State(state): State<AppState>,
    Json(config): Json<ErezConfig>,
) -> impl IntoResponse {
    *state.config.write().await = config.clone();
    *state.registry.write().await = load_registry(&config);
    emit(&state, EventKind::Status, "config updated", json!({}));
    Json(config)
}

async fn listen_once(
    State(state): State<AppState>,
    Json(request): Json<ListenOnceRequest>,
) -> impl IntoResponse {
    let Some(transcript) = request.transcript else {
        let event = emit(
            &state,
            EventKind::Error,
            "live microphone capture is not enabled in this v1 build",
            json!({ "reason": "transcript_required" }),
        );
        return (StatusCode::NOT_IMPLEMENTED, Json(json!({ "event": event })));
    };

    process_transcript_with_replies(state, transcript, request.replies).await
}

#[cfg(all(feature = "live-audio", feature = "vosk-stt"))]
async fn listen_live(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let config = state.config.read().await.clone();
    match capture_live_transcript(&config) {
        Ok(command) => {
            emit(
                &state,
                EventKind::SpeechRecognized,
                "speech recognized from microphone",
                json!({
                    "transcript": command.transcript.text,
                    "language": command.transcript.language,
                    "confidence": command.transcript.confidence,
                    "audio_samples": command.audio_samples
                }),
            );
            process_transcript(state, command.transcript.text).await
        }
        Err(err) => {
            let event = emit(
                &state,
                EventKind::Error,
                "live listen failed",
                json!({ "error": err.to_string() }),
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "event": event })),
            );
        }
    }
}

#[cfg(all(feature = "live-audio", feature = "vosk-stt"))]
async fn listen_start(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    start_live_listener(state).await
}

#[cfg(all(feature = "live-audio", feature = "vosk-stt"))]
async fn start_live_listener(state: AppState) -> (StatusCode, Json<Value>) {
    match start_live_listener_inner(state).await {
        Ok(running) => (
            StatusCode::OK,
            Json(json!({ "ok": true, "running": running })),
        ),
        Err(event) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "event": event })),
        ),
    }
}

#[cfg(all(feature = "live-audio", feature = "vosk-stt"))]
async fn start_live_listener_inner(state: AppState) -> Result<bool, AssistantEvent> {
    {
        let mut listener = state.listener.write().await;
        if let Some(control) = listener.as_ref() {
            if control.running.load(Ordering::SeqCst) {
                return Ok(true);
            }
            *listener = None;
        }
    }

    let stop = Arc::new(AtomicBool::new(false));
    let running = Arc::new(AtomicBool::new(true));
    let config = state.config.read().await.clone();
    let events = state.events.clone();
    let (transcripts_tx, mut transcripts_rx) = mpsc::channel::<String>(16);
    let worker_stop = stop.clone();
    let worker_running = running.clone();
    let audio_playback_until_ms = state.audio_playback_until_ms.clone();

    if let Err(err) = std::thread::Builder::new()
        .name("erez-live-listener".into())
        .spawn(move || {
            run_live_listener(
                config,
                events,
                transcripts_tx,
                worker_stop,
                audio_playback_until_ms,
            );
            worker_running.store(false, Ordering::SeqCst);
        })
    {
        let event = emit(
            &state,
            EventKind::Error,
            "failed to start live listener",
            json!({ "error": err.to_string() }),
        );
        return Err(event);
    }

    let processor_state = state.clone();
    let runtime = tokio::runtime::Handle::current();
    if let Err(err) = std::thread::Builder::new()
        .name("erez-transcript-processor".into())
        .spawn(move || {
            while let Some(transcript) = transcripts_rx.blocking_recv() {
                let _ = runtime.block_on(process_transcript(processor_state.clone(), transcript));
            }
        })
    {
        let event = emit(
            &state,
            EventKind::Error,
            "failed to start transcript processor",
            json!({ "error": err.to_string() }),
        );
        return Err(event);
    }

    *state.listener.write().await = Some(LiveListenerControl { stop, running });
    emit(
        &state,
        EventKind::Status,
        "live listener started",
        json!({ "mode": "wake_loop" }),
    );
    Ok(true)
}

#[cfg(not(all(feature = "live-audio", feature = "vosk-stt")))]
async fn listen_start(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let event = emit(
        &state,
        EventKind::Error,
        "always-on listener requires daemon feature `live-vosk`",
        json!({ "required_features": ["live-audio", "vosk-stt"] }),
    );
    (StatusCode::NOT_IMPLEMENTED, Json(json!({ "event": event })))
}

async fn listen_stop(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    stop_live_listener(&state, true).await;
    (
        StatusCode::OK,
        Json(json!({ "ok": true, "running": false })),
    )
}

async fn stop_live_listener(state: &AppState, play_shutdown_sound: bool) -> bool {
    let stopped = {
        let mut listener = state.listener.write().await;
        if let Some(control) = listener.take() {
            control.stop.store(true, Ordering::SeqCst);
            control.running.store(false, Ordering::SeqCst);
            true
        } else {
            false
        }
    };

    if stopped {
        emit(
            state,
            EventKind::Status,
            "live listener stopping",
            json!({}),
        );
        if play_shutdown_sound {
            let config = state.config.read().await.clone();
            play_configured_sound_async(
                "shutdown",
                config.sounds.shutdown.as_deref(),
                state.audio_playback_until_ms.clone(),
            );
        }
    }

    stopped
}

fn maybe_autostart_listener(state: AppState) {
    let autostart = std::env::var("KOMP_AUTOSTART")
        .or_else(|_| std::env::var("EREZ_AUTOSTART"))
        .ok();
    if autostart.as_deref() != Some("1") {
        return;
    }

    #[cfg(all(feature = "live-audio", feature = "vosk-stt"))]
    tokio::spawn(async move {
        let (status, body) = start_live_listener(state).await;
        info!(%status, response = %body.0, "autostart live listener requested");
    });

    #[cfg(not(all(feature = "live-audio", feature = "vosk-stt")))]
    {
        emit(
            &state,
            EventKind::Error,
            "KOMP_AUTOSTART requires daemon feature `live-vosk`",
            json!({ "required_features": ["live-audio", "vosk-stt"] }),
        );
    }
}

#[cfg(all(feature = "live-audio", feature = "vosk-stt"))]
fn capture_live_transcript(
    config: &ErezConfig,
) -> Result<erez_core::RecognizedCommand, erez_core::pipeline::PipelineError> {
    let mut source = erez_core::audio::cpal_capture::CpalAudioSource::default_input(
        config.audio.sample_rate_hz,
    )?;
    let mut recognizer = erez_core::stt::vosk_backend::VoskSpeechRecognizer::from_config(config)?;
    erez_core::capture_and_transcribe_command(&mut source, &mut recognizer, config)
}

#[cfg(all(feature = "live-audio", feature = "vosk-stt"))]
fn run_live_listener(
    config: ErezConfig,
    events: broadcast::Sender<AssistantEvent>,
    transcripts: mpsc::Sender<String>,
    stop: Arc<AtomicBool>,
    audio_playback_until_ms: Arc<AtomicU64>,
) {
    use erez_core::{
        audio::{
            collect_command_audio, cpal_capture::CpalAudioSource, AudioSource, VoiceActivityConfig,
        },
        stt::vosk_backend::VoskSpeechRecognizer,
        transcribe_command_preferred,
    };

    let mut source = match CpalAudioSource::default_input(config.audio.sample_rate_hz) {
        Ok(source) => source,
        Err(err) => {
            let _ = events.send(AssistantEvent::new(
                EventKind::Error,
                "microphone capture failed",
                json!({ "error": err.to_string() }),
            ));
            return;
        }
    };
    let mut recognizer = match VoskSpeechRecognizer::from_config(&config) {
        Ok(recognizer) => recognizer,
        Err(err) => {
            let _ = events.send(AssistantEvent::new(
                EventKind::Error,
                "vosk recognizer initialization failed",
                json!({ "error": err.to_string() }),
            ));
            return;
        }
    };
    let wake_grammar = config.effective_wake_grammar();
    let mut wake = match recognizer.wake_detector(&wake_grammar) {
        Ok(wake) => wake,
        Err(err) => {
            let _ = events.send(AssistantEvent::new(
                EventKind::Error,
                "wake recognizer initialization failed",
                json!({ "error": err.to_string() }),
            ));
            return;
        }
    };
    let vad = VoiceActivityConfig {
        end_silence_ms: config.audio.end_silence_ms,
        max_duration_ms: config.audio.command_timeout_ms,
        preroll_ms: config.audio.command_preroll_ms,
        ..VoiceActivityConfig::default()
    };

    while !stop.load(Ordering::SeqCst) {
        let frame = match source.next_frame() {
            Ok(Some(frame)) => frame,
            Ok(None) => continue,
            Err(err) => {
                let _ = events.send(AssistantEvent::new(
                    EventKind::Error,
                    "audio stream failed",
                    json!({ "error": err.to_string() }),
                ));
                break;
            }
        };

        if is_audio_playback_active(audio_playback_until_ms.as_ref()) {
            wake.reset();
            continue;
        }

        let wake_text = match wake.accept_frame(&frame.samples_i16) {
            Ok(wake_text) => wake_text,
            Err(err) => {
                let _ = events.send(AssistantEvent::new(
                    EventKind::Error,
                    "wake recognizer failed",
                    json!({ "error": err.to_string() }),
                ));
                continue;
            }
        };
        let Some(wake_text) = wake_text else {
            continue;
        };

        info!(wake_text = %wake_text, "wake phrase detected");
        let _ = events.send(AssistantEvent::new(
            EventKind::WakeDetected,
            "wake phrase detected",
            json!({ "text": wake_text }),
        ));
        play_wake_feedback_sounds(
            config.sounds.wake.as_deref(),
            config.sounds.listening.as_deref(),
            audio_playback_until_ms.as_ref(),
        );
        if let Err(err) = drain_audio_playback(&mut source, audio_playback_until_ms.as_ref(), &stop)
        {
            let _ = events.send(AssistantEvent::new(
                EventKind::Error,
                "audio playback drain failed",
                json!({ "error": err.to_string() }),
            ));
            continue;
        }

        let audio =
            match collect_command_audio(&mut source, vad.clone(), config.audio.sample_rate_hz) {
                Ok(audio) => audio,
                Err(err) => {
                    let _ = events.send(AssistantEvent::new(
                        EventKind::Error,
                        "command capture failed",
                        json!({ "error": err.to_string() }),
                    ));
                    continue;
                }
            };

        match transcribe_command_preferred(&mut recognizer, &audio, &config) {
            Ok(transcript) if !transcript.text.trim().is_empty() => {
                info!(
                    text = %transcript.text,
                    language = ?transcript.language,
                    confidence = transcript.confidence,
                    "speech recognized after wake"
                );
                let _ = transcripts.blocking_send(transcript.text);
            }
            Ok(_) => {
                info!("empty command after wake phrase");
                let _ = events.send(AssistantEvent::new(
                    EventKind::CommandUnrecognized,
                    "empty command after wake phrase",
                    json!({}),
                ));
            }
            Err(err) => {
                let _ = events.send(AssistantEvent::new(
                    EventKind::Error,
                    "command transcription failed",
                    json!({ "error": err.to_string() }),
                ));
            }
        }
    }
    let _ = events.send(AssistantEvent::new(
        EventKind::Status,
        "live listener stopped",
        json!({}),
    ));
}

#[cfg(all(feature = "live-audio", feature = "vosk-stt"))]
fn drain_audio_playback(
    source: &mut impl erez_core::audio::AudioSource,
    audio_playback_until_ms: &AtomicU64,
    stop: &AtomicBool,
) -> Result<(), erez_core::audio::AudioError> {
    while !stop.load(Ordering::SeqCst) && is_audio_playback_active(audio_playback_until_ms) {
        let _ = source.next_frame()?;
    }
    Ok(())
}

fn play_wake_feedback_sounds(wake: Option<&str>, listening: Option<&str>, guard: &AtomicU64) {
    let wake = non_empty_sound(wake);
    let listening = non_empty_sound(listening);

    if let Some(wake) = wake {
        play_configured_sound_blocking("wake", Some(wake), Some(guard));
    }
    if let Some(listening) = listening {
        if wake != Some(listening) {
            play_configured_sound_blocking("listening", Some(listening), Some(guard));
        }
    }
}

fn non_empty_sound(file: Option<&str>) -> Option<&str> {
    file.map(str::trim).filter(|file| !file.is_empty())
}

fn play_configured_sound_async(
    label: &'static str,
    file: Option<&str>,
    audio_playback_until_ms: Arc<AtomicU64>,
) {
    let Some(file) = file.filter(|file| !file.trim().is_empty()) else {
        return;
    };
    let file = file.to_string();
    mark_audio_playback(
        Some(audio_playback_until_ms.as_ref()),
        AUDIO_PLAYBACK_INITIAL_GUARD_MS,
    );
    std::thread::spawn(move || {
        match ActionExecutor.execute(&Action::PlaySound { file: file.clone() }) {
            Ok(outcome) => info!(
                sound = %file,
                message = %outcome.message,
                "played configured {label} sound"
            ),
            Err(err) => error!(
                sound = %file,
                error = %err,
                "failed to play configured {label} sound"
            ),
        }
        mark_audio_playback(
            Some(audio_playback_until_ms.as_ref()),
            AUDIO_PLAYBACK_SETTLE_GUARD_MS,
        );
    });
}

fn play_configured_sound_blocking(label: &str, file: Option<&str>, guard: Option<&AtomicU64>) {
    let Some(file) = file.filter(|file| !file.trim().is_empty()) else {
        return;
    };
    mark_audio_playback(guard, AUDIO_PLAYBACK_INITIAL_GUARD_MS);
    match ActionExecutor.execute(&Action::PlaySound {
        file: file.to_string(),
    }) {
        Ok(outcome) => info!(
            sound = %file,
            message = %outcome.message,
            "played configured {label} sound"
        ),
        Err(err) => error!(
            sound = %file,
            error = %err,
            "failed to play configured {label} sound"
        ),
    }
    mark_audio_playback(guard, AUDIO_PLAYBACK_SETTLE_GUARD_MS);
}

fn mark_audio_playback(guard: Option<&AtomicU64>, guard_ms: u64) {
    let Some(guard) = guard else {
        return;
    };
    guard.store(now_ms().saturating_add(guard_ms), Ordering::SeqCst);
}

#[allow(dead_code)]
fn is_audio_playback_active(guard: &AtomicU64) -> bool {
    now_ms() < guard.load(Ordering::SeqCst)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

#[cfg(not(all(feature = "live-audio", feature = "vosk-stt")))]
async fn listen_live(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let event = emit(
        &state,
        EventKind::Error,
        "live microphone recognition requires daemon feature `live-vosk`",
        json!({ "required_features": ["live-audio", "vosk-stt"] }),
    );
    (StatusCode::NOT_IMPLEMENTED, Json(json!({ "event": event })))
}

#[allow(dead_code)]
async fn process_transcript(state: AppState, transcript: String) -> (StatusCode, Json<Value>) {
    process_transcript_with_replies(state, transcript, Vec::new()).await
}

async fn process_transcript_with_replies(
    state: AppState,
    transcript: String,
    replies: Vec<String>,
) -> (StatusCode, Json<Value>) {
    emit(
        &state,
        EventKind::SpeechRecognized,
        "speech recognized",
        json!({ "transcript": transcript }),
    );

    let config = state.config.read().await.clone();
    let registry = state.registry.read().await.clone();
    let resolver = DefaultIntentResolver::new(registry, config.lmstudio);
    let result = resolver
        .resolve(IntentRequest {
            utterance: transcript,
            locale_hint: None,
        })
        .await;

    process_intent_result(&state, result, replies).await
}

async fn process_intent_result(
    state: &AppState,
    result: Result<IntentResult, erez_core::intent::IntentError>,
    replies: Vec<String>,
) -> (StatusCode, Json<Value>) {
    match result {
        Ok(result) => {
            if let Some(resolved) = &result.resolved {
                emit(
                    &state,
                    EventKind::IntentResolved,
                    "intent resolved",
                    serde_json::to_value(resolved).unwrap_or(Value::Null),
                );
                if let erez_core::Action::Scenario {
                    plugin_id,
                    scenario_id,
                } = &resolved.action
                {
                    let registry = state.registry.read().await.clone();
                    let runner = ScenarioRunner::new(registry, state.executor.clone());
                    let mut static_replies = StaticReplyProvider::new(replies);
                    let mut no_replies = NoopReplyProvider;
                    let reply_provider: &mut dyn erez_core::scenario::ReplyProvider =
                        if static_replies.is_empty() {
                            &mut no_replies
                        } else {
                            &mut static_replies
                        };
                    mark_audio_playback(
                        Some(state.audio_playback_until_ms.as_ref()),
                        SCENARIO_AUDIO_GUARD_MS,
                    );
                    match runner.run(
                        plugin_id,
                        scenario_id,
                        resolved.slots.clone(),
                        reply_provider,
                    ) {
                        Ok(run) => {
                            mark_audio_playback(
                                Some(state.audio_playback_until_ms.as_ref()),
                                AUDIO_PLAYBACK_SETTLE_GUARD_MS,
                            );
                            for step in &run.steps {
                                info!(
                                    scenario_id = %run.scenario_id,
                                    step_id = %step.id,
                                    skipped = step.skipped,
                                    success = step.success,
                                    message = %step.message,
                                    "scenario step"
                                );
                                emit(
                                    &state,
                                    EventKind::ActionExecuted,
                                    "scenario step executed",
                                    serde_json::to_value(step).unwrap_or(Value::Null),
                                );
                            }
                            emit(
                                &state,
                                EventKind::ActionExecuted,
                                "scenario executed",
                                serde_json::to_value(run).unwrap_or(Value::Null),
                            );
                            handle_system_scenario_effects(state, scenario_id).await;
                        }
                        Err(err) => {
                            mark_audio_playback(
                                Some(state.audio_playback_until_ms.as_ref()),
                                AUDIO_PLAYBACK_SETTLE_GUARD_MS,
                            );
                            emit(
                                &state,
                                EventKind::Error,
                                "scenario execution failed",
                                json!({ "error": err.to_string() }),
                            );
                        }
                    }
                } else {
                    let action = apply_slots_to_action(&resolved.action, &resolved.slots);
                    if matches!(action, Action::PlaySound { .. } | Action::SaySound { .. }) {
                        mark_audio_playback(
                            Some(state.audio_playback_until_ms.as_ref()),
                            AUDIO_PLAYBACK_INITIAL_GUARD_MS,
                        );
                    }
                    match state.executor.execute(&action) {
                        Ok(outcome) => {
                            if matches!(action, Action::PlaySound { .. } | Action::SaySound { .. })
                            {
                                mark_audio_playback(
                                    Some(state.audio_playback_until_ms.as_ref()),
                                    AUDIO_PLAYBACK_SETTLE_GUARD_MS,
                                );
                            }
                            emit(
                                &state,
                                EventKind::ActionExecuted,
                                "action executed",
                                serde_json::to_value(outcome).unwrap_or(Value::Null),
                            );
                        }
                        Err(err) => {
                            if matches!(action, Action::PlaySound { .. } | Action::SaySound { .. })
                            {
                                mark_audio_playback(
                                    Some(state.audio_playback_until_ms.as_ref()),
                                    AUDIO_PLAYBACK_SETTLE_GUARD_MS,
                                );
                            }
                            emit(
                                &state,
                                EventKind::Error,
                                "action execution failed",
                                json!({ "error": err.to_string() }),
                            );
                        }
                    }
                }
            } else {
                emit(
                    &state,
                    EventKind::CommandUnrecognized,
                    "command unrecognized",
                    json!({ "fallback_error": result.fallback_error }),
                );
            }
            (StatusCode::OK, Json(json!(result)))
        }
        Err(err) => {
            error!("intent resolution failed: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": err.to_string() })),
            )
        }
    }
}

async fn handle_system_scenario_effects(state: &AppState, scenario_id: &str) {
    match scenario_id {
        "shutdown_komp" => {
            let stopped = stop_live_listener(state, false).await;
            emit(
                state,
                EventKind::Status,
                "KOMP shutdown scenario applied",
                json!({ "listener_stopped": stopped }),
            );
        }
        "restart_komp" => {
            let stopped = stop_live_listener(state, false).await;
            emit(
                state,
                EventKind::Status,
                "KOMP restart scenario stopping listener",
                json!({ "listener_stopped": stopped }),
            );

            #[cfg(all(feature = "live-audio", feature = "vosk-stt"))]
            {
                match start_live_listener_inner(state.clone()).await {
                    Ok(running) => info!(running, "KOMP restart scenario started listener"),
                    Err(event) => error!(
                        event = %event.message,
                        "KOMP restart scenario failed to start listener"
                    ),
                }
            }

            #[cfg(not(all(feature = "live-audio", feature = "vosk-stt")))]
            emit(
                state,
                EventKind::Error,
                "KOMP restart requires daemon feature `live-vosk`",
                json!({ "required_features": ["live-audio", "vosk-stt"] }),
            );
        }
        _ => {}
    }
}

async fn events_sse(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let receiver = state.events.subscribe();
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
        loop {
            match receiver.recv().await {
                Ok(event) => {
                    let payload = serde_json::to_string(&event).unwrap_or_default();
                    return Some((Ok(Event::default().data(payload)), receiver));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::new().interval(Duration::from_secs(15)))
}

async fn events_ws(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| events_ws_socket(socket, state))
}

async fn events_ws_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let mut events = state.events.subscribe();
    loop {
        tokio::select! {
            event = events.recv() => {
                match event {
                    Ok(event) => {
                        if sender
                            .send(Message::Text(serde_json::to_string(&event).unwrap_or_default()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            message = receiver.next() => {
                match message {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }
}

fn load_registry(config: &ErezConfig) -> PluginRegistry {
    let mut manifests = Vec::new();
    for dir in &config.plugin_dirs {
        match PluginRegistry::load_dir(dir) {
            Ok(registry) => manifests.extend_from_slice(registry.manifests()),
            Err(err) => error!("failed to load plugins from {}: {err}", dir.display()),
        }
    }
    PluginRegistry::from_manifests(manifests)
}

fn emit(
    state: &AppState,
    kind: EventKind,
    message: impl Into<String>,
    data: Value,
) -> AssistantEvent {
    let event = AssistantEvent::new(kind, message, data);
    info!(
        kind = ?event.kind,
        message = %event.message,
        data = %event.data,
        "assistant event"
    );
    let _ = state.events.send(event.clone());
    event
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::{header, Method, Request},
    };
    use std::fs;
    use tower::ServiceExt;

    fn test_state(config: ErezConfig) -> AppState {
        let registry = load_registry(&config);
        let (events, _) = broadcast::channel(32);
        AppState {
            config: Arc::new(RwLock::new(config)),
            registry: Arc::new(RwLock::new(registry)),
            events,
            executor: ActionExecutor,
            listener: Arc::new(RwLock::new(None)),
            audio_playback_until_ms: Arc::new(AtomicU64::new(0)),
        }
    }

    async fn json_body(response: axum::response::Response) -> Value {
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let app = build_router(test_state(ErezConfig::default()));
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["status"], "ok");
    }

    #[tokio::test]
    async fn models_reflect_config_update() {
        let app = build_router(test_state(ErezConfig::default()));
        let mut config = ErezConfig::default();
        config.models.ru_vosk_path = Some(PathBuf::from("/models/ru"));
        config.models.en_vosk_path = Some(PathBuf::from("/models/en"));
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/config")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&config).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = json_body(response).await;
        assert_eq!(body["ru_vosk_path"], "/models/ru");
        assert_eq!(body["en_vosk_path"], "/models/en");
    }

    #[tokio::test]
    async fn listen_once_resolves_plugin_and_emits_event() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("test.toml"),
            r#"
id = "test"
name = "Test"
enabled = true

[[commands]]
id = "lights"
aliases = ["turn lights on", "включи свет"]
patterns = []

[commands.action]
type = "emit_event"
event = "lights_on"
payload = {}
"#,
        )
        .unwrap();

        let mut config = ErezConfig::default();
        config.plugin_dirs = vec![dir.path().to_path_buf()];
        config.lmstudio.enabled = false;
        let state = test_state(config);
        let mut events = state.events.subscribe();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/listen/once")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"transcript":"turn lights on"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["resolved"]["command_id"], "lights");
        assert_eq!(body["resolved"]["source"], "plugin");

        let first = events.recv().await.unwrap();
        assert_eq!(first.kind, EventKind::SpeechRecognized);
        let second = events.recv().await.unwrap();
        assert_eq!(second.kind, EventKind::IntentResolved);
        let third = events.recv().await.unwrap();
        assert_eq!(third.kind, EventKind::ActionExecuted);
    }

    #[tokio::test]
    async fn listen_once_resolves_and_executes_scenario_events() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("test.toml"),
            r#"
id = "test"
name = "Test"
enabled = true

[[scenarios]]
id = "browser_quieter"
aliases = ["включи браузер громкость ниже"]
priority = 20

[[scenarios.steps]]
id = "first"
action = { type = "emit_event", event = "first", payload = {} }

[[scenarios.steps]]
id = "second"
when = { previous_success = true }
action = { type = "emit_event", event = "second", payload = {} }
"#,
        )
        .unwrap();

        let mut config = ErezConfig::default();
        config.plugin_dirs = vec![dir.path().to_path_buf()];
        config.lmstudio.enabled = false;
        let state = test_state(config);
        let mut events = state.events.subscribe();
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/listen/once")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"transcript":"включи браузер громкость ниже"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["resolved"]["source"], "scenario");
        assert_eq!(body["resolved"]["action"]["type"], "scenario");

        let mut action_events = 0;
        for _ in 0..5 {
            let event = events.recv().await.unwrap();
            if event.kind == EventKind::ActionExecuted {
                action_events += 1;
            }
        }
        assert!(action_events >= 3);
    }

    #[tokio::test]
    async fn listen_once_dialog_reply_branches_scenario() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("test.toml"),
            r#"
id = "test"
name = "Test"
enabled = true

[[scenarios]]
id = "choose_browser"
aliases = ["открой браузер какой"]
priority = 20

[[scenarios.steps]]
id = "ask"
action = { type = "ask", reply_slot = "browser" }

[[scenarios.steps]]
id = "chrome"
when = { slot = "browser", contains = "chrome" }
action = { type = "emit_event", event = "chrome", payload = {} }

[[scenarios.steps]]
id = "safari"
when = { slot = "browser", contains = "safari" }
action = { type = "emit_event", event = "safari", payload = {} }
"#,
        )
        .unwrap();

        let mut config = ErezConfig::default();
        config.plugin_dirs = vec![dir.path().to_path_buf()];
        config.lmstudio.enabled = false;
        let app = build_router(test_state(config));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/listen/once")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"transcript":"открой браузер какой","replies":["chrome"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["resolved"]["source"], "scenario");
    }

    #[tokio::test]
    async fn listen_once_without_transcript_returns_not_implemented() {
        let app = build_router(test_state(ErezConfig::default()));
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/listen/once")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let body = json_body(response).await;
        assert_eq!(body["event"]["kind"], "error");
    }

    #[tokio::test]
    async fn listen_start_without_live_feature_reports_required_features() {
        let app = build_router(test_state(ErezConfig::default()));
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/listen/start")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let body = json_body(response).await;
        assert_eq!(body["event"]["kind"], "error");
        assert_eq!(
            body["event"]["data"]["required_features"],
            json!(["live-audio", "vosk-stt"])
        );
    }
}
