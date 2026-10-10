//! Wake-word spotting and speech detection with sherpa-onnx. Both run on 16 kHz mono audio.
use std::path::Path;

use sherpa_onnx::{
    KeywordSpotter, KeywordSpotterConfig, OnlineModelConfig, OnlineStream,
    OnlineTransducerModelConfig, SileroVadModelConfig, VadModelConfig, VoiceActivityDetector,
};

use super::{SAMPLE_RATE, VoiceError};
use crate::models::speech::{KeywordModel, VadModel};

/// Listens for the wake word in a continuous stream. Only this runs while Luna is passive.
pub struct WakeWord {
    spotter: KeywordSpotter,
    stream: OnlineStream,
}

impl WakeWord {
    /// `keyword` is the wake word spelled in the model's pieces, from `keyword::keyword_line`.
    pub fn load(dir: &Path, model: &KeywordModel, keyword: &str) -> Result<Self, VoiceError> {
        let file = |name: &str| Some(dir.join(name).to_string_lossy().into_owned());
        let config = KeywordSpotterConfig {
            model_config: OnlineModelConfig {
                transducer: OnlineTransducerModelConfig {
                    encoder: file(&model.encoder),
                    decoder: file(&model.decoder),
                    joiner: file(&model.joiner),
                },
                tokens: file(&model.tokens),
                num_threads: 1,
                ..OnlineModelConfig::default()
            },
            keywords_score: model.score,
            keywords_threshold: model.threshold,
            keywords_buf: Some(format!("{keyword}\n")),
            ..KeywordSpotterConfig::default()
        };
        let spotter = KeywordSpotter::create(&config).ok_or(VoiceError::ModelLoad("wake word"))?;
        let stream = spotter.create_stream();
        Ok(Self { spotter, stream })
    }

    /// Feeds audio and returns whether the wake word was heard in it.
    pub fn hears(&mut self, samples: &[f32]) -> bool {
        self.stream.accept_waveform(SAMPLE_RATE as i32, samples);
        let mut heard = false;
        while self.spotter.is_ready(&self.stream) {
            self.spotter.decode(&self.stream);
            if self
                .spotter
                .get_result(&self.stream)
                .is_some_and(|result| !result.keyword.is_empty())
            {
                // Resetting stops one utterance from triggering twice.
                self.spotter.reset(&self.stream);
                heard = true;
            }
        }
        heard
    }

    /// Forgets partially heard audio, for example after Luna has been speaking.
    pub fn reset(&mut self) {
        self.stream = self.spotter.create_stream();
    }
}

/// Finds where speech starts and ends. Created when Luna is addressed and dropped afterwards.
pub struct SpeechDetector {
    vad: VoiceActivityDetector,
    window: usize,
    pending: Vec<f32>,
}

/// A finished stretch of speech, positioned in samples from when the detector was created.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub start: usize,
    pub samples: Vec<f32>,
}

impl Segment {
    pub fn end(&self) -> usize {
        self.start + self.samples.len()
    }
}

impl SpeechDetector {
    pub fn load(path: &Path, model: &VadModel) -> Result<Self, VoiceError> {
        let config = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(path.to_string_lossy().into_owned()),
                threshold: model.threshold,
                min_silence_duration: model.min_silence_seconds,
                min_speech_duration: model.min_speech_seconds,
                window_size: model.window_size,
                max_speech_duration: model.max_speech_seconds,
            },
            sample_rate: SAMPLE_RATE as i32,
            num_threads: 1,
            ..VadModelConfig::default()
        };
        let vad = VoiceActivityDetector::create(&config, model.max_speech_seconds + 5.0)
            .ok_or(VoiceError::ModelLoad("speech detection"))?;
        Ok(Self {
            vad,
            window: model.window_size as usize,
            pending: Vec::new(),
        })
    }

    /// Feeds audio and returns any segments of speech that have ended.
    pub fn accept(&mut self, samples: &[f32]) -> Vec<Segment> {
        self.pending.extend_from_slice(samples);
        let whole = self.pending.len() / self.window * self.window;
        for window in self.pending[..whole].chunks(self.window) {
            self.vad.accept_waveform(window);
        }
        self.pending.drain(..whole);
        self.take_segments()
    }

    /// Whether speech is in progress right now.
    pub fn speaking(&self) -> bool {
        self.vad.detected()
    }

    /// Starts over, as after Luna has spoken.
    pub fn reset(&mut self) {
        self.vad.reset();
        self.pending.clear();
    }

    /// Ends the current segment, if any, as if silence followed.
    pub fn finish(&mut self) -> Vec<Segment> {
        self.vad.flush();
        self.take_segments()
    }

    fn take_segments(&mut self) -> Vec<Segment> {
        let mut segments = Vec::new();
        while let Some(segment) = self.vad.front() {
            segments.push(Segment {
                start: segment.start().max(0) as usize,
                samples: segment.samples().to_vec(),
            });
            self.vad.pop();
        }
        segments
    }
}
