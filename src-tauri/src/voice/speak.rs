//! Spoken replies with Kokoro, synthesised by the separate GPL voice helper (`voice-helper/`) and
//! played here. The helper starts with a conversation, exits with it, and never saves audio.
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use sherpa_onnx::LinearResampler;

use super::VoiceError;
use super::playback::{Output, Source};
use crate::models::speech::{SpeechOutputModel, Voice};

/// A frame count the helper sends when synthesis failed.
const FAILED: u32 = u32::MAX;
/// Normal speech reaches about this RMS; louder audio shows as the full level.
const LOUD: f32 = 0.1;

/// The helper binary next to Luna's own, where Tauri places sidecars.
pub fn bundled_helper() -> std::io::Result<PathBuf> {
    let executable = std::env::current_exe()?;
    Ok(executable.with_file_name(format!("luna-voice{}", std::env::consts::EXE_SUFFIX)))
}

#[derive(Default)]
struct Playing {
    /// The reply being spoken, if any. Audio for any other reply is dropped.
    current: Option<u32>,
    queue: VecDeque<f32>,
    /// The helper has sent all of the current reply.
    generated: bool,
    /// The helper exited, so nothing more will be spoken.
    failed: bool,
}

struct Shared {
    playing: Mutex<Playing>,
    finished: Box<dyn Fn() + Send + Sync>,
    level: AtomicU32,
}

impl Shared {
    fn playing(&self) -> MutexGuard<'_, Playing> {
        self.playing
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub struct Speaker {
    child: Child,
    stdin: Option<ChildStdin>,
    speaker: i32,
    speed: f32,
    next: u32,
    shared: Arc<Shared>,
    output: Option<Output>,
    threads: Vec<JoinHandle<()>>,
}

impl Speaker {
    /// Starts the helper without waiting for its model, which loads while the user is speaking.
    /// `finished` runs when a reply has been played to the end.
    pub fn new(
        helper: &Path,
        dir: &Path,
        model: &SpeechOutputModel,
        voice: &Voice,
        finished: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, VoiceError> {
        let file = |name: &str| dir.join(name);
        let mut command = Command::new(helper);
        command
            .arg(file(&model.model))
            .arg(file(&model.voices_file))
            .arg(file(&model.tokens))
            .arg(file(&voice.lexicon))
            .arg(file(&model.data_dir))
            .arg(&voice.lang)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().map_err(|error| {
            log::error!("could not start the voice helper: {error}");
            VoiceError::Speech
        })?;
        let shared = Arc::new(Shared {
            playing: Mutex::default(),
            finished: Box::new(finished),
            level: AtomicU32::new(0),
        });
        let source: Source = {
            let shared = shared.clone();
            Arc::new(move |buffer: &mut [f32]| play(&shared, buffer))
        };
        let output = Output::start(source)?;
        let stdout = child.stdout.take().ok_or(VoiceError::Speech)?;
        let stderr = child.stderr.take().ok_or(VoiceError::Speech)?;
        let reader = {
            let shared = shared.clone();
            let output_rate = output.sample_rate;
            std::thread::spawn(move || read_replies(stdout, &shared, output_rate))
        };
        let errors = std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                log::warn!("voice helper: {line}");
            }
        });
        Ok(Self {
            stdin: child.stdin.take(),
            child,
            speaker: voice.speaker,
            speed: model.speed,
            next: 0,
            shared,
            output: Some(output),
            threads: vec![reader, errors],
        })
    }

    pub fn speak(&mut self, text: &str) -> Result<(), VoiceError> {
        self.next = self.next.wrapping_add(1) % FAILED;
        {
            let mut playing = self.shared.playing();
            if playing.failed {
                return Err(VoiceError::Speech);
            }
            *playing = Playing {
                current: Some(self.next),
                ..Playing::default()
            };
        }
        let line = format!(
            "{}\t{}\t{}\t{}\n",
            self.next,
            self.speaker,
            self.speed,
            text.replace(['\n', '\r', '\t'], " ")
        );
        let stdin = self.stdin.as_mut().ok_or(VoiceError::Speech)?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|error| {
                log::error!("could not reach the voice helper: {error}");
                VoiceError::Speech
            })
    }

    /// Stops speaking at once; audio still arriving for this reply is dropped.
    pub fn stop(&mut self) {
        let mut playing = self.shared.playing();
        playing.current = None;
        playing.queue.clear();
    }

    /// How loud the reply is right now, from 0 to 1.
    pub fn level(&self) -> f32 {
        f32::from_bits(self.shared.level.load(Ordering::Relaxed))
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        self.output.take();
        // Closing its input makes the helper exit; killing it covers a helper that is mid-reply.
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// The audio callback: plays queued samples and reports when the current reply has ended.
fn play(shared: &Shared, buffer: &mut [f32]) {
    let mut playing = shared.playing();
    for sample in buffer.iter_mut() {
        *sample = playing.queue.pop_front().unwrap_or(0.0);
    }
    let rms = (buffer.iter().map(|sample| sample * sample).sum::<f32>()
        / buffer.len().max(1) as f32)
        .sqrt();
    shared
        .level
        .store((rms / LOUD).min(1.0).to_bits(), Ordering::Relaxed);
    let done = playing.current.is_some() && playing.generated && playing.queue.is_empty();
    if done {
        playing.current = None;
        drop(playing);
        (shared.finished)();
    }
}

/// Reads the helper's sample rate, then its audio frames, until it exits.
fn read_replies(mut stdout: ChildStdout, shared: &Shared, output_rate: u32) {
    let Some(rate) = read_u32(&mut stdout) else {
        log::error!("the voice helper could not load its model");
        fail(shared);
        return;
    };
    let resampler = (rate != output_rate)
        .then(|| LinearResampler::create(rate as i32, output_rate as i32))
        .flatten();
    while let (Some(id), Some(count)) = (read_u32(&mut stdout), read_u32(&mut stdout)) {
        let samples = match count {
            0 | FAILED => Vec::new(),
            count => {
                let mut bytes = vec![0; count as usize * 4];
                if stdout.read_exact(&mut bytes).is_err() {
                    break;
                }
                let samples: Vec<f32> = bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|bytes| f32::from_le_bytes(*bytes))
                    .collect();
                match &resampler {
                    Some(resampler) => resampler.resample(&samples, false),
                    None => samples,
                }
            }
        };
        if count == FAILED {
            log::error!("the voice helper could not speak a reply");
        }
        let mut playing = shared.playing();
        if playing.current == Some(id) {
            playing.queue.extend(samples);
            playing.generated |= count == 0 || count == FAILED;
        }
    }
    log::warn!("the voice helper stopped");
    fail(shared);
}

/// Nothing more will be spoken; a reply in progress ends so the conversation does not wait.
fn fail(shared: &Shared) {
    let mut playing = shared.playing();
    playing.failed = true;
    playing.generated = true;
}

fn read_u32(reader: &mut impl Read) -> Option<u32> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes).ok()?;
    Some(u32::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    fn shared(finished: Arc<AtomicUsize>) -> Shared {
        Shared {
            playing: Mutex::default(),
            finished: Box::new(move || {
                finished.fetch_add(1, Ordering::SeqCst);
            }),
            level: AtomicU32::new(0),
        }
    }

    #[test]
    fn a_reply_finishes_once_all_of_it_has_played() {
        let finished = Arc::new(AtomicUsize::new(0));
        let shared = shared(finished.clone());
        *shared.playing() = Playing {
            current: Some(1),
            queue: VecDeque::from(vec![0.5; 4]),
            ..Playing::default()
        };
        let mut buffer = [0.0; 3];
        play(&shared, &mut buffer);
        assert_eq!(buffer, [0.5; 3]);
        assert_eq!(finished.load(Ordering::SeqCst), 0);
        shared.playing().generated = true;
        play(&shared, &mut buffer);
        assert_eq!(buffer, [0.5, 0.0, 0.0]);
        assert_eq!(finished.load(Ordering::SeqCst), 1);
        play(&shared, &mut buffer);
        assert_eq!(finished.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_failed_helper_ends_the_reply_in_progress() {
        let finished = Arc::new(AtomicUsize::new(0));
        let shared = shared(finished.clone());
        shared.playing().current = Some(7);
        fail(&shared);
        play(&shared, &mut [0.0; 4]);
        assert_eq!(finished.load(Ordering::SeqCst), 1);
        assert!(shared.playing().failed);
    }
}
