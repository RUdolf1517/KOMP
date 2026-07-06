use crate::{
    config::{ErezConfig, Language},
    normalize::matches_wake_phrase,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SttError {
    #[error("vosk model for {0:?} is not configured")]
    ModelNotConfigured(Language),
    #[error("offline recognizer backend is not linked in this build")]
    BackendUnavailable,
    #[error("failed to load vosk model for {language:?} from {path}")]
    ModelLoadFailed { language: Language, path: String },
    #[error("failed to create vosk recognizer for {0:?}")]
    RecognizerCreateFailed(Language),
    #[error("vosk failed to accept waveform: {0}")]
    AcceptWaveform(String),
    #[error("whisper.cpp is enabled but cli_path or model_path is missing")]
    WhisperNotConfigured,
    #[error("failed to run whisper.cpp: {0}")]
    WhisperIo(#[from] std::io::Error),
    #[error("whisper.cpp exited with {status}: {stderr}")]
    WhisperFailed { status: String, stderr: String },
    #[error("whisper.cpp timed out after {0} ms")]
    WhisperTimeout(u64),
    #[error("failed to write temporary whisper wav: {0}")]
    WhisperWav(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Transcript {
    pub text: String,
    pub language: Language,
    pub confidence: f32,
}

pub trait SpeechRecognizer {
    fn transcribe(
        &mut self,
        pcm_i16_16khz: &[i16],
        language: Language,
    ) -> Result<Transcript, SttError>;
}

pub mod whisper_backend {
    use super::{Language, SpeechRecognizer, SttError, Transcript};
    use crate::{
        audio::write_wav_mono_i16,
        config::{ErezConfig, WhisperConfig},
        normalize::normalize_phrase,
    };
    use std::{
        path::PathBuf,
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };
    use uuid::Uuid;

    #[derive(Debug, Clone)]
    pub struct WhisperCppRecognizer {
        config: WhisperConfig,
        sample_rate_hz: u32,
    }

    impl WhisperCppRecognizer {
        pub fn from_config(config: &ErezConfig) -> Result<Option<Self>, SttError> {
            if !config.whisper.enabled {
                return Ok(None);
            }
            if config.whisper.cli_path.is_none() || config.whisper.model_path.is_none() {
                return Err(SttError::WhisperNotConfigured);
            }
            Ok(Some(Self {
                config: config.whisper.clone(),
                sample_rate_hz: config.audio.sample_rate_hz,
            }))
        }
    }

    impl SpeechRecognizer for WhisperCppRecognizer {
        fn transcribe(
            &mut self,
            pcm_i16_16khz: &[i16],
            language: Language,
        ) -> Result<Transcript, SttError> {
            let cli_path = self
                .config
                .cli_path
                .as_ref()
                .ok_or(SttError::WhisperNotConfigured)?;
            let model_path = self
                .config
                .model_path
                .as_ref()
                .ok_or(SttError::WhisperNotConfigured)?;
            let wav_path = temp_wav_path();
            write_wav_mono_i16(&wav_path, pcm_i16_16khz, self.sample_rate_hz)
                .map_err(|err| SttError::WhisperWav(err.to_string()))?;

            let whisper_language = self
                .config
                .language
                .clone()
                .unwrap_or_else(|| language_code(language).to_string());
            let mut command = Command::new(cli_path);
            command
                .arg("-m")
                .arg(model_path)
                .arg("-f")
                .arg(&wav_path)
                .arg("-l")
                .arg(whisper_language)
                .args(&self.config.extra_args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());

            let output = run_with_timeout(command, Duration::from_millis(self.config.timeout_ms))?;
            let _ = std::fs::remove_file(&wav_path);
            if !output.status.success() {
                return Err(SttError::WhisperFailed {
                    status: output.status.to_string(),
                    stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
                });
            }

            let raw = String::from_utf8_lossy(&output.stdout);
            let text = clean_whisper_output(&raw);
            Ok(Transcript {
                text,
                language,
                confidence: 0.90,
            })
        }
    }

    fn run_with_timeout(
        mut command: Command,
        timeout: Duration,
    ) -> Result<std::process::Output, SttError> {
        let start = Instant::now();
        let mut child = command.spawn()?;
        loop {
            if child.try_wait()?.is_some() {
                return child.wait_with_output().map_err(SttError::WhisperIo);
            }
            if start.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(SttError::WhisperTimeout(timeout.as_millis() as u64));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn clean_whisper_output(output: &str) -> String {
        let mut text = String::new();
        for line in output.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty()
                || trimmed.starts_with("whisper_")
                || trimmed.starts_with("main:")
                || trimmed.starts_with("system_info:")
            {
                continue;
            }
            let without_timestamp = strip_timestamp_prefix(trimmed);
            if !without_timestamp.is_empty() {
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(without_timestamp);
            }
        }
        normalize_phrase(&text)
    }

    fn strip_timestamp_prefix(line: &str) -> &str {
        let Some(rest) = line.strip_prefix('[') else {
            return line;
        };
        let Some(index) = rest.find(']') else {
            return line;
        };
        rest[index + 1..].trim()
    }

    fn temp_wav_path() -> PathBuf {
        std::env::temp_dir().join(format!("komp-whisper-{}.wav", Uuid::new_v4()))
    }

    fn language_code(language: Language) -> &'static str {
        match language {
            Language::Ru => "ru",
            Language::En => "en",
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn cleans_whisper_output() {
            assert_eq!(
                clean_whisper_output("[00:00:00.000 --> 00:00:01.000] Открой браузер"),
                "открой браузер"
            );
        }
    }
}

#[derive(Debug, Clone)]
pub struct WakePhraseDetector {
    grammar: Vec<String>,
}

impl WakePhraseDetector {
    pub fn new(config: &ErezConfig) -> Self {
        Self {
            grammar: config.effective_wake_grammar(),
        }
    }

    pub fn accepts_text(&self, text: &str) -> bool {
        matches_wake_phrase(text, &self.grammar)
    }
}

#[cfg(feature = "vosk-stt")]
pub mod vosk_backend {
    use super::{Language, SpeechRecognizer, SttError, Transcript};
    use crate::config::ErezConfig;
    use std::path::Path;
    use vosk::{CompleteResult, Model, Recognizer};

    pub struct VoskSpeechRecognizer {
        ru_model: Option<Model>,
        en_model: Option<Model>,
        sample_rate_hz: f32,
    }

    pub struct VoskWakeRecognizer {
        recognizer: Recognizer,
        grammar: Vec<String>,
    }

    impl VoskSpeechRecognizer {
        pub fn from_config(config: &ErezConfig) -> Result<Self, SttError> {
            Ok(Self {
                ru_model: load_model(Language::Ru, config.models.ru_vosk_path.as_deref())?,
                en_model: load_model(Language::En, config.models.en_vosk_path.as_deref())?,
                sample_rate_hz: config.audio.sample_rate_hz as f32,
            })
        }

        pub fn wake_recognizer(&self, grammar: &[String]) -> Result<Recognizer, SttError> {
            let model = self
                .model(Language::Ru)
                .ok_or(SttError::ModelNotConfigured(Language::Ru))?;
            Recognizer::new_with_grammar(model, self.sample_rate_hz, grammar)
                .ok_or(SttError::RecognizerCreateFailed(Language::Ru))
        }

        pub fn wake_detector(&self, grammar: &[String]) -> Result<VoskWakeRecognizer, SttError> {
            Ok(VoskWakeRecognizer {
                recognizer: self.wake_recognizer(grammar)?,
                grammar: grammar.to_vec(),
            })
        }

        fn model(&self, language: Language) -> Option<&Model> {
            match language {
                Language::Ru => self.ru_model.as_ref(),
                Language::En => self.en_model.as_ref(),
            }
        }
    }

    impl VoskWakeRecognizer {
        pub fn reset(&mut self) {
            self.recognizer.reset();
        }

        pub fn accept_frame(&mut self, pcm_i16: &[i16]) -> Result<Option<String>, SttError> {
            let state = self
                .recognizer
                .accept_waveform(pcm_i16)
                .map_err(|err| SttError::AcceptWaveform(err.to_string()))?;
            let partial = self.recognizer.partial_result().partial.to_string();
            if crate::normalize::matches_wake_phrase(&partial, &self.grammar) {
                self.recognizer.reset();
                return Ok(Some(partial));
            }

            if matches!(state, vosk::DecodingState::Finalized) {
                let finalized = complete_result_text(self.recognizer.result());
                if crate::normalize::matches_wake_phrase(&finalized, &self.grammar) {
                    self.recognizer.reset();
                    return Ok(Some(finalized));
                }
            }

            Ok(None)
        }
    }

    impl SpeechRecognizer for VoskSpeechRecognizer {
        fn transcribe(
            &mut self,
            pcm_i16_16khz: &[i16],
            language: Language,
        ) -> Result<Transcript, SttError> {
            let model = self
                .model(language)
                .ok_or(SttError::ModelNotConfigured(language))?;
            let mut recognizer = Recognizer::new(model, self.sample_rate_hz)
                .ok_or(SttError::RecognizerCreateFailed(language))?;
            recognizer.set_words(true);
            recognizer
                .accept_waveform(pcm_i16_16khz)
                .map_err(|err| SttError::AcceptWaveform(err.to_string()))?;
            complete_result_to_transcript(recognizer.final_result(), language)
        }
    }

    fn load_model(language: Language, path: Option<&Path>) -> Result<Option<Model>, SttError> {
        let Some(path) = path else {
            return Ok(None);
        };
        let display = path.display().to_string();
        Model::new(display.clone())
            .ok_or(SttError::ModelLoadFailed {
                language,
                path: display,
            })
            .map(Some)
    }

    fn complete_result_to_transcript(
        result: CompleteResult<'_>,
        language: Language,
    ) -> Result<Transcript, SttError> {
        match result {
            CompleteResult::Single(single) => {
                let confidence = if single.result.is_empty() {
                    0.0
                } else {
                    single.result.iter().map(|word| word.conf).sum::<f32>()
                        / single.result.len() as f32
                };
                Ok(Transcript {
                    text: single.text.to_string(),
                    language,
                    confidence,
                })
            }
            CompleteResult::Multiple(multiple) => {
                let Some(best) = multiple.alternatives.first() else {
                    return Ok(Transcript {
                        text: String::new(),
                        language,
                        confidence: 0.0,
                    });
                };
                Ok(Transcript {
                    text: best.text.to_string(),
                    language,
                    confidence: best.confidence,
                })
            }
        }
    }

    fn complete_result_text(result: CompleteResult<'_>) -> String {
        match result {
            CompleteResult::Single(single) => single.text.to_string(),
            CompleteResult::Multiple(multiple) => multiple
                .alternatives
                .first()
                .map(|alternative| alternative.text.to_string())
                .unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_detector_accepts_configured_grammar() {
        let detector = WakePhraseDetector::new(&ErezConfig::default());
        assert!(detector.accepts_text("комп"));
        assert!(detector.accepts_text("компьютер"));
        assert!(!detector.accepts_text("привет"));
    }
}
