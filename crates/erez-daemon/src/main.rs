use axum::{
    extract::{
        ws::{Message, WebSocket},
        Path as AxumPath, State, WebSocketUpgrade,
    },
    http::StatusCode,
    response::{sse::Event, IntoResponse, Sse},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose, Engine as _};
use erez_core::{
    apply_slots_to_action, ActionExecutor, AssistantEvent, DefaultIntentResolver, ErezConfig,
    EventKind, IntentRequest, IntentResolver, IntentResult, LmStudioConfig, NoopReplyProvider,
    PluginRegistry, ResolvedAction, ScenarioRunner, StaticReplyProvider,
};
use erez_core::{plugins::Scenario, Action, PluginManifest};
use futures_util::{SinkExt, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::process::Command;
use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
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
const SYSTEM_SOUND_EXTENSIONS: [&str; 3] = ["mp3", "wav", "ogg"];
const SYSTEM_SOUND_SLOTS: [&str; 21] = [
    "startup",
    "shutdown",
    "wake",
    "listening",
    "success",
    "error",
    "timeout",
    "power_connected",
    "power_disconnected",
    "battery_0_10",
    "battery_10_20",
    "battery_20_30",
    "battery_30_40",
    "battery_40_50",
    "battery_50_60",
    "battery_60_70",
    "battery_70_80",
    "battery_80_90",
    "battery_90_100",
    "battery_100",
    "battery_unavailable",
];
const AUDIO_PLAYBACK_SETTLE_GUARD_MS: u64 = 700;
const SCENARIO_AUDIO_GUARD_MS: u64 = 8_000;
const DEFAULT_USER_PLUGIN_ROOT: &str = "plugins.user";
const BATTERY_STATUS_ALIASES: [&str; 8] = [
    "сколько зарядки",
    "сколько заряда",
    "заряд батареи",
    "статус батареи",
    "сколько процентов батарея",
    "сколько процентов заряд",
    "проверить заряд",
    "battery status",
];

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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScenarioDocument {
    manifest_id: String,
    manifest_name: String,
    enabled: bool,
    scenario: Scenario,
}

#[derive(Debug, Clone, Serialize)]
struct ScenarioSummary {
    id: String,
    manifest_id: String,
    manifest_name: String,
    aliases: Vec<String>,
    patterns: Vec<String>,
    priority: i32,
    step_count: usize,
    readonly: bool,
    path: String,
}

#[derive(Debug, Clone, Serialize)]
struct ValidationResponse {
    ok: bool,
    errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct AppInfo {
    name: String,
    path: Option<String>,
    source: String,
}

#[derive(Debug, Deserialize)]
struct SoundUploadRequest {
    file_name: String,
    data_base64: String,
}

#[derive(Debug, Clone, Serialize)]
struct LmStudioTestResponse {
    ok: bool,
    base_url: String,
    models: Vec<String>,
    error: Option<String>,
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
    start_system_monitor(state.clone());

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
        .route("/config", get(get_config).post(update_config))
        .route("/lmstudio/test", post(lmstudio_test))
        .route("/models", get(models))
        .route("/scenarios", get(scenarios_list).post(scenarios_create))
        .route(
            "/scenarios/:id",
            get(scenarios_get)
                .put(scenarios_update)
                .delete(scenarios_delete),
        )
        .route("/scenarios/:id/validate", post(scenarios_validate))
        .route("/scenarios/:id/dry-run", post(scenarios_dry_run))
        .route("/scenarios/:id/sounds", post(scenarios_sound_upload))
        .route("/system-sounds", get(system_sounds_list))
        .route("/system-sounds/:slot", post(system_sound_upload))
        .route("/apps", get(apps_list))
        .route("/logo", get(logo_get))
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
        })
        .or_else(|| {
            let path = PathBuf::from("komp.prototype.toml");
            path.exists().then_some(path)
        })
        .or_else(|| {
            let path = PathBuf::from("erez.prototype.toml");
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

async fn scenarios_list(State(state): State<AppState>) -> impl IntoResponse {
    let config = state.config.read().await.clone();
    Json(json!({ "scenarios": collect_scenario_summaries(&config) }))
}

async fn scenarios_get(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> impl IntoResponse {
    let config = state.config.read().await.clone();
    match find_scenario_document(&config, &id) {
        Ok(Some((document, readonly, path))) => (
            StatusCode::OK,
            Json(
                json!({ "scenario": document, "readonly": readonly, "path": path.display().to_string() }),
            ),
        ),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "scenario not found" })),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": err.to_string() })),
        ),
    }
}

async fn scenarios_create(
    State(state): State<AppState>,
    Json(document): Json<ScenarioDocument>,
) -> impl IntoResponse {
    let config = state.config.read().await.clone();
    let validation = validate_scenario_document(&document, &config, None);
    if !validation.errors.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!(validation)));
    }

    match write_user_scenario(&document, false) {
        Ok(path) => {
            reload_registry(&state).await;
            (
                StatusCode::CREATED,
                Json(json!({ "ok": true, "path": path.display().to_string() })),
            )
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": err.to_string() })),
        ),
    }
}

async fn scenarios_update(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(mut document): Json<ScenarioDocument>,
) -> impl IntoResponse {
    document.scenario.id = id.clone();
    let config = state.config.read().await.clone();
    if scenario_is_readonly(&config, &id) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "system scenarios are read-only" })),
        );
    }
    let validation = validate_scenario_document(&document, &config, Some(&id));
    if !validation.errors.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!(validation)));
    }

    match write_user_scenario(&document, true) {
        Ok(path) => {
            reload_registry(&state).await;
            (
                StatusCode::OK,
                Json(json!({ "ok": true, "path": path.display().to_string() })),
            )
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": err.to_string() })),
        ),
    }
}

async fn scenarios_delete(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> impl IntoResponse {
    let config = state.config.read().await.clone();
    if scenario_is_readonly(&config, &id) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "system scenarios are read-only" })),
        );
    }
    let path = user_scenario_dir(&id);
    if !path.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "user scenario not found" })),
        );
    }
    match fs::remove_dir_all(&path) {
        Ok(()) => {
            reload_registry(&state).await;
            (StatusCode::OK, Json(json!({ "ok": true })))
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": err.to_string() })),
        ),
    }
}

async fn scenarios_validate(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    body: Option<Json<ScenarioDocument>>,
) -> impl IntoResponse {
    let config = state.config.read().await.clone();
    let validation = if let Some(Json(mut document)) = body {
        document.scenario.id = id;
        validate_scenario_document(&document, &config, Some(&document.scenario.id))
    } else {
        match find_scenario_document(&config, &id) {
            Ok(Some((document, _, _))) => validate_scenario_document(&document, &config, Some(&id)),
            Ok(None) => ValidationResponse {
                ok: false,
                errors: vec!["scenario not found".into()],
            },
            Err(err) => ValidationResponse {
                ok: false,
                errors: vec![err.to_string()],
            },
        }
    };
    let status = if validation.ok {
        StatusCode::OK
    } else {
        StatusCode::BAD_REQUEST
    };
    (status, Json(json!(validation)))
}

async fn scenarios_dry_run(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> impl IntoResponse {
    let registry = state.registry.read().await.clone();
    let Some((manifest, _)) = registry.manifests().iter().find_map(|manifest| {
        manifest
            .scenarios
            .iter()
            .find(|scenario| scenario.id == id)
            .map(|scenario| (manifest, scenario))
    }) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "scenario not found" })),
        );
    };
    let runner = ScenarioRunner::new(registry.clone(), ActionExecutor).dry_run(true);
    let mut replies = NoopReplyProvider;
    match runner.run(&manifest.id, &id, HashMap::new(), &mut replies) {
        Ok(run) => (StatusCode::OK, Json(json!({ "run": run }))),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": err.to_string() })),
        ),
    }
}

async fn scenarios_sound_upload(
    AxumPath(id): AxumPath<String>,
    Json(request): Json<SoundUploadRequest>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid scenario id" })),
        );
    }
    let file_name = sanitize_sound_file_name(&request.file_name);
    if file_name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid sound file name" })),
        );
    }
    let relative = user_plugin_root()
        .join("scenarios")
        .join(&id)
        .join("sounds")
        .join(&file_name);
    let extension = relative
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "mp3" | "wav" | "ogg") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "sound file must be MP3, WAV or OGG" })),
        );
    }
    let bytes = match general_purpose::STANDARD.decode(request.data_base64.as_bytes()) {
        Ok(bytes) => bytes,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": err.to_string() })),
            )
        }
    };
    if let Some(parent) = relative.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": err.to_string() })),
            );
        }
    }
    match fs::write(&relative, bytes) {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({ "ok": true, "file": relative.display().to_string() })),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": err.to_string() })),
        ),
    }
}

async fn system_sound_upload(
    AxumPath(slot): AxumPath<String>,
    Json(request): Json<SoundUploadRequest>,
) -> impl IntoResponse {
    if !SYSTEM_SOUND_SLOTS.contains(&slot.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "unknown system sound slot" })),
        );
    }
    let file_name = sanitize_sound_file_name(&request.file_name);
    let extension = Path::new(&file_name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !SYSTEM_SOUND_EXTENSIONS.contains(&extension.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "sound file must be MP3, WAV or OGG" })),
        );
    }
    let bytes = match general_purpose::STANDARD.decode(request.data_base64.as_bytes()) {
        Ok(bytes) => bytes,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": err.to_string() })),
            )
        }
    };
    let root = system_sound_root();
    if let Err(err) = fs::create_dir_all(&root) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": err.to_string() })),
        );
    }
    for existing_extension in SYSTEM_SOUND_EXTENSIONS {
        if existing_extension != extension {
            let _ = fs::remove_file(root.join(format!("{slot}.{existing_extension}")));
        }
    }
    let path = root.join(format!("{slot}.{extension}"));
    match fs::write(&path, bytes) {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({ "ok": true, "file": path.display().to_string(), "slot": slot })),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": err.to_string() })),
        ),
    }
}

async fn system_sounds_list() -> impl IntoResponse {
    let root = system_sound_root();
    let sounds = SYSTEM_SOUND_SLOTS
        .iter()
        .map(|slot| {
            let file = find_system_sound_path(slot).map(|path| path.display().to_string());
            json!({
                "slot": slot,
                "exists": file.is_some(),
                "file": file
            })
        })
        .collect::<Vec<_>>();
    Json(json!({ "root": root.display().to_string(), "sounds": sounds }))
}

async fn apps_list() -> impl IntoResponse {
    Json(json!({ "apps": scan_installed_apps() }))
}

async fn logo_get() -> impl IntoResponse {
    let Some(path) = find_logo_path() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "logo not found" })),
        );
    };
    match fs::read(&path) {
        Ok(bytes) => {
            let mime = match path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or_default()
            {
                "svg" => "image/svg+xml",
                "jpg" | "jpeg" => "image/jpeg",
                "webp" => "image/webp",
                _ => "image/png",
            };
            (
                StatusCode::OK,
                Json(json!({
                    "path": path.display().to_string(),
                    "data_url": format!("data:{mime};base64,{}", general_purpose::STANDARD.encode(bytes))
                })),
            )
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": err.to_string() })),
        ),
    }
}

async fn commands_reload(State(state): State<AppState>) -> impl IntoResponse {
    let count = reload_registry(&state).await;
    Json(json!({ "ok": true, "plugin_count": count }))
}

async fn get_config(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.config.read().await.clone())
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

async fn lmstudio_test(
    State(state): State<AppState>,
    body: Option<Json<LmStudioConfig>>,
) -> impl IntoResponse {
    let config = if let Some(Json(config)) = body {
        config
    } else {
        state.config.read().await.lmstudio.clone()
    };
    Json(test_lmstudio_connection(config).await)
}

async fn test_lmstudio_connection(config: LmStudioConfig) -> LmStudioTestResponse {
    let base_url = config.base_url.trim_end_matches('/').to_string();
    let url = format!("{base_url}/models");
    let timeout = Duration::from_millis(config.timeout_ms.max(500));
    let client = match reqwest::Client::builder().timeout(timeout).build() {
        Ok(client) => client,
        Err(err) => {
            return LmStudioTestResponse {
                ok: false,
                base_url,
                models: Vec::new(),
                error: Some(err.to_string()),
            }
        }
    };
    match client.get(url).send().await {
        Ok(response) if response.status().is_success() => {
            let body = response.json::<Value>().await.unwrap_or_else(|_| json!({}));
            let models = body
                .get("data")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.get("id").and_then(Value::as_str))
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            LmStudioTestResponse {
                ok: true,
                base_url,
                models,
                error: None,
            }
        }
        Ok(response) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            LmStudioTestResponse {
                ok: false,
                base_url,
                models: Vec::new(),
                error: Some(format!("LM Studio returned {status}: {body}")),
            }
        }
        Err(err) => LmStudioTestResponse {
            ok: false,
            base_url,
            models: Vec::new(),
            error: Some(err.to_string()),
        },
    }
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

fn start_system_monitor(state: AppState) {
    tokio::spawn(async move {
        let mut last_power_connected =
            read_battery_snapshot().map(|snapshot| snapshot.power_connected);
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        info!(?last_power_connected, "system battery monitor started");
        loop {
            interval.tick().await;
            let Some(snapshot) = read_battery_snapshot() else {
                info!("system battery monitor could not read battery status");
                continue;
            };

            let Some(previous_power_connected) = last_power_connected else {
                last_power_connected = Some(snapshot.power_connected);
                info!(
                    power_connected = snapshot.power_connected,
                    battery_percent = snapshot.percent,
                    "system battery monitor initialized power state"
                );
                continue;
            };

            if previous_power_connected != snapshot.power_connected {
                last_power_connected = Some(snapshot.power_connected);
                let kind = if snapshot.power_connected {
                    "power_connected"
                } else {
                    "power_disconnected"
                };
                emit(
                    &state,
                    EventKind::Status,
                    if snapshot.power_connected {
                        "power connected"
                    } else {
                        "power disconnected"
                    },
                    json!({
                        "event": kind,
                        "battery_percent": snapshot.percent,
                        "charging": snapshot.power_connected,
                        "power_connected": snapshot.power_connected
                    }),
                );
                let played_power_sound =
                    play_optional_system_sound(kind, state.audio_playback_until_ms.clone());
                if !played_power_sound {
                    info!(
                        slot = kind,
                        "system power sound not configured; put MP3/WAV/OGG into sounds/system"
                    );
                }
                if played_power_sound {
                    tokio::time::sleep(Duration::from_millis(1_200)).await;
                }
                announce_battery_snapshot(&state, snapshot, "power_change");
            }
        }
    });
}

fn announce_battery_status(state: &AppState) {
    let Some(snapshot) = read_battery_snapshot() else {
        emit(
            state,
            EventKind::Error,
            "battery status unavailable",
            json!({ "event": "battery_status_unavailable" }),
        );
        play_optional_system_sound("battery_unavailable", state.audio_playback_until_ms.clone());
        return;
    };
    announce_battery_snapshot(state, snapshot, "voice_command");
}

fn announce_battery_snapshot(state: &AppState, snapshot: BatterySnapshot, trigger: &str) {
    let slot = battery_sound_slot(snapshot.percent);
    let old_bucket = legacy_battery_bucket(snapshot.percent);
    emit(
        state,
        EventKind::Status,
        "battery status requested",
        json!({
            "event": "battery_status",
            "battery_percent": snapshot.percent,
            "charging": snapshot.power_connected,
            "power_connected": snapshot.power_connected,
            "sound_slot": slot,
            "legacy_bucket": old_bucket,
            "trigger": trigger,
            "message": format!("battery charge is {}%", snapshot.percent)
        }),
    );
    let played = play_optional_system_sound(slot, state.audio_playback_until_ms.clone());
    if !played {
        if let Some(bucket) = old_bucket {
            play_optional_system_sound(
                &format!("battery_gt_{bucket}"),
                state.audio_playback_until_ms.clone(),
            );
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BatterySnapshot {
    percent: u8,
    power_connected: bool,
}

fn battery_sound_slot(percent: u8) -> &'static str {
    match percent {
        0..=9 => "battery_0_10",
        10..=19 => "battery_10_20",
        20..=29 => "battery_20_30",
        30..=39 => "battery_30_40",
        40..=49 => "battery_40_50",
        50..=59 => "battery_50_60",
        60..=69 => "battery_60_70",
        70..=79 => "battery_70_80",
        80..=89 => "battery_80_90",
        90..=99 => "battery_90_100",
        _ => "battery_100",
    }
}

fn legacy_battery_bucket(percent: u8) -> Option<u8> {
    if percent < 50 {
        None
    } else {
        Some((percent / 10) * 10)
    }
}

fn play_optional_system_sound(name: &str, guard: Arc<AtomicU64>) -> bool {
    if let Some(path) = find_system_sound_path(name) {
        play_configured_sound_async("system_monitor", path.to_str(), guard);
        return true;
    }
    false
}

fn find_system_sound_path(name: &str) -> Option<PathBuf> {
    SYSTEM_SOUND_EXTENSIONS
        .iter()
        .map(|extension| system_sound_root().join(format!("{name}.{extension}")))
        .find(|path| path.exists())
}

fn read_battery_snapshot() -> Option<BatterySnapshot> {
    #[cfg(target_os = "macos")]
    {
        read_macos_battery_snapshot()
    }
    #[cfg(target_os = "windows")]
    {
        read_windows_battery_snapshot()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        read_linux_battery_snapshot()
    }
}

#[cfg(target_os = "macos")]
fn read_macos_battery_snapshot() -> Option<BatterySnapshot> {
    let output = Command::new("pmset").args(["-g", "batt"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let percent = parse_percent(&text)?;
    let power_connected = parse_macos_power_connected(&text);
    Some(BatterySnapshot {
        percent,
        power_connected,
    })
}

fn parse_macos_power_connected(text: &str) -> bool {
    text.contains("AC Power")
}

#[cfg(target_os = "windows")]
fn read_windows_battery_snapshot() -> Option<BatterySnapshot> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "$b=Get-CimInstance Win32_Battery | Select-Object -First 1; if ($b) { \"$($b.EstimatedChargeRemaining);$($b.BatteryStatus)\" }",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut parts = text.trim().split(';');
    let percent = parts.next()?.trim().parse::<u8>().ok()?;
    let status = parts.next().unwrap_or_default().trim();
    let power_connected = windows_battery_status_power_connected(status);
    Some(BatterySnapshot {
        percent,
        power_connected,
    })
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn windows_battery_status_power_connected(status: &str) -> bool {
    matches!(status, "2" | "3" | "6" | "7" | "8" | "9")
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn read_linux_battery_snapshot() -> Option<BatterySnapshot> {
    let battery = fs::read_dir("/sys/class/power_supply")
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            fs::read_to_string(path.join("type"))
                .map(|kind| kind.trim() == "Battery")
                .unwrap_or(false)
        })?;
    let percent = fs::read_to_string(battery.join("capacity"))
        .ok()?
        .trim()
        .parse::<u8>()
        .ok()?;
    let status = fs::read_to_string(battery.join("status")).unwrap_or_default();
    let power_connected = read_linux_power_connected()
        .unwrap_or_else(|| status.contains("Charging") || status.contains("Full"));
    Some(BatterySnapshot {
        percent,
        power_connected,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn read_linux_power_connected() -> Option<bool> {
    let supplies = fs::read_dir("/sys/class/power_supply").ok()?;
    for entry in supplies.flatten() {
        let path = entry.path();
        let kind = fs::read_to_string(path.join("type")).unwrap_or_default();
        if !matches!(kind.trim(), "Mains" | "USB" | "USB_C" | "USB_PD") {
            continue;
        }
        let online = fs::read_to_string(path.join("online")).unwrap_or_default();
        if online.trim() == "1" {
            return Some(true);
        }
    }
    Some(false)
}

fn parse_percent(text: &str) -> Option<u8> {
    let percent_index = text.find('%')?;
    let start = text[..percent_index]
        .rfind(|ch: char| !ch.is_ascii_digit())
        .map(|index| index + 1)
        .unwrap_or(0);
    text[start..percent_index].parse::<u8>().ok()
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
        play_wake_feedback_sounds_async(
            config.sounds.wake.as_deref(),
            config.sounds.listening.as_deref(),
        );

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

        let command_audio_ms =
            audio.len() as u64 * 1_000 / u64::from(config.audio.sample_rate_hz.max(1));
        info!(
            samples = audio.len(),
            duration_ms = command_audio_ms,
            "command audio captured after wake"
        );

        let stt_started_at = std::time::Instant::now();
        match transcribe_command_preferred(&mut recognizer, &audio, &config) {
            Ok(transcript) if !transcript.text.trim().is_empty() => {
                let stt_latency_ms = stt_started_at.elapsed().as_millis() as u64;
                info!(
                    text = %transcript.text,
                    language = ?transcript.language,
                    confidence = transcript.confidence,
                    stt_latency_ms,
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

#[allow(dead_code)]
fn play_wake_feedback_sounds_async(wake: Option<&str>, listening: Option<&str>) {
    let wake = non_empty_sound(wake).map(str::to_string);
    let listening = non_empty_sound(listening).map(str::to_string);
    if wake.is_none() && listening.is_none() {
        return;
    };

    std::thread::spawn(move || {
        if let Some(wake) = wake.as_deref() {
            play_configured_sound_blocking("wake", Some(wake), None);
        }
        if let Some(listening) = listening {
            if wake.as_deref() != Some(listening.as_str()) {
                play_configured_sound_blocking("listening", Some(&listening), None);
            }
        }
    });
}

#[allow(dead_code)]
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
    if let Some(result) = resolve_system_intent(&transcript) {
        return process_intent_result(&state, Ok(result), replies).await;
    }

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

fn resolve_system_intent(transcript: &str) -> Option<IntentResult> {
    let best_score = BATTERY_STATUS_ALIASES
        .iter()
        .filter_map(|alias| erez_core::normalize::fuzzy_phrase_score(transcript, alias))
        .fold(0.0_f32, f32::max);
    if best_score < 0.78 {
        return None;
    }

    Some(IntentResult {
        utterance: transcript.to_string(),
        resolved: Some(ResolvedAction {
            source: "system".into(),
            plugin_id: Some("komp_system".into()),
            command_id: Some("battery_status".into()),
            action: Action::EmitEvent {
                event: "battery_status_requested".into(),
                payload: json!({ "source": "system_intent" }),
            },
            confidence: best_score,
            slots: HashMap::new(),
            speak: None,
        }),
        fallback_error: None,
    })
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
                                handle_system_step_effects(state, step);
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
                } else if is_system_battery_status_intent(resolved) {
                    emit(
                        &state,
                        EventKind::ActionExecuted,
                        "system battery status requested",
                        json!({
                            "executed": true,
                            "message": "system battery status requested"
                        }),
                    );
                    announce_battery_status(state);
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
                            handle_system_action_effects(state, &action);
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

fn is_system_battery_status_intent(resolved: &ResolvedAction) -> bool {
    resolved.source == "system" && resolved.command_id.as_deref() == Some("battery_status")
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

fn handle_system_action_effects(state: &AppState, action: &Action) {
    if let Action::EmitEvent { event, .. } = action {
        if event == "battery_status_requested" {
            announce_battery_status(state);
        }
    }
}

fn handle_system_step_effects(state: &AppState, step: &erez_core::scenario::ScenarioStepResult) {
    if step.success && step.message.contains("`battery_status_requested`") {
        announce_battery_status(state);
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
    for dir in effective_plugin_dirs(config) {
        match PluginRegistry::load_dir(&dir) {
            Ok(registry) => manifests.extend_from_slice(registry.manifests()),
            Err(err) => error!("failed to load plugins from {}: {err}", dir.display()),
        }
    }
    PluginRegistry::from_manifests(manifests)
}

async fn reload_registry(state: &AppState) -> usize {
    let config = state.config.read().await.clone();
    let registry = load_registry(&config);
    let count = registry.manifests().len();
    *state.registry.write().await = registry;
    emit(
        state,
        EventKind::Status,
        "commands reloaded",
        json!({ "plugin_count": count }),
    );
    count
}

fn collect_scenario_summaries(config: &ErezConfig) -> Vec<ScenarioSummary> {
    let mut summaries = Vec::new();
    for dir in effective_plugin_dirs(config) {
        collect_scenario_summaries_from_dir(&dir, &mut summaries);
    }
    summaries.sort_by(|a, b| a.id.cmp(&b.id));
    summaries
}

fn collect_scenario_summaries_from_dir(dir: &Path, summaries: &mut Vec<ScenarioSummary>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_scenario_summaries_from_dir(&path, summaries);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let Ok(manifest) = PluginRegistry::load_manifest(&path) else {
            continue;
        };
        let readonly = !is_user_manifest_path(&path);
        for scenario in manifest.scenarios {
            summaries.push(ScenarioSummary {
                id: scenario.id,
                manifest_id: manifest.id.clone(),
                manifest_name: manifest.name.clone(),
                aliases: scenario.aliases,
                patterns: scenario.patterns,
                priority: scenario.priority,
                step_count: scenario.steps.len(),
                readonly,
                path: path.display().to_string(),
            });
        }
    }
}

fn find_scenario_document(
    config: &ErezConfig,
    id: &str,
) -> anyhow::Result<Option<(ScenarioDocument, bool, PathBuf)>> {
    for dir in effective_plugin_dirs(config) {
        if let Some(found) = find_scenario_document_in_dir(&dir, id)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

fn find_scenario_document_in_dir(
    dir: &Path,
    id: &str,
) -> anyhow::Result<Option<(ScenarioDocument, bool, PathBuf)>> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(None);
    };
    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            if let Some(found) = find_scenario_document_in_dir(&path, id)? {
                return Ok(Some(found));
            }
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let manifest = PluginRegistry::load_manifest(&path)?;
        if let Some(scenario) = manifest
            .scenarios
            .iter()
            .find(|scenario| scenario.id == id)
            .cloned()
        {
            let readonly = !is_user_manifest_path(&path);
            return Ok(Some((
                ScenarioDocument {
                    manifest_id: manifest.id,
                    manifest_name: manifest.name,
                    enabled: manifest.enabled,
                    scenario,
                },
                readonly,
                path,
            )));
        }
    }
    Ok(None)
}

fn validate_scenario_document(
    document: &ScenarioDocument,
    config: &ErezConfig,
    current_id: Option<&str>,
) -> ValidationResponse {
    let mut errors = Vec::new();
    let id = document.scenario.id.trim();
    if id.is_empty() {
        errors.push("scenario id is required".into());
    }
    if !is_safe_id(id) {
        errors.push("scenario id can contain only letters, numbers, `_` and `-`".into());
    }
    if document.scenario.aliases.is_empty() && document.scenario.patterns.is_empty() {
        errors.push("add at least one alias or pattern".into());
    }
    if document.scenario.steps.is_empty() {
        errors.push("add at least one step".into());
    }

    let mut step_ids = HashSet::new();
    for step in &document.scenario.steps {
        if step.id.trim().is_empty() {
            errors.push("step id is required".into());
        }
        if !step_ids.insert(step.id.clone()) {
            errors.push(format!("duplicate step id `{}`", step.id));
        }
    }
    for step in &document.scenario.steps {
        for target in [step.on_success.as_ref(), step.on_error.as_ref()]
            .into_iter()
            .flatten()
        {
            if !step_ids.contains(target) {
                errors.push(format!(
                    "step `{}` points to missing branch target `{target}`",
                    step.id
                ));
            }
        }
        validate_action_for_ui(&step.action, &mut errors);
        validate_sound_ref(step.before_sound.as_deref(), &mut errors);
        validate_sound_ref(step.after_sound.as_deref(), &mut errors);
    }

    for summary in collect_scenario_summaries(config) {
        if Some(summary.id.as_str()) != current_id && summary.id == id {
            errors.push(format!("scenario id `{id}` already exists"));
        }
    }

    ValidationResponse {
        ok: errors.is_empty(),
        errors,
    }
}

fn validate_action_for_ui(action: &Action, errors: &mut Vec<String>) {
    match action {
        Action::OpenApp { app } if app.trim().is_empty() => {
            errors.push("open_app requires app".into())
        }
        Action::SetVolume { level, delta } if level.is_none() && delta.is_none() => {
            errors.push("set_volume requires level or delta".into())
        }
        Action::PlaySound { file } | Action::SaySound { file } => {
            validate_sound_ref(Some(file), errors);
        }
        Action::Ask { sound, reply_slot } => {
            if reply_slot.trim().is_empty() {
                errors.push("ask requires reply_slot".into());
            }
            validate_sound_ref(sound.as_deref(), errors);
        }
        Action::WaitForReply { reply_slot } if reply_slot.trim().is_empty() => {
            errors.push("wait_for_reply requires reply_slot".into())
        }
        Action::Shell { command, .. } if command.trim().is_empty() => {
            errors.push("shell requires command".into())
        }
        Action::Hotkey { keys } if keys.is_empty() => errors.push("hotkey requires keys".into()),
        Action::Url { url } if !(url.starts_with("http://") || url.starts_with("https://")) => {
            errors.push("url must start with http:// or https://".into())
        }
        Action::HttpRequest { method, url, .. } => {
            if method.trim().is_empty() {
                errors.push("http_request requires method".into());
            }
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                errors.push("http_request url must start with http:// or https://".into());
            }
        }
        Action::EmitEvent { event, .. } if event.trim().is_empty() => {
            errors.push("emit_event requires event".into())
        }
        _ => {}
    }
}

fn validate_sound_ref(sound: Option<&str>, errors: &mut Vec<String>) {
    let Some(sound) = sound else {
        return;
    };
    if sound.trim().is_empty() {
        errors.push("sound path cannot be empty".into());
        return;
    }
    let extension = Path::new(sound)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "mp3" | "wav" | "ogg") {
        errors.push(format!("sound `{sound}` must be MP3, WAV or OGG"));
    }
}

fn write_user_scenario(document: &ScenarioDocument, overwrite: bool) -> anyhow::Result<PathBuf> {
    let dir = user_scenario_dir(&document.scenario.id);
    let path = dir.join("scenario.toml");
    if path.exists() && !overwrite {
        anyhow::bail!("scenario already exists");
    }
    fs::create_dir_all(&dir)?;
    let manifest = PluginManifest {
        id: user_manifest_id(&document.scenario.id),
        name: if document.manifest_name.trim().is_empty() {
            format!("{} scenario", document.scenario.id)
        } else {
            document.manifest_name.clone()
        },
        enabled: document.enabled,
        commands: Vec::new(),
        scenarios: vec![document.scenario.clone()],
    };
    fs::write(&path, toml::to_string_pretty(&manifest)?)?;
    Ok(path)
}

fn scenario_is_readonly(config: &ErezConfig, id: &str) -> bool {
    find_scenario_document(config, id)
        .ok()
        .flatten()
        .map(|(_, readonly, _)| readonly)
        .unwrap_or(false)
}

fn effective_plugin_dirs(config: &ErezConfig) -> Vec<PathBuf> {
    let mut dirs = config
        .plugin_dirs
        .iter()
        .map(|dir| resolve_project_path(dir))
        .collect::<Vec<_>>();
    let user = user_plugin_root();
    if !dirs.iter().any(|dir| dir == &user) {
        dirs.push(user);
    }
    dirs
}

fn user_scenario_dir(id: &str) -> PathBuf {
    user_plugin_root().join("scenarios").join(id)
}

fn user_plugin_root() -> PathBuf {
    std::env::var_os("KOMP_USER_PLUGIN_DIR")
        .map(PathBuf::from)
        .map(|path| resolve_project_path(&path))
        .unwrap_or_else(|| project_root().join(DEFAULT_USER_PLUGIN_ROOT))
}

fn system_sound_root() -> PathBuf {
    std::env::var_os("KOMP_SYSTEM_SOUND_DIR")
        .map(PathBuf::from)
        .map(|path| resolve_project_path(&path))
        .unwrap_or_else(|| project_root().join("sounds/system"))
}

fn project_root() -> PathBuf {
    std::env::var_os("KOMP_PROJECT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        })
}

fn resolve_project_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root().join(path)
    }
}

fn user_manifest_id(id: &str) -> String {
    format!("user_{id}")
}

fn is_user_manifest_path(path: &Path) -> bool {
    path.starts_with(user_plugin_root())
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn sanitize_sound_file_name(file_name: &str) -> String {
    Path::new(file_name)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn scan_installed_apps() -> Vec<AppInfo> {
    let mut apps: Vec<AppInfo> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        for dir in [
            "/Applications",
            "/Applications/Utilities",
            "/System/Applications",
            "/System/Applications/Utilities",
        ] {
            scan_macos_apps(Path::new(dir), &mut apps);
        }
        if let Some(home) = std::env::var_os("HOME") {
            scan_macos_apps(&PathBuf::from(home).join("Applications"), &mut apps);
        }
    }
    #[cfg(target_os = "windows")]
    {
        for dir in [
            std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .map(|path| path.join("Microsoft/Windows/Start Menu/Programs")),
            std::env::var_os("PROGRAMDATA")
                .map(PathBuf::from)
                .map(|path| path.join("Microsoft/Windows/Start Menu/Programs")),
        ]
        .into_iter()
        .flatten()
        {
            scan_windows_shortcuts(&dir, &mut apps);
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        for dir in [
            Some(PathBuf::from("/usr/share/applications")),
            Some(PathBuf::from("/usr/local/share/applications")),
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|path| path.join(".local/share/applications")),
        ]
        .into_iter()
        .flatten()
        {
            scan_linux_desktop_apps(&dir, &mut apps);
        }
    }
    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
    apps
}

fn find_logo_path() -> Option<PathBuf> {
    let candidates = [
        "logo.png",
        "logo.svg",
        "logo.jpg",
        "logo.jpeg",
        "logo.webp",
        "komp-logo.png",
        "komp-logo.svg",
        "komp-logo.jpg",
        "komp-logo.jpeg",
        "komp-logo.webp",
    ];
    if let Some(path) = candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
    {
        return Some(path);
    }

    fs::read_dir(".")
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.is_file()
                && matches!(
                    path.extension()
                        .and_then(|ext| ext.to_str())
                        .map(|ext| ext.to_ascii_lowercase())
                        .as_deref(),
                    Some("png" | "svg" | "jpg" | "jpeg" | "webp")
                )
        })
}

#[cfg(target_os = "macos")]
fn scan_macos_apps(dir: &Path, apps: &mut Vec<AppInfo>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("app") {
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            if !name.is_empty() {
                apps.push(AppInfo {
                    name,
                    path: Some(path.display().to_string()),
                    source: "macos_app_bundle".into(),
                });
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn scan_windows_shortcuts(dir: &Path, apps: &mut Vec<AppInfo>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_windows_shortcuts(&path, apps);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) == Some("lnk") {
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            if !name.is_empty() {
                apps.push(AppInfo {
                    name,
                    path: Some(path.display().to_string()),
                    source: "windows_start_menu".into(),
                });
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn scan_linux_desktop_apps(dir: &Path, apps: &mut Vec<AppInfo>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("desktop") {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        if content
            .lines()
            .any(|line| line.trim().eq_ignore_ascii_case("NoDisplay=true"))
        {
            continue;
        }
        let name = content
            .lines()
            .find_map(|line| line.strip_prefix("Name=").map(str::trim))
            .unwrap_or_default()
            .to_string();
        let desktop_id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        if !name.is_empty() && !desktop_id.is_empty() {
            apps.push(AppInfo {
                name,
                path: Some(desktop_id),
                source: "linux_desktop_entry".into(),
            });
        }
    }
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
    use std::{fs, sync::OnceLock};
    use tower::ServiceExt;

    fn env_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

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
    async fn config_get_returns_lmstudio_settings() {
        let mut config = ErezConfig::default();
        config.lmstudio.enabled = false;
        config.lmstudio.base_url = "http://localhost:1234/v1".into();
        let app = build_router(test_state(config));
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["lmstudio"]["enabled"], false);
        assert_eq!(body["lmstudio"]["base_url"], "http://localhost:1234/v1");
    }

    #[tokio::test]
    async fn system_sounds_lists_existing_files() {
        let _guard = env_lock().lock().await;
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("KOMP_SYSTEM_SOUND_DIR", dir.path());
        fs::write(dir.path().join("power_connected.mp3"), b"komp").unwrap();
        let app = build_router(test_state(ErezConfig::default()));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/system-sounds")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        let sounds = body["sounds"].as_array().unwrap();
        assert!(sounds.iter().any(|sound| sound["slot"] == "startup"));
        assert!(sounds.iter().any(|sound| sound["slot"] == "shutdown"));
        assert!(sounds.iter().any(|sound| sound["slot"] == "wake"));
        let power_connected = sounds
            .iter()
            .find(|sound| sound["slot"] == "power_connected")
            .unwrap();
        assert_eq!(power_connected["exists"], true);
        assert!(power_connected["file"]
            .as_str()
            .unwrap()
            .ends_with("power_connected.mp3"));
        std::env::remove_var("KOMP_SYSTEM_SOUND_DIR");
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

    #[test]
    fn battery_sound_slot_covers_all_decade_ranges() {
        assert_eq!(battery_sound_slot(0), "battery_0_10");
        assert_eq!(battery_sound_slot(9), "battery_0_10");
        assert_eq!(battery_sound_slot(10), "battery_10_20");
        assert_eq!(battery_sound_slot(49), "battery_40_50");
        assert_eq!(battery_sound_slot(50), "battery_50_60");
        assert_eq!(battery_sound_slot(59), "battery_50_60");
        assert_eq!(battery_sound_slot(60), "battery_60_70");
        assert_eq!(battery_sound_slot(99), "battery_90_100");
        assert_eq!(battery_sound_slot(100), "battery_100");
    }

    #[test]
    fn legacy_battery_bucket_keeps_old_sound_names_as_fallback() {
        assert_eq!(legacy_battery_bucket(49), None);
        assert_eq!(legacy_battery_bucket(50), Some(50));
        assert_eq!(legacy_battery_bucket(59), Some(50));
        assert_eq!(legacy_battery_bucket(60), Some(60));
        assert_eq!(legacy_battery_bucket(100), Some(100));
    }

    #[test]
    fn parse_percent_finds_pmset_style_percent() {
        assert_eq!(
            parse_percent("Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1234567)\t58%; discharging;"),
            Some(58)
        );
    }

    #[test]
    fn macos_power_parser_tracks_cable_not_charge_verb() {
        assert!(parse_macos_power_connected(
            "Now drawing from 'AC Power'\n -InternalBattery-0\t100%; charged;"
        ));
        assert!(!parse_macos_power_connected(
            "Now drawing from 'Battery Power'\n -InternalBattery-0\t58%; discharging;"
        ));
    }

    #[test]
    fn windows_power_parser_tracks_online_statuses() {
        assert!(!windows_battery_status_power_connected("1"));
        assert!(windows_battery_status_power_connected("2"));
        assert!(windows_battery_status_power_connected("3"));
        assert!(windows_battery_status_power_connected("6"));
        assert!(!windows_battery_status_power_connected("4"));
        assert!(!windows_battery_status_power_connected("5"));
    }

    #[test]
    fn system_intent_resolves_battery_status_without_plugin_scenario() {
        let result = resolve_system_intent("сколько зарядки").unwrap();
        let resolved = result.resolved.unwrap();
        assert_eq!(resolved.source, "system");
        assert_eq!(resolved.command_id.as_deref(), Some("battery_status"));
        assert_eq!(resolved.confidence, 1.0);
        assert!(matches!(resolved.action, Action::EmitEvent { .. }));
    }

    #[tokio::test]
    async fn scenarios_api_creates_updates_and_deletes_user_scenario() {
        let _guard = env_lock().lock().await;
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("KOMP_USER_PLUGIN_DIR", dir.path().join("plugins.user"));

        let mut config = ErezConfig::default();
        config.plugin_dirs = vec![dir.path().join("plugins.example")];
        config.lmstudio.enabled = false;
        let app = build_router(test_state(config));
        let document = json!({
            "manifest_id": "user_open_discord",
            "manifest_name": "Open Discord",
            "enabled": true,
            "scenario": {
                "id": "open_discord",
                "aliases": ["открыть дискорд", "запусти discord"],
                "patterns": [],
                "priority": 20,
                "sounds": {},
                "steps": [
                    { "id": "open", "action": { "type": "open_app", "app": "Discord" } }
                ]
            }
        });

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/scenarios")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&document).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        assert!(dir
            .path()
            .join("plugins.user/scenarios/open_discord/scenario.toml")
            .exists());

        let mut updated = document.clone();
        updated["scenario"]["aliases"] = json!(["открой дискорд"]);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/scenarios/open_discord")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&updated).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let content = fs::read_to_string(
            dir.path()
                .join("plugins.user/scenarios/open_discord/scenario.toml"),
        )
        .unwrap();
        assert!(content.contains("открой дискорд"));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/scenarios/open_discord")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!dir
            .path()
            .join("plugins.user/scenarios/open_discord")
            .exists());
    }

    #[tokio::test]
    async fn scenarios_api_rejects_system_update_and_uploads_sound() {
        let _guard = env_lock().lock().await;
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("KOMP_USER_PLUGIN_DIR", dir.path().join("plugins.user"));
        let system_dir = dir.path().join("plugins.example/scenarios/system");
        fs::create_dir_all(&system_dir).unwrap();
        fs::write(
            system_dir.join("scenario.toml"),
            r#"
id = "system_test"
name = "System Test"
enabled = true

[[scenarios]]
id = "system_test"
aliases = ["system test"]

[[scenarios.steps]]
id = "emit"
action = { type = "emit_event", event = "system_test", payload = {} }
"#,
        )
        .unwrap();

        let mut config = ErezConfig::default();
        config.plugin_dirs = vec![dir.path().join("plugins.example")];
        config.lmstudio.enabled = false;
        let app = build_router(test_state(config));
        let document = json!({
            "manifest_id": "system_test",
            "manifest_name": "System Test",
            "enabled": true,
            "scenario": {
                "id": "system_test",
                "aliases": ["system test"],
                "patterns": [],
                "priority": 0,
                "sounds": {},
                "steps": [
                    { "id": "emit", "action": { "type": "emit_event", "event": "system_test", "payload": {} } }
                ]
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/scenarios/system_test")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&document).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/scenarios/open_discord/sounds")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"file_name":"ok.mp3","data_base64":"a29tcA=="}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(dir
            .path()
            .join("plugins.user/scenarios/open_discord/sounds/ok.mp3")
            .exists());
    }

    #[tokio::test]
    async fn listen_once_resolves_user_scenario_from_plugins_user() {
        let _guard = env_lock().lock().await;
        let dir = tempfile::tempdir().unwrap();
        let user_root = dir.path().join("plugins.user");
        std::env::set_var("KOMP_USER_PLUGIN_DIR", &user_root);
        let scenario_dir = user_root.join("scenarios/user_voice_test");
        fs::create_dir_all(&scenario_dir).unwrap();
        fs::write(
            scenario_dir.join("scenario.toml"),
            r#"
id = "user_voice_test_manifest"
name = "User Voice Test"
enabled = true

[[scenarios]]
id = "user_voice_test"
aliases = ["пользовательский тест"]
priority = 80

[[scenarios.steps]]
id = "emit"
action = { type = "emit_event", event = "user_voice_test", payload = {} }
"#,
        )
        .unwrap();

        let mut config = ErezConfig::default();
        config.plugin_dirs = vec![dir.path().join("plugins.example")];
        config.lmstudio.enabled = false;
        let app = build_router(test_state(config));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/listen/once")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"transcript":"пользовательский тест"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["resolved"]["source"], "scenario");
        assert_eq!(body["resolved"]["plugin_id"], "user_voice_test_manifest");
        assert_eq!(body["resolved"]["command_id"], "user_voice_test");
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
