//! The voice thread. It runs the wake word detector on every chunk of audio while passive and
//! drives the conversation state machine when Luna is addressed. Everything it hears is kept in
//! memory only, and only for as long as the current state needs it.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use sherpa_onnx::LinearResampler;

use super::address::{self, Addressed};
use super::buffer::RollingBuffer;
use super::conversation::{Conversation, Expired, Heard, State, Timing};
use super::keyword;
use super::speak::Speaker;
use super::transcribe::Transcriber;
use super::wake::{Segment, SpeechDetector, WakeWord};
use super::{Channels, Host, Input, SAMPLE_RATE, SpeechFiles, VoiceError, VoiceState};
use crate::assistant::Relevance;
use crate::models::catalog;

/// Speech separated by a longer pause than this belongs to a different utterance.
const UTTERANCE_GAP: usize = SAMPLE_RATE as usize;
/// How far before the wake word fired the speech containing it may have ended.
const WAKE_SLACK: usize = SAMPLE_RATE as usize;
/// A spoken reply that never reports finishing is abandoned after this long.
const MAX_REPLY: Duration = Duration::from_secs(60);
const NAME_ONLY_REPLY: &str = "Yes?";
const NOT_UNDERSTOOD: &str = "Sorry, I didn't catch that.";

/// Models that only exist while Luna is being spoken to.
struct Active {
    detector: SpeechDetector,
    transcriber: Transcriber,
    segments: Vec<Segment>,
    /// Where the wake word fired, in detector samples. `None` for follow-ups.
    wake_at: Option<usize>,
}

enum Interpretation {
    Request { text: String, ends: bool },
    NameOnly,
    Closing,
    Ignore,
}

pub struct Listener {
    files: SpeechFiles,
    wake_word: String,
    host: Arc<dyn Host>,
    input: Receiver<Input>,
    accepting: Arc<AtomicBool>,
    stop_requested: Arc<AtomicBool>,
    conversation: Conversation,
    wake: WakeWord,
    preroll: RollingBuffer,
    resampler: Option<LinearResampler>,
    speaker: Speaker,
    active: Option<Active>,
    reported: VoiceState,
    stopping: bool,
}

impl Listener {
    pub fn new(
        files: SpeechFiles,
        wake_word: String,
        timing: Timing,
        sample_rate: u32,
        host: Arc<dyn Host>,
        channels: Channels,
    ) -> Result<Self, VoiceError> {
        let Channels {
            input,
            sender,
            accepting,
            stopping: stop_requested,
        } = channels;
        let model = &catalog::speech().wake_word;
        let vocabulary =
            std::fs::read(files.wake_word_dir.join(&model.vocabulary)).map_err(|error| {
                log::error!("could not read the wake word vocabulary: {error}");
                VoiceError::ModelLoad("wake word")
            })?;
        let line = keyword::keyword_line(&vocabulary, &wake_word).ok_or(VoiceError::WakeWord)?;
        let wake = WakeWord::load(&files.wake_word_dir, model, &line)?;
        let resampler = if sample_rate == SAMPLE_RATE {
            None
        } else {
            Some(
                LinearResampler::create(sample_rate as i32, SAMPLE_RATE as i32)
                    .ok_or(VoiceError::Capture(super::CaptureError::Failed))?,
            )
        };
        let speaker = Speaker::new(move || {
            let _ = sender.send(Input::SpeechFinished);
        })?;
        let preroll_samples = (timing.preroll.as_secs_f32() * SAMPLE_RATE as f32) as usize;
        host.state_changed(VoiceState::Listening);
        Ok(Self {
            files,
            wake_word,
            host,
            input,
            accepting,
            stop_requested,
            conversation: Conversation::new(timing),
            wake,
            preroll: RollingBuffer::new(preroll_samples),
            resampler,
            speaker,
            active: None,
            reported: VoiceState::Listening,
            stopping: false,
        })
    }

    pub fn run(mut self) {
        while !self.stopping {
            let received = match self.conversation.deadline() {
                Some(deadline) => self
                    .input
                    .recv_timeout(deadline.saturating_duration_since(Instant::now())),
                None => self
                    .input
                    .recv()
                    .map_err(|_| RecvTimeoutError::Disconnected),
            };
            match received {
                Ok(Input::Audio(samples)) => self.audio(&samples),
                Ok(Input::MicrophoneLost(error)) => {
                    self.host.stopped(error.into());
                    break;
                }
                Ok(Input::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(Input::SpeechFinished) => {}
                Err(RecvTimeoutError::Timeout) => self.expire(),
            }
        }
        self.conversation.end();
        self.release();
        self.report(VoiceState::Off);
    }

    fn audio(&mut self, samples: &[f32]) {
        let audio = match &self.resampler {
            Some(resampler) => resampler.resample(samples, false),
            None => samples.to_vec(),
        };
        if self.conversation.state() == State::Passive {
            self.preroll.push(&audio);
            if self.wake.hears(&audio) && self.conversation.wake(Instant::now()) {
                log::info!("wake word heard");
                self.begin_listening();
            }
            return;
        }
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let segments = active.detector.accept(&audio);
        active.segments.extend(segments);
        self.advance();
    }

    /// Loads speech detection and transcription and replays the audio from before the wake word.
    fn begin_listening(&mut self) {
        let speech = catalog::speech();
        let loaded = SpeechDetector::load(&self.files.speech_detection, &speech.speech_detection)
            .and_then(|detector| {
                let transcriber =
                    Transcriber::load(&self.files.transcription, &speech.transcription)?;
                Ok((detector, transcriber))
            });
        let (mut detector, transcriber) = match loaded {
            Ok(models) => models,
            Err(error) => {
                log::error!("could not start listening for a request: {error}");
                self.conversation.end();
                self.release();
                return;
            }
        };
        let preroll = self.preroll.take();
        let segments = detector.accept(&preroll);
        self.active = Some(Active {
            detector,
            transcriber,
            segments,
            wake_at: Some(preroll.len()),
        });
        self.advance();
    }

    /// Moves the conversation forward based on what the speech detector has found.
    fn advance(&mut self) {
        let Some(active) = self.active.as_ref() else {
            return;
        };
        // After the wake word, only speech around it counts, not earlier speech in the pre-roll.
        let heard_speech = active.detector.speaking() || utterance_done(active);
        let now = Instant::now();
        match self.conversation.state() {
            State::WakeDetected | State::AwaitingFollowUp if heard_speech => {
                self.conversation.speech_started(now);
                self.advance();
            }
            State::CapturingCommand if !active.detector.speaking() && utterance_done(active) => {
                self.conversation.utterance_ended();
                self.process();
            }
            _ => {}
        }
    }

    fn expire(&mut self) {
        match self.conversation.tick(Instant::now()) {
            Some(Expired::Utterance) => {
                if let Some(active) = self.active.as_mut() {
                    let segments = active.detector.finish();
                    active.segments.extend(segments);
                }
                self.process();
            }
            Some(Expired::FalseWake) => {
                log::info!("wake word was not followed by speech");
                self.release();
            }
            Some(Expired::Conversation) => {
                log::info!("conversation ended");
                self.release();
            }
            None => {}
        }
    }

    /// Transcribes the finished utterance and acts on it.
    fn process(&mut self) {
        self.accepting.store(false, Ordering::Relaxed);
        self.report(VoiceState::Processing);
        let follow_up = self.conversation.is_follow_up();
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let audio = utterance(std::mem::take(&mut active.segments));
        let text = if audio.is_empty() {
            Ok(String::new())
        } else {
            active.transcriber.transcribe(&audio)
        };
        drop(audio);
        let interpretation = match text {
            Ok(text) => self.interpret(&text, follow_up),
            Err(_) if follow_up => Interpretation::Ignore,
            Err(_) => Interpretation::Request {
                text: String::new(),
                ends: false,
            },
        };
        let now = Instant::now();
        match interpretation {
            Interpretation::Request { text, ends } => {
                self.conversation.heard(Heard::Request, now);
                let reply = if text.is_empty() {
                    NOT_UNDERSTOOD.to_owned()
                } else {
                    self.host.respond(&text)
                };
                self.reply(&reply, ends);
            }
            Interpretation::NameOnly => {
                self.conversation.heard(Heard::Request, now);
                self.reply(NAME_ONLY_REPLY, false);
            }
            Interpretation::Closing => {
                self.conversation.heard(Heard::Closing, now);
            }
            Interpretation::Ignore => {
                log::info!("ignored speech that was not meant for Luna");
                self.conversation.heard(Heard::Ignored, now);
            }
        }
        self.listen_again();
    }

    /// Never logs the transcript: it may be speech that was not meant for Luna.
    fn interpret(&self, text: &str, follow_up: bool) -> Interpretation {
        let request = |text: &str| match self.host.relevance(text) {
            Relevance::Closing => Interpretation::Closing,
            relevance => Interpretation::Request {
                text: text.to_owned(),
                ends: relevance == Relevance::Thanks,
            },
        };
        match address::extract(text, &self.wake_word) {
            Addressed::Request(candidates) => {
                let likely = candidates
                    .iter()
                    .find(|candidate| self.host.relevance(candidate) != Relevance::Unrelated)
                    .unwrap_or(&candidates[0]);
                request(likely)
            }
            Addressed::NameOnly => Interpretation::NameOnly,
            Addressed::NotForLuna if follow_up => match self.host.relevance(text) {
                Relevance::Unrelated => Interpretation::Ignore,
                _ => request(text),
            },
            Addressed::NotForLuna => Interpretation::Ignore,
        }
    }

    /// Speaks the reply and waits until it finishes, ignoring the microphone meanwhile so Luna
    /// never hears herself.
    fn reply(&mut self, text: &str, ends: bool) {
        self.drain();
        if self.stop_requested.load(Ordering::SeqCst) {
            self.stopping = true;
        }
        if self.stopping || !self.conversation.replying(ends) {
            return;
        }
        self.report(VoiceState::Responding);
        if self.speaker.speak(text).is_ok() {
            let deadline = Instant::now() + MAX_REPLY;
            loop {
                let wait = deadline.saturating_duration_since(Instant::now());
                match self.input.recv_timeout(wait) {
                    Ok(Input::SpeechFinished) => break,
                    Ok(Input::Audio(_)) => {}
                    Ok(Input::MicrophoneLost(error)) => {
                        self.speaker.stop();
                        self.host.stopped(error.into());
                        self.stopping = true;
                        break;
                    }
                    Ok(Input::Stop) | Err(RecvTimeoutError::Disconnected) => {
                        self.speaker.stop();
                        self.stopping = true;
                        break;
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        log::warn!("spoken reply did not finish");
                        self.speaker.stop();
                        break;
                    }
                }
            }
        }
        self.conversation.finished_speaking(Instant::now());
    }

    /// Resumes listening after a reply, or returns to passive listening.
    fn listen_again(&mut self) {
        self.drain();
        self.wake.reset();
        match self.conversation.state() {
            State::AwaitingFollowUp => {
                if let Some(active) = self.active.as_mut() {
                    active.detector.reset();
                    active.segments.clear();
                    active.wake_at = None;
                }
                self.report(VoiceState::Listening);
            }
            _ => self.release(),
        }
        self.accepting.store(true, Ordering::Relaxed);
    }

    /// Drops audio that arrived while busy and notes any request to stop.
    fn drain(&mut self) {
        while let Ok(input) = self.input.try_recv() {
            match input {
                Input::Audio(_) | Input::SpeechFinished => {}
                Input::MicrophoneLost(error) => {
                    self.host.stopped(error.into());
                    self.stopping = true;
                }
                Input::Stop => self.stopping = true,
            }
        }
    }

    /// Back to passive: drops the speech models and every buffered sample.
    fn release(&mut self) {
        self.active = None;
        self.preroll.clear();
        self.wake.reset();
        if !self.stopping {
            self.report(VoiceState::Listening);
        }
    }

    fn report(&mut self, state: VoiceState) {
        if self.reported != state {
            self.reported = state;
            self.host.state_changed(state);
        }
    }
}

/// Whether speech has ended after the wake word, or any speech has ended for a follow-up.
fn utterance_done(active: &Active) -> bool {
    let Some(last) = active.segments.last() else {
        return false;
    };
    match active.wake_at {
        Some(wake_at) => last.end() + WAKE_SLACK >= wake_at,
        None => true,
    }
}

/// The last stretch of speech, together with speech just before it that had no long pause.
fn utterance(segments: Vec<Segment>) -> Vec<f32> {
    let mut start = segments.len();
    while start > 0 {
        let previous = start - 1;
        let joined = start == segments.len()
            || segments[start].start <= segments[previous].end() + UTTERANCE_GAP;
        if !joined {
            break;
        }
        start = previous;
    }
    segments[start..]
        .iter()
        .flat_map(|segment| segment.samples.iter().copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(start_seconds: f32, seconds: f32) -> Segment {
        let rate = SAMPLE_RATE as f32;
        Segment {
            start: (start_seconds * rate) as usize,
            samples: vec![start_seconds; (seconds * rate) as usize],
        }
    }

    #[test]
    fn earlier_speech_after_a_long_pause_is_left_out() {
        let audio = utterance(vec![
            segment(0.0, 1.0),
            segment(3.0, 1.0),
            segment(4.5, 1.0),
        ]);
        assert_eq!(audio.len(), 2 * SAMPLE_RATE as usize);
        assert!(audio.iter().all(|sample| *sample >= 3.0));
        assert!(utterance(Vec::new()).is_empty());
    }
}
