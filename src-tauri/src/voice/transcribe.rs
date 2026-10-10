//! Local speech-to-text with whisper.cpp. The model is loaded when Luna is addressed and
//! dropped when the conversation ends, so it holds no memory while Luna is passive.
use std::path::Path;

use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

use super::{SAMPLE_RATE, VoiceError};
use crate::models::speech::TranscriptionModel;

/// Whisper's encoder covers 30 s in 1500 frames.
const FRAMES_PER_SECOND: f32 = 50.0;
const FULL_WINDOW: i32 = 1500;
/// Extra frames beyond the audio, so speech at the very end is not cut off.
const WINDOW_MARGIN: i32 = 64;
/// Very short windows make whisper repeat itself, so a request never gets less than this.
const MIN_WINDOW: i32 = 256;
/// The prompt stays well under whisper's limit of half its 448-token text context.
const MAX_PROMPT_TOKENS: usize = 120;
const MAX_PROMPT_CHARS: usize = 400;

pub struct Transcriber {
    context: WhisperContext,
    state: WhisperState,
    language: String,
    threads: i32,
    prompt: Vec<i32>,
}

impl Transcriber {
    pub fn load(path: &Path, model: &TranscriptionModel) -> Result<Self, VoiceError> {
        // Keeps whisper.cpp from writing to the console.
        whisper_rs::install_logging_hooks();
        let load_failed = |error: whisper_rs::WhisperError| {
            log::error!("failed to load the transcription model: {error}");
            VoiceError::ModelLoad("transcription")
        };
        let context = WhisperContext::new_with_params(path, WhisperContextParameters::default())
            .map_err(load_failed)?;
        let state = context.create_state().map_err(load_failed)?;
        let threads =
            std::thread::available_parallelism().map_or(4, |count| count.get().min(8)) as i32;
        Ok(Self {
            context,
            state,
            language: model.language.clone(),
            threads,
            prompt: Vec::new(),
        })
    }

    /// Primes recognition with names from the home, such as "Front Porch, Bedroom AC", so they
    /// are spelled as Home Assistant has them rather than as similar-sounding words.
    pub fn expect_words(&mut self, words: &str) -> Result<(), VoiceError> {
        let text = vocabulary_prompt(words);
        let mut tokens = self
            .context
            .tokenize(text, text.len().max(1))
            .map_err(|error| {
                log::error!("could not prepare the transcription vocabulary: {error}");
                VoiceError::ModelLoad("transcription")
            })?;
        tokens.truncate(MAX_PROMPT_TOKENS);
        self.prompt = tokens;
        Ok(())
    }

    /// Transcribes 16 kHz mono audio. The text is never logged or stored here.
    pub fn transcribe(&mut self, samples: &[f32]) -> Result<String, VoiceError> {
        let failed = |error: whisper_rs::WhisperError| {
            log::error!("transcription failed: {error}");
            VoiceError::Transcription
        };
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
        params.set_audio_ctx(window(samples.len()));
        if !self.prompt.is_empty() {
            params.set_tokens(&self.prompt);
        }
        self.state.full(params, samples).map_err(failed)?;
        let mut text = String::new();
        for segment in self.state.as_iter() {
            text.push_str(&segment.to_str_lossy().map_err(failed)?);
        }
        Ok(text.trim().to_owned())
    }
}

/// The names used to prime transcription, locally or in the cloud. whisper-rs misreads text over
/// its token limit, so the limit is in bytes: a whisper token is never shorter than one byte.
pub fn vocabulary_prompt(words: &str) -> &str {
    leading_names(words, MAX_PROMPT_CHARS)
}

/// Whole names from the start of a comma-separated list, within `limit` bytes. A token never
/// spans fewer than one byte, so the result tokenizes within `limit` tokens.
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

/// Encoder frames for the audio instead of a full 30 s window, which is most of the work for a
/// short request.
fn window(samples: usize) -> i32 {
    let seconds = samples as f32 / SAMPLE_RATE as f32;
    ((seconds * FRAMES_PER_SECOND).ceil() as i32 + WINDOW_MARGIN).clamp(MIN_WINDOW, FULL_WINDOW)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_covers_the_audio_and_never_exceeds_whisper_s() {
        assert_eq!(window(SAMPLE_RATE as usize * 3), MIN_WINDOW);
        assert_eq!(window(SAMPLE_RATE as usize * 10), 564);
        assert_eq!(window(SAMPLE_RATE as usize * 60), FULL_WINDOW);
    }

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
