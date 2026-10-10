//! Voice interaction: wake word, speech capture, transcription, and spoken replies.
//! Audio stays in memory on this device and is discarded as soon as it is no longer needed.
//! While passive, only the wake word detector runs. Speech detection and transcription are
//! loaded when Luna is addressed and dropped when the conversation ends.
mod address;
mod buffer;
mod capture;
mod conversation;
mod listener;
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
use listener::Listener;

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
    #[error(transparent)]
    Capture(#[from] CaptureError),
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
    /// Cancels a request in progress, if any.
    fn cancel(&self);
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
                    let listener = Listener::new(files, timing, sample_rate, host, channels);
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
