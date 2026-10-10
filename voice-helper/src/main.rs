//! Luna's speech synthesis helper, GPL-3.0 because it links espeak-ng. It runs as its own process
//! so Luna does not; the stdin/stdout protocol is described in docs/architecture.md.
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsModelConfig,
};

const FAILED: u32 = u32::MAX;
/// Words that must follow the first clause in its sentence before it is spoken on its own.
const MIN_WORDS_AFTER_SPLIT: usize = 4;
/// Silence kept on each side of a split clause, about half a comma's pause.
const PAUSE_MS: usize = 90;
/// Quieter samples count as silence when trimming.
const SILENCE: f32 = 0.003;

#[derive(Clone, Copy)]
enum Trim {
    None,
    Start,
    End,
}

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
        // Each sentence is sent as soon as it is ready, so playback starts early. A first clause
        // like "Yes," is spoken on its own, so a long first sentence does not delay the reply.
        let (first, rest) = first_clause(text);
        let pause = tts.sample_rate() as usize * PAUSE_MS / 1000;
        let parts = [(first, Trim::End), (rest, Trim::Start)];
        let sent = parts
            .into_iter()
            .filter(|(part, _)| !part.is_empty())
            .all(|(part, trim)| {
                // Each part has its own silence, so only a comma's pause is kept between them.
                let trim = if rest.is_empty() { Trim::None } else { trim };
                let opening = std::cell::Cell::new(true);
                tts.generate_with_config(
                    part,
                    &generation,
                    Some(move |samples: &[f32], _progress: f32| {
                        let samples = match trim {
                            Trim::End => trim_end(samples, pause),
                            Trim::Start if opening.replace(false) => trim_start(samples, pause),
                            _ => samples,
                        };
                        samples.is_empty() || frame(id, samples).is_ok()
                    }),
                )
                .is_some()
            });
        let end = if sent { 0 } else { FAILED };
        let ended = write(&[id.to_le_bytes(), end.to_le_bytes()].concat());
        if ended.is_err() {
            break;
        }
    }
    ExitCode::SUCCESS
}

/// Splits "Yes, but only the 2.4G. Want me to…" after the first sentence's first comma. A
/// clause spoken alone is drawn out, so short sentences like "Yep, it's on." stay whole.
fn first_clause(text: &str) -> (&str, &str) {
    let sentence_end = [". ", "? ", "! "]
        .iter()
        .filter_map(|end| text.find(end))
        .min()
        .unwrap_or(text.len());
    let Some(comma) = text[..sentence_end].find(", ") else {
        return (text, "");
    };
    let after = text[comma + 2..sentence_end].split_whitespace().count();
    if after < MIN_WORDS_AFTER_SPLIT {
        return (text, "");
    }
    (&text[..=comma], text[comma + 2..].trim_start())
}

fn trim_end(samples: &[f32], keep: usize) -> &[f32] {
    let last = samples.iter().rposition(|sample| sample.abs() > SILENCE);
    let end = last.map_or(0, |last| (last + 1 + keep).min(samples.len()));
    &samples[..end]
}

fn trim_start(samples: &[f32], keep: usize) -> &[f32] {
    let first = samples.iter().position(|sample| sample.abs() > SILENCE);
    let start = first.map_or(samples.len(), |first| first.saturating_sub(keep));
    &samples[start..]
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

#[cfg(test)]
mod tests {
    use super::{first_clause, trim_end, trim_start};

    #[test]
    fn silence_is_trimmed_to_a_short_pause() {
        let samples = [0.0, 0.0, 0.5, 0.4, 0.0, 0.0, 0.0];
        assert_eq!(trim_end(&samples, 1), [0.0, 0.0, 0.5, 0.4, 0.0]);
        assert_eq!(trim_start(&samples, 1), [0.0, 0.5, 0.4, 0.0, 0.0, 0.0]);
        assert!(trim_end(&[0.0; 4], 1).is_empty());
    }

    #[test]
    fn a_long_first_sentence_starts_with_its_first_clause() {
        assert_eq!(
            first_clause("Yes, but only the 2.4G. Want me to turn on the 5G too?"),
            ("Yes,", "but only the 2.4G. Want me to turn on the 5G too?")
        );
        assert_eq!(first_clause("Done."), ("Done.", ""));
        assert_eq!(first_clause("Yep, it's on."), ("Yep, it's on.", ""));
        assert_eq!(
            first_clause("The 2.4G is on. Sure, the 5G too."),
            ("The 2.4G is on. Sure, the 5G too.", "")
        );
    }
}
