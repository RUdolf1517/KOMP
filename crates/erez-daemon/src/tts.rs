use base64::{engine::general_purpose, Engine as _};
use erez_core::{
    executor::ActionError, ActionOutcome, AssistantEvent, EventKind, TextSpeaker, TtsConfig,
};
use rodio::{buffer::SamplesBuffer, Decoder, OutputStream, Sink, Source};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{hash_map::DefaultHasher, HashMap, HashSet},
    fs,
    hash::{Hash, Hasher},
    io::{BufReader, Cursor, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::broadcast;

const TARGET_SAMPLE_RATE: u32 = 16_000;
const MIN_VOICE_SECONDS: f32 = 3.0;
const MAX_VOICE_SECONDS: f32 = 15.0;
const PLAYBACK_SETTLE_MS: u64 = 700;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceUploadRequest {
    pub id: String,
    pub prompt_text: String,
    pub file_name: String,
    pub data_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceProfile {
    pub id: String,
    pub prompt_text: String,
    pub prompt_wav: String,
    pub duration_seconds: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct TtsStatus {
    pub provider: String,
    pub label: String,
    pub enabled: bool,
    pub installed: bool,
    pub running: bool,
    pub model_available: bool,
    pub voice_ready: bool,
    pub voice_id: String,
    pub device: String,
    pub base_url: String,
    pub language: String,
    pub license_accepted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TtsProviderInfo {
    pub id: String,
    pub label: String,
    pub selected: bool,
    pub requires_license: bool,
    pub license_accepted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TtsBenchmark {
    pub provider: String,
    pub load_ms: u64,
    pub first_chunk_ms: u64,
    pub total_ms: u64,
    pub audio_duration_ms: u64,
    pub rtf: f64,
    pub sample_rate_hz: u32,
}

pub struct TtsRuntime {
    root: PathBuf,
    config: RwLock<TtsConfig>,
    events: broadcast::Sender<AssistantEvent>,
    audio_playback_until_ms: Arc<AtomicU64>,
    cancel_generation: Arc<AtomicU64>,
    processes: Mutex<HashMap<String, Child>>,
    speak_lock: Mutex<()>,
    starting: Mutex<HashSet<String>>,
}

impl std::fmt::Debug for TtsRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TtsRuntime")
            .field("root", &self.root)
            .finish()
    }
}

impl TtsRuntime {
    pub fn new(
        root: PathBuf,
        config: TtsConfig,
        events: broadcast::Sender<AssistantEvent>,
        audio_playback_until_ms: Arc<AtomicU64>,
        cancel_generation: Arc<AtomicU64>,
    ) -> Self {
        Self {
            root,
            config: RwLock::new(config),
            events,
            audio_playback_until_ms,
            cancel_generation,
            processes: Mutex::new(HashMap::new()),
            speak_lock: Mutex::new(()),
            starting: Mutex::new(HashSet::new()),
        }
    }

    pub fn update_config(&self, config: TtsConfig) {
        *self.config.write().expect("tts config lock poisoned") = config;
    }

    pub fn autostart(self: &Arc<Self>) {
        let config = self.config();
        let provider = normalized_provider(&config.provider);
        if !config.enabled || !self.provider_autostart(&provider, &config) {
            return;
        }
        let runtime = self.clone();
        thread::spawn(move || {
            if let Err(err) = runtime.start_provider(&provider) {
                runtime.emit(
                    EventKind::Error,
                    "TTS autostart failed",
                    json!({"provider": provider, "error": err}),
                );
            }
        });
    }

    pub fn status(&self) -> TtsStatus {
        let config = self.config();
        self.provider_status(&config.provider)
    }

    pub fn providers(&self) -> Vec<TtsProviderInfo> {
        let config = self.config();
        ["xtts", "cosyvoice"]
            .into_iter()
            .map(|provider| TtsProviderInfo {
                id: provider.into(),
                label: provider_label(provider).into(),
                selected: normalized_provider(&config.provider) == provider,
                requires_license: provider == "xtts",
                license_accepted: provider != "xtts" || config.xtts.license_accepted,
            })
            .collect()
    }

    pub fn provider_status(&self, provider: &str) -> TtsStatus {
        let config = self.config();
        let provider = normalized_provider(provider);
        if !is_supported_provider(&provider) {
            return TtsStatus {
                provider,
                label: "Неизвестный движок".into(),
                enabled: config.enabled,
                installed: false,
                running: false,
                model_available: false,
                voice_ready: false,
                voice_id: config.voice_id,
                device: "unknown".into(),
                base_url: String::new(),
                language: String::new(),
                license_accepted: false,
            };
        }
        let python = self.python_path(&provider);
        let model_available = if provider == "xtts" {
            self.root.join("vendor/xtts/model-installed").exists()
        } else {
            self.resolve(&config.model_path).exists()
        };
        TtsStatus {
            provider: provider.clone(),
            label: provider_label(&provider).into(),
            enabled: config.enabled,
            installed: python.exists() && self.server_path(&provider).exists(),
            running: self.health_ok_provider(&provider),
            model_available,
            voice_ready: self.load_voice(&config.voice_id).is_ok(),
            voice_id: config.voice_id.clone(),
            device: self.provider_device(&provider, &config),
            base_url: self.provider_base_url(&provider, &config),
            language: if provider == "xtts" {
                config.xtts.language
            } else {
                "auto".into()
            },
            license_accepted: provider != "xtts" || config.xtts.license_accepted,
        }
    }

    pub fn install(&self) -> Result<String, String> {
        let provider = self.selected_provider();
        self.install_provider(&provider)
    }

    pub fn install_provider(&self, provider: &str) -> Result<String, String> {
        let provider = normalized_provider(provider);
        ensure_supported_provider(&provider)?;
        let config = self.config();
        if provider == "xtts" && !config.xtts.license_accepted {
            return Err(
                "XTTS v2 requires explicit acceptance of the CPML non-commercial license".into(),
            );
        }
        let script_name = match (provider.as_str(), cfg!(target_os = "windows")) {
            ("xtts", true) => "setup-xtts-windows.ps1",
            ("xtts", false) => "setup-xtts.sh",
            (_, true) => "setup-cosyvoice-windows.ps1",
            (_, false) => "setup-cosyvoice.sh",
        };
        let script = self.root.join("scripts").join(script_name);
        if !script.exists() {
            return Err(format!("installer not found: {}", script.display()));
        }
        let mut command = if cfg!(target_os = "windows") {
            let mut command = Command::new("powershell");
            command.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]);
            command.arg(&script);
            command
        } else {
            let mut command = Command::new("bash");
            command.arg(&script);
            command
        };
        command
            .current_dir(&self.root)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        if provider == "xtts" {
            command.env("KOMP_XTTS_ACCEPT_CPML", "1");
        }
        let mut child = command.spawn().map_err(|err| err.to_string())?;
        let events = self.events.clone();
        let event_provider = provider.clone();
        thread::spawn(move || {
            let result = child.wait();
            let (kind, message, data) = match result {
                Ok(status) if status.success() => (
                    EventKind::Status,
                    "TTS installation completed",
                    json!({"provider": event_provider, "success": true}),
                ),
                Ok(status) => (
                    EventKind::Error,
                    "TTS installation failed",
                    json!({"provider": event_provider, "success": false, "status": status.to_string()}),
                ),
                Err(err) => (
                    EventKind::Error,
                    "TTS installer could not be monitored",
                    json!({"provider": event_provider, "error": err.to_string()}),
                ),
            };
            let _ = events.send(AssistantEvent::new(kind, message, data));
        });
        self.emit(
            EventKind::Status,
            "TTS installation started",
            json!({"provider": provider}),
        );
        Ok(format!(
            "{} installation started; progress is shown in daemon logs",
            provider_label(&provider)
        ))
    }

    pub fn start(&self) -> Result<String, String> {
        let provider = self.selected_provider();
        self.start_provider(&provider)
    }

    pub fn start_provider(&self, provider: &str) -> Result<String, String> {
        let provider = normalized_provider(provider);
        ensure_supported_provider(&provider)?;
        if self.health_ok_provider(&provider) {
            return Ok(format!("{} is already running", provider_label(&provider)));
        }
        {
            let mut starting = self.starting.lock().map_err(|err| err.to_string())?;
            if !starting.insert(provider.clone()) {
                return Ok(format!("{} is already starting", provider_label(&provider)));
            }
        }
        let _starting_guard = ProviderStartingGuard {
            provider: provider.clone(),
            starting: &self.starting,
        };
        self.stop_provider_process(&provider, false);
        let start_generation = self.cancel_generation.load(Ordering::SeqCst);
        let config = self.config();
        if provider == "xtts" && !config.xtts.license_accepted {
            return Err("XTTS v2 CPML license has not been accepted".into());
        }
        let python = self.python_path(&provider);
        if !python.exists() {
            return Err(format!(
                "{} is not installed; use the Install button first",
                provider_label(&provider)
            ));
        }
        if provider == "cosyvoice" && !self.resolve(&config.model_path).exists() {
            return Err(format!(
                "CosyVoice model not found: {}",
                self.resolve(&config.model_path).display()
            ));
        }
        let mut command = Command::new(python);
        command.arg(self.server_path(&provider));
        if provider == "xtts" {
            command
                .arg("--model")
                .arg(&config.xtts.model)
                .arg("--device")
                .arg(&config.xtts.device)
                .arg("--port")
                .arg(provider_port(&config.xtts.base_url, 50010).to_string());
            command.env("COQUI_TOS_AGREED", "1");
            command.env("TTS_HOME", self.root.join("vendor/xtts/models"));
        } else {
            command
                .arg("--model-dir")
                .arg(self.resolve(&config.model_path))
                .arg("--device")
                .arg(&config.device)
                .arg("--port")
                .arg(provider_port(&config.base_url, 50000).to_string());
        }
        command
            .current_dir(&self.root)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        if self.provider_device(&provider, &config) == "cpu" {
            command.env("CUDA_VISIBLE_DEVICES", "-1");
        }
        let child = command
            .spawn()
            .map_err(|err| format!("failed to start {}: {err}", provider_label(&provider)))?;
        self.processes
            .lock()
            .expect("tts process lock poisoned")
            .insert(provider.clone(), child);

        let timeout_ms = self.provider_timeout(&provider, &config);
        for _ in 0..(timeout_ms / 500).max(2) {
            if self.cancel_generation.load(Ordering::SeqCst) != start_generation {
                return Err(format!("{} startup cancelled", provider_label(&provider)));
            }
            if self.health_ok_provider(&provider) {
                self.emit(
                    EventKind::Status,
                    "TTS provider ready",
                    json!({"provider": provider, "device": self.provider_device(&provider, &config)}),
                );
                return Ok(format!("{} started", provider_label(&provider)));
            }
            let status = self
                .processes
                .lock()
                .expect("tts process lock poisoned")
                .get_mut(&provider)
                .and_then(|child| child.try_wait().ok())
                .flatten();
            if let Some(status) = status {
                self.processes
                    .lock()
                    .expect("tts process lock poisoned")
                    .remove(&provider);
                return Err(format!(
                    "{} exited during startup: {status}",
                    provider_label(&provider)
                ));
            }
            thread::sleep(Duration::from_millis(500));
        }
        self.stop_provider_process(&provider, false);
        Err(format!(
            "{} model loading timed out",
            provider_label(&provider)
        ))
    }

    pub fn stop(&self) {
        let provider = self.selected_provider();
        self.stop_provider(&provider);
    }

    pub fn stop_all(&self) {
        self.cancel_generation.fetch_add(1, Ordering::SeqCst);
        let providers: Vec<String> = self
            .processes
            .lock()
            .expect("tts process lock poisoned")
            .keys()
            .cloned()
            .collect();
        for provider in providers {
            self.cancel_provider_request(&provider);
            self.stop_provider_process(&provider, true);
        }
    }

    pub fn stop_provider(&self, provider: &str) {
        self.cancel_generation.fetch_add(1, Ordering::SeqCst);
        let provider = normalized_provider(provider);
        self.cancel_provider_request(&provider);
        self.stop_provider_process(&provider, true);
    }

    pub fn cancel_current(&self) {
        self.cancel_provider_request(&self.selected_provider());
    }

    fn cancel_provider_request(&self, provider: &str) {
        let base_url = self.provider_base_url(provider, &self.config());
        let _ = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(300))
            .build()
            .and_then(|client| {
                client
                    .post(format!("{}/v1/cancel", base_url.trim_end_matches('/')))
                    .send()
            });
    }

    fn stop_provider_process(&self, provider: &str, emit_event: bool) {
        if let Some(mut child) = self
            .processes
            .lock()
            .expect("tts process lock poisoned")
            .remove(provider)
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        if emit_event {
            self.emit(
                EventKind::Status,
                "TTS provider stopped",
                json!({"provider": provider}),
            );
        }
    }

    pub fn voices(&self) -> Vec<VoiceProfile> {
        fs::read_dir(self.voice_root())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                self.load_voice(entry.file_name().to_string_lossy().as_ref())
                    .ok()
            })
            .collect()
    }

    pub fn save_voice(&self, request: VoiceUploadRequest) -> Result<VoiceProfile, String> {
        let id = sanitize_id(&request.id);
        if id.is_empty() {
            return Err("voice id is required".into());
        }
        let prompt_text = request.prompt_text.trim();
        let extension = Path::new(&request.file_name)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !matches!(extension.as_str(), "mp3" | "wav" | "ogg") {
            return Err("voice sample must be MP3, WAV or OGG".into());
        }
        let bytes = general_purpose::STANDARD
            .decode(&request.data_base64)
            .map_err(|err| format!("invalid audio data: {err}"))?;
        let (samples, duration) = decode_and_resample(&bytes)?;
        if !(MIN_VOICE_SECONDS..=MAX_VOICE_SECONDS).contains(&duration) {
            return Err(format!(
                "voice sample must be 3-15 seconds, got {duration:.1}"
            ));
        }
        let dir = self.voice_root().join(&id);
        fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
        let wav = dir.join("prompt.wav");
        write_wav(&wav, &samples)?;
        let profile = VoiceProfile {
            id,
            prompt_text: prompt_text.into(),
            prompt_wav: wav.display().to_string(),
            duration_seconds: duration,
        };
        fs::write(
            dir.join("voice.toml"),
            toml::to_string_pretty(&profile).map_err(|err| err.to_string())?,
        )
        .map_err(|err| err.to_string())?;
        self.emit(
            EventKind::Status,
            "TTS voice profile saved",
            json!({"voice_id": profile.id}),
        );
        Ok(profile)
    }

    pub fn delete_voice(&self, id: &str) -> Result<(), String> {
        let id = sanitize_id(id);
        if id.is_empty() {
            return Err("invalid voice id".into());
        }
        let path = self.voice_root().join(id);
        if path.exists() {
            fs::remove_dir_all(path).map_err(|err| err.to_string())?;
        }
        Ok(())
    }

    pub fn speak_text(
        &self,
        text: &str,
        voice: Option<&str>,
        speed: f32,
        cache: bool,
    ) -> Result<ActionOutcome, String> {
        self.speak_text_inner(text, voice, speed, cache, true, None)
    }

    pub fn test_speak_text(
        &self,
        text: &str,
        voice: Option<&str>,
        speed: f32,
        cache: bool,
    ) -> Result<ActionOutcome, String> {
        self.speak_text_inner(text, voice, speed, cache, false, None)
    }

    pub fn test_speak_text_provider(
        &self,
        provider: &str,
        text: &str,
        voice: Option<&str>,
        speed: f32,
        cache: bool,
    ) -> Result<ActionOutcome, String> {
        self.speak_text_inner(text, voice, speed, cache, false, Some(provider))
    }

    fn speak_text_inner(
        &self,
        text: &str,
        voice: Option<&str>,
        speed: f32,
        cache: bool,
        require_enabled: bool,
        provider_override: Option<&str>,
    ) -> Result<ActionOutcome, String> {
        let _single_speech = self.speak_lock.lock().map_err(|err| err.to_string())?;
        let text = text.trim();
        if text.is_empty() {
            return Err("text cannot be empty".into());
        }
        if !(0.5..=2.0).contains(&speed) {
            return Err("speed must be between 0.5 and 2.0".into());
        }
        let config = self.config();
        if require_enabled && !config.enabled {
            return Err("dynamic speech is disabled".into());
        }
        let provider = normalized_provider(provider_override.unwrap_or(&config.provider));
        ensure_supported_provider(&provider)?;
        let voice_id = voice
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&config.voice_id);
        let profile = self.load_voice(voice_id)?;
        let mut generation = self.cancel_generation.load(Ordering::SeqCst);
        let _audio_guard = AudioGuard::new(self.audio_playback_until_ms.clone());
        if provider == "cosyvoice" && profile.prompt_text.trim().is_empty() {
            return Err("CosyVoice requires an exact transcript for this voice profile".into());
        }
        let cache_path = self.cache_path(&provider, &config, &profile, text, voice_id, speed);
        let cached = cache && config.cache_enabled && cache_path.exists();
        let samples = if cached {
            decode_wav_samples(&cache_path)?
        } else {
            if !self.health_ok_provider(&provider) {
                self.start_provider(&provider)?;
            }
            let samples = match self
                .synthesize_and_play(&provider, &config, &profile, text, speed, generation)
            {
                Ok(samples) => samples,
                Err(first_error) if self.cancel_generation.load(Ordering::SeqCst) == generation => {
                    self.emit(
                        EventKind::Error,
                        "TTS synthesis failed; restarting provider once",
                        json!({"provider": provider, "error": first_error}),
                    );
                    self.stop_provider(&provider);
                    self.start_provider(&provider)?;
                    generation = self.cancel_generation.load(Ordering::SeqCst);
                    self.synthesize_and_play(&provider, &config, &profile, text, speed, generation)?
                }
                Err(err) => return Err(err),
            };
            if cache && config.cache_enabled {
                if let Some(parent) = cache_path.parent() {
                    fs::create_dir_all(parent).map_err(|err| err.to_string())?;
                }
                write_wav(&cache_path, &samples)?;
            }
            samples
        };
        if cached {
            self.play_samples(samples, generation)?;
        }
        Ok(ActionOutcome {
            executed: true,
            message: format!("spoke text with voice `{voice_id}` using {provider}"),
            slots: HashMap::new(),
        })
    }

    fn synthesize_and_play(
        &self,
        provider: &str,
        config: &TtsConfig,
        profile: &VoiceProfile,
        text: &str,
        speed: f32,
        generation: u64,
    ) -> Result<Vec<i16>, String> {
        self.emit(
            EventKind::Status,
            "TTS synthesis started",
            json!({"provider": provider, "voice_id": profile.id, "text": text}),
        );
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(
                self.provider_timeout(provider, config).max(1_000),
            ))
            .build()
            .map_err(|err| err.to_string())?;
        let url = format!(
            "{}/v1/synthesize",
            self.provider_base_url(provider, config)
                .trim_end_matches('/')
        );
        let streaming = config.playback_mode.eq_ignore_ascii_case("streaming");
        let mut response = client
            .post(url)
            .json(&json!({
                "text": text,
                "voice_id": profile.id,
                "prompt_text": profile.prompt_text,
                "prompt_wav": profile.prompt_wav,
                "conditioning_path": self.voice_root().join(&profile.id).join("xtts_conditioning.pt"),
                "language": if provider == "xtts" { config.xtts.language.as_str() } else { "auto" },
                "speed": speed,
                "stream": streaming
            }))
            .send()
            .map_err(|err| format!("{} request failed: {err}", provider_label(provider)))?;
        if !response.status().is_success() {
            return Err(format!(
                "{} returned {}: {}",
                provider_label(provider),
                response.status(),
                response.text().unwrap_or_default()
            ));
        }
        let playback = if streaming {
            let (stream, handle) = OutputStream::try_default().map_err(|err| err.to_string())?;
            let sink = Sink::try_new(&handle).map_err(|err| err.to_string())?;
            Some((stream, sink))
        } else {
            None
        };
        let mut samples = Vec::new();
        let mut pending_byte = None;
        let mut chunk = [0_u8; 16_384];
        loop {
            if self.cancel_generation.load(Ordering::SeqCst) != generation {
                if let Some((_, sink)) = &playback {
                    sink.stop();
                }
                return Err("speech cancelled".into());
            }
            let count = response.read(&mut chunk).map_err(|err| err.to_string())?;
            if count == 0 {
                break;
            }
            let mut pcm = Vec::with_capacity((count + usize::from(pending_byte.is_some())) / 2);
            let mut index = 0;
            if let Some(first) = pending_byte.take() {
                pcm.push(i16::from_le_bytes([first, chunk[0]]));
                index = 1;
            }
            while index + 1 < count {
                pcm.push(i16::from_le_bytes([chunk[index], chunk[index + 1]]));
                index += 2;
            }
            if index < count {
                pending_byte = Some(chunk[index]);
            }
            if !pcm.is_empty() {
                samples.extend_from_slice(&pcm);
                if let Some((_, sink)) = &playback {
                    sink.append(SamplesBuffer::new(1, TARGET_SAMPLE_RATE, pcm));
                }
            }
        }
        if samples.is_empty() {
            return Err(format!("{} returned empty audio", provider_label(provider)));
        }
        if let Some((_, sink)) = &playback {
            while !sink.empty() {
                if self.cancel_generation.load(Ordering::SeqCst) != generation {
                    sink.stop();
                    return Err("speech cancelled".into());
                }
                thread::sleep(Duration::from_millis(25));
            }
            self.emit(
                EventKind::Status,
                "TTS speech streamed",
                json!({"provider": provider}),
            );
        } else {
            self.emit(
                EventKind::Status,
                "TTS synthesis completed; playing buffered speech",
                json!({"provider": provider, "samples": samples.len()}),
            );
            self.play_samples(samples.clone(), generation)?;
        }
        Ok(samples)
    }

    fn play_samples(&self, samples: Vec<i16>, generation: u64) -> Result<(), String> {
        self.audio_playback_until_ms
            .store(now_ms().saturating_add(60_000), Ordering::SeqCst);
        let (_stream, handle) = OutputStream::try_default().map_err(|err| err.to_string())?;
        let sink = Sink::try_new(&handle).map_err(|err| err.to_string())?;
        sink.append(SamplesBuffer::new(1, TARGET_SAMPLE_RATE, samples));
        while !sink.empty() {
            if self.cancel_generation.load(Ordering::SeqCst) != generation {
                sink.stop();
                self.audio_playback_until_ms.store(
                    now_ms().saturating_add(PLAYBACK_SETTLE_MS),
                    Ordering::SeqCst,
                );
                return Err("speech cancelled".into());
            }
            thread::sleep(Duration::from_millis(25));
        }
        self.audio_playback_until_ms.store(
            now_ms().saturating_add(PLAYBACK_SETTLE_MS),
            Ordering::SeqCst,
        );
        self.emit(EventKind::Status, "TTS speech played", json!({}));
        Ok(())
    }

    fn health_ok_provider(&self, provider: &str) -> bool {
        let config = self.config();
        let base_url = self.provider_base_url(provider, &config);
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(350))
            .build()
            .and_then(|client| {
                client
                    .get(format!("{}/health", base_url.trim_end_matches('/')))
                    .send()
            })
            .map(|response| response.status().is_success())
            .unwrap_or(false)
    }

    fn load_voice(&self, id: &str) -> Result<VoiceProfile, String> {
        let id = sanitize_id(id);
        let path = self.voice_root().join(id).join("voice.toml");
        let content = fs::read_to_string(&path)
            .map_err(|_| format!("voice profile not found: {}", path.display()))?;
        toml::from_str(&content).map_err(|err| err.to_string())
    }

    fn cache_path(
        &self,
        provider: &str,
        config: &TtsConfig,
        profile: &VoiceProfile,
        text: &str,
        voice: &str,
        speed: f32,
    ) -> PathBuf {
        let mut hasher = DefaultHasher::new();
        provider.hash(&mut hasher);
        if provider == "xtts" {
            config.xtts.model.hash(&mut hasher);
            config.xtts.language.hash(&mut hasher);
        } else {
            config.model_path.hash(&mut hasher);
        }
        text.hash(&mut hasher);
        voice.hash(&mut hasher);
        speed.to_bits().hash(&mut hasher);
        fs::metadata(&profile.prompt_wav)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .hash(&mut hasher);
        self.root
            .join("cache/tts")
            .join(provider)
            .join(format!("{:016x}.wav", hasher.finish()))
    }

    pub fn benchmark_provider(
        &self,
        provider: &str,
        text: &str,
        voice: Option<&str>,
    ) -> Result<TtsBenchmark, String> {
        let provider = normalized_provider(provider);
        ensure_supported_provider(&provider)?;
        let config = self.config();
        let voice_id = voice
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&config.voice_id);
        let profile = self.load_voice(voice_id)?;
        let load_started = Instant::now();
        if !self.health_ok_provider(&provider) {
            self.start_provider(&provider)?;
        }
        let load_ms = elapsed_ms(load_started);
        let generation = self.cancel_generation.load(Ordering::SeqCst);
        let started = Instant::now();
        let (samples, first_chunk_ms) =
            self.synthesize_samples(&provider, &config, &profile, text, 1.0, generation)?;
        let total_ms = elapsed_ms(started);
        let audio_duration_ms =
            ((samples.len() as u128 * 1_000) / TARGET_SAMPLE_RATE as u128) as u64;
        let rtf = if audio_duration_ms == 0 {
            0.0
        } else {
            total_ms as f64 / audio_duration_ms as f64
        };
        Ok(TtsBenchmark {
            provider,
            load_ms,
            first_chunk_ms,
            total_ms,
            audio_duration_ms,
            rtf,
            sample_rate_hz: TARGET_SAMPLE_RATE,
        })
    }

    fn synthesize_samples(
        &self,
        provider: &str,
        config: &TtsConfig,
        profile: &VoiceProfile,
        text: &str,
        speed: f32,
        generation: u64,
    ) -> Result<(Vec<i16>, u64), String> {
        let started = Instant::now();
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(
                self.provider_timeout(provider, config).max(1_000),
            ))
            .build()
            .map_err(|err| err.to_string())?;
        let mut response = client
            .post(format!("{}/v1/synthesize", self.provider_base_url(provider, config).trim_end_matches('/')))
            .json(&json!({
                "text": text,
                "voice_id": profile.id,
                "prompt_text": profile.prompt_text,
                "prompt_wav": profile.prompt_wav,
                "conditioning_path": self.voice_root().join(&profile.id).join("xtts_conditioning.pt"),
                "language": if provider == "xtts" { config.xtts.language.as_str() } else { "auto" },
                "speed": speed,
                "stream": true
            }))
            .send().map_err(|err| format!("{} request failed: {err}", provider_label(provider)))?;
        if !response.status().is_success() {
            return Err(format!(
                "{} returned {}: {}",
                provider_label(provider),
                response.status(),
                response.text().unwrap_or_default()
            ));
        }
        let mut samples = Vec::new();
        let mut first_chunk_ms = 0;
        let mut pending = None;
        let mut chunk = [0_u8; 16_384];
        loop {
            if self.cancel_generation.load(Ordering::SeqCst) != generation {
                return Err("speech cancelled".into());
            }
            let count = response.read(&mut chunk).map_err(|err| err.to_string())?;
            if count == 0 {
                break;
            }
            if first_chunk_ms == 0 {
                first_chunk_ms = elapsed_ms(started).max(1);
            }
            let mut index = 0;
            if let Some(first) = pending.take() {
                samples.push(i16::from_le_bytes([first, chunk[0]]));
                index = 1;
            }
            while index + 1 < count {
                samples.push(i16::from_le_bytes([chunk[index], chunk[index + 1]]));
                index += 2;
            }
            if index < count {
                pending = Some(chunk[index]);
            }
        }
        if samples.is_empty() {
            return Err(format!("{} returned empty audio", provider_label(provider)));
        }
        Ok((samples, first_chunk_ms))
    }

    fn config(&self) -> TtsConfig {
        self.config
            .read()
            .expect("tts config lock poisoned")
            .clone()
    }

    fn resolve(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        }
    }

    fn selected_provider(&self) -> String {
        normalized_provider(&self.config().provider)
    }

    fn provider_base_url(&self, provider: &str, config: &TtsConfig) -> String {
        if provider == "xtts" {
            config.xtts.base_url.clone()
        } else {
            config.base_url.clone()
        }
    }

    fn provider_device(&self, provider: &str, config: &TtsConfig) -> String {
        if provider == "xtts" {
            config.xtts.device.clone()
        } else {
            config.device.clone()
        }
    }

    fn provider_timeout(&self, provider: &str, config: &TtsConfig) -> u64 {
        if provider == "xtts" {
            config.xtts.timeout_ms
        } else {
            config.timeout_ms
        }
    }

    fn provider_autostart(&self, provider: &str, config: &TtsConfig) -> bool {
        if provider == "xtts" {
            config.xtts.autostart && config.xtts.license_accepted
        } else {
            config.autostart
        }
    }

    fn python_path(&self, provider: &str) -> PathBuf {
        let runtime = if provider == "xtts" {
            "xtts"
        } else {
            "cosyvoice"
        };
        if cfg!(target_os = "windows") {
            self.root
                .join("vendor")
                .join(runtime)
                .join(".venv/Scripts/python.exe")
        } else {
            self.root
                .join("vendor")
                .join(runtime)
                .join(".venv/bin/python")
        }
    }

    fn server_path(&self, provider: &str) -> PathBuf {
        self.root
            .join("services")
            .join(if provider == "xtts" {
                "xtts"
            } else {
                "cosyvoice"
            })
            .join("server.py")
    }
    fn voice_root(&self) -> PathBuf {
        self.root.join("voices")
    }

    fn emit(&self, kind: EventKind, message: &str, data: serde_json::Value) {
        let _ = self.events.send(AssistantEvent::new(kind, message, data));
    }
}

impl TextSpeaker for TtsRuntime {
    fn speak(
        &self,
        text: &str,
        voice: Option<&str>,
        speed: f32,
        cache: bool,
    ) -> Result<ActionOutcome, ActionError> {
        thread::scope(|scope| {
            scope
                .spawn(|| self.speak_text(text, voice, speed, cache))
                .join()
                .map_err(|_| ActionError::Invalid("TTS worker panicked".into()))?
                .map_err(ActionError::Invalid)
        })
    }
}

fn sanitize_id(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .collect()
}

fn normalized_provider(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "xtts" | "xtts_v2" | "xtts-v2" => "xtts".into(),
        "cosy" | "cosyvoice" => "cosyvoice".into(),
        other => other.into(),
    }
}

fn is_supported_provider(provider: &str) -> bool {
    matches!(provider, "xtts" | "cosyvoice")
}

fn ensure_supported_provider(provider: &str) -> Result<(), String> {
    if is_supported_provider(provider) {
        Ok(())
    } else {
        Err(format!("unsupported TTS provider `{provider}`"))
    }
}

fn provider_label(provider: &str) -> &'static str {
    if provider == "xtts" {
        "XTTS v2"
    } else {
        "CosyVoice"
    }
}

fn provider_port(base_url: &str, fallback: u16) -> u16 {
    base_url
        .rsplit_once(':')
        .and_then(|(_, port)| port.trim_end_matches('/').parse().ok())
        .unwrap_or(fallback)
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

fn decode_and_resample(bytes: &[u8]) -> Result<(Vec<i16>, f32), String> {
    let decoder = Decoder::new(BufReader::new(Cursor::new(bytes.to_vec())))
        .map_err(|err| format!("failed to decode voice sample: {err}"))?;
    let source_rate = decoder.sample_rate();
    let channels = decoder.channels() as usize;
    let samples: Vec<f32> = decoder.convert_samples::<f32>().collect();
    if samples.is_empty() || channels == 0 {
        return Err("voice sample is empty".into());
    }
    let mono: Vec<f32> = samples
        .chunks(channels)
        .map(|frame| frame.iter().copied().sum::<f32>() / frame.len() as f32)
        .collect();
    let rms = (mono.iter().map(|sample| sample * sample).sum::<f32>() / mono.len() as f32).sqrt();
    if rms < 0.002 {
        return Err("voice sample does not contain audible speech".into());
    }
    let duration = mono.len() as f32 / source_rate as f32;
    let target_len = (duration * TARGET_SAMPLE_RATE as f32).round() as usize;
    let mut output = Vec::with_capacity(target_len);
    for index in 0..target_len {
        let position = index as f64 * source_rate as f64 / TARGET_SAMPLE_RATE as f64;
        let left = position.floor() as usize;
        let right = (left + 1).min(mono.len() - 1);
        let fraction = (position - left as f64) as f32;
        let sample = mono[left] * (1.0 - fraction) + mono[right] * fraction;
        output.push((sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
    }
    Ok((output, duration))
}

fn write_wav(path: &Path, samples: &[i16]) -> Result<(), String> {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: TARGET_SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .map_err(|err| err.to_string())?;
    for sample in samples {
        writer
            .write_sample(*sample)
            .map_err(|err| err.to_string())?;
    }
    writer.finalize().map_err(|err| err.to_string())
}

fn decode_wav_samples(path: &Path) -> Result<Vec<i16>, String> {
    let reader = hound::WavReader::open(path).map_err(|err| err.to_string())?;
    reader
        .into_samples::<i16>()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| err.to_string())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

struct AudioGuard {
    guard: Arc<AtomicU64>,
}

impl AudioGuard {
    fn new(guard: Arc<AtomicU64>) -> Self {
        guard.store(now_ms().saturating_add(60_000), Ordering::SeqCst);
        Self { guard }
    }
}

impl Drop for AudioGuard {
    fn drop(&mut self) {
        self.guard.store(
            now_ms().saturating_add(PLAYBACK_SETTLE_MS),
            Ordering::SeqCst,
        );
    }
}

struct ProviderStartingGuard<'a> {
    provider: String,
    starting: &'a Mutex<HashSet<String>>,
}

impl Drop for ProviderStartingGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut starting) = self.starting.lock() {
            starting.remove(&self.provider);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime(root: &Path) -> TtsRuntime {
        let (events, _) = broadcast::channel(8);
        TtsRuntime::new(
            root.to_path_buf(),
            TtsConfig::default(),
            events,
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        )
    }

    #[test]
    fn voice_upload_normalizes_wav_and_writes_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.wav");
        let mut writer = hound::WavWriter::create(
            &input,
            hound::WavSpec {
                channels: 1,
                sample_rate: 8_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for index in 0..32_000 {
            let sample = (((index as f32 / 20.0).sin()) * 10_000.0) as i16;
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
        let profile = runtime(dir.path())
            .save_voice(VoiceUploadRequest {
                id: "komp_test".into(),
                prompt_text: "Это точная тестовая фраза".into(),
                file_name: "input.wav".into(),
                data_base64: general_purpose::STANDARD.encode(fs::read(input).unwrap()),
            })
            .unwrap();
        assert!((3.9..=4.1).contains(&profile.duration_seconds));
        let reader = hound::WavReader::open(&profile.prompt_wav).unwrap();
        assert_eq!(reader.spec().sample_rate, TARGET_SAMPLE_RATE);
        assert!(dir.path().join("voices/komp_test/voice.toml").exists());
    }

    #[test]
    fn voice_upload_rejects_short_samples() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("short.wav");
        let mut writer = hound::WavWriter::create(
            &input,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for index in 0..16_000 {
            writer
                .write_sample((((index as f32 / 20.0).sin()) * 10_000.0) as i16)
                .unwrap();
        }
        writer.finalize().unwrap();
        let result = runtime(dir.path()).save_voice(VoiceUploadRequest {
            id: "short".into(),
            prompt_text: "коротко".into(),
            file_name: "short.wav".into(),
            data_base64: general_purpose::STANDARD.encode(fs::read(input).unwrap()),
        });
        assert!(result.unwrap_err().contains("3-15 seconds"));
    }

    #[test]
    fn cache_is_separated_by_provider_model_language_and_sample() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path());
        let prompt = dir.path().join("prompt.wav");
        fs::write(&prompt, b"voice-one").unwrap();
        let profile = VoiceProfile {
            id: "cave".into(),
            prompt_text: "sample".into(),
            prompt_wav: prompt.display().to_string(),
            duration_seconds: 4.0,
        };
        let config = TtsConfig::default();
        let cosy = runtime.cache_path("cosyvoice", &config, &profile, "привет", "cave", 1.0);
        let xtts = runtime.cache_path("xtts", &config, &profile, "привет", "cave", 1.0);
        assert_ne!(cosy, xtts);
        assert!(cosy.to_string_lossy().contains("cache/tts/cosyvoice"));
        assert!(xtts.to_string_lossy().contains("cache/tts/xtts"));
    }

    #[test]
    fn provider_aliases_are_normalized_without_fallback_selection() {
        assert_eq!(normalized_provider("xtts-v2"), "xtts");
        assert_eq!(normalized_provider("cosyvoice"), "cosyvoice");
        assert_eq!(normalized_provider("unknown"), "unknown");
    }
}
