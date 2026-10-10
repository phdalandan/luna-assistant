//! Local speech-to-text with NVIDIA Parakeet through sherpa-onnx. The model is loaded when Luna is
//! addressed and dropped when the conversation ends, so it holds no memory while Luna is passive.
use std::path::Path;

use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig};

use super::{SAMPLE_RATE, VoiceError};
use crate::models::speech::TranscriptionModel;

/// The model aborts the whole process on audio too short to make features, so shorter requests
/// are padded with silence up to this length.
const MIN_SAMPLES: usize = SAMPLE_RATE as usize;
/// Names primed into cloud transcription stay short, as the earliest names matter most.
const MAX_PROMPT_CHARS: usize = 400;

pub struct Transcriber {
    recognizer: OfflineRecognizer,
}

impl Transcriber {
    pub fn load(dir: &Path, model: &TranscriptionModel) -> Result<Self, VoiceError> {
        let file = |name: &str| Some(dir.join(name).to_string_lossy().into_owned());
        let mut config = OfflineRecognizerConfig::default();
        config.model_config.transducer.encoder = file(&model.encoder);
        config.model_config.transducer.decoder = file(&model.decoder);
        config.model_config.transducer.joiner = file(&model.joiner);
        config.model_config.tokens = file(&model.tokens);
        config.model_config.model_type = Some(model.model_type.clone());
        config.model_config.num_threads =
            std::thread::available_parallelism().map_or(2, |count| count.get().min(4)) as i32;
        let recognizer = OfflineRecognizer::create(&config).ok_or_else(|| {
            log::error!("failed to load the transcription model");
            VoiceError::ModelLoad("transcription")
        })?;
        Ok(Self { recognizer })
    }

    /// Transcribes 16 kHz mono audio. The text is never logged or stored here.
    pub fn transcribe(&self, samples: &[f32]) -> Result<String, VoiceError> {
        let padded;
        let samples = if samples.len() < MIN_SAMPLES {
            padded = [samples, &vec![0.0; MIN_SAMPLES - samples.len()]].concat();
            &padded
        } else {
            samples
        };
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(SAMPLE_RATE as i32, samples);
        self.recognizer.decode(&stream);
        let result = stream.get_result().ok_or_else(|| {
            log::error!("transcription returned no result");
            VoiceError::Transcription
        })?;
        Ok(result.text.trim().to_owned())
    }
}

/// The home's names as a hint for cloud transcription, cut between names to stay short.
pub fn vocabulary_prompt(words: &str) -> &str {
    leading_names(words, MAX_PROMPT_CHARS)
}

/// Whole names from the start of a comma-separated list, within `limit` bytes.
fn leading_names(names: &str, limit: usize) -> &str {
    if names.len() <= limit {
        return names;
    }
    let end = (0..=limit)
        .rev()
        .find(|&index| names.is_char_boundary(index))
        .unwrap_or(0);
    let cut = names[..end].rfind(", ").unwrap_or(0);
    &names[..cut]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_name_lists_are_cut_between_names() {
        assert_eq!(leading_names("Kitchen, Hallway", 100), "Kitchen, Hallway");
        assert_eq!(
            leading_names("Kitchen, Hallway, Garage", 20),
            "Kitchen, Hallway"
        );
        assert_eq!(leading_names("Kitchen", 3), "");
        assert_eq!(leading_names("Café, Salón", 9), "Café");
    }
}
