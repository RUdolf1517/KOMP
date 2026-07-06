use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("live microphone capture is not enabled in this build")]
    BackendUnavailable,
    #[error("audio device error: {0}")]
    Device(String),
    #[error("audio stream ended")]
    StreamEnded,
    #[error("failed to read wav file: {0}")]
    Wav(#[from] hound::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioFrame {
    pub sample_rate_hz: u32,
    pub samples_i16: Vec<i16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListenResult {
    pub woke: bool,
    pub transcript: Option<String>,
}

pub trait AudioSource {
    fn next_frame(&mut self) -> Result<Option<AudioFrame>, AudioError>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceActivityConfig {
    pub speech_rms_threshold: f32,
    pub min_speech_ms: u64,
    pub preroll_ms: u64,
    pub end_silence_ms: u64,
    pub max_duration_ms: u64,
}

impl Default for VoiceActivityConfig {
    fn default() -> Self {
        Self {
            speech_rms_threshold: 0.012,
            min_speech_ms: 120,
            preroll_ms: 300,
            end_silence_ms: 700,
            max_duration_ms: 5_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VoiceActivityDetector {
    config: VoiceActivityConfig,
    speech_ms: u64,
    trailing_silence_ms: u64,
    in_speech: bool,
}

impl VoiceActivityDetector {
    pub fn new(config: VoiceActivityConfig) -> Self {
        Self {
            config,
            speech_ms: 0,
            trailing_silence_ms: 0,
            in_speech: false,
        }
    }

    pub fn observe(&mut self, frame: &AudioFrame) -> VoiceActivityState {
        let duration_ms = frame_duration_ms(frame);
        let rms = rms_i16(&frame.samples_i16);
        let speech_like = rms >= self.config.speech_rms_threshold;

        if speech_like {
            self.speech_ms += duration_ms;
            self.trailing_silence_ms = 0;
            if self.speech_ms >= self.config.min_speech_ms {
                self.in_speech = true;
            }
        } else if self.in_speech {
            self.trailing_silence_ms += duration_ms;
        }

        if self.in_speech && self.trailing_silence_ms >= self.config.end_silence_ms {
            VoiceActivityState::Ended
        } else if self.in_speech {
            VoiceActivityState::Speech
        } else {
            VoiceActivityState::Silence
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceActivityState {
    Silence,
    Speech,
    Ended,
}

pub fn collect_command_audio(
    source: &mut impl AudioSource,
    vad_config: VoiceActivityConfig,
    output_rate_hz: u32,
) -> Result<Vec<i16>, AudioError> {
    let mut vad = VoiceActivityDetector::new(vad_config.clone());
    let started_at = Instant::now();
    let mut collected = Vec::new();
    let mut preroll = VecDeque::new();
    let mut preroll_duration_ms = 0;
    let mut flushed_preroll = false;

    while started_at.elapsed() < Duration::from_millis(vad_config.max_duration_ms) {
        let Some(frame) = source.next_frame()? else {
            break;
        };
        let state = vad.observe(&frame);
        if !flushed_preroll {
            preroll_duration_ms += frame_duration_ms(&frame);
            preroll.push_back(frame.clone());
            let preroll_limit_ms = vad_config.preroll_ms + frame_duration_ms(&frame);
            while preroll_duration_ms > preroll_limit_ms {
                if let Some(old) = preroll.pop_front() {
                    preroll_duration_ms =
                        preroll_duration_ms.saturating_sub(frame_duration_ms(&old));
                } else {
                    break;
                }
            }
        }
        if matches!(
            state,
            VoiceActivityState::Speech | VoiceActivityState::Ended
        ) {
            if !flushed_preroll {
                for buffered in preroll.drain(..) {
                    collected.extend(resample_nearest_mono_i16(
                        &buffered.samples_i16,
                        buffered.sample_rate_hz,
                        output_rate_hz,
                    ));
                }
                flushed_preroll = true;
            } else {
                collected.extend(resample_nearest_mono_i16(
                    &frame.samples_i16,
                    frame.sample_rate_hz,
                    output_rate_hz,
                ));
            }
        }
        if state == VoiceActivityState::Ended {
            break;
        }
        if flushed_preroll && state == VoiceActivityState::Silence {
            collected.extend(resample_nearest_mono_i16(
                &frame.samples_i16,
                frame.sample_rate_hz,
                output_rate_hz,
            ));
        }
    }

    Ok(collected)
}

pub fn resample_nearest_mono_i16(input: &[i16], input_rate: u32, output_rate: u32) -> Vec<i16> {
    if input_rate == output_rate || input.is_empty() {
        return input.to_vec();
    }

    let output_len = (input.len() as u64 * output_rate as u64 / input_rate as u64).max(1) as usize;
    (0..output_len)
        .map(|idx| {
            let source_idx = idx as u64 * input_rate as u64 / output_rate as u64;
            input[source_idx.min(input.len() as u64 - 1) as usize]
        })
        .collect()
}

pub fn rms_i16(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_squares = samples
        .iter()
        .map(|sample| {
            let normalized = *sample as f32 / i16::MAX as f32;
            normalized * normalized
        })
        .sum::<f32>();
    (sum_squares / samples.len() as f32).sqrt()
}

pub fn frame_duration_ms(frame: &AudioFrame) -> u64 {
    if frame.sample_rate_hz == 0 {
        return 0;
    }
    (frame.samples_i16.len() as u64 * 1_000 / frame.sample_rate_hz as u64).max(1)
}

pub fn read_wav_mono_i16(
    path: impl AsRef<Path>,
    output_rate_hz: u32,
) -> Result<AudioFrame, AudioError> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let raw = match spec.sample_format {
        hound::SampleFormat::Int => {
            if spec.bits_per_sample <= 16 {
                reader.samples::<i16>().collect::<Result<Vec<_>, _>>()?
            } else {
                reader
                    .samples::<i32>()
                    .map(|sample| sample.map(|value| (value >> (spec.bits_per_sample - 16)) as i16))
                    .collect::<Result<Vec<_>, _>>()?
            }
        }
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|sample| sample.map(|value| (value.clamp(-1.0, 1.0) * i16::MAX as f32) as i16))
            .collect::<Result<Vec<_>, _>>()?,
    };
    let mono = raw
        .chunks(channels)
        .map(|frame| {
            let sum = frame.iter().map(|sample| *sample as i32).sum::<i32>();
            (sum / frame.len() as i32) as i16
        })
        .collect::<Vec<_>>();
    Ok(AudioFrame {
        sample_rate_hz: output_rate_hz,
        samples_i16: resample_nearest_mono_i16(&mono, spec.sample_rate, output_rate_hz),
    })
}

pub fn write_wav_mono_i16(
    path: impl AsRef<Path>,
    samples: &[i16],
    sample_rate_hz: u32,
) -> Result<(), AudioError> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: sample_rate_hz,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for sample in samples {
        writer.write_sample(*sample)?;
    }
    writer.finalize()?;
    Ok(())
}

#[cfg(feature = "audio-cpal")]
pub mod cpal_capture {
    use super::{resample_nearest_mono_i16, AudioError, AudioFrame, AudioSource};
    use cpal::{
        traits::{DeviceTrait, HostTrait, StreamTrait},
        SampleFormat, Stream,
    };
    use std::{
        sync::mpsc::{self, Receiver},
        time::Duration,
    };

    pub struct CpalAudioSource {
        _stream: Stream,
        receiver: Receiver<AudioFrame>,
    }

    impl CpalAudioSource {
        pub fn default_input(output_rate_hz: u32) -> Result<Self, AudioError> {
            let host = cpal::default_host();
            let device = host
                .default_input_device()
                .ok_or_else(|| AudioError::Device("no default input device".into()))?;
            let config = device
                .default_input_config()
                .map_err(|err| AudioError::Device(err.to_string()))?;
            let sample_rate_hz = config.sample_rate().0;
            let channels = config.channels() as usize;
            let stream_config = config.clone().into();
            let (sender, receiver) = mpsc::sync_channel(8);
            let err_fn = |err| tracing::error!("input audio stream error: {err}");

            let stream = match config.sample_format() {
                SampleFormat::I16 => device.build_input_stream(
                    &stream_config,
                    move |data: &[i16], _| {
                        send_frame(&sender, data, channels, sample_rate_hz, output_rate_hz);
                    },
                    err_fn,
                    None,
                ),
                SampleFormat::U16 => device.build_input_stream(
                    &stream_config,
                    move |data: &[u16], _| {
                        let converted = data
                            .iter()
                            .map(|sample| (*sample as i32 - 32768) as i16)
                            .collect::<Vec<_>>();
                        send_frame(
                            &sender,
                            &converted,
                            channels,
                            sample_rate_hz,
                            output_rate_hz,
                        );
                    },
                    err_fn,
                    None,
                ),
                SampleFormat::F32 => device.build_input_stream(
                    &stream_config,
                    move |data: &[f32], _| {
                        let converted = data
                            .iter()
                            .map(|sample| {
                                (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
                            })
                            .collect::<Vec<_>>();
                        send_frame(
                            &sender,
                            &converted,
                            channels,
                            sample_rate_hz,
                            output_rate_hz,
                        );
                    },
                    err_fn,
                    None,
                ),
                other => {
                    return Err(AudioError::Device(format!(
                        "unsupported sample format: {other:?}"
                    )))
                }
            }
            .map_err(|err| AudioError::Device(err.to_string()))?;

            stream
                .play()
                .map_err(|err| AudioError::Device(err.to_string()))?;
            Ok(Self {
                _stream: stream,
                receiver,
            })
        }
    }

    impl AudioSource for CpalAudioSource {
        fn next_frame(&mut self) -> Result<Option<AudioFrame>, AudioError> {
            match self.receiver.recv_timeout(Duration::from_millis(250)) {
                Ok(frame) => Ok(Some(frame)),
                Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
                Err(mpsc::RecvTimeoutError::Disconnected) => Err(AudioError::StreamEnded),
            }
        }
    }

    fn send_frame(
        sender: &mpsc::SyncSender<AudioFrame>,
        interleaved: &[i16],
        channels: usize,
        input_rate_hz: u32,
        output_rate_hz: u32,
    ) {
        if interleaved.is_empty() || channels == 0 {
            return;
        }
        let mono = interleaved
            .chunks(channels)
            .map(|frame| {
                let sum = frame.iter().map(|sample| *sample as i32).sum::<i32>();
                (sum / frame.len() as i32) as i16
            })
            .collect::<Vec<_>>();
        let samples_i16 = resample_nearest_mono_i16(&mono, input_rate_hz, output_rate_hz);
        let _ = sender.try_send(AudioFrame {
            sample_rate_hz: output_rate_hz,
            samples_i16,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resamples_down_without_empty_output() {
        let out = resample_nearest_mono_i16(&[1, 2, 3, 4], 48_000, 16_000);
        assert!(!out.is_empty());
    }

    #[test]
    fn rms_detects_non_silent_audio() {
        assert_eq!(rms_i16(&[]), 0.0);
        assert!(rms_i16(&[0, 8192, -8192]) > 0.1);
    }

    #[test]
    fn vad_ends_after_trailing_silence() {
        let mut vad = VoiceActivityDetector::new(VoiceActivityConfig {
            speech_rms_threshold: 0.01,
            min_speech_ms: 1,
            preroll_ms: 0,
            end_silence_ms: 20,
            max_duration_ms: 1_000,
        });
        let speech = AudioFrame {
            sample_rate_hz: 1_000,
            samples_i16: vec![2_000; 10],
        };
        let silence = AudioFrame {
            sample_rate_hz: 1_000,
            samples_i16: vec![0; 10],
        };
        assert_eq!(vad.observe(&speech), VoiceActivityState::Speech);
        assert_eq!(vad.observe(&silence), VoiceActivityState::Speech);
        assert_eq!(vad.observe(&silence), VoiceActivityState::Ended);
    }

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

    #[test]
    fn collect_command_audio_keeps_preroll_before_speech_confirmation() {
        let quiet = AudioFrame {
            sample_rate_hz: 1_000,
            samples_i16: vec![0; 50],
        };
        let speech_start = AudioFrame {
            sample_rate_hz: 1_000,
            samples_i16: vec![2_000; 50],
        };
        let speech_confirm = AudioFrame {
            sample_rate_hz: 1_000,
            samples_i16: vec![3_000; 80],
        };
        let silence = AudioFrame {
            sample_rate_hz: 1_000,
            samples_i16: vec![0; 200],
        };
        let mut source = FakeSource {
            frames: vec![quiet, speech_start, speech_confirm, silence],
        };
        let audio = collect_command_audio(
            &mut source,
            VoiceActivityConfig {
                speech_rms_threshold: 0.01,
                min_speech_ms: 100,
                preroll_ms: 120,
                end_silence_ms: 100,
                max_duration_ms: 1_000,
            },
            1_000,
        )
        .unwrap();

        assert!(audio.starts_with(&vec![0; 50]));
        assert!(audio[50..100].iter().all(|sample| *sample == 2_000));
    }

    #[test]
    fn reads_wav_fixture_as_mono_i16() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 1_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        writer.write_sample::<i16>(1_000).unwrap();
        writer.write_sample::<i16>(3_000).unwrap();
        writer.finalize().unwrap();

        let frame = read_wav_mono_i16(&path, 1_000).unwrap();
        assert_eq!(frame.sample_rate_hz, 1_000);
        assert_eq!(frame.samples_i16, vec![2_000]);
    }
}
