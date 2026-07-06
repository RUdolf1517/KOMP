use crate::{
    audio::{collect_command_audio, AudioError, AudioSource, VoiceActivityConfig},
    config::{ErezConfig, Language},
    stt::{whisper_backend::WhisperCppRecognizer, SpeechRecognizer, SttError, Transcript},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error(transparent)]
    Audio(#[from] AudioError),
    #[error(transparent)]
    Stt(#[from] SttError),
    #[error("no speech was captured before timeout")]
    NoSpeechCaptured,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecognizedCommand {
    pub audio_samples: usize,
    pub transcript: Transcript,
}

pub fn capture_and_transcribe_command(
    source: &mut impl AudioSource,
    recognizer: &mut impl SpeechRecognizer,
    config: &ErezConfig,
) -> Result<RecognizedCommand, PipelineError> {
    let vad = VoiceActivityConfig {
        end_silence_ms: config.audio.end_silence_ms,
        max_duration_ms: config.audio.command_timeout_ms,
        preroll_ms: config.audio.command_preroll_ms,
        ..VoiceActivityConfig::default()
    };
    let audio = collect_command_audio(source, vad, config.audio.sample_rate_hz)?;
    if audio.is_empty() {
        return Err(PipelineError::NoSpeechCaptured);
    }

    let transcript = transcribe_command_preferred(recognizer, &audio, config)?;
    Ok(RecognizedCommand {
        audio_samples: audio.len(),
        transcript,
    })
}

pub fn transcribe_with_fallback(
    recognizer: &mut impl SpeechRecognizer,
    pcm_i16_16khz: &[i16],
    config: &ErezConfig,
) -> Result<Transcript, SttError> {
    let primary = recognizer.transcribe(pcm_i16_16khz, config.primary_language)?;
    if should_accept_primary(&primary)
        || !config.english_fallback
        || config.primary_language == Language::En
    {
        return Ok(primary);
    }

    match recognizer.transcribe(pcm_i16_16khz, Language::En) {
        Ok(english) if english.confidence > primary.confidence => Ok(english),
        Ok(_) => Ok(primary),
        Err(SttError::ModelNotConfigured(Language::En)) => Ok(primary),
        Err(err) => Err(err),
    }
}

pub fn transcribe_command_preferred(
    recognizer: &mut impl SpeechRecognizer,
    pcm_i16_16khz: &[i16],
    config: &ErezConfig,
) -> Result<Transcript, SttError> {
    match WhisperCppRecognizer::from_config(config) {
        Ok(Some(mut whisper)) => match whisper.transcribe(pcm_i16_16khz, config.primary_language) {
            Ok(transcript) if !transcript.text.trim().is_empty() => return Ok(transcript),
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(error = %err, "whisper.cpp command transcription failed; falling back to vosk");
            }
        },
        Ok(None) => {}
        Err(err) => {
            tracing::warn!(error = %err, "whisper.cpp is not ready; falling back to vosk");
        }
    }

    transcribe_with_fallback(recognizer, pcm_i16_16khz, config)
}

fn should_accept_primary(transcript: &Transcript) -> bool {
    !transcript.text.trim().is_empty() && transcript.confidence >= 0.35
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioFrame;

    struct FakeSource {
        frames: Vec<AudioFrame>,
    }

    impl AudioSource for FakeSource {
        fn next_frame(&mut self) -> Result<Option<AudioFrame>, AudioError> {
            Ok(if self.frames.is_empty() {
                None
            } else {
                Some(self.frames.remove(0))
            })
        }
    }

    struct FakeRecognizer;

    impl SpeechRecognizer for FakeRecognizer {
        fn transcribe(
            &mut self,
            _pcm_i16_16khz: &[i16],
            language: Language,
        ) -> Result<Transcript, SttError> {
            Ok(match language {
                Language::Ru => Transcript {
                    text: String::new(),
                    language,
                    confidence: 0.1,
                },
                Language::En => Transcript {
                    text: "open browser".into(),
                    language,
                    confidence: 0.8,
                },
            })
        }
    }

    #[test]
    fn falls_back_to_english_when_primary_is_weak() {
        let config = ErezConfig::default();
        let transcript =
            transcribe_with_fallback(&mut FakeRecognizer, &[1, 2, 3], &config).unwrap();
        assert_eq!(transcript.language, Language::En);
        assert_eq!(transcript.text, "open browser");
    }

    #[test]
    fn captures_speech_until_vad_end_and_transcribes() {
        let speech = AudioFrame {
            sample_rate_hz: 1_000,
            samples_i16: vec![2_000; 200],
        };
        let silence = AudioFrame {
            sample_rate_hz: 1_000,
            samples_i16: vec![0; 800],
        };
        let mut source = FakeSource {
            frames: vec![speech, silence],
        };
        let mut config = ErezConfig::default();
        config.audio.sample_rate_hz = 1_000;
        config.audio.end_silence_ms = 500;
        let command =
            capture_and_transcribe_command(&mut source, &mut FakeRecognizer, &config).unwrap();
        assert_eq!(command.transcript.text, "open browser");
        assert!(command.audio_samples >= 200);
    }
}
