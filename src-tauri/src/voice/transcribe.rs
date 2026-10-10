//! Local speech-to-text with whisper.cpp. The model is loaded when Luna is addressed and
//! dropped when the conversation ends, so it holds no memory while Luna is passive.
use std::path::Path;

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::VoiceError;
use crate::models::speech::TranscriptionModel;

pub struct Transcriber {
    context: WhisperContext,
    language: String,
    threads: i32,
}

impl Transcriber {
    pub fn load(path: &Path, model: &TranscriptionModel) -> Result<Self, VoiceError> {
        // Keeps whisper.cpp from writing to the console.
        whisper_rs::install_logging_hooks();
        let context = WhisperContext::new_with_params(path, WhisperContextParameters::default())
            .map_err(|error| {
                log::error!("failed to load the transcription model: {error}");
                VoiceError::ModelLoad("transcription")
            })?;
        let threads =
            std::thread::available_parallelism().map_or(4, |count| count.get().min(8)) as i32;
        Ok(Self {
            context,
            language: model.language.clone(),
            threads,
        })
    }

    /// Transcribes 16 kHz mono audio. The text is never logged or stored here.
    pub fn transcribe(&self, samples: &[f32]) -> Result<String, VoiceError> {
        let failed = |error: whisper_rs::WhisperError| {
            log::error!("transcription failed: {error}");
            VoiceError::Transcription
        };
        let mut state = self.context.create_state().map_err(failed)?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(&self.language));
        params.set_n_threads(self.threads);
        params.set_no_context(true);
        params.set_single_segment(true);
        params.set_no_timestamps(true);
        params.set_suppress_nst(true);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_special(false);
        params.set_print_timestamps(false);
        state.full(params, samples).map_err(failed)?;
        let mut text = String::new();
        for segment in state.as_iter() {
            text.push_str(&segment.to_str_lossy().map_err(failed)?);
        }
        Ok(text.trim().to_owned())
    }
}
