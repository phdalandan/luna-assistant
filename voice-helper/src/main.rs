//! Luna's speech synthesis helper, GPL-3.0 because it links espeak-ng. It runs as its own process
//! so Luna does not; the stdin/stdout protocol is described in docs/architecture.md.
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsModelConfig,
};

const FAILED: u32 = u32::MAX;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [model, voices, tokens, lexicon, data_dir, lang] = args.as_slice() else {
        eprintln!("usage: luna-voice <model> <voices> <tokens> <lexicon> <espeak-ng-data> <lang>");
        return ExitCode::FAILURE;
    };
    let config = OfflineTtsConfig {
        model: OfflineTtsModelConfig {
            kokoro: OfflineTtsKokoroModelConfig {
                model: Some(model.clone()),
                voices: Some(voices.clone()),
                tokens: Some(tokens.clone()),
                lexicon: Some(lexicon.clone()),
                data_dir: Some(data_dir.clone()),
                lang: Some(lang.clone()),
                length_scale: 1.0,
                ..Default::default()
            },
            num_threads: threads(),
            ..Default::default()
        },
        max_num_sentences: 1,
        ..Default::default()
    };
    let Some(tts) = OfflineTts::create(&config) else {
        eprintln!("could not load the speech model");
        return ExitCode::FAILURE;
    };
    // The first synthesis is slow, so it happens now, while the user is still speaking.
    tts.generate_with_config(
        ".",
        &GenerationConfig::default(),
        None::<fn(&[f32], f32) -> bool>,
    );
    let rate = u32::try_from(tts.sample_rate()).unwrap_or(0);
    if write(&rate.to_le_bytes()).is_err() {
        return ExitCode::FAILURE;
    }

    for line in io::stdin().lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let mut parts = line.splitn(4, '\t');
        let (Some(id), Some(speaker), Some(speed), Some(text)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            eprintln!("malformed request");
            continue;
        };
        let (Ok(id), Ok(speaker), Ok(speed)) = (
            id.parse::<u32>(),
            speaker.parse::<i32>(),
            speed.parse::<f32>(),
        ) else {
            eprintln!("malformed request");
            continue;
        };
        let generation = GenerationConfig {
            sid: speaker,
            speed,
            ..Default::default()
        };
        // Each sentence is sent as soon as it is ready, so playback starts early.
        let sent = tts.generate_with_config(
            text,
            &generation,
            Some(move |samples: &[f32], _progress: f32| {
                samples.is_empty() || frame(id, samples).is_ok()
            }),
        );
        let end = if sent.is_some() { 0 } else { FAILED };
        let ended = write(&[id.to_le_bytes(), end.to_le_bytes()].concat());
        if ended.is_err() {
            break;
        }
    }
    ExitCode::SUCCESS
}

fn frame(id: u32, samples: &[f32]) -> io::Result<()> {
    let count = u32::try_from(samples.len()).map_err(io::Error::other)?;
    let mut bytes = Vec::with_capacity(8 + samples.len() * 4);
    bytes.extend(id.to_le_bytes());
    bytes.extend(count.to_le_bytes());
    for sample in samples {
        bytes.extend(sample.to_le_bytes());
    }
    write(&bytes)
}

fn write(bytes: &[u8]) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(bytes)?;
    stdout.flush()
}

fn threads() -> i32 {
    std::thread::available_parallelism().map_or(2, |count| count.get().min(8)) as i32
}
