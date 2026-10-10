//! Real-model voice tests (they speak aloud): `LUNA_SPEECH_DIR=<models> LUNA_SPEECH_WAVS=<16 kHz
//! WAVs> cargo test --release voice -- --ignored --nocapture --test-threads 1`
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::keyword::keyword_line;
use super::listener::Listener;
use super::transcribe::Transcriber;
use super::wake::WakeWord;
use super::*;
use crate::assistant::{self, Relevance};
use crate::home_assistant::model::Home;
use crate::home_assistant::model::fixtures::home;
use crate::models::catalog;

fn env_dir(name: &str) -> PathBuf {
    PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set")))
}

fn models_dir() -> PathBuf {
    env_dir("LUNA_SPEECH_DIR")
}

fn whisper_file() -> String {
    std::env::var("LUNA_WHISPER_FILE")
        .unwrap_or_else(|_| catalog::speech().transcription.file.file_name.clone())
}

fn read_wav(name: &str) -> Vec<f32> {
    let path = env_dir("LUNA_SPEECH_WAVS").join(format!("{name}.wav"));
    let wave = sherpa_onnx::Wave::read(&path.to_string_lossy())
        .unwrap_or_else(|| panic!("missing {}", path.display()));
    assert_eq!(wave.sample_rate(), SAMPLE_RATE as i32);
    wave.samples().to_vec()
}

fn wavs() -> Vec<(String, Vec<f32>)> {
    let mut names: Vec<String> = std::fs::read_dir(env_dir("LUNA_SPEECH_WAVS"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "wav"))
        .map(|path| path.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let samples = read_wav(&name);
            (name, samples)
        })
        .collect()
}

fn keyword_line_for(models: &std::path::Path, name: &str) -> String {
    let vocabulary = models
        .join("kws")
        .join(&catalog::speech().wake_word.vocabulary);
    keyword_line(&std::fs::read(vocabulary).unwrap(), name).unwrap()
}

fn resident_megabytes() -> u64 {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let mut system = System::new();
    let pid = sysinfo::get_current_pid().unwrap();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    system
        .process(pid)
        .map_or(0, |process| process.memory() / 1_000_000)
}

#[test]
#[ignore = "needs speech models and recordings"]
fn measure_wake_word_and_transcription() {
    let speech = catalog::speech();
    let models = models_dir();
    let before = resident_megabytes();
    let mut wake = WakeWord::load(
        &models.join("kws"),
        &speech.wake_word,
        &keyword_line_for(&models, "Luna"),
    )
    .unwrap();
    println!(
        "wake word model: +{} MB resident",
        resident_megabytes() - before
    );
    let before = resident_megabytes();
    let started = Instant::now();
    let mut transcriber =
        Transcriber::load(&models.join(whisper_file()), &speech.transcription).unwrap();
    if let Ok(words) = std::env::var("LUNA_WHISPER_PROMPT") {
        transcriber.expect_words(&words).unwrap();
    }
    println!(
        "loaded {} in {} ms, +{} MB resident",
        whisper_file(),
        started.elapsed().as_millis(),
        resident_megabytes() - before
    );

    let chunk = SAMPLE_RATE as usize / 10;
    let mut spotting_audio = 0.0;
    let mut spotting_time = Duration::ZERO;
    for (name, samples) in wavs() {
        wake.reset();
        let started = Instant::now();
        let mut heard_at = Vec::new();
        // Silence after the phrase lets the spotter finish decoding it.
        let padded: Vec<f32> = samples.iter().copied().chain(vec![0.0; 8_000]).collect();
        for (index, piece) in padded.chunks(chunk).enumerate() {
            if wake.hears(piece) {
                heard_at.push(format!("{:.1}s", (index + 1) as f32 / 10.0));
            }
        }
        spotting_time += started.elapsed();
        spotting_audio += padded.len() as f32 / SAMPLE_RATE as f32;
        let started = Instant::now();
        let text = transcriber.transcribe(&samples).unwrap();
        let audio = samples.len() as f32 / SAMPLE_RATE as f32;
        println!(
            "{name:<22} {audio:>4.1}s | wake {:<12} | whisper {:>5} ms | {text}",
            format!("{heard_at:?}"),
            started.elapsed().as_millis(),
        );
    }
    println!(
        "wake word: {:.1} ms of one core per second of audio",
        spotting_time.as_secs_f32() * 1000.0 / spotting_audio
    );
}

/// Records what the voice thread asks of Luna.
struct FakeHost {
    home: Home,
    requests: Mutex<Vec<String>>,
    states: Mutex<Vec<VoiceState>>,
    stopped: Mutex<Vec<VoiceError>>,
    /// When set, a request waits here until it is cancelled.
    hold: Mutex<Option<mpsc::Receiver<()>>>,
    release: Mutex<Option<mpsc::Sender<()>>>,
    /// When each reply was requested, and when its voice was first heard.
    replies: Mutex<Vec<(Instant, Option<Instant>)>>,
    /// What the cloud provider returns, for cloud recognition.
    cloud_transcript: Mutex<Result<String, TranscriptionFailed>>,
}

impl FakeHost {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            home: home(),
            requests: Mutex::default(),
            states: Mutex::default(),
            stopped: Mutex::default(),
            hold: Mutex::default(),
            release: Mutex::default(),
            replies: Mutex::default(),
            cloud_transcript: Mutex::new(Ok(String::new())),
        })
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn state(&self) -> Option<VoiceState> {
        self.states.lock().unwrap().last().copied()
    }
}

impl Host for FakeHost {
    fn respond(&self, request: &str) -> String {
        self.requests.lock().unwrap().push(request.to_owned());
        self.replies.lock().unwrap().push((Instant::now(), None));
        if let Some(hold) = self.hold.lock().unwrap().take() {
            let _ = hold.recv();
            return "Stopped.".into();
        }
        "Done.".into()
    }

    fn relevance(&self, text: &str) -> Relevance {
        assistant::relevance(&self.home, text)
    }

    fn vocabulary(&self) -> String {
        assistant::vocabulary(&self.home)
    }

    fn transcribe(&self, _: &[f32]) -> Result<String, TranscriptionFailed> {
        self.cloud_transcript.lock().unwrap().clone()
    }

    fn cancel(&self) {
        if let Some(release) = self.release.lock().unwrap().take() {
            let _ = release.send(());
        }
    }

    fn audio_level(&self, level: f32) {
        let responding = self.state() == Some(VoiceState::Responding);
        if let Some((_, heard @ None)) = self.replies.lock().unwrap().last_mut()
            && responding
            && level > 0.0
        {
            *heard = Some(Instant::now());
        }
    }

    fn state_changed(&self, state: VoiceState) {
        self.states.lock().unwrap().push(state);
    }

    fn stopped(&self, error: VoiceError) {
        self.stopped.lock().unwrap().push(error);
    }
}

struct Running {
    input: SyncSender<Input>,
    stopping: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

fn speech_files() -> SpeechFiles {
    let models = models_dir();
    SpeechFiles {
        wake_word_dir: models.join("kws"),
        speech_detection: models.join("silero_vad.onnx"),
        transcription: models.join(whisper_file()),
        speech_output_dir: models.join("kokoro"),
    }
}

/// The voice helper built from `voice-helper/`, or `LUNA_VOICE_HELPER`.
fn voice_settings(wake_word: &str) -> VoiceSettings {
    let helper = std::env::var("LUNA_VOICE_HELPER").map_or_else(
        |_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../voice-helper/target/release")
                .join(format!("luna-voice{}", std::env::consts::EXE_SUFFIX))
        },
        PathBuf::from,
    );
    VoiceSettings {
        wake_word: wake_word.into(),
        voice: "af_heart".into(),
        helper,
        cloud_transcription: false,
    }
}

fn start(host: Arc<FakeHost>) -> Running {
    start_with(host, voice_settings("Luna"))
}

fn start_with(host: Arc<FakeHost>, settings: VoiceSettings) -> Running {
    let files = speech_files();
    let (input, receiver) = mpsc::sync_channel(AUDIO_QUEUE);
    let stopping = Arc::new(AtomicBool::new(false));
    let channels = Channels {
        input: receiver,
        sender: input.clone(),
        accepting: Arc::new(AtomicBool::new(true)),
        stopping: stopping.clone(),
    };
    let thread = std::thread::spawn(move || {
        Listener::new(
            files,
            settings,
            Timing::default(),
            SAMPLE_RATE,
            host,
            channels,
        )
        .unwrap()
        .run();
    });
    Running {
        input,
        stopping,
        thread,
    }
}

/// Plays a recording into the listener as the microphone would, followed by silence.
fn say(running: &Running, name: &str) {
    let mut audio = read_wav(name);
    audio.extend(vec![0.0; SAMPLE_RATE as usize * 3 / 2]);
    for chunk in audio.chunks(SAMPLE_RATE as usize / 50) {
        running.input.send(Input::Audio(chunk.to_vec())).unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Waits until Luna has finished with the last utterance and is listening again.
fn settle(host: &FakeHost) {
    std::thread::sleep(Duration::from_millis(300));
    wait_until("listening", || host.state() == Some(VoiceState::Listening));
}

#[test]
#[ignore = "needs speech models and recordings"]
fn wake_word_anywhere_follow_ups_and_unrelated_speech() {
    let host = FakeHost::new();
    let running = start(host.clone());

    say(&running, "start-Zira");
    wait_until("the first request", || host.requests().len() == 1);
    settle(&host);
    say(&running, "follow_brightness");
    wait_until("the follow-up", || host.requests().len() == 2);
    settle(&host);
    say(&running, "follow_unrelated");
    settle(&host);
    say(&running, "follow_thanks");
    wait_until("thanks", || host.requests().len() == 3);
    settle(&host);

    // The conversation ended with thanks, so speech without the name is ignored.
    say(&running, "follow_brightness");
    settle(&host);
    say(&running, "mention-Zira");
    settle(&host);
    say(&running, "middle-David");
    wait_until("the middle request", || host.requests().len() == 4);
    settle(&host);
    say(&running, "follow_thanks");
    wait_until("thanks", || host.requests().len() == 5);
    settle(&host);
    say(&running, "end-David");
    wait_until("the trailing request", || host.requests().len() == 6);
    settle(&host);
    say(&running, "follow_thanks");
    wait_until("thanks", || host.requests().len() == 7);
    settle(&host);
    say(&running, "chatter_then_command");
    wait_until("the request after chatter", || host.requests().len() == 8);
    settle(&host);

    let words = |text: &str| -> String {
        text.to_lowercase()
            .replace('%', " percent")
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let heard: Vec<String> = host.requests().iter().map(|text| words(text)).collect();
    assert_eq!(
        heard,
        [
            "turn off the lights",
            "make it 50 percent",
            "thanks",
            "check the temperature in the kitchen",
            "thanks",
            "can you turn off the lights",
            "thanks",
            "turn off the kitchen lights",
        ]
    );

    for (requested, heard) in host.replies.lock().unwrap().iter() {
        let heard = heard.expect("a reply was spoken");
        println!(
            "reply heard {} ms after it was ready",
            (heard - *requested).as_millis()
        );
    }

    let started = Instant::now();
    running
        .input
        .send(Input::MicrophoneLost(CaptureError::Disconnected))
        .unwrap();
    running.thread.join().unwrap();
    println!(
        "stopped {} ms after the microphone was lost",
        started.elapsed().as_millis()
    );
    assert_eq!(
        *host.stopped.lock().unwrap(),
        [VoiceError::Capture(CaptureError::Disconnected)]
    );
    assert_eq!(host.state(), Some(VoiceState::Off));
}

#[test]
#[ignore = "needs speech models and recordings"]
fn cloud_recognition_uses_the_provider_s_transcript_and_speaks_failures() {
    let host = FakeHost::new();
    *host.cloud_transcript.lock().unwrap() = Ok("Luna, what about the AC? Is it on?".into());
    let settings = VoiceSettings {
        cloud_transcription: true,
        ..voice_settings("Luna")
    };
    let running = start_with(host.clone(), settings);

    say(&running, "start-Zira");
    wait_until("the request", || host.requests().len() == 1);
    assert!(host.requests()[0].to_lowercase().contains("is it on"));
    settle(&host);

    *host.cloud_transcript.lock().unwrap() = Err(TranscriptionFailed {
        reply: "Unable to connect to OpenAI.".into(),
    });
    say(&running, "follow_thanks");
    settle(&host);
    // A failed follow-up is dropped, like speech that was not meant for Luna.
    assert_eq!(host.requests().len(), 1);

    running.input.send(Input::Stop).unwrap();
    running.thread.join().unwrap();
}

#[test]
#[ignore = "needs speech models and recordings"]
fn stopping_while_processing_cancels_and_shuts_down() {
    let host = FakeHost::new();
    let (release, hold) = mpsc::channel();
    *host.hold.lock().unwrap() = Some(hold);
    *host.release.lock().unwrap() = Some(release);
    let running = start(host.clone());

    say(&running, "start-Zira");
    wait_until("the request", || host.requests().len() == 1);
    assert_eq!(host.state(), Some(VoiceState::Processing));

    // In the order `Voice::stop` uses.
    let started = Instant::now();
    running
        .stopping
        .store(true, std::sync::atomic::Ordering::SeqCst);
    host.cancel();
    running.input.send(Input::Stop).unwrap();
    running.thread.join().unwrap();
    println!(
        "shut down {} ms after stopping",
        started.elapsed().as_millis()
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(
        !host
            .states
            .lock()
            .unwrap()
            .contains(&VoiceState::Responding)
    );
    assert_eq!(host.state(), Some(VoiceState::Off));
}

#[test]
#[ignore = "needs speech models and a microphone"]
fn measure_passive_listening_with_the_microphone() {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
    let seconds = std::env::var("LUNA_MEASURE_SECONDS").map_or(60, |value| value.parse().unwrap());
    let pid = sysinfo::get_current_pid().unwrap();
    let mut system = System::new();
    let refresh = |system: &mut System| {
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
    };
    refresh(&mut system);
    let before = system.process(pid).unwrap().memory() / 1_000_000;

    let host = FakeHost::new();
    let voice = Voice::default();
    voice
        .start(
            speech_files(),
            voice_settings("Luna"),
            Timing::default(),
            host.clone(),
        )
        .unwrap();
    std::thread::sleep(Duration::from_secs(2));
    refresh(&mut system);
    std::thread::sleep(Duration::from_secs(seconds));
    refresh(&mut system);
    let process = system.process(pid).unwrap();
    println!(
        "passive listening for {seconds} s: {:.1}% of one core, {} MB resident ({} MB before listening)",
        process.cpu_usage(),
        process.memory() / 1_000_000,
        before
    );
    assert_eq!(host.state(), Some(VoiceState::Listening));

    let started = Instant::now();
    voice.stop();
    println!("stopped in {} ms", started.elapsed().as_millis());
    assert_eq!(host.state(), Some(VoiceState::Off));
}

/// Word pieces the official sentencepiece tokenizer produces for the same names.
#[test]
#[ignore = "needs speech models"]
fn custom_wake_words_are_spelled_like_the_official_tokenizer() {
    let models = models_dir();
    for (name, expected) in [
        ("Luna", "▁ LU N A"),
        ("Jarvis", "▁JA R VI S"),
        ("Hey Jarvis", "▁HE Y ▁JA R VI S"),
        ("Max", "▁MA X"),
        ("Computer", "▁COMP U TER"),
        ("Mary Jane", "▁MAR Y ▁JA NE"),
        ("Athena", "▁A TH EN A"),
        ("Nova", "▁NO V A"),
        ("Zephyr", "▁ Z E PH Y R"),
        ("O'Brien", "▁O ' B RI EN"),
        ("Kai", "▁K A I"),
        ("Alexandria", "▁A LE X AN D RI A"),
        ("Friday", "▁F RI DAY"),
        ("Jeeves", "▁JE E VE S"),
        ("Echo", "▁E CH O"),
    ] {
        assert_eq!(
            keyword_line_for(&models, name),
            format!("{expected} @WAKE"),
            "{name}"
        );
    }
}

#[test]
#[ignore = "needs speech models and recordings"]
fn a_custom_wake_word_replaces_luna() {
    let models = models_dir();
    let speech = catalog::speech();
    let mut wake = WakeWord::load(
        &models.join("kws"),
        &speech.wake_word,
        &keyword_line_for(&models, "Jarvis"),
    )
    .unwrap();
    let mut heard = |name: &str| {
        wake.reset();
        let mut audio = read_wav(name);
        audio.extend(vec![0.0; 8_000]);
        audio
            .chunks(SAMPLE_RATE as usize / 10)
            .any(|chunk| wake.hears(chunk))
    };
    for name in ["jarvis_start", "jarvis_end"] {
        assert!(heard(name), "{name}");
    }
    for name in ["start-Zira", "end-David", "neg_long-David"] {
        assert!(!heard(name), "{name}");
    }
}

#[test]
#[ignore = "needs speech models"]
fn a_large_home_vocabulary_primes_transcription_without_crashing() {
    let speech = catalog::speech();
    let mut transcriber =
        Transcriber::load(&models_dir().join(whisper_file()), &speech.transcription).unwrap();
    let names: Vec<String> = (0..300).map(|n| format!("Living Room Lamp {n}")).collect();
    transcriber.expect_words(&names.join(", ")).unwrap();
    transcriber.expect_words("").unwrap();
}
