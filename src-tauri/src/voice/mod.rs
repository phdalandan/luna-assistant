//! Voice: wake word, capture, transcription, and spoken replies. Audio stays in memory and only
//! the wake word detector runs while passive; the other models load only during a conversation.
mod address;
mod buffer;
mod capture;
mod conversation;
mod keyword;
mod listener;
mod playback;
mod speak;
mod transcribe;
mod wake;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde::Serialize;

use crate::assistant::Relevance;
pub use capture::CaptureError;
use capture::{AudioSink, Microphone};
pub use conversation::Timing;
pub use keyword::is_valid_name as is_valid_wake_word;
use listener::Listener;
pub use speak::bundled_helper;
pub use transcribe::vocabulary_prompt;

/// All voice processing runs on 16 kHz mono audio.
pub const SAMPLE_RATE: u32 = 16_000;
/// Audio waiting for the listener, about two seconds. Older audio is dropped, never queued.
const AUDIO_QUEUE: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VoiceError {
    #[error("could not load the {0} model")]
    ModelLoad(&'static str),
    #[error("transcription failed")]
    Transcription,
    #[error("text to speech is unavailable")]
    Speech,
    #[error("the wake word cannot be spelled with the keyword model")]
    WakeWord,
    #[error(transparent)]
    Capture(#[from] CaptureError),
}

/// A cloud transcription that failed, with the reply that tells the user what to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptionFailed {
    pub reply: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "camelCase")]
pub enum VoiceState {
    Off,
    Listening,
    Processing,
    Responding,
}

/// What Luna's application needs to provide for voice requests.
pub trait Host: Send + Sync + 'static {
    /// Handles a request spoken to Luna and returns the reply to say.
    fn respond(&self, request: &str) -> String;
    /// Whether something said without Luna's name is meant for her.
    fn relevance(&self, text: &str) -> Relevance;
    /// Names of rooms and devices in the home, so transcription spells them correctly.
    fn vocabulary(&self) -> String;
    /// Transcribes 16 kHz audio with the cloud provider, when the user chose cloud recognition.
    fn transcribe(&self, samples: &[f32]) -> Result<String, TranscriptionFailed>;
    /// Cancels a request in progress, if any.
    fn cancel(&self);
    /// How loud the microphone is, from 0 to 1, a few times a second while listening.
    fn audio_level(&self, level: f32);
    fn state_changed(&self, state: VoiceState);
    /// Listening stopped by itself, for example because the microphone was disconnected.
    fn stopped(&self, error: VoiceError);
}

/// Locations of the installed speech models.
#[derive(Debug, Clone)]
pub struct SpeechFiles {
    pub wake_word_dir: PathBuf,
    pub speech_detection: PathBuf,
    pub transcription: PathBuf,
    pub speech_output_dir: PathBuf,
}

/// The user's voice choices and where the voice helper is.
#[derive(Debug, Clone)]
pub struct VoiceSettings {
    pub wake_word: String,
    /// A voice id from the catalogue, such as "af_heart".
    pub voice: String,
    pub helper: PathBuf,
    /// Requests are transcribed by the host's cloud provider instead of whisper.cpp.
    pub cloud_transcription: bool,
}

enum Input {
    Audio(Vec<f32>),
    MicrophoneLost(CaptureError),
    SpeechFinished,
    Stop,
}

/// How the listener hears from the microphone, the speaker, and `Voice`.
struct Channels {
    input: Receiver<Input>,
    sender: SyncSender<Input>,
    /// Cleared while the listener is busy, so the microphone's audio is dropped at the source.
    accepting: Arc<AtomicBool>,
    /// Set before a request is cancelled, so its reply is never spoken.
    stopping: Arc<AtomicBool>,
}

/// Sends microphone audio to the listener, dropping it while the listener is busy.
struct Sink {
    input: SyncSender<Input>,
    accepting: Arc<AtomicBool>,
}

impl AudioSink for Sink {
    fn audio(&self, samples: Vec<f32>) {
        if !self.accepting.load(Ordering::Relaxed) {
            return;
        }
        if let Err(TrySendError::Full(_)) = self.input.try_send(Input::Audio(samples)) {
            log::debug!("voice audio dropped while the listener was busy");
        }
    }

    fn lost(&self, error: CaptureError) {
        let _ = self.input.try_send(Input::MicrophoneLost(error));
    }
}

struct Running {
    microphone: Arc<Mutex<Option<Microphone>>>,
    input: SyncSender<Input>,
    stopping: Arc<AtomicBool>,
    host: Arc<dyn Host>,
    thread: JoinHandle<()>,
}

/// Starts and stops listening. Stopping ends capture immediately and waits for the listener.
#[derive(Default)]
pub struct Voice {
    running: Mutex<Option<Running>>,
}

impl Voice {
    pub fn start(
        &self,
        files: SpeechFiles,
        settings: VoiceSettings,
        timing: Timing,
        host: Arc<dyn Host>,
    ) -> Result<(), VoiceError> {
        self.stop();
        let (input, receiver) = mpsc::sync_channel(AUDIO_QUEUE);
        let accepting = Arc::new(AtomicBool::new(true));
        let mic = Microphone::start(Sink {
            input: input.clone(),
            accepting: accepting.clone(),
        })?;
        let sample_rate = mic.sample_rate;
        let microphone = Arc::new(Mutex::new(Some(mic)));
        let stopping = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();
        let spawned = {
            let channels = Channels {
                input: receiver,
                sender: input.clone(),
                accepting,
                stopping: stopping.clone(),
            };
            let host = host.clone();
            let microphone = microphone.clone();
            std::thread::Builder::new()
                .name("luna-voice".into())
                .spawn(move || {
                    let listener =
                        Listener::new(files, settings, timing, sample_rate, host, channels);
                    match listener {
                        Ok(listener) => {
                            let _ = ready_tx.send(Ok(()));
                            listener.run();
                        }
                        Err(error) => {
                            let _ = ready_tx.send(Err(error));
                        }
                    }
                    // Whether stopped or failed, capture ends with the listener.
                    lock(&microphone).take();
                })
        };
        let thread = spawned.map_err(|error| {
            log::error!("could not start the voice thread: {error}");
            VoiceError::Capture(CaptureError::Failed)
        })?;
        let ready = ready_rx
            .recv()
            .unwrap_or(Err(VoiceError::ModelLoad("wake word")));
        if let Err(error) = ready {
            let _ = thread.join();
            return Err(error);
        }
        *lock(&self.running) = Some(Running {
            microphone,
            input,
            stopping,
            host,
            thread,
        });
        Ok(())
    }

    /// Stops capture at once, cancels any request, and waits for the listener to finish.
    pub fn stop(&self) {
        let Some(running) = lock(&self.running).take() else {
            return;
        };
        running.stopping.store(true, Ordering::SeqCst);
        lock(&running.microphone).take();
        running.host.cancel();
        let _ = running.input.send(Input::Stop);
        if running.thread.join().is_err() {
            log::error!("the voice thread panicked");
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
