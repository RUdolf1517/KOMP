use base64::{engine::general_purpose, Engine as _};
use erez_core::{
    executor::ActionError, ActionOutcome, AssistantEvent, EventKind, TextSpeaker, TtsConfig,
};
use rodio::{buffer::SamplesBuffer, Decoder, OutputStream, Sink, Source};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    io::{BufReader, Cursor, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
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
    pub enabled: bool,
    pub installed: bool,
    pub running: bool,
    pub model_available: bool,
    pub voice_ready: bool,
    pub voice_id: String,
    pub device: String,
    pub base_url: String,
}

pub struct TtsRuntime {
    root: PathBuf,
    config: RwLock<TtsConfig>,
    events: broadcast::Sender<AssistantEvent>,
    audio_playback_until_ms: Arc<AtomicU64>,
    cancel_generation: Arc<AtomicU64>,
    process: Mutex<Option<Child>>,
    speak_lock: Mutex<()>,
    starting: AtomicBool,
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
            process: Mutex::new(None),
            speak_lock: Mutex::new(()),
            starting: AtomicBool::new(false),
        }
    }

    pub fn update_config(&self, config: TtsConfig) {
        *self.config.write().expect("tts config lock poisoned") = config;
    }

    pub fn autostart(self: &Arc<Self>) {
        let config = self.config();
        if !config.enabled || !config.autostart {
            return;
        }
        let runtime = self.clone();
        thread::spawn(move || {
            if let Err(err) = runtime.start() {
                runtime.emit(
                    EventKind::Error,
                    "CosyVoice autostart failed",
                    json!({"error": err}),
                );
            }
        });
    }

    pub fn status(&self) -> TtsStatus {
        let config = self.config();
        let python = self.python_path();
        let model = self.resolve(&config.model_path);
        TtsStatus {
            enabled: config.enabled,
            installed: python.exists() && self.server_path().exists(),
            running: self.health_ok(),
            model_available: model.exists(),
            voice_ready: self.load_voice(&config.voice_id).is_ok(),
            voice_id: config.voice_id,
            device: config.device,
            base_url: config.base_url,
        }
    }

    pub fn install(&self) -> Result<String, String> {
        let script = if cfg!(target_os = "windows") {
            self.root.join("scripts/setup-cosyvoice-windows.ps1")
        } else {
            self.root.join("scripts/setup-cosyvoice.sh")
        };
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
        let mut child = command.spawn().map_err(|err| err.to_string())?;
        let events = self.events.clone();
        thread::spawn(move || {
            let result = child.wait();
            let (kind, message, data) = match result {
                Ok(status) if status.success() => (
                    EventKind::Status,
                    "CosyVoice installation completed",
                    json!({"success": true}),
                ),
                Ok(status) => (
                    EventKind::Error,
                    "CosyVoice installation failed",
                    json!({"success": false, "status": status.to_string()}),
                ),
                Err(err) => (
                    EventKind::Error,
                    "CosyVoice installer could not be monitored",
                    json!({"error": err.to_string()}),
                ),
            };
            let _ = events.send(AssistantEvent::new(kind, message, data));
        });
        self.emit(
            EventKind::Status,
            "CosyVoice installation started",
            json!({}),
        );
        Ok("CosyVoice installation started; progress is shown in daemon logs".into())
    }

    pub fn start(&self) -> Result<String, String> {
        if self.health_ok() {
            return Ok("CosyVoice is already running".into());
        }
        if self.starting.swap(true, Ordering::SeqCst) {
            return Ok("CosyVoice is already starting".into());
        }
        let _starting_guard = StartingGuard(&self.starting);
        self.stop();
        let start_generation = self.cancel_generation.load(Ordering::SeqCst);
        let config = self.config();
        let python = self.python_path();
        if !python.exists() {
            return Err("CosyVoice is not installed; use the Install button first".into());
        }
        let model = self.resolve(&config.model_path);
        if !model.exists() {
            return Err(format!("CosyVoice model not found: {}", model.display()));
        }
        let mut command = Command::new(python);
        command
            .arg(self.server_path())
            .arg("--model-dir")
            .arg(model)
            .arg("--device")
            .arg(&config.device)
            .current_dir(&self.root)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        if cfg!(target_os = "macos") || config.device == "cpu" {
            command.env("CUDA_VISIBLE_DEVICES", "-1");
        }
        let child = command
            .spawn()
            .map_err(|err| format!("failed to start CosyVoice: {err}"))?;
        *self.process.lock().expect("tts process lock poisoned") = Some(child);

        for _ in 0..600 {
            if self.cancel_generation.load(Ordering::SeqCst) != start_generation {
                return Err("CosyVoice startup cancelled".into());
            }
            if self.health_ok() {
                self.emit(
                    EventKind::Status,
                    "CosyVoice ready",
                    json!({"device": config.device}),
                );
                return Ok("CosyVoice started".into());
            }
            let status = self
                .process
                .lock()
                .expect("tts process lock poisoned")
                .as_mut()
                .and_then(|child| child.try_wait().ok())
                .flatten();
            if let Some(status) = status {
                self.process
                    .lock()
                    .expect("tts process lock poisoned")
                    .take();
                return Err(format!("CosyVoice exited during startup: {status}"));
            }
            thread::sleep(Duration::from_millis(500));
        }
        self.stop();
        Err("CosyVoice model loading timed out".into())
    }

    pub fn stop(&self) {
        self.cancel_generation.fetch_add(1, Ordering::SeqCst);
        if let Some(mut child) = self
            .process
            .lock()
            .expect("tts process lock poisoned")
            .take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.emit(EventKind::Status, "CosyVoice stopped", json!({}));
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
        if prompt_text.is_empty() {
            return Err("exact prompt transcript is required".into());
        }
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
            "CosyVoice profile saved",
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
        self.speak_text_inner(text, voice, speed, cache, true)
    }

    pub fn test_speak_text(
        &self,
        text: &str,
        voice: Option<&str>,
        speed: f32,
        cache: bool,
    ) -> Result<ActionOutcome, String> {
        self.speak_text_inner(text, voice, speed, cache, false)
    }

    fn speak_text_inner(
        &self,
        text: &str,
        voice: Option<&str>,
        speed: f32,
        cache: bool,
        require_enabled: bool,
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
            return Err("CosyVoice is disabled".into());
        }
        let voice_id = voice
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&config.voice_id);
        let profile = self.load_voice(voice_id)?;
        let mut generation = self.cancel_generation.load(Ordering::SeqCst);
        let _audio_guard = AudioGuard::new(self.audio_playback_until_ms.clone());
        let cache_path = self.cache_path(text, voice_id, speed);
        let cached = cache && config.cache_enabled && cache_path.exists();
        let samples = if cached {
            decode_wav_samples(&cache_path)?
        } else {
            if !self.health_ok() {
                self.start()?;
            }
            let samples = match self.synthesize_and_play(&config, &profile, text, speed, generation)
            {
                Ok(samples) => samples,
                Err(first_error) if self.cancel_generation.load(Ordering::SeqCst) == generation => {
                    self.emit(
                        EventKind::Error,
                        "CosyVoice synthesis failed; restarting once",
                        json!({"error": first_error}),
                    );
                    self.stop();
                    self.start()?;
                    generation = self.cancel_generation.load(Ordering::SeqCst);
                    self.synthesize_and_play(&config, &profile, text, speed, generation)?
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
            message: format!("spoke text with voice `{voice_id}`"),
        })
    }

    fn synthesize_and_play(
        &self,
        config: &TtsConfig,
        profile: &VoiceProfile,
        text: &str,
        speed: f32,
        generation: u64,
    ) -> Result<Vec<i16>, String> {
        self.emit(
            EventKind::Status,
            "CosyVoice synthesis started",
            json!({"voice_id": profile.id, "text": text}),
        );
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms.max(1_000)))
            .build()
            .map_err(|err| err.to_string())?;
        let url = format!("{}/v1/synthesize", config.base_url.trim_end_matches('/'));
        let mut response = client
            .post(url)
            .json(&json!({
                "text": text,
                "voice_id": profile.id,
                "prompt_text": profile.prompt_text,
                "prompt_wav": profile.prompt_wav,
                "speed": speed,
                "stream": config.playback_mode.eq_ignore_ascii_case("streaming")
            }))
            .send()
            .map_err(|err| format!("CosyVoice request failed: {err}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "CosyVoice returned {}: {}",
                response.status(),
                response.text().unwrap_or_default()
            ));
        }
        let streaming = config.playback_mode.eq_ignore_ascii_case("streaming");
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
            return Err("CosyVoice returned empty audio".into());
        }
        if let Some((_, sink)) = &playback {
            while !sink.empty() {
                if self.cancel_generation.load(Ordering::SeqCst) != generation {
                    sink.stop();
                    return Err("speech cancelled".into());
                }
                thread::sleep(Duration::from_millis(25));
            }
            self.emit(EventKind::Status, "CosyVoice speech streamed", json!({}));
        } else {
            self.emit(
                EventKind::Status,
                "CosyVoice synthesis completed; playing buffered speech",
                json!({"samples": samples.len()}),
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
        self.emit(EventKind::Status, "CosyVoice speech played", json!({}));
        Ok(())
    }

    fn health_ok(&self) -> bool {
        let config = self.config();
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(350))
            .build()
            .and_then(|client| {
                client
                    .get(format!("{}/health", config.base_url.trim_end_matches('/')))
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

    fn cache_path(&self, text: &str, voice: &str, speed: f32) -> PathBuf {
        let mut hasher = DefaultHasher::new();
        "cosyvoice3-0.5b-2512".hash(&mut hasher);
        text.hash(&mut hasher);
        voice.hash(&mut hasher);
        speed.to_bits().hash(&mut hasher);
        self.root
            .join("cache/tts")
            .join(format!("{:016x}.wav", hasher.finish()))
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

    fn python_path(&self) -> PathBuf {
        if cfg!(target_os = "windows") {
            self.root.join("vendor/cosyvoice/.venv/Scripts/python.exe")
        } else {
            self.root.join("vendor/cosyvoice/.venv/bin/python")
        }
    }

    fn server_path(&self) -> PathBuf {
        self.root.join("services/cosyvoice/server.py")
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
                .map_err(|_| ActionError::Invalid("CosyVoice worker panicked".into()))?
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

struct StartingGuard<'a>(&'a AtomicBool);

impl Drop for StartingGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
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
}
