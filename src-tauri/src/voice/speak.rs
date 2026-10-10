//! Spoken replies through the operating system's own voice (WinRT on Windows, AVFoundation on
//! macOS). Speech is synthesised locally and never saved.
use tts::{Features, Tts};

use super::VoiceError;

pub struct Speaker {
    tts: Tts,
}

impl Speaker {
    /// `finished` runs when an utterance ends or is stopped.
    pub fn new(finished: impl Fn() + Clone + 'static) -> Result<Self, VoiceError> {
        let failed = |error: tts::Error| {
            log::error!("text to speech unavailable: {error}");
            VoiceError::Speech
        };
        let tts = Tts::default().map_err(failed)?;
        let Features {
            utterance_callbacks,
            stop,
            ..
        } = tts.supported_features();
        if !utterance_callbacks || !stop {
            log::error!("text to speech cannot report when it finishes speaking");
            return Err(VoiceError::Speech);
        }
        let ended = finished.clone();
        tts.on_utterance_end(Some(Box::new(move |_| ended())))
            .map_err(failed)?;
        tts.on_utterance_stop(Some(Box::new(move |_| finished())))
            .map_err(failed)?;
        Ok(Self { tts })
    }

    pub fn speak(&mut self, text: &str) -> Result<(), VoiceError> {
        self.tts.speak(text, true).map(|_| ()).map_err(|error| {
            log::error!("could not speak the reply: {error}");
            VoiceError::Speech
        })
    }

    pub fn stop(&mut self) {
        if let Err(error) = self.tts.stop() {
            log::warn!("could not stop speaking: {error}");
        }
    }
}
